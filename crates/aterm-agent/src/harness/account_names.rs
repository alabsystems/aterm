// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The harness's bridge to the account layer (docs/DESIGN-shared-files-2026-11-11.md §3, §10;
//! `aterm_file::account`): what an account is named on this Mac, and the account a word names.
//!
//! - [`names_of_row`] and [`names_of_login`]: a roster row's or a session login's account key
//!   and names: the lab's where the lab names the account ([`Lab::names`]), with whatever name
//!   or address the lab's copy lacks made here; else names made here from its realm and claims
//!   address, with no slug, since a slug is the lab's to give.
//! - [`resolve`]: the account a word names among one pool's accounts (Claude Code's realm, or
//!   both of Kimi Code's), the roster's rows, this Mac's session logins and the lab's accounts
//!   alike: its slug, its key, its provider word and address (`claude ada@x.example` or
//!   `claude:ada@x.example`), or an address that names one account of the pool. A roster label
//!   names nothing: it is a local key, and the verbs reach a row from the word through its
//!   account key; a word that names one account while a roster row here labelled with it is
//!   another's is refused. A word that names an account of another pool says which `--cli`
//!   takes it, or, for a verb of one pool alone ([`resolve_in`], [`OtherPool::Alone`]), the
//!   verb's own words, since no `--cli` does.
//! - [`asked`]: the account a verb that lists a row (`add`, `adopt-default`) is asked for:
//!   [`resolve`]'s, else an account neither this Mac nor the lab knows yet, named by its provider
//!   word and address alone; the row the verb lists is labelled with the account's reserved slug
//!   ([`keys::reserved_slug`]). [`login_named`]: the account a login's own address names, which
//!   `adopt-default` lists where no account is named.
//! - [`Named`]: how each surface prints the account it resolved: a message to a person
//!   ([`Named::shown`]), the footer ([`Named::wide`], [`Named::narrow`]), a ledger row or a log
//!   line ([`Named::logged`]) and a step a person types ([`Named::word`]; [`Named::word_among`]
//!   its key where a roster row here is labelled with its slug and holds another account).
//! - [`words`]: the operands an accounts verb's parse leaves once it took its flags, with a
//!   provider word and the address after it joined into one name, so `accounts use claude
//!   ada@x.example` takes the name as one word.
//! - [`Listing`]: how a report names every row of a roster, in its words and in its JSON.
//! - [`RowName`] and [`row_named`]: the sign-in verbs' rows (`login`, `renew`, `shared-renew`,
//!   `browser`), reached from a word and named in their lines as a message, a step and a log name
//!   them.
//!
//! Rows with no address have no key and no names; they are not candidates. Such a row is
//! printed by its label marked so (`work (no address)`), and the one word that types its label
//! is the step that signs it in ([`sign_in_word`]), which only the sign-in verbs take, for such a
//! row alone ([`row_named`]). A lab source's Kimi Code row that no region places yet (a pending
//! one, `needs-region`) has no key either, since Kimi Code's two regions are separate account
//! systems: it is printed by its label marked `(no region)` ([`names_of_listed`], [`unkeyed`]).
//! No surface here prints the label of a row with a key.

use super::accounts::{Cli, Row};
use super::alab_roster::Lab;
use super::claims::LeaseKey;
use super::journal;
use crate::ai::connector::Region;
use aterm_db::typed::keys;
use aterm_file::{
    AccountNames,
    account::{self, Found},
};
use std::collections::{BTreeMap, BTreeSet};

/// The realms of `cli`'s pool: the accounts a word may name for it.
fn realms(cli: Cli) -> &'static [&'static str] {
    match cli {
        Cli::Claude => &["claude"],
        Cli::Codex => &["codex"],
        Cli::Grok => &["grok"],
        Cli::Kimi => &["kimi:global", "kimi:mainland-cn"],
        Cli::Agy => &["google"],
    }
}

/// The provider word of `cli`'s pool (`claude`, `oai`, `xai`, `kimi`, `google`): its first realm's
/// ([`keys::provider_word`]), what a person types before an address to name one of its accounts.
#[must_use]
pub fn provider_word_of(cli: Cli) -> &'static str {
    realms(cli)
        .first()
        .and_then(|realm| keys::provider_word(realm))
        .unwrap_or_default()
}

/// The canonical realm of `names`, where one is known: a realm an older build spelled
/// (`openai`) reads canonical, and an empty one (a missing or conflicted cell) is none, so such
/// an account belongs to no pool.
fn realm_of(names: &AccountNames) -> Option<&'static str> {
    Some(names.realm.as_str())
        .filter(|realm| !realm.trim_ascii().is_empty())
        .and_then(keys::canonical_realm)
}

