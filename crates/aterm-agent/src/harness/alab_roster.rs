// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The harness's side of LabView (`docs/DESIGN-alab-lab-model-2026-11-01.md` §0, §7, §9, step
//! 11): a roster from the lab registry's preferences instead of a sheet.
//!
//! `aterm-agent` does not link `aterm-db`, so the registry reaches it as a [`Lab`] the `aterm`
//! executable projects in the background and hands over through a [`Source`] it installs at
//! start (`crates/aterm/src/alab_view.rs`), as the journal's sink is. With none installed there is
//! no lab and every sync reads the sources it always read: a Mac without the registry keeps its
//! rows and its fleet files.
//!
//! - [`fleet_for`]: the `alab` source, the fleet a sync of one CLI reads: this Mac's preferred
//!   accounts, preferred first in rank order. A preference orders a pool and never limits it, so
//!   the sync keeps every row already on this Mac whatever the lab says of it ([`retain`]) and
//!   adds only accounts this Mac prefers; nothing here drops, refuses or signs out a row.
//! - [`migrate`]: once this Mac's registry inventory has equalled a peer's ([`Lab::settled`]),
//!   each row whose account the lab calls by another label takes that label, its login
//!   directory pinned where it is, its records moved with it ([`accounts::move_accounts`]) in one
//!   crash-safe commit (the sync receipt, `accounts_sync_commit.rs`).
//! - [`mail_targets`]: the addresses an alias's auth codes arrive at, for the mail routes, and
//!   [`legacy_labels`]: the labels a renamed account's label-keyed sign-in secrets still name.

use super::accounts::{self, Class, Cli, Roster, Row, Store, Vendor};
use super::accounts_sync::{self, FleetAccount, FleetRoster};
use std::path::Path;
use std::sync::{Arc, OnceLock};

/// The source word a sync names the lab by (`aterm harness accounts sync alab`), and the
/// [`FleetRoster::source`] of the fleet it reads.
pub const SOURCE: &str = "alab";

/// The registry as this Mac's harness reads it: what `LabView` projects, with the addresses an
/// account row holds. It stays in this process and is never printed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Lab {
    /// This Mac's machine label (`m22`), the machine its own facts row binds; `None` where this
    /// Mac has no live binding yet, which leaves every sync on the sources it always read.
    pub me: Option<String>,
    /// This Mac's registry inventory has equalled an authenticated peer's: until then the
    /// registry here may be a part of the lab's, so the lab adds rows and renames none.
    pub settled: bool,
    /// Every account of a pooled CLI, live or not.
    pub accounts: Vec<LabAccount>,
    /// Each address with an `alias_of` link, lower-cased, to the live addresses its auth codes
    /// arrive at, nearest first ([`mail_targets`]).
    pub aliases: std::collections::BTreeMap<String, Vec<String>>,
}

/// One account of a pooled CLI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LabAccount {
    /// The canonical account key (`ac-…`).
    pub pk: String,
    pub cli: Cli,
    /// `region` for Kimi Code, `flow` for Codex and Grok Build.
    pub vendor: Vendor,
    /// What the lab calls it: a name that names no machine (`claude-1a2b3c`, or the owner's).
    pub label: String,
    /// What a Mac called its row before (`m8-2`, `m106-m38`, `kimi-m16`).
    pub legacy_label: Option<String>,
    /// The login address.
    pub email: String,
    /// The provider identity, where the login is an alias.
    pub identity: Option<String>,
    pub browser: Option<String>,
    /// A live row whose status lets a sign-in be tried: only such an account is added unasked.
    pub joinable: bool,
    /// The machines that prefer it, by label, smaller rank first, then the smaller machine number.
    pub preferred: Vec<(String, Option<u64>)>,
}

impl LabAccount {
    /// Its rank on `machine`, where `machine` prefers it.
    #[must_use]
    pub fn preferred_on(&self, machine: &str) -> Option<Option<u64>> {
        self.preferred
            .iter()
            .find(|(label, _)| label == machine)
            .map(|(_, rank)| *rank)
    }

