// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The generated exports (`docs/DESIGN-alab-lab-model-2026-11-01.md` §9, step 11): for one
//! release, the files older Macs and Macs the lab does not name still read (Claude Code's
//! `fleet-accounts.toml`, each other pooled CLI's `ai-accounts-<cli>.toml` and `ai-pool.toml`),
//! rendered from the lab, pure.
//!
//! An older build reads these files as limits (`accounts_for_machine`, a warm list), so a file
//! that left out an account a Mac runs would have its next sync drop that row and sign it out.
//! Each row therefore lists every Mac that holds its account, not only those that prefer it:
//!
//! - the machines that prefer it, in its preference order ([`Lab::holders`]);
//! - then the machines whose live `place` rows say they run it ([`Lab::places`]);
//! - then every machine the file already lists for it.
//!
//! No machine the current file lists is ever dropped. The lab cannot tell which Macs read a file
//! as a limit: a `sum` row saying `journal: on` comes from `[alab] journal` alone, and a Mac with
//! `[alab] roster = false`, a dev build or a build from before the place writer states no
//! holdings while it reads these files. A file reader's row is the status quo it already runs, so
//! carrying it signs no one out; a stale mapping of a Mac that reads the lab costs only a line in
//! a file that Mac does not read. A row whose account the lab does not know keeps its label and
//! every Mac it lists, and the lab's accounts take other labels. A paused, retired or barred
//! account gains no machine from a preference. A row of a lab account keeps the label the file
//! already gives it (what older Macs call their row), else its legacy label, else the lab's.
//!
//! `ai-pool.toml`'s warm list is cut to [`ai_pool::MAX_WARM_SLOTS`] (`ai_pool.rs` refuses the
//! whole file past that; the limit is the old file's, never the registry's), so its order is the
//! other way round: the Macs the current pool already lists first (an older Mac reads this file
//! ahead of every other source, so a slot it loses signs it out), then the preferring Macs by rank
//! and the smaller machine number, then the place-row Macs (which read the lab and gain nothing
//! from the file). The writer (`aterm db alab export`) rewrites only files the vault already
//! holds, since a new file would switch every older Mac's source to it. Every byte is a function
//! of the lab and the current files, so an unchanged lab renders the same bytes and the writer
//! writes nothing.

use super::accounts::{self, Class, Cli, Vendor};
use super::accounts_sync::{self, FleetAccount, FleetRoster};
use super::ai_pool::{self, Pool, PoolAccount, SlotWhy};
use super::alab_roster::{Lab, LabAccount};
use std::collections::BTreeMap;

/// One generated file: its name in `~/.secrets` and its text, without the generator's seal line
/// (the writer adds it).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Export {
    pub name: String,
    pub body: String,
}

/// The files [`render`] makes, by name: Claude Code's fleet, each other pooled CLI's, the pool.
#[must_use]
pub fn names() -> Vec<String> {
    let mut out: Vec<String> = Cli::ALL.into_iter().map(fleet_file).collect();
    out.push(ai_pool::AI_POOL_FILE.to_owned());
    out
}

/// `cli`'s fleet file.
fn fleet_file(cli: Cli) -> String {
    match cli {
        Cli::Claude => accounts_sync::FLEET_ROSTER_FILE.to_owned(),
        _ => accounts_sync::cli_fleet_file(cli),
    }
}

/// Whether `account` is the account a file's row of `cli` names: its address or provider
/// identity, in the same realm.
fn names_account(account: &LabAccount, cli: Cli, email: &str, vendor: Vendor) -> bool {
    account.cli == cli
        && accounts::same_provider_realm(cli, vendor.region, account.vendor.region)
        && [Some(account.email.as_str()), account.identity.as_deref()]
            .into_iter()
            .flatten()
            .any(|own| accounts::email_matches(own, email))
}

/// Whether the lab knows the account a file's row of `cli` names.
fn known(lab: &Lab, cli: Cli, email: &str, vendor: Vendor) -> bool {
    lab.of(cli)
        .any(|account| names_account(account, cli, email, vendor))
}

/// Which of [`machines`]' two halves comes first.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Order {
    /// The lab's holders, then the listed Macs (a fleet file, which has no cap).
    HoldersFirst,
    /// The listed Macs, then the lab's holders (the pool's warm list, which is cut).
    ListedFirst,
}