/// The account key `realm` and `email` make, the lab's spelling.
fn key_of(realm: &str, email: &str) -> Option<String> {
    keys::account_pk(realm, email).ok()
}

/// Names made on this Mac for the account `lease` keys: its provider word and claims address,
/// and its login address; no slug.
fn local(lease: &LeaseKey, login: Option<&str>) -> AccountNames {
    let realm = keys::canonical_realm(&lease.pool).unwrap_or_default();
    AccountNames {
        realm: realm.to_owned(),
        slug: String::new(),
        name: keys::account_name(realm, &lease.email),
        email: Some(lease.email.clone()),
        login: login.and_then(|login| keys::address(login).ok()),
        live: true,
    }
}

/// The names of account `pk`: the lab's slug and lifecycle, with its realm and whatever the
/// lab's copy lacks of its name and addresses taken from `made`, else `made` where the lab does
/// not hold it. `made` is the names this Mac makes from the realm and claims address `pk` was
/// hashed from, so they are the same account's: a lab whose address row has not replicated here
/// yet, or is conflicted, still leaves the address a row or a login holds naming the account.
fn lab_or(pk: String, lab: Option<&Lab>, made: AccountNames) -> (String, AccountNames) {
    let Some(held) = lab.and_then(|lab| lab.names.get(&pk)) else {
        return (pk, made);
    };
    let names = AccountNames {
        realm: made.realm,
        slug: held.slug.clone(),
        name: held.name.clone().or(made.name),
        email: held.email.clone().or(made.email),
        login: held.login.clone().or(made.login),
        live: held.live,
    };
    (pk, names)
}

/// The account key and names of `cli`'s roster row `row`: the lab's names for its key, filled
/// from names made here where the lab's lack a name or an address, else names made here
/// ([`local`]). `None` for a row with no address, which has no key.
#[must_use]
pub fn names_of_row(cli: Cli, row: &Row, lab: Option<&Lab>) -> Option<(String, AccountNames)> {
    let pk = journal::account_key_with(cli, row, &key_of)?;
    let lease = LeaseKey::for_cli(cli, row.vendor.region, row.expected_identity_email()?).ok()?;
    Some(lab_or(
        pk.as_str().to_owned(),
        lab,
        local(&lease, row.email.as_deref()),
    ))
}

/// The account key and names of the `cli` account a session here signed in to as `email`
/// (Kimi Code's in `region`), as [`names_of_row`] names a row's. `None` for text that is no
/// address.
#[must_use]
pub fn names_of_login(
    cli: Cli,
    region: Option<Region>,
    email: &str,
    lab: Option<&Lab>,
) -> Option<(String, AccountNames)> {
    let lease = LeaseKey::for_cli(cli, region, email).ok()?;
    let pk = key_of(&lease.pool, &lease.email)?;
    Some(lab_or(pk, lab, local(&lease, Some(email))))
}

/// The account key and names of a lab source's `cli` row that lists `email` in `region`, which no
/// row here holds, as [`names_of_login`] names a login's; `None` for a Kimi Code row the source
/// places in no region yet (a pending one, `needs-region`): Kimi Code's two regions are separate
/// account systems, so its address could be either's account, where a region left open
/// everywhere else names the global one ([`LeaseKey::for_cli`]).
#[must_use]
pub fn names_of_listed(
    cli: Cli,
    region: Option<Region>,
    email: &str,
    lab: Option<&Lab>,
) -> Option<(String, AccountNames)> {
    if cli == Cli::Kimi && region.is_none() {
        return None;
    }
    names_of_login(cli, region, email, lab)
}

/// How a message marks the label it names a lab source's `cli` row by where the row has no
/// account key: `(no region)` for a Kimi Code address no region places ([`names_of_listed`]),
/// else `(no address)`.
#[must_use]
pub fn unkeyed(cli: Cli, region: Option<Region>, address: bool) -> &'static str {
    if address && cli == Cli::Kimi && region.is_none() {
        "(no region)"
    } else {
        "(no address)"
    }
}

/// `cli`'s roster row `row` as a message to a person names it: its account's name and slug
/// ([`Named::shown`]); a row with no address, which has no key and no names, by its label
/// marked so (`work (no address)`), the one place a roster label is printed.
#[must_use]
pub fn shown_row(cli: Cli, row: &Row, lab: Option<&Lab>) -> String {
    match names_of_row(cli, row, lab) {
        Some((pk, names)) => Named {
            pk,
            row: Some(row),
            names,
        }
        .shown(),
        None => format!("{} (no address)", row.label),
    }
}