    /// Whether `row` is this account's: its address or provider identity, in the same realm.
    #[must_use]
    pub fn is_row(&self, row: &Row) -> bool {
        accounts::same_provider_realm(self.cli, row.vendor.region, self.vendor.region)
            && [Some(self.email.as_str()), self.identity.as_deref()]
                .into_iter()
                .flatten()
                .any(|email| row.matches_email(email))
    }
}

impl Lab {
    /// `cli`'s accounts.
    pub fn of(&self, cli: Cli) -> impl Iterator<Item = &LabAccount> {
        self.accounts.iter().filter(move |a| a.cli == cli)
    }

    /// The one account of `cli` that `row` is; `None` for a row the lab does not know, or one
    /// two accounts could be.
    #[must_use]
    pub fn account_of(&self, cli: Cli, row: &Row) -> Option<&LabAccount> {
        let mut found = self.of(cli).filter(|account| account.is_row(row));
        let first = found.next()?;
        found.next().is_none().then_some(first)
    }
}

/// What gives the harness its [`Lab`]: the `aterm` executable's cache.
pub trait Source: Send + Sync {
    /// The lab as last projected, at once; `None` while nothing is projected, or where the
    /// registry is not held here or the roster source is turned off.
    fn lab(&self) -> Option<Arc<Lab>>;
}

static INSTALLED: OnceLock<Box<dyn Source>> = OnceLock::new();

/// Install the process's [`Source`], once; a second install is refused.
pub fn install(source: Box<dyn Source>) -> bool {
    INSTALLED.set(source).is_ok()
}

#[cfg(test)]
thread_local! {
    static OVERRIDE: std::cell::RefCell<Option<Arc<Lab>>> = const { std::cell::RefCell::new(None) };
}

/// The lab as this process holds it now.
#[must_use]
pub fn current() -> Option<Arc<Lab>> {
    #[cfg(test)]
    if let Some(lab) = OVERRIDE.with(|cell| cell.borrow().clone()) {
        return Some(lab);
    }
    INSTALLED.get()?.lab()
}

/// Run `f` with `lab` as this thread's [`current`] lab.
#[cfg(test)]
pub(crate) fn with_lab<T>(lab: Lab, f: impl FnOnce() -> T) -> T {
    OVERRIDE.with(|cell| *cell.borrow_mut() = Some(Arc::new(lab)));
    let out = f();
    OVERRIDE.with(|cell| *cell.borrow_mut() = None);
    out
}

/// The `alab` fleet of `cli` for this Mac ([`SOURCE`]): every account it prefers that may join,
/// smaller rank first, each under the label `existing` already gives it (a row is renamed only
/// by [`migrate`], never by a sync), else the lab's. `None` where the lab names no machine for
/// this Mac. Every row has this Mac alone as its machine and [`Class::Dedicated`] (preferred
/// here); a row this Mac has that the fleet does not list stays, by [`retain`].
#[must_use]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AlabCells",
        action = "Sync",
        project = "aterm_agent::harness::alab_roster::tests::preference_transition"
    )
)]
pub fn fleet_for(lab: &Lab, cli: Cli, existing: Option<&Roster>) -> Option<FleetRoster> {
    let me = lab.me.clone()?;
    let mut mine: Vec<(&LabAccount, Option<u64>)> = lab
        .of(cli)
        .filter(|account| account.joinable)
        .filter_map(|account| Some((account, account.preferred_on(&me)?)))
        .collect();
    mine.sort_by(|(a, ra), (b, rb)| {
        (ra.is_none(), ra, &a.label).cmp(&(rb.is_none(), rb, &b.label))
    });
    let mut accounts: Vec<FleetAccount> = Vec::with_capacity(mine.len());
    for (account, _) in mine {
        let kept = existing.and_then(|roster| {
            let mut rows = roster.rows.iter().filter(|row| account.is_row(row));
            let row = rows.next()?;
            rows.next().is_none().then_some(row)
        });
        let label = kept.map_or_else(|| account.label.clone(), |row| row.label.clone());
        // A label an existing row of another account holds is that row's: the account is not
        // added under it, so a sync never rebuilds that row with another address in its login.
        let held_by_another = kept.is_none()
            && existing.is_some_and(|roster| {
                roster
                    .rows
                    .iter()
                    .any(|row| row.label == label && !account.is_row(row))
            });
        if held_by_another
            || accounts::label_refusal(&label).is_some()
            || accounts.iter().any(|a| a.label == label)
        {
            continue;
        }
        accounts.push(FleetAccount {
            label,
            machines: vec![me.clone()],
            class: Class::Dedicated,
            email: account.email.clone(),
            browser: account.browser.clone(),
            vendor: match cli {
                Cli::Claude => Vendor::default(),
                _ => account.vendor,
            },
        });
    }
    Some(FleetRoster {
        source: SOURCE.to_owned(),
        accounts,
        alab: Some(me),
        ..FleetRoster::default()
    })
}