/// The machines that hold `account`, as old files spell a machine: the lab's holders
/// ([`Lab::holders`]; only its place rows where the account may not be signed in, so a
/// preference never sends a Mac to sign in to it) and every machine of `listed`, in `order`.
fn machines(
    lab: &Lab,
    account: Option<&LabAccount>,
    listed: &[String],
    order: Order,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |machine: &str| {
        if let Some(machine) = accounts_sync::normalize_machine_id(machine)
            && !out.contains(&machine)
        {
            out.push(machine);
        }
    };
    if order == Order::ListedFirst {
        for machine in listed {
            push(machine);
        }
    }
    if let Some(account) = account {
        let holders = if account.joinable {
            lab.holders(account)
        } else {
            lab.places
                .by_account
                .get(&account.pk)
                .into_iter()
                .flatten()
                .map(|(machine, _)| machine.clone())
                .collect()
        };
        for machine in &holders {
            push(machine);
        }
    }
    if order == Order::HoldersFirst {
        for machine in listed {
            push(machine);
        }
    }
    out
}

/// The label a row of `account` takes: the one `held` (what the file already calls it) where it
/// is admissible and no row taken so far holds it, else its legacy label, else the lab's.
fn label_for(account: &LabAccount, held: Option<&str>, taken: &[String]) -> Option<String> {
    [
        held,
        account.legacy_label.as_deref(),
        Some(account.label.as_str()),
    ]
    .into_iter()
    .flatten()
    .find(|label| accounts::label_refusal(label).is_none() && !taken.iter().any(|t| t == label))
    .map(str::to_owned)
}

/// `cli`'s fleet file from `lab`, carrying every Mac `current` (the file as it is) lists.
fn fleet(lab: &Lab, cli: Cli, current: Option<&FleetRoster>) -> FleetRoster {
    let held = current.map_or(&[][..], |fleet| fleet.accounts.as_slice());
    let mut rows: Vec<FleetAccount> = Vec::new();
    // A row naming no account the lab knows keeps its label and every Mac it lists; its label is
    // taken before any lab account picks one.
    let mut taken: Vec<String> = Vec::new();
    for row in held {
        if known(lab, cli, &row.email, row.vendor) || taken.contains(&row.label) {
            continue;
        }
        let machines = machines(lab, None, &row.machines, Order::HoldersFirst);
        if machines.is_empty() {
            continue;
        }
        taken.push(row.label.clone());
        rows.push(FleetAccount {
            class: class(&machines),
            machines,
            ..row.clone()
        });
    }
    for account in lab.of(cli) {
        let row = held
            .iter()
            .find(|row| names_account(account, cli, &row.email, row.vendor));
        let machines = machines(
            lab,
            Some(account),
            row.map_or(&[][..], |r| &r.machines),
            Order::HoldersFirst,
        );
        if machines.is_empty() {
            continue;
        }
        let Some(label) = label_for(account, row.map(|r| r.label.as_str()), &taken) else {
            continue;
        };
        taken.push(label.clone());
        rows.push(FleetAccount {
            label,
            class: class(&machines),
            machines,
            email: account.email.clone(),
            browser: account.browser.clone(),
            vendor: vendor(cli, account.vendor),
        });
    }
    rows.sort_by(|a, b| a.label.cmp(&b.label));
    FleetRoster {
        source: super::alab_roster::SOURCE.to_owned(),
        accounts: rows,
        ..FleetRoster::default()
    }
}

/// The legacy class word a row of `machines` reads by: one Mac `dedicated`, several `shared`,
/// as an older reader infers it with no `class` written.
fn class(machines: &[String]) -> Class {
    if machines.len() > 1 {
        Class::Pool
    } else {
        Class::Preferred
    }
}

fn vendor(cli: Cli, vendor: Vendor) -> Vendor {
    match cli {
        Cli::Claude => Vendor::default(),
        _ => vendor,
    }
}