/// The word a step names `cli`'s roster row `row` by, among the roster's rows `roster` and the
/// lab ([`Named::word_among`]); `None` for a row with no address, which no word names.
#[must_use]
pub fn word_of_row(cli: Cli, row: &Row, roster: &[Row], lab: Option<&Lab>) -> Option<Word> {
    let (pk, names) = names_of_row(cli, row, lab)?;
    let candidates = Candidates {
        roster,
        lab,
        ..Candidates::default()
    };
    Some(
        Named {
            pk,
            row: Some(row),
            names,
        }
        .word_among(cli, &candidates),
    )
}

/// `cli`'s roster row `row` as a ledger row, an event or a log line names it: its account's key
/// with its slug beside it ([`Named::logged`]); a row with no address, which has no key, by its
/// label marked so (`work (no address)`), as [`shown_row`] names one.
#[must_use]
pub fn logged_row(cli: Cli, row: &Row, lab: Option<&Lab>) -> String {
    match names_of_row(cli, row, lab) {
        Some((pk, names)) => Named {
            pk,
            row: Some(row),
            names,
        }
        .logged(),
        None => format!("{} (no address)", row.label),
    }
}

/// The word a step that signs `cli`'s roster row `row` in names it by (`accounts login`,
/// `accounts renew`, [`super::accounts_step::Step::sign_in`]): its account's word among the
/// roster's rows `roster` and the lab ([`word_of_row`]); a row with no address, which has no key,
/// no name and no slug, by its label ([`Word::unaddressed`]), the one word that names such a row,
/// as `<label> (no address)` is the one way a message names it, and the word the sign-in verbs
/// take for it alone ([`row_named`]).
#[must_use]
pub fn sign_in_word(cli: Cli, row: &Row, roster: &[Row], lab: Option<&Lab>) -> Word {
    word_of_row(cli, row, roster, lab).unwrap_or_else(|| Word::unaddressed(&row.label))
}

/// The word a step that signs `cli`'s roster row `row` in names it by where the roster's other
/// rows are not at hand (a sign-in's approver whose caller held no roster,
/// [`super::login_drive::Request::word`]): its account's key,
/// which names that account whatever another row here is labelled ([`label_clash`] never refuses
/// a key), where its slug may be the label of another row here and so be refused; a row with no
/// address by its label, as [`sign_in_word`] names one.
#[must_use]
pub fn key_word_of_row(cli: Cli, row: &Row) -> Word {
    names_of_row(cli, row, None).map_or_else(|| Word::unaddressed(&row.label), |(pk, _)| Word(pk))
}

/// The word a step names the `cli` account `email` makes by ([`Named::word`]): a row a verb has
/// just listed with that address, before the roster this process read holds it. `None` for text
/// that is no address.
#[must_use]
pub fn word_of_login(cli: Cli, email: &str, lab: Option<&Lab>) -> Option<Word> {
    let (pk, names) = names_of_login(cli, None, email, lab)?;
    Some(
        Named {
            pk,
            row: None,
            names,
        }
        .word(),
    )
}

/// [`sign_in_word`] under the lab this process holds.
#[must_use]
pub fn sign_in_word_here(cli: Cli, row: &Row, roster: &[Row]) -> Word {
    let lab = super::alab_roster::current();
    sign_in_word(cli, row, roster, lab.as_deref())
}

/// [`sign_in_word`] of the row of `roster` labelled `label`, where one is; else `label` itself,
/// which no row holds and so no verb takes either: a surface that names a label it read from a
/// record of a row since removed.
#[must_use]
pub fn sign_in_word_of(cli: Cli, label: &str, roster: &[Row], lab: Option<&Lab>) -> Word {
    roster.iter().find(|row| row.label == label).map_or_else(
        || Word::unaddressed(label),
        |row| sign_in_word(cli, row, roster, lab),
    )
}

/// How a report names every row of one pool's roster (`accounts`, its outlook, its journal
/// section): each row in its words by its account's name and slug ([`shown_row`]), and in its
/// JSON by its account's key ([`Listing::id`]) beside its slug and name ([`Listing::names`]). A
/// row with no address has none of those, so its words name it by its label marked so (`work (no
/// address)`) and its JSON by its label. A label no row holds (a record of a row since removed)
/// is marked so in the words (`work (no longer listed)`), since bare it could be a word that
/// names another account now, and is itself in the JSON, where no row's `ac` or `label` matches
/// it. Empty ([`Listing::default`]), it lists no roster and names every row by its label (a
/// caller that names no account yet).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Listing {
    rows: BTreeMap<String, Listed>,
    /// Whether it lists a roster ([`Listing::of`]), so a label none of `rows` holds is a row since
    /// removed.
    roster: bool,
}