/// The `alab` fleet a sync of `cli` reads, given the source it was `named` (none, or a path or
/// URL), its roster at `roster_path` and the `aterm.toml` at `config`: with [`SOURCE`] named,
/// the lab's fleet or a refusal where no lab names this Mac; with another source named, or for
/// Claude Code a source the owner set in `config` (`[fleet] accounts_source`), none, so that
/// source is read; with none named, the lab's fleet ahead of every other default once the lab
/// names this Mac and either prefers an account here or this Mac already keeps rows (a new Mac
/// the lab says nothing of yet keeps reading what it always read). The roster is read without
/// the lock, only to keep each account's label; the sync reads it again under the lock.
///
/// # Errors
/// [`SOURCE`] was named and no lab names this Mac.
pub(super) fn chosen(
    cli: Cli,
    roster_path: Option<&Path>,
    named: Option<&str>,
    config: Option<&Path>,
) -> Result<Option<FleetRoster>, String> {
    let named = named.map(str::trim).filter(|s| !s.is_empty());
    if named.is_some_and(|named| named != SOURCE) {
        return Ok(None);
    }
    if named.is_none() && cli == Cli::Claude && config.and_then(accounts_sync::source_key).is_some()
    {
        return Ok(None);
    }
    let existing = roster_path.and_then(|path| Roster::load_for(cli, path).ok());
    let fleet = current().and_then(|lab| fleet_for(&lab, cli, existing.as_ref()));
    if named.is_some() {
        return fleet.map(Some).ok_or_else(|| {
            "the lab registry names no machine for this Mac yet (its facts row binds none, or \
             this Mac does not hold the registry), so the `alab` source has nothing to read"
                .to_owned()
        });
    }
    Ok(fleet.filter(|fleet| {
        !fleet.accounts.is_empty() || existing.is_some_and(|roster| !roster.rows.is_empty())
    }))
}

/// `roster`, a sync's rebuild from the `alab` fleet, with every row of `existing` it lost put
/// back as it was (its label, login directory, address and keys), so a preference never drops a
/// row; and each row the lab knows given its derived class: [`Class::Dedicated`] where `machine`
/// prefers its account, else [`Class::Shared`], which a pool tries after the preferred ones. A
/// row the lab does not know keeps its class. A row is matched by its account, and a rebuilt row
/// holding an existing row's label under another address gives the label back to that row. No
/// row keeps a `held_by`: nothing under the lab limits who runs an account.
/// [`accounts::MAX_ACCOUNTS`] still bounds a roster: an existing row is put back before any row
/// the fleet added is kept.
#[must_use]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AlabCells",
        action = "Sync",
        project = "aterm_agent::harness::alab_roster::tests::preference_transition"
    )
)]
pub fn retain(
    mut roster: Roster,
    existing: &Roster,
    lab: Option<&Lab>,
    cli: Cli,
    machine: &str,
) -> Roster {
    for prev in &existing.rows {
        if roster
            .rows
            .iter()
            .any(|row| accounts_sync::same_account(prev, row))
        {
            continue;
        }
        match roster.rows.iter().position(|row| row.label == prev.label) {
            // A row that names no address is known by its label alone.
            Some(_) if prev.expected_identity_email().is_none() => {}
            // Another account rebuilt under this row's label: the row this Mac had takes it
            // back, so its login directory never holds another account's sign-in.
            Some(at) => roster.rows[at] = prev.clone(),
            None => roster.rows.push(prev.clone()),
        }
    }
    while roster.rows.len() > accounts::MAX_ACCOUNTS {
        let Some(added) = roster
            .rows
            .iter()
            .rposition(|row| existing.row(&row.label).is_none())
        else {
            break;
        };
        roster.rows.remove(added);
    }
    for row in &mut roster.rows {
        // A hold an old pool file set would outlive its source: under the lab nothing limits
        // who runs an account here.
        row.held_by = None;
        if let Some(account) = lab.and_then(|lab| lab.account_of(cli, row)) {
            row.class = if account.preferred_on(machine).is_some() {
                Class::Dedicated
            } else {
                Class::Shared
            };
        }
    }
    roster
        .rows
        .sort_by_key(|r| (r.dir.is_some(), r.class, r.label.clone()));
    roster
}

