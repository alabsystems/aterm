// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm db alab` with the harness's reading of the account sheet
//! (docs/DESIGN-alab-lab-model-2026-11-01.md §9): the lab importer lives in `aterm-db`, which
//! does not link the harness, so this executable hands it `accounts_sync::read_sheet_cell`. The
//! importer and every Mac's pool then read one sheet one way: Kimi Code's bare `1` held, Codex
//! and Grok Build's flow words, a disqualifying note pausing the vendor it names (or every
//! vendor when it names none) and, as Claude Code's pool reads it, Claude Code whatever it names.
//! The generated exports (`aterm db alab export`, §9) are rendered here the same way, by the
//! harness's own writers (`alab_exports`), so an older Mac reads them with the parser it runs.

use aterm_agent::harness::{
    accounts::{Cli, Vendor},
    accounts_sync::{FleetRoster, SheetCell, SheetColumn, read_sheet_cell},
    ai_pool::Pool,
};
use aterm_db::{
    alab::import::{Mark, SheetKind, Stated, Terms, Vendors},
    typed::keys,
};
use std::{ffi::OsString, process::ExitCode};

/// The harness's reading of a provider cell.
pub(crate) struct Harness;

fn one_file_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains('\0')
}

impl Vendors for Harness {
    fn mark(&self, provider: &str, cell: &str, notes: &str) -> Mark {
        let column = match provider {
            keys::ANTHROPIC => SheetColumn::Pool(Cli::Claude),
            keys::OPENAI => SheetColumn::Pool(Cli::Codex),
            keys::XAI => SheetColumn::Pool(Cli::Grok),
            keys::KIMI => SheetColumn::Pool(Cli::Kimi),
            keys::GOOGLE => SheetColumn::Google,
            keys::META => SheetColumn::Meta,
            _ => return Mark::Unread,
        };
        let terms = |vendor: Vendor| Terms {
            region: vendor.region.map(|region| region.as_str()),
            flow: vendor.flow.map(|flow| flow.as_str()),
        };
        match read_sheet_cell(column, cell, notes) {
            SheetCell::Absent => Mark::Absent,
            SheetCell::Member(vendor) => Mark::Member(terms(vendor)),
            SheetCell::Disqualified(vendor) => Mark::Paused(terms(vendor)),
            SheetCell::Held => Mark::Held,
            SheetCell::Unread => Mark::Unread,
        }
    }

    /// This Mac, read as a registration pass reads it.
    fn this_mac(&self) -> Option<aterm_db::alab::register::ThisMac> {
        crate::alab_register::this_mac()
    }

    fn export_names(&self) -> Vec<String> {
        aterm_agent::harness::alab_exports::names()
    }

    /// The generated exports, rendered by the harness from the registry and the journal's place
    /// rows (`alab_exports::render`); no mailbox login is read for them.
    fn exports(
        &self,
        view: &aterm_db::alab::view::LabView,
        places: &aterm_db::alab::journal::place::Places,
        current: &std::collections::BTreeMap<String, String>,
    ) -> Result<Vec<(String, String)>, String> {
        let mut lab = crate::alab_view::harness_lab(view, false, &Default::default());
        lab.places = crate::alab_view::placements(view, places);
        Ok(aterm_agent::harness::alab_exports::render(&lab, current)?
            .into_iter()
            .map(|export| (export.name, export.body))
            .collect())
    }

    /// A fleet file through the harness's own parser (the file's name says whose it is, as
    /// `cli_fleet_file` names them), or `ai-pool.toml`'s warm lists.
    fn stated(&self, kind: SheetKind, name: &str, text: &str) -> Result<Vec<Stated>, String> {
        if !one_file_name(name) {
            return Err("a harness source's name must be one file name".to_owned());
        }
        match kind {
            SheetKind::AiPool => {
                let pool = Pool::from_toml(text, name)?;
                Ok(pool
                    .accounts
                    .into_iter()
                    .map(|row| Stated {
                        realm: realm_of(row.provider, &row.vendor),
                        email: row.email,
                        label: row.label,
                        machines: row.warm,
                    })
                    .collect())
            }
            SheetKind::FleetAccounts => {
                let cli = if name == "fleet-accounts.toml" {
                    Cli::Claude
                } else if let Some(id) = name
                    .strip_prefix("ai-accounts-")
                    .and_then(|rest| rest.strip_suffix(".toml"))
                {
                    [Cli::Codex, Cli::Grok, Cli::Kimi, Cli::Claude]
                        .into_iter()
                        .find(|cli| cli.id() == id)
                        .ok_or_else(|| "a fleet file names a CLI this build pools".to_owned())?
                } else {
                    return Err(
                        "a fleet file must be named fleet-accounts.toml or ai-accounts-<cli>.toml"
                            .to_owned(),
                    );
                };
                let roster = FleetRoster::from_toml_for(cli, text, name)?;
                Ok(roster
                    .accounts
                    .into_iter()
                    .map(|row| Stated {
                        realm: realm_of(cli, &row.vendor),
                        email: row.email,
                        label: Some(row.label),
                        machines: row.machines,
                    })
                    .collect())
            }
            SheetKind::Vault
            | SheetKind::Logins
            | SheetKind::Phones
            | SheetKind::Computers
            | SheetKind::Emails => Err("CSV sheets are not harness TOML files".to_owned()),
            SheetKind::FleetDb(_) => Err("a fleet_db dump is not a harness TOML file".to_owned()),
            SheetKind::MailboxToml => {
                Err("mailbox TOML is not a harness account source".to_owned())
            }
        }
    }
}

/// The realm word of a pooled CLI's row, which `keys::canonical_realm` reads.
fn realm_of(cli: Cli, vendor: &Vendor) -> String {
    match (cli, vendor.region) {
        (Cli::Kimi, Some(region)) => format!("kimi:{}", region.as_str()),
        (Cli::Kimi, None) => "kimi".to_owned(),
        (cli, _) => cli.id().to_owned(),
    }
}

/// `aterm db <args>`: `alab` with the harness's sheet reading, everything else as `aterm-db`
/// routes it.
pub(crate) fn db_entry(forwarded: Vec<OsString>) -> ExitCode {
    if forwarded.first().is_some_and(|word| word == "alab") {
        let mut iter = forwarded.into_iter();
        iter.next();
        let Some(words) = iter
            .map(|word| word.into_string().ok())
            .collect::<Option<Vec<String>>>()
        else {
            return aterm_uds::stdout::answer_exit("aterm db alab", "arguments must be UTF-8\n", 2);
        };
        return aterm_db::alab::command::main_entry(&words, Some(&Harness));
    }
    aterm_db::cli::main_entry(forwarded)
}

#[cfg(test)]
#[path = "alab_sheet_tests.rs"]
mod tests;