/// One row of a [`Listing`].
#[derive(Clone, Debug, PartialEq, Eq)]
struct Listed {
    shown: String,
    names: Option<(String, AccountNames)>,
}

impl Listing {
    /// `cli`'s rows `roster`, named under the lab `lab`.
    #[must_use]
    pub fn of(cli: Cli, roster: &[Row], lab: Option<&Lab>) -> Self {
        let rows = roster
            .iter()
            .map(|row| {
                let listed = Listed {
                    shown: shown_row(cli, row, lab),
                    names: names_of_row(cli, row, lab),
                };
                (row.label.clone(), listed)
            })
            .collect();
        Self { rows, roster: true }
    }

    /// [`Listing::of`] under the lab this process holds.
    #[must_use]
    pub fn here(cli: Cli, roster: &[Row]) -> Self {
        let lab = super::alab_roster::current();
        Self::of(cli, roster, lab.as_deref())
    }

    /// This listing with the row labelled `label` named as the `cli` account a login here signed
    /// in to as `email` names it under `lab` ([`names_of_login`]): the row a report of a Mac that
    /// lists no account stands for this Mac's own login by ([`super::accounts::Roster::own`]),
    /// which has no address of its own. Text that is no address leaves the row as it was.
    #[must_use]
    pub fn with_login(mut self, cli: Cli, label: &str, email: &str, lab: Option<&Lab>) -> Self {
        if let Some((pk, names)) = names_of_login(cli, None, email, lab) {
            let shown = Named {
                pk: pk.clone(),
                row: None,
                names: names.clone(),
            }
            .shown();
            let listed = Listed {
                shown,
                names: Some((pk, names)),
            };
            self.rows.insert(label.to_owned(), listed);
        }
        self
    }

    /// The row labelled `label` as the report's words name it: its account's name and slug, a
    /// row with no address by its label marked so, and a label no row of the roster holds marked
    /// `(no longer listed)`.
    #[must_use]
    pub fn shown<'a>(&'a self, label: &'a str) -> std::borrow::Cow<'a, str> {
        match self.rows.get(label) {
            Some(row) => std::borrow::Cow::Borrowed(row.shown.as_str()),
            None if self.roster => std::borrow::Cow::Owned(format!("{label} (no longer listed)")),
            None => std::borrow::Cow::Borrowed(label),
        }
    }

    /// The row labelled `label` as the report's JSON names it: its account's key, else its label.
    #[must_use]
    pub fn id<'a>(&'a self, label: &'a str) -> &'a str {
        self.rows
            .get(label)
            .and_then(|row| row.names.as_ref())
            .map_or(label, |(pk, _)| pk.as_str())
    }

    /// The account key and names of the row labelled `label`, where it has an address.
    #[must_use]
    pub fn names(&self, label: &str) -> Option<(&str, &AccountNames)> {
        let (pk, names) = self.rows.get(label)?.names.as_ref()?;
        Some((pk.as_str(), names))
    }
}

/// How a surface about one roster row's sign-in names the row: as a message to a person shows
/// it, as the step that signs it in types it, and as a log line names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowName {
    /// As a message to a person shows it ([`shown_row`]).
    pub shown: String,
    /// As a step that signs it in types it ([`sign_in_word`]).
    pub word: Word,
    /// As a ledger row, an event or a log line names it ([`logged_row`]).
    pub logged: String,
}

impl RowName {
    /// `cli`'s roster row `row`, named among the roster's rows `roster` and the lab.
    #[must_use]
    pub fn of(cli: Cli, row: &Row, roster: &[Row], lab: Option<&Lab>) -> Self {
        Self {
            shown: shown_row(cli, row, lab),
            word: sign_in_word(cli, row, roster, lab),
            logged: logged_row(cli, row, lab),
        }
    }

    /// The row of `roster` labelled `label`, named as [`RowName::of`] names it; a label no row
    /// holds (a record of a row since removed) by that label, which no verb takes either.
    #[must_use]
    pub fn of_label(cli: Cli, label: &str, roster: &[Row], lab: Option<&Lab>) -> Self {
        match roster.iter().find(|row| row.label == label) {
            Some(row) => Self::of(cli, row, roster, lab),
            None => Self {
                shown: label.to_owned(),
                word: Word::unaddressed(label),
                logged: label.to_owned(),
            },
        }
    }

    /// [`RowName::of_label`] under the lab this process holds.
    #[must_use]
    pub fn here(cli: Cli, label: &str, roster: &[Row]) -> Self {
        let lab = super::alab_roster::current();
        Self::of_label(cli, label, roster, lab.as_deref())
    }
}