/// `AlabCells`' preference area besides its `Sync`: the owner's verbs and a person's acts, which
/// `a_sync_takes_the_preference_models_steps` sets in the lab and roster each sync reads.
#[cfg(any(test, debug_assertions))]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AlabCells",
        action = "Prefer",
        reason = "A preference is the owner's `lab prefer`, a row of the registry aterm-db writes: the conformance test names this Mac among the account's preferred machines in the Lab the sync reads."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AlabCells",
        action = "Unprefer",
        reason = "Withdrawing a preference is the owner's verb in the registry: the conformance test names another Mac as the account's preferred machine in the Lab the sync reads."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AlabCells",
        action = "Pause",
        reason = "A pause is the account's status in the registry, which LabView projects as `joinable`: the conformance test clears it in the Lab the sync reads."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AlabCells",
        action = "Ask",
        reason = "A person adding an account here is a roster row this Mac already has: the conformance test writes it into the roster the sync starts from."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AlabCells",
        action = "Hold",
        reason = "A hold is the `held_by` an old pool file left on a row: the conformance test writes it into the roster the sync starts from."
    )
)]
pub fn alab_preference_environment() {}

/// What [`migrate`] did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Migrated {
    /// `(old label, new label)` for each row renamed.
    pub moved: Vec<(String, String)>,
    /// Rows left under their label: the row's label is not its account's legacy label, or the
    /// new label is refused, held by another row or retained in the store ([`renames`]).
    pub kept: Vec<String>,
}

/// The renames `lab` asks of `roster`: each row whose one lab account has another label, where
/// the row's label is the one the lab recorded the account under before
/// ([`LabAccount::legacy_label`]), and the new label is admissible, held by no other row and
/// not `retained` (records the store still keeps under it, which [`accounts::move_accounts`]
/// refuses). The legacy label is what keeps a label-keyed sign-in secret (a password, a mailbox
/// password, a TOTP seed named `<label>`) reaching the account after the rename: the mailbox
/// config maps it to the account's address from the lab (`headless_oauth::mailbox_aliases`), so
/// a row under any other label, whose secrets nothing would map, keeps its label. Pure.
#[must_use]
pub fn renames(lab: &Lab, cli: Cli, roster: &Roster, retained: impl Fn(&str) -> bool) -> Migrated {
    let mut out = Migrated::default();
    for row in &roster.rows {
        let Some(account) = lab.account_of(cli, row) else {
            continue;
        };
        if account.label == row.label {
            continue;
        }
        let taken = roster.rows.iter().any(|other| other.label == account.label)
            || out.moved.iter().any(|(_, to)| *to == account.label)
            || retained(&account.label);
        let bridged = account.legacy_label.as_deref() == Some(row.label.as_str());
        if taken || !bridged || accounts::label_refusal(&account.label).is_some() {
            out.kept.push(row.label.clone());
        } else {
            out.moved.push((row.label.clone(), account.label.clone()));
        }
    }
    out
}

