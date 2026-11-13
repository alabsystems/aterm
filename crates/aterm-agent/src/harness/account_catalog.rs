// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Read-only shared account descriptions. A catalog row never enters a local
//! executable roster or supplies permission to use a credential.

use super::accounts::{self, Cli, Roster};
use aterm_json::{Value, json};
use std::{path::Path, sync::OnceLock};

pub(crate) const NAMED_BROWSER_UNSUPPORTED: &str =
    "Automatic sign-in cannot use a required named browser profile";

/// Metadata observed from one retained Commons proposal table.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Catalog {
    pub table: String,
    pub import_table: Option<String>,
    pub source_errors: Vec<String>,
    pub history_complete: bool,
    pub rows: Vec<Account>,
}

/// Exact coordinates of an inert imported row and its provider marker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportOrigin {
    pub table: String,
    pub source: String,
    pub record: u64,
    pub provider_column: Option<u64>,
    pub presence: Option<bool>,
    pub machine_hint: Option<String>,
    pub unclassified_columns: Vec<u64>,
}

/// One proposal candidate. Credential contents and authorization are absent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    pub key: String,
    pub operation: String,
    pub label: String,
    pub provider: String,
    pub method: Option<String>,
    pub email: Option<String>,
    pub realm: Option<String>,
    pub secret_reference: bool,
    pub required_browser: bool,
    pub conflicted: bool,
    pub stopped: bool,
    pub import: Option<ImportOrigin>,
    pub import_link: Option<String>,
}

type Reader = dyn Fn(&Path) -> Result<Option<Catalog>, String> + Send + Sync;
static READER: OnceLock<Box<Reader>> = OnceLock::new();

/// The composing executable installs a reader for its enrolled database.
/// Library-only callers have no ambient database access.
pub fn install_reader(
    reader: impl Fn(&Path) -> Result<Option<Catalog>, String> + Send + Sync + 'static,
) {
    let _ = READER.set(Box::new(reader));
}

/// The Commons catalog under `home` as the installed reader reads it; `None` with no reader, no
/// home, or no catalog set up or declared there ([`read_with`]).
pub(crate) fn read(home: Option<&Path>) -> Option<Result<Catalog, String>> {
    read_with(READER.get().map(|reader| &**reader), home)
}

/// [`read`] through `reader`: `None` where there is none, no home is known, the home holds no
/// secrets directory (`~/.config/aterm/secrets`), or the reader finds no catalog table declared
/// there, the machine having set up no catalog, which is no failure to read one; a directory or
/// table that is there and cannot be read stays the reader's error, which a report calls
/// unavailable. Off Unix aterm keeps no secrets client, so no catalog is set up there.
fn read_with(reader: Option<&Reader>, home: Option<&Path>) -> Option<Result<Catalog, String>> {
    let (reader, home) = reader.zip(home)?;
    #[cfg(unix)]
    let absent = std::fs::symlink_metadata(aterm_secrets::client::Paths::for_home(home).dir)
        .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound);
    #[cfg(not(unix))]
    let absent = true;
    (!absent).then(|| reader(home))?.transpose()
}

/// What this Mac holds, as a catalog row's `local` reads it: its roster, where one was read, and
/// the address its default home's login file names now (`own`), which counts as this Mac's own
/// login where no row lists it (`docs/DESIGN-account-discovery-2026-11-09.md`).
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Here<'a> {
    pub roster: Option<&'a Roster>,
    pub own: Option<&'a str>,
}