/// The roster row a verb that signs a row in (`accounts login`, `accounts renew`, `accounts
/// browser`) names by `word` among `candidates`: the row of the account the word names
/// ([`resolve`]); or, where the word names no account and the roster row labelled with it holds
/// no address, that row, which has no key, no name and no slug, so its label is the one word that
/// names it ([`sign_in_word`]). A roster label of a row with an address names nothing.
///
/// # Errors
/// The resolver's refusal; or the word names an account no row of this Mac's roster holds,
/// named by its key and slug ([`Named::logged`]), with `none`, what the verb did not do.
pub fn row_named<'a>(
    cli: Cli,
    word: &str,
    candidates: &Candidates<'a>,
    none: &str,
) -> Result<&'a Row, String> {
    match resolve(cli, word, candidates) {
        Ok(Named { row: Some(row), .. }) => Ok(row),
        Ok(named) => Err(format!(
            "`{word}` names {}, which no row of this Mac's roster holds: `{}` lists an account; \
             {none}",
            named.logged(),
            super::accounts_step::Step::Add(cli)
        )),
        Err(why) => candidates
            .roster
            .iter()
            .find(|row| {
                row.label == word
                    && why == aterm_file::no_account(word)
                    && names_of_row(cli, row, candidates.lab).is_none()
            })
            .ok_or(why),
    }
}

/// A login a session on this Mac is signed in to: Kimi Code's region, and its address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Login<'a> {
    pub region: Option<Region>,
    pub email: &'a str,
}

/// The accounts a word may name for one pool: the roster's rows, this Mac's session logins, the
/// lab's accounts and the account keys this Mac's records hold.
#[derive(Clone, Copy, Debug, Default)]
pub struct Candidates<'a> {
    pub roster: &'a [Row],
    pub logins: &'a [Login<'a>],
    pub lab: Option<&'a Lab>,
    /// Account keys (`ac-…`) this Mac's records of the pool hold (a record's, a forgotten
    /// record's): an account no row here and no lab copy may still name, such as a removed row's
    /// whose observations this Mac keeps. Each is named by its key alone, since nothing here
    /// says its address or slug; a key the lab or a row names takes their names. Text that is no
    /// account key is no candidate.
    pub held: &'a [String],
}

/// The account a word named ([`resolve`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Named<'a> {
    /// Its account key (`ac-…`).
    pub pk: String,
    /// The roster row of it, where this Mac has one: the first in file order.
    pub row: Option<&'a Row>,
    pub names: AccountNames,
}

/// The word a step a person types names an account by: its slug, else its key. Never its
/// address, since a step reaches logs and captured stderr, and never the roster label of a row
/// with an address, which no verb takes. A row with no address has no account to name, and a
/// step that signs it in names it by its label ([`sign_in_word`]).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Word(String);

impl Word {
    /// The word.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The account a person names, as a step that takes one spells it (`<account>`): a step
    /// that follows another making the account, before this Mac holds a row of it to name.
    #[must_use]
    pub fn chosen() -> Self {
        Self("<account>".to_owned())
    }

    /// The word of a roster row with no address, which has no account key, name or slug: its
    /// label, which only a step that signs such a row in types ([`sign_in_word`]) and only the
    /// sign-in verbs take, for such a row alone ([`row_named`]).
    fn unaddressed(label: &str) -> Self {
        Self(label.to_owned())
    }
}

impl std::fmt::Display for Word {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl Named<'_> {
    /// The account as a message to a person shows it: its name and its slug (`claude
    /// ada@x.example (claude-ada-x)`), else what it has of them, else its key.
    #[must_use]
    pub fn shown(&self) -> String {
        Some(self.names.shown())
            .filter(|shown| !shown.is_empty())
            .unwrap_or_else(|| self.pk.clone())
    }

