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

impl Catalog {
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

pub(crate) fn report(
    catalog: &Result<Catalog, String>,
    cli: Cli,
    roster: Option<&Roster>,
) -> Value {
    report_rows(catalog, Some(cli), roster)
}

/// The shared application includes every provider, including those with no local adapter.
pub(crate) fn report_all(catalog: &Result<Catalog, String>) -> Value {
    report_rows(catalog, None, None)
}

fn report_rows(
    catalog: &Result<Catalog, String>,
    cli: Option<Cli>,
    roster: Option<&Roster>,
) -> Value {
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
                "state":row.state(), "local_label":roster.and_then(|roster| catalog.local_label(row, roster)),
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

pub(crate) fn text(catalog: &Result<Catalog, String>, cli: Cli, roster: Option<&Roster>) -> String {
    text_rows(catalog, Some(cli), roster)
}

pub(crate) fn text_all(catalog: &Result<Catalog, String>) -> String {
    text_rows(catalog, None, None)
}

/// A report's part of the catalog: the rows this Mac runs, each whose source is in question, and a
/// count of the others, which `accounts table` lists. On a sheet of 83 rows, 80 of them other
/// Macs', the listing made up most of the report and came after the answer.
pub(crate) fn text_here(
    catalog: &Result<Catalog, String>,
    cli: Cli,
    roster: Option<&Roster>,
) -> String {
    let Ok(rows) = catalog else {
        return text_rows(catalog, Some(cli), roster);
    };
    let of_cli = rows.rows.iter().filter(|row| row.belongs_to(cli));
    let (shown, others): (Vec<_>, Vec<_>) = of_cli.partition(|row| {
        row.stopped
            || row.conflicted
            || roster.is_some_and(|roster| rows.local_label(row, roster).is_some())
    });
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
        out.push_str(&row_line(rows, row, roster));
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

fn row_line(catalog: &Catalog, row: &Account, roster: Option<&Roster>) -> String {
    let local = roster.and_then(|roster| catalog.local_label(row, roster));
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
        local.unwrap_or("not configured")
    )
}

fn text_rows(
    catalog: &Result<Catalog, String>,
    cli: Option<Cli>,
    roster: Option<&Roster>,
) -> String {
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
                out.push_str(&row_line(catalog, row, roster));
            }
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A HOME WITH NO SECRETS DIRECTORY KEEPS NO CATALOG, SO NO REPORT CALLS ONE UNAVAILABLE:
    /// `read_with` gives `None` there, not the reader's error. CONTROL: once the directory is
    /// there, the reader's error stands (a catalog set up and not read is unavailable), and with
    /// no home known there is none either.
    #[cfg(unix)]
    #[test]
    fn a_home_with_no_secrets_directory_keeps_no_catalog() {
        let tmp = aterm_tempfile::tempdir().expect("a scratch home");
        let home = tmp.path();
        let reader: &Reader = &|_: &Path| Err("proposals: unreadable".to_owned());
        assert_eq!(read_with(Some(reader), Some(home)), None);
        let undeclared: &Reader = &|_: &Path| Ok(None);
        let dir = aterm_secrets::client::Paths::for_home(home).dir;
        std::fs::create_dir_all(&dir).expect("the secrets directory");
        assert_eq!(
            read_with(Some(reader), Some(home)),
            Some(Err("proposals: unreadable".to_owned())),
            "CONTROL: a catalog set up and not read is unavailable"
        );
        assert_eq!(
            read_with(Some(undeclared), Some(home)),
            None,
            "a catalog table nobody declared is no failure to read one"
        );
        assert_eq!(read_with(Some(reader), None), None, "CONTROL: no home");
    }

    pub(super) fn account() -> Account {
        Account {
            key: "account-one".into(),
            operation: "operation-one".into(),
            label: "Shared one".into(),
            provider: "anthropic".into(),
            method: Some("oauth".into()),
            email: Some("one@example.test".into()),
            realm: None,
            secret_reference: true,
            required_browser: false,
            conflicted: false,
            stopped: false,
            import: None,
            import_link: None,
        }
    }

    #[test]
    fn a_shared_description_never_grants_login_authority() {
        let catalog = Ok(Catalog {
            table: "shared-table".into(),
            history_complete: true,
            rows: vec![account()],
            ..Default::default()
        });
        let value = report(&catalog, Cli::Claude, Some(&Roster::default()));
        assert_eq!(value["accounts"][0]["key"].as_str(), Some("account-one"));
        assert_eq!(value["accounts"][0]["local_label"], Value::Null);
        assert_eq!(
            value["accounts"][0]["login_authority"].as_str(),
            Some("none")
        );
        assert!(value["accounts"][0]["secret_reference"].as_bool().unwrap());
        assert_eq!(Roster::default().rows.len(), 0);
    }

    #[test]
    fn an_unsupported_profile_is_explicit_without_granting_authority() {
        let mut row = account();
        row.required_browser = true;
        let catalog = Ok(Catalog {
            rows: vec![row],
            history_complete: true,
            ..Default::default()
        });
        let report = report_all(&catalog);
        assert_eq!(
            report["accounts"][0]["state"].as_str(),
            Some("unsupported-preparation")
        );
        assert_eq!(
            report["accounts"][0]["preparation_reason"].as_str(),
            Some(NAMED_BROWSER_UNSUPPORTED)
        );
        assert_eq!(
            report["accounts"][0]["login_authority"].as_str(),
            Some("none")
        );
        assert!(text_all(&catalog).contains(NAMED_BROWSER_UNSUPPORTED));
    }

    /// A REPORT LISTS THIS MAC'S ROWS AND COUNTS THE REST, and keeps a row whose source is in
    /// question, which no count would show. CONTROLS: `text`, the `accounts table` listing, still
    /// has every row, and another CLI's row is neither listed nor counted.
    #[test]
    fn a_report_lists_this_macs_rows_and_counts_the_rest() {
        let with = |label: &str, provider: &str| Account {
            label: label.into(),
            provider: provider.into(),
            email: Some(format!("{label}@example.test")),
            ..account()
        };
        let mut questioned = with("questioned", "anthropic");
        questioned.conflicted = true;
        let catalog = Ok(Catalog {
            table: "table".into(),
            history_complete: true,
            rows: vec![
                with("mine", "anthropic"),
                with("theirs", "anthropic"),
                with("further", "anthropic"),
                questioned,
                with("codexrow", "openai"),
            ],
            ..Default::default()
        });
        let roster = Roster {
            rows: vec![accounts::Row {
                label: "local".into(),
                email: Some("mine@example.test".into()),
                ..Default::default()
            }],
        };
        let here = text_here(&catalog, Cli::Claude, Some(&roster));
        assert!(
            here.contains("2 of its 4 rows are not on this Mac"),
            "{here}"
        );
        assert!(here.contains("<mine@example.test>"), "{here}");
        assert!(here.contains("local local"), "{here}");
        assert!(here.contains("<questioned@example.test>"), "{here}");
        assert!(
            !here.contains("theirs@") && !here.contains("further@"),
            "{here}"
        );
        assert!(!here.contains("codexrow@"), "{here}");
        let all = text(&catalog, Cli::Claude, Some(&roster));
        for label in ["mine", "theirs", "further", "questioned"] {
            assert!(all.contains(&format!("<{label}@example.test>")), "{all}");
        }
        assert!(
            text_here(&catalog, Cli::Claude, None).contains("3 of its 4 rows"),
            "with no roster only the questioned row stays"
        );
        let unavailable = Err("unreadable".to_owned());
        assert_eq!(
            text_here(&unavailable, Cli::Claude, None),
            text(&unavailable, Cli::Claude, None)
        );
    }

    #[test]
    fn only_a_unique_matching_local_identity_supplies_display_facts() {
        let mut roster = Roster {
            rows: vec![accounts::Row {
                label: "local".into(),
                email: account().email,
                ..Default::default()
            }],
        };
        assert_eq!(account().local_label(&roster), Some("local"));
        roster.rows.push(roster.rows[0].clone());
        assert_eq!(account().local_label(&roster), None);
        roster.rows.pop();
        let mut changed = account();
        changed.conflicted = true;
        assert_eq!(changed.local_label(&roster), None);
        changed.conflicted = false;
        changed.realm = Some("mainland-cn".into());
        assert_eq!(changed.local_label(&roster), None);
        roster.rows[0].held_by = Some("other-mac".into());
        assert_eq!(account().local_label(&roster), None);
        roster.rows[0].held_by = None;
        roster.rows[0].email = None;
        roster.rows[0].identity_email = account().email;
        assert_eq!(account().local_label(&roster), Some("local"));
    }

    #[test]
    fn incomplete_or_duplicate_shared_identity_withholds_local_status() {
        let roster = Roster {
            rows: vec![accounts::Row {
                label: "local".into(),
                email: account().email,
                ..Default::default()
            }],
        };
        let mut catalog = Catalog {
            table: "table".into(),
            history_complete: true,
            rows: vec![account()],
            ..Default::default()
        };
        assert_eq!(
            catalog.local_label(&catalog.rows[0], &roster),
            Some("local")
        );
        catalog.history_complete = false;
        assert_eq!(catalog.local_label(&catalog.rows[0], &roster), None);
        catalog.history_complete = true;
        let mut duplicate = account();
        duplicate.key = "another".into();
        duplicate.provider = "claude".into();
        catalog.rows.push(duplicate);
        assert_eq!(catalog.local_label(&catalog.rows[0], &roster), None);
    }

    #[test]
    fn imported_markers_remain_distinct_and_authentication_is_unspecified() {
        let mut row = account();
        row.provider = "google".into();
        row.method = None;
        row.import = Some(ImportOrigin {
            table: "imports".into(),
            source: "source-one".into(),
            record: 9,
            provider_column: Some(4),
            presence: Some(false),
            machine_hint: Some("m3.2".into()),
            unclassified_columns: vec![7],
        });
        let catalog = Ok(Catalog {
            rows: vec![row],
            history_complete: true,
            ..Default::default()
        });
        let report = report_all(&catalog);
        assert_eq!(report["accounts"].as_array().unwrap().len(), 1);
        let row = &report["accounts"][0];
        assert_eq!(row["provider"].as_str(), Some("google"));
        assert_eq!(row["state"].as_str(), Some("not-indicated"));
        assert_eq!(row["method"], Value::Null);
        assert_eq!(row["import"]["presence_marker"].as_bool(), Some(false));
        assert_eq!(row["import"]["record"].as_u64(), Some(9));
        assert_eq!(row["login_authority"].as_str(), Some("none"));
    }
}