/// Give each row of `cli`'s roster at `roster_path` the label the lab calls its account by
/// ([`renames`], against the store's records as the lock holds them), once the lab is
/// [`Lab::settled`]: the roster's new text, the state's and the
/// bindings' moved records ([`accounts::move_accounts`]: observations, selections, sessions,
/// claims) go in one commit under the store's writer, so a crash leaves the old files or a
/// receipt the next run (or any reader's recovery) completes. A row's login directory is pinned
/// where it is: the default login stays the default (no `dir`), and every other row keeps the
/// exact `dir` it had, so a running session's [`accounts::LOGIN_ENV`] still names its row and
/// nobody is signed out. A second run finds every label already the lab's and writes nothing.
///
/// # Errors
/// The writer lock is held past `ladder`, an interrupted commit cannot be completed, the roster
/// or the store cannot be read, or the moves are refused (a destination label still holds
/// records): nothing is written.
pub fn migrate(
    lab: &Lab,
    cli: Cli,
    store: &Store,
    roster_path: &Path,
    now: i64,
    ladder: &[std::time::Duration],
) -> Result<Migrated, String> {
    if !lab.settled {
        return Ok(Migrated::default());
    }
    let held = store
        .acquire_within(ladder)
        .map_err(|why| format!("{}: {why}", store.dir.display()))?
        .ok_or_else(|| format!("{} at {}", accounts::LOCK_HELD, store.dir.display()))?;
    let (write_to, keep_mode) = super::cli::roster_target(roster_path)?;
    store.recover_sync_held(&held, &write_to)?;
    let before = accounts::roster_text(&write_to)?;
    let Some(text) = before.as_deref() else {
        return Ok(Migrated::default());
    };
    let roster = Roster::parse_for(cli, text)?;
    let (state, _) = store.load(now)?;
    let (logins, _) = store.load_logins(now)?;
    let planned = renames(lab, cli, &roster, |label| {
        accounts::label_is_retained(&state, &logins, label)
    });
    if planned.moved.is_empty() {
        return Ok(planned);
    }
    let mut renamed = roster.clone();
    for row in &mut renamed.rows {
        if let Some((_, to)) = planned.moved.iter().find(|(from, _)| *from == row.label) {
            row.label.clone_from(to);
        }
    }
    let after = accounts::append_rows_for(cli, "", &renamed.rows)?;
    let pairs: Vec<(&str, &str)> = planned
        .moved
        .iter()
        .map(|(from, to)| (from.as_str(), to.as_str()))
        .collect();
    store.transact_sync_held(
        &held,
        now,
        &mut None,
        accounts::SyncTarget {
            path: &write_to,
            before: Some(text),
            after: Some(&after),
            mode: keep_mode,
        },
        |state, logins| {
            accounts::move_accounts(state, logins, &pairs)?;
            Ok(Vec::new())
        },
        || Ok(()),
    )?;
    Ok(planned)
}

/// What a sync of `cli` runs before it reads its source, where that source is the lab's
/// ([`chosen`] picks it for the source `named` and the `aterm.toml` at `config`): [`migrate`]
/// with the lab this process holds, so the fleet it then reads ([`fleet_for`]) already finds
/// every row under the lab's label and the sync renames nothing.
///
/// # Errors
/// As [`migrate`].
pub(super) fn migrate_before_sync(
    cli: Cli,
    store: &Store,
    (roster_path, config): (&Path, Option<&Path>),
    named: Option<&str>,
    now: i64,
    ladder: &[std::time::Duration],
) -> Result<Migrated, String> {
    match (current(), chosen(cli, Some(roster_path), named, config)) {
        (Some(lab), Ok(Some(_))) => migrate(&lab, cli, store, roster_path, now, ladder),
        // The sync's own read of its source says why the lab is not one.
        _ => Ok(Migrated::default()),
    }
}

/// The addresses whose mail routes receive `email`'s auth codes besides its own: the lab's
/// `alias_of` chain, nearest first (§0: an alias is linked with the address its auth login is
/// received at). Empty where no lab is held.
#[must_use]
pub fn mail_targets(email: &str) -> Vec<String> {
    current()
        .and_then(|lab| lab.aliases.get(&email.to_ascii_lowercase()).cloned())
        .unwrap_or_default()
}

/// Each `(cli, legacy label, login address)` of the lab this process holds where the account's
/// label is no longer its legacy one: what the mailbox config maps a `<label>` password, mailbox
/// password or TOTP seed through after [`migrate`] renamed the row ([`renames`] moves only a row
/// under its legacy label). Empty where no lab is held.
#[must_use]
pub fn legacy_labels() -> Vec<(Cli, String, String)> {
    current()
        .map(|lab| {
            lab.accounts
                .iter()
                .filter_map(|account| {
                    let legacy = account.legacy_label.as_deref()?;
                    (legacy != account.label)
                        .then(|| (account.cli, legacy.to_owned(), account.email.clone()))
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
#[path = "alab_roster_tests.rs"]
mod tests;