    /// The account where there is room: its name, else its slug, else its key.
    #[must_use]
    pub fn wide(&self) -> &str {
        self.names
            .name
            .as_deref()
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| self.narrow())
    }

    /// The account where there is little room: its slug, else its name, else its key.
    #[must_use]
    pub fn narrow(&self) -> &str {
        Some(self.names.narrow())
            .filter(|narrow| !narrow.is_empty())
            .unwrap_or(&self.pk)
    }

    /// The account as a ledger row, an event or a log line names it: its key, with its slug
    /// beside it (`ac-… (claude-ada-x)`). Never its address.
    #[must_use]
    pub fn logged(&self) -> String {
        if self.names.slug.is_empty() {
            self.pk.clone()
        } else {
            format!("{} ({})", self.pk, self.names.slug)
        }
    }

    /// The word a step names it by ([`Word`]).
    #[must_use]
    pub fn word(&self) -> Word {
        Word(if self.names.slug.is_empty() {
            self.pk.clone()
        } else {
            self.names.slug.clone()
        })
    }

    /// The word a step names it by among `candidates` ([`Named::word`]): its key where a roster
    /// row here is labelled with its slug and that row holds another account or no address, since
    /// [`resolve`] refuses that slug there ([`label_clash`]), so the step a surface offers is one
    /// the verb takes.
    #[must_use]
    pub fn word_among(&self, cli: Cli, candidates: &Candidates<'_>) -> Word {
        let word = self.word();
        match label_clash(cli, word.as_str(), self, candidates) {
            Some(_) => Word(self.pk.clone()),
            None => word,
        }
    }
}

/// A canonical realm's provider, as a refusal names it, and the `--cli` word of its pool, where
/// one pools it.
fn provider(realm: &str) -> (&'static str, Option<&'static str>) {
    match realm {
        "claude" => ("a Claude account", Some("claude")),
        "codex" => ("an OpenAI account", Some("codex")),
        "grok" => ("an xAI account", Some("grok")),
        "kimi:global" => ("a Kimi account", Some("kimi")),
        "kimi:mainland-cn" => ("a Kimi (mainland China) account", Some("kimi")),
        "google" => ("a Google account", Some("agy")),
        "meta" => ("a Meta account", None),
        _ => ("an account of another provider", None),
    }
}

/// What a refusal says of a word that names no account of the verb's pool and names one of
/// another pool ([`resolve_in`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OtherPool {
    /// Which `--cli` takes it: the verb takes every pool's accounts.
    SaysCli,
    /// The verb is its pool's alone, so no `--cli` takes the word: the refusal ends with these
    /// words (`a token is made only for a Claude Code account`).
    Alone(&'static str),
}

/// What a word that names no account of `cli`'s pool says when it names one of another pool
/// (`realms`, each once): which `--cli` takes it, or, for a verb of `cli`'s pool alone
/// ([`OtherPool::Alone`]), the verb's own words.
fn elsewhere(word: &str, cli: Cli, realms: &[&str], other: OtherPool) -> String {
    if let OtherPool::Alone(alone) = other {
        let what: Vec<&str> = realms.iter().map(|realm| provider(realm).0).collect();
        return match what.as_slice() {
            [what] => format!("`{word}` is {what}: {alone}"),
            _ => format!(
                "`{word}` names no {} account; it names {}: {alone}",
                cli.connector().product,
                what.join(" and ")
            ),
        };
    }
    let clause = |realm: &str| match provider(realm) {
        (what, Some(cli)) => format!("{what} (`--cli {cli}`)"),
        (what, None) => format!("{what}, which no pool here holds"),
    };
    match realms {
        [realm] => match provider(realm) {
            (what, Some(other)) => format!("`{word}` is {what}: add `--cli {other}`"),
            (what, None) => format!("`{word}` is {what}, which no pool here holds"),
        },
        _ => format!(
            "`{word}` names no {} account; it names {}",
            cli.connector().product,
            realms
                .iter()
                .map(|realm| clause(realm))
                .collect::<Vec<_>>()
                .join(" and ")
        ),
    }
}

/// Why `word` may not name `named`: a roster row here is labelled `word`, and that row is
/// another account's or holds no address. A roster label names nothing, but a habit, a step a
/// log kept or a script may still type one, and it must never quietly name another account (an
/// owner may since have slugged another account with the very word a row here is labelled).
/// Both accounts are named by key and slug ([`Named::logged`]): the refusal is about which
/// account the word names, and the key is the word that names `named` exactly, whatever a row is
/// labelled, which the refusal asks for. (A verb's refusal about the state of the account a word
/// named is a message to a person, and shows its name, [`Named::shown`].)
fn label_clash(
    cli: Cli,
    word: &str,
    named: &Named<'_>,
    candidates: &Candidates<'_>,
) -> Option<String> {
    if word == named.pk {
        return None;
    }
    let labelled = candidates.roster.iter().find(|row| row.label == word)?;
    let held = match names_of_row(cli, labelled, candidates.lab) {
        Some((pk, _)) if pk == named.pk => return None,
        Some((pk, names)) => format!(
            "holds {}",
            Named {
                pk,
                row: Some(labelled),
                names
            }
            .logged()
        ),
        None => "holds no address".to_owned(),
    };
    Some(format!(
        "`{word}` names {}, but this Mac's roster row labelled `{word}` {held}: name the account \
         by its key, or by its provider word and address",
        named.logged()
    ))
}