impl Catalog {
    /// What `account` is on this Mac, as a person reads it: the roster row it is ([`Self::local_label`]),
    /// else `this Mac's own login` where the default home's login names it, by the same unique
    /// identity rule. `None` where this Mac holds it nowhere it can see.
    pub(crate) fn local_words(&self, account: &Account, here: Here<'_>) -> Option<String> {
        if let Some(label) = here
            .roster
            .and_then(|roster| self.local_label(account, roster))
        {
            return Some(label.to_owned());
        }
        let own = here.own?;
        let listed = here
            .roster
            .is_some_and(|roster| roster.rows.iter().any(|row| row.matches_email(own)));
        let unique = self.history_complete
            && !account.conflicted
            && !account.stopped
            && account.indicates_account()
            && account.cli() == Some(Cli::Claude)
            && account.realm.is_none()
            && account
                .email
                .as_deref()
                .is_some_and(|email| accounts::email_matches(email, own))
            && self
                .rows
                .iter()
                .filter(|other| {
                    other.indicates_account()
                        && other.cli() == account.cli()
                        && other.realm == account.realm
                        && other
                            .email
                            .as_deref()
                            .is_some_and(|other| accounts::email_matches(other, own))
                })
                .count()
                == 1;
        (unique && !listed).then(|| "this Mac's own login".to_owned())
    }

    pub(crate) fn local_label<'a>(&self, account: &Account, roster: &'a Roster) -> Option<&'a str> {
        if !self.history_complete || !account.indicates_account() || account.cli().is_none() {
            return None;
        }
        let email = account.email.as_deref()?;
        let matches = self
            .rows
            .iter()
            .filter(|other| {
                other.indicates_account()
                    && other.cli() == account.cli()
                    && other.realm == account.realm
                    && other
                        .email
                        .as_deref()
                        .is_some_and(|other| accounts::email_matches(other, email))
            })
            .count();
        (matches == 1)
            .then(|| account.local_label(roster))
            .flatten()
    }
}

impl Account {
    fn indicates_account(&self) -> bool {
        self.import
            .as_ref()
            .is_none_or(|source| source.presence == Some(true))
    }
    pub fn belongs_to(&self, cli: Cli) -> bool {
        self.cli() == Some(cli)
    }

    fn cli(&self) -> Option<Cli> {
        match self.provider.as_str() {
            "anthropic" | "claude" => Some(Cli::Claude),
            "openai" | "codex" => Some(Cli::Codex),
            "xai" | "grok" => Some(Cli::Grok),
            "kimi" | "moonshot" => Some(Cli::Kimi),
            _ => None,
        }
    }

    /// A display association only. Ambiguous identities and conflicted proposals
    /// never inherit local login or usage facts.
    pub(crate) fn local_label<'a>(&self, roster: &'a Roster) -> Option<&'a str> {
        if self.conflicted || self.stopped || !self.indicates_account() {
            return None;
        }
        let email = self.email.as_deref()?;
        let mut rows = roster.rows.iter().filter(|row| {
            row.held_by.is_none()
                && row.matches_email(email)
                && row.vendor.region.map(|region| region.as_str()) == self.realm.as_deref()
        });
        let row = rows.next()?;
        rows.next().is_none().then_some(row.label.as_str())
    }

    pub(crate) fn state(&self) -> &'static str {
        if self.stopped {
            "stopped"
        } else if self.conflicted {
            "conflicted"
        } else if self.required_browser {
            "unsupported-preparation"
        } else if let Some(source) = &self.import {
            match source.presence {
                Some(true) => "auth-details-pending",
                Some(false) => "not-indicated",
                None => "provider-details-pending",
            }
        } else {
            "proposed"
        }
    }

    pub(crate) fn state_words(&self) -> &'static str {
        match self.state() {
            "stopped" => "Stopped",
            "conflicted" => "Conflicting source details",
            "unsupported-preparation" => NAMED_BROWSER_UNSUPPORTED,
            "auth-details-pending" => "Awaiting sign-in details",
            "not-indicated" => "Provider not indicated in source",
            "provider-details-pending" => "Awaiting provider details",
            _ => "Account details available",
        }
    }
}

pub(crate) fn report(catalog: &Result<Catalog, String>, cli: Cli, here: Here<'_>) -> Value {
    report_rows(catalog, Some(cli), here)
}

/// The shared application includes every provider, including those with no local adapter.
pub(crate) fn report_all(catalog: &Result<Catalog, String>) -> Value {
    report_rows(catalog, None, Here::default())
}