/// Each warm slot's reason: `pin` where the machine prefers the account, its place row's reason
/// where one says it runs there, else the reason the current pool gave that slot (`pin` if none).
fn slot_why(
    lab: &Lab,
    account: Option<&LabAccount>,
    machine: &str,
    current: Option<&PoolAccount>,
) -> SlotWhy {
    if let Some(account) = account {
        if account.preferred_on(machine).is_some() {
            return SlotWhy::Pin;
        }
        if let Some(why) = lab
            .places
            .by_account
            .get(&account.pk)
            .into_iter()
            .flatten()
            .find(|(m, _)| m == machine)
            .and_then(|(_, why)| SlotWhy::parse(why))
        {
            return why;
        }
    }
    current
        .and_then(|row| {
            let at = row.warm.iter().position(|m| m == machine)?;
            row.why.get(at).copied()
        })
        .unwrap_or(SlotWhy::Pin)
}

/// One pool row's warm list: [`machines`] listed first, cut to the pool's limit.
fn warm(lab: &Lab, account: Option<&LabAccount>, current: Option<&PoolAccount>) -> Vec<String> {
    machines(
        lab,
        account,
        current.map_or(&[][..], |r| &r.warm),
        Order::ListedFirst,
    )
    .into_iter()
    .take(ai_pool::MAX_WARM_SLOTS)
    .collect()
}

/// `ai-pool.toml` from `lab`, carrying every Mac `current` lists and its `last_heard` days.
fn pool(lab: &Lab, current: Option<&Pool>) -> Pool {
    let held = current.map_or(&[][..], |pool| pool.accounts.as_slice());
    let mut rows: Vec<PoolAccount> = Vec::new();
    for cli in Cli::ALL {
        let mut taken: Vec<String> = Vec::new();
        for row in held.iter().filter(|row| row.provider == cli) {
            if known(lab, cli, &row.email, row.vendor)
                || row
                    .label
                    .as_ref()
                    .is_some_and(|label| taken.contains(label))
            {
                continue;
            }
            let warm = warm(lab, None, Some(row));
            if warm.is_empty() {
                continue;
            }
            taken.extend(row.label.clone());
            rows.push(PoolAccount {
                why: warm
                    .iter()
                    .map(|m| slot_why(lab, None, m, Some(row)))
                    .collect(),
                warm,
                since: Vec::new(),
                used: Vec::new(),
                ..row.clone()
            });
        }
        for account in lab.of(cli) {
            let row = held.iter().find(|row| {
                row.provider == cli && names_account(account, cli, &row.email, row.vendor)
            });
            let warm = warm(lab, Some(account), row);
            if warm.is_empty() {
                continue;
            }
            let Some(label) = label_for(account, row.and_then(|r| r.label.as_deref()), &taken)
            else {
                continue;
            };
            taken.push(label.clone());
            rows.push(PoolAccount {
                provider: cli,
                vendor: account.vendor,
                label: Some(label),
                email: account.email.clone(),
                browser: account.browser.clone(),
                why: warm
                    .iter()
                    .map(|m| slot_why(lab, Some(account), m, row))
                    .collect(),
                warm,
                since: Vec::new(),
                used: Vec::new(),
            });
        }
    }
    Pool {
        source: super::alab_roster::SOURCE.to_owned(),
        last_heard: current
            .map(|pool| pool.last_heard.clone())
            .unwrap_or_default(),
        accounts: rows,
    }
}

/// Every export of `lab` (§9), given `current`, each file's text as it is now by name (a file
/// not there is absent). Pure. A file that is not there renders too, for the drift report; the
/// writer never creates one.
///
/// # Errors
/// A current file is there and does not parse: which Macs it lists is unknown, so nothing is
/// rendered over it until it is fixed or removed.
pub fn render(lab: &Lab, current: &BTreeMap<String, String>) -> Result<Vec<Export>, String> {
    let mut out = Vec::new();
    for cli in Cli::ALL {
        let name = fleet_file(cli);
        let held = current
            .get(&name)
            .map(|text| FleetRoster::from_toml_for(cli, text, &name))
            .transpose()?;
        out.push(Export {
            body: fleet(lab, cli, held.as_ref()).to_toml(),
            name,
        });
    }
    let name = ai_pool::AI_POOL_FILE.to_owned();
    let held = current
        .get(&name)
        .map(|text| Pool::from_toml(text, &name))
        .transpose()?;
    out.push(Export {
        body: pool(lab, held.as_ref()).to_toml(),
        name,
    });
    Ok(out)
}

#[cfg(test)]
#[path = "alab_exports_tests.rs"]
mod tests;