/// What `word`, which names no account of `cli`'s pool, says when it names an account of the
/// lab's in another pool ([`elsewhere`]). `None` where it names none there either, or only
/// accounts whose realm is unknown, which belong to no pool.
fn elsewhere_in(
    word: &str,
    cli: Cli,
    lab: &Lab,
    pool: &[&str],
    other: OtherPool,
) -> Option<String> {
    let others: BTreeMap<String, AccountNames> = lab
        .names
        .iter()
        .filter(|(_, names)| realm_of(names).is_some_and(|realm| !pool.contains(&realm)))
        .map(|(pk, names)| (pk.clone(), names.clone()))
        .collect();
    let pks = match account::named(&others, word) {
        Found::One(pk) => vec![pk],
        Found::Many(pks) => pks,
        Found::None => return None,
    };
    let mut realms: Vec<&str> = pks.iter().filter_map(|pk| realm_of(&others[*pk])).collect();
    realms.sort_unstable();
    realms.dedup();
    Some(elsewhere(word, cli, &realms, other))
}

/// The one account of `cli`'s pool `word` names among `candidates` ([`aterm_file::account`]):
/// its slug, its key, its provider word and address in either spelling, or an address that names
/// one account of the pool, live accounts tried first. Every candidate is named as the lab
/// names it, else as [`names_of_row`] and [`names_of_login`] make its names here, else (a key
/// only this Mac's records hold, [`Candidates::held`]) by its key alone; an account is the
/// pool's by its canonical realm, and one whose realm is unknown is no pool's.
///
/// # Errors
/// `word` names no account of the pool, or several: a word that names none lists none
/// (`aterm_file::no_account`), or says which `--cli` takes the account of another pool it
/// names; one that names several lists each by its slug, its name and its key
/// (`aterm_file::many_accounts`). A roster label names nothing, and a word that names one
/// account while a roster row here labelled with it holds another, or no address, is refused
/// with both.
pub fn resolve<'a>(cli: Cli, word: &str, candidates: &Candidates<'a>) -> Result<Named<'a>, String> {
    resolve_in(cli, word, candidates, OtherPool::SaysCli)
}

/// [`resolve`] for a verb whose refusal of a word that names an account of another pool says
/// `other` ([`OtherPool`]): a verb of one pool alone (`accounts token`, `token-run`) names no
/// `--cli` that would take it, since none does.
///
/// # Errors
/// As [`resolve`]'s.
pub fn resolve_in<'a>(
    cli: Cli,
    word: &str,
    candidates: &Candidates<'a>,
    other: OtherPool,
) -> Result<Named<'a>, String> {
    let pool = realms(cli);
    let in_pool = |names: &AccountNames| realm_of(names).is_some_and(|realm| pool.contains(&realm));
    let lab = candidates.lab;
    let mut names: BTreeMap<String, AccountNames> = lab
        .into_iter()
        .flat_map(|lab| &lab.names)
        .filter(|(_, names)| in_pool(names))
        .map(|(pk, names)| (pk.clone(), names.clone()))
        .collect();
    // A key only this Mac's records hold is the pool's (they are the pool's records), its realm
    // the pool's first: nothing here says which of Kimi Code's regions it was.
    for pk in candidates.held {
        if pk.starts_with("ac-") && journal::Key::new(pk).is_some() {
            names.entry(pk.clone()).or_insert_with(|| AccountNames {
                realm: pool.first().copied().unwrap_or_default().to_owned(),
                ..AccountNames::default()
            });
        }
    }
    let mut rows: BTreeMap<String, &'a Row> = BTreeMap::new();
    // The keys a row or a login here named first, whose names replace the lab's copy.
    let mut here: BTreeSet<String> = BTreeSet::new();
    for row in candidates.roster {
        if let Some((pk, held)) = names_of_row(cli, row, lab)
            && here.insert(pk.clone())
        {
            rows.insert(pk.clone(), row);
            names.insert(pk, held);
        }
    }
    for login in candidates.logins {
        if let Some((pk, held)) = names_of_login(cli, login.region, login.email, lab)
            && here.insert(pk.clone())
        {
            names.insert(pk, held);
        }
    }
    match account::one(&names, word) {
        Ok(pk) => {
            let named = Named {
                pk: pk.to_owned(),
                row: rows.get(pk).copied(),
                names: names[pk].clone(),
            };
            match label_clash(cli, word, &named, candidates) {
                Some(why) => Err(why),
                None => Ok(named),
            }
        }
        Err(why) if account::named(&names, word) != Found::None => Err(why),
        Err(why) => Err(lab
            .and_then(|lab| elsewhere_in(word, cli, lab, pool, other))
            .unwrap_or(why)),
    }
}