fn report_rows(catalog: &Result<Catalog, String>, cli: Option<Cli>, here: Here<'_>) -> Value {
    match catalog {
        Err(why) => json!({"state":"unavailable", "reason":why}),
        Ok(catalog) => json!({
            "state":"retained", "table":catalog.table, "import_table":catalog.import_table,
            "source_errors":catalog.source_errors,
            "history_complete":catalog.history_complete,
            "accounts":catalog.rows.iter().filter(|row| cli.is_none_or(|cli| row.belongs_to(cli))).map(|row| json!({
                "key":row.key, "operation":row.operation, "label":row.label,
                "provider":row.provider, "method":row.method, "email":row.email,
                "realm":row.realm, "secret_reference":row.secret_reference,
                "native_capability":super::native_keys::capability(&row.provider,row.method.as_deref(),row.secret_reference),
                "state":row.state(), "local_label":catalog.local_words(row, here),
                "preparation_reason":(row.state() == "unsupported-preparation").then_some(NAMED_BROWSER_UNSUPPORTED),
                "login_authority":"none", "auth_details_pending":row.import.is_some(),
                "origin":if row.import.is_some() { "import-candidate" } else { "proposal" },
                "import_link":row.import_link,
                "import":row.import.as_ref().map(|source| json!({
                    "table":source.table, "source_sha256":source.source, "record":source.record,
                    "provider_column":source.provider_column, "presence_marker":source.presence,
                    "machine_slot_hint":source.machine_hint, "unclassified_columns":source.unclassified_columns
                }))
            })).collect::<Vec<_>>()
        }),
    }
}

pub(crate) fn text(catalog: &Result<Catalog, String>, cli: Cli, here: Here<'_>) -> String {
    text_rows(catalog, Some(cli), here)
}

pub(crate) fn text_all(catalog: &Result<Catalog, String>) -> String {
    text_rows(catalog, None, Here::default())
}

/// A report's part of the catalog: the rows this Mac runs, each whose source is in question, and a
/// count of the others, which `accounts table` lists. On a sheet of 83 rows, 80 of them other
/// Macs', the listing made up most of the report and came after the answer.
pub(crate) fn text_here(catalog: &Result<Catalog, String>, cli: Cli, here: Here<'_>) -> String {
    let Ok(rows) = catalog else {
        return text_rows(catalog, Some(cli), here);
    };
    let of_cli = rows.rows.iter().filter(|row| row.belongs_to(cli));
    let (shown, others): (Vec<_>, Vec<_>) = of_cli
        .partition(|row| row.stopped || row.conflicted || rows.local_words(row, here).is_some());
    let mut out = header(rows);
    if !others.is_empty() {
        out.push_str(&format!(
            ": {} of its {} rows are not on this Mac (`aterm harness accounts table` lists them)",
            others.len(),
            others.len() + shown.len()
        ));
    }
    out.push('\n');
    for row in shown {
        out.push_str(&row_line(rows, row, here));
    }
    out
}

fn header(catalog: &Catalog) -> String {
    format!(
        "Commons account catalog: {}{}",
        if catalog.table.is_empty() {
            catalog.import_table.as_deref().unwrap_or("unknown")
        } else {
            &catalog.table
        },
        if catalog.history_complete {
            ""
        } else {
            " (incomplete history)"
        }
    )
}

fn row_line(catalog: &Catalog, row: &Account, here: Here<'_>) -> String {
    let local = catalog.local_words(row, here);
    format!(
        "  {}{} [{}]: {} {}; local {}\n",
        row.label,
        row.email
            .as_deref()
            .map(|email| format!(" <{email}>"))
            .unwrap_or_default(),
        row.key,
        row.provider,
        row.state_words(),
        local.as_deref().unwrap_or("not listed here")
    )
}

fn text_rows(catalog: &Result<Catalog, String>, cli: Option<Cli>, here: Here<'_>) -> String {
    match catalog {
        Err(why) => format!("Commons account catalog unavailable: {why}\n"),
        Ok(catalog) => {
            let mut out = header(catalog);
            out.push('\n');
            for row in catalog
                .rows
                .iter()
                .filter(|row| cli.is_none_or(|cli| row.belongs_to(cli)))
            {
                out.push_str(&row_line(catalog, row, here));
            }
            out
        }
    }
}

#[cfg(test)]
#[path = "account_catalog_tests.rs"]
mod tests;