/// The account a verb that lists a row (`accounts add`, `adopt-default`, `import`) names by
/// `word` among `candidates`: the account [`resolve`] names there; or, where it names none, the
/// account of `cli`'s pool that `word` names by its provider word and address
/// (`claude ada@x.example`, `claude:ada@x.example`), which no row here and no lab copy holds yet,
/// named by its name and its key (made here from its realm and address), with no slug, which is
/// the lab's to give, and no row; an address spelled in another case than a row's or the lab's
/// names that account, found by its key. The address alone names no such account: nothing says
/// its provider.
///
/// # Errors
/// [`resolve`]'s refusal, where `word` names no account and is no provider word and address; a
/// provider word and address of another pool says which `--cli` takes it.
pub fn asked<'a>(cli: Cli, word: &str, candidates: &Candidates<'a>) -> Result<Named<'a>, String> {
    let why = match resolve(cli, word, candidates) {
        Ok(named) => return Ok(named),
        Err(why) if why == aterm_file::no_account(word) => why,
        Err(why) => return Err(why),
    };
    let Some((provider, address)) = word.split_once(' ').or_else(|| word.split_once(':')) else {
        return Err(why);
    };
    let (Some(realm), Ok(email)) = (
        keys::canonical_realm(provider).filter(|_| is_provider_word(provider)),
        keys::address(address),
    ) else {
        return Err(why);
    };
    if !realms(cli).contains(&realm) {
        return Err(elsewhere(word, cli, &[realm], OtherPool::SaysCli));
    }
    let pk = key_of(realm, &email).ok_or(why)?;
    // The resolver matches exactly, and an address is an account's however its case is spelled
    // (`claude Ada@x.example`): an account a row here or the lab holds is that account.
    if let Ok(named) = resolve(cli, &pk, candidates) {
        return Ok(named);
    }
    Ok(Named {
        pk,
        row: None,
        names: AccountNames {
            realm: realm.to_owned(),
            slug: String::new(),
            name: keys::account_name(realm, &email),
            email: Some(email),
            login: Some(address.trim_ascii().to_owned()),
            live: true,
        },
    })
}

/// The account of `cli`'s pool a login signed in to as `email` (Kimi Code's in `region`) names,
/// among `candidates`: its key and names as [`names_of_login`] makes them, with the roster row of
/// that account where one holds it. `None` for text that is no address.
#[must_use]
pub fn login_named<'a>(
    cli: Cli,
    region: Option<Region>,
    email: &str,
    candidates: &Candidates<'a>,
) -> Option<Named<'a>> {
    let (pk, names) = names_of_login(cli, region, email, candidates.lab)?;
    let row = candidates
        .roster
        .iter()
        .find(|row| names_of_row(cli, row, candidates.lab).is_some_and(|(held, _)| held == pk));
    Some(Named { pk, row, names })
}

/// Whether `word` is a provider word (`claude`, `oai`, `kimicn`, …,
/// [`keys::provider_word`]).
fn is_provider_word(word: &str) -> bool {
    keys::canonical_realm(word).and_then(keys::provider_word) == Some(word)
}

/// `operands` with each provider word followed by an address joined into one name (`["use",
/// "claude", "ada@x.example"]` is `["use", "claude ada@x.example"]`): the space form of a
/// provider word and address arrives as two argv words.
///
/// `operands` are the positional words an accounts verb's parse leaves once it has taken its
/// flags and their values (`cli::parse`'s `rest`), never raw argv: a provider word is also a
/// flag's value (`--cli claude`, `--cli kimi`, `--browser google`), which a join would swallow.
/// So that a raw argv still fails safe, a provider word right after a word that starts with `-`
/// is never joined. The word after a provider word joins only where it is an address
/// ([`keys::address`]), never a tab ID such as `@t12`.
#[must_use]
pub fn words(operands: &[&str]) -> Vec<String> {
    let mut out = Vec::with_capacity(operands.len());
    let mut at = 0;
    while at < operands.len() {
        let after_flag = at
            .checked_sub(1)
            .is_some_and(|before| operands[before].starts_with('-'));
        match (operands[at], operands.get(at + 1)) {
            (word, Some(address))
                if !after_flag && is_provider_word(word) && keys::address(address).is_ok() =>
            {
                out.push(format!("{word} {address}"));
                at += 2;
            }
            (word, _) => {
                out.push(word.to_owned());
                at += 1;
            }
        }
    }
    out
}

#[cfg(test)]
#[path = "account_names_tests.rs"]
pub(crate) mod tests;
