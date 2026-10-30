// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Fleet and machine account synchronization.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, BufRead, Read, Seek, SeekFrom, Write};
use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use aterm_json::{Map, Value};

use super::accounts::{self, Class, Cli, Roster, Row};
use super::cli::{ACCOUNTS_OWN_LABEL, Env};
use super::upgrade;
use crate::ai::connector::{Flow, Region, RowKeys};

#[path = "accounts_auto_shared.rs"]
pub mod shared_renewal;

/// Table name whose [`SOURCE_KEY`] names this machine's fleet accounts source.
pub const SOURCE_TABLE: &str = "fleet";

/// Key under [`SOURCE_TABLE`] naming the fleet accounts path or URL.
pub const SOURCE_KEY: &str = "accounts_source";

/// Prefix naming an `rclone` remote, which [`fetch_plan`] refuses by name.
pub const RCLONE_PREFIX: &str = "rclone:";

/// What a sync says when it was handed no source and none is configured.
pub const NO_SOURCE: &str = "no accounts source is configured, so nothing was fetched or read: \
     name one (`aterm harness accounts sync <path-or-url>`) or set `[fleet] accounts_source` \
     in aterm.toml";

/// Development seam enabling live window upkeep under a scratch state root in tests.
pub const UPKEEP_SEAM: &str = "ATERM_DEBUG_ACCOUNTS_UPKEEP";

/// Shared fleet config file written beside `accounts.toml`.
pub const FLEET_ROSTER_FILE: &str = "fleet-accounts.toml";

/// Top-level key recording why a sync wrote this machine's fleet roster ([`OptIn`]).
pub const OPT_IN_KEY: &str = "opt_in";

/// The first line of a fleet file aterm writes of a list it was handed
/// ([`FleetRoster::to_toml_for`]), as every aterm since fleet files began has written it.
const FLEET_HEAD_NAMED: &str =
    "# Managed by `aterm harness accounts sync` — shared LAN fleet account roster.";

/// The first line of a fleet file aterm writes of a list it found on this Mac
/// ([`OptIn::Discovered`]).
const FLEET_HEAD_FOUND: &str =
    "# Found on this Mac by aterm: the accounts its own logins, profiles and transcripts";

/// Why a sync wrote this machine's fleet roster ([`OPT_IN_KEY`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptIn {
    /// Sync was handed an explicit source or read a fleet file that opted in by name.
    Named,
    /// The sync read `[fleet] accounts_source`.
    Key,
    /// No source was named or configured: the accounts are the ones this machine's own logins,
    /// profiles and transcripts show (local discovery), and discovery adds the ones it finds later
    /// ([`fleet_takes_discovered`]). A fleet file that says so opts in as [`Self::Named`] does.
    Discovered,
}

impl OptIn {
    /// The word written for it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Named => "named",
            Self::Key => "key",
            Self::Discovered => "discovered",
        }
    }

    fn parse(word: &str) -> Option<Self> {
        [Self::Named, Self::Key, Self::Discovered]
            .into_iter()
            .find(|opt_in| opt_in.as_str() == word)
    }

    /// Whether a fleet file beside the roster that records it opts this machine in
    /// ([`configured_source`]): [`Self::Named`] or [`Self::Discovered`].
    #[must_use]
    pub const fn opts_in_by_file(self) -> bool {
        matches!(self, Self::Named | Self::Discovered)
    }
}

/// Default bind address for `aterm harness accounts serve`.
pub const DEFAULT_SERVE_BIND: &str = "127.0.0.1:8741";

const TRANSCRIPT_TAIL_BYTES: u64 = 256 * 1024;

/// One active account in a fleet roster: Claude Code's ([`FleetRoster::from_toml`],
/// [`parse_accounting_csv`]), or another pooled CLI's ([`FleetRoster::from_toml_for`],
/// [`parse_accounting_csv_for`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetAccount {
    /// Canonical roster label (`m8`, `m8-2`, `m106-m38`, …).
    pub label: String,
    /// Normalized machine IDs (`m8`, `m106`, `m38`, …) that own or share this row.
    pub machines: Vec<String>,
    /// `Dedicated` when assigned to a single machine, `Shared` across multiple.
    pub class: Class,
    /// Account email address (`name@domain`).
    pub email: String,
    /// Optional browser profile hint.
    pub browser: Option<String>,
    /// Its vendor keys in another CLI's pool (`region`, `flow`); none in Claude Code's.
    pub vendor: accounts::Vendor,
}

/// Complete fleet account table parsed from CSV or the fleet roster file.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FleetRoster {
    /// Human-readable source description (path or URL; never an email).
    pub source: String,
    /// Active Claude accounts in deterministic order.
    pub accounts: Vec<FleetAccount>,
}

impl FleetRoster {
    /// Filter accounts belonging to `machine`, returning rows in slot order.
    #[must_use]
    pub fn accounts_for_machine(&self, machine: &str) -> Vec<&FleetAccount> {
        let norm = normalize_machine_id(machine).unwrap_or_else(|| machine.to_ascii_lowercase());
        let mut rows: Vec<&FleetAccount> = self
            .accounts
            .iter()
            .filter(|a| a.machines.iter().any(|m| m == &norm))
            .collect();
        rows.sort_by_key(|a| (a.class, a.label.clone()));
        rows
    }

    /// All distinct machine IDs present in this fleet roster.
    #[must_use]
    pub fn machines(&self) -> Vec<String> {
        let mut set = BTreeSet::new();
        for acct in &self.accounts {
            for m in &acct.machines {
                set.insert(m.clone());
            }
        }
        set.into_iter().collect()
    }

    /// Serialize the fleet roster as `fleet-accounts.toml` for LAN sharing.
    #[must_use]
    pub fn to_toml(&self) -> String {
        self.to_toml_for(None)
    }

    /// [`Self::to_toml`], with this machine's [`OPT_IN_KEY`] where `opt_in` names one. A fleet
    /// discovery made ([`OptIn::Discovered`]) says it was found on this Mac, not handed to a sync.
    #[must_use]
    pub fn to_toml_for(&self, opt_in: Option<OptIn>) -> String {
        let mut out = if opt_in == Some(OptIn::Discovered) {
            format!(
                "{FLEET_HEAD_FOUND}\n\
                 # show. No person named a source for them, and aterm adds each account it finds\n\
                 # later. `aterm harness accounts sync <path-or-url>` replaces it with a fleet you name.\n\n"
            )
        } else {
            format!(
                "{FLEET_HEAD_NAMED}\n\
                 # Run `aterm harness accounts sync <path-or-url>` on any machine to materialize\n\
                 # its local `accounts.toml`.\n\n"
            )
        };
        if let Some(opt_in) = opt_in {
            out.push_str(&format!(
                "# Why this machine keeps its fleet accounts: `named`, a sync handed its source\n\
                 # (delete this file to stop); `key`, `[fleet] accounts_source` in aterm.toml;\n\
                 # `discovered`, the accounts this machine's own logins and transcripts show,\n\
                 # which it adds to as it finds more (delete this file to stop).\n\
                 {OPT_IN_KEY} = {}\n\n",
                toml_quote(opt_in.as_str())
            ));
        }
        for acct in &self.accounts {
            out.push_str("[[account]]\n");
            out.push_str(&format!("label = {}\n", toml_quote(&acct.label)));
            let machines: Vec<String> = acct.machines.iter().map(|m| toml_quote(m)).collect();
            out.push_str(&format!("machines = [{}]\n", machines.join(", ")));
            out.push_str(&format!("class = {}\n", toml_quote(acct.class.as_str())));
            out.push_str(&format!("email = {}\n", toml_quote(&acct.email)));
            if let Some(browser) = &acct.browser {
                out.push_str(&format!("browser = {}\n", toml_quote(browser)));
            }
            if let Some(region) = acct.vendor.region {
                out.push_str(&format!("region = {}\n", toml_quote(region.as_str())));
            }
            if let Some(flow) = acct.vendor.flow {
                out.push_str(&format!("flow = {}\n", toml_quote(flow.as_str())));
            }
            out.push('\n');
        }
        out
    }

    /// Parse `fleet-accounts.toml` text back into a [`FleetRoster`].
    /// # Errors
    /// Returns a human-readable error if the TOML is invalid, `account` is not an array of tables,
    /// or any row is missing required fields, carries a refused label or email, has a
    /// malformed machine assignment, or names an invalid explicit class.
    pub fn from_toml(text: &str, source: &str) -> Result<Self, String> {
        Self::from_toml_for(Cli::Claude, text, source)
    }

    /// Parse `cli`'s fleet roster ([`cli_fleet_file`]): [`Self::from_toml`]'s rows, plus the
    /// vendor keys `cli`'s rows take ([`RowKeys`]): a Kimi Code row names its `region`, a Codex
    /// or Grok Build row may name its `flow`. Claude Code's is [`Self::from_toml`], whose rows
    /// carry no vendor key and whose other keys are ignored as they always were.
    /// # Errors
    /// As [`Self::from_toml`]; and, in another CLI's, a vendor key the CLI does not take, one
    /// that is not a word it knows, or a Kimi Code row with no `region`.
    pub fn from_toml_for(cli: Cli, text: &str, source: &str) -> Result<Self, String> {
        let table: aterm_toml::Table = aterm_toml::from_str(text)
            .map_err(|e| format!("fleet roster at {source} is not valid TOML: {e}"))?;
        let mut accounts = Vec::new();
        let Some(rows) = table.get("account") else {
            return Ok(Self {
                source: source.to_owned(),
                accounts,
            });
        };
        let rows = rows
            .as_array()
            .ok_or_else(|| format!("fleet roster at {source}: `account` must be `[[account]]`"))?;
        for (idx, item) in rows.iter().enumerate() {
            let row = item
                .as_table()
                .ok_or_else(|| format!("fleet roster row {}: not a table", idx + 1))?;
            let label = row
                .get("label")
                .and_then(aterm_toml::Value::as_str)
                .ok_or_else(|| format!("fleet roster row {}: missing `label`", idx + 1))?
                .to_owned();
            if let Some(why) = accounts::label_refusal(&label) {
                return Err(format!("fleet roster row {} ({label}): {why}", idx + 1));
            }
            let email = row
                .get("email")
                .and_then(aterm_toml::Value::as_str)
                .ok_or_else(|| format!("fleet roster row {} ({label}): missing `email`", idx + 1))?
                .to_owned();
            if let Some(why) = accounts::email_refusal(&email) {
                return Err(format!(
                    "fleet roster row {} ({label}): email {why}",
                    idx + 1
                ));
            }
            let (machines, class) = fleet_assignment(row, idx + 1, &label)?;
            let browser = row
                .get("browser")
                .and_then(aterm_toml::Value::as_str)
                .map(str::to_owned);
            let vendor = match cli {
                Cli::Claude => accounts::Vendor::default(),
                _ => fleet_vendor(cli, row, idx + 1, &label)?,
            };
            accounts.push(FleetAccount {
                label,
                machines,
                class,
                email,
                browser,
                vendor,
            });
        }
        Ok(Self {
            source: source.to_owned(),
            accounts,
        })
    }
}

// An explicit assignment is admitted whole. Normalize before stable deduplication:
// repeated spellings of one host cannot change its inferred contention class.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "FleetMachineAssignment",
        action = "Missing",
        project = "aterm_agent::harness::accounts_sync::fleet_assignment_tests::assignment_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "FleetMachineAssignment",
        action = "InvalidMachine",
        project = "aterm_agent::harness::accounts_sync::fleet_assignment_tests::assignment_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "FleetMachineAssignment",
        action = "WrongType",
        project = "aterm_agent::harness::accounts_sync::fleet_assignment_tests::assignment_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "FleetMachineAssignment",
        action = "InvalidClass",
        project = "aterm_agent::harness::accounts_sync::fleet_assignment_tests::assignment_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "FleetMachineAssignment",
        action = "Scalar",
        project = "aterm_agent::harness::accounts_sync::fleet_assignment_tests::assignment_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "FleetMachineAssignment",
        action = "Distinct",
        project = "aterm_agent::harness::accounts_sync::fleet_assignment_tests::assignment_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "FleetMachineAssignment",
        action = "Duplicate",
        project = "aterm_agent::harness::accounts_sync::fleet_assignment_tests::assignment_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "FleetMachineAssignment",
        action = "Override",
        project = "aterm_agent::harness::accounts_sync::fleet_assignment_tests::assignment_transition"
    )
)]
fn fleet_assignment(
    row: &aterm_toml::Table,
    n: usize,
    label: &str,
) -> Result<(Vec<String>, Class), String> {
    let values = match row.get("machines") {
        Some(aterm_toml::Value::Array(values)) if !values.is_empty() => values.as_slice(),
        Some(value @ aterm_toml::Value::String(_)) => std::slice::from_ref(value),
        _ => {
            return Err(format!(
                "fleet roster row {n} ({label}): `machines` must be a machine name or a nonempty array of machine names"
            ));
        }
    };
    let mut seen = BTreeSet::new();
    let mut machines = Vec::new();
    for (index, value) in values.iter().enumerate() {
        let machine = value
            .as_str()
            .and_then(normalize_machine_id)
            .ok_or_else(|| {
                format!(
                    "fleet roster row {n} ({label}): `machines` entry {} must be a recognized machine name",
                    index + 1
                )
            })?;
        if seen.insert(machine.clone()) {
            machines.push(machine);
        }
    }
    let class = match row.get("class") {
        None if machines.len() > 1 => Class::Shared,
        None => Class::Dedicated,
        Some(value) => value.as_str().and_then(Class::parse).ok_or_else(|| {
            format!("fleet roster row {n} ({label}): `class` must be `dedicated` or `shared`")
        })?,
    };
    Ok((machines, class))
}

/// The vendor keys of `cli`'s fleet roster row `n` ([`FleetRoster::from_toml_for`]).
fn fleet_vendor(
    cli: Cli,
    row: &aterm_toml::Table,
    n: usize,
    label: &str,
) -> Result<accounts::Vendor, String> {
    let product = cli.connector().product;
    let word = |key: &str| match row.get(key) {
        None => Ok(None),
        Some(aterm_toml::Value::String(word)) => Ok(Some(word.as_str())),
        Some(other) => Err(format!(
            "fleet roster row {n} ({label}): `{key}` is a TOML {}, not a string",
            other.type_str()
        )),
    };
    let keys = cli.accounts().keys;
    let region = match (word("region")?, keys) {
        (None, RowKeys::Region) => {
            return Err(format!(
                "fleet roster row {n} ({label}): a {product} row names its `region` (`global` \
                 for kimi.ai, `mainland-cn` for kimi.com): the two are separate account systems"
            ));
        }
        (Some(word), RowKeys::Region) => Some(Region::parse(word).ok_or_else(|| {
            format!(
                "fleet roster row {n} ({label}): `region` is `global` or `mainland-cn`, not {:?}",
                super::one_line(word, 40)
            )
        })?),
        (Some(_), _) => {
            return Err(format!(
                "fleet roster row {n} ({label}): {product}'s rows take no `region`"
            ));
        }
        (None, _) => None,
    };
    let flow = match (word("flow")?, keys) {
        (Some(word), RowKeys::Flow(_)) => Some(Flow::parse(word).ok_or_else(|| {
            format!(
                "fleet roster row {n} ({label}): `flow` is `browser` or `device`, not {:?}",
                super::one_line(word, 40)
            )
        })?),
        (Some(_), _) => {
            return Err(format!(
                "fleet roster row {n} ({label}): {product}'s rows take no `flow`"
            ));
        }
        (None, _) => None,
    };
    Ok(accounts::Vendor { region, flow })
}

fn normalize_machine_token(trimmed: &str) -> Option<String> {
    let lower = trimmed.to_ascii_lowercase();
    let rest = lower.strip_prefix('m')?;
    let digits: String = rest
        .bytes()
        .take_while(u8::is_ascii_digit)
        .map(char::from)
        .collect();
    if digits.is_empty() {
        return None;
    }
    let after = &rest[digits.len()..];
    if after.is_empty() || after.starts_with(['-', '.', '_', ' ']) {
        Some(format!("m{digits}"))
    } else {
        None
    }
}

/// Normalize a machine token to its canonical lowercase `"m<digits>"` identifier.
#[must_use]
pub fn normalize_machine_id(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.starts_with('*') || trimmed.contains('?') {
        return None;
    }
    if let Some(m) = normalize_machine_token(trimmed) {
        return Some(m);
    }
    let stem = trimmed.strip_suffix(".local").unwrap_or(trimmed);
    let tokens: Vec<&str> = stem
        .split(['-', '.', '_', ' ', '(', ')'])
        .filter(|s| !s.is_empty())
        .collect();
    for (idx, &tok) in tokens.iter().enumerate() {
        let Some(m) = normalize_machine_token(tok) else {
            continue;
        };
        let Ok(num) = m[1..].parse::<u32>() else {
            continue;
        };
        if idx > 0
            && (1..=5).contains(&num)
            && tokens.get(idx + 1).is_some_and(|next| {
                let nl = next.to_ascii_lowercase();
                matches!(nl.as_str(), "max" | "pro" | "ultra" | "air")
                    || (nl.ends_with("gb")
                        && nl[..nl.len() - 2].bytes().all(|b| b.is_ascii_digit()))
            })
        {
            continue;
        }
        return Some(m);
    }
    None
}

fn parse_computer_cell(cell: &str) -> Option<(Vec<String>, Option<u32>)> {
    let trimmed = cell.trim();
    if trimmed.is_empty() || trimmed.starts_with('*') || trimmed.contains('?') {
        return None;
    }
    let lower = trimmed.to_ascii_lowercase();
    let rest = lower.strip_prefix('m')?;
    let parts: Vec<&str> = rest.split('.').collect();
    let mut machines = Vec::new();
    let mut slot = None;
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let n: u32 = part.parse().ok()?;
        if i == 0 {
            machines.push(format!("m{n}"));
        } else if i + 1 == parts.len() && n <= 9 {
            slot = Some(n);
        } else {
            machines.push(format!("m{n}"));
        }
    }
    if machines.is_empty() {
        None
    } else {
        Some((machines, slot))
    }
}

fn machines_from_email_hint(email: &str) -> Vec<String> {
    email_machines_and_slot(email).0
}

pub(super) fn csv_records(mut text: &str) -> impl Iterator<Item = Result<&str, &'static str>> {
    text = text.strip_prefix('\u{feff}').unwrap_or(text);
    std::iter::from_fn(move || {
        let input = text;
        if input.is_empty() {
            return None;
        }
        text = "";
        let bytes = input.as_bytes();
        let (mut first, mut quoted, mut closed) = (true, false, false);
        let mut at = 0;
        while at < bytes.len() {
            let byte = bytes[at];
            if quoted {
                if byte == b'"' {
                    if bytes.get(at + 1) == Some(&b'"') {
                        at += 2;
                        continue;
                    }
                    quoted = false;
                    closed = true;
                }
            } else {
                match byte {
                    b',' => {
                        first = true;
                        closed = false;
                    }
                    b'"' if first => {
                        first = false;
                        quoted = true;
                    }
                    b'"' => return Some(Err("quote in an unquoted field")),
                    b'\n' | b'\r' => {
                        let mut end = at + 1;
                        if byte == b'\r' {
                            if bytes.get(end) != Some(&b'\n') {
                                return Some(Err("bare carriage return outside a quoted field"));
                            }
                            end += 1;
                        }
                        text = &input[end..];
                        return Some(Ok(&input[..at]));
                    }
                    _ if closed => return Some(Err("text after a closing quote")),
                    _ => first = false,
                }
            }
            at += 1;
        }
        Some(if quoted {
            Err("unterminated quoted field")
        } else {
            Ok(input)
        })
    })
}

pub(super) fn parse_csv_line(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut cur = String::new();
    let mut chars = line.chars().peekable();
    let mut in_quotes = false;
    while let Some(c) = chars.next() {
        if in_quotes {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    cur.push('"');
                    let _ = chars.next();
                } else {
                    in_quotes = false;
                }
            } else {
                cur.push(c);
            }
        } else if c == '"' {
            in_quotes = true;
        } else if c == ',' {
            fields.push(std::mem::take(&mut cur));
        } else if c != '\r' {
            cur.push(c);
        }
    }
    fields.push(cur);
    fields
}

fn notes_disqualify(notes: &str) -> bool {
    let lower = notes.to_ascii_lowercase();
    lower.contains("banned")
        || lower.contains("roll off")
        || lower.contains("payment disabled")
        || lower.contains("deactivated")
        || lower.contains("suspended")
}

/// Parse the accounting CSV into a [`FleetRoster`].
/// # Errors
/// Returns an error if the CSV has no header row, missing or ambiguous identity/membership
/// columns, an ambiguous required machine-layout marker, or malformed quoting.
/// Repeated selected-provider rows for one address must describe the same account.
/// Invalid generated account labels and exhaustion of their disambiguating suffixes also fail.
/// LF and CRLF separate records only outside quotes.
pub fn parse_accounting_csv(csv_text: &str, source: &str) -> Result<FleetRoster, String> {
    parse_sheet(csv_text, source, Cli::Claude).map(|(fleet, _)| fleet)
}

/// One pooled CLI's fleet as the sheet or its fleet file lists it ([`parse_accounting_csv_for`],
/// [`load_cli_fleet`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliFleet {
    /// The CLI.
    pub cli: Cli,
    /// Its rows.
    pub roster: FleetRoster,
    /// The rows a sheet lists for Kimi Code with no region (a bare `1` in its cell): held out
    /// of the pool until the cell names `global` or `mainland-cn`, since a sign-in on the wrong
    /// one can make a new account.
    pub held: Vec<FleetAccount>,
}

/// The sheet's header words naming `cli`'s membership column, matched trimmed and in any case:
/// `claude`; `Open AI` (or `openai`, `codex`) for Codex; `xai` (or `x`, the heading of the
/// vault's own sheet, or `grok`) for Grok Build; `kimi` for Kimi Code. A header word is matched
/// whole; a note's vendor words have no lone `x`, so `x` scopes no note.
#[must_use]
pub const fn csv_columns(cli: Cli) -> &'static [&'static str] {
    match cli {
        Cli::Claude => &["claude"],
        Cli::Codex => &["open ai", "openai", "codex"],
        Cli::Grok => &["xai", "x", "grok"],
        Cli::Kimi => &["kimi"],
    }
}

/// Parse `cli`'s pool from the accounting CSV ([`parse_accounting_csv`]): the rows whose cell in
/// `cli`'s column ([`csv_columns`]) says it, labelled `<cli>-<machine label>` (`kimi-m16`,
/// `kimi-m16-2`). A cell says `1`, or one of the CLI's vendor words: `global` or `mainland-cn`
/// (also `kimi.ai`, `kimi.com`, `cn`) for Kimi Code, whose bare `1` is held ([`CliFleet::held`]);
/// `browser` or `device` for Codex and Grok Build. A disqualifying note that names a vendor
/// (`kimi banned`) drops that vendor's membership alone; one that names none drops every one.
/// Claude Code's is [`parse_accounting_csv`], unchanged: any disqualifying note drops its row,
/// as every older build reads the same sheet.
/// # Errors
/// As [`parse_accounting_csv`], with `cli`'s column in place of `claude`.
pub fn parse_accounting_csv_for(
    cli: Cli,
    csv_text: &str,
    source: &str,
) -> Result<CliFleet, String> {
    parse_sheet(csv_text, source, cli).map(|(roster, held)| CliFleet { cli, roster, held })
}

/// Whether the sheet's header names `cli`'s column ([`csv_columns`]). Duplicate names still
/// count as present, so the selected source reaches the parser's explicit refusal.
#[must_use]
pub fn csv_names_column(cli: Cli, csv_text: &str) -> bool {
    csv_records(csv_text)
        .next()
        .and_then(Result::ok)
        .is_some_and(|header| !matches!(member_column(cli, &parse_csv_line(header)), Ok(None)))
}

fn unique_column(header: &[String], names: &[&str]) -> Result<Option<usize>, ()> {
    let mut matches = header.iter().enumerate().filter_map(|(index, h)| {
        names
            .iter()
            .any(|word| h.trim().eq_ignore_ascii_case(word))
            .then_some(index)
    });
    let first = matches.next();
    if matches.next().is_some() {
        Err(())
    } else {
        Ok(first)
    }
}

fn member_column(cli: Cli, header: &[String]) -> Result<Option<usize>, ()> {
    unique_column(header, csv_columns(cli))
}

/// What a row's cell in a CLI's column says of its membership ([`parse_sheet`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Member {
    /// Not a member.
    No,
    /// A member, with the vendor keys the cell names.
    Yes(accounts::Vendor),
    /// A member the pool cannot take as listed ([`CliFleet::held`]).
    Held,
}

#[derive(Debug, PartialEq, Eq)]
struct SheetAccount {
    machines: Vec<String>,
    slot: Option<u32>,
    member: Member,
}

// Rows claiming or explicitly disqualifying this provider reach here. Excluded
// rows retain negative evidence so an active duplicate cannot replace them.
fn observe_sheet_account(
    seen: &mut BTreeMap<String, Option<SheetAccount>>,
    email: &str,
    account: Option<SheetAccount>,
    source: &str,
) -> Result<bool, String> {
    match seen.entry(email.to_ascii_lowercase()) {
        std::collections::btree_map::Entry::Vacant(entry) => {
            entry.insert(account);
            Ok(false)
        }
        std::collections::btree_map::Entry::Occupied(entry) if entry.get() == &account => Ok(true),
        std::collections::btree_map::Entry::Occupied(_) => Err(format!(
            "CSV at {source} has conflicting duplicate account rows; \
             give each account one consistent assignment or use explicit fleet account records"
        )),
    }
}

fn member_of(cli: Cli, cell: &str) -> Member {
    let word = cell.to_ascii_lowercase();
    let vendor = |region, flow| Member::Yes(accounts::Vendor { region, flow });
    match (cli.accounts().keys, word.as_str()) {
        (RowKeys::Region, "1") => Member::Held,
        (RowKeys::Region, "global" | "kimi.ai") => vendor(Some(Region::Global), None),
        (RowKeys::Region, "mainland-cn" | "cn" | "kimi.com") => {
            vendor(Some(Region::MainlandCn), None)
        }
        (RowKeys::Flow(_), word) if Flow::parse(word).is_some() => vendor(None, Flow::parse(word)),
        (RowKeys::Plain | RowKeys::Flow(_), "1") => vendor(None, None),
        _ => Member::No,
    }
}

/// The vendors a sheet note can name, each with its words ([`notes_disqualify_for`]): the pooled
/// CLIs, and the sheet's other columns (`google`, `meta`), which scope a note away from them.
const NOTE_VENDORS: [(Option<Cli>, &[&str]); 6] = [
    (Some(Cli::Claude), &["claude", "anthropic"]),
    (Some(Cli::Codex), &["openai", "open ai", "codex", "chatgpt"]),
    (Some(Cli::Grok), &["xai", "grok"]),
    (Some(Cli::Kimi), &["kimi", "moonshot"]),
    (None, &["google", "gemini"]),
    (None, &["meta"]),
];

/// Whether `notes` disqualify a row's membership of `cli`'s pool: a disqualifying word
/// ([`notes_disqualify`]) in a note that names no vendor, or names `cli`'s ([`NOTE_VENDORS`],
/// whole words).
fn notes_disqualify_for(cli: Cli, notes: &str) -> bool {
    if !notes_disqualify(notes) {
        return false;
    }
    let lower = notes.to_ascii_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    let names = |phrase: &str| {
        let parts: Vec<&str> = phrase.split(' ').collect();
        words.windows(parts.len()).any(|w| w == parts.as_slice())
    };
    let named: Vec<Option<Cli>> = NOTE_VENDORS
        .iter()
        .filter(|(_, phrases)| phrases.iter().any(|p| names(p)))
        .map(|(vendor, _)| *vendor)
        .collect();
    named.is_empty() || named.contains(&Some(cli))
}

/// The one reading of the accounting sheet, for `cli`'s column ([`parse_accounting_csv`],
/// [`parse_accounting_csv_for`]): its rows, and the rows it held ([`CliFleet::held`]).
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSheetColumns",
        action = "Unique",
        project = "aterm_agent::harness::accounts_sync::sheet_columns_tests::sheet_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSheetColumns",
        action = "DuplicateEmail",
        project = "aterm_agent::harness::accounts_sync::sheet_columns_tests::sheet_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSheetColumns",
        action = "DuplicateMember",
        project = "aterm_agent::harness::accounts_sync::sheet_columns_tests::sheet_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSheetColumns",
        action = "MissingEmail",
        project = "aterm_agent::harness::accounts_sync::sheet_columns_tests::sheet_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSheetColumns",
        action = "MissingMember",
        project = "aterm_agent::harness::accounts_sync::sheet_columns_tests::sheet_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSheetColumns",
        action = "DuplicateAnchor",
        project = "aterm_agent::harness::accounts_sync::sheet_columns_tests::sheet_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSheetColumns",
        action = "MultiMachine",
        project = "aterm_agent::harness::accounts_sync::sheet_columns_tests::sheet_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSheetColumns",
        action = "ConflictingRows",
        project = "aterm_agent::harness::accounts_sync::sheet_columns_tests::sheet_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSheetColumns",
        action = "EquivalentRows",
        project = "aterm_agent::harness::accounts_sync::sheet_columns_tests::sheet_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSheetColumns",
        action = "ExcludedDuplicate",
        project = "aterm_agent::harness::accounts_sync::sheet_columns_tests::sheet_transition"
    )
)]
fn parse_sheet(
    csv_text: &str,
    source: &str,
    cli: Cli,
) -> Result<(FleetRoster, Vec<FleetAccount>), String> {
    let mut records = csv_records(csv_text);
    let header_line = records
        .next()
        .transpose()
        .map_err(|why| format!("CSV at {source}: {why}"))?
        .ok_or_else(|| format!("CSV at {source} is empty"))?;
    let header = parse_csv_line(header_line);
    let email_col = unique_column(&header, &["email"])
        .map_err(|()| format!("CSV at {source} has ambiguous `email` columns"))?
        .ok_or_else(|| format!("CSV at {source} has no `email` column"))?;
    let member_col = member_column(cli, &header)
        .map_err(|()| {
            format!(
                "CSV at {source} has ambiguous `{}` membership columns",
                cli.id()
            )
        })?
        .ok_or_else(|| {
            format!(
                "CSV at {source} has no `{}` column",
                csv_columns(cli).first().copied().unwrap_or("claude")
            )
        })?;
    let mut computer_cols: Vec<usize> = header
        .iter()
        .enumerate()
        .filter(|(_, h)| h.trim().eq_ignore_ascii_case("computer"))
        .map(|(i, _)| i)
        .collect();
    if let Some(&first_comp) = computer_cols.first()
        && header
            .get(first_comp + 1)
            .is_some_and(|h| h.trim().is_empty())
        && !computer_cols.contains(&(first_comp + 1))
    {
        let candidate = first_comp + 1;
        let last_claude = header
            .iter()
            .rposition(|h| h.trim().eq_ignore_ascii_case("claude"));
        let within_machines = match last_claude {
            Some(last) if candidate < last => member_column(Cli::Claude, &header)
                .map_err(|()| {
                    format!("CSV at {source} has ambiguous `claude` columns for its machine layout")
                })?
                .is_some(),
            Some(_) => false,
            None => candidate < member_col,
        };
        if within_machines {
            computer_cols.push(candidate);
        }
    }
    let notes_start = header
        .iter()
        .position(|h| h.trim().eq_ignore_ascii_case("notes"))
        .unwrap_or(header.len());

    let mut used_labels = BTreeSet::new();
    let mut seen_emails = BTreeMap::new();
    let mut accounts = Vec::new();
    let mut held = Vec::new();

    for record in records {
        let raw_line = record.map_err(|why| format!("CSV at {source}: {why}"))?;
        if raw_line.trim().is_empty() {
            continue;
        }
        let cols = parse_csv_line(raw_line);
        if cols.first().is_some_and(|c| c.trim().starts_with('*')) {
            continue;
        }
        let email = cols.get(email_col).map(|s| s.trim()).unwrap_or("");
        if email.is_empty() || email.starts_with('*') || accounts::email_refusal(email).is_some() {
            continue;
        }
        let member_val = cols.get(member_col).map(|s| s.trim()).unwrap_or("");
        let member = match cli {
            Cli::Claude if member_val == "1" => Member::Yes(accounts::Vendor::default()),
            Cli::Claude => Member::No,
            _ => member_of(cli, member_val),
        };
        let notes = if notes_start < cols.len() {
            cols[notes_start..].join(" ")
        } else {
            String::new()
        };
        let disqualified = match cli {
            Cli::Claude => notes_disqualify(&notes),
            _ => notes_disqualify_for(cli, &notes),
        };
        if matches!(member, Member::No) {
            let inactive = member_val.is_empty()
                || ["0", "no", "false"]
                    .iter()
                    .any(|word| member_val.eq_ignore_ascii_case(word));
            if disqualified || !inactive {
                let _ = observe_sheet_account(&mut seen_emails, email, None, source)?;
            }
            continue;
        }
        if disqualified {
            let _ = observe_sheet_account(&mut seen_emails, email, None, source)?;
            continue;
        }

        let mut machines = Vec::new();
        let mut slot: Option<u32> = None;
        let mut disqualified_cell = false;
        for &col_idx in &computer_cols {
            let Some(cell) = cols.get(col_idx).map(|s| s.trim()) else {
                continue;
            };
            if cell.is_empty() {
                continue;
            }
            if cell.starts_with('*') || cell.contains('?') {
                disqualified_cell = true;
                break;
            }
            if let Some((cell_machines, cell_slot)) = parse_computer_cell(cell) {
                for m in cell_machines {
                    if !machines.contains(&m) {
                        machines.push(m);
                    }
                }
                if slot.is_none() {
                    slot = cell_slot;
                }
            }
        }
        if disqualified_cell || machines.is_empty() {
            let _ = observe_sheet_account(&mut seen_emails, email, None, source)?;
            continue;
        }
        let (email_machines, email_slot) = email_machines_and_slot(email);
        for hint_m in email_machines {
            if !machines.contains(&hint_m) {
                machines.push(hint_m);
            }
        }
        if slot.is_none() {
            slot = email_slot;
        }

        if observe_sheet_account(
            &mut seen_emails,
            email,
            Some(SheetAccount {
                machines: machines.clone(),
                slot: slot.filter(|slot| *slot > 1),
                member,
            }),
            source,
        )? {
            continue;
        }

        let machine_label = match (&machines[..], slot) {
            ([only], None | Some(1)) => only.clone(),
            ([only], Some(n)) => format!("{only}-{n}"),
            (many, None | Some(1)) => many.join("-"),
            (many, Some(n)) => format!("{}-{n}", many.join("-")),
        };
        let base_label = match cli {
            Cli::Claude => machine_label,
            _ => format!("{}-{machine_label}", cli.id()),
        };
        let mut label = base_label.clone();
        let mut suffix = 2u32;
        loop {
            if let Some(why) = accounts::label_refusal(&label) {
                return Err(format!(
                    "CSV at {source} generates an account label that {why}; \
                     give this machine assignment an explicit label in fleet-accounts.toml"
                ));
            }
            if !used_labels.contains(&label) {
                break;
            }
            label = format!("{base_label}-{suffix}");
            suffix = suffix
                .checked_add(1)
                .ok_or_else(|| format!("CSV at {source} exhausts account label suffixes"))?;
        }
        used_labels.insert(label.clone());

        let class = if machines.len() > 1 {
            Class::Shared
        } else {
            Class::Dedicated
        };

        let (vendor, into) = match member {
            Member::Yes(vendor) => (vendor, &mut accounts),
            Member::Held | Member::No => (accounts::Vendor::default(), &mut held),
        };
        into.push(FleetAccount {
            label,
            machines,
            class,
            email: email.to_owned(),
            browser: None,
            vendor,
        });
    }

    Ok((
        FleetRoster {
            source: source.to_owned(),
            accounts,
        },
        held,
    ))
}

fn config_machine_id(config: Option<&Path>) -> Option<String> {
    let cfg_path = config?;
    let text = fs::read_to_string(cfg_path).ok()?;
    let table = aterm_toml::from_str::<aterm_toml::Table>(&text).ok()?;
    for section in ["ai", "fleet", "harness"] {
        if let Some(m) = table
            .get(section)
            .and_then(aterm_toml::Value::as_table)
            .and_then(|t| t.get("machine"))
            .and_then(aterm_toml::Value::as_str)
            .and_then(normalize_machine_id)
        {
            return Some(m);
        }
    }
    None
}

fn machine_record_id(home: &Path) -> Option<String> {
    let text = fs::read_to_string(home.join(".aterm/machine.toml")).ok()?;
    let table = aterm_toml::from_str::<aterm_toml::Table>(&text).ok()?;
    table
        .get("id")
        .and_then(aterm_toml::Value::as_str)
        .and_then(normalize_machine_id)
}

/// Detect this machine's canonical ID from an override, config file, or system hostname, and last
/// the home's `~/.aterm/machine.toml`. This crate's tests read neither the host's names nor the
/// person's home: they name the machine by an override or a config, as `ai_pool`'s host names
/// are none in them.
#[must_use]
pub fn detect_machine_id(override_machine: Option<&str>, config: Option<&Path>) -> Option<String> {
    if let Some(m) = override_machine {
        return normalize_machine_id(m);
    }
    if let Some(m) = config_machine_id(config) {
        return Some(m);
    }
    if cfg!(test) {
        return None;
    }
    let names = [
        ("/usr/sbin/scutil", &["--get", "LocalHostName"][..]),
        ("/usr/sbin/scutil", &["--get", "ComputerName"][..]),
        ("/bin/hostname", &[][..]),
    ]
    .into_iter()
    .filter_map(|(prog, args)| {
        let mut cmd = aterm_uds::spawnfd::command(prog);
        cmd.args(args);
        let cap = super::align::capture_bounded(cmd, Duration::from_secs(2), 256).ok()?;
        if cap.code == Some(0) {
            String::from_utf8(cap.stdout).ok()
        } else {
            None
        }
    });
    machine_id_among(names, || {
        aterm_types::dirs::home_dir().and_then(|home| machine_record_id(&home))
    })
}

/// [`detect_machine_id`]'s choice past an override and a config: the first of `names` (the
/// host's, each asked only once those before it named no machine) that [`normalize_machine_id`]
/// reads as one, else `record`'s, the home's machine record.
fn machine_id_among(
    names: impl IntoIterator<Item = String>,
    record: impl FnOnce() -> Option<String>,
) -> Option<String> {
    names
        .into_iter()
        .find_map(|name| normalize_machine_id(&name))
        .or_else(record)
}

fn read_default_profile_email(env: &Env) -> Option<String> {
    let path = env.claude_profile.as_deref().map(|own| {
        super::profile::effective(own, env.claude_dir().as_deref(), env.claude_custom_oauth)
    })?;
    profile_email(&path)
}

/// The default login's address at `home`, as a sync from init reads it ([`quiet_env`]).
fn home_default_email(home: &Path) -> Option<String> {
    profile_email(&super::profile::effective(
        &home.join(".claude.json"),
        Some(&home.join(".claude")),
        false,
    ))
}

/// The address Claude Code's profile at `path` is signed in as.
fn profile_email(path: &Path) -> Option<String> {
    let text = read_local_source_text(path).ok()?;
    let val: Value = aterm_json::from_str(&text).ok()?;
    val.get("oauthAccount")?
        .get("emailAddress")?
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// Find the newest standing quota wall across transcripts in the default Claude Code home.
pub fn find_default_home_standing_wall(
    claude_dir: &Path,
    now: i64,
) -> Option<upgrade::StandingWall> {
    let projects = claude_dir.join("projects");
    let entries = fs::read_dir(&projects).ok()?;
    let mut newest_wall: Option<upgrade::StandingWall> = None;
    let mut newest_clear_at: u64 = 0;
    for proj in entries.flatten() {
        let Ok(ft) = proj.file_type() else {
            continue;
        };
        if !ft.is_dir() {
            continue;
        }
        let Ok(files) = fs::read_dir(proj.path()) else {
            continue;
        };
        for file in files.flatten() {
            let path = file.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let Some(tail) = read_file_tail(&path, TRANSCRIPT_TAIL_BYTES) else {
                continue;
            };
            let Some(wall) = upgrade::transcript_standing_wall(&tail) else {
                if let Some(ans) = upgrade::transcript_last_answer(&tail, u64::MAX) {
                    newest_clear_at = newest_clear_at.max(ans);
                }
                continue;
            };
            let until = wall.until.and_then(|u| i64::try_from(u).ok());
            if until.is_some_and(|u| u > now)
                && newest_wall.as_ref().is_none_or(|prev| wall.at > prev.at)
            {
                newest_wall = Some(wall);
            }
        }
    }
    newest_wall.filter(|w| w.at >= newest_clear_at)
}

fn match_session_context_label(line: &str, roster: &Roster) -> Option<Option<String>> {
    let tok = session_context_email(line)?;
    let matched = roster
        .rows
        .iter()
        .find(|r| {
            r.email
                .as_deref()
                .is_some_and(|re| accounts::email_matches(re, &tok))
        })
        .map(|r| r.label.clone());
    Some(matched)
}

/// One row of a transcript [`find_transcript_walls_by_email`] books, in the order it was written.
enum TranscriptRow {
    /// A `session_context` naming the conversation's account (`None`: no roster row's), and the
    /// instant it was written (its row's time, else the last time the transcript showed before it).
    Context(Option<String>, u64),
    /// A `/login` that succeeded in this conversation, at its row's time where it shows one.
    Login(Option<u64>),
    /// A rejected quota row that stands as a wall.
    Wall(upgrade::StandingWall),
    /// A model's answer, at its row's time.
    Answer(u64),
}

/// Who a transcript's rows are booked to ([`find_transcript_walls_by_email`]).
enum Booking {
    /// The default login's row, presumed where the transcript has named no account yet.
    Presumed(String),
    /// The row a `session_context` written at the instant named.
    Named(String, u64),
}

/// The row's time (`timestamp`) of the transcript line `line`.
fn transcript_row_at(line: &str) -> Option<u64> {
    let v: Value = aterm_json::from_str(line).ok()?;
    v.get("timestamp")
        .and_then(Value::as_str)
        .and_then(super::upgrade_models::parse_utc)
}

/// The rows of one transcript [`find_transcript_walls_by_email`] books.
fn transcript_rows(path: &Path, roster: &Roster, now: i64) -> Vec<TranscriptRow> {
    let Ok(f) = fs::File::open(path) else {
        return Vec::new();
    };
    let mut reader = io::BufReader::new(f);
    let mut rows = Vec::new();
    let mut last_at = 0;
    let mut line = String::new();
    while reader.read_line(&mut line).ok().is_some_and(|n| n > 0) {
        if line.contains("\"session_context\"") && line.contains("\"userEmail\"") {
            if let Some(matched) = match_session_context_label(&line, roster) {
                let at = transcript_row_at(&line).unwrap_or(last_at);
                last_at = last_at.max(at);
                rows.push(TranscriptRow::Context(matched, at));
            }
        } else if line.contains("<local-command-stdout>Login successful</local-command-stdout>") {
            let at = transcript_row_at(&line);
            if let Some(at) = at {
                last_at = last_at.max(at);
            }
            rows.push(TranscriptRow::Login(at));
        } else if line.contains("\"assistant\"") {
            if line.contains("\"quotaLimits\"") && line.contains("\"rejected\"") {
                if let Some(wall) = upgrade::transcript_standing_wall(&line) {
                    last_at = last_at.max(wall.at);
                    let until = wall.until.and_then(|u| i64::try_from(u).ok());
                    if until.is_some_and(|u| u > now) {
                        rows.push(TranscriptRow::Wall(wall));
                    }
                }
            } else if line.contains("\"model\"")
                && !line.contains("<synthetic>")
                && !line.contains("\"isApiErrorMessage\":true")
                && !line.contains("\"isApiErrorMessage\": true")
                && !line.contains("\"isSidechain\":true")
                && !line.contains("\"isSidechain\": true")
                && let Some(ans_at) = upgrade::transcript_last_answer(&line, u64::MAX)
            {
                last_at = last_at.max(ans_at);
                rows.push(TranscriptRow::Answer(ans_at));
            }
        }
        line.clear();
    }
    rows
}

/// Scan recent transcripts and attribute standing quota walls to active account rows.
///
/// A transcript's rows are booked to the row its last `session_context` names (before any, the
/// default login's row, presumed only for rows written since the newest `/login` the scan
/// finds), and a `/login` that succeeded in it books nothing until the next one. A
/// `session_context` is the vendor's word as its process cached it, and a `/login` in another
/// tab replaces the default login under every session on it without that word changing: on m2
/// on 2026-10-27, two tabs went on naming the login a person's `/login` replaced for 40 to 75
/// minutes, one in a `session_context` written 40 minutes after the `/login`, while the new
/// login served their answers. So a `/login` any scanned transcript records at its row's time
/// ends every naming before it, in every transcript: no row written after it is booked to the
/// account a `session_context` written before it named. After the newest one a row is booked
/// only to a row with a login on this machine (the default login's row, or one whose own
/// directory holds a login: [`accounts::enrolled`]), as a row that `/login` signed out of the
/// default login has none to serve it. Between two `/login`s a row is booked as the
/// `session_context` written since says.
#[must_use]
pub fn find_transcript_walls_by_email(
    claude_dir: &Path,
    roster: &Roster,
    now: i64,
) -> BTreeMap<String, upgrade::StandingWall> {
    let projects = claude_dir.join("projects");
    let Ok(entries) = fs::read_dir(&projects) else {
        return BTreeMap::new();
    };
    let mut files_with_mtime: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    for proj in entries.flatten() {
        if !proj.file_type().is_ok_and(|ft| ft.is_dir()) {
            continue;
        }
        let Ok(files) = fs::read_dir(proj.path()) else {
            continue;
        };
        for file in files.flatten() {
            let path = file.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let mtime = file
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            files_with_mtime.push((mtime, path));
        }
    }
    files_with_mtime.sort_by_key(|a| std::cmp::Reverse(a.0));
    files_with_mtime.truncate(16);

    let transcripts: Vec<Vec<TranscriptRow>> = files_with_mtime
        .iter()
        .map(|(_, path)| transcript_rows(path, roster, now))
        .collect();
    let logins: Vec<u64> = transcripts
        .iter()
        .flatten()
        .filter_map(|row| match row {
            TranscriptRow::Login(at) => *at,
            _ => None,
        })
        .collect();
    let newest_login = logins.iter().copied().max();
    let enrolled = accounts::enrolled(roster);
    let default_label = roster.row_for_login(None).map(|r| r.label.clone());
    let booked = |booking: &Booking, at: u64| -> Option<String> {
        match booking {
            Booking::Presumed(label) => newest_login
                .is_none_or(|login| at >= login)
                .then(|| label.clone()),
            Booking::Named(label, named) => (!logins.iter().any(|&l| *named < l && l <= at)
                && (newest_login.is_none_or(|login| at < login) || enrolled.contains(label)))
            .then(|| label.clone()),
        }
    };
    let mut newest_wall: BTreeMap<String, upgrade::StandingWall> = BTreeMap::new();
    let mut newest_answer: BTreeMap<String, u64> = BTreeMap::new();
    for rows in transcripts {
        let mut booking = default_label.clone().map(Booking::Presumed);
        for row in rows {
            match row {
                TranscriptRow::Context(matched, at) => {
                    booking = matched.map(|label| Booking::Named(label, at));
                }
                TranscriptRow::Login(_) => booking = None,
                TranscriptRow::Wall(wall) => {
                    if let Some(label) = booking.as_ref().and_then(|b| booked(b, wall.at))
                        && newest_wall.get(&label).is_none_or(|prev| wall.at > prev.at)
                    {
                        newest_wall.insert(label, wall);
                    }
                }
                TranscriptRow::Answer(ans_at) => {
                    if let Some(label) = booking.as_ref().and_then(|b| booked(b, ans_at)) {
                        let entry = newest_answer.entry(label).or_insert(0);
                        *entry = (*entry).max(ans_at);
                    }
                }
            }
        }
    }
    newest_wall.retain(|label, wall| wall.at >= newest_answer.get(label).copied().unwrap_or(0));
    newest_wall
}

fn read_file_tail(path: &Path, max_bytes: u64) -> Option<String> {
    let mut f = fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    if len > max_bytes {
        let _ = f.seek(SeekFrom::Start(len - max_bytes)).ok()?;
    }
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    if len > max_bytes {
        Some(
            text.split_once('\n')
                .map_or("", |(_, rest)| rest)
                .to_owned(),
        )
    } else {
        Some(text.into_owned())
    }
}

/// The label discovery gives an account of `machines` in `slot` (`m8`, `m8-2`, `m106-m38`).
fn discovered_label(machines: &[String], slot: Option<u32>) -> String {
    match (machines, slot) {
        ([only], None | Some(1)) => only.clone(),
        ([only], Some(n)) => format!("{only}-{n}"),
        (many, None | Some(1)) => many.join("-"),
        (many, Some(n)) => format!("{}-{n}", many.join("-")),
    }
}

/// `email` as one more account of `machine` alone, labelled as discovery labels it
/// ([`discovered_label`], its slot where the email's own machine hint is `machine`) and made
/// unique against `used` by `-2`, `-3`, …; `None` for an email or a label the roster refuses.
fn local_account(email: &str, machine: &str, used: &BTreeSet<String>) -> Option<FleetAccount> {
    let email = email.trim();
    if accounts::email_refusal(email).is_some() {
        return None;
    }
    let machines = vec![machine.to_owned()];
    let (hinted, slot) = email_machines_and_slot(email);
    let slot = slot.filter(|_| hinted.is_empty() || hinted == machines);
    let base = discovered_label(&machines, slot);
    let mut label = base.clone();
    let mut suffix = 2u32;
    while used.contains(&label) {
        label = format!("{base}-{suffix}");
        suffix = suffix.checked_add(1)?;
    }
    accounts::label_refusal(&label)
        .is_none()
        .then(|| FleetAccount {
            label,
            machines,
            class: Class::Dedicated,
            email: email.to_owned(),
            browser: None,
            vendor: accounts::Vendor::default(),
        })
}

// A label can be reassigned by the fleet; only the explicit login address carries
// the retained canonical-identity assertion to a new label. Ambiguity grants none.
fn retained_login_row<'a>(existing: Option<&'a Roster>, email: &str) -> Option<&'a Row> {
    if accounts::email_refusal(email).is_some() {
        return None;
    }
    let mut rows = existing?.rows.iter().filter(|row| {
        row.email
            .as_deref()
            .is_some_and(|held| accounts::email_matches(held, email))
    });
    let row = rows.next()?;
    rows.next().is_none().then_some(row)
}

fn default_identity_hint<'a>(
    existing: Option<&'a Roster>,
    observed: Option<&'a str>,
) -> Option<&'a str> {
    observed
        .map(str::trim)
        .filter(|email| !email.is_empty())
        .or_else(|| {
            existing
                .and_then(|roster| roster.row_for_login(None))
                .filter(|row| {
                    row.email
                        .as_deref()
                        .is_some_and(|email| accounts::email_refusal(email).is_none())
                })
                .and_then(Row::expected_identity_email)
        })
}

/// Build the machine's [`Roster`] from `fleet` for `machine`. The row whose email is
/// `default_email` is the default login's (no `dir`); where none of `machine`'s rows has that
/// email, the default login holds an account they do not list, and the roster lists it too, as
/// the default login's row: the fleet's own row for it where another machine's lists it, else
/// the label `existing` already gives that account where the fleet gives it to no account, else
/// a new one ([`local_account`], unique among the fleet's labels and `existing`'s), so no label
/// is kept on an account it is not and an account keeps its label while it can.
/// With no observed profile email, the prior default row's declared identity remains the hint.
/// An explicit alias expectation follows its unique login address across label changes.
#[must_use]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountAliasRelabel",
        action = "Build",
        project = "aterm_agent::harness::accounts_sync::alias_relabel_tests::alias_transition"
    )
)]
pub fn build_machine_roster(
    fleet: &FleetRoster,
    machine: &str,
    store: &accounts::Store,
    existing: Option<&Roster>,
    default_email: Option<&str>,
) -> Roster {
    let mut fleet_rows = fleet.accounts_for_machine(machine);
    if fleet_rows.is_empty() {
        return existing.cloned().unwrap_or_default();
    }
    let default_email = default_identity_hint(existing, default_email);
    let matches_identity = |acct: &FleetAccount, email: &str| {
        let expected = retained_login_row(existing, &acct.email)
            .and_then(Row::expected_identity_email)
            .unwrap_or(&acct.email);
        accounts::email_matches(expected, email)
    };
    let unlisted = default_email
        .filter(|de| {
            !fleet_rows.iter().any(|r| matches_identity(r, de))
                && fleet_rows.len() < accounts::MAX_ACCOUNTS
        })
        .and_then(|de| {
            fleet
                .accounts
                .iter()
                .find(|a| matches_identity(a, de))
                .cloned()
                .or_else(|| {
                    let mut used: BTreeSet<String> =
                        fleet.accounts.iter().map(|a| a.label.clone()).collect();
                    let kept = existing.and_then(|ex| {
                        ex.rows.iter().find(|r| {
                            r.expected_identity_email()
                                .is_some_and(|re| accounts::email_matches(re, de))
                        })
                    });
                    if let Some(kept) = kept.filter(|row| !used.contains(&row.label)) {
                        return local_account(
                            kept.email.as_deref().unwrap_or(de),
                            machine,
                            &BTreeSet::new(),
                        )
                        .map(|acct| FleetAccount {
                            label: kept.label.clone(),
                            ..acct
                        });
                    }
                    used.extend(
                        existing
                            .iter()
                            .flat_map(|ex| ex.rows.iter().map(|r| r.label.clone())),
                    );
                    local_account(
                        kept.and_then(|row| row.email.as_deref()).unwrap_or(de),
                        machine,
                        &used,
                    )
                })
        });
    if let Some(acct) = &unlisted {
        fleet_rows.push(acct);
    }

    let default_label: String = default_email
        .and_then(|de| {
            fleet_rows
                .iter()
                .find(|r| matches_identity(r, de))
                .map(|r| r.label.clone())
        })
        .or_else(|| {
            existing
                .and_then(|ex| ex.row_for_login(None))
                .filter(|r| fleet_rows.iter().any(|fr| fr.label == r.label))
                .map(|r| r.label.clone())
        })
        .unwrap_or_else(|| fleet_rows[0].label.clone());

    let mut rows = Vec::with_capacity(fleet_rows.len());
    for acct in &fleet_rows {
        let existing_row = existing.and_then(|ex| ex.row(&acct.label));
        let dir = if acct.label == default_label {
            None
        } else if let Some(prev_dir) = existing_row.and_then(|r| r.dir.clone()) {
            Some(prev_dir)
        } else {
            store
                .login_dir(&acct.label)
                .map(|p| p.to_string_lossy().into_owned())
        };
        let browser = acct
            .browser
            .clone()
            .or_else(|| retained_login_row(existing, &acct.email).and_then(|r| r.browser.clone()));
        rows.push(Row {
            label: acct.label.clone(),
            class: acct.class,
            dir,
            email: Some(acct.email.clone()),
            identity_email: retained_login_row(existing, &acct.email)
                .and_then(|row| row.identity_email.clone()),
            browser,
            held_by: None,
            was: Vec::new(),
            vendor: Default::default(),
        });
    }

    rows.sort_by_key(|r| (r.dir.is_some(), r.class, r.label.clone()));

    Roster { rows }
}

/// Format a [`Roster`] into valid `accounts.toml` text.
/// # Errors
/// The generated rows do not admit as one complete roster.
pub fn format_roster_toml(roster: &Roster) -> Result<String, String> {
    accounts::append_rows("", &roster.rows)
}

fn toml_quote(s: &str) -> String {
    aterm_toml::Value::String(s.to_owned()).to_string()
}

fn parse_local_machines_and_slot(local: &str) -> (Vec<String>, Option<u32>) {
    let tokens: Vec<&str> = local.split(['.', '-', '_']).collect();
    let mut machines = Vec::new();
    let mut slot = None;
    for (idx, tok) in tokens.iter().enumerate() {
        if let Some(m) = normalize_machine_token(tok) {
            if !machines.contains(&m) {
                machines.push(m);
            }
            for (rem_idx, next) in tokens[idx + 1..].iter().enumerate() {
                if next.is_empty() || !next.bytes().all(|b| b.is_ascii_digit()) {
                    break;
                }
                let Ok(n) = next.parse::<u32>() else {
                    break;
                };
                let is_last = tokens
                    .get(idx + 1 + rem_idx + 1)
                    .is_none_or(|t| t.is_empty() || !t.bytes().all(|b| b.is_ascii_digit()));
                if is_last && n <= 9 {
                    slot = Some(n);
                } else {
                    let m2 = format!("m{n}");
                    if !machines.contains(&m2) {
                        machines.push(m2);
                    }
                }
            }
            break;
        }
    }
    (machines, slot)
}

fn email_machines_and_slot(email: &str) -> (Vec<String>, Option<u32>) {
    let Some((raw_local, _)) = email.split_once('@') else {
        return (Vec::new(), None);
    };
    let (base, plus_tag) = raw_local
        .split_once('+')
        .map_or((raw_local, None), |(b, t)| (b, Some(t)));
    let (mut machines, mut slot) = parse_local_machines_and_slot(base);
    if let Some(tag) = plus_tag {
        if machines.is_empty() {
            (machines, slot) = parse_local_machines_and_slot(tag);
        } else if slot.is_none()
            && let Some(first) = tag.split(['.', '-', '_', '+']).next()
            && !first.is_empty()
            && first.bytes().all(|b| b.is_ascii_digit())
            && let Ok(n) = first.parse::<u32>()
            && (1..=9).contains(&n)
        {
            slot = Some(n);
        }
    }
    (machines, slot)
}

/// Where this machine's person said the fleet accounts come from ([`configured_source`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FleetSource {
    /// Local fleet roster file beside `accounts.toml` ([`OptIn::Named`]).
    File(PathBuf),
    /// `[fleet] accounts_source` in the config file ([`SOURCE_KEY`]).
    Key(String),
}

impl FleetSource {
    /// The source as a sync reads it: the file's path, or the key's value.
    #[must_use]
    pub fn source(&self) -> String {
        match self {
            Self::File(path) => path.to_string_lossy().into_owned(),
            Self::Key(source) => source.clone(),
        }
    }

    /// Which of the two it is, for a log line.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::File(_) => "fleet-file",
            Self::Key(_) => "key",
        }
    }
}

/// THE ONE OPT-IN of the fleet accounts feature.
#[must_use]
pub fn configured_source(config: Option<&Path>, roster: Option<&Path>) -> Option<FleetSource> {
    if let Some(fleet) = roster
        .and_then(fleet_file_beside)
        .filter(|fleet| fleet_file_opt_in(fleet).is_some_and(OptIn::opts_in_by_file))
    {
        return Some(FleetSource::File(fleet));
    }
    config.and_then(source_key).map(FleetSource::Key)
}

/// Where this machine's Claude Code roster rows come from, in the order a sync from init reads
/// its sources ([`sync_accounts_for_init`]): what `aterm harness accounts` says of its list, and
/// where `accounts add` or `adopt-default` lists a row so that the next sync keeps it ([`fleet_origin`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FleetOrigin {
    /// No fleet file and no source: `accounts.toml` is the list, as it was written.
    Roster,
    /// Fleet files on this machine that a sync reads and writes back as they hold: the one
    /// beside the roster, and a copy in `~/.secrets` that a sync from init reads first and that
    /// aterm keeps of it ([`own_secrets_copy`]) or that holds this machine's rows alone
    /// ([`fleet_of_this_machine`]). `opt_in` is the word they
    /// record (the beside file's where it opts in, else the copy's): [`OptIn::Discovered`]
    /// where the list was found on this Mac, from its own logins, profiles and transcripts;
    /// [`OptIn::Named`] where a sync was handed a source (and where discovery wrote it before it
    /// recorded `discovered`); none for a file that opts nothing in, which a sync from init
    /// still reads.
    Local {
        /// The files, the one beside the roster first.
        files: Vec<PathBuf>,
        /// The word they record.
        opt_in: Option<OptIn>,
    },
    /// `[fleet] accounts_source`, which this machine reads and does not write.
    Key(String),
    /// A sheet or fleet in `~/.secrets` (other machines' rows among them) that aterm did not
    /// write ([`own_secrets_copy`]): the vault's subscriber writes that folder, or a person did.
    /// This machine reads it and does not write it.
    Vault(PathBuf),
}

impl FleetOrigin {
    /// One word for it: `roster`, `discovered`, `named`, `fleet-file`, `key` or `vault`.
    #[must_use]
    pub const fn word(&self) -> &'static str {
        match self {
            Self::Roster => "roster",
            Self::Local {
                opt_in: Some(OptIn::Discovered),
                ..
            } => "discovered",
            Self::Local {
                opt_in: Some(OptIn::Named),
                ..
            } => "named",
            Self::Local { .. } => "fleet-file",
            Self::Key(_) => "key",
            Self::Vault(_) => "vault",
        }
    }

    /// The file or source it names; none for [`Self::Roster`].
    #[must_use]
    pub fn source(&self) -> Option<String> {
        match self {
            Self::Roster => None,
            Self::Local { files, .. } => files.first().map(|f| f.display().to_string()),
            Self::Key(source) => Some(source.clone()),
            Self::Vault(path) => Some(path.display().to_string()),
        }
    }

    /// What `aterm harness accounts` says of the list.
    #[must_use]
    pub fn words(&self) -> String {
        let source = self.source().unwrap_or_default();
        match self {
            Self::Roster => "as written here (no fleet source names these accounts)".to_owned(),
            Self::Local {
                opt_in: Some(OptIn::Discovered),
                ..
            } => format!(
                "found on this Mac, in its own logins, profiles and transcripts ({source} says \
                 `discovered`); aterm adds each account it finds later"
            ),
            Self::Local {
                opt_in: Some(OptIn::Named),
                ..
            } => format!("{source}, kept from a source a sync was handed (`named`)"),
            Self::Local { .. } => {
                format!("{source}, which opts nothing in; a sync from `aterm init` reads it")
            }
            Self::Key(_) => format!("`[fleet] accounts_source` ({source})"),
            Self::Vault(_) => format!("{source}, which this machine reads and does not write"),
        }
    }
}

/// [`FleetOrigin`] of the roster at `roster`, for `home` and the config file `config`.
#[must_use]
pub fn fleet_origin(home: Option<&Path>, config: Option<&Path>, roster: &Path) -> FleetOrigin {
    let beside = fleet_file_beside(roster).filter(|fleet| source_entry_present(fleet));
    let beside_opt_in = beside.as_deref().and_then(fleet_file_opt_in);
    let beside_opts_in = beside_opt_in.is_some_and(OptIn::opts_in_by_file);
    if !beside_opts_in && let Some(source) = config.and_then(source_key) {
        return FleetOrigin::Key(source);
    }
    let secrets = home.map(|home| home.join(".secrets"));
    if config.and_then(source_key).is_none()
        && let Some(pool) = secrets
            .as_ref()
            .map(|s| s.join(super::ai_pool::AI_POOL_FILE))
            .filter(|p| pool_entry_present(p))
    {
        return FleetOrigin::Vault(pool);
    }
    let sheet = secrets
        .as_ref()
        .map(|secrets| secrets.join(ACCOUNTS_CSV_FILE))
        .filter(|sheet| source_entry_present(sheet));
    if let Some((home, vault)) = home
        .zip(
            secrets
                .as_ref()
                .map(|secrets| secrets.join(FLEET_ROSTER_FILE))
                .filter(|vault| source_entry_present(vault)),
        )
        .filter(|(home, vault)| {
            !beside
                .as_deref()
                .is_some_and(|beside| superseded_secrets_copy(home, vault, beside))
        })
    {
        let own = beside
            .as_deref()
            .is_some_and(|beside| own_secrets_copy(home, &vault, beside));
        // The sheet outranks aterm's own copy where it gives this machine rows, as a sync from
        // init reads them.
        if own
            && let Some(sheet) = sheet
            && sheet_names_this_machine(home, config, &sheet)
        {
            return FleetOrigin::Vault(sheet);
        }
        let read = read_local_source_text(&vault).ok().and_then(|text| {
            let fleet = FleetRoster::from_toml(&text, &vault.display().to_string()).ok()?;
            Some((opt_in_of(&text), fleet))
        });
        return match read {
            Some((opt_in, fleet)) if own || fleet_of_this_machine(opt_in, &fleet) => {
                let files: Vec<PathBuf> = beside.into_iter().chain([vault]).collect();
                FleetOrigin::Local {
                    opt_in: if beside_opts_in {
                        beside_opt_in
                    } else {
                        opt_in
                    },
                    files,
                }
            }
            _ => FleetOrigin::Vault(vault),
        };
    }
    if let Some(sheet) = sheet {
        return FleetOrigin::Vault(sheet);
    }
    match beside {
        Some(fleet) => FleetOrigin::Local {
            files: vec![fleet],
            opt_in: beside_opt_in,
        },
        None => FleetOrigin::Roster,
    }
}

/// Whether a fleet roster entry stands beside `roster` without a readable opt-in
/// ([`configured_source`]). An unusable entry stays visible but grants no opt-in.
#[must_use]
pub fn fleet_file_ignored(roster: &Path) -> bool {
    fleet_file_beside(roster).is_some_and(|fleet| {
        source_entry_present(&fleet)
            && !fleet_file_opt_in(&fleet).is_some_and(OptIn::opts_in_by_file)
    })
}

fn fleet_file_beside(roster: &Path) -> Option<PathBuf> {
    roster.parent().map(|dir| dir.join(FLEET_ROSTER_FILE))
}

/// Whether `copy`, `home`'s `~/.secrets/fleet-accounts.toml`, is the copy aterm keeps there of
/// `beside`, the fleet file beside the roster, and not a file the vault delivered or a person
/// wrote. All three hold:
/// - aterm wrote it: its first line is one only aterm writes ([`FLEET_HEAD_NAMED`],
///   [`FLEET_HEAD_FOUND`]), which every aterm that wrote such a copy wrote;
/// - removing it loses nothing: it lists `beside`'s accounts, as they are (its comments and its
///   `opt_in` aside, which older builds wrote otherwise and a sync does not carry over);
/// - the vault did not write it: the vault's own record of the files it wrote on this Mac
///   ([`aterm_secrets::client::delivered_files`]) does not list it, and that record can be read.
///
/// A fleet the vault delivers is the vault's even where aterm on another Mac wrote it, and keeps
/// its precedence; a copy an older aterm writes again is aterm's again, whenever it writes it.
fn own_secrets_copy(home: &Path, copy: &Path, beside: &Path) -> bool {
    let (Ok(text), Ok(kept)) = (read_local_source_text(copy), read_local_source_text(beside))
    else {
        return false;
    };
    let accounts = |text: &str| {
        FleetRoster::from_toml(text, FLEET_ROSTER_FILE)
            .ok()
            .map(|fleet| fleet.accounts)
    };
    text.lines()
        .next()
        .is_some_and(|head| [FLEET_HEAD_NAMED, FLEET_HEAD_FOUND].contains(&head))
        && accounts(&text).is_some_and(|listed| accounts(&kept) == Some(listed))
        && vault_delivered(home).is_some_and(|delivered| !delivered.contains(copy))
}

/// Whether `copy`, `home`'s `~/.secrets/fleet-accounts.toml`, is a copy aterm wrote there that a
/// later word on this machine's list superseded: aterm wrote it (its first line is
/// [`FLEET_HEAD_NAMED`] or [`FLEET_HEAD_FOUND`]), the vault did not (its readable record of the
/// files it wrote does not list it, as for [`own_secrets_copy`]), it lists other accounts than
/// `beside`, the fleet file beside the roster, and `beside` opts in by file and was written after
/// it, by a sync a person handed a source or by the person's own edit. A sync from init then
/// reads `beside`, not the copy, so the copy cannot put back the accounts the later word dropped;
/// the copy is left as it is, and an edit that makes it the newer file makes it a source again.
fn superseded_secrets_copy(home: &Path, copy: &Path, beside: &Path) -> bool {
    let written = |path: &Path| fs::metadata(path).and_then(|meta| meta.modified()).ok();
    let accounts = |path: &Path| {
        let text = read_local_source_text(path).ok()?;
        let fleet = FleetRoster::from_toml(&text, FLEET_ROSTER_FILE).ok()?;
        Some((text, fleet.accounts))
    };
    let (Some((text, listed)), Some((_, kept))) = (accounts(copy), accounts(beside)) else {
        return false;
    };
    text.lines()
        .next()
        .is_some_and(|head| [FLEET_HEAD_NAMED, FLEET_HEAD_FOUND].contains(&head))
        && listed != kept
        && fleet_file_opt_in(beside).is_some_and(OptIn::opts_in_by_file)
        && written(beside)
            .zip(written(copy))
            .is_some_and(|(beside_at, copy_at)| beside_at > copy_at)
        && vault_delivered(home).is_some_and(|delivered| !delivered.contains(copy))
}

/// The files the vault wrote under `home` ([`aterm_secrets::client::delivered_files`]), `None`
/// where its record cannot be read. None where no vault runs: it runs on unix alone.
fn vault_delivered(home: &Path) -> Option<BTreeSet<PathBuf>> {
    #[cfg(unix)]
    {
        aterm_secrets::client::delivered_files(&aterm_secrets::client::Paths::for_home(home)).ok()
    }
    #[cfg(not(unix))]
    {
        let _ = home;
        Some(BTreeSet::new())
    }
}

/// Whether the sheet at `sheet` gives this machine rows, the machine a sync from init takes them
/// for ([`sync_machine`]: `config`'s `[fleet] machine`, else this host's id, else the machine of
/// the row the default login at `home` holds). An unreadable or refused sheet counts as giving
/// them, so the sync that reads it says why.
fn sheet_names_this_machine(home: &Path, config: Option<&Path>, sheet: &Path) -> bool {
    let Ok(fleet) = read_local_source_text(sheet)
        .map_err(|why| why.to_string())
        .and_then(|text| parse_accounting_csv(&text, ACCOUNTS_CSV_FILE))
    else {
        return true;
    };
    let default_email = home_default_email(home);
    machine_for(config, &fleet, default_email.as_deref(), None)
        .is_some_and(|machine| !fleet.accounts_for_machine(&machine).is_empty())
}

fn fleet_file_opt_in(fleet: &Path) -> Option<OptIn> {
    opt_in_of(&read_local_source_text(fleet).ok()?)
}

/// The [`OPT_IN_KEY`] a fleet roster's `text` records, where it is TOML that names one.
fn opt_in_of(text: &str) -> Option<OptIn> {
    let table: aterm_toml::Table = aterm_toml::from_str(text).ok()?;
    OptIn::parse(table.get(OPT_IN_KEY)?.as_str()?)
}

/// Whether `fleet`, whose file records `opt_in`, is this machine's own list, which this machine
/// writes (`accounts add` or `adopt-default` lists a row there): a fleet discovery made ([`OptIn::Discovered`]), or
/// a `named` one whose every row names one and the same machine alone. A fleet from a vault, a
/// sheet or a URL lists the fleet's machines and stays as its source wrote it.
fn fleet_of_this_machine(opt_in: Option<OptIn>, fleet: &FleetRoster) -> bool {
    match opt_in {
        Some(OptIn::Discovered) => true,
        Some(OptIn::Named) => fleet.machines().len() == 1,
        Some(OptIn::Key) | None => false,
    }
}

/// Whether local discovery may add this machine's accounts to `fleet`, whose file records
/// `opt_in`: a fleet discovery made ([`OptIn::Discovered`]); or a `named` one of this machine
/// alone ([`fleet_of_this_machine`]) every account of which this machine's own files show
/// (`shown`, [`Shown::shows_all`]), as discovery wrote its fleets before it recorded
/// `discovered`. A list a person handed `accounts sync` that names an account this machine shows
/// nothing of is theirs and is never added to; `shown` is read only for a `named` one.
fn fleet_takes_discovered(
    opt_in: Option<OptIn>,
    fleet: &FleetRoster,
    shown: impl FnOnce() -> Shown,
) -> bool {
    match opt_in {
        Some(OptIn::Discovered) => true,
        Some(OptIn::Named) => fleet_of_this_machine(opt_in, fleet) && shown().shows_all(fleet),
        Some(OptIn::Key) | None => false,
    }
}

fn source_key(config: &Path) -> Option<String> {
    let text = fs::read_to_string(config).ok()?;
    let table: aterm_toml::Table = aterm_toml::from_str(&text).ok()?;
    table
        .get(SOURCE_TABLE)?
        .as_table()?
        .get(SOURCE_KEY)?
        .as_str()
        .map(str::trim)
        .filter(|source| !source.is_empty())
        .map(str::to_owned)
}

type Fetch<'f> = dyn Fn(&Env, &str) -> Result<String, String> + Sync + 'f;

#[derive(Debug, Clone)]
struct PoolSyncCtx {
    pool: super::ai_pool::Pool,
    identity: super::ai_pool::LocalPoolIdentity,
}

impl PoolSyncCtx {
    fn holder_for_row(&self, cli: Cli, row: &Row) -> Option<String> {
        let email = row.expected_identity_email().or(row.email.as_deref())?;
        let Some(acct) = self.pool.accounts.iter().find(|a| {
            a.provider == cli
                && accounts::same_provider_realm(cli, row.vendor.region, a.vendor.region)
                && accounts::email_matches(&a.email, email)
        }) else {
            return Some(super::ai_pool::POOL_HOLDER_UNASSIGNED.to_owned());
        };
        (!acct.is_warm_for(&self.identity)).then(|| acct.holder_word().to_owned())
    }

    fn is_home_for_row(&self, cli: Cli, row: &Row) -> bool {
        let Some(email) = row.expected_identity_email().or(row.email.as_deref()) else {
            return false;
        };
        self.pool.accounts.iter().any(|a| {
            a.provider == cli
                && accounts::same_provider_realm(cli, row.vendor.region, a.vendor.region)
                && accounts::email_matches(&a.email, email)
                && a.home_mac().is_some_and(|h| self.identity.matches(h))
        })
    }
}

fn project_pool_fleet(
    pool: &super::ai_pool::Pool,
    cli: Cli,
    identity: Option<&super::ai_pool::LocalPoolIdentity>,
) -> FleetRoster {
    let mut accounts = Vec::new();
    for (acct, label) in pool.accounts_for_cli(cli) {
        if acct.warm.is_empty() {
            continue;
        }
        let mut machines = Vec::with_capacity(acct.warm.len());
        for slot in &acct.warm {
            let mapped = match identity {
                Some(id) if id.matches(slot) => id.primary.clone(),
                _ => slot.clone(),
            };
            if !machines.contains(&mapped) {
                machines.push(mapped);
            }
        }
        accounts.push(FleetAccount {
            label,
            machines,
            class: acct.class(),
            email: acct.email.clone(),
            browser: acct.browser.clone(),
            vendor: match cli {
                Cli::Claude => accounts::Vendor::default(),
                _ => acct.vendor,
            },
        });
    }
    FleetRoster {
        source: pool.source.clone(),
        accounts,
    }
}

/// Load a [`FleetRoster`] from `source`, or from [`configured_source`] when `source` is `None`.
/// # Errors
/// No source, or the source could not be read or parsed.
pub fn load_fleet_source(env: &Env, source: Option<&str>) -> Result<FleetRoster, String> {
    load_fleet_source_for_machine(env, source, None, &fetch_source_text).map(|(fleet, _, _)| fleet)
}

#[cfg(test)]
fn load_fleet_source_with(
    env: &Env,
    source: Option<&str>,
    fetch: &Fetch<'_>,
) -> Result<(FleetRoster, OptIn), String> {
    load_fleet_source_for_machine(env, source, None, fetch)
        .map(|(fleet, opt_in, _)| (fleet, opt_in))
}

fn load_fleet_source_for_machine(
    env: &Env,
    source: Option<&str>,
    machine_override: Option<&str>,
    fetch: &Fetch<'_>,
) -> Result<(FleetRoster, OptIn, Option<PoolSyncCtx>), String> {
    let normalized = source_env(env)?;
    let env = &normalized;
    let (chosen, opt_in) = match source.map(str::trim).filter(|s| !s.is_empty()) {
        Some(named) => (named.to_owned(), OptIn::Named),
        None => {
            let has_key = env.config.as_deref().and_then(source_key).is_some();
            let secrets_pool = env
                .home
                .as_ref()
                .map(|h| h.join(".secrets").join(super::ai_pool::AI_POOL_FILE))
                .filter(|p| !has_key && pool_entry_present(p));
            if let Some(pool_path) = secrets_pool {
                (pool_path.to_string_lossy().into_owned(), OptIn::Named)
            } else {
                configured_source(env.config.as_deref(), env.accounts_roster().as_deref())
                    .map(|configured| {
                        let opt_in = match configured {
                            FleetSource::File(_) => OptIn::Named,
                            FleetSource::Key(_) => OptIn::Key,
                        };
                        (configured.source(), opt_in)
                    })
                    .ok_or_else(|| NO_SOURCE.to_owned())?
            }
        }
    };
    let configured = (opt_in == OptIn::Key)
        .then(|| configured_source_env(env))
        .transpose()?;
    let raw = fetch(configured.as_ref().unwrap_or(env), &chosen)?;
    if super::ai_pool::Pool::looks_like_pool(&chosen, &raw) {
        let pool = super::ai_pool::Pool::from_toml(&raw, &chosen)?;
        let identity = super::ai_pool::resolve_local_pool_identity(
            env.home.as_deref(),
            env.config.as_deref(),
            machine_override,
        )
        .ok_or_else(|| "could not determine machine id (`m<N>`); pass `--machine m8`".to_owned())?;
        let fleet = project_pool_fleet(&pool, Cli::Claude, Some(&identity));
        return Ok((fleet, opt_in, Some(PoolSyncCtx { pool, identity })));
    }
    let toml = chosen.ends_with(".toml")
        || raw.trim_start().starts_with('#')
        || raw.contains("[[account]]");
    let fleet = if toml {
        FleetRoster::from_toml(&raw, &chosen)
    } else {
        parse_accounting_csv(&raw, &chosen)
    }?;
    // A discovered fleet stays discovery's through every sync of the local file that holds it.
    let opt_in = if opt_in == OptIn::Named
        && toml
        && matches!(fetch_plan(env, &chosen), Ok(FetchPlan::File(_)))
        && opt_in_of(&raw) == Some(OptIn::Discovered)
    {
        OptIn::Discovered
    } else {
        opt_in
    };
    Ok((fleet, opt_in, None))
}

#[cfg(test)]
fn discover_local_fleet(env: &Env) -> Option<FleetRoster> {
    discover_local_fleet_with_min(env, 2)
}

fn discover_local_fleet_with_min(env: &Env, min_accounts: usize) -> Option<FleetRoster> {
    discovered_fleet(env, &local_shows(env), min_accounts)
}

/// What this machine's own files show of the accounts on it, each address by what shows it
/// ([`local_shows`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Shown {
    /// Claude Code's own: the default login's profile and its copies (`~/.claude.json*`), the
    /// login directories' profiles and renewal checkpoints, Claude Code's backups and its
    /// transcripts' `session_context`.
    claude: BTreeSet<String>,
    /// Sign-in credentials, which name an address and say nothing of a Claude Code account
    /// there: the mailbox configuration's user and the addresses its passwords and TOTP secrets
    /// are kept for (one file every pool's sign-in reads), and Muse Code's providers.
    credentials: BTreeSet<String>,
    /// The rows of this machine's roster.
    roster: BTreeSet<String>,
}

impl Shown {
    /// Every address shown, whatever shows it: what discovery lists.
    fn all(&self) -> BTreeSet<String> {
        self.claude
            .iter()
            .chain(&self.credentials)
            .chain(&self.roster)
            .cloned()
            .collect()
    }

    /// The addresses only a credential shows, none of Claude Code's own files.
    fn credentials_only(&self) -> BTreeSet<String> {
        self.credentials
            .iter()
            .filter(|cred| {
                !self
                    .claude
                    .iter()
                    .any(|em| accounts::email_matches(em, cred))
            })
            .cloned()
            .collect()
    }

    /// Whether every account `fleet` lists is one this machine's own files show (Claude Code's
    /// or a credential's, never the roster that mirrors the fleet), as each of a fleet discovery
    /// wrote is.
    fn shows_all(&self, fleet: &FleetRoster) -> bool {
        fleet.accounts.iter().all(|acct| {
            self.claude
                .iter()
                .chain(&self.credentials)
                .any(|em| accounts::email_matches(em, &acct.email))
        })
    }
}

/// Reads [`Shown`] from `env`'s home, state and config.
fn local_shows(env: &Env) -> Shown {
    let mut shown = Shown::default();
    let record = |into: &mut BTreeSet<String>, candidate: &str| {
        let trimmed = candidate.trim();
        if !trimmed.is_empty() && accounts::email_refusal(trimmed).is_none() {
            into.insert(trimmed.to_owned());
        }
    };
    if let Some(de) = read_default_profile_email(env) {
        record(&mut shown.claude, &de);
    }
    if let Some(home) = &env.home {
        if let Ok(entries) = fs::read_dir(home) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let Some(name_str) = name.to_str() else {
                    continue;
                };
                if name_str.starts_with(".claude.json")
                    && entry.file_type().is_ok_and(|ft| ft.is_file())
                    && let Ok(text) = read_local_source_text(&entry.path())
                    && let Ok(v) = aterm_json::from_str::<Value>(&text)
                    && let Some(em) = v
                        .get("oauthAccount")
                        .and_then(|o| o.get("emailAddress"))
                        .and_then(Value::as_str)
                {
                    record(&mut shown.claude, em);
                }
            }
        }
        let muse_auth = home.join(".config/muse/auth.json");
        if let Ok(text) = read_local_source_text(&muse_auth)
            && let Ok(v) = aterm_json::from_str::<Value>(&text)
            && let Some(providers) = v.get("providers").and_then(Value::as_object)
        {
            for prov in providers.values() {
                if let Some(em) = prov.get("user_email").and_then(Value::as_str) {
                    record(&mut shown.credentials, em);
                }
            }
        }
    }
    if let Some(roster_path) = env.accounts_roster()
        && let Ok(roster) = Roster::load(&roster_path)
    {
        for row in &roster.rows {
            if let Some(em) = row.email.as_deref() {
                record(&mut shown.roster, em);
            }
        }
    }
    if let Some(mb) = super::headless_oauth::discover_mailbox_config_files(
        env.home.as_deref(),
        Some(&accounts::Store::at(&env.state).dir),
        env.config.as_deref(),
    ) {
        if let Some(u) = mb.user.as_deref().filter(|u| u.contains('@')) {
            record(&mut shown.credentials, u);
        }
        for key in mb
            .passwords
            .keys()
            .chain(mb.mailbox_passwords.keys())
            .chain(mb.totp_secrets.keys())
        {
            record(&mut shown.credentials, key);
        }
    }
    if let Ok(entries) = fs::read_dir(accounts::Store::at(&env.state).logins_dir()) {
        for entry in entries.flatten() {
            if !entry.file_type().is_ok_and(|ft| ft.is_dir()) {
                continue;
            }
            let dir_path = entry.path();
            if let Ok(text) = read_local_source_text(&dir_path.join(".claude.json"))
                && let Ok(v) = aterm_json::from_str::<Value>(&text)
                && let Some(em) = v
                    .get("oauthAccount")
                    .and_then(|o| o.get("emailAddress"))
                    .and_then(Value::as_str)
            {
                record(&mut shown.claude, em);
            }
            if let Ok(text) =
                read_local_source_text(&dir_path.join(accounts::RENEWAL_CHECKPOINT_FILE))
                && let Ok(v) = aterm_json::from_str::<Value>(&text)
                && let Some(em) = v.get("email").and_then(Value::as_str)
            {
                record(&mut shown.claude, em);
            }
        }
    }
    if let Some(claude_dir) = env.claude_dir() {
        if let Ok(backups) = fs::read_dir(claude_dir.join("backups")) {
            for entry in backups.flatten() {
                if entry.file_type().is_ok_and(|ft| ft.is_file())
                    && let Ok(text) = read_local_source_text(&entry.path())
                    && let Ok(v) = aterm_json::from_str::<Value>(&text)
                    && let Some(em) = v
                        .get("oauthAccount")
                        .and_then(|o| o.get("emailAddress"))
                        .and_then(Value::as_str)
                {
                    record(&mut shown.claude, em);
                }
            }
        }
        if let Ok(entries) = fs::read_dir(claude_dir.join("projects")) {
            let mut files_with_mtime: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
            for proj in entries.flatten() {
                if !proj.file_type().is_ok_and(|ft| ft.is_dir()) {
                    continue;
                }
                let Ok(files) = fs::read_dir(proj.path()) else {
                    continue;
                };
                for file in files.flatten() {
                    let path = file.path();
                    if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                        continue;
                    }
                    let mtime = file
                        .metadata()
                        .and_then(|m| m.modified())
                        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                    files_with_mtime.push((mtime, path));
                }
            }
            files_with_mtime.sort_by_key(|a| std::cmp::Reverse(a.0));
            files_with_mtime.truncate(TRANSCRIPT_FILES);
            let paths: Vec<PathBuf> = files_with_mtime.into_iter().map(|(_, p)| p).collect();
            for email in transcript_emails(&paths, &TRANSCRIPT_SCANS, TRANSCRIPT_SCAN_BYTES) {
                record(&mut shown.claude, &email);
            }
        }
    }
    shown
}

/// [`discovered_fleet`]'s machine past a config: `host`, this Mac's id, where one of `emails`
/// names no machine or names it (or none of them, nor the default login's `default_email`, hints
/// at one), else the machine the default login's email hints at, else the first one `emails`
/// hint at.
fn discovery_machine(
    host: Option<String>,
    emails: &BTreeSet<String>,
    default_email: impl Fn() -> Option<String>,
) -> Option<String> {
    let email_hint_machine = || {
        default_email()
            .as_deref()
            .and_then(|de| machines_from_email_hint(de).into_iter().next())
            .or_else(|| {
                emails
                    .iter()
                    .find_map(|em| machines_from_email_hint(em).into_iter().next())
            })
    };
    host.filter(|host_m| {
        emails.iter().any(|em| {
            let (machines, _) = email_machines_and_slot(em);
            machines.is_empty() || machines.contains(host_m)
        }) || email_hint_machine().is_none()
    })
    .or_else(email_hint_machine)
}

/// The fleet discovery makes of what `shown` holds ([`Shown::all`]) for `env`'s machine, where
/// it lists at least `min_accounts`.
fn discovered_fleet(env: &Env, shown: &Shown, min_accounts: usize) -> Option<FleetRoster> {
    let emails = shown.all();
    if emails.is_empty() {
        return None;
    }
    let fallback_machine = config_machine_id(env.config.as_deref()).or_else(|| {
        discovery_machine(detect_machine_id(None, None), &emails, || {
            read_default_profile_email(env)
        })
    })?;
    let mut rows_with_slot: Vec<(Option<u32>, Vec<String>, String)> = Vec::new();
    for email in emails {
        let (mut machines, slot) = email_machines_and_slot(&email);
        if machines.is_empty() {
            machines.push(fallback_machine.clone());
        }
        if !machines.contains(&fallback_machine) {
            continue;
        }
        rows_with_slot.push((slot, machines, email));
    }
    rows_with_slot.sort_by_key(|(slot, _, email)| (slot.unwrap_or(1), email.clone()));
    let mut used_labels = BTreeSet::new();
    let mut accounts = Vec::new();
    for (slot, machines, email) in rows_with_slot {
        let base_label = discovered_label(&machines, slot);
        let mut label = base_label.clone();
        let mut suffix = 2u32;
        while used_labels.contains(&label) {
            label = format!("{base_label}-{suffix}");
            suffix = suffix.saturating_add(1);
        }
        if accounts::label_refusal(&label).is_some() {
            continue;
        }
        used_labels.insert(label.clone());
        let class = if machines.len() > 1 {
            Class::Shared
        } else {
            Class::Dedicated
        };
        accounts.push(FleetAccount {
            label,
            machines,
            class,
            email,
            browser: None,
            vendor: accounts::Vendor::default(),
        });
    }
    (accounts.len() >= min_accounts).then_some(FleetRoster {
        source: "local-claude-transcripts".to_owned(),
        accounts,
    })
}

/// How many transcripts discovery reads, the newest first.
const TRANSCRIPT_FILES: usize = 32;

/// The most transcript bytes one discovery reads that no earlier one in this process read
/// ([`transcript_emails`]): a transcript is read from where the last read stopped, so a long one
/// is finished over several discoveries.
const TRANSCRIPT_SCAN_BYTES: u64 = 256 * 1024 * 1024;

/// The longest transcript line discovery reads: a longer one (a tool's output, a file it read)
/// is passed over unkept, since a `session_context` line is a few KiB.
const TRANSCRIPT_LINE_MAX: usize = 256 * 1024;

/// How many transcripts [`TRANSCRIPT_SCANS`] remembers before it keeps only the last read's.
const TRANSCRIPT_SCANS_MAX: usize = 4 * TRANSCRIPT_FILES;

/// What discovery has read of one transcript in this process ([`transcript_emails`]).
struct TranscriptScan {
    /// The file it read (device and inode where the platform names them).
    id: Option<(u64, u64)>,
    /// The file's length when it was read.
    len: u64,
    /// The file's modification time when it was read.
    modified: Option<std::time::SystemTime>,
    /// Where the next read starts: just past the last line read whole.
    offset: u64,
    /// The `session_context` emails read so far.
    emails: BTreeSet<String>,
}

/// Discovery's reads of transcripts, by path, in this process only (never written anywhere).
type TranscriptScans = std::sync::Mutex<BTreeMap<PathBuf, TranscriptScan>>;

static TRANSCRIPT_SCANS: TranscriptScans = std::sync::Mutex::new(BTreeMap::new());

/// The email of a transcript `line` that is Claude Code's `session_context` attachment
/// (`attachment.context.userEmail`), its first word holding an `@`.
fn session_context_email(line: &str) -> Option<String> {
    if !line.contains("\"session_context\"") || !line.contains("\"userEmail\"") {
        return None;
    }
    let v: Value = aterm_json::from_str(line).ok()?;
    let attachment = v.get("attachment")?;
    if attachment.get("type").and_then(Value::as_str) != Some("session_context") {
        return None;
    }
    let user_email = attachment.get("context")?.get("userEmail")?.as_str()?;
    user_email
        .split([' ', '\t', '\n', '\r', '<', '>', '"', '\''])
        .map(|s| s.trim_end_matches(['.', ',', ';', ':']))
        .find(|s| s.contains('@'))
        .map(str::to_owned)
}

/// Calls `each` with every line of `reader` no longer than `line_max` bytes (without its
/// newline), passing longer ones over without keeping more than `line_max` bytes of any; a last
/// line with no newline is handed over too. Returns how many bytes were read up to and through
/// the last newline.
fn each_bounded_line(
    reader: &mut impl BufRead,
    line_max: usize,
    mut each: impl FnMut(&[u8]),
) -> io::Result<u64> {
    let mut line = Vec::new();
    let mut overlong = false;
    let mut read = 0u64;
    let mut through_newline = 0u64;
    loop {
        let buf = reader.fill_buf()?;
        if buf.is_empty() {
            break;
        }
        let (take, ends) = match buf.iter().position(|&b| b == b'\n') {
            Some(at) => (at, true),
            None => (buf.len(), false),
        };
        if !overlong {
            if line.len() + take <= line_max {
                line.extend_from_slice(&buf[..take]);
            } else {
                overlong = true;
                line.clear();
            }
        }
        let used = take + usize::from(ends);
        reader.consume(used);
        read += used as u64;
        if ends {
            if !overlong {
                each(&line);
            }
            line.clear();
            overlong = false;
            through_newline = read;
        }
    }
    if !overlong && !line.is_empty() {
        each(&line);
    }
    Ok(through_newline)
}

/// The `session_context` emails ([`session_context_email`]) of the transcripts at `paths`, each
/// read line by line ([`TRANSCRIPT_LINE_MAX`]) whatever its size, from where `scans` says the
/// last read of it stopped: a file that only grew is read from there, and one that shrank or
/// changed in place or was replaced is read again from its start. At most `budget` bytes are read
/// in all; a file the budget cut short is read on from there next time. A link or a file that is
/// not a regular one is not read.
fn transcript_emails(paths: &[PathBuf], scans: &TranscriptScans, budget: u64) -> BTreeSet<String> {
    let mut scans = scans
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut left = budget;
    let mut emails = BTreeSet::new();
    for path in paths {
        let Ok(file) = astream_sys::open_nonblocking(path, astream_sys::Links::Refuse) else {
            continue;
        };
        let Ok(meta) = file.metadata() else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        #[cfg(unix)]
        let id = {
            use std::os::unix::fs::MetadataExt as _;
            Some((meta.dev(), meta.ino()))
        };
        #[cfg(not(unix))]
        let id = None;
        let (len, modified) = (meta.len(), meta.modified().ok());
        let mut scan = match scans.remove(path) {
            Some(held) if held.id == id && held.len == len && held.modified == modified => held,
            Some(held) if held.id == id && len > held.len => held,
            _ => TranscriptScan {
                id,
                len,
                modified,
                offset: 0,
                emails: BTreeSet::new(),
            },
        };
        let want = len.saturating_sub(scan.offset).min(left);
        if want > 0 {
            let mut file = file;
            if file.seek(SeekFrom::Start(scan.offset)).is_ok() {
                let mut reader = io::BufReader::with_capacity(64 * 1024, file.take(want));
                let found = &mut scan.emails;
                if let Ok(through) = each_bounded_line(&mut reader, TRANSCRIPT_LINE_MAX, |line| {
                    if let Some(email) = std::str::from_utf8(line)
                        .ok()
                        .and_then(session_context_email)
                    {
                        found.insert(email);
                    }
                }) {
                    scan.offset = scan.offset.saturating_add(through);
                }
                left -= want;
            }
        }
        scan.len = len;
        scan.modified = modified;
        emails.extend(scan.emails.iter().cloned());
        scans.insert(path.clone(), scan);
    }
    if scans.len() > TRANSCRIPT_SCANS_MAX {
        scans.retain(|path, _| paths.contains(path));
    }
    emails
}

const SOURCE_TEXT_MAX_BYTES: usize = 4 * 1024 * 1024;

fn decode_source_text(bytes: Vec<u8>, source: &str) -> Result<String, String> {
    decode_source_text_bounded(bytes, source, SOURCE_TEXT_MAX_BYTES)
}

fn decode_source_text_bounded(
    bytes: Vec<u8>,
    source: &str,
    max_bytes: usize,
) -> Result<String, String> {
    if bytes.len() > max_bytes {
        return Err(format!(
            "account source at {source} exceeds the {max_bytes}-byte limit"
        ));
    }
    String::from_utf8(bytes)
        .map_err(|e| format!("account source at {source} is not valid UTF-8: {e}"))
}

fn read_local_source_text(path: &Path) -> io::Result<String> {
    read_local_source_text_with(path, astream_sys::Links::Refuse)
}

fn read_local_source_text_with(path: &Path, links: astream_sys::Links) -> io::Result<String> {
    read_local_text_bounded(path, links, SOURCE_TEXT_MAX_BYTES)
}

fn read_local_text_bounded(
    path: &Path,
    links: astream_sys::Links,
    max_bytes: usize,
) -> io::Result<String> {
    let file = astream_sys::open_nonblocking(path, links)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "local account source is not a regular file",
        ));
    }
    if metadata.len() > max_bytes as u64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "local account source exceeds the byte limit",
        ));
    }
    let mut bytes = Vec::new();
    file.take((max_bytes as u64).saturating_add(1))
        .read_to_end(&mut bytes)?;
    decode_source_text_bounded(bytes, &path.to_string_lossy(), max_bytes)
        .map_err(|why| io::Error::new(io::ErrorKind::InvalidData, why))
}

// Source selection may skip an absent entry, never an unreadable one. When an
// ancestor is missing, distinguish a normal absent directory from a dangling
// link or a non-directory obstruction. This probe grants no read/write authority;
// the bounded reader and publication snapshots still judge what they open.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSourcePresence",
        action = "Primary",
        project = "aterm_agent::harness::accounts_sync::cli_pool_tests::source_presence::source_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSourcePresence",
        action = "Fallback",
        project = "aterm_agent::harness::accounts_sync::cli_pool_tests::source_presence::source_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSourcePresence",
        action = "PrimaryUnavailable",
        project = "aterm_agent::harness::accounts_sync::cli_pool_tests::source_presence::source_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSourcePresence",
        action = "FallbackUnavailable",
        project = "aterm_agent::harness::accounts_sync::cli_pool_tests::source_presence::source_transition"
    )
)]
fn source_entry_present(path: &Path) -> bool {
    let mut at = path;
    loop {
        match fs::symlink_metadata(at) {
            Ok(_) if at == path => return true,
            Ok(_) => return fs::metadata(at).map_or(true, |meta| !meta.is_dir()),
            Err(why) if why.kind() == io::ErrorKind::NotFound => {
                let Some(parent) = at.parent() else {
                    return false;
                };
                at = parent;
            }
            Err(_) => return true,
        }
    }
}

fn pool_entry_present(path: &Path) -> bool {
    let Some(parent) = path.parent() else {
        return false;
    };
    if !fs::metadata(parent).is_ok_and(|meta| meta.is_dir()) {
        return false;
    }
    match fs::symlink_metadata(path) {
        Ok(_) => true,
        Err(why) => why.kind() != io::ErrorKind::NotFound,
    }
}

fn has_fleet_source(dir: &Path) -> bool {
    pool_entry_present(&dir.join(super::ai_pool::AI_POOL_FILE))
        || [FLEET_ROSTER_FILE, ACCOUNTS_CSV_FILE]
            .into_iter()
            .any(|name| source_entry_present(&dir.join(name)))
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum FetchPlan {
    Http(String),
    File(PathBuf),
}

/// Resolve from captured context only. A drive-relative Windows path must not consult
/// the process's per-drive current directory, and an unknown base grants no fallback.
fn source_path(base: &Path, path: &Path) -> Result<PathBuf, String> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    if path
        .components()
        .any(|part| matches!(part, std::path::Component::Prefix(_)))
    {
        return Err(format!(
            "account source path {} is drive-relative; name an absolute path",
            path.display()
        ));
    }
    if !base.is_absolute() {
        return Err("a relative account source needs a captured absolute directory".to_owned());
    }
    let resolved = base.join(path);
    if !resolved.is_absolute() {
        return Err(format!(
            "account source path {} has no captured root",
            path.display()
        ));
    }
    Ok(resolved)
}

/// Resolve a relative config path against the same captured directory used by source reads.
/// The caller uses this environment for both source selection and roster publication.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSourcePath",
        action = "NoConfigFile",
        project = "aterm_agent::harness::accounts_sync::source_path_tests::path_transition"
    )
)]
fn source_env(env: &Env) -> Result<Env, String> {
    let mut resolved = env.clone();
    resolved.config = env
        .config
        .as_deref()
        .map(|path| {
            if path.file_name().is_none() {
                Err("an account sync config path must name a file".to_owned())
            } else {
                source_path(&env.cwd, path)
            }
        })
        .transpose()?;
    Ok(resolved)
}

fn configured_source_env(env: &Env) -> Result<Env, String> {
    let mut resolved = source_env(env)?;
    resolved.cwd = resolved
        .config
        .as_deref()
        .and_then(Path::parent)
        .ok_or("a configured account source needs a config directory")?
        .to_path_buf();
    Ok(resolved)
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSourcePath",
        action = "Explicit",
        project = "aterm_agent::harness::accounts_sync::source_path_tests::path_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSourcePath",
        action = "Configured",
        project = "aterm_agent::harness::accounts_sync::source_path_tests::path_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSourcePath",
        action = "Absolute",
        project = "aterm_agent::harness::accounts_sync::source_path_tests::path_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSourcePath",
        action = "Home",
        project = "aterm_agent::harness::accounts_sync::source_path_tests::path_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSourcePath",
        action = "NoBase",
        project = "aterm_agent::harness::accounts_sync::source_path_tests::path_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSourcePath",
        action = "NoHome",
        project = "aterm_agent::harness::accounts_sync::source_path_tests::path_transition"
    )
)]
fn fetch_plan(env: &Env, source: &str) -> Result<FetchPlan, String> {
    if source.starts_with(RCLONE_PREFIX) {
        return Err(format!(
            "{source}: rclone remotes are not read; name the sheet's CSV export URL \
             (https://docs.google.com/spreadsheets/d/<id>/export?format=csv), a LAN \
             {FLEET_ROSTER_FILE} URL or path, or a local CSV file"
        ));
    }
    if source.starts_with("http://") || source.starts_with("https://") {
        return Ok(FetchPlan::Http(normalize_google_sheet_url(source)));
    }
    let path = if let Some(rest) = source.strip_prefix("~/") {
        let home = env
            .home
            .as_deref()
            .filter(|home| home.is_absolute())
            .ok_or("a home-relative account source needs a captured absolute home")?;
        let relative = Path::new(rest.trim_start_matches(std::path::is_separator));
        if relative
            .components()
            .any(|part| matches!(part, std::path::Component::Prefix(_)))
        {
            return Err("a home-relative account source cannot name a drive prefix".to_owned());
        }
        source_path(home, relative)?
    } else {
        source_path(&env.cwd, Path::new(source))?
    };
    Ok(FetchPlan::File(path))
}

type RunFetcher =
    dyn Fn(aterm_uds::spawnfd::Command) -> Result<super::align::Captured, String> + Sync;

fn run_fetcher(cmd: aterm_uds::spawnfd::Command) -> Result<super::align::Captured, String> {
    super::align::capture_bounded(cmd, Duration::from_secs(20), SOURCE_TEXT_MAX_BYTES + 1)
}

fn fetch_http(url: &str, run: &RunFetcher) -> Result<String, String> {
    let mut cmd = aterm_uds::spawnfd::command("/usr/bin/curl");
    cmd.args([
        "-q",
        "-fsSL",
        "--max-time",
        "15",
        "--proto",
        "=http,https",
        "--proto-redir",
        "=http,https",
        "--",
        url,
    ]);
    let cap = run(cmd).map_err(|e| format!("failed to run curl for {url}: {e}"))?;
    if cap.code != Some(0) {
        let out = String::from_utf8_lossy(&cap.stdout);
        return Err(format!("curl failed for {url}: {}", out.trim()));
    }
    decode_source_text(cap.stdout, url)
}

fn fetch_source_text(env: &Env, source: &str) -> Result<String, String> {
    fetch_source_text_with(env, source, &run_fetcher)
}

fn fetch_source_text_with(env: &Env, source: &str, run: &RunFetcher) -> Result<String, String> {
    match fetch_plan(env, source)? {
        FetchPlan::Http(url) => fetch_http(&url, run),
        // An explicit source keeps ordinary path semantics, including final symlinks.
        // Refuse non-regular descriptors before reading; on Unix the nonblocking open also
        // prevents a FIFO from waiting for a writer.
        FetchPlan::File(path) => read_local_source_text_with(&path, astream_sys::Links::Follow)
            .map_err(|e| format!("could not read {}: {e}", path.display())),
    }
}

/// Convert a Google Sheets `/edit` URL into its `/export?format=csv` URL.
#[must_use]
pub fn normalize_google_sheet_url(url: &str) -> String {
    if let Some((prefix, _)) = url.split_once("/edit")
        && prefix.contains("docs.google.com/spreadsheets/d/")
    {
        return format!("{prefix}/export?format=csv&gid=0");
    }
    url.to_owned()
}

struct SyncedAccounts {
    fleet_path: PathBuf,
    note: Option<String>,
    /// Whether the sync changed anything: a file whose bytes differ from what it held (each is
    /// written only then), account state or logins, or a ledger row.
    changed: bool,
}

struct FleetSnapshot {
    path: PathBuf,
    bytes: Option<Vec<u8>>,
    // Source merges, initial seeds and removals publish under the same lock as admission.
    publish: Publish,
}

/// What a sync does with a fleet file it captured ([`FleetSnapshot`]), under the accounts lock
/// once the file still holds the captured bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Publish {
    /// Nothing: the sync only refuses where the file changed since it was captured.
    No,
    /// Writes the fleet the sync publishes where it differs: a merge into its source, or aterm's
    /// own copy in `~/.secrets` of a list found on this Mac, where none was ([`own_secrets_copy`]).
    Fleet,
    /// Removes aterm's own copy in `~/.secrets`, which the sheet beside it outranks.
    Remove,
}

fn sync_lock_ladder() -> Vec<Duration> {
    accounts::LOCK_LADDER
        .iter()
        .copied()
        .chain(std::iter::repeat_n(Duration::from_millis(500), 16))
        .collect()
}

fn default_relocation(
    existing: &Roster,
    next: &Roster,
) -> Result<Option<(String, String)>, String> {
    let Some((was, now)) = existing.row_for_login(None).zip(next.row_for_login(None)) else {
        return Ok(None);
    };
    if was.label == now.label
        || !was
            .email
            .as_deref()
            .zip(now.email.as_deref())
            .is_some_and(|(was, now)| accounts::email_matches(was, now))
    {
        return Ok(None);
    }
    if existing.row(&now.label).is_some() {
        return Err(format!(
            "cannot relabel `{}` to `{}`: the previous roster already uses that label; choose an unused label; no new sync was published",
            was.label, now.label
        ));
    }
    Ok(Some((was.label.clone(), now.label.clone())))
}

fn apply_pool_to_claude_roster(
    mut roster: Roster,
    existing: &Roster,
    store: &accounts::Store,
    ctx: &PoolSyncCtx,
) -> Roster {
    for prev in &existing.rows {
        let already_in = match prev.expected_identity_email().or(prev.email.as_deref()) {
            Some(prev_email) => roster.rows.iter().any(|r| {
                r.expected_identity_email()
                    .or(r.email.as_deref())
                    .is_some_and(|e| accounts::email_matches(e, prev_email))
            }),
            None => roster.rows.iter().any(|r| r.label == prev.label),
        };
        if already_in || roster.rows.len() >= accounts::MAX_ACCOUNTS {
            continue;
        }
        let mut label = prev.label.clone();
        let mut suffix = 2u32;
        while roster.rows.iter().any(|r| r.label == label) {
            label = format!("{}-{suffix}", prev.label);
            suffix = suffix.saturating_add(1);
        }
        let has_default = roster.rows.iter().any(|r| r.dir.is_none());
        let dir = if prev.dir.is_none() && has_default {
            store
                .login_dir(&label)
                .map(|p| p.to_string_lossy().into_owned())
        } else {
            prev.dir.clone()
        };
        roster.rows.push(Row {
            label,
            dir,
            ..prev.clone()
        });
    }
    for row in &mut roster.rows {
        if row.email.is_some() || row.identity_email.is_some() {
            row.held_by = ctx.holder_for_row(Cli::Claude, row);
        }
    }
    roster
        .rows
        .sort_by_key(|r| (r.dir.is_some(), r.class, r.label.clone()));
    roster
}

#[cfg(test)]
fn persist_accounts_sync(
    store: &accounts::Store,
    roster_path: &Path,
    (fleet, opt_in): (&FleetRoster, OptIn),
    (machine, default_email): (&str, Option<&str>),
    standing_walls: &BTreeMap<String, upgrade::StandingWall>,
    now: i64,
    (ladder, fleet_snapshots): (&[Duration], &[FleetSnapshot]),
) -> Result<SyncedAccounts, String> {
    persist_accounts_sync_with_pool(
        store,
        roster_path,
        (fleet, opt_in, None),
        (machine, default_email),
        standing_walls,
        now,
        (ladder, fleet_snapshots),
    )
}

fn persist_accounts_sync_with_pool(
    store: &accounts::Store,
    roster_path: &Path,
    (fleet, opt_in, pool_ctx): (&FleetRoster, OptIn, Option<&PoolSyncCtx>),
    (machine, default_email): (&str, Option<&str>),
    standing_walls: &BTreeMap<String, upgrade::StandingWall>,
    now: i64,
    (ladder, fleet_snapshots): (&[Duration], &[FleetSnapshot]),
) -> Result<SyncedAccounts, String> {
    let held = store
        .acquire_within(ladder)
        .map_err(|why| format!("{}: {why}", store.dir.display()))?
        .ok_or_else(|| {
            format!(
                "{} at {} after {} ms: another `aterm harness accounts` or a window's \
                 supervisor holds it; nothing was synced, so run the sync again",
                accounts::LOCK_HELD,
                store.dir.display(),
                ladder.iter().sum::<Duration>().as_millis()
            )
        })?;
    let (write_to, keep_mode) = super::cli::roster_target(roster_path)?;
    let recovered = store.recover_sync_held(&held, &write_to)?;
    let fleet_path = roster_path
        .parent()
        .map(|p| p.join(FLEET_ROSTER_FILE))
        .unwrap_or_else(|| PathBuf::from(FLEET_ROSTER_FILE));
    for expected in fleet_snapshots {
        let current = match read_local_source_text(&expected.path) {
            Ok(text) => Some(text.into_bytes()),
            Err(why) if why.kind() == io::ErrorKind::NotFound => None,
            Err(why) => {
                return Err(format!(
                    "could not check captured fleet at {}: {why}; nothing was synced",
                    expected.path.display()
                ));
            }
        };
        if current != expected.bytes {
            return Err(
                "fleet accounts changed after init discovery; nothing was synced".to_owned(),
            );
        }
    }
    let existing = Roster::load(&write_to).map_err(|why| {
        format!(
            "the roster at {} is refused: {why}; the roster is unchanged",
            roster_path.display()
        )
    })?;
    let mut roster = build_machine_roster(fleet, machine, store, Some(&existing), default_email);
    if let Some(ctx) = pool_ctx {
        roster = apply_pool_to_claude_roster(roster, &existing, store, ctx);
    }
    if let Some(expected) = default_identity_hint(Some(&existing), default_email)
        && !roster
            .row_for_login(None)
            .and_then(Row::expected_identity_email)
            .is_some_and(|actual| accounts::email_matches(actual, expected))
    {
        return Err("the generated roster cannot represent the known default login; retain that account or free a row; no new sync was published".into());
    }
    if roster.rows.is_empty() {
        return Err(format!(
            "no active Claude accounts found for machine `{machine}` in {}",
            fleet.source
        ));
    }
    let roster_toml = format_roster_toml(&roster)
        .map_err(|why| format!("generated roster refused admission: {why}"))?;
    let (fleet_write_to, fleet_mode) = super::cli::roster_target(&fleet_path)?;
    // Source publication must not replace the account authority whose before images
    // the local commit is about to retain, even through an existing path alias.
    let destination = |path: &Path| -> Result<PathBuf, String> {
        fs::canonicalize(path)
            .or_else(|_| {
                path.parent()
                    .zip(path.file_name())
                    .ok_or_else(|| io::Error::other("destination has no parent or name"))
                    .and_then(|(parent, name)| {
                        fs::canonicalize(parent).map(|parent| parent.join(name))
                    })
            })
            .or_else(|_| std::path::absolute(path))
            .map_err(|why| why.to_string())
    };
    let authority_dir = destination(&store.dir)?;
    let authority_roster = destination(&write_to)?;
    for path in std::iter::once(fleet_write_to.as_path()).chain(
        fleet_snapshots
            .iter()
            .filter(|snapshot| snapshot.publish != Publish::No)
            .map(|snapshot| snapshot.path.as_path()),
    ) {
        let target = destination(path)?;
        if target.starts_with(&authority_dir) || target == authority_roster {
            return Err(format!(
                "sync auxiliary destination {} overlaps the account store or roster; no new sync was published",
                path.display()
            ));
        }
    }
    let fleet_toml = fleet.to_toml_for(Some(opt_in));
    let default_row_label = roster.row_for_login(None).map(|r| r.label.clone());
    // The labels the fleet gave another account than the one the roster gave them: what is on
    // record under each is the account it held before, never the one it holds now.
    let relabelled: Vec<String> = roster
        .rows
        .iter()
        .filter(|row| {
            existing.row(&row.label).is_some_and(|was| {
                match (was.email.as_deref(), row.email.as_deref()) {
                    (Some(was), Some(now)) => !accounts::email_matches(was, now),
                    (None, Some(_)) => true,
                    _ => false,
                }
            })
        })
        .map(|row| row.label.clone())
        .collect();
    // A default login retains its state when its label moves, whether or not the old
    // label is reused. Only an unchanged login address permits this metadata relocation.
    let default_moved = default_relocation(&existing, &roster)?;
    let legacy_assignments: Vec<&str> = roster
        .rows
        .iter()
        .filter(|row| {
            row.email.is_some()
                && existing
                    .row(&row.label)
                    .is_some_and(|was| was.email.is_none())
        })
        .map(|row| row.label.as_str())
        .collect();
    if !legacy_assignments.is_empty() {
        store
            .check_new_labels(&legacy_assignments, now)
            .map_err(|why| format!("cannot assign an identity to a retained legacy row: {why}"))?;
    }
    // The row the default login held before: where the profile now names another row's account,
    // the vendor's own sign-in wrote over the one login that row had here. A label the fleet gave
    // another account was written over by no sign-in.
    let displaced = existing
        .row_for_login(None)
        .map(|r| r.label.clone())
        .filter(|was| {
            default_email.is_some()
                && default_row_label.as_deref().is_some_and(|now| now != was)
                && roster.row(was).is_some_and(|r| r.dir.is_some())
                && !relabelled.contains(was)
        });
    let default_matched = default_email.is_some_and(|de| {
        roster
            .row_for_login(None)
            .and_then(Row::expected_identity_email)
            .is_some_and(|re| accounts::email_matches(re, de))
    });
    // The row the default login held before, where the default login holds another account now
    // (that row's known address is not the default login's) and names another row: the vendor's
    // own sign-in just wrote the default login, so the row it names now holds a login made now,
    // whatever its earlier login left on record (accounts::note_default_login_made). A relabel of
    // the same account, or a row whose address is unknown, is no such move.
    let repointed_from = existing
        .row_for_login(None)
        .filter(|was| {
            default_row_label
                .as_deref()
                .is_some_and(|now| now != was.label)
                && was
                    .expected_identity_email()
                    .zip(default_email)
                    .is_some_and(|(held, now)| !accounts::email_matches(held, now))
        })
        .map(|r| r.label.clone());
    let mut note = None;
    let mut publishing = false;
    // A sync that finds what is already there writes nothing: each file is replaced only where
    // its bytes differ, so a periodic caller rewrites no file and wakes no watcher.
    let roster_held = accounts::roster_text(&write_to)?;
    let mut changed = recovered || roster_held.as_deref() != Some(roster_toml.as_str());
    let mut publish_if_changed = |path: &Path, held: Option<&[u8]>, text: &str, mode| {
        if held == Some(text.as_bytes()) {
            return Ok(());
        }
        changed = true;
        super::upgrade_models::write_atomic_mode(path, text, mode)
            .map_err(|why| format!("could not write {}: {why}", path.display()))
    };
    let fleet_held = fs::read(&fleet_write_to).ok();
    let mut state_changed = false;
    let mut copy_removed = false;
    let events = store
        .transact_sync_held(&held, now, &mut note, accounts::SyncTarget {
            path: &write_to,
            before: roster_held.as_deref(),
            after: Some(&roster_toml),
            mode: keep_mode,
        }, |state, logins| {
            let before = (state.clone(), logins.clone());
            if let Some((from, to)) = &default_moved {
                accounts::move_account(state, logins, from, to)?;
            }
            for label in &relabelled {
                if default_moved.as_ref().is_none_or(|(from, _)| from != label) {
                    accounts::forget_account(state, logins, label);
                }
            }
            for row in &roster.rows {
                if let Some(dir) = row.dir.as_deref()
                    && let Some(owner) = accounts::retained_directory_owner(logins, &roster.rows, &row.label, dir)?
                {
                    return Err(format!("cannot use `{}`: its login directory is retained by `{owner}`; choose an unused label; nothing was written", row.label));
                }
            }
            let mut events = Vec::new();
            if let Some(label) = &default_row_label {
                let unseen_default = state
                    .accounts
                    .get(label)
                    .is_none_or(|a| a.auth == accounts::Auth::Unseen)
                    && logins.get(label).is_none_or(|b| {
                        b.served_at.is_none() && b.renewed_at.is_none() && b.ended_at.is_none()
                    });
                // The default login newly names this row, which no renewal bound elsewhere: its
                // login was just made, so a sign-out, an end or a failed sign-in its earlier
                // login left is no news of it, and the sessions on the default login are its.
                // A row the default login merely still names keeps what is on record (a login
                // seen ended after it was matched stays signed out), and so does one whose
                // sign-out or end is on record at or after this sync's own instant: the login was
                // made before the sync saw it, so that word came after it and is its news.
                let ended_since = state
                    .accounts
                    .get(label)
                    .filter(|a| a.auth == accounts::Auth::SignedOut)
                    .and_then(|a| a.auth_at)
                    .into_iter()
                    .chain(logins.get(label).and_then(|b| b.ended_at))
                    .any(|at| at >= now);
                // A row bound to a renewal directory of its own launches from that directory:
                // the vendor's sign-in made a second login of its account on the default login
                // and left the row's own as it was, so what is on record of it stands, while the
                // sessions that ran on the default login (and `active`, where it named the row
                // the default login held) are its account's now.
                let bound_elsewhere = logins.get(label).is_some_and(|b| b.dir.is_some());
                let made_now = default_matched
                    && repointed_from.is_some()
                    && !ended_since
                    && !bound_elsewhere;
                let was = repointed_from
                    .as_deref()
                    .filter(|was| logins.get(was).is_none_or(|b| b.dir.is_none()));
                if made_now {
                    events.extend(accounts::note_default_login_made(
                        state, logins, label, was, now,
                    ));
                } else if default_matched && unseen_default {
                    accounts::note_served_login(logins, label, now);
                    events.extend(accounts::note_auth(state, label, accounts::Auth::Ok, now));
                }
                if default_matched
                    && bound_elsewhere
                    && let Some(was) = was
                {
                    events.extend(accounts::move_default_sessions(state, label, was, now));
                }
                if let Some(own_acct) = state.accounts.get(ACCOUNTS_OWN_LABEL).cloned() {
                    for (win, wall) in own_acct.walls.into_iter().filter(|(_, wall)| !wall.cleared)
                    {
                        events.extend(accounts::note_wall(
                            state, label, &win, wall.until, wall.at, now,
                        ));
                    }
                }
                // A row bound to a login of its own kept it: only the default login was replaced.
                if let Some(was) = &displaced
                    && logins.get(was).is_none_or(|b| b.dir.is_none())
                {
                    events.extend(accounts::note_displaced(state, logins, was, label, now));
                }
            }
            // No row whose login directory holds no login is on record as a working login (a
            // default login written over before its row was told, a directory emptied by hand).
            for row in &roster.rows {
                let dir = logins
                    .get(&row.label)
                    .and_then(|b| b.dir.clone())
                    .or_else(|| row.dir.clone());
                if dir.is_some_and(|dir| !accounts::login_made(Path::new(&dir)))
                    && state
                        .accounts
                        .get(&row.label)
                        .is_some_and(|a| a.auth == accounts::Auth::Ok)
                {
                    events.extend(accounts::note_auth(
                        state,
                        &row.label,
                        accounts::Auth::SignedOut,
                        now,
                    ));
                }
            }
            for (wall_label, wall) in standing_walls {
                if roster.row(wall_label).is_none() {
                    continue;
                }
                let win = wall
                    .window
                    .as_deref()
                    .filter(|w| !w.trim().is_empty())
                    .unwrap_or(accounts::UNNAMED_WINDOW);
                let until = wall.until.and_then(|u| i64::try_from(u).ok());
                let at = i64::try_from(wall.at).unwrap_or(now).min(now);
                events.extend(accounts::note_wall(state, wall_label, win, until, at, now));
            }
            if let Some(ctx) = pool_ctx {
                state.swarm_label = Some(ctx.identity.primary.clone());
                state.swarm_aliases.clone_from(&ctx.identity.aliases);
                state.home_accounts = roster
                    .rows
                    .iter()
                    .filter(|row| row.held_by.is_none() && ctx.is_home_for_row(Cli::Claude, row))
                    .map(|row| row.label.clone())
                    .collect();
            }
            state_changed = (&*state, &*logins) != (&before.0, &before.1);
            Ok(events)
        }, || {
            publishing = true;
            for row in &roster.rows {
                if let Some(dir) = &row.dir {
                    aterm_types::fs_restricted::ensure_private_dir(&store.logins_dir())
                        .and_then(|()| aterm_types::fs_restricted::ensure_private_dir(Path::new(dir)))
                        .map_err(|why| format!("could not create {dir}: {why}"))?;
                }
            }
            publish_if_changed(&fleet_write_to, fleet_held.as_deref(), &fleet_toml, fleet_mode)?;
            for expected in fleet_snapshots {
                match expected.publish {
                    Publish::No => {}
                    Publish::Fleet => {
                        publish_if_changed(&expected.path, expected.bytes.as_deref(), &fleet_toml, Some(0o600))?;
                    }
                    Publish::Remove => match fs::remove_file(&expected.path) {
                        Ok(()) => copy_removed = true,
                        Err(why) if why.kind() == io::ErrorKind::NotFound => {}
                        Err(why) => {
                            return Err(format!("could not remove {}: {why}", expected.path.display()));
                        }
                    },
                }
            }
            Ok(())
        })
        .map_err(|why| {
            if publishing {
                format!(
                    "sync publication did not complete: {why}; any pending local commit is \
                     retained for the next sync to resume; auxiliary fleet sources or login \
                     directories may already have changed"
                )
            } else {
                format!("sync refused before roster publication: {why}")
            }
        })?;
    let changed = changed || state_changed || copy_removed || !events.is_empty();
    if changed {
        store.touch_wake().map_err(|why| {
            format!(
                "sync files were published but the wake marker could not be written: {why}; \
                 the published files are kept"
            )
        })?;
    }
    Ok(SyncedAccounts {
        fleet_path,
        note,
        changed,
    })
}

/// Run `aterm harness accounts sync [<source>] [--machine <mN>] [--json]`.
pub fn run_accounts_sync(
    env: &Env,
    source: Option<&str>,
    machine_override: Option<&str>,
    json: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitCode {
    let how = SyncHow {
        fetch: &fetch_source_text,
        ladder: &sync_lock_ladder(),
        fleet_snapshots: &[],
        heal: Some(default_login_check_live),
        allow_empty_pool: false,
    };
    run_accounts_sync_with(env, (source, machine_override, json), (out, err), &how)
}

struct SyncHow<'a> {
    fetch: &'a Fetch<'a>,
    ladder: &'a [Duration],
    fleet_snapshots: &'a [FleetSnapshot],
    /// The default login's one read-only check after the sync, where `accounts sync` makes it
    /// ([`heal_default_login_now`]); `None` makes none.
    heal: Option<fn(&Path) -> super::cli::LoginCheck>,
    allow_empty_pool: bool,
}

fn run_accounts_sync_with(
    env: &Env,
    (source, machine_override, json): (Option<&str>, Option<&str>, bool),
    (out, err): (&mut dyn Write, &mut dyn Write),
    how: &SyncHow<'_>,
) -> ExitCode {
    let report = match sync_accounts_with(env, source, machine_override, how) {
        Ok(report) => report,
        Err(why) => {
            let _ = writeln!(err, "aterm harness accounts sync: {why}");
            return ExitCode::FAILURE;
        }
    };
    write_sync_report(&report, env.now, json, out, err)
}

/// The source this sync admitted and whether its publication or recovery wrote data.
struct SyncPublication {
    machine: String,
    roster_path: PathBuf,
    fleet_path: PathBuf,
    fleet_accounts: usize,
    changed: bool,
}

/// One observation after sync and sign-in recovery. The source metadata describes
/// this sync; the account records describe one completed local publication. Login
/// file presence is observed once afterward, outside the local commit protocol.
struct SyncReport {
    publication: SyncPublication,
    records: accounts::AccountRecords,
    enrolled: BTreeSet<String>,
    needs_renew: Vec<String>,
}

impl SyncReport {
    fn into_init(self, recovered: bool) -> InitSynced {
        let warm_rows = self
            .records
            .roster
            .rows
            .iter()
            .filter(|row| row.held_by.is_none())
            .count();
        let enrolled = self
            .records
            .roster
            .rows
            .iter()
            .filter(|row| {
                row.held_by.is_none()
                    && (row.dir.is_none() || self.enrolled.contains(&row.label))
                    && self
                        .records
                        .state
                        .accounts
                        .get(&row.label)
                        .is_none_or(|account| account.auth != accounts::Auth::SignedOut)
            })
            .count();
        InitSynced {
            machine: self.publication.machine,
            rows: warm_rows,
            enrolled,
            fleet: self.publication.fleet_accounts,
            changed: self.publication.changed || recovered,
            to_sign_in: self.needs_renew,
        }
    }
}

fn sync_accounts_with(
    env: &Env,
    source: Option<&str>,
    machine_override: Option<&str>,
    how: &SyncHow<'_>,
) -> Result<SyncReport, String> {
    let normalized = source_env(env)?;
    let env = &normalized;
    let roster_path = env
        .accounts_roster()
        .ok_or("no config directory is known (set HOME or XDG_CONFIG_HOME)")?;
    let store = env.accounts_store();
    let recovered_commit = store.recover_sync_within(how.ladder, &roster_path)?;
    let (fleet, opt_in, pool_ctx) =
        load_fleet_source_for_machine(env, source, machine_override, how.fetch)?;
    let default_email = read_default_profile_email(env);
    let machine = if let Some(ctx) = &pool_ctx {
        ctx.identity.primary.clone()
    } else {
        sync_machine(env, &fleet, default_email.as_deref(), machine_override)
            .ok_or("could not determine machine id (`m<N>`); pass `--machine m8`")?
    };
    let fleet_accounts = pool_ctx.as_ref().map_or(fleet.accounts.len(), |ctx| {
        ctx.pool.accounts_for_cli(Cli::Claude).len()
    });
    if let Some(ctx) = &pool_ctx
        && how.allow_empty_pool
        && fleet.accounts_for_machine(&machine).is_empty()
        && default_email.is_none()
        && !roster_path.exists()
    {
        let mut state_wrote = false;
        store
            .transact_within(how.ladder, env.now, &mut None, |state| {
                let target_label = Some(ctx.identity.primary.clone());
                if state.swarm_label != target_label
                    || state.swarm_aliases != ctx.identity.aliases
                    || !state.home_accounts.is_empty()
                {
                    state.swarm_label = target_label;
                    state.swarm_aliases.clone_from(&ctx.identity.aliases);
                    state.home_accounts.clear();
                    state_wrote = true;
                }
                Ok(Vec::new())
            })
            .map_err(|why| format!("could not update account state: {why}"))?;
        let fleet_path = roster_path
            .parent()
            .map(|p| p.join(FLEET_ROSTER_FILE))
            .unwrap_or_else(|| PathBuf::from(FLEET_ROSTER_FILE));
        let (fleet_write_to, fleet_mode) = super::cli::roster_target(&fleet_path)?;
        let fleet_toml = fleet.to_toml_for(Some(opt_in));
        let fleet_wrote = if fs::read(&fleet_write_to).ok().as_deref()
            != Some(fleet_toml.as_bytes())
        {
            super::upgrade_models::write_atomic_mode(&fleet_write_to, &fleet_toml, fleet_mode)
                .map_err(|why| format!("could not write {}: {why}", fleet_write_to.display()))?;
            true
        } else {
            false
        };
        return read_sync_report(
            &store,
            env.now,
            SyncPublication {
                machine,
                roster_path,
                fleet_path,
                fleet_accounts,
                changed: state_wrote || fleet_wrote || recovered_commit,
            },
            None,
        );
    }

    let prelim_roster =
        build_machine_roster(&fleet, &machine, &store, None, default_email.as_deref());
    let standing_walls = env
        .claude_dir()
        .map(|dir| find_transcript_walls_by_email(&dir, &prelim_roster, env.now))
        .unwrap_or_default();
    let SyncedAccounts {
        fleet_path,
        note,
        changed,
    } = persist_accounts_sync_with_pool(
        &store,
        &roster_path,
        (&fleet, opt_in, pool_ctx.as_ref()),
        (&machine, default_email.as_deref()),
        &standing_walls,
        env.now,
        (how.ladder, how.fleet_snapshots),
    )?;
    let recovered = recover_interrupted_staging_renewals_for(
        Cli::Claude,
        &store,
        &roster_path,
        &AUTO_RENEW_GUARD,
        env.now,
    );
    let healed = how.heal.zip(env.home.as_deref()).and_then(|(check, home)| {
        heal_default_login_now(home, &store, &roster_path, env.now, check)
    });
    let mut report = read_sync_report(
        &store,
        env.now,
        SyncPublication {
            machine,
            roster_path,
            fleet_path,
            fleet_accounts,
            changed: changed
                || recovered_commit
                || recovered
                || healed.as_ref().is_some_and(|(healed, _)| *healed),
        },
        note,
    )?;
    if let Some((_, line)) = healed {
        report.records.notes.push(line);
    }
    Ok(report)
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSyncReport",
        action = "Read",
        project = "aterm_agent::harness::accounts_sync::sync_report_tests::report_transition"
    )
)]
fn read_sync_report(
    store: &accounts::Store,
    now: i64,
    mut publication: SyncPublication,
    note: Option<String>,
) -> Result<SyncReport, String> {
    let failure = |why| {
        format!(
            "sync ran but its report could not be read: {why}; \
             any published files are kept"
        )
    };
    // A staging transaction can leave an admitted journal even when it could not
    // finish. Count the recovery performed by the same read that captures its image.
    let (recovered, mut records) = store
        .recover_account_records(Some(&publication.roster_path), now)
        .map_err(|why| {
            failure(match why {
                accounts::RosterReadError::Roster(why)
                | accounts::RosterReadError::Records(why) => why,
            })
        })?;
    publication.changed |= recovered;
    if let Some(note) = note
        && !records.notes.contains(&note)
    {
        records.notes.insert(0, note);
    }
    let roster = &records.roster;
    let state = &records.state;
    let enrolled = accounts::enrolled(roster);
    let needs_renew: Vec<String> = roster
        .rows
        .iter()
        .filter(|r| {
            r.held_by.is_none()
                && r.dir.is_some()
                && !enrolled.contains(&r.label)
                && !state
                    .accounts
                    .get(&r.label)
                    .is_some_and(|a| a.limited_until(now).is_some())
        })
        .map(|r| r.label.clone())
        .collect();
    Ok(SyncReport {
        publication,
        records,
        enrolled,
        needs_renew,
    })
}

fn write_sync_report(
    report: &SyncReport,
    now: i64,
    json: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitCode {
    let SyncReport {
        publication:
            SyncPublication {
                machine,
                roster_path,
                fleet_path,
                fleet_accounts,
                changed,
            },
        records,
        enrolled,
        needs_renew,
    } = report;
    let roster = &records.roster;
    let state = &records.state;
    for note in &records.notes {
        let _ = writeln!(err, "aterm harness accounts: {note}");
    }
    if json {
        let mut o = Map::new();
        o.insert("schema".to_owned(), Value::from(1u64));
        o.insert("kind".to_owned(), Value::from("accounts-sync".to_owned()));
        o.insert("machine".to_owned(), Value::from(machine.clone()));
        o.insert(
            "roster".to_owned(),
            Value::from(roster_path.display().to_string()),
        );
        o.insert(
            "fleet_roster".to_owned(),
            Value::from(fleet_path.display().to_string()),
        );
        o.insert(
            "fleet_accounts".to_owned(),
            Value::from(u64::try_from(*fleet_accounts).unwrap_or(0)),
        );
        o.insert("changed".to_owned(), Value::from(*changed));
        o.insert(
            "machine_accounts".to_owned(),
            Value::Array(
                roster
                    .rows
                    .iter()
                    .map(|r| {
                        let here = r.dir.is_none() || enrolled.contains(&r.label);
                        let cond = accounts::condition(r, state, here, now);
                        let mut m = Map::new();
                        m.insert("label".to_owned(), Value::from(r.label.clone()));
                        m.insert("class".to_owned(), Value::from(r.class.as_str().to_owned()));
                        m.insert("default_login".to_owned(), Value::from(r.dir.is_none()));
                        m.insert("enrolled".to_owned(), Value::from(here));
                        m.insert(
                            "condition".to_owned(),
                            Value::from(cond.as_str().to_owned()),
                        );
                        Value::Object(m)
                    })
                    .collect(),
            ),
        );
        o.insert(
            "needs_renew".to_owned(),
            Value::Array(needs_renew.iter().cloned().map(Value::from).collect()),
        );
        let _ = writeln!(
            out,
            "{}",
            aterm_json::to_string(&Value::Object(o)).unwrap_or_default()
        );
    } else {
        let _ = writeln!(
            out,
            "synced machine `{machine}` ({} accounts on this machine, {} across fleet){}",
            roster.rows.len(),
            fleet_accounts,
            if *changed { "" } else { "; no account changes" }
        );
        let _ = writeln!(out, "roster: {}", roster_path.display());
        let _ = writeln!(out, "fleet:  {}", fleet_path.display());
        for row in &roster.rows {
            let here = row.dir.is_none() || enrolled.contains(&row.label);
            let cond = match accounts::condition(row, state, here, now) {
                accounts::Condition::Limited { until } => {
                    format!("limited until {}", super::usage::rfc3339_utc(until))
                }
                accounts::Condition::NotEnrolled => match state
                    .accounts
                    .get(&row.label)
                    .and_then(|a| a.limited_until(now))
                {
                    Some(until) => format!(
                        "not-enrolled (limited until {})",
                        super::usage::rfc3339_utc(until)
                    ),
                    None => accounts::Condition::NotEnrolled.as_str().to_owned(),
                },
                other => other.as_str().to_owned(),
            };
            let where_word = if row.dir.is_none() {
                "default login (adopted)"
            } else if here {
                "isolated login (enrolled)"
            } else {
                "isolated login (not signed in yet)"
            };
            let _ = writeln!(
                out,
                "  {:8}  {:9}  {:24}  {}",
                row.label,
                row.class.as_str(),
                cond,
                where_word
            );
        }
        if !needs_renew.is_empty() {
            let _ = writeln!(
                out,
                "not signed in yet: {}; aterm signs them in by itself (`aterm harness accounts` \
                 shows each one's last attempt, `aterm harness accounts renew {}` runs it now)",
                needs_renew.join(", "),
                needs_renew.join(" ")
            );
        }
    }
    ExitCode::SUCCESS
}

/// The machine a sync takes `fleet`'s rows for: `machine_override`, else `[fleet] machine`, else
/// this host's id where the fleet lists it (or the default login, `default_email`, holds no fleet
/// row), else the first machine of the fleet row the default login holds.
fn sync_machine(
    env: &Env,
    fleet: &FleetRoster,
    default_email: Option<&str>,
    machine_override: Option<&str>,
) -> Option<String> {
    machine_for(
        env.config.as_deref(),
        fleet,
        default_email,
        machine_override,
    )
}

/// [`sync_machine`], for the config file `config`.
fn machine_for(
    config: Option<&Path>,
    fleet: &FleetRoster,
    default_email: Option<&str>,
    machine_override: Option<&str>,
) -> Option<String> {
    machine_override
        .and_then(normalize_machine_id)
        .or_else(|| config_machine_id(config))
        .or_else(|| host_or_login_machine(detect_machine_id(None, None), fleet, default_email))
}

/// [`machine_for`]'s choice past an override and a config: `host`, this Mac's id, where `fleet`
/// lists it or the default login (`default_email`) holds no fleet row, else the first machine of
/// the row it holds.
fn host_or_login_machine(
    host: Option<String>,
    fleet: &FleetRoster,
    default_email: Option<&str>,
) -> Option<String> {
    let email_machine = || {
        default_email.and_then(|de| {
            fleet
                .accounts
                .iter()
                .find(|a| accounts::email_matches(&a.email, de))
                .and_then(|a| a.machines.first().cloned())
        })
    };
    host.filter(|m| !fleet.accounts_for_machine(m).is_empty() || email_machine().is_none())
        .or_else(email_machine)
}

/// The text each fleet file of `files` ([`FleetOrigin::Local`]) holds with `row` listed as one
/// more account of this machine ([`sync_machine`]), which the next sync then keeps; a file that
/// lists it already is left out. Nothing is written here: `accounts add` or `adopt-default` writes them, under the
/// accounts lock, before the roster.
/// # Errors
/// `row` has no email (a fleet lists each account by its email), a file cannot be read or is
/// refused, it lists `row`'s label as another account or `row`'s account under another label,
/// or this machine's id is unknown.
pub(super) fn list_in_local_fleet(
    env: &Env,
    files: &[PathBuf],
    row: &Row,
) -> Result<Vec<(PathBuf, String)>, String> {
    let email = row.email.as_deref().ok_or_else(|| {
        format!(
            "{} lists each account by its email, and a sync keeps only the accounts it lists: \
             pass `--email <address>`; nothing was written",
            files
                .first()
                .map_or_else(|| FLEET_ROSTER_FILE.to_owned(), |f| f.display().to_string())
        )
    })?;
    let default_email = read_default_profile_email(env);
    let mut texts = Vec::new();
    for file in files {
        let text = read_local_source_text(file)
            .map_err(|why| format!("{}: {why}; nothing was written", file.display()))?;
        let mut fleet = FleetRoster::from_toml(&text, &file.display().to_string())
            .map_err(|why| format!("{why}; nothing was written"))?;
        let machine =
            sync_machine(env, &fleet, default_email.as_deref(), None).ok_or_else(|| {
                "could not determine this machine's id (`m<N>`) for the fleet row: set `[fleet] \
             machine` in aterm.toml; nothing was written"
                    .to_owned()
            })?;
        let same_email = |acct: &FleetAccount| accounts::email_matches(&acct.email, email);
        if let Some(listed) = fleet.accounts.iter().find(|a| a.label == row.label) {
            if same_email(listed) && listed.machines.contains(&machine) {
                continue;
            }
            return Err(format!(
                "{} lists `{}` already as another account, so a sync would give the label that \
                 one: pick another label; nothing was written",
                file.display(),
                row.label
            ));
        }
        if let Some(listed) = fleet.accounts.iter().find(|a| same_email(a)) {
            return Err(format!(
                "{} lists that account already, as `{}` (machines {}): a sync keeps one row per \
                 account; nothing was written",
                file.display(),
                listed.label,
                listed.machines.join(", ")
            ));
        }
        fleet.accounts.push(FleetAccount {
            label: row.label.clone(),
            machines: vec![machine],
            class: row.class,
            email: email.to_owned(),
            browser: row.browser.clone(),
            vendor: accounts::Vendor::default(),
        });
        texts.push((file.clone(), fleet.to_toml_for(opt_in_of(&text))));
    }
    Ok(texts)
}

static AUTO_SYNC_COOLDOWN: std::sync::Mutex<Option<Instant>> = std::sync::Mutex::new(None);

/// How often upkeep rediscovers local accounts and revalidates configured local sources whose
/// metadata is unchanged ([`init_sync_needed`], [`key_file_changed`]).
const REDISCOVER_EVERY: Duration = Duration::from_secs(15 * 60);

/// How long a window's upkeep waits before it syncs again for the same default login the roster
/// still does not give its default row ([`default_login_unlisted`]).
const UNLISTED_RETRY: Duration = Duration::from_secs(10 * 60);

/// What a window's upkeep or the setup pass's resync last did to keep the roster up with this
/// machine's own logins and sources ([`sync_called_for`]), in this process only.
#[derive(Debug, Default)]
struct Rediscovery {
    /// When it last read this machine's logins and transcripts for new accounts.
    read_at: Option<Instant>,
    /// The default login's email it last synced for because the roster's default row was another
    /// account, and when.
    synced_for: Option<(String, Instant)>,
    /// The local source observation last scheduled for an attempt, whether it succeeded or
    /// failed. Even an unchanged observation is retried after `REDISCOVER_EVERY`.
    key_attempt: Option<LocalSourceAttempt>,
    /// The addresses a fleet discovery made lists only for a credential, and when they were
    /// read ([`unclaimed_emails`]).
    unclaimed: Option<(Instant, BTreeSet<String>)>,
    /// What the last sync that failed read ([`sync_inputs`]) and when it ran ([`periodic_sync`]).
    failed: Option<FailedSync>,
    /// Each pool syncing by default whose last upkeep sync, with no roster of it here, took
    /// nothing or failed ([`default_sync_settled`]): what it read ([`cli_sync_inputs`]) and
    /// when ([`default_sync_holds`]).
    default_held: BTreeMap<Cli, ([u8; 32], Instant)>,
}

static REDISCOVERY: std::sync::Mutex<Rediscovery> = std::sync::Mutex::new(Rediscovery {
    read_at: None,
    synced_for: None,
    key_attempt: None,
    unclaimed: None,
    failed: None,
    default_held: BTreeMap::new(),
});

/// A periodic caller's sync that failed ([`periodic_sync`]).
#[derive(Debug, Clone, PartialEq, Eq)]
struct FailedSync {
    /// What it read ([`sync_inputs`]).
    inputs: [u8; 32],
    /// When it ran.
    at: Instant,
    /// Why, which the next failure is logged against ([`note_sync_result`]).
    why: String,
    /// Whether it holds a sync of the same inputs back: not where another process held the
    /// accounts lock, which says nothing of what the sync read.
    holds: bool,
}

impl FailedSync {
    /// Whether it holds back a sync that would read `inputs` now: one of the same inputs, within
    /// [`REDISCOVER_EVERY`] of this attempt, where its failure was not the lock's.
    fn holds_back(&self, inputs: &[u8; 32]) -> bool {
        self.holds && self.inputs == *inputs && self.at.elapsed() < REDISCOVER_EVERY
    }
}

/// Keeps what a periodic caller's sync of `inputs` did in `failed` ([`periodic_sync`]), and
/// returns whether its failure is news, which a caller logs: one of other inputs or for another
/// reason than the last failure's. A sync that succeeds forgets the last failure.
fn note_sync_result(
    failed: &mut Option<FailedSync>,
    inputs: [u8; 32],
    result: Result<(), &str>,
) -> bool {
    let Err(why) = result else {
        *failed = None;
        return false;
    };
    let news = failed
        .as_ref()
        .is_none_or(|last| last.inputs != inputs || last.why != why);
    *failed = Some(FailedSync {
        inputs,
        at: Instant::now(),
        why: why.to_owned(),
        holds: !why.contains(accounts::LOCK_HELD),
    });
    news
}

/// Whether [`REDISCOVER_EVERY`] has passed since the upkeep last read this machine's logins and
/// transcripts for new accounts; when it has, that read is counted from now.
fn rediscover_due(rediscovery: &std::sync::Mutex<Rediscovery>) -> bool {
    let mut held = rediscovery
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if held
        .read_at
        .is_some_and(|at| at.elapsed() < REDISCOVER_EVERY)
    {
        return false;
    }
    held.read_at = Some(Instant::now());
    true
}

/// Whether a sync is due because the default login holds an account the roster at `roster_path`
/// does not give its default row (one no row lists, or another row's: the vendor's own sign-in
/// on the default login changed it): at once for an email the upkeep has not synced for, else
/// once per [`UNLISTED_RETRY`]. Only a roster a configured source writes is synced
/// ([`configured_source`]); one written by hand stays as written.
/// The profile names the provider identity, which may differ from its login alias.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountAliasRelabel",
        action = "ChangedDefault",
        project = "aterm_agent::harness::accounts_sync::alias_relabel_tests::alias_transition"
    )
)]
fn default_login_unlisted(
    home: &Path,
    state_dir: &Path,
    (roster_path, config): (&Path, &Path),
    now: i64,
    rediscovery: &std::sync::Mutex<Rediscovery>,
) -> bool {
    if configured_source(Some(config), Some(roster_path)).is_none() {
        return false;
    }
    let Ok(roster) = Roster::load(roster_path) else {
        return false;
    };
    if roster.rows.is_empty() {
        return false;
    }
    let Some(email) = read_default_profile_email(&quiet_env(home, state_dir, config, now)) else {
        return false;
    };
    if roster
        .row_for_login(None)
        .and_then(Row::expected_identity_email)
        .is_some_and(|held| accounts::email_matches(held, &email))
    {
        return false;
    }
    let mut held = rediscovery
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if held.synced_for.as_ref().is_some_and(|(was, at)| {
        accounts::email_matches(was, &email) && at.elapsed() < UNLISTED_RETRY
    }) {
        return false;
    }
    held.synced_for = Some((email, Instant::now()));
    true
}

/// Which of the upkeep's two works one call makes ([`upkeep_live`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpkeepStep {
    /// Both, the sync first: the activation wake's upkeep.
    Both,
    /// Sync a missing roster alone before reading a wall against the roster.
    Sync,
    /// The renewal alone: after a wall is recorded, so the wall just recorded is the one renewed.
    Renew,
}

impl UpkeepStep {
    const fn syncs(self) -> bool {
        matches!(self, Self::Both | Self::Sync)
    }

    const fn renews(self) -> bool {
        matches!(self, Self::Both | Self::Renew)
    }
}

/// What one upkeep of the window's fleet accounts did ([`upkeep_live`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Upkept {
    /// `[harness] accounts = "off"` ([`accounts_sync_off`]): neither sync nor sign-in ran.
    Off,
    /// No automatic source is configured ([`configured_source`]), so nothing is fetched; a saved
    /// explicit selection may still progress, and the rows of a roster that stands are still
    /// signed in by the automatic ladder ([`auto_renew_next_account`]) unless the config turns
    /// sign-ins off ([`sign_ins_on`]).
    Unconfigured {
        /// [`fleet_file_ignored`].
        fleet_file_ignored: bool,
        /// A renewal or saved selection progressed.
        renewal: bool,
    },
    /// A source is configured (`kind`, [`FleetSource::kind`]).
    Configured {
        /// [`FleetSource::kind`].
        kind: &'static str,
        /// `accounts.toml` exists after the upkeep.
        roster: bool,
        /// A renewal or saved selection progressed ([`auto_renew_next_account`]).
        renewal: bool,
    },
}

struct UpkeepEffects<'a> {
    fetch: &'a Fetch<'a>,
    spawn_renewal: &'a SpawnRenewal,
    quota_refresh: &'a RefreshQuota,
    sync_cooldown: &'a std::sync::Mutex<Option<Instant>>,
    /// When the upkeep last caught its roster up with this machine's own logins
    /// ([`rediscover_due`], [`default_login_unlisted`]).
    rediscovery: &'a std::sync::Mutex<Rediscovery>,
    renew_guard: &'a std::sync::Arc<std::sync::Mutex<AutoRenewGuard>>,
    /// Whether a headless-capable browser is installed for `home` ([`browser_installed`]).
    find_browser: &'a (dyn Fn(&Path) -> bool + Sync),
    /// One bounded `aterm secrets sync` for `home` ([`run_vault_sync`]).
    vault_sync: fn(&Path) -> super::sign_in_ladder::Vault,
    /// Where that sync, and the default login's one check ([`Self::default_check`]), run, off
    /// the caller's thread ([`spawn_vault_thread`]).
    spawn_vault: &'a SpawnRenewal,
    /// One read-only check of this machine's own login, the default home's
    /// ([`default_login_check_live`]).
    default_check: fn(&Path) -> super::cli::LoginCheck,
    /// What the configured mailbox offers a row's sign-in ([`ladder_mailbox_live`]).
    mailbox: &'a LadderMailbox,
    /// Where automatic sign-ins and source refresh refusals go ([`log_sign_in`]).
    log: SignInLog,
    /// One run of `claude auth login` for a route, where it is not the live one
    /// ([`spawn_row_renewal`]).
    run_attempt: Option<RunAttempt>,
    shared: shared_renewal::Effects<'a>,
}

type SpawnRenewal = dyn Fn(Box<dyn FnOnce() + Send>) -> io::Result<()> + Sync;
type RefreshQuota = dyn Fn(&Path, &Path, &Path, i64) + Sync;

/// [`UpkeepEffects::mailbox`]: what the configured mailbox offers `row` for `home` and `store`,
/// and its masked login where it has one ([`mailbox_offer`]).
type LadderMailbox = dyn Fn(&Path, &accounts::Store, &accounts::Row) -> (super::sign_in_ladder::Mailbox, Option<String>)
    + Sync;

/// [`UpkeepEffects::log`].
type SignInLog = std::sync::Arc<dyn Fn(&str) + Send + Sync>;

/// [`UpkeepEffects::run_attempt`]: one run with the browser it is handed, headless where the
/// flag says so, and how it ended.
type RunAttempt =
    std::sync::Arc<dyn Fn(&super::cli::RenewBrowser, bool) -> super::cli::Renewal + Send + Sync>;

fn spawn_renewal_thread(work: Box<dyn FnOnce() + Send>) -> io::Result<()> {
    std::thread::Builder::new()
        .name("aterm-auto-renew".to_owned())
        .spawn(work)
        .map(|_| ())
}

fn spawn_vault_thread(work: Box<dyn FnOnce() + Send>) -> io::Result<()> {
    std::thread::Builder::new()
        .name("aterm-vault-sync".to_owned())
        .spawn(work)
        .map(|_| ())
}

/// The live [`UpkeepEffects::find_browser`]: [`super::headless_oauth::find_headless_browser`].
fn browser_installed(home: &Path) -> bool {
    super::headless_oauth::find_headless_browser(Some(home)).is_some()
}

/// How long one `aterm secrets sync` of the ladder may take before it is ended.
const VAULT_SYNC_BUDGET: Duration = Duration::from_secs(45);

/// The live [`UpkeepEffects::vault_sync`]: this program's own `secrets sync`, with no terminal,
/// its stdin empty, at most [`VAULT_SYNC_BUDGET`] (the LAN vault's fetch, its verification and
/// the files it delivers, `~/.secrets/ai-mailbox.toml` among them).
fn run_vault_sync(home: &Path) -> super::sign_in_ladder::Vault {
    use super::sign_in_ladder::Vault;
    // Only the shipped executable (or a build of it) takes `secrets sync`: a test binary or
    // another host of this library would read the words as its own arguments.
    let Some(exe) = std::env::current_exe().ok().filter(|exe| {
        exe.file_stem()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name == "aterm")
    }) else {
        return Vault::Failed("this program is not aterm".to_owned());
    };
    let mut cmd = aterm_uds::spawnfd::command(&exe);
    cmd.args(["secrets", "sync", "--diagnostic"])
        .env("HOME", home);
    match super::align::capture_bounded(cmd, VAULT_SYNC_BUDGET, 16 * 1024) {
        Ok(ran) => Vault::of_sync(&String::from_utf8_lossy(&ran.stdout), ran.code),
        Err(why) => Vault::of_run_error(&why, VAULT_SYNC_BUDGET),
    }
}

/// The live [`UpkeepEffects::default_check`]: one `claude auth status --json` of this machine's
/// own login, run as `accounts login`'s check runs it ([`super::cli::auth_status_of`] with an
/// empty [`accounts::LOGIN_ENV`]: the default home), read as [`super::cli::login_check`] reads it.
/// It only asks; it never signs in.
fn default_login_check_live(home: &Path) -> super::cli::LoginCheck {
    let program = resolve_claude_program(home);
    super::cli::login_check(super::cli::auth_status_of(
        program.as_os_str(),
        "",
        &|name| std::env::var_os(name),
    ))
}

/// How many of the ledger's newest events [`default_login_heal_due`] reads for the default
/// login's last move.
const HEAL_LEDGER_TAIL: usize = 2_000;

/// A default row the default login's one check is due for ([`default_login_heal_due`]).
#[derive(Debug, Clone, PartialEq, Eq)]
struct DefaultHeal {
    /// The roster's default row.
    label: String,
    /// When its sign-out on record was written.
    signed_out_at: i64,
    /// The row the default login held before it moved onto `label`, where that row holds no
    /// login bound elsewhere: the sessions on record under it ran on the default login.
    was: Option<String>,
}

/// Whether the default login's one check is due: the roster's default row is the account the
/// default login holds (its profile's address), no renewal bound it elsewhere, it is on record
/// as signed out, and the ledger's newest `displaced` event moved the default login onto it
/// after that sign-out. That is a sign-out an earlier login of the row left (its own, before a
/// person's `/login` made the one the default login holds now) on a machine whose roster already
/// named the row when the sync that saw the move ran, so the sync's own repoint
/// (`note_default_login_made`) never fired. A sign-out written at or after the move is the
/// login's own news and is left alone.
fn default_login_heal_due(
    home: &Path,
    store: &accounts::Store,
    roster: &Roster,
    now: i64,
) -> Option<DefaultHeal> {
    let row = roster.row_for_login(None)?;
    let email = home_default_email(home)?;
    if !row
        .expected_identity_email()
        .is_some_and(|expected| accounts::email_matches(expected, &email))
    {
        return None;
    }
    let (state, _) = store.load(now).ok()?;
    let (logins, _) = store.load_logins(now).ok()?;
    if logins.get(&row.label).is_some_and(|b| b.dir.is_some()) {
        return None;
    }
    let account = state.accounts.get(&row.label)?;
    if account.auth != accounts::Auth::SignedOut {
        return None;
    }
    let signed_out_at = account.auth_at?;
    let moved = store
        .tail(HEAL_LEDGER_TAIL)
        .ok()?
        .into_iter()
        .rev()
        .find(|e| e.kind == accounts::Kind::Displaced && e.to.as_deref() == Some(&row.label))?;
    if moved.t <= signed_out_at {
        return None;
    }
    let was = Some(moved.account)
        .filter(|was| *was != row.label && logins.get(was).is_none_or(|b| b.dir.is_none()));
    Some(DefaultHeal {
        label: row.label.clone(),
        signed_out_at,
        was,
    })
}

/// What the default login's one check found, applied ([`default_login_heal_due`]): signed in,
/// the row holds the login made by the move (`note_default_login_made`: Ok, served, its earlier
/// login's end and failed sign-in cleared, the default login's sessions and `active` its), under
/// the store's lock and only while the sign-out it was due for is still the one on record.
/// Signed out, or no answer, records nothing. Returns whether the row was healed.
fn apply_default_heal(
    store: &accounts::Store,
    heal: &DefaultHeal,
    check: &super::cli::LoginCheck,
    now: i64,
) -> bool {
    if *check != super::cli::LoginCheck::SignedIn {
        return false;
    }
    let mut healed = false;
    let mut note = None;
    let _ = store.transact_logins(now, &mut note, |state, logins| {
        let standing = state.accounts.get(&heal.label).is_some_and(|a| {
            a.auth == accounts::Auth::SignedOut && a.auth_at == Some(heal.signed_out_at)
        }) && logins.get(&heal.label).is_none_or(|b| b.dir.is_none());
        if !standing {
            return Ok(Vec::new());
        }
        healed = true;
        Ok(accounts::note_default_login_made(
            state,
            logins,
            &heal.label,
            heal.was.as_deref(),
            now,
        ))
    });
    healed
}

/// The `aterm.log` line of one default-login check ([`heal_default_login_in_upkeep`]).
fn default_heal_line(heal: &DefaultHeal, check: &super::cli::LoginCheck, healed: bool) -> String {
    let at = super::usage::rfc3339_utc(heal.signed_out_at);
    if healed {
        format!(
            "account {}: the default login checked signed in, so its sign-out of {at} was an \
             earlier login's; on record as signed in{}",
            heal.label,
            heal.was
                .as_deref()
                .map(|was| format!(", with the sessions on record under {was}"))
                .unwrap_or_default()
        )
    } else {
        format!(
            "account {}: the default login's check after its move answered {}; its sign-out of \
             {at} stands",
            heal.label,
            check.words()
        )
    }
}

/// THE DEFAULT LOGIN'S HEAL in the window's upkeep: where [`default_login_heal_due`] holds and
/// this process has not checked that row's sign-out yet, one read-only check of the default
/// login ([`UpkeepEffects::default_check`]) off the caller's thread, then
/// [`apply_default_heal`] and one `aterm.log` line. Once per row and sign-out on record.
fn heal_default_login_in_upkeep(
    home: &Path,
    state_dir: &Path,
    roster_path: &Path,
    now: i64,
    fx: &UpkeepEffects<'_>,
) {
    let Ok(roster) = Roster::load(roster_path) else {
        return;
    };
    let store = accounts::Store::at(state_dir);
    let Some(heal) = default_login_heal_due(home, &store, &roster, now) else {
        return;
    };
    {
        let mut guard = fx
            .renew_guard
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let key = (heal.label.clone(), heal.signed_out_at);
        if guard.default_checked.as_ref() == Some(&key) {
            return;
        }
        guard.default_checked = Some(key);
    }
    let (check, log, home) = (
        fx.default_check,
        std::sync::Arc::clone(&fx.log),
        home.to_path_buf(),
    );
    let _ = (fx.spawn_vault)(Box::new(move || {
        let found = check(&home);
        let healed = apply_default_heal(&store, &heal, &found, now);
        log(&default_heal_line(&heal, &found, healed));
    }));
}

/// [`heal_default_login_in_upkeep`] for `aterm harness accounts sync`, on the caller's thread:
/// the line it logs, where the check was due.
fn heal_default_login_now(
    home: &Path,
    store: &accounts::Store,
    roster_path: &Path,
    now: i64,
    check: fn(&Path) -> super::cli::LoginCheck,
) -> Option<(bool, String)> {
    let roster = Roster::load(roster_path).ok()?;
    let heal = default_login_heal_due(home, store, &roster, now)?;
    let found = check(home);
    let healed = apply_default_heal(store, &heal, &found, now);
    Some((healed, default_heal_line(&heal, &found, healed)))
}

/// The Kimi account whose quota [`refresh_kimi_quota`] reads afresh at `now`: the active row (or
/// the roster's default row) once its sample or its monthly window is due and no probe holds it;
/// `None` when the records cannot be read, no row stands, or the observation is still fresh.
fn kimi_quota_label(home: &Path, state_dir: &Path, roster_path: &Path, now: i64) -> Option<String> {
    let store = accounts::Store::at_for(state_dir, Cli::Kimi, home);
    let records = store.read_account_records(Some(roster_path), now).ok()?;
    let label = records.state.active.as_deref().or_else(|| {
        records
            .roster
            .row_for_login(None)
            .map(|row| row.label.as_str())
    })?;
    let account = records.state.accounts.get(label);
    let sample_due = account.is_none_or(|account| accounts::probe_due(account, now));
    let monthly_due = account.is_none_or(|account| {
        account.windows.get("monthly").is_none_or(|window| {
            now.saturating_sub(window.at) >= super::accounts_pool::USAGE_FRESH_S
        })
    });
    if (!sample_due && !monthly_due) || accounts::probe_held(&records.state, label, now) {
        return None;
    }
    Some(label.to_owned())
}

/// The live [`UpkeepEffects::quota_refresh`]: a fresh Kimi `/usage` panel in a temporary tab,
/// at most once the account's sample is due ([`kimi_quota_label`]), and never inline with roster
/// upkeep.
fn refresh_kimi_quota(home: &Path, state_dir: &Path, roster_path: &Path, now: i64) {
    let Some(label) = kimi_quota_label(home, state_dir, roster_path, now) else {
        return;
    };
    let Some(exe) = std::env::current_exe().ok().filter(|exe| {
        exe.file_stem()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name == "aterm")
    }) else {
        return;
    };
    let store = accounts::Store::at_for(state_dir, Cli::Kimi, home);
    let claimed = store
        .transact(now, &mut None, |state| {
            Ok(accounts::claim_probe(state, &label, "quota-upkeep", now)
                .into_iter()
                .collect())
        })
        .is_ok_and(|events: Vec<accounts::Event>| events.len() == 1);
    if !claimed {
        return;
    }
    let home = home.to_path_buf();
    let state_dir = state_dir.to_path_buf();
    let spawned = spawn_renewal_thread(Box::new(move || {
        let mut cmd = aterm_uds::spawnfd::command(&exe);
        cmd.args(["harness", "--state"])
            .arg(&state_dir)
            .args(["accounts", "refresh", "--cli", "kimi"])
            .env("HOME", &home);
        match super::align::capture_bounded(cmd, Duration::from_secs(150), 16 * 1024) {
            Ok(ran) if ran.code == Some(0) => {
                log_sign_in("Kimi quota refreshed from its /usage panel")
            }
            Ok(_) => log_sign_in("Kimi quota refresh failed; the previous observation is kept"),
            Err(why) => log_sign_in(&format!(
                "Kimi quota refresh could not run: {}; the previous observation is kept",
                super::one_line(&why, 200)
            )),
        }
    }));
    if spawned.is_err() {
        log_sign_in("Kimi quota refresh could not start a background reader");
    }
}

/// The live [`UpkeepEffects::mailbox`]: the mailbox `row`'s sign-in reads, from the process's
/// environment ([`super::open_url::MAILBOX_ENV`] and its kin) and the mailbox files.
fn ladder_mailbox_live(
    home: &Path,
    store: &accounts::Store,
    row: &accounts::Row,
) -> (super::sign_in_ladder::Mailbox, Option<String>) {
    mailbox_offer(
        resolve_mailbox_dir(Some(home), store).is_some(),
        super::headless_oauth::discover_mailbox_config(Some(home), Some(&store.dir)),
        row,
    )
}

/// [`ladder_mailbox_live`] read from the mailbox files under `home` and `store` alone, never the
/// process's environment: a test's mailbox is its scratch home's.
#[cfg(test)]
fn ladder_mailbox_files(
    home: &Path,
    store: &accounts::Store,
    row: &accounts::Row,
) -> (super::sign_in_ladder::Mailbox, Option<String>) {
    let dir_found = [
        store.dir.join(super::open_url::DEFAULT_MAILBOX_DIR),
        home.join(".config/aterm")
            .join(super::open_url::DEFAULT_MAILBOX_DIR),
    ]
    .iter()
    .any(|dir| dir.is_dir());
    mailbox_offer(
        dir_found,
        super::headless_oauth::discover_mailbox_config_files(Some(home), Some(&store.dir), None),
        row,
    )
}

/// What a mailbox read offers `row`, and the masked login of the mailbox its email's codes are
/// read from (through its `[[mailboxes]]` entry where one receives it): a mailbox folder
/// (`dir_found`) or a configuration that reads its inbox over a folder, HTTP or IMAP is ready;
/// one that names only a webmail inbox needs a headless browser; an entry with no password yet
/// is named as such and never tried (the email's own entry, which the lab may be asked for, or
/// a shared one, which it is not); none for the row is none.
pub(super) fn mailbox_offer(
    dir_found: bool,
    cfg: Option<super::headless_oauth::MailboxConfig>,
    row: &accounts::Row,
) -> (super::sign_in_ladder::Mailbox, Option<String>) {
    use super::sign_in_ladder::Mailbox;
    if dir_found {
        return (Mailbox::Ready, None);
    }
    let email = row.email.as_deref();
    let Some(mut cfg) = cfg else {
        return (Mailbox::None, None);
    };
    cfg.alias_label_to_email(Some(&row.label), email);
    let unready = cfg.route_for(email).filter(|route| !route.ready());
    let view = cfg.routed_for(email);
    if !view.is_configured_for(email) {
        return match unready {
            Some(route) if route.own() => (Mailbox::NoPassword, Some(route.masked())),
            Some(route) => (Mailbox::SharedNoPassword, Some(route.masked())),
            None => (Mailbox::None, None),
        };
    }
    let login = view.masked_login_for(email);
    if view.dir.is_some()
        || view.http_url.is_some()
        || view.effective_imap_url(email).is_some()
        || (view.relay_enabled && email.is_some())
    {
        (Mailbox::Ready, login)
    } else {
        (Mailbox::WebmailOnly, login)
    }
}

/// Where an automatic sign-in's `aterm.log` line goes ([`log_sign_ins_to`]); unset, nowhere.
static SIGN_IN_LOG: std::sync::OnceLock<fn(&str)> = std::sync::OnceLock::new();

/// Sends each automatic sign-in's one line (its routes and how it ended) to `log`: the window's
/// `aterm.log`. The first call wins.
pub fn log_sign_ins_to(log: fn(&str)) {
    let _ = SIGN_IN_LOG.set(log);
}

fn log_sign_in(line: &str) {
    if let Some(log) = SIGN_IN_LOG.get() {
        log(line);
    }
}

/// The window's network moved (docs/DESIGN-fluid-swarm-2026-10-28.md F3c): the automatic sign-in
/// ladder's waits end ([`super::sign_in_ladder::Ladder::moved`]), so the next upkeep asks the
/// vault and tries each row at once.
pub fn network_moved() {
    AUTO_RENEW_GUARD
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .ladder
        .moved(Instant::now());
}

/// THE WINDOW'S FLEET ACCOUNTS' UPKEEP, the one way a window reaches this module.
pub fn upkeep_live(
    home: &Path,
    state_dir: &Path,
    roster_path: &Path,
    now: i64,
    step: UpkeepStep,
) -> Upkept {
    upkeep(home, state_dir, (roster_path, now, step), &live_effects())
}

/// The effects the live upkeep makes.
fn live_effects() -> UpkeepEffects<'static> {
    UpkeepEffects {
        fetch: &fetch_source_text,
        spawn_renewal: &spawn_renewal_thread,
        quota_refresh: &refresh_kimi_quota,
        sync_cooldown: &AUTO_SYNC_COOLDOWN,
        rediscovery: &REDISCOVERY,
        renew_guard: &AUTO_RENEW_GUARD,
        find_browser: &browser_installed,
        vault_sync: run_vault_sync,
        spawn_vault: &spawn_vault_thread,
        default_check: default_login_check_live,
        mailbox: &ladder_mailbox_live,
        log: std::sync::Arc::new(log_sign_in),
        run_attempt: None,
        shared: shared_renewal::Effects::live(),
    }
}

/// Claude Code's pool's upkeep ([`upkeep_claude`]), then the other CLIs' ([`upkeep_pools`]),
/// which the result does not report; [`Upkept::Off`] where [`accounts_sync_off`] holds.
fn upkeep(
    home: &Path,
    state_dir: &Path,
    (roster_path, now, step): (&Path, i64, UpkeepStep),
    fx: &UpkeepEffects<'_>,
) -> Upkept {
    let config = config_beside(roster_path);
    if accounts_sync_off(Some(&config)) {
        return Upkept::Off;
    }
    let claude = upkeep_claude(home, state_dir, (roster_path, now, step), fx);
    upkeep_pools(home, state_dir, (roster_path, now, step), fx);
    claude
}

fn upkeep_claude(
    home: &Path,
    state_dir: &Path,
    (roster_path, now, step): (&Path, i64, UpkeepStep),
    fx: &UpkeepEffects<'_>,
) -> Upkept {
    let config = config_beside(roster_path);
    let syncs = step.syncs() && !accounts_sync_off(Some(&config));
    if syncs
        && sync_called_for(home, state_dir, (roster_path, &config), now, fx.rediscovery)
        && let Some((Err(why), true)) = periodic_sync(home, state_dir, (roster_path, now), fx)
    {
        (fx.log)(&format!("accounts sync: {why}"));
    }
    if step.syncs() {
        heal_default_login_in_upkeep(home, state_dir, roster_path, now, fx);
    }
    // A saved explicit choice is independent of automatic fleet-source admission.
    let pending = step
        .renews()
        .then(|| resume_pending_pin(home, state_dir, roster_path, now, fx))
        .flatten();
    // The ladder signs rows in by default; a person's own `[harness] logins = "off"` or
    // `enabled = false` turns it off (a saved explicit choice above still completes).
    let signs_in = step.renews() && sign_ins_on(&config);
    let Some(source) = configured_source(Some(&config), Some(roster_path)) else {
        // A roster written by hand or kept from before is signed in like a fetched one: the
        // ladder needs only its rows, never a source.
        let renewal = pending.map_or_else(
            || {
                signs_in
                    && roster_path.is_file()
                    && auto_renew_next_account(home, state_dir, roster_path, now, fx)
            },
            RenewalDispatch::progressed,
        );
        return Upkept::Unconfigured {
            fleet_file_ignored: fleet_file_ignored(roster_path),
            renewal,
        };
    };
    let roster = if syncs {
        sync_if_missing(home, state_dir, roster_path, &config, now, fx)
    } else {
        roster_path.exists()
    };
    let renewal = pending.map_or_else(
        || signs_in && auto_renew_next_account(home, state_dir, roster_path, now, fx),
        RenewalDispatch::progressed,
    );
    Upkept::Configured {
        kind: source.kind(),
        roster,
        renewal,
    }
}

/// Whether the config at `config` lets the harness sign rows in by itself: the harness on
/// (`[harness] enabled`), accounts not turned off (`[harness] accounts`), and its login lifecycle
/// on (`[harness] logins`).
fn sign_ins_on(config: &Path) -> bool {
    sign_ins_off(config).is_none()
}

/// The switch in the config at `config` that keeps the harness from signing rows in by itself
/// ([`sign_ins_on`]), in the words that set it, where one does: `aterm ai` then leaves a
/// row's sign-in to a person, as the upkeep does.
pub(crate) fn sign_ins_off(config: &Path) -> Option<&'static str> {
    let (cfg, _) = crate::supervise::config::SupervisorConfig::from_path(Some(config));
    if !cfg.enabled {
        Some("`[harness] enabled = false`")
    } else if cfg.accounts == accounts::Mode::Off {
        Some("`[harness] accounts = \"off\"`")
    } else if !cfg.logins {
        Some("`[harness] logins = \"off\"`")
    } else {
        None
    }
}

/// Why a sync [`accounts_sync_off`] holds back reads nothing.
pub const ACCOUNTS_SYNC_OFF: &str = "`[harness] accounts = \"off\"` turns the accounts' sync off";

/// THE ACCOUNTS' OFF SWITCH (`docs/DESIGN-init-on-by-default-2026-10-25.md` §8.5,
/// `docs/DESIGN-alab-swarm-2026-10-27.md` §9): whether the config at `config` says
/// `[harness] accounts = "off"`. Then no automatic sync or upkeep reads a source, writes a roster,
/// or starts a sign-in of any pool: not the window's upkeep ([`upkeep_live`]), the setup's resync
/// ([`resync_for_setup`]), [`sync_accounts_for_init`] or [`sync_cli_accounts_for_init`] (what
/// `aterm init`'s `base.ai.accounts` and `aterm secrets join` run), nor the converge pass's
/// `sync.accounts`. A person's own `aterm harness accounts sync` still syncs, and while accounts
/// are not `"off"`, `[harness] logins` switches the sign-in ladder alone ([`sign_ins_on`]).
#[must_use]
pub fn accounts_sync_off(config: Option<&Path>) -> bool {
    crate::supervise::config::SupervisorConfig::from_path(config)
        .0
        .accounts
        == accounts::Mode::Off
}

/// THE ONE RULE OF WHEN A PERIODIC CALLER SYNCS a roster that stands (a window's upkeep, the
/// setup pass's [`resync_for_setup`]): when its inputs changed or need revalidation. That is
/// `~/.secrets` holding a fleet, a sheet or a mailbox where no source is configured yet; a vault
/// fleet or sheet the fleet file does not match, or accounts this machine's own logins show that
/// a fleet discovery made does not list ([`init_sync_needed`], read every [`REDISCOVER_EVERY`]);
/// the default login holding an account the roster's default row is not
/// ([`default_login_unlisted`]); or a `[fleet] accounts_source` that is a local file this process
/// needs to read again ([`key_file_changed`]). A sync that runs and finds nothing new
/// writes nothing ([`persist_accounts_sync`]); one that would read just what the last failed
/// one read waits ([`periodic_sync`]).
fn sync_called_for(
    home: &Path,
    state_dir: &Path,
    (roster_path, config): (&Path, &Path),
    now: i64,
    rediscovery: &std::sync::Mutex<Rediscovery>,
) -> bool {
    if accounts::Store::at(state_dir).sync_needs_recovery() {
        return true;
    }
    let secrets = home.join(".secrets");
    let configured = configured_source(Some(config), Some(roster_path));
    // A mode change invalidates this process's observation. Returning to the same path
    // must not inherit the timer from a source which stopped being authoritative.
    let key_changed = if matches!(configured, Some(FleetSource::Key(_))) {
        key_file_changed(home, state_dir, config, now, rediscovery)
    } else {
        rediscovery
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .key_attempt = None;
        false
    };
    let unconfigured_with_secrets = configured.is_none()
        && (has_fleet_source(&secrets)
            || [
                super::headless_oauth::SECRETS_MAILBOX_TOML_FILE,
                super::headless_oauth::MAILBOX_TOML_FILE,
            ]
            .into_iter()
            .any(|name| secrets.join(name).is_file())
            || (secrets.is_dir()
                && super::headless_oauth::discover_mailbox_config_files(
                    Some(home),
                    None,
                    Some(config),
                )
                .is_some()));
    // A roster stays up with this machine's own logins: a default login the vendor's sign-in
    // moved to another account is synced for at this upkeep (its displaced row is then signed in
    // like any other), and a fleet discovery made takes the accounts discovery finds, read again
    // every REDISCOVER_EVERY.
    unconfigured_with_secrets
        || init_sync_needed(
            home,
            state_dir,
            roster_path,
            now,
            rediscover_due(rediscovery),
        )
        || default_login_unlisted(home, state_dir, (roster_path, config), now, rediscovery)
        || key_changed
}

/// THE ONE RETRY RULE of a periodic caller's sync, once a missing roster or [`sync_called_for`]
/// calls for one: a sync whose inputs ([`sync_inputs`]) are those of the last one that failed
/// waits [`REDISCOVER_EVERY`] from that attempt, so a sheet that names no account of this
/// machine is not read again at every upkeep, while changed inputs (an edited sheet, a roster or
/// fleet file written since, another default login or `[fleet] machine`) are read at once. A
/// failure because another process held the accounts lock holds nothing back, and the recovery
/// of a pending local commit is never held. Returns what the sync did and whether its failure is
/// news ([`note_sync_result`]), which a caller logs; none while it waits.
fn periodic_sync(
    home: &Path,
    state_dir: &Path,
    (roster_path, now): (&Path, i64),
    fx: &UpkeepEffects<'_>,
) -> Option<(Result<InitSynced, String>, bool)> {
    let inputs = sync_inputs(home, state_dir, roster_path, now);
    let recovering = accounts::Store::at(state_dir).sync_needs_recovery();
    if !recovering
        && fx
            .rediscovery
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .failed
            .as_ref()
            .is_some_and(|failed| failed.holds_back(&inputs))
    {
        return None;
    }
    let synced = sync_accounts_for_init_with(home, state_dir, roster_path, now, || {}, fx.fetch);
    let news = note_sync_result(
        &mut fx
            .rediscovery
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .failed,
        inputs,
        synced.as_ref().map(|_| ()).map_err(String::as_str),
    );
    Some((synced, news))
}

/// One digest of what a sync from init reads that a periodic caller reads cheaply
/// ([`periodic_sync`]): the fleet, the sheet and the mailbox files in `~/.secrets`, whether the
/// vault wrote that fleet ([`own_secrets_copy`]), the roster, the fleet file beside it, the
/// config (`[fleet] machine`, `accounts_source`) and a local file that source names, and the
/// default login's address. What else discovery reads (logins, transcripts, other mailbox
/// configurations), a URL and the host's name are left out: [`REDISCOVER_EVERY`] reads them
/// again.
fn sync_inputs(home: &Path, state_dir: &Path, roster_path: &Path, now: i64) -> [u8; 32] {
    let config = config_beside(roster_path);
    let env = quiet_env(home, state_dir, &config, now);
    let secrets = home.join(".secrets");
    let mut hash = aterm_digest::Sha256::new();
    hash.update(b"aterm-accounts-sync-inputs-v1");
    let mut field = |value: Option<&[u8]>| match value {
        None => hash.update([0]),
        Some(value) => {
            hash.update([1]);
            hash.update((value.len() as u64).to_be_bytes());
            hash.update(value);
        }
    };
    let secrets_pool = secrets.join(super::ai_pool::AI_POOL_FILE);
    for path in [
        Some(secrets_pool.clone()),
        Some(secrets.join(FLEET_ROSTER_FILE)),
        Some(secrets.join(ACCOUNTS_CSV_FILE)),
        Some(secrets.join(super::headless_oauth::SECRETS_MAILBOX_TOML_FILE)),
        Some(secrets.join(super::headless_oauth::MAILBOX_TOML_FILE)),
        Some(roster_path.to_path_buf()),
        fleet_file_beside(roster_path),
        Some(config.clone()),
        key_source_file(&env),
    ] {
        field(
            path.and_then(|path| read_local_source_text(&path).ok())
                .as_deref()
                .map(str::as_bytes),
        );
    }
    if pool_entry_present(&secrets_pool) {
        let id_bytes = super::ai_pool::resolve_local_pool_identity(Some(home), Some(&config), None)
            .map(|id| format!("{}:{}", id.primary, id.aliases.join(",")).into_bytes());
        field(id_bytes.as_deref());
    }
    let delivered =
        vault_delivered(home).map(|delivered| delivered.contains(&secrets.join(FLEET_ROSTER_FILE)));
    field(Some(match delivered {
        Some(true) => b"vault".as_slice(),
        Some(false) => b"local".as_slice(),
        None => b"unread".as_slice(),
    }));
    field(
        read_default_profile_email(&env)
            .as_deref()
            .map(str::as_bytes),
    );
    hash.finalize()
}

/// Whether a configured local source needs an attempt: on its first observation, changed
/// metadata (including unavailable metadata), or after `REDISCOVER_EVERY`. Metadata is only a
/// scheduling hint: unchanged metadata cannot suppress failed reads or same-stamp edits forever.
/// A URL is read only when the roster is missing or another existing trigger calls for a sync.
fn key_file_changed(
    home: &Path,
    state_dir: &Path,
    config: &Path,
    now: i64,
    rediscovery: &std::sync::Mutex<Rediscovery>,
) -> bool {
    let path = key_source_file(&quiet_env(home, state_dir, config, now));
    let version = path.map(|path| {
        let stamp = fs::metadata(&path)
            .and_then(|meta| meta.modified().map(|modified| (modified, meta.len())))
            .ok();
        (path, stamp)
    });
    let mut held = rediscovery
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(version) = version {
        schedule_local_source(&mut held.key_attempt, version, Instant::now())
    } else {
        held.key_attempt = None;
        false
    }
}

/// The local file `[fleet] accounts_source` in `env`'s config names, where it names one.
fn key_source_file(env: &Env) -> Option<PathBuf> {
    let env = configured_source_env(env).ok()?;
    let source = source_key(env.config.as_deref()?)?;
    match fetch_plan(&env, &source) {
        Ok(FetchPlan::File(path)) => Some(path),
        _ => None,
    }
}

type LocalSourceVersion = (PathBuf, Option<(std::time::SystemTime, u64)>);

#[derive(Debug)]
struct LocalSourceAttempt {
    version: LocalSourceVersion,
    at: Instant,
}

/// Record attempts, never success. Quiet polls keep the deadline anchored to the last attempt.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSourceRefresh",
        action = "Poll",
        project = "aterm_agent::harness::accounts_sync::source_refresh_tests::refresh_transition"
    )
)]
fn schedule_local_source(
    last: &mut Option<LocalSourceAttempt>,
    version: LocalSourceVersion,
    now: Instant,
) -> bool {
    if last.as_ref().is_some_and(|last| {
        last.version == version && now.saturating_duration_since(last.at) < REDISCOVER_EVERY
    }) {
        return false;
    }
    *last = Some(LocalSourceAttempt { version, at: now });
    true
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountSourceRefresh",
        action = "Edit",
        reason = "An external writer changes the source observation between polls."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountSourceRefresh",
        action = "Disappear",
        reason = "The environment makes source metadata unavailable between polls."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountSourceRefresh",
        action = "Elapse",
        reason = "The environment advances monotonic time independently of polling."
    )
)]
#[doc(hidden)]
pub fn account_source_refresh_environment() {}

fn config_beside(roster: &Path) -> PathBuf {
    roster
        .parent()
        .map(|p| p.join("aterm.toml"))
        .unwrap_or_else(|| PathBuf::from("aterm.toml"))
}

fn sync_if_missing(
    home: &Path,
    state_dir: &Path,
    roster_path: &Path,
    config: &Path,
    now: i64,
    fx: &UpkeepEffects<'_>,
) -> bool {
    if roster_path.exists() {
        return true;
    }
    {
        let mut guard = fx
            .sync_cooldown
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if guard.is_some_and(|last| last.elapsed() < Duration::from_secs(600)) {
            return false;
        }
        *guard = Some(Instant::now());
    }
    let env = Env {
        state: state_dir.to_path_buf(),
        aterm_state: state_dir
            .parent()
            .and_then(Path::parent)
            .map(Path::to_path_buf),
        cwd: home.to_path_buf(),
        home: Some(home.to_path_buf()),
        claude_dir: Some(home.join(".claude")),
        claude_config_unusable: None,
        claude_profile: Some(home.join(".claude.json")),
        claude_custom_oauth: false,
        claude_session: None,
        claude_pid: None,
        codex_thread: None,
        agy_conversation: None,
        codex_home: None,
        codex_login: None,
        now,
        tz_s: 0,
        sid: String::new(),
        sock: None,
        config: Some(config.to_path_buf()),
        claude_tmp: None,
        claude_running: None,
    };
    let mut out = Vec::new();
    let mut err = Vec::new();
    let how = SyncHow {
        fetch: fx.fetch,
        ladder: &accounts::LOCK_LADDER,
        fleet_snapshots: &[],
        heal: None,
        allow_empty_pool: false,
    };
    let _ = run_accounts_sync_with(&env, (None, None, true), (&mut out, &mut err), &how);
    roster_path.exists()
}

/// Whether a fleet accounts source is configured or discoverable for [`sync_accounts_for_init`].
/// An unusable expected source also returns true so setup can report its read error.
#[must_use]
pub fn has_init_accounts_source(
    home: &Path,
    state_dir: &Path,
    roster_path: &Path,
    now: i64,
) -> bool {
    let config_path = roster_path
        .parent()
        .map(|p| p.join("aterm.toml"))
        .unwrap_or_else(|| PathBuf::from("aterm.toml"));
    if configured_source(Some(&config_path), Some(roster_path)).is_some() {
        return true;
    }
    let secrets = home.join(".secrets");
    if has_fleet_source(&secrets)
        || [
            super::headless_oauth::SECRETS_MAILBOX_TOML_FILE,
            super::headless_oauth::MAILBOX_TOML_FILE,
        ]
        .into_iter()
        .any(|name| secrets.join(name).is_file())
    {
        return true;
    }
    if Roster::load(roster_path).is_ok_and(|r| !r.rows.is_empty())
        && roster_path
            .parent()
            .is_some_and(|p| source_entry_present(&p.join(FLEET_ROSTER_FILE)))
    {
        return true;
    }
    let has_discovered_secrets_mailbox = secrets.is_dir()
        && super::headless_oauth::discover_mailbox_config_files(
            Some(home),
            Some(&accounts::Store::at(state_dir).dir),
            Some(&config_path),
        )
        .is_some();
    let env = Env {
        state: state_dir.to_path_buf(),
        aterm_state: state_dir
            .parent()
            .and_then(Path::parent)
            .map(Path::to_path_buf),
        cwd: home.to_path_buf(),
        home: Some(home.to_path_buf()),
        claude_dir: Some(home.join(".claude")),
        claude_config_unusable: None,
        claude_profile: Some(home.join(".claude.json")),
        claude_custom_oauth: false,
        claude_session: None,
        claude_pid: None,
        codex_thread: None,
        agy_conversation: None,
        codex_home: None,
        codex_login: None,
        now,
        tz_s: 0,
        sid: String::new(),
        sock: None,
        config: Some(config_path),
        claude_tmp: None,
        claude_running: None,
    };
    let min_accounts = if has_discovered_secrets_mailbox { 1 } else { 2 };
    discover_local_fleet_with_min(&env, min_accounts).is_some()
}

/// Whether `~/.secrets/` holds an updated fleet or accounting sheet not yet converged into
/// `roster_path`, or, for a fleet discovery made ([`fleet_takes_discovered`]), local discovery
/// (a mailbox file's accounts among what it reads) finds an account the fleet does not list yet.
/// An unreadable or invalid selected source also needs a sync attempt to report its refusal.
#[must_use]
pub fn needs_init_accounts_sync(
    home: &Path,
    state_dir: &Path,
    roster_path: &Path,
    now: i64,
) -> bool {
    init_sync_needed(home, state_dir, roster_path, now, true)
}

/// [`needs_init_accounts_sync`]; `discover` false reads this machine's logins and transcripts
/// only where a mailbox file names accounts (a window's upkeep between [`REDISCOVER_EVERY`]s).
fn init_sync_needed(
    home: &Path,
    state_dir: &Path,
    roster_path: &Path,
    now: i64,
    discover: bool,
) -> bool {
    let secrets = home.join(".secrets");
    let Some(fb) = fleet_file_beside(roster_path) else {
        return false;
    };
    if !fleet_file_opt_in(&fb).is_some_and(OptIn::opts_in_by_file) {
        return false;
    }
    let secrets_pool = secrets.join(super::ai_pool::AI_POOL_FILE);
    let secrets_fleet = secrets.join(FLEET_ROSTER_FILE);
    let secrets_csv = secrets.join(ACCOUNTS_CSV_FILE);
    let has_secrets_pool = pool_entry_present(&secrets_pool);
    let has_secrets_fleet =
        source_entry_present(&secrets_fleet) && !superseded_secrets_copy(home, &secrets_fleet, &fb);
    let has_secrets_csv = source_entry_present(&secrets_csv);
    let config_path = config_beside(roster_path);
    if has_secrets_pool && source_key(&config_path).is_none() {
        let Ok(existing_text) = read_local_source_text(&fb) else {
            return true;
        };
        let Ok(existing_fleet) = FleetRoster::from_toml(&existing_text, FLEET_ROSTER_FILE) else {
            return true;
        };
        let Ok(raw) = read_local_source_text(&secrets_pool) else {
            return true;
        };
        let Ok(pool) = super::ai_pool::Pool::from_toml(&raw, super::ai_pool::AI_POOL_FILE) else {
            return true;
        };
        let Some(identity) =
            super::ai_pool::resolve_local_pool_identity(Some(home), Some(&config_path), None)
        else {
            return true;
        };
        let projected = project_pool_fleet(&pool, Cli::Claude, Some(&identity));
        if projected.accounts != existing_fleet.accounts {
            return true;
        }
        if !projected.accounts_for_machine(&identity.primary).is_empty() && !roster_path.exists() {
            return true;
        }
        return accounts::Store::at(state_dir)
            .load(now)
            .map(|(state, _)| {
                state.swarm_label.as_deref() != Some(identity.primary.as_str())
                    || state.swarm_aliases != identity.aliases
            })
            .unwrap_or(true);
    }
    if has_secrets_fleet {
        if own_secrets_copy(home, &secrets_fleet, &fb) {
            // aterm's own copy holds the fleet file's accounts, so a sync reads nothing new from
            // it; beside a sheet that gives this machine rows, a sync reads the sheet and
            // removes the copy.
            if has_secrets_csv && sheet_names_this_machine(home, Some(&config_path), &secrets_csv) {
                return true;
            }
        } else if !matches!(
            (read_local_source_text(&secrets_fleet), read_local_source_text(&fb)),
            (Ok(source), Ok(kept)) if source == kept
        ) {
            return true;
        }
    }
    let Ok(existing_text) = read_local_source_text(&fb) else {
        return true;
    };
    let Ok(existing_fleet) = FleetRoster::from_toml(&existing_text, "fleet-accounts.toml") else {
        return true;
    };
    let opt_in = opt_in_of(&existing_text);
    // A sync from the sheet writes its accounts as they are: any edit (a row added, removed, or
    // moved to another machine) is one the fleet file does not hold yet.
    if !has_secrets_fleet && has_secrets_csv {
        let Ok(csv_text) = read_local_source_text(&secrets_csv) else {
            return true;
        };
        let Ok(csv_fleet) = parse_accounting_csv(&csv_text, ACCOUNTS_CSV_FILE) else {
            return true;
        };
        if csv_fleet.accounts != existing_fleet.accounts {
            return true;
        }
    }
    // Discovery's accounts are added only to a fleet discovery made: a fleet from a vault, a
    // sheet or a URL stays as its source wrote it, and a person's list as they wrote it.
    if !fleet_of_this_machine(opt_in, &existing_fleet) {
        return false;
    }
    if !discover
        && ![
            super::headless_oauth::SECRETS_MAILBOX_TOML_FILE,
            super::headless_oauth::MAILBOX_TOML_FILE,
        ]
        .into_iter()
        .any(|name| secrets.join(name).is_file())
        && !(secrets.is_dir()
            && super::headless_oauth::discover_mailbox_config_files(
                Some(home),
                None,
                Some(&config_path),
            )
            .is_some())
    {
        return false;
    }
    let env = Env {
        state: state_dir.to_path_buf(),
        aterm_state: state_dir
            .parent()
            .and_then(Path::parent)
            .map(Path::to_path_buf),
        cwd: home.to_path_buf(),
        home: Some(home.to_path_buf()),
        claude_dir: Some(home.join(".claude")),
        claude_config_unusable: None,
        claude_profile: Some(home.join(".claude.json")),
        claude_custom_oauth: false,
        claude_session: None,
        claude_pid: None,
        codex_thread: None,
        agy_conversation: None,
        codex_home: None,
        codex_login: None,
        now,
        tz_s: 0,
        sid: String::new(),
        sock: None,
        config: Some(config_path),
        claude_tmp: None,
        claude_running: None,
    };
    let shown = local_shows(&env);
    fleet_takes_discovered(opt_in, &existing_fleet, || shown.clone())
        && discovered_fleet(&env, &shown, 1).is_some_and(|local| {
            local.accounts.iter().any(|acct| {
                !existing_fleet
                    .accounts
                    .iter()
                    .any(|a| accounts::email_matches(&a.email, &acct.email))
            })
        })
}

/// Adds to `existing_fleet` each account of `local` whose email it does not list, labelled from
/// its own machines and slot ([`discovered_label`], as [`local_account`] labels a default login
/// no row lists, so the two name one account alike) and made unique by `-2`, `-3`, ….
fn merge_discovered_accounts(
    existing_fleet: &mut FleetRoster,
    local: FleetRoster,
) -> Result<bool, String> {
    let mut used_labels: BTreeSet<String> = existing_fleet
        .accounts
        .iter()
        .map(|a| a.label.clone())
        .collect();
    let mut added = false;
    for mut acct in local.accounts {
        if !existing_fleet
            .accounts
            .iter()
            .any(|a| accounts::email_matches(&a.email, &acct.email))
        {
            let base_label =
                discovered_label(&acct.machines, email_machines_and_slot(&acct.email).1);
            acct.label = base_label.clone();
            let mut suffix = 2u32;
            loop {
                if let Some(why) = accounts::label_refusal(&acct.label) {
                    return Err(format!("discovered account label {} {why}", acct.label));
                }
                if used_labels.insert(acct.label.clone()) {
                    break;
                }
                acct.label = format!("{base_label}-{suffix}");
                suffix = suffix
                    .checked_add(1)
                    .ok_or_else(|| "discovered account label suffixes exhausted".to_owned())?;
            }
            existing_fleet.accounts.push(acct);
            added = true;
        }
    }
    Ok(added)
}

/// The fleet file at `path` as a sync from init reads it, with `merge`, the accounts local
/// discovery finds that it does not list added where discovery may add to it
/// ([`fleet_takes_discovered`]; it keeps the `opt_in` it records); and the snapshot that sync
/// checks it against, which publishes the merged text back to `path` where `publish` asks.
fn capture_init_fleet(
    env: &Env,
    path: &Path,
    merge: bool,
    publish: bool,
) -> Result<(String, FleetSnapshot), String> {
    let text = read_local_source_text(path).map_err(|why| format!("{}: {why}", path.display()))?;
    let mut answer = text.clone();
    let mut added = false;
    if merge {
        let mut fleet = FleetRoster::from_toml(&text, &path.display().to_string())?;
        let opt_in = opt_in_of(&text);
        let mut shown = None;
        if fleet_takes_discovered(opt_in, &fleet, || {
            shown.get_or_insert_with(|| local_shows(env)).clone()
        }) && let Some(local) =
            discovered_fleet(env, shown.get_or_insert_with(|| local_shows(env)), 1)
        {
            added = merge_discovered_accounts(&mut fleet, local)?;
            if added {
                answer = fleet.to_toml_for(opt_in);
            }
        }
    }
    Ok((
        answer,
        FleetSnapshot {
            path: path.to_path_buf(),
            bytes: Some(text.into_bytes()),
            publish: if publish && added {
                Publish::Fleet
            } else {
                Publish::No
            },
        },
    ))
}

/// Synchronize the machine and fleet account rosters during `aterm init` or `aterm secrets`:
/// the machine, its rows, those enrolled and not signed out, and the fleet's rows.
/// # Errors
/// [`accounts_sync_off`] holds ([`ACCOUNTS_SYNC_OFF`]: nothing is read or written), no source
/// was found, or the sync refused, as [`run_accounts_sync`] says.
pub fn sync_accounts_for_init(
    home: &Path,
    state_dir: &Path,
    roster_path: &Path,
    now: i64,
) -> Result<(String, usize, usize, usize), String> {
    sync_accounts_for_init_with(home, state_dir, roster_path, now, || {}, &fetch_source_text)
        .map(|synced| (synced.machine, synced.rows, synced.enrolled, synced.fleet))
}

/// What a sync from init made of this machine's roster ([`sync_accounts_for_init_with`]).
#[derive(Debug)]
struct InitSynced {
    machine: String,
    rows: usize,
    /// Rows enrolled here and not signed out.
    enrolled: usize,
    fleet: usize,
    /// Whether the sync changed anything ([`SyncedAccounts::changed`]).
    changed: bool,
    /// The rows it left with no login made here (its `needs_renew`), which the automatic
    /// sign-in ladder signs in ([`resync_with`]).
    to_sign_in: Vec<String>,
}

/// [`sync_accounts_for_init`], running `before_sync` once its sources are captured and reading
/// any source it did not capture (a `[fleet] accounts_source`) with `fetch`.
fn sync_accounts_for_init_with(
    home: &Path,
    state_dir: &Path,
    roster_path: &Path,
    now: i64,
    before_sync: impl FnOnce(),
    fetch_uncaptured: &Fetch<'_>,
) -> Result<InitSynced, String> {
    if accounts_sync_off(Some(&config_beside(roster_path))) {
        return Err(ACCOUNTS_SYNC_OFF.to_owned());
    }
    let recovered_commit =
        accounts::Store::at(state_dir).recover_sync_within(&sync_lock_ladder(), roster_path)?;
    let config_path = roster_path
        .parent()
        .map(|p| p.join("aterm.toml"))
        .unwrap_or_else(|| PathBuf::from("aterm.toml"));
    let env = Env {
        state: state_dir.to_path_buf(),
        aterm_state: state_dir
            .parent()
            .and_then(Path::parent)
            .map(Path::to_path_buf),
        cwd: home.to_path_buf(),
        home: Some(home.to_path_buf()),
        claude_dir: Some(home.join(".claude")),
        claude_config_unusable: None,
        claude_profile: Some(home.join(".claude.json")),
        claude_custom_oauth: false,
        claude_session: None,
        claude_pid: None,
        codex_thread: None,
        agy_conversation: None,
        codex_home: None,
        codex_login: None,
        now,
        tz_s: 0,
        sid: String::new(),
        sock: None,
        config: Some(config_path),
        claude_tmp: None,
        claude_running: None,
    };
    let fleet_path = roster_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(FLEET_ROSTER_FILE);
    let has_named_fleet = fleet_file_opt_in(&fleet_path).is_some_and(OptIn::opts_in_by_file);
    let has_key = env.config.as_deref().and_then(source_key).is_some();
    let secrets_dir = home.join(".secrets");
    let secrets_pool = secrets_dir.join(super::ai_pool::AI_POOL_FILE);
    let secrets_fleet = secrets_dir.join(FLEET_ROSTER_FILE);
    let secrets_sheet = secrets_dir.join(ACCOUNTS_CSV_FILE);
    // The sheet outranks the copy aterm keeps there of the fleet file beside the roster where it
    // gives this machine rows, and the sync that reads the sheet removes that copy, so a sheet
    // edited later reaches the roster. Where it gives none, the copy stays this Mac's own list.
    let own_copy = source_entry_present(&secrets_fleet)
        && source_entry_present(&secrets_sheet)
        && own_secrets_copy(home, &secrets_fleet, &fleet_path)
        && sheet_names_this_machine(home, env.config.as_deref(), &secrets_sheet);
    let superseded = superseded_secrets_copy(home, &secrets_fleet, &fleet_path);
    let secrets_source = pool_entry_present(&secrets_pool)
        .then_some(&secrets_pool)
        .or_else(|| {
            [&secrets_fleet, &secrets_sheet]
                .into_iter()
                .filter(|path| !((own_copy || superseded) && *path == &secrets_fleet))
                .find(|path| source_entry_present(path))
        })
        .map(|path| path.to_string_lossy().into_owned());
    let mut captured_fleet = None;
    let mut fleet_snapshots = Vec::new();
    let init_source = if has_key && !has_named_fleet {
        None
    } else if let Some(src) = secrets_source {
        let (text, snapshot) = capture_init_fleet(
            &env,
            Path::new(&src),
            src.ends_with(FLEET_ROSTER_FILE),
            true,
        )?;
        captured_fleet = Some((src.clone(), text));
        fleet_snapshots.push(snapshot);
        if own_copy && src == secrets_sheet.to_string_lossy() {
            let copy = read_local_source_text(&secrets_fleet)
                .map_err(|why| format!("{}: {why}", secrets_fleet.display()))?;
            fleet_snapshots.push(FleetSnapshot {
                path: secrets_fleet.clone(),
                bytes: Some(copy.into_bytes()),
                publish: Publish::Remove,
            });
        }
        Some(src)
    } else if configured_source(env.config.as_deref(), Some(roster_path)).is_some() {
        if has_named_fleet {
            let (text, snapshot) = capture_init_fleet(&env, &fleet_path, true, false)?;
            captured_fleet = Some((fleet_path.to_string_lossy().into_owned(), text));
            fleet_snapshots.push(snapshot);
        }
        None
    } else if source_entry_present(&fleet_path) {
        let source = fleet_path.to_string_lossy().into_owned();
        let (text, snapshot) = capture_init_fleet(&env, &fleet_path, false, false)?;
        captured_fleet = Some((source.clone(), text));
        fleet_snapshots.push(snapshot);
        Some(source)
    } else {
        let min_accounts = if [
            super::headless_oauth::SECRETS_MAILBOX_TOML_FILE,
            super::headless_oauth::MAILBOX_TOML_FILE,
        ]
        .into_iter()
        .any(|name| home.join(".secrets").join(name).is_file())
            || (home.join(".secrets").is_dir()
                && super::headless_oauth::discover_mailbox_config_files(
                    Some(home),
                    Some(&accounts::Store::at(state_dir).dir),
                    env.config.as_deref(),
                )
                .is_some())
        {
            1
        } else {
            2
        };
        discover_local_fleet_with_min(&env, min_accounts).map(|local| {
            let source = fleet_path.to_string_lossy().into_owned();
            captured_fleet = Some((source.clone(), local.to_toml_for(Some(OptIn::Discovered))));
            fleet_snapshots.push(FleetSnapshot {
                path: fleet_path.clone(),
                bytes: None,
                publish: Publish::No,
            });
            source
        })
    };
    // aterm copies into `~/.secrets` only a list it found on this Mac (`discovered`), never one a
    // source wrote (the vault's sheet or fleet there, a sheet since withdrawn, a URL, a file a
    // person named): `~/.secrets` is the vault's folder, and a copy there of another source's
    // list could only stand beside that source and outrank it.
    let discovered = captured_fleet
        .as_ref()
        .is_some_and(|(_, text)| opt_in_of(text) == Some(OptIn::Discovered));
    if discovered && secrets_dir.is_dir() && !source_entry_present(&secrets_fleet) {
        fleet_snapshots.push(FleetSnapshot {
            path: secrets_fleet,
            bytes: None,
            publish: Publish::Fleet,
        });
    }
    before_sync();
    let fetch = move |env: &Env, source: &str| {
        if let Some((path, text)) = &captured_fleet
            && source == path
        {
            Ok(text.clone())
        } else {
            fetch_uncaptured(env, source)
        }
    };
    let report = sync_accounts_with(
        &env,
        init_source.as_deref(),
        None,
        &SyncHow {
            fetch: &fetch,
            ladder: &sync_lock_ladder(),
            fleet_snapshots: &fleet_snapshots,
            heal: None,
            allow_empty_pool: true,
        },
    )?;
    let synced = report.into_init(recovered_commit);
    if synced.changed {
        let _ = accounts::Store::at(state_dir).touch_wake();
    }
    Ok(synced)
}

/// What one periodic resync of this machine's accounts did ([`resync_for_setup`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Resync {
    /// An account sync ran: Claude Code's roster was missing with a source to read (at most
    /// once per 10 minutes), its inputs changed or were due for revalidation
    /// ([`sync_called_for`], subject to [`periodic_sync`]'s failed-input hold), or an enabled
    /// other CLI's sync read its fleet, even if no rows needed adding.
    pub synced: bool,
    /// Why Claude Code's sync failed. Recovery may have completed a previous commit before this
    /// attempt refused its source or encountered another publication error.
    pub failed: Option<String>,
    /// It changed a file, the account state or the ledger. A sync that finds what is already
    /// there writes nothing.
    pub changed: bool,
    /// Claude Code's rows it left with no login made here, for the automatic sign-in ladder.
    pub to_sign_in: Vec<String>,
    /// The ladder started a sign-in, or a saved account choice progressed.
    pub renewal: bool,
    /// Each other CLI whose sync failed, with why.
    pub cli_failed: Vec<(Cli, String)>,
}

/// THE SETUP PASS'S RESYNC (`aterm`'s first-run setup runs it every pass it retries; nothing at
/// all where [`accounts_sync_off`] holds): Claude
/// Code's sync where the roster is missing with a source to read or its inputs changed or need
/// revalidation ([`sync_called_for`], the rule a window's upkeep keeps), each other CLI's
/// (which writes only the rows it adds), and, where the sync leaves rows with no login made here, `sign_in` allows
/// (an instance that keeps this account's own fleet accounts) and the config does not turn
/// sign-ins off ([`sign_ins_on`]), the automatic sign-in ladder at once
/// ([`auto_renew_next_account`]): each attempt, and what it found where no route could sign a
/// row in, is on record as a window's are, and no person is asked.
#[must_use]
pub fn resync_for_setup(
    home: &Path,
    state_dir: &Path,
    roster_path: &Path,
    now: i64,
    sign_in: bool,
) -> Resync {
    resync_with(
        home,
        state_dir,
        (roster_path, now, sign_in),
        &live_effects(),
    )
}

/// The sources a missing Claude Code roster's sync can actually read. A sync from init also
/// reads an unannotated local fleet beside the roster ([`FleetOrigin::Local`]), even though
/// that file by itself opts no window into the fleet accounts feature.
fn has_claude_resync_source(home: &Path, state_dir: &Path, roster_path: &Path, now: i64) -> bool {
    has_init_accounts_source(home, state_dir, roster_path, now)
        || fleet_file_beside(roster_path).is_some_and(|fleet| source_entry_present(&fleet))
}

/// Whether `ai-pool.toml` is the active Claude Code source, lists no Claude accounts for this
/// machine, this machine has no default `.claude.json` email, and `fleet-accounts.toml` plus
/// the store's swarm identity are already in sync (`sync_accounts_with` with `allow_empty_pool`
/// intentionally leaves `roster_path` absent in that state).
fn empty_claude_pool_settled(home: &Path, state_dir: &Path, roster_path: &Path, now: i64) -> bool {
    let config_path = config_beside(roster_path);
    let secrets_pool = home.join(".secrets").join(super::ai_pool::AI_POOL_FILE);
    if !pool_entry_present(&secrets_pool) || source_key(&config_path).is_some() {
        return false;
    }
    let Some(fb) = fleet_file_beside(roster_path) else {
        return false;
    };
    if !fleet_file_opt_in(&fb).is_some_and(OptIn::opts_in_by_file) {
        return false;
    }
    let env = quiet_env(home, state_dir, &config_path, now);
    if read_default_profile_email(&env).is_some() {
        return false;
    }
    !init_sync_needed(home, state_dir, roster_path, now, false)
}

fn claude_roster_missing(home: &Path, state_dir: &Path, roster_path: &Path, now: i64) -> bool {
    !roster_path.exists()
        && has_claude_resync_source(home, state_dir, roster_path, now)
        && !empty_claude_pool_settled(home, state_dir, roster_path, now)
}

fn resync_with(
    home: &Path,
    state_dir: &Path,
    (roster_path, now, sign_in): (&Path, i64, bool),
    fx: &UpkeepEffects<'_>,
) -> Resync {
    let config = config_beside(roster_path);
    if accounts_sync_off(Some(&config)) {
        return Resync::default();
    }
    let missing = claude_roster_missing(home, state_dir, roster_path, now) && {
        let mut last = fx
            .sync_cooldown
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let due = last.is_none_or(|at| at.elapsed() >= Duration::from_secs(600));
        if due {
            *last = Some(Instant::now());
        }
        due
    };
    let mut done = Resync::default();
    if (missing || sync_called_for(home, state_dir, (roster_path, &config), now, fx.rediscovery))
        && let Some((synced, _)) = periodic_sync(home, state_dir, (roster_path, now), fx)
    {
        done.synced = true;
        match synced {
            Ok(synced) => {
                done.changed = synced.changed;
                done.to_sign_in = synced.to_sign_in;
            }
            Err(why) => done.failed = Some(why),
        }
    }
    for (cli, synced) in sync_cli_accounts_with(home, state_dir, roster_path, now, fx.fetch) {
        match synced {
            Ok(CliSync::Synced(synced)) => {
                done.synced = true;
                done.changed |= !synced.added.is_empty();
            }
            Ok(
                CliSync::Off | CliSync::Left { .. } | CliSync::NoSource | CliSync::Unlisted { .. },
            ) => {}
            Err(why) => done.cli_failed.push((cli, why)),
        }
    }
    if sign_in && sign_ins_on(&config) && !done.to_sign_in.is_empty() {
        done.renewal = auto_renew_next_account(home, state_dir, roster_path, now, fx);
    }
    done
}

/// What [`resync_due`] has read in this process: kept apart from the syncs' own
/// ([`REDISCOVERY`]), so a read never uses up a sync's turn, and kept across a converge pass's
/// probe and its read-back, so the read after a sync reads what that sync left.
static RESYNC_READ: std::sync::Mutex<Rediscovery> = std::sync::Mutex::new(Rediscovery {
    read_at: None,
    synced_for: None,
    key_attempt: None,
    unclaimed: None,
    failed: None,
    default_held: BTreeMap::new(),
});

/// Whether [`resync_for_setup`] has a source to read: Claude Code's configured or discovered
/// source ([`has_init_accounts_source`]), or another CLI's fleet where its pool syncs by default
/// ([`syncs_by_default`]) and its default admission takes rows, or this machine already keeps
/// its roster. A vault file alone opts no machine into a pool that does not sync by default;
/// a default pool this machine left or whose fleet does not list it stays out.
/// An interrupted sync's recovery counts first, as it runs before that admission.
/// An unusable selected source also counts so [`resync_due`] can report its refusal.
#[must_use]
pub fn has_resync_accounts_source(
    home: &Path,
    state_dir: &Path,
    roster_path: &Path,
    now: i64,
) -> bool {
    if has_init_accounts_source(home, state_dir, roster_path, now) {
        return true;
    }
    let config = config_beside(roster_path);
    let env = quiet_env(home, state_dir, &config, now);
    OTHER_CLIS.into_iter().any(|cli| {
        let Some(roster) = accounts::roster_beside_for(&config, cli) else {
            return false;
        };
        let by_default = !roster.is_file();
        if by_default && !syncs_by_default(cli) {
            return false;
        }
        let store = accounts::Store::at_for(state_dir, cli, home);
        if store.sync_needs_recovery() {
            return true;
        }
        match load_cli_fleet_source(cli, &env, home, None, None, &fetch_source_text) {
            Ok(None) => false,
            Ok(Some((fleet, pool_ctx))) if by_default => {
                let machine = pool_ctx.as_ref().map_or_else(
                    || cli_sync_machine(&config, None),
                    |ctx| Some(ctx.identity.primary.clone()),
                );
                !matches!(by_default_admits(&store, &fleet, machine, now), Ok(Err(_)))
            }
            Ok(Some(_)) | Err(_) => true,
        }
    })
}

/// Why [`resync_for_setup`] would sync this machine's accounts now, read with nothing synced or
/// written (the converge pass's `sync.accounts` probe), or `None` where it would sync nothing.
/// THE SAME RULE as its sync: nothing where [`accounts_sync_off`] holds; Claude Code's roster
/// where it is missing with a source to read or [`sync_called_for`] calls for a sync; and each
/// other CLI's pool whose sync would add a row ([`cli_sync_due`]), or whose sync would refuse
/// (a read of its fleet or roster that fails, which the sync reports). A process's first read reads every input; a later
/// one, as a sync's does, reads again what changed.
#[must_use]
pub fn resync_due(home: &Path, state_dir: &Path, roster_path: &Path, now: i64) -> Option<String> {
    resync_due_with(home, state_dir, roster_path, now, &RESYNC_READ)
}

fn resync_due_with(
    home: &Path,
    state_dir: &Path,
    roster_path: &Path,
    now: i64,
    rediscovery: &std::sync::Mutex<Rediscovery>,
) -> Option<String> {
    let config = config_beside(roster_path);
    if accounts_sync_off(Some(&config)) {
        return None;
    }
    // Read whether or not the roster stands, so the read after a sync that wrote it compares
    // with this one rather than reading every input as new.
    let called = sync_called_for(home, state_dir, (roster_path, &config), now, rediscovery);
    let mut why = Vec::new();
    if claude_roster_missing(home, state_dir, roster_path, now) {
        why.push("the roster is missing".to_owned());
    } else if called {
        why.push("a source it is made from changed or is due a fresh read".to_owned());
    }
    for cli in OTHER_CLIS {
        match cli_sync_due(cli, home, state_dir, &config, now) {
            Ok(added) if added.is_empty() => {}
            Ok(added) => why.push(format!(
                "{}'s pool has accounts to add ({})",
                cli.id(),
                added.join(", ")
            )),
            Err(error) => why.push(format!("{}'s pool cannot be synced: {error}", cli.id())),
        }
    }
    (!why.is_empty()).then(|| why.join("; "))
}

/// The labels a sync of `cli`'s pool ([`sync_cli_with`], as [`resync_for_setup`] runs it, with
/// no source named) would add to its roster beside `config`, read with nothing written: none
/// where its pool neither syncs by default ([`syncs_by_default`]) nor has a kept roster,
/// no fleet of it is there, or its default admission leaves it out ([`by_default_admits`]);
/// this machine's fleet rows no roster row matches by identity and none holds by label, as
/// the sync adds them. A store whose last sync left a commit to recover reads due first
/// (its own `recovery`), as the sync recovers before reading its fleet or default admission.
///
/// # Errors
/// As the sync's reads: the fleet, roster or pool's store is refused, or a kept roster's
/// machine id is unknown.
fn cli_sync_due(
    cli: Cli,
    home: &Path,
    state_dir: &Path,
    config: &Path,
    now: i64,
) -> Result<Vec<String>, String> {
    let roster_path = accounts::roster_beside_for(config, cli)
        .ok_or_else(|| format!("{} has no directory", config.display()))?;
    let by_default = !roster_path.is_file();
    if by_default && !syncs_by_default(cli) {
        return Ok(Vec::new());
    }
    let store = accounts::Store::at_for(state_dir, cli, home);
    if store.sync_needs_recovery() {
        return Ok(vec!["recovery".to_owned()]);
    }
    let env = quiet_env(home, state_dir, config, now);
    let Some((fleet, pool_ctx)) =
        load_cli_fleet_source(cli, &env, home, None, None, &fetch_source_text)?
    else {
        return Ok(Vec::new());
    };
    let machine = pool_ctx.as_ref().map_or_else(
        || cli_sync_machine(config, None),
        |ctx| Some(ctx.identity.primary.clone()),
    );
    let machine = if by_default {
        match by_default_admits(&store, &fleet, machine, now)? {
            Ok(machine) => machine,
            Err(_) => return Ok(Vec::new()),
        }
    } else {
        machine.ok_or_else(|| {
            "could not determine machine id (`m<N>`); pass `--machine m8`".to_owned()
        })?
    };
    let (read_from, _) = super::cli::roster_target(&roster_path)?;
    let existing_text = accounts::roster_text(&read_from)?.unwrap_or_default();
    let existing = Roster::parse_for(cli, &existing_text)
        .map_err(|why| format!("the roster at {} is refused: {why}", roster_path.display()))?;
    let added: Vec<String> = fleet
        .roster
        .accounts_for_machine(&machine)
        .into_iter()
        .filter(|acct| {
            !existing
                .rows
                .iter()
                .any(|row| fleet_matches_identity(cli, row, acct))
                && existing.row(&acct.label).is_none()
        })
        .map(|acct| acct.label.clone())
        .collect();
    Ok(added)
}

/// The accounting sheet's name in `~/.secrets` (`DESIGN-secrets-2026-10-20.md` §11).
pub const ACCOUNTS_CSV_FILE: &str = "ai-accounts.csv";

/// The pooled CLIs other than Claude Code, whose pools [`sync_cli_accounts_for_init`] and the
/// window's upkeep ([`upkeep_live`]) keep beside Claude Code's.
pub const OTHER_CLIS: [Cli; 3] = [Cli::Codex, Cli::Grok, Cli::Kimi];

/// Every pooled CLI takes this machine's rows from the vault's fleet with no roster kept here.
/// The source supplies the preferred-machine mapping; a local roster is not an opt-in requirement.
/// `[harness] accounts = "off"` ([`accounts_sync_off`]) still stops every automatic sync.
#[must_use]
pub fn syncs_by_default(cli: Cli) -> bool {
    matches!(cli, Cli::Codex | Cli::Grok | Cli::Kimi)
}

/// The fleet file of `cli`'s pool in `~/.secrets` (`ai-accounts-codex.toml`,
/// `ai-accounts-grok.toml`, `ai-accounts-kimi.toml`): the vault's subscriber writes it, and
/// aterm only reads it ([`sync_cli_with`]).
#[must_use]
pub fn cli_fleet_file(cli: Cli) -> String {
    format!("ai-accounts-{}.toml", cli.id())
}

/// What a sync of another CLI's pool did ([`sync_cli_with`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliSync {
    /// Nothing: this machine keeps no roster of the CLI's and no source was named, and the
    /// pool does not sync by default ([`syncs_by_default`]), so nothing was read.
    Off,
    /// Nothing: the pool syncs by default, and this machine kept its roster once and removed
    /// it, which takes a machine out of a pool ([`by_default_admits`]): the pool's store still
    /// holds the records of `label`, a row the sync would add, which no new row may inherit.
    /// Nothing was written.
    Left {
        /// The first row the sync would add that the store still holds records of.
        label: String,
    },
    /// Opted in or synced by default, and no fleet of the CLI's is there to read: neither its
    /// fleet file ([`cli_fleet_file`]) nor its column of the sheet ([`ACCOUNTS_CSV_FILE`]).
    NoSource,
    /// Synced by default with no roster kept here, and the fleet lists no account of this
    /// machine's, or no `m<N>` names this machine: nothing was written, not even a lock (but
    /// for the recovery of an interrupted sync of the pool's own, which runs first).
    Unlisted {
        /// The machine the fleet was read for, where one is known.
        machine: Option<String>,
        /// Where the fleet came from.
        source: String,
    },
    /// The fleet was read.
    Synced(CliSynced),
}

/// A sync that read the fleet ([`CliSync::Synced`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliSynced {
    /// The CLI.
    pub cli: Cli,
    /// The machine the rows were taken for.
    pub machine: String,
    /// The roster beside `aterm.toml` (`accounts-<cli>.toml`).
    pub roster: PathBuf,
    /// Where the fleet came from.
    pub source: String,
    /// The fleet's rows, every machine's.
    pub fleet_accounts: usize,
    /// The roster's rows after the sync.
    pub rows: usize,
    /// The labels the sync added, each with a login directory of its own.
    pub added: Vec<String>,
    /// This machine's rows the sheet held for want of a region ([`CliFleet::held`]).
    pub held: Vec<String>,
    /// This machine's fleet rows left out because their label has another identity or vendor
    /// setting, or their address matches another label or multiple rows: the local rows stand.
    pub kept: Vec<String>,
}

/// An [`Env`] for a sync run from init, the vault's verbs or a window's upkeep: it names the
/// paths and reads no process environment.
fn quiet_env(home: &Path, state_dir: &Path, config: &Path, now: i64) -> Env {
    Env {
        state: state_dir.to_path_buf(),
        aterm_state: state_dir
            .parent()
            .and_then(Path::parent)
            .map(Path::to_path_buf),
        cwd: home.to_path_buf(),
        home: Some(home.to_path_buf()),
        claude_dir: Some(home.join(".claude")),
        claude_config_unusable: None,
        claude_profile: Some(home.join(".claude.json")),
        claude_custom_oauth: false,
        claude_session: None,
        claude_pid: None,
        codex_thread: None,
        agy_conversation: None,
        codex_home: None,
        codex_login: None,
        now,
        tz_s: 0,
        sid: String::new(),
        sock: None,
        config: Some(config.to_path_buf()),
        claude_tmp: None,
        claude_running: None,
    }
}

/// `cli`'s fleet and where it came from: `source` where one is named (a `.toml` read as
/// [`FleetRoster::from_toml_for`], else a sheet whose `cli` column [`parse_accounting_csv_for`]
/// reads), else `~/.secrets`' [`cli_fleet_file`], else its [`ACCOUNTS_CSV_FILE`] where the header
/// names `cli`'s column ([`csv_names_column`]); `None` where none is there. Nothing is written.
fn load_cli_fleet(
    cli: Cli,
    env: &Env,
    home: &Path,
    source: Option<&str>,
    fetch: &Fetch<'_>,
) -> Result<Option<CliFleet>, String> {
    load_cli_fleet_source(cli, env, home, source, None, fetch).map(|res| res.map(|(f, _)| f))
}

fn load_cli_fleet_source(
    cli: Cli,
    env: &Env,
    home: &Path,
    source: Option<&str>,
    machine_override: Option<&str>,
    fetch: &Fetch<'_>,
) -> Result<Option<(CliFleet, Option<PoolSyncCtx>)>, String> {
    let (chosen, raw) = match source.map(str::trim).filter(|s| !s.is_empty()) {
        Some(named) => (named.to_owned(), fetch(env, named)?),
        None => {
            let secrets = home.join(".secrets");
            let pool = secrets.join(super::ai_pool::AI_POOL_FILE);
            let toml = secrets.join(cli_fleet_file(cli));
            let csv = secrets.join(ACCOUNTS_CSV_FILE);
            let read = |path: &Path| {
                read_local_source_text(path).map_err(|why| format!("{}: {why}", path.display()))
            };
            if pool_entry_present(&pool) {
                (pool.display().to_string(), read(&pool)?)
            } else if source_entry_present(&toml) {
                (toml.display().to_string(), read(&toml)?)
            } else if source_entry_present(&csv) {
                let text = read(&csv)?;
                if !csv_names_column(cli, &text) {
                    return Ok(None);
                }
                (csv.display().to_string(), text)
            } else {
                return Ok(None);
            }
        }
    };
    if super::ai_pool::Pool::looks_like_pool(&chosen, &raw) {
        let pool = super::ai_pool::Pool::from_toml(&raw, &chosen)?;
        let identity = super::ai_pool::resolve_local_pool_identity(
            Some(home),
            env.config.as_deref(),
            machine_override,
        )
        .ok_or_else(|| "could not determine machine id (`m<N>`); pass `--machine m8`".to_owned())?;
        let roster = project_pool_fleet(&pool, cli, Some(&identity));
        return Ok(Some((
            CliFleet {
                cli,
                roster,
                held: Vec::new(),
            },
            Some(PoolSyncCtx { pool, identity }),
        )));
    }
    let fleet = if chosen.ends_with(".toml")
        || raw.trim_start().starts_with('#')
        || raw.contains("[[account]]")
    {
        CliFleet {
            cli,
            roster: FleetRoster::from_toml_for(cli, &raw, &chosen)?,
            held: Vec::new(),
        }
    } else {
        parse_accounting_csv_for(cli, &raw, &chosen)?
    };
    Ok(Some((fleet, None)))
}

/// Read one pool's complete source table, including rows held before sign-in.
/// Unlike sync, this needs no local roster or machine ID and changes nothing.
///
/// # Errors
/// There is no home or selected source, or the source cannot be read and parsed.
pub fn account_source_table(cli: Cli, env: &Env, source: Option<&str>) -> Result<CliFleet, String> {
    let home = env
        .home
        .as_deref()
        .ok_or_else(|| format!("{} has no home directory", cli.connector().product))?;
    let config = env
        .config
        .clone()
        .unwrap_or_else(|| env.state.join("aterm.toml"));
    let quiet = quiet_env(home, &env.state, &config, env.now);
    load_cli_fleet(cli, &quiet, home, source, &fetch_source_text)?
        .ok_or_else(|| format!("no source table for {}", cli.connector().product))
}

fn sync_cli_with(
    cli: Cli,
    paths: (&Path, &Path, &Path),
    source: (Option<&str>, Option<&str>),
    now: i64,
    reads: (&Fetch<'_>, &[Duration]),
) -> Result<CliSync, String> {
    sync_cli_with_replan(cli, paths, source, now, reads, || {})
}

fn fleet_matches_identity(cli: Cli, row: &Row, account: &FleetAccount) -> bool {
    accounts::same_provider_realm(cli, row.vendor.region, account.vendor.region)
        && (row
            .email
            .as_deref()
            .is_some_and(|email| accounts::email_matches(email, &account.email))
            || (cli == Cli::Codex
                && row
                    .expected_identity_email()
                    .is_some_and(|email| accounts::email_matches(email, &account.email))))
}

/// Sync `cli`'s pool (a CLI of [`OTHER_CLIS`]) on this machine from its fleet
/// ([`load_cli_fleet`]): where its roster (`accounts-<cli>.toml`) stands beside `config`, a
/// `source` is named, or the pool syncs by default ([`syncs_by_default`]) and the fleet lists
/// rows of this machine's ([`CliSync::Unlisted`] where it lists none, before any lock or
/// directory is made; an interrupted sync's recovery runs first, as for any sync), this
/// machine's fleet rows the roster does not list are added to it, each
/// with a login directory under `~/.aterm/logins/<cli>/` ([`accounts::Store::at_for`]), and a roster
/// with no rows first gets this machine's own login (`main`, no dir), as `accounts add` gives it.
/// It writes the roster and the CLI's store alone, never `~/.secrets`; a row the roster has is
/// never changed or removed. A no-op requires exactly one identity match with the same label
/// and vendor settings; conflicting or ambiguous matches are left out ([`CliSynced::kept`]).
/// Under the CLI's store lock, waited for on `ladder`.
/// New labels require readable, unoccupied local state and an idle sign-in lock.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSyncNewLabels",
        action = "Sync",
        project = "aterm_agent::harness::accounts_sync::cli_pool_tests::new_label_tests::sync_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountProviderRealm",
        action = "Sync",
        project = "aterm_agent::harness::accounts_sync::cli_pool_tests::provider_realm_tests::realm_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountAliasAdmission",
        action = "Sync",
        project = "aterm_agent::harness::accounts_sync::cli_pool_tests::alias_admission_tests::alias_transition"
    )
)]
fn sync_cli_with_replan(
    cli: Cli,
    (home, state_dir, config): (&Path, &Path, &Path),
    (source, machine_override): (Option<&str>, Option<&str>),
    now: i64,
    (fetch, ladder): (&Fetch<'_>, &[Duration]),
    after_unlock: impl FnOnce(),
) -> Result<CliSync, String> {
    let product = cli.connector().product;
    let roster_path = accounts::roster_beside_for(config, cli)
        .ok_or_else(|| format!("{} has no directory", config.display()))?;
    let named = source.is_some_and(|s| !s.trim().is_empty());
    // No roster kept here and no source named: only a pool that syncs by default reads its
    // fleet, and it writes nothing until the fleet lists rows of this machine's.
    let by_default = !named && !roster_path.is_file();
    if by_default && !syncs_by_default(cli) {
        return Ok(CliSync::Off);
    }
    let store = accounts::Store::at_for(state_dir, cli, home);
    store.recover_sync_within(ladder, &roster_path)?;
    let env = quiet_env(home, state_dir, config, now);
    let Some((fleet, pool_ctx)) =
        load_cli_fleet_source(cli, &env, home, source, machine_override, fetch)?
    else {
        return Ok(CliSync::NoSource);
    };
    let machine = pool_ctx.as_ref().map_or_else(
        || cli_sync_machine(config, machine_override),
        |ctx| Some(ctx.identity.primary.clone()),
    );
    let machine = if by_default {
        match by_default_admits(&store, &fleet, machine, now)? {
            Ok(machine) => machine,
            Err(instead) => return Ok(instead),
        }
    } else {
        machine.ok_or_else(|| {
            "could not determine machine id (`m<N>`); pass `--machine m8`".to_owned()
        })?
    };
    let fleet_accounts = pool_ctx
        .as_ref()
        .map_or(fleet.roster.accounts.len(), |ctx| {
            ctx.pool.accounts_for_cli(cli).len()
        });
    let mine = fleet.roster.accounts_for_machine(&machine);
    if named && mine.is_empty() {
        return Err(format!(
            "no active {product} accounts found for machine `{machine}` in {}",
            fleet.roster.source
        ));
    }
    let held: Vec<String> = fleet
        .held
        .iter()
        .filter(|a| {
            let norm = normalize_machine_id(&machine).unwrap_or_else(|| machine.clone());
            a.machines.contains(&norm)
        })
        .map(|a| a.label.clone())
        .collect();
    let take_writer = || {
        store
            .acquire_within(ladder)
            .map_err(|why| format!("{}: {why}", store.dir.display()))?
            .ok_or_else(|| {
                format!(
                    "{} at {}: another `aterm harness accounts` or a window holds it; nothing was \
                 synced, so run the sync again",
                    accounts::LOCK_HELD,
                    store.dir.display()
                )
            })
    };
    let mut lock = take_writer()?;
    let mut creation_signin = None;
    let mut after_unlock = Some(after_unlock);
    let mut roster_wrote = false;
    let (write_to, total, added, kept, final_rows) = loop {
        let (write_to, keep_mode) = super::cli::roster_target(&roster_path)?;
        let existing_text = accounts::roster_text(&write_to)?.unwrap_or_default();
        let existing = Roster::parse_for(cli, &existing_text).map_err(|why| {
            format!(
                "the roster at {} is refused: {why}; the roster is unchanged",
                roster_path.display()
            )
        })?;
        let mut rows = Vec::new();
        let mut added = Vec::new();
        let mut kept = Vec::new();
        for acct in &mine {
            let mut candidates = existing
                .rows
                .iter()
                .filter(|row| fleet_matches_identity(cli, row, acct));
            // One row's login address can be another's canonical identity.
            // Count rows, so two matching fields on one row are still unambiguous.
            match (candidates.next(), candidates.next()) {
                (Some(row), None) if row.label == acct.label && row.vendor == acct.vendor => {}
                (None, None) if existing.row(&acct.label).is_none() => {
                    let dir = store
                        .login_dir(&acct.label)
                        .ok_or_else(|| format!("`{}` names no login directory", acct.label))?;
                    let dir = dir.to_str().map(str::to_owned).ok_or_else(|| {
                        format!("the login directory {} is not UTF-8", dir.display())
                    })?;
                    rows.push(Row {
                        label: acct.label.clone(),
                        class: acct.class,
                        dir: Some(dir),
                        email: Some(acct.email.clone()),
                        identity_email: None,
                        browser: acct.browser.clone(),
                        held_by: None,
                        was: Vec::new(),
                        vendor: acct.vendor,
                    });
                    added.push(acct.label.clone());
                }
                _ => kept.push(acct.label.clone()),
            }
        }
        let mut held_changed = false;
        let mut merged_existing = existing.rows.clone();
        if let Some(ctx) = &pool_ctx {
            for row in &mut merged_existing {
                if row.email.is_some() || row.identity_email.is_some() {
                    let want = ctx.holder_for_row(cli, row);
                    if row.held_by != want {
                        row.held_by = want;
                        held_changed = true;
                    }
                }
            }
        }
        let mut total = merged_existing.len() + rows.len();
        if !rows.is_empty() {
            if merged_existing.is_empty() {
                rows.insert(
                    0,
                    Row {
                        label: ACCOUNTS_OWN_LABEL.to_owned(),
                        ..Row::default()
                    },
                );
                total += 1;
            }
            let text = if held_changed {
                let mut all_rows = merged_existing.clone();
                all_rows.extend(rows.iter().cloned());
                accounts::append_rows_for(cli, "", &all_rows)
            } else {
                accounts::append_rows_for(cli, &existing_text, &rows)
            }
            .map_err(|why| {
                format!("the synced rows do not admit: {why}; the roster is unchanged")
            })?;
            if creation_signin.is_none() {
                // New rows need both locks. Release the writer before trying the
                // sign-in lock, then read and plan again under a fresh writer lock.
                drop(lock);
                if let Some(after_unlock) = after_unlock.take() {
                    after_unlock();
                }
                creation_signin = Some(
                    store
                        .signin_now()
                        .map_err(|why| format!("{}: {why}", store.shared_dir().display()))?
                        .ok_or_else(|| {
                            format!("{}; no new account rows were synced", accounts::SIGNIN_HELD)
                        })?,
                );
                lock = take_writer()?;
                continue;
            }
            store.check_new_rows(&rows, &existing, now)?;
            aterm_types::fs_restricted::ensure_private_dir(&store.logins_dir())
                .map_err(|why| format!("{}: {why}", store.logins_dir().display()))?;
            for dir in rows.iter().filter_map(|row| row.dir.as_deref()) {
                aterm_types::fs_restricted::ensure_private_dir(Path::new(dir))
                    .map_err(|why| format!("could not create {dir}: {why}"))?;
            }
            super::upgrade_models::write_atomic_mode(&write_to, &text, keep_mode)
                .map_err(|why| format!("could not write {}: {why}", write_to.display()))?;
            roster_wrote = true;
            merged_existing.extend(rows);
        } else if held_changed {
            let text = accounts::append_rows_for(cli, "", &merged_existing).map_err(|why| {
                format!("the synced rows do not admit: {why}; the roster is unchanged")
            })?;
            if text != existing_text {
                super::upgrade_models::write_atomic_mode(&write_to, &text, keep_mode)
                    .map_err(|why| format!("could not write {}: {why}", write_to.display()))?;
                roster_wrote = true;
            }
        }
        break (write_to, total, added, kept, merged_existing);
    };
    drop(lock);
    // Recovery takes the sign-in lock too and must see this publication ended.
    drop(creation_signin);
    let mut state_wrote = false;
    if let Some(ctx) = &pool_ctx
        && (total > 0 || store.dir.exists())
    {
        let home_accounts: BTreeSet<String> = final_rows
            .iter()
            .filter(|row| row.held_by.is_none() && ctx.is_home_for_row(cli, row))
            .map(|row| row.label.clone())
            .collect();
        store
            .transact_within(ladder, now, &mut None, |state| {
                let target_label = Some(ctx.identity.primary.clone());
                if state.swarm_label != target_label
                    || state.swarm_aliases != ctx.identity.aliases
                    || state.home_accounts != home_accounts
                {
                    state.swarm_label = target_label;
                    state.swarm_aliases.clone_from(&ctx.identity.aliases);
                    state.home_accounts.clone_from(&home_accounts);
                    state_wrote = true;
                }
                Ok(Vec::new())
            })
            .map_err(|why| format!("could not update {product} account state: {why}"))?;
    }
    let recovered =
        recover_interrupted_staging_renewals_for(cli, &store, &write_to, &AUTO_RENEW_GUARD, now);
    // A sync that added nothing wrote nothing, and wakes no watcher.
    if !added.is_empty() || roster_wrote || state_wrote || recovered {
        let _ = store.touch_wake();
    }
    Ok(CliSync::Synced(CliSynced {
        cli,
        machine,
        roster: roster_path,
        source: fleet.roster.source.clone(),
        fleet_accounts,
        rows: total,
        added,
        held,
        kept,
    }))
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountSyncNewLabels",
        action = "AnotherCase",
        reason = "Model bookkeeping: choose retained snapshots, source rows, or a competing sign-in"
    )
)]
#[doc(hidden)]
pub fn account_sync_new_labels_environment() {}

/// Fixture selection for the provider-realm model, linked by the source audit.
#[doc(hidden)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountProviderRealm",
        action = "AnotherCase",
        reason = "Select a local roster and an explicit provider fleet before sync."
    )
)]
pub fn account_provider_realm_environment() {}

/// The alias-admission model's local roster and request selection.
#[doc(hidden)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountAliasAdmission",
        action = "AddCandidate",
        reason = "Select a local roster with zero, one or two matching rows."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountAliasAdmission",
        action = "NameFirst",
        reason = "Choose the first matching row's label for the incoming account."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountAliasAdmission",
        action = "NameSecond",
        reason = "Choose the second matching row's label for the incoming account."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountAliasAdmission",
        action = "NameOther",
        reason = "Choose an existing unrelated label for the incoming account."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountAliasAdmission",
        action = "ChangeMethod",
        reason = "Choose a different incoming login method without changing identity."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountAliasAdmission",
        action = "AddCompanion",
        reason = "Include an independent fresh account that requires an append and replan."
    )
)]
pub fn account_alias_admission_environment() {}

/// `aterm harness accounts sync --cli <id> [<source>] [--machine <mN>] [--json]`
/// ([`sync_cli_with`]).
pub fn run_cli_accounts_sync(
    cli: Cli,
    env: &Env,
    (source, machine_override, json): (Option<&str>, Option<&str>, bool),
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitCode {
    run_cli_accounts_sync_with(
        cli,
        env,
        (source, machine_override, json),
        (out, err),
        &fetch_source_text,
    )
}

fn run_cli_accounts_sync_with(
    cli: Cli,
    env: &Env,
    (source, machine_override, json): (Option<&str>, Option<&str>, bool),
    (out, err): (&mut dyn Write, &mut dyn Write),
    fetch: &Fetch<'_>,
) -> ExitCode {
    let flag = format!("--cli {}", cli.id());
    let product = cli.connector().product;
    let normalized = match source_env(env) {
        Ok(env) => env,
        Err(why) => {
            let _ = writeln!(err, "aterm harness accounts sync {flag}: {why}");
            return ExitCode::FAILURE;
        }
    };
    let env = &normalized;
    let (Some(config), Some(home)) = (env.config.as_deref(), env.home.as_deref()) else {
        let _ = writeln!(
            err,
            "aterm harness accounts sync {flag}: no config directory or home is known (set HOME \
             or XDG_CONFIG_HOME)"
        );
        return ExitCode::FAILURE;
    };
    // The pool helper reconstructs a home-based environment for background upkeep.
    // An explicit CLI fetch keeps the session context captured by this invocation.
    let fetch_in_session = |_: &Env, source: &str| fetch(env, source);
    let synced = sync_cli_with(
        cli,
        (home, &env.state, config),
        (source, machine_override),
        env.now,
        (&fetch_in_session, &sync_lock_ladder()),
    );
    let file = cli.accounts().roster_file;
    let synced = match synced {
        Ok(CliSync::Synced(synced)) => synced,
        Ok(CliSync::Left { label }) => {
            let _ = writeln!(
                err,
                "aterm harness accounts sync {flag}: nothing was synced: this machine kept a \
                 {product} roster ({file}) and removed it, which takes a machine out of the \
                 pool; its store still holds the records of `{label}`, a row the sync would \
                 add, which no new row may inherit; to rejoin, restore {file} with the rows it \
                 held, `{label}` among them"
            );
            return ExitCode::FAILURE;
        }
        Ok(CliSync::Off) => {
            let _ = writeln!(
                err,
                "aterm harness accounts sync {flag}: nothing was read: this machine keeps no \
                 {product} roster ({file}) and no source was named; name one (`aterm harness \
                 accounts sync {flag} ~/.secrets/{}`), or list an account (`aterm harness \
                 accounts add <label> {flag}`)",
                cli_fleet_file(cli)
            );
            return ExitCode::FAILURE;
        }
        Ok(CliSync::NoSource) => {
            let _ = writeln!(
                err,
                "aterm harness accounts sync {flag}: nothing to read: neither ~/.secrets/{} nor a \
                 `{}` column in ~/.secrets/{ACCOUNTS_CSV_FILE} is there; the roster is unchanged",
                cli_fleet_file(cli),
                csv_columns(cli).first().copied().unwrap_or(cli.id())
            );
            return ExitCode::FAILURE;
        }
        Ok(CliSync::Unlisted { machine, source }) => {
            let _ = match machine {
                Some(machine) => writeln!(
                    err,
                    "aterm harness accounts sync {flag}: no active {product} accounts found for \
                     machine `{machine}` in {source}; no roster ({file}) was made"
                ),
                None => writeln!(
                    err,
                    "aterm harness accounts sync {flag}: could not determine machine id \
                     (`m<N>`), so no {product} account in {source} is this machine's; pass \
                     `--machine m8`"
                ),
            };
            return ExitCode::FAILURE;
        }
        Err(why) => {
            let _ = writeln!(err, "aterm harness accounts sync {flag}: {why}");
            return ExitCode::FAILURE;
        }
    };
    if json {
        let words =
            |labels: &[String]| Value::Array(labels.iter().cloned().map(Value::from).collect());
        let mut o = Map::new();
        o.insert("schema".to_owned(), Value::from(1u64));
        o.insert("kind".to_owned(), Value::from("accounts-sync".to_owned()));
        o.insert("cli".to_owned(), Value::from(cli.id().to_owned()));
        o.insert("machine".to_owned(), Value::from(synced.machine.clone()));
        o.insert(
            "roster".to_owned(),
            Value::from(synced.roster.display().to_string()),
        );
        o.insert("source".to_owned(), Value::from(synced.source.clone()));
        o.insert(
            "fleet_accounts".to_owned(),
            Value::from(u64::try_from(synced.fleet_accounts).unwrap_or(0)),
        );
        o.insert(
            "rows".to_owned(),
            Value::from(u64::try_from(synced.rows).unwrap_or(0)),
        );
        o.insert("added".to_owned(), words(&synced.added));
        o.insert("held".to_owned(), words(&synced.held));
        o.insert("kept".to_owned(), words(&synced.kept));
        let _ = writeln!(
            out,
            "{}",
            aterm_json::to_string(&Value::Object(o)).unwrap_or_default()
        );
        return ExitCode::SUCCESS;
    }
    let _ = writeln!(
        out,
        "synced {product}'s pool for machine `{}` ({} added; {} rows in the roster, {} across \
         the fleet)",
        synced.machine,
        synced.added.len(),
        synced.rows,
        synced.fleet_accounts
    );
    let _ = writeln!(out, "roster: {}", synced.roster.display());
    let _ = writeln!(out, "source: {}", synced.source);
    if !synced.held.is_empty() {
        let _ = writeln!(
            out,
            "held (the sheet names no region; write `global` or `mainland-cn` in its cell): {}",
            synced.held.join(" ")
        );
    }
    if !synced.kept.is_empty() {
        let _ = writeln!(
            out,
            "left as the roster lists them (conflicting accounts or sign-in settings): {}",
            synced.kept.join(" ")
        );
    }
    if !synced.added.is_empty() && cli.renews_headlessly() {
        let _ = writeln!(
            out,
            "not signed in yet: {}; aterm signs one in by itself once a session needs it \
             (`aterm harness accounts renew {} {flag}` runs that now)",
            synced.added.join(", "),
            synced.added.join(" ")
        );
    } else if !synced.added.is_empty() {
        let _ = writeln!(
            out,
            "not signed in yet: {}; {product}'s sign-in page answers aterm's headless browser \
             with a block page, so a person signs each in: `aterm harness accounts {} <label> \
             {flag}`",
            synced.added.join(", "),
            cli.sign_in_verb(true)
        );
    }
    ExitCode::SUCCESS
}

/// This machine's id for a sync of another CLI's pool: `machine_override`, else `[ai]`,
/// `[fleet]` or `[harness] machine` in `config`, else the host's name ([`detect_machine_id`]).
fn cli_sync_machine(config: &Path, machine_override: Option<&str>) -> Option<String> {
    machine_override
        .and_then(normalize_machine_id)
        .or_else(|| config_machine_id(Some(config)))
        .or_else(|| detect_machine_id(None, None))
}

/// THE DEFAULT SYNC'S ADMISSION, for a pool syncing by default ([`syncs_by_default`]) with no
/// roster of it here and no source named: `machine`, where the sync takes rows for it, or what
/// the sync is instead. That is [`CliSync::Unlisted`] where `fleet` lists no row of this
/// machine's or no `m<N>` names it, and [`CliSync::Left`] where this machine kept the pool's
/// roster once and removed it, which takes a machine out of a pool as it always has: the
/// pool's store still holds the records of a row the sync would add (`main` or a fleet row),
/// which a new row may never inherit. The store's records are what tell a machine that left
/// from a new one, so a machine whose rows were all relabelled since it left holds none of
/// theirs and joins again. It reads only, the store where it exists.
/// # Errors
/// The pool's store cannot be read.
fn by_default_admits(
    store: &accounts::Store,
    fleet: &CliFleet,
    machine: Option<String>,
    now: i64,
) -> Result<Result<String, CliSync>, String> {
    let listed = match machine.as_deref() {
        Some(m) => fleet.roster.accounts_for_machine(m),
        None => Vec::new(),
    };
    let machine = match machine {
        Some(machine) if !listed.is_empty() => machine,
        machine => {
            return Ok(Err(CliSync::Unlisted {
                machine,
                source: fleet.roster.source.clone(),
            }));
        }
    };
    let labels: Vec<&str> = std::iter::once(ACCOUNTS_OWN_LABEL)
        .chain(listed.iter().map(|account| account.label.as_str()))
        .collect();
    if store.dir.exists()
        && let Some(label) = store.retained(&labels, now)?
    {
        return Ok(Err(CliSync::Left {
            label: label.to_owned(),
        }));
    }
    Ok(Ok(machine))
}

/// The pools syncing by default ([`syncs_by_default`]) that this machine keeps no roster of
/// beside `config` and whose next automatic sync ([`sync_cli_accounts_for_init`]) takes rows or
/// fails: the vault's fleet in `~/.secrets` lists rows of this machine's and it has not left
/// the pool ([`by_default_admits`]), or that fleet or the pool's store cannot be read, which
/// the sync then reports. `aterm init`'s `base.ai.accounts` probe names them. It only reads.
#[must_use]
pub fn cli_pools_due(config: &Path, state_dir: &Path, home: &Path, now: i64) -> Vec<Cli> {
    let env = quiet_env(home, state_dir, config, now);
    OTHER_CLIS
        .into_iter()
        .filter(|&cli| {
            syncs_by_default(cli)
                && accounts::roster_beside_for(config, cli).is_some_and(|roster| !roster.is_file())
                && match load_cli_fleet_source(cli, &env, home, None, None, &fetch_source_text) {
                    Ok(None) => false,
                    Ok(Some((fleet, pool_ctx))) => {
                        let machine = pool_ctx.as_ref().map_or_else(
                            || cli_sync_machine(config, None),
                            |ctx| Some(ctx.identity.primary.clone()),
                        );
                        !matches!(
                            by_default_admits(
                                &accounts::Store::at_for(state_dir, cli, home),
                                &fleet,
                                machine,
                                now,
                            ),
                            Ok(Err(_))
                        )
                    }
                    Err(_) => true,
                }
        })
        .collect()
}

/// [`sync_accounts_for_init`] for the other pooled CLIs ([`OTHER_CLIS`]), each through
/// [`sync_cli_with`] with no source: a CLI whose roster this machine keeps beside `roster_path`,
/// or whose pool syncs by default ([`syncs_by_default`]), syncs from its fleet in `~/.secrets`;
/// any other reads nothing ([`CliSync::Off`]), whatever the vault put there. Where
/// [`accounts_sync_off`] holds, no CLI is synced and the list is empty.
#[must_use]
pub fn sync_cli_accounts_for_init(
    home: &Path,
    state_dir: &Path,
    roster_path: &Path,
    now: i64,
) -> Vec<(Cli, Result<CliSync, String>)> {
    sync_cli_accounts_with(home, state_dir, roster_path, now, &fetch_source_text)
}

/// [`sync_cli_accounts_for_init`], reading a named source with `fetch`.
fn sync_cli_accounts_with(
    home: &Path,
    state_dir: &Path,
    roster_path: &Path,
    now: i64,
    fetch: &Fetch<'_>,
) -> Vec<(Cli, Result<CliSync, String>)> {
    let config = config_beside(roster_path);
    if accounts_sync_off(Some(&config)) {
        return Vec::new();
    }
    let ladder = sync_lock_ladder();
    OTHER_CLIS
        .into_iter()
        .map(|cli| {
            let synced = sync_cli_with(
                cli,
                (home, state_dir, &config),
                (None, None),
                now,
                (fetch, &ladder),
            );
            (cli, synced)
        })
        .collect()
}

/// One other CLI's pool as this machine keeps it ([`cli_pools`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliPool {
    /// The CLI.
    pub cli: Cli,
    /// Its roster beside `aterm.toml`.
    pub roster: PathBuf,
    /// Its rows, bound to local logins, or why the roster, bindings or state is refused.
    pub rows: Result<Roster, String>,
    /// The labels whose logins are made on this machine ([`accounts::enrolled_for`]).
    pub enrolled: BTreeSet<String>,
    /// Its store's state, for each row's condition. Use only when the rows field is Ok.
    pub state: accounts::State,
}

/// The pools of [`OTHER_CLIS`] whose roster stands beside `config`, plus refused
/// stores whose records cannot establish absence. Clean absent pools stay absent.
/// The doctor and dashboard use one recovered snapshot for each pool.
#[must_use]
pub fn cli_pools(config: &Path, state_dir: &Path, home: &Path, now: i64) -> Vec<CliPool> {
    OTHER_CLIS
        .into_iter()
        .filter_map(|cli| {
            let roster = accounts::roster_beside_for(config, cli)?;
            let store = accounts::Store::at_for(state_dir, cli, home);
            let records = store.read_account_records(Some(&roster), now);
            if records
                .as_ref()
                .is_ok_and(|records| !records.roster_present)
            {
                return None;
            }
            let (rows, state) = match records {
                Ok(records) => (Ok(records.roster), records.state),
                Err(
                    accounts::RosterReadError::Roster(why)
                    | accounts::RosterReadError::Records(why),
                ) => (Err(why), accounts::State::default()),
            };
            let enrolled = rows
                .as_ref()
                .map(|bound| accounts::enrolled_for(cli, bound))
                .unwrap_or_default();
            Some(CliPool {
                cli,
                roster,
                rows,
                enrolled,
                state,
            })
        })
        .collect()
}

/// The pools in one phrase, each CLI by its id and row count (`codex 1, kimi 2`), a refused
/// roster as `refused`; `None` where this machine keeps none.
#[must_use]
pub fn cli_pools_words(pools: &[CliPool]) -> Option<String> {
    if pools.is_empty() {
        return None;
    }
    Some(
        pools
            .iter()
            .map(|pool| match &pool.rows {
                Ok(rows) => format!("{} {}", pool.cli.id(), rows.rows.len()),
                Err(_) => format!("{} refused", pool.cli.id()),
            })
            .collect::<Vec<_>>()
            .join(", "),
    )
}

/// The window's upkeep of the other CLIs' pools ([`upkeep`]): each CLI whose roster stands beside
/// `roster_path`, or whose pool syncs by default ([`syncs_by_default`]), syncs from its fleet
/// without waiting on its lock (unless [`accounts_sync_off`] holds), and, once its roster stands
/// (this pass's sync may have written it) and `[harness] logins` renews, starts one due login's
/// renewal in the background ([`spawn_row_renewal`]); any other CLI whose roster this machine
/// does not keep is left alone.
fn upkeep_pools(
    home: &Path,
    state_dir: &Path,
    (roster_path, now, step): (&Path, i64, UpkeepStep),
    fx: &UpkeepEffects<'_>,
) {
    let config = config_beside(roster_path);
    let syncs = step.syncs() && !accounts_sync_off(Some(&config));
    let renews = step.renews() && sign_ins_on(&config);
    for cli in OTHER_CLIS {
        let Some(path) = accounts::roster_beside_for(&config, cli) else {
            continue;
        };
        if syncs && (syncs_by_default(cli) || path.is_file()) {
            let by_default = (!path.is_file()).then(|| cli_sync_inputs(cli, home, &config));
            if !by_default.is_some_and(|inputs| default_sync_holds(fx.rediscovery, cli, &inputs)) {
                let synced = sync_cli_with(
                    cli,
                    (home, state_dir, &config),
                    (None, None),
                    now,
                    (fx.fetch, &[]),
                );
                let read = by_default.filter(|_| default_sync_settled(&synced));
                note_default_sync(fx.rediscovery, cli, read);
            }
        }
        if !path.is_file() {
            continue;
        }
        if cli == Cli::Kimi {
            (fx.quota_refresh)(home, state_dir, &path, now);
        }
        if let Some(pending) = step
            .renews()
            .then(|| resume_pending_pin_for(cli, home, state_dir, &path, now, fx))
            .flatten()
        {
            let _ = pending;
        } else if renews {
            let _ = renew_pool_due(cli, home, state_dir, &path, now, fx);
        }
    }
}

/// What a sync of `cli`'s pool with no roster here reads that a periodic caller reads cheaply
/// ([`default_sync_holds`]): its fleet file and the sheet in `~/.secrets` (each as the sync
/// reads it, never through a link), and `config` (`[fleet] machine`, read through a link as
/// the sync reads it). The host's name and the pool's store are left out: [`REDISCOVER_EVERY`]
/// reads them again.
fn cli_sync_inputs(cli: Cli, home: &Path, config: &Path) -> [u8; 32] {
    let secrets = home.join(".secrets");
    let mut hash = aterm_digest::Sha256::new();
    hash.update(b"aterm-cli-sync-inputs-v1");
    hash.update(cli.id().as_bytes());
    let fleet = [
        secrets.join(cli_fleet_file(cli)),
        secrets.join(ACCOUNTS_CSV_FILE),
    ]
    .map(|path| read_local_source_text(&path).map(String::into_bytes).ok());
    for read in fleet.into_iter().chain([fs::read(config).ok()]) {
        match read {
            Some(bytes) => {
                hash.update([1]);
                hash.update((bytes.len() as u64).to_be_bytes());
                hash.update(&bytes);
            }
            None => hash.update([0]),
        }
    }
    hash.finalize()
}

/// THE QUIET RULE of a pool syncing by default that this machine keeps no roster of
/// ([`upkeep_pools`]): once its sync took nothing or failed for what it read
/// ([`default_sync_settled`]: the fleet lists no row of this machine's, this machine left the
/// pool, no source, a refusal), the upkeep syncs the same inputs ([`cli_sync_inputs`]) again
/// only after [`REDISCOVER_EVERY`], so such a Mac neither parses the sheet nor asks its host's
/// name at every upkeep and wall read; changed inputs sync at once.
fn default_sync_holds(
    rediscovery: &std::sync::Mutex<Rediscovery>,
    cli: Cli,
    inputs: &[u8; 32],
) -> bool {
    rediscovery
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .default_held
        .get(&cli)
        .is_some_and(|(read, at)| read == inputs && at.elapsed() < REDISCOVER_EVERY)
}

/// Whether a default sync's outcome stands for what it read ([`default_sync_holds`]): it took
/// no rows, or failed, but not because another process held the pool's or the sign-in lock,
/// which says nothing of what it read.
fn default_sync_settled(synced: &Result<CliSync, String>) -> bool {
    match synced {
        Ok(CliSync::Synced(_)) => false,
        Ok(CliSync::Off | CliSync::Left { .. } | CliSync::NoSource | CliSync::Unlisted { .. }) => {
            true
        }
        Err(why) => !why.contains(accounts::LOCK_HELD) && !why.contains(accounts::SIGNIN_HELD),
    }
}

/// Keeps `read` as what `cli`'s last default sync read ([`default_sync_holds`]), or forgets
/// it where `read` is `None` (the sync wrote rows, a lock was held, or the roster stands), so
/// the next upkeep syncs again.
fn note_default_sync(
    rediscovery: &std::sync::Mutex<Rediscovery>,
    cli: Cli,
    read: Option<[u8; 32]>,
) {
    let mut held = rediscovery
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match read {
        Some(inputs) => {
            held.default_held.insert(cli, (inputs, Instant::now()));
        }
        None => {
            held.default_held.remove(&cli);
        }
    }
}

/// Start one of `cli`'s due or wall-recovery logins' renewal ([`needed_renew_candidates_for`],
/// [`spawn_row_renewal`]); whether one started. A CLI none of whose renewals starts in the
/// background ([`Cli::renews_headlessly`]: Grok Build's) has its saved selection served (and so
/// cancelled) and its interrupted renewals recovered, and its candidates are never read.
fn renew_pool_due(
    cli: Cli,
    home: &Path,
    state_dir: &Path,
    roster_path: &Path,
    now: i64,
    fx: &UpkeepEffects<'_>,
) -> bool {
    if let Some(pending) = resume_pending_pin_for(cli, home, state_dir, roster_path, now, fx) {
        return pending.progressed();
    }
    let store = accounts::Store::at_for(state_dir, cli, home);
    let recovered =
        recover_interrupted_staging_renewals_for(cli, &store, roster_path, fx.renew_guard, now);
    if !cli.renews_headlessly() {
        return recovered;
    }
    let Ok(candidates) = read_renewal_candidates(cli, &store, roster_path, now) else {
        return recovered;
    };
    for (_, row) in candidates
        .tiered
        .into_iter()
        .filter(|(tier, _)| *tier < UNNEEDED_TIER)
    {
        if spawn_row_renewal(
            cli,
            home,
            state_dir,
            roster_path,
            candidates.records.raw_roster.clone(),
            &candidates.records.logins,
            row,
            (None, false),
            now,
            fx.renew_guard,
            fx.spawn_renewal,
            None,
        )
        .progressed()
        {
            return true;
        }
    }
    recovered
}

/// What a renewal reads of the machine beyond its home and store: the process's environment
/// (the mailbox and browser variables, `XDG_CACHE_HOME`) and the browsers installed outside the
/// home (`/Applications`, `PATH`). The window's guard ([`AUTO_RENEW_GUARD`]) reads the machine
/// ([`MachineReads::live`]); every other guard, a test's among them, reads nothing
/// ([`MachineReads::default`]), so what it decides comes from its home and store alone.
#[derive(Clone, Copy)]
struct MachineReads {
    /// One environment variable.
    var: fn(&str) -> Option<std::ffi::OsString>,
    /// Whether a browser outside the home counts.
    system_browsers: bool,
}

impl Default for MachineReads {
    fn default() -> Self {
        Self {
            var: |_| None,
            system_browsers: false,
        }
    }
}

impl MachineReads {
    /// The process's environment and every installed browser.
    fn live() -> Self {
        Self {
            var: |name| std::env::var_os(name),
            system_browsers: true,
        }
    }

    /// A headless-capable browser this machine has ([`super::headless_oauth::find_headless_browser`]).
    fn has_browser(self, home: &Path) -> bool {
        super::headless_oauth::find_headless_browser_with(
            Some(home),
            &self.var,
            self.system_browsers,
        )
        .is_some()
    }
}

/// What `guard`'s renewals read of the machine ([`AutoRenewGuard::reads`]).
fn guard_reads(guard: &std::sync::Arc<std::sync::Mutex<AutoRenewGuard>>) -> MachineReads {
    guard
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .reads
}

#[derive(Default)]
struct AutoRenewGuard {
    /// What this guard's renewals read of the machine ([`MachineReads`]).
    reads: MachineReads,
    in_flight: bool,
    last_attempt: BTreeMap<String, Instant>,
    mailbox_mtime: Option<std::time::SystemTime>,
    /// The rows whose last background renewal stopped where only a person can go on
    /// ([`super::cli::Renewal::NeedsPerson`]), and since when: none is tried again in the
    /// background before [`PERSON_BACKOFF`] has passed.
    parked: BTreeMap<String, Instant>,
    /// The rows whose last background renewal met Cloudflare's bot check
    /// ([`super::cli::Renewal::BotCheck`]), and since when: none is tried again in the
    /// background before [`PERSON_BACKOFF`] has passed, and nobody is asked (design R-13). Kept
    /// apart from `parked`, which the vendor's gate releases where a binding has no sign-in task
    /// (a bot check notes none).
    bot_checked: BTreeMap<String, Instant>,
    /// The newest `(at, same_second_count)` signed-in `accounts browser` window event already used
    /// to lift a row's background hold once.
    browser_unparked: BTreeMap<String, (i64, usize)>,
    /// The automatic sign-in ladder's backoffs and vault clock ([`super::sign_in_ladder::Ladder`]).
    ladder: super::sign_in_ladder::Ladder,
    /// The default row and the sign-out on record the default login's one check last ran for
    /// ([`heal_default_login_in_upkeep`]): it runs once per pair in this process.
    default_checked: Option<(String, i64)>,
    /// Attribution only; these do not participate in admission or retry policy.
    activity_owners: BTreeMap<String, LoginOwner>,
    ladder_owners: BTreeMap<String, LoginOwner>,
    active_login: Option<ActiveLogin>,
}

#[derive(Clone, Debug)]
struct LoginOwner {
    store: PathBuf,
    cli: Cli,
    label: String,
    observed_at: i64,
    token: std::sync::Arc<()>,
}

impl LoginOwner {
    fn new(store: &accounts::Store, cli: Cli, label: &str, observed_at: i64) -> Self {
        Self {
            store: store.dir.clone(),
            cli,
            label: label.into(),
            observed_at,
            token: std::sync::Arc::new(()),
        }
    }

    fn matches(&self, store: &accounts::Store, cli: Cli) -> bool {
        self.store == store.dir && self.cli == cli
    }
}

/// Claim identities captured before publication. Holding a token prevents a
/// later claim from reusing its identity, even when their wall times coincide.
struct LoginClaims {
    activity: BTreeMap<String, LoginOwner>,
    ladder: BTreeMap<String, LoginOwner>,
}

impl LoginClaims {
    fn capture(guard: &std::sync::Arc<std::sync::Mutex<AutoRenewGuard>>) -> Self {
        let state = guard
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Self {
            activity: state.activity_owners.clone(),
            ladder: state.ladder_owners.clone(),
        }
    }
}

struct ActiveLogin {
    owner: LoginOwner,
    route: Option<String>,
    token: std::sync::Arc<()>,
}

/// Process-local facts about an attributed login worker and its existing retry clocks.
/// An absent row is unobserved, not evidence that no other process is signing in.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct LoginActivity {
    pub label: String,
    pub route: Option<String>,
    pub active: bool,
    pub observed_at: i64,
    pub next_at: Option<i64>,
}

/// Read only this process's attributed work for the exact store and CLI. No filesystem
/// read, route probe, policy decision or credential operation is performed.
/// Returns `None` while another thread holds the observation lock.
#[must_use]
pub fn login_activity(store: &accounts::Store, cli: Cli, now: i64) -> Option<Vec<LoginActivity>> {
    let guard = match AUTO_RENEW_GUARD.try_lock() {
        Ok(guard) => guard,
        Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner(),
        Err(std::sync::TryLockError::WouldBlock) => return None,
    };
    Some(login_activity_of(&guard, store, cli, now, Instant::now()))
}

fn login_activity_of(
    guard: &AutoRenewGuard,
    store: &accounts::Store,
    cli: Cli,
    now: i64,
    clock: Instant,
) -> Vec<LoginActivity> {
    let mut rows = BTreeMap::<String, LoginActivity>::new();
    let mut add = |owner: &LoginOwner, until: Option<Instant>| {
        if !owner.matches(store, cli) {
            return;
        }
        let Some(until) = until.filter(|until| *until > clock) else {
            return;
        };
        let remaining = until.saturating_duration_since(clock);
        let seconds = remaining
            .as_secs()
            .saturating_add(u64::from(remaining.subsec_nanos() > 0));
        let next = now.saturating_add(i64::try_from(seconds).unwrap_or(i64::MAX));
        let row = rows
            .entry(owner.label.clone())
            .or_insert_with(|| LoginActivity {
                label: owner.label.clone(),
                route: None,
                active: false,
                observed_at: owner.observed_at,
                next_at: None,
            });
        row.observed_at = row.observed_at.max(owner.observed_at);
        row.next_at = Some(row.next_at.map_or(next, |held| held.min(next)));
    };
    for (key, owner) in &guard.activity_owners {
        // A claim is refused while any of these holds stands, so the row's next try is at the
        // latest of them.
        let held = [
            guard
                .last_attempt
                .get(key)
                .map(|at| *at + Duration::from_secs(900)),
            guard.parked.get(key).map(|at| *at + PERSON_BACKOFF),
            guard.bot_checked.get(key).map(|at| *at + PERSON_BACKOFF),
        ]
        .into_iter()
        .flatten()
        .max();
        add(owner, held);
    }
    for (key, owner) in &guard.ladder_owners {
        add(owner, guard.ladder.retry_at(key, clock));
    }
    if let Some(active) = &guard.active_login
        && active.owner.matches(store, cli)
    {
        let row = rows
            .entry(active.owner.label.clone())
            .or_insert_with(|| LoginActivity {
                label: active.owner.label.clone(),
                route: None,
                active: true,
                observed_at: active.owner.observed_at,
                next_at: None,
            });
        row.active = true;
        row.route.clone_from(&active.route);
        row.observed_at = active.owner.observed_at;
    }
    rows.into_values().collect()
}

struct LoginActivityGuard {
    guard: std::sync::Arc<std::sync::Mutex<AutoRenewGuard>>,
    token: std::sync::Arc<()>,
}

/// Who hears that this process's [`login_activity`] moved ([`watch_login_activity`]).
static LOGIN_ACTIVITY_WATCH: std::sync::OnceLock<Box<dyn Fn() + Send + Sync>> =
    std::sync::OnceLock::new();

/// Tell `moved` each time a sign-in of this process's begins or ends, after its lock is let go,
/// so a reader of [`login_activity`] needs no clock of its own. The first watch stands; a later
/// one is refused and returned.
///
/// # Errors
///
/// `moved` itself, when a watch already stands.
pub fn watch_login_activity(
    moved: Box<dyn Fn() + Send + Sync>,
) -> Result<(), Box<dyn Fn() + Send + Sync>> {
    LOGIN_ACTIVITY_WATCH.set(moved)
}

fn login_activity_moved() {
    if let Some(moved) = LOGIN_ACTIVITY_WATCH.get() {
        moved();
    }
}

/// A committed login ends the captured holds of its actual account. Replaced
/// claims, other owners, active work and browser-event history stay.
fn note_published_login(
    guard: &std::sync::Arc<std::sync::Mutex<AutoRenewGuard>>,
    observed: &LoginClaims,
    store: &accounts::Store,
    cli: Cli,
    label: &str,
) {
    let mut state = guard
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let owns = |owner: &LoginOwner| owner.matches(store, cli) && owner.label == label;
    let activity: Vec<_> = state
        .activity_owners
        .iter()
        .filter(|(key, owner)| {
            owns(owner)
                && observed
                    .activity
                    .get(*key)
                    .is_some_and(|before| std::sync::Arc::ptr_eq(&before.token, &owner.token))
        })
        .map(|(key, _)| key.clone())
        .collect();
    let ladder: Vec<_> = state
        .ladder_owners
        .iter()
        .filter(|(key, owner)| {
            owns(owner)
                && observed
                    .ladder
                    .get(*key)
                    .is_some_and(|before| std::sync::Arc::ptr_eq(&before.token, &owner.token))
        })
        .map(|(key, _)| key.clone())
        .collect();
    for key in &activity {
        state.last_attempt.remove(key);
        state.parked.remove(key);
        state.bot_checked.remove(key);
        state.activity_owners.remove(key);
    }
    for key in &ladder {
        state.ladder.signed_in(key);
        state.ladder_owners.remove(key);
    }
    drop(state);
    if !activity.is_empty() || !ladder.is_empty() {
        login_activity_moved();
    }
}

impl LoginActivityGuard {
    fn begin(
        guard: &std::sync::Arc<std::sync::Mutex<AutoRenewGuard>>,
        store: &accounts::Store,
        cli: Cli,
        label: &str,
        route: Option<&str>,
    ) -> Self {
        let token = std::sync::Arc::new(());
        guard
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .active_login = Some(ActiveLogin {
            owner: LoginOwner::new(store, cli, label, crate::supervise::limit::unix_now()),
            route: route.map(str::to_owned),
            token: token.clone(),
        });
        login_activity_moved();
        Self {
            guard: guard.clone(),
            token,
        }
    }
}

impl Drop for LoginActivityGuard {
    fn drop(&mut self) {
        let mut guard = self
            .guard
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if guard
            .active_login
            .as_ref()
            .is_some_and(|active| std::sync::Arc::ptr_eq(&active.token, &self.token))
        {
            guard.active_login = None;
            drop(guard);
            login_activity_moved();
        }
    }
}

#[cfg(test)]
mod login_activity_tests {
    use super::*;

    #[test]
    fn no_route_backoff_has_a_deadline_without_an_active_worker() {
        let guard = std::sync::Arc::new(std::sync::Mutex::new(AutoRenewGuard::default()));
        let store = accounts::Store::at(Path::new("/synthetic/activity-a"));
        let clock = Instant::now();
        {
            let mut state = guard.lock().unwrap();
            state.ladder.failed("row", "no-route", clock);
            state.ladder_owners.insert(
                "row".into(),
                LoginOwner::new(&store, Cli::Claude, "row", 900),
            );
        }
        let rows = observed(&guard, &store, Cli::Claude, clock);
        assert_eq!(rows.len(), 1);
        assert!(!rows[0].active);
        assert_eq!(rows[0].route, None);
        assert_eq!(rows[0].next_at, Some(1900));
        // The same changed-input path that admits a fresh route clears attribution.
        {
            let mut state = guard.lock().unwrap();
            assert!(!state.ladder.held("row", "new-mailbox", clock));
            state.ladder_owners.remove("row");
        }
        assert!(observed(&guard, &store, Cli::Claude, clock).is_empty());
    }

    fn observed(
        guard: &std::sync::Arc<std::sync::Mutex<AutoRenewGuard>>,
        store: &accounts::Store,
        cli: Cli,
        clock: Instant,
    ) -> Vec<LoginActivity> {
        login_activity_of(&guard.lock().unwrap(), store, cli, 1000, clock)
    }

    #[test]
    fn queued_and_failed_spawns_are_not_active_routes() {
        let guard = std::sync::Arc::new(std::sync::Mutex::new(AutoRenewGuard::default()));
        let store = accounts::Store::at(Path::new("/synthetic/activity-a"));
        let clock = Instant::now();
        let permit = AutoRenewPermit::claim(&guard, "row", clock, false).unwrap();
        permit.attribute(&store, Cli::Claude, "row", 900);
        let queued = std::sync::Arc::new(std::sync::Mutex::new(None));
        let receiver = queued.clone();
        let worker_guard = guard.clone();
        start_auto_renew(
            permit,
            move || {
                let store = accounts::Store::at(Path::new("/synthetic/activity-a"));
                let _activity = LoginActivityGuard::begin(
                    &worker_guard,
                    &store,
                    Cli::Claude,
                    "row",
                    Some("profile"),
                );
                let rows = observed(&worker_guard, &store, Cli::Claude, clock);
                assert!(rows[0].active);
                assert_eq!(rows[0].route.as_deref(), Some("profile"));
            },
            move |work| {
                *receiver.lock().unwrap() = Some(work);
                Ok(())
            },
        )
        .unwrap();
        assert!(!observed(&guard, &store, Cli::Claude, clock)[0].active);
        let work = queued.lock().unwrap().take().unwrap();
        work();
        let rows = observed(&guard, &store, Cli::Claude, clock);
        assert!(!rows[0].active);
        assert_eq!(rows[0].route, None);
        assert_eq!(rows[0].next_at, Some(1900));
        let permit = AutoRenewPermit::claim(&guard, "row", clock, true).unwrap();
        permit.attribute(&store, Cli::Claude, "row", 901);
        assert!(
            start_auto_renew(
                permit,
                || panic!("failed spawn ran work"),
                |_| Err(io::Error::other("spawn failed"))
            )
            .is_err()
        );
        assert!(!observed(&guard, &store, Cli::Claude, clock)[0].active);
        assert!(!guard.lock().unwrap().in_flight);
    }

    #[test]
    fn activity_is_scoped_to_the_attributed_store_and_cli_and_expires() {
        let guard = std::sync::Arc::new(std::sync::Mutex::new(AutoRenewGuard::default()));
        let a = accounts::Store::at(Path::new("/synthetic/activity-a"));
        let b = accounts::Store::at(Path::new("/synthetic/activity-b"));
        let clock = Instant::now();
        let permit = AutoRenewPermit::claim(&guard, "row", clock, false).unwrap();
        permit.attribute(&a, Cli::Claude, "row", 900);
        let active = LoginActivityGuard::begin(&guard, &a, Cli::Claude, "row", None);
        assert!(observed(&guard, &a, Cli::Claude, clock)[0].active);
        assert!(observed(&guard, &a, Cli::Grok, clock).is_empty());
        assert!(observed(&guard, &b, Cli::Claude, clock).is_empty());
        drop(active);
        drop(permit);
        let permit = AutoRenewPermit::claim(&guard, "row", clock, true).unwrap();
        permit.attribute(&b, Cli::Claude, "row", 901);
        assert!(observed(&guard, &a, Cli::Claude, clock).is_empty());
        assert_eq!(observed(&guard, &b, Cli::Claude, clock).len(), 1);
        assert!(observed(&guard, &b, Cli::Claude, clock + Duration::from_secs(901)).is_empty());
    }

    std::thread_local! {
        static HEARD: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    }

    fn hear_activity() {
        let _ = watch_login_activity(Box::new(|| HEARD.with(|n| n.set(n.get() + 1))));
    }

    #[test]
    fn published_login_clears_only_its_attributed_holds() {
        hear_activity();
        let store = accounts::Store::at(Path::new("/synthetic/publication-a"));
        let other = accounts::Store::at(Path::new("/synthetic/publication-b"));
        let owner = |scope| match scope {
            "local" => Some(LoginOwner::new(&store, Cli::Claude, "row", 900)),
            "store" => Some(LoginOwner::new(&other, Cli::Claude, "row", 900)),
            "cli" => Some(LoginOwner::new(&store, Cli::Codex, "row", 900)),
            "label" => Some(LoginOwner::new(&store, Cli::Claude, "other", 900)),
            "unattributed" => None,
            _ => unreachable!(),
        };
        for activity in ["local", "store", "cli", "label", "unattributed"] {
            for ladder in ["local", "store", "cli", "label", "unattributed"] {
                let clock = Instant::now();
                let token = std::sync::Arc::new(());
                let mut state = AutoRenewGuard {
                    in_flight: true,
                    ..AutoRenewGuard::default()
                };
                // Keys need not equal the account label; each domain has its own owner.
                state.last_attempt.insert("key".into(), clock);
                state.parked.insert("key".into(), clock);
                state.bot_checked.insert("key".into(), clock);
                state.ladder.failed("key", "inputs", clock);
                if let Some(owner) = owner(activity) {
                    state.activity_owners.insert("key".into(), owner);
                }
                if let Some(owner) = owner(ladder) {
                    state.ladder_owners.insert("key".into(), owner);
                }
                state.browser_unparked.insert("key".into(), (42, 1));
                state.active_login = Some(ActiveLogin {
                    owner: LoginOwner::new(&store, Cli::Claude, "row", 900),
                    route: Some("profile".into()),
                    token: token.clone(),
                });
                let guard = std::sync::Arc::new(std::sync::Mutex::new(state));
                let claims = LoginClaims::capture(&guard);
                let heard = HEARD.with(std::cell::Cell::get);
                note_published_login(&guard, &claims, &store, Cli::Claude, "row");
                assert_eq!(
                    HEARD.with(std::cell::Cell::get) - heard,
                    u32::from(activity == "local" || ladder == "local")
                );
                let state = guard.lock().unwrap();
                for held in [&state.last_attempt, &state.parked, &state.bot_checked] {
                    assert_eq!(
                        held.contains_key("key"),
                        activity != "local",
                        "{activity}/{ladder}"
                    );
                }
                assert_eq!(
                    state.activity_owners.contains_key("key"),
                    !matches!(activity, "local" | "unattributed"),
                    "{activity}/{ladder}"
                );
                assert_eq!(
                    state.ladder.retry_at("key", clock).is_some(),
                    ladder != "local",
                    "{activity}/{ladder}"
                );
                assert_eq!(
                    state.ladder_owners.contains_key("key"),
                    !matches!(ladder, "local" | "unattributed"),
                    "{activity}/{ladder}"
                );
                assert!(state.in_flight);
                assert_eq!(state.browser_unparked.get("key"), Some(&(42, 1)));
                assert!(std::sync::Arc::ptr_eq(
                    &state.active_login.as_ref().unwrap().token,
                    &token
                ));
            }
        }
    }

    #[test]
    fn a_new_claim_survives_an_older_publication_at_the_same_wall_time() {
        let store = accounts::Store::at(Path::new("/synthetic/claim-replacement"));
        for activity_changed in [false, true] {
            for ladder_changed in [false, true] {
                let clock = Instant::now();
                let mut state = AutoRenewGuard::default();
                state.last_attempt.insert("row".into(), clock);
                state.ladder.failed("row", "input", clock);
                // A wall-clock jump cannot identify which claim was captured first.
                state.activity_owners.insert(
                    "row".into(),
                    LoginOwner::new(&store, Cli::Claude, "row", 5000),
                );
                state.ladder_owners.insert(
                    "row".into(),
                    LoginOwner::new(&store, Cli::Claude, "row", 5000),
                );
                let guard = std::sync::Arc::new(std::sync::Mutex::new(state));
                let claims = LoginClaims::capture(&guard);
                {
                    let mut state = guard.lock().unwrap();
                    if activity_changed {
                        state.activity_owners.insert(
                            "row".into(),
                            LoginOwner::new(&store, Cli::Claude, "row", 5000),
                        );
                    }
                    if ladder_changed {
                        state.ladder_owners.insert(
                            "row".into(),
                            LoginOwner::new(&store, Cli::Claude, "row", 5000),
                        );
                    }
                }
                note_published_login(&guard, &claims, &store, Cli::Claude, "row");
                let state = guard.lock().unwrap();
                assert_eq!(state.last_attempt.contains_key("row"), activity_changed);
                assert_eq!(state.ladder.has_backoff("row"), ladder_changed);
            }
        }
    }

    #[test]
    fn publication_resets_an_expired_but_retained_retry_step() {
        let store = accounts::Store::at(Path::new("/synthetic/expired-retry"));
        let clock = Instant::now();
        let mut state = AutoRenewGuard::default();
        let first = state.ladder.failed("row", "input", clock);
        let later = clock + first;
        assert!(!state.ladder.held("row", "input", later));
        assert!(state.ladder.has_backoff("row"));
        state.ladder_owners.insert(
            "row".into(),
            LoginOwner::new(&store, Cli::Claude, "row", 900),
        );
        let guard = std::sync::Arc::new(std::sync::Mutex::new(state));
        let claims = LoginClaims::capture(&guard);
        note_published_login(&guard, &claims, &store, Cli::Claude, "row");
        let mut state = guard.lock().unwrap();
        assert!(!state.ladder.has_backoff("row"));
        assert_eq!(state.ladder.failed("row", "input", later), first);
    }

    #[test]
    fn a_sign_in_that_begins_or_ends_is_heard_and_one_another_took_over_ends_unheard() {
        // The watch runs on the thread that moved the activity, so this thread's count is its own.
        hear_activity();
        let heard = || HEARD.with(std::cell::Cell::get);
        let guard = std::sync::Arc::new(std::sync::Mutex::new(AutoRenewGuard::default()));
        let store = accounts::Store::at(Path::new("/synthetic/activity-heard"));
        let first = LoginActivityGuard::begin(&guard, &store, Cli::Claude, "row", None);
        assert_eq!(heard(), 1, "a sign-in that began is heard");
        let second = LoginActivityGuard::begin(&guard, &store, Cli::Claude, "row", None);
        assert_eq!(heard(), 2);
        drop(first);
        assert_eq!(
            heard(),
            2,
            "the end of one another took over leaves the activity as it was"
        );
        drop(second);
        assert_eq!(heard(), 3, "a sign-in that ended is heard");
    }
}

/// How long a row whose background renewal stopped for a person, or met Cloudflare's bot check
/// ([`AutoRenewGuard::bot_checked`]), is left alone by the background before it is tried again:
/// a renewal that stops so (accounts.x.ai's block page, a page that waits on a person, a check
/// that did not clear) starts a live device sign-in every time it runs.
const PERSON_BACKOFF: Duration = Duration::from_secs(6 * 60 * 60);
/// [`PERSON_BACKOFF`] in seconds.
pub(crate) const PERSON_BACKOFF_SECS: i64 = 6 * 60 * 60;

fn is_signed_in_browser_window_why(why: &str) -> bool {
    let Some((_, tail)) = why.split_once(" window ") else {
        return false;
    };
    let Some((end, state)) = tail.split_once(' ') else {
        return false;
    };
    state == "session-kept" || (state == "session-unknown" && matches!(end, "done" | "quit"))
}

fn is_person_stop_why(why: &str) -> bool {
    why.ends_with(" needs-sign-in") || why.ends_with(" needs-you") || why.ends_with(" hung")
}

fn browser_window_signed_in_token(
    store: &accounts::Store,
    label: &str,
    since_at: i64,
) -> Option<(i64, usize)> {
    let events = store.tail(accounts::LEDGER_KEEP_ROWS).ok()?;
    let mut last_at: Option<i64> = None;
    let mut same_at_count = 0usize;
    let mut satisfied: Option<(i64, usize)> = None;
    for event in events {
        if event.kind != accounts::Kind::SignIn || event.account != label {
            continue;
        }
        let why = event.why.as_str();
        if is_signed_in_browser_window_why(why) {
            if last_at == Some(event.t) {
                same_at_count += 1;
            } else {
                last_at = Some(event.t);
                same_at_count = 1;
            }
            if event.t >= since_at {
                satisfied = Some((event.t, same_at_count));
            }
        } else if why.contains(" window ") {
            if event.t >= since_at {
                satisfied = None;
            }
        } else if is_person_stop_why(why) {
            satisfied = None;
        }
    }
    satisfied
}

/// When `label`'s newest try on record met Cloudflare's bot check, which did not clear (its
/// ledger line ends `bot-check`, as [`super::cli::renew_one_for`] writes it), the second it
/// ended; `None` once a later sign-in end, signed-in `accounts browser` window, renewal or
/// wrong account is on record, or the ledger cannot be read. It holds the row's background
/// renewal across a restart of aterm, which forgets [`AutoRenewGuard::bot_checked`].
pub(crate) fn bot_check_on_record_at(store: &accounts::Store, label: &str) -> Option<i64> {
    let events = store.tail(accounts::LEDGER_KEEP_ROWS).ok()?;
    let newest = events.iter().rev().find(|event| {
        event.account == label
            && match event.kind {
                accounts::Kind::SignIn => {
                    !event.why.contains(" window ") || is_signed_in_browser_window_why(&event.why)
                }
                accounts::Kind::Renewed | accounts::Kind::WrongAccount => true,
                _ => false,
            }
    })?;
    (newest.kind == accounts::Kind::SignIn && newest.why.ends_with(" bot-check"))
        .then_some(newest.t)
}

/// When the step a person owes `label` (its sign-in task, [`accounts::SignInAction`]) was noted,
/// where one stands: a browser profile to sign in that no signed-in `accounts browser` window has
/// answered since, or a sign-in to approve.
pub(crate) fn unaddressed_sign_in_task_at(
    store: &accounts::Store,
    logins: &accounts::Logins,
    label: &str,
) -> Option<i64> {
    let task = logins.get(label).and_then(|b| b.sign_in_task)?;
    if task.action == accounts::SignInAction::Browser
        && browser_window_signed_in_token(store, label, task.at).is_some()
    {
        return None;
    }
    Some(task.at)
}

fn reconcile_browser_window_unpark(
    state: &std::sync::Arc<std::sync::Mutex<AutoRenewGuard>>,
    key: &str,
    token: Option<(i64, usize)>,
) {
    let Some(token) = token else {
        return;
    };
    let mut guard = state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if guard.browser_unparked.get(key).copied() != Some(token) {
        guard.browser_unparked.insert(key.to_owned(), token);
        guard.parked.remove(key);
        guard.bot_checked.remove(key);
        guard.last_attempt.remove(key);
    }
}

/// Notes how a background renewal of `label` ended: one that stopped for a person
/// ([`super::cli::Renewal::NeedsPerson`]) parks the row until [`PERSON_BACKOFF`] has passed
/// since `now`, and one that met Cloudflare's bot check ([`super::cli::Renewal::BotCheck`])
/// holds it as long, asking no one; a contended lock leaves any hold as it was; any other end
/// lifts both.
fn note_background_end(
    state: &std::sync::Arc<std::sync::Mutex<AutoRenewGuard>>,
    label: &str,
    renewal: super::cli::Renewal,
    now: Instant,
) {
    use super::cli::Renewal;
    let mut guard = state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match renewal {
        Renewal::NeedsPerson => {
            guard.parked.insert(label.to_owned(), now);
        }
        Renewal::BotCheck => {
            guard.parked.remove(label);
            guard.bot_checked.insert(label.to_owned(), now);
        }
        Renewal::Busy | Renewal::Deferred => {}
        Renewal::Renewed
        | Renewal::OtherRenewed
        | Renewal::AuthFailed
        | Renewal::WrongAccount
        | Renewal::Failed => {
            guard.parked.remove(label);
            guard.bot_checked.remove(label);
        }
    }
}

/// Whether Claude Code's store or any pooled CLI's store under `state_dir` holds a saved
/// explicit account selection (`pending_pin`) or an unfinished account commit that upkeep
/// must recover before it can determine whether a selection is pending.
#[must_use]
pub fn any_pending_pin(state_dir: &Path, now: i64) -> bool {
    accounts::Cli::ALL.into_iter().any(|cli| {
        let store = accounts::Store::at_for(state_dir, cli, Path::new(""));
        store.sync_needs_recovery()
            || store
                .load(now)
                .is_ok_and(|(state, _)| state.pending_pin.is_some())
    })
}

static AUTO_RENEW_GUARD: std::sync::LazyLock<std::sync::Arc<std::sync::Mutex<AutoRenewGuard>>> =
    std::sync::LazyLock::new(|| {
        std::sync::Arc::new(std::sync::Mutex::new(AutoRenewGuard {
            reads: MachineReads::live(),
            ..AutoRenewGuard::default()
        }))
    });

struct AutoRenewPermit {
    guard: std::sync::Arc<std::sync::Mutex<AutoRenewGuard>>,
    wake_pending: Option<PathBuf>,
    key: String,
}

impl AutoRenewPermit {
    #[cfg(test)]
    fn claim(
        state: &std::sync::Arc<std::sync::Mutex<AutoRenewGuard>>,
        label: &str,
        now: Instant,
        ignore_cooldown: bool,
    ) -> Option<Self> {
        Self::claim_with_mailbox(state, label, now, ignore_cooldown, None)
    }

    fn claim_with_mailbox(
        state: &std::sync::Arc<std::sync::Mutex<AutoRenewGuard>>,
        label: &str,
        now: Instant,
        ignore_cooldown: bool,
        mailbox_mtime: Option<std::time::SystemTime>,
    ) -> Option<Self> {
        let mut guard = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if mailbox_mtime.is_some() && guard.mailbox_mtime != mailbox_mtime {
            guard.mailbox_mtime = mailbox_mtime;
            guard.last_attempt.clear();
        }
        if guard.in_flight
            || (!ignore_cooldown
                && (guard.last_attempt.get(label).is_some_and(|last| {
                    now.saturating_duration_since(*last) < Duration::from_secs(900)
                }) || guard
                    .parked
                    .get(label)
                    .is_some_and(|since| now.saturating_duration_since(*since) < PERSON_BACKOFF)
                    || guard.bot_checked.get(label).is_some_and(|since| {
                        now.saturating_duration_since(*since) < PERSON_BACKOFF
                    })))
        {
            return None;
        }
        guard.in_flight = true;
        guard.last_attempt.insert(label.to_owned(), now);
        Some(Self {
            guard: std::sync::Arc::clone(state),
            wake_pending: None,
            key: label.to_owned(),
        })
    }

    fn attribute(&self, store: &accounts::Store, cli: Cli, label: &str, now: i64) {
        self.guard
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .activity_owners
            .insert(self.key.clone(), LoginOwner::new(store, cli, label, now));
    }
}

impl Drop for AutoRenewPermit {
    fn drop(&mut self) {
        self.guard
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .in_flight = false;
        // Publish the wake only after admission is available to the next upkeep.
        if let Some(state_dir) = &self.wake_pending {
            let now = crate::supervise::limit::unix_now();
            let root_store = accounts::Store::at(state_dir);
            let mut any_pending = root_store
                .load(now)
                .is_ok_and(|(state, _)| state.pending_pin.is_some());
            for cli in OTHER_CLIS {
                let pool_store = accounts::Store::at_for(state_dir, cli, Path::new(""));
                if pool_store
                    .load(now)
                    .is_ok_and(|(state, _)| state.pending_pin.is_some())
                {
                    let _ = pool_store.touch_wake();
                    any_pending = true;
                }
            }
            if any_pending {
                let _ = root_store.touch_wake();
            }
        }
    }
}

fn start_auto_renew(
    permit: AutoRenewPermit,
    work: impl FnOnce() + Send + 'static,
    spawn: impl FnOnce(Box<dyn FnOnce() + Send>) -> io::Result<()>,
) -> io::Result<()> {
    spawn(Box::new(move || {
        let _permit = permit;
        work();
    }))
}

pub(super) fn resolve_claude_program(home: &Path) -> PathBuf {
    if let Some(twin) = super::upgrade_drive::managed_agent_twin("claude") {
        return twin;
    }
    let local_bin = home.join(".local/bin/claude");
    if local_bin.is_file() {
        return local_bin;
    }
    PathBuf::from("claude")
}

fn resolve_opener_link(home: &Path, store: &accounts::Store) -> Option<PathBuf> {
    let custom_opener = home.join(".local/bin").join(super::open_url::OPENER);
    if custom_opener.is_file() {
        struct HomeOpenerHost(PathBuf);
        impl super::cli::AccountsHost for HomeOpenerHost {
            fn stdin_is_terminal(&self) -> bool {
                false
            }
            fn auth_status(&self, _: &str) -> Result<super::align::Captured, String> {
                Err("unused".to_owned())
            }
            fn unaskable(&self) -> Option<String> {
                None
            }
            fn login(&self, _: Option<&str>) -> Result<i32, String> {
                Err("unused".to_owned())
            }
            fn now(&self) -> i64 {
                0
            }
            fn renew_flow(
                &self,
                _: &str,
                _: &str,
                _: &super::cli::RenewBrowser,
            ) -> Result<i32, String> {
                Err("unused".to_owned())
            }
            fn status_in_home(&self, _: &str) -> Result<super::align::Captured, String> {
                Err("unused".to_owned())
            }
            fn current_exe(&self) -> Option<PathBuf> {
                Some(self.0.clone())
            }
        }
        return super::cli::opener_link(store, &HomeOpenerHost(custom_opener));
    }
    super::cli::opener_link(store, &super::cli::LiveAccounts)
}

pub(super) fn resolve_mailbox_dir(home: Option<&Path>, store: &accounts::Store) -> Option<PathBuf> {
    resolve_mailbox_dir_with(&|name| std::env::var_os(name), home, store)
}

/// [`resolve_mailbox_dir`] reading the environment through `var`.
pub(super) fn resolve_mailbox_dir_with(
    var: &dyn Fn(&str) -> Option<std::ffi::OsString>,
    home: Option<&Path>,
    store: &accounts::Store,
) -> Option<PathBuf> {
    if let Some(env_val) = var(super::open_url::MAILBOX_ENV).filter(|v| !v.is_empty()) {
        let path = PathBuf::from(env_val);
        if path.is_dir() || path.is_file() {
            return Some(path);
        }
    }
    let store_mailbox = store.dir.join(super::open_url::DEFAULT_MAILBOX_DIR);
    if store_mailbox.is_dir() {
        return Some(store_mailbox);
    }
    if let Some(home) = home {
        let config_mailbox = home
            .join(".config/aterm")
            .join(super::open_url::DEFAULT_MAILBOX_DIR);
        if config_mailbox.is_dir() {
            return Some(config_mailbox);
        }
    }
    None
}

pub(super) fn has_configured_mailbox_for_row(
    home: Option<&Path>,
    store: &accounts::Store,
    label: Option<&str>,
    target_email: Option<&str>,
) -> bool {
    has_configured_mailbox_for_row_with(
        &|name| std::env::var_os(name),
        home,
        store,
        label,
        target_email,
    )
}

/// [`has_configured_mailbox_for_row`] reading the environment through `var`.
pub(super) fn has_configured_mailbox_for_row_with(
    var: &dyn Fn(&str) -> Option<std::ffi::OsString>,
    home: Option<&Path>,
    store: &accounts::Store,
    label: Option<&str>,
    target_email: Option<&str>,
) -> bool {
    resolve_mailbox_dir_with(var, home, store).is_some()
        || super::headless_oauth::discover_mailbox_config_with_env(home, Some(&store.dir), var)
            .is_some_and(|mut cfg| {
                cfg.alias_label_to_email(label, target_email);
                cfg.is_configured_for(target_email)
            })
}

/// [`latest_mailbox_mtime_with`] reading no environment, as a test's guard reads it.
#[cfg(test)]
fn latest_mailbox_mtime(
    home: Option<&Path>,
    store: &accounts::Store,
) -> Option<std::time::SystemTime> {
    latest_mailbox_mtime_with(&|_| None, home, store)
}

/// The newest modification time among the files a sign-in reads (each file in `~/.secrets`,
/// the configuration and roster files, the mailbox configurations, and the messages in the mail
/// folders, the environment `var` reads naming one): a change
/// here ends an automatic sign-in's cooldown. Neither the `~/.secrets` folder's own time nor
/// the vault's `state.toml`, which its subscriber rewrites about once a minute only to say when
/// it last heard the vault, is among them, and nor is a mail folder's own time, which the mark a
/// sign-in leaves beside a message it used (`.clicked`) moves: a sign-in's own reading never ends
/// its cooldown. A future timestamp is not a new input: it neither ends a retry hold nor hides
/// an earlier message that has arrived by the time of this read.
fn latest_mailbox_mtime_with(
    var: &dyn Fn(&str) -> Option<std::ffi::OsString>,
    home: Option<&Path>,
    store: &accounts::Store,
) -> Option<std::time::SystemTime> {
    latest_mailbox_mtime_at(var, home, store, std::time::SystemTime::now())
}

/// Read mailbox inputs as of `at`, excluding future timestamps before selecting the newest.
fn latest_mailbox_mtime_at(
    var: &dyn Fn(&str) -> Option<std::ffi::OsString>,
    home: Option<&Path>,
    store: &accounts::Store,
    at: std::time::SystemTime,
) -> Option<std::time::SystemTime> {
    let mut paths = vec![store.dir.join(super::headless_oauth::MAILBOX_TOML_FILE)];
    if let Some(h) = home {
        let secrets_dir = h.join(".secrets");
        if let Ok(entries) = std::fs::read_dir(&secrets_dir) {
            for entry in entries.flatten() {
                if entry.file_type().is_ok_and(|ft| ft.is_file()) {
                    paths.push(entry.path());
                }
            }
        }
        paths.push(h.join(".config/aterm/aterm.toml"));
        paths.push(h.join(".config/aterm/accounts.toml"));
        paths.push(h.join(".config/aterm").join(FLEET_ROSTER_FILE));
        paths.push(
            h.join(".secrets")
                .join(super::headless_oauth::SECRETS_MAILBOX_TOML_FILE),
        );
        paths.push(
            h.join(".secrets")
                .join(super::headless_oauth::MAILBOX_TOML_FILE),
        );
        paths.push(
            h.join(".config/aterm")
                .join(super::headless_oauth::MAILBOX_TOML_FILE),
        );
    }
    let push_maildir_paths = |paths: &mut Vec<PathBuf>, root: &Path| {
        // A mailbox that is one file (an mbox) counts by its own time.
        if root.is_file() {
            paths.push(root.to_path_buf());
            return;
        }
        for dir in [root.to_path_buf(), root.join("new"), root.join("cur")] {
            if let Ok(entries) = std::fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    let name = entry.file_name();
                    let name_str = name.to_string_lossy();
                    if !name_str.starts_with('.')
                        && !name_str.ends_with(".clicked")
                        && !name_str.ends_with(".seen")
                        && entry.file_type().is_ok_and(|ft| ft.is_file())
                    {
                        paths.push(entry.path());
                    }
                }
            }
        }
    };
    if let Some(mb_dir) = resolve_mailbox_dir_with(var, home, store) {
        push_maildir_paths(&mut paths, &mb_dir);
    }
    if let Some(cfg_dir) =
        super::headless_oauth::discover_mailbox_config_with_env(home, Some(&store.dir), var)
            .and_then(|c| c.dir)
    {
        push_maildir_paths(&mut paths, &cfg_dir);
    }
    paths
        .into_iter()
        .filter_map(|p| std::fs::metadata(p).ok()?.modified().ok())
        .filter(|mtime| *mtime <= at)
        .max()
}

/// Claude Code's [`auto_renew_candidates_tiered`] rows, in its order.
#[cfg(test)]
pub(super) fn auto_renew_candidates(
    file_roster: &Roster,
    state: &accounts::State,
    logins: &accounts::Logins,
    now: i64,
) -> Vec<accounts::Row> {
    auto_renew_candidates_tiered(Cli::Claude, None, file_roster, state, logins, now)
        .into_iter()
        .map(|(_, row)| row)
        .collect()
}

/// The first tier no session needs yet ([`auto_renew_candidates_tiered`]' tiers 2 and 3): the
/// rows Claude Code's upkeep signs in by itself, and a person's `accounts renew --due`, the
/// fact menu's rotation or a pooled CLI's upkeep never does ([`needed_renew_candidates_for`]).
const UNNEEDED_TIER: u8 = 2;

/// A scheduling observation, not permission to authenticate. Workers still admit
/// its raw policy and login generation again before starting an attempt.
struct RenewalCandidates {
    records: accounts::AccountRecords,
    tiered: Vec<(u8, accounts::Row)>,
}

/// Keep candidate ranking and the worker's captured policy in the same completed
/// account publication. Credential presence and sign-in task history remain
/// independent observations, as in the existing ranking policy.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountRenewalRead",
        action = "Read",
        project = "aterm_agent::harness::accounts_sync::renewal_read_tests::renewal_read_transition"
    )
)]
fn read_renewal_candidates(
    cli: Cli,
    store: &accounts::Store,
    roster_path: &Path,
    now: i64,
) -> Result<RenewalCandidates, accounts::RosterReadError> {
    let records = store.read_account_records(Some(roster_path), now)?;
    let tiered = auto_renew_candidates_tiered(
        cli,
        Some(store),
        &records.raw_roster,
        &records.state,
        &records.logins,
        now,
    );
    Ok(RenewalCandidates { records, tiered })
}

/// Claude Code's [`needed_renew_candidates_for`], ordered without a store.
pub(super) fn needed_renew_candidates(
    file_roster: &Roster,
    state: &accounts::State,
    logins: &accounts::Logins,
    now: i64,
) -> Vec<accounts::Row> {
    needed_renew_candidates_for(Cli::Claude, None, file_roster, state, logins, now)
}

/// The rows of `cli`'s pool a session needs signed in now, best first:
/// [`auto_renew_candidates_tiered`]' tiers 0 and 1 (the successors of a walled, low, signed-out
/// or warned session where none is ready, and the logins due for renewal), what a person's
/// `accounts renew --due`, the fact menu's rotation and a pooled CLI's upkeep start.
pub(super) fn needed_renew_candidates_for(
    cli: Cli,
    store: Option<&accounts::Store>,
    file_roster: &Roster,
    state: &accounts::State,
    logins: &accounts::Logins,
    now: i64,
) -> Vec<accounts::Row> {
    auto_renew_candidates_tiered(cli, store, file_roster, state, logins, now)
        .into_iter()
        .filter(|(tier, _)| *tier < UNNEEDED_TIER)
        .map(|(_, row)| row)
        .collect()
}

/// THE ROWS AN UPKEEP SIGNS IN for `cli`'s pool, best first, each with its tier, every pass and
/// by default: every row with an email that has no login it can be run on here (not enrolled,
/// or signed out) is one, whether or not any session needs it yet. Tier 0, first every row a
/// session runs on whose login is signed out, whatever else is ready, then the rows that would
/// carry a session whose account is walled, low, signed out or already warned that a window is
/// filling (Claude Code's first warning, well before the account is low), where no other row is
/// ready to take it; tier 1, the enrolled logins due for renewal or ended; tier 2, every other
/// row to sign in; tier 3, the rows to sign in that are themselves walled, ready once their wall
/// ends. Within a tier, the signed-out rows sessions run on first, then roster order (dedicated
/// rows first in tiers 0, 2 and 3), a row with an unaddressed sign-in task after every row without
/// one and the older task first (read through `store` where one is given, else from `logins`
/// alone), a row whose last renewal answered another account last.
fn auto_renew_candidates_tiered(
    cli: Cli,
    store: Option<&accounts::Store>,
    file_roster: &Roster,
    state: &accounts::State,
    logins: &accounts::Logins,
    now: i64,
) -> Vec<(u8, accounts::Row)> {
    let mut tiered = rank_renewal_candidates(cli, store, file_roster, state, logins, now);
    // Ordinary renewal never substitutes for fresh root authorization.
    tiered.retain(|(_, row)| matches!(logins.shared.required(row), Ok(None)));
    tiered
}

fn rank_renewal_candidates(
    cli: Cli,
    store: Option<&accounts::Store>,
    file_roster: &Roster,
    state: &accounts::State,
    logins: &accounts::Logins,
    now: i64,
) -> Vec<(u8, accounts::Row)> {
    let roster = accounts::bound(file_roster, logins);
    let enrolled = accounts::enrolled_for(cli, &roster);
    let is_walled_or_low = |label: &str| {
        state.accounts.get(label).is_some_and(|a| {
            a.limited_until(now).is_some()
                || a.low(now)
                || a.warned(now)
                || a.auth == accounts::Auth::SignedOut
        })
    };
    let is_limited = |label: &str| {
        state
            .accounts
            .get(label)
            .is_some_and(|a| a.limited_until(now).is_some())
    };
    let wants_sign_in = |row: &accounts::Row| {
        let is_enr = row.dir.is_none() || enrolled.contains(&row.label);
        row.held_by.is_none()
            && row.email.is_some()
            && matches!(
                accounts::condition(row, state, is_enr, now),
                accounts::Condition::NotEnrolled | accounts::Condition::SignedOut
            )
    };
    let me = state
        .active
        .as_deref()
        .filter(|label| roster.row(label).is_some())
        .or_else(|| {
            state
                .sessions
                .values()
                .map(|s| s.account.as_str())
                .find(|label| roster.row(label).is_some() && is_walled_or_low(label))
        })
        .or_else(|| roster.row_for_login(None).map(|row| row.label.as_str()))
        .or_else(|| roster.rows.first().map(|row| row.label.as_str()));
    let any_walled = me.is_some_and(is_walled_or_low)
        || state
            .sessions
            .values()
            .any(|s| is_walled_or_low(&s.account));
    let mut tiered: Vec<(u8, accounts::Row)> = Vec::new();
    let mut seen = BTreeSet::new();
    // A ROW A SESSION RUNS ON WHOSE LOGIN IS SIGNED OUT comes first, whatever else is ready, and
    // ahead of every other row of tier 0 whatever its sign-in task or wrong account says
    // (`waited_on`, the sort's second key): that session's lost login follows only its own row
    // (`accounts::Point::LoginEnded`), and its supervisor waits on this sign-in rather than
    // typing `/login` in its tab.
    let mut waited_on = BTreeSet::new();
    for row in roster
        .rows
        .iter()
        .filter(|r| r.class == Class::Dedicated)
        .chain(roster.rows.iter().filter(|r| r.class == Class::Shared))
        .filter(|row| {
            state.sessions.values().any(|s| s.account == row.label)
                && wants_sign_in(row)
                && !is_limited(&row.label)
                && state
                    .accounts
                    .get(&row.label)
                    .is_some_and(|a| a.auth == accounts::Auth::SignedOut)
        })
    {
        if seen.insert(row.label.clone()) {
            waited_on.insert(row.label.clone());
            tiered.push((0, row.clone()));
        }
    }
    if any_walled {
        let wall_point = accounts::Point::Wall {
            window: String::new(),
            at: None,
        };
        let has_ready_candidate =
            accounts::candidates(&roster, state, &enrolled, me, &wall_point, now)
                .iter()
                .any(|(_, verdict)| verdict.is_ok());
        if !has_ready_candidate {
            for next_row in roster
                .rows
                .iter()
                .filter(|r| r.class == Class::Dedicated)
                .chain(roster.rows.iter().filter(|r| r.class == Class::Shared))
                .filter(|row| me != Some(row.label.as_str()))
                .filter(|row| wants_sign_in(row) && !is_limited(&row.label))
            {
                if seen.insert(next_row.label.clone()) {
                    tiered.push((0, next_row.clone()));
                }
            }
            if let Some(me_label) = me
                && state.accounts.get(me_label).is_some_and(|a| {
                    a.auth == accounts::Auth::SignedOut && a.limited_until(now).is_none()
                })
                && let Some(me_row) = roster.row(me_label).cloned()
                && me_row.held_by.is_none()
                && me_row.email.is_some()
                && seen.insert(me_row.label.clone())
            {
                tiered.push((0, me_row));
            }
        }
    }
    for label in accounts::renew_due(&roster, logins, state, &enrolled, now) {
        if is_limited(&label) {
            continue;
        }
        if let Some(row) = roster.row(&label)
            && row.held_by.is_none()
            && row.email.is_some()
            && seen.insert(row.label.clone())
        {
            tiered.push((1, row.clone()));
        }
    }
    for next_row in roster
        .rows
        .iter()
        .filter(|r| r.class == Class::Dedicated)
        .chain(roster.rows.iter().filter(|r| r.class == Class::Shared))
        .filter(|row| wants_sign_in(row))
    {
        let tier = if is_limited(&next_row.label) {
            UNNEEDED_TIER + 1
        } else {
            UNNEEDED_TIER
        };
        if seen.insert(next_row.label.clone()) {
            tiered.push((tier, next_row.clone()));
        }
    }
    tiered.sort_by_key(|(tier, row)| {
        let task_at = match store {
            Some(store) => unaddressed_sign_in_task_at(store, logins, &row.label),
            None => logins
                .get(&row.label)
                .and_then(|b| b.sign_in_task.map(|t| t.at)),
        };
        (
            *tier,
            !waited_on.contains(&row.label),
            task_at,
            logins.get(&row.label).and_then(|b| b.wrong_at),
        )
    });
    tiered
}

#[cfg(test)]
pub(super) fn next_auto_renew_target(
    file_roster: &Roster,
    state: &accounts::State,
    logins: &accounts::Logins,
    now: i64,
) -> Option<accounts::Row> {
    auto_renew_candidates(file_roster, state, logins, now)
        .into_iter()
        .next()
}

/// Publish the verified staged login. Notify its actual label only after the
/// transaction succeeds and releases its writer; failures never notify.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountCompletionIdentity",
        action = "Complete",
        project = "aterm_agent::harness::accounts_sync::completion_identity_tests::completion_identity_transition"
    )
)]
fn complete_staging_renewal_result(
    store: &accounts::Store,
    policy: accounts::RenewalPolicy<'_>,
    staging: &Path,
    staging_text: &str,
    (status_got, success_route): (&super::align::Captured, Option<&str>),
    now_done: i64,
    on_published: &dyn Fn(&str),
) -> super::cli::Renewal {
    use super::cli::Renewal;
    let file_roster = policy.roster;
    let captured_logins = policy.logins;
    let captured_rows = accounts::bound(file_roster, captured_logins);
    let requested_label = policy.row.label.as_str();

    // A failed status cannot qualify any row, irrespective of the store's bindings.
    let facts = super::cli::status_facts(status_got, "");
    if facts
        .as_ref()
        .is_none_or(|facts| !facts.signed_in || !facts.claude_ai)
    {
        let _ = fs::remove_dir_all(staging);
        return if facts.is_some() {
            Renewal::authentication_failed(status_got.code)
        } else {
            Renewal::Failed
        };
    }
    let mut note = None;
    let mut outcome = Renewal::Failed;
    let mut published_label = None;
    let result = store.transact_renewal_within(
        &accounts::LOCK_LADDER,
        now_done,
        &mut note,
        policy,
        |state, logins| {
            let roster = accounts::bound(file_roster, logins);
            let enrolled = accounts::enrolled(&roster);
            let Some(next_row) = roster.row(requested_label) else {
                return Ok(Vec::new());
            };
            let wrong_account = next_row
                .expected_identity_email()
                .and_then(|email| super::cli::status_facts(status_got, email))
                .is_some_and(|facts| facts.signed_in && facts.claude_ai && !facts.email_matches);
            let matched = std::iter::once(next_row)
                .chain(roster.rows.iter().filter(|r| r.label != requested_label))
                .find_map(|row| {
                    let email = row.expected_identity_email()?;
                    let facts = super::cli::status_facts(status_got, email)?;
                    if !facts.renews()
                        || state
                            .accounts
                            .get(&row.label)
                            .is_some_and(|a| a.limited_until(now_done).is_some())
                    {
                        return None;
                    }
                    if row.label != requested_label {
                        let is_enr = row.dir.is_none() || enrolled.contains(&row.label);
                        let cond = accounts::condition(row, state, is_enr, now_done);
                        let lstate = accounts::login_state(row, logins, state, now_done);
                        let unobserved_unbound = !logins.labels.contains_key(&row.label)
                            && state.accounts.get(&row.label).is_none_or(|a| !a.observed());
                        if !(unobserved_unbound
                            || matches!(
                                cond,
                                accounts::Condition::NotEnrolled | accounts::Condition::SignedOut
                            )
                            || matches!(
                                lstate,
                                accounts::LoginState::Expired | accounts::LoginState::Due { .. }
                            ))
                        {
                            return None;
                        }
                    }
                    Some((row.label.clone(), email.to_owned(), facts.subscription))
                });
            let Some((matched_label, matched_email, subscription)) = matched else {
                return Ok(if wrong_account {
                    outcome = Renewal::WrongAccount;
                    accounts::note_wrong_account(logins, requested_label, now_done)
                } else {
                    Vec::new()
                });
            };
            accounts::RenewalParent::capture(captured_logins, &matched_label)
                .require_current(logins, &matched_label)?;
            if captured_rows.row(&matched_label) != roster.row(&matched_label) {
                return Err(format!(
                    "account `{matched_label}` changed during sign-in; nothing recorded"
                ));
            }
            let matched_row = roster
                .row(&matched_label)
                .ok_or_else(|| "matched account is no longer listed".to_owned())?;
            if !accounts::login_made(staging) {
                return Err("staged Claude login disappeared before publication".into());
            }
            let observed = super::profile::identity_in_home(staging, &matched_email);
            if observed.email_matches == Some(false) {
                return Err("staged Claude login contradicts its completed status check".into());
            }
            let principal = if logins.shared.is_empty() {
                None
            } else {
                observed.principal
            };
            super::cli::shared_renewal::unmanaged(logins, matched_row, principal, true)?;
            accounts::write_renewal_checkpoint(
                &matched_label,
                &matched_email,
                staging_text,
                subscription.as_deref(),
                now_done,
                logins,
            )
            .map_err(|why| why.to_string())?;
            let own_dir = file_roster
                .row(&matched_label)
                .and_then(|r| r.dir.as_deref());
            let requested = matched_label == requested_label;
            outcome = if requested {
                Renewal::Renewed
            } else {
                Renewal::OtherRenewed
            };
            let mut events = accounts::note_renewed(
                state,
                logins,
                &matched_label,
                own_dir,
                staging_text,
                subscription.as_deref(),
                now_done,
            );
            if !requested {
                events.extend(accounts::note_wrong_account(
                    logins,
                    requested_label,
                    now_done,
                ));
            }
            if let Some(route) = success_route {
                events.extend(accounts::note_sign_in_attempt(
                    logins,
                    &matched_label,
                    accounts::AttemptOutcome::SignedIn,
                    route,
                    now_done,
                ));
                if !requested {
                    events.extend(accounts::note_sign_in_attempt(
                        logins,
                        requested_label,
                        accounts::AttemptOutcome::Failed,
                        &format!("{route}: another configured account signed in"),
                        now_done,
                    ));
                }
            }
            published_label = Some(matched_label);
            Ok(events)
        },
    );
    match result {
        Ok(_) => {
            if let Some(label) = published_label {
                on_published(&label);
            } else {
                let _ = fs::remove_dir_all(staging);
            }
            outcome
        }
        Err(why) => {
            let _ = writeln!(
                io::stderr(),
                "aterm harness accounts renew: {requested_label}: completion could not be recorded \
                 ({}); its home is kept at {}",
                super::one_line(&why, 160),
                staging.display(),
            );
            Renewal::Failed
        }
    }
}

/// Connect verified publication to the local claims captured before it.
fn complete_auto_staging_renewal(
    store: &accounts::Store,
    policy: accounts::RenewalPolicy<'_>,
    staging: &Path,
    staging_text: &str,
    status: (&super::align::Captured, Option<&str>),
    now_done: i64,
    (guard, cli): (&std::sync::Arc<std::sync::Mutex<AutoRenewGuard>>, Cli),
) -> super::cli::Renewal {
    let claims = LoginClaims::capture(guard);
    complete_staging_renewal_result(
        store,
        policy,
        staging,
        staging_text,
        status,
        now_done,
        &|label| note_published_login(guard, &claims, store, cli, label),
    )
}

/// Fixture choices for automatic completion's captured status and local identity.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountCompletionIdentity",
        action = "Shared",
        reason = "The native fixture supplies an empty store or unrelated retained shared custody."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountCompletionIdentity",
        action = "Alternate",
        reason = "The captured provider status selects the requested row or an eligible alternate row."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountCompletionIdentity",
        action = "Credential",
        reason = "The native fixture changes the profile after capturing successful provider status."
    )
)]
#[doc(hidden)]
pub fn account_completion_identity_environment() {}

#[cfg(test)]
fn complete_staging_renewal(
    store: &accounts::Store,
    file_roster: &Roster,
    requested_label: &str,
    staging: &Path,
    staging_text: &str,
    status_got: &super::align::Captured,
    now_done: i64,
) -> bool {
    let fixture = aterm_tempfile::TempDir::new().unwrap();
    let roster_path = fixture.path().join(accounts::ACCOUNTS_FILE);
    fs::write(&roster_path, format_roster_toml(file_roster).unwrap()).unwrap();
    let Ok((logins, _)) = store.load_logins(now_done) else {
        return false;
    };
    let captured = accounts::bound(file_roster, &logins);
    let Some(row) = captured.row(requested_label) else {
        return false;
    };
    complete_staging_renewal_result(
        store,
        accounts::RenewalPolicy {
            roster_path: &roster_path,
            roster: file_roster,
            row,
            logins: &logins,
        },
        staging,
        staging_text,
        (status_got, None),
        now_done,
        &|_| {},
    )
    .published()
}

fn select_auto_renew_browser(
    row_browser: Option<&str>,
    mailbox_dir: Option<&Path>,
    opener: Option<PathBuf>,
) -> Option<super::cli::RenewBrowser> {
    let opener = opener?;
    if mailbox_dir.is_some() {
        return Some(super::cli::RenewBrowser::Remote {
            opener: Some(opener),
        });
    }
    row_browser.map(|profile| super::cli::RenewBrowser::Profile {
        opener,
        profile: profile.to_owned(),
    })
}

#[cfg(test)]
fn recover_interrupted_staging_renewals(
    store: &accounts::Store,
    file_roster: &Roster,
    renew_guard: &std::sync::Arc<std::sync::Mutex<AutoRenewGuard>>,
    now: i64,
) -> bool {
    let fixture = aterm_tempfile::TempDir::new().unwrap();
    let path = fixture.path().join(accounts::ACCOUNTS_FILE);
    fs::write(&path, format_roster_toml(file_roster).unwrap()).unwrap();
    recover_interrupted_staging_renewals_for(Cli::Claude, store, &path, renew_guard, now)
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountRecoveryPolicy",
        action = "Recover",
        project = "aterm_agent::harness::accounts_sync::recovery_policy_tests::recovery_transition"
    )
)]
fn recover_interrupted_staging_renewals_for(
    cli: Cli,
    store: &accounts::Store,
    roster_path: &Path,
    renew_guard: &std::sync::Arc<std::sync::Mutex<AutoRenewGuard>>,
    now: i64,
) -> bool {
    recover_interrupted_staging_renewals_with(
        cli,
        store,
        roster_path,
        renew_guard,
        now,
        &accounts::LOCK_LADDER,
        || {},
    )
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountRecoveryLock",
        action = "Recover",
        project = "aterm_agent::harness::accounts_sync::recovery_lock_tests::recovery_lock_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedCheckpoint",
        action = "Recover",
        project = "aterm_agent::harness::cli::tests::shared_renewal_tests::checkpoint_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountRecoveryIdentity",
        action = "Recover",
        project = "aterm_agent::harness::accounts_sync::recovery_lock_tests::recovery_identity_transition"
    )
)]
fn recover_interrupted_staging_renewals_with(
    cli: Cli,
    store: &accounts::Store,
    roster_path: &Path,
    renew_guard: &std::sync::Arc<std::sync::Mutex<AutoRenewGuard>>,
    now: i64,
    ladder: &[Duration],
    mut on_locked: impl FnMut(),
) -> bool {
    if renew_guard
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .in_flight
    {
        return false;
    }
    let mut any_recovered = match store.recover_sync_within(ladder, roster_path) {
        Ok(recovered) => recovered,
        Err(_) => return false,
    };
    let Ok(entries) = fs::read_dir(store.logins_dir()) else {
        return any_recovered;
    };
    struct Candidate {
        prefix: String,
        sequence: u64,
        dir: PathBuf,
        actual_dir: PathBuf,
        proof: accounts::RenewalCheckpoint,
    }
    let has_required_login = |dir: &Path| {
        if cli == Cli::Claude {
            accounts::login_made(dir)
        } else {
            cli.connector()
                .stores
                .iter()
                .any(|store_file| dir.join(store_file).is_file())
        }
    };
    let mut candidates: Vec<Candidate> = entries
        .flatten()
        .filter_map(|entry| {
            if !entry.file_type().is_ok_and(|ft| ft.is_dir()) {
                return None;
            }
            let name = entry.file_name().into_string().ok()?;
            let (prefix, suffix) = name.rsplit_once('+')?;
            if prefix.is_empty() || suffix.is_empty() || !suffix.bytes().all(|b| b.is_ascii_digit())
            {
                return None;
            }
            let sequence = suffix.parse().ok()?;
            let dir = entry.path();
            if !has_required_login(&dir) {
                return None;
            }
            let actual_dir = fs::canonicalize(&dir).ok()?;
            // A vendor profile is only a local hint. Without the result of the
            // check immediately after fresh sign-in, preserve this work unadopted.
            let proof = accounts::read_renewal_checkpoint_for(cli, &dir, &actual_dir, now)?;
            Some(Candidate {
                prefix: prefix.to_owned(),
                sequence,
                dir,
                actual_dir,
                proof,
            })
        })
        .collect();
    if candidates.is_empty() {
        return any_recovered;
    }
    // Recovery publishes a sign-in too. Keep the machine-wide guard until every
    // candidate transaction ends, including when the lock file did not exist.
    // The account writer is acquired below, in the same order as fresh sign-ins.
    let Ok(Some(_signin)) = store.signin_now() else {
        return any_recovered;
    };
    on_locked();
    // The unlocked scan is only a hint. Refresh proofs after admission so a
    // removed or changed checkpoint cannot be published from that earlier read.
    candidates.retain_mut(|candidate| {
        if fs::canonicalize(&candidate.dir).ok().as_ref() != Some(&candidate.actual_dir) {
            return false;
        }
        let Some(proof) =
            accounts::read_renewal_checkpoint_for(cli, &candidate.dir, &candidate.actual_dir, now)
        else {
            return false;
        };
        candidate.proof = proof;
        true
    });
    // A check can match a row other than the requested staging prefix. Prefer
    // its recorded completion clock across prefixes, then numeric sequence
    // within a prefix for equal clocks. Neither is a vendor credential age.
    candidates.sort_by(|a, b| {
        b.proof
            .checked_at
            .cmp(&a.proof.checked_at)
            .then_with(|| a.prefix.cmp(&b.prefix))
            .then_with(|| b.sequence.cmp(&a.sequence))
            .then_with(|| a.dir.cmp(&b.dir))
    });

    let mut recovered_labels = BTreeSet::new();
    for Candidate {
        dir,
        actual_dir,
        proof,
        ..
    } in candidates
    {
        if recovered_labels.contains(&proof.label) {
            continue;
        }
        // All candidates use the same writer. Do not multiply its contention
        // wait by the number of checkpoints while holding every provider's sign-in.
        let Ok(Some(writer)) = store.acquire_within(ladder) else {
            on_locked();
            break;
        };
        let mut note = None;
        if store
            .transact_roster_logins_held(
                &writer,
                roster_path,
                now,
                &mut note,
                |current, state, logins| {
                    // A prior candidate's transaction or this writer wait may outlive
                    // the refreshed proof. Keep the selected path and proof together.
                    if fs::canonicalize(&dir).ok().as_ref() != Some(&actual_dir)
                        || !has_required_login(&actual_dir)
                        || accounts::read_renewal_checkpoint_for(cli, &dir, &actual_dir, now)
                            .as_ref()
                            != Some(&proof)
                    {
                        return Ok(Vec::new());
                    }
                    // A checkpoint can outlive the configuration read by upkeep.
                    // Every provider resolves its identity policy under the writer lock.
                    let file_roster = current;
                    let referenced =
                        file_roster
                            .rows
                            .iter()
                            .flat_map(|row| row.dir.iter().chain(row.was.iter().flatten()))
                            .chain(logins.labels.values().flat_map(|entry| {
                                entry.dir.iter().chain(entry.was.iter().flatten())
                            }))
                            .any(|held| {
                                Path::new(held) == dir
                                    || fs::canonicalize(held).is_ok_and(|path| path == actual_dir)
                            });
                    if referenced {
                        return Ok(Vec::new());
                    }
                    // Another process may have renewed or limited this account
                    // since the staging check. Decide under its lock.
                    let bound = accounts::bound(file_roster, logins);
                    let enrolled = accounts::enrolled_for(cli, &bound);
                    let Some(row) = bound.row(&proof.label).filter(|r| {
                        if r.vendor.region != proof.region {
                            return false;
                        }
                        if cli == Cli::Codex && proof.proved_by != Some(accounts::Proof::Status) {
                            return false;
                        }
                        let expected = if matches!(cli, Cli::Claude | Cli::Codex) {
                            r.expected_identity_email()
                        } else {
                            r.email.as_deref()
                        };
                        expected.is_some_and(|em| accounts::email_matches(em, &proof.email))
                    }) else {
                        return Ok(Vec::new());
                    };
                    // The saved check is not a lease on the staged login. Refuse
                    // a known replacement before changing any recovery state.
                    let claude_identity = if cli == Cli::Claude {
                        super::profile::identity_in_home(&dir, &proof.email)
                    } else {
                        super::profile::IdentityObservation::default()
                    };
                    let identity_contradicted = match cli {
                        Cli::Claude => claude_identity.email_matches == Some(false),
                        Cli::Codex => {
                            super::codex_app_login::check_login_identity(&dir, Some(&proof.email))
                                .is_err()
                        }
                        _ => false,
                    };
                    if identity_contradicted {
                        return Ok(Vec::new());
                    }
                    let principal = if cli == Cli::Claude && !logins.shared.is_empty() {
                        claude_identity.principal
                    } else {
                        None
                    };
                    if cli == Cli::Claude
                        && !logins.shared.is_empty()
                        && !logins.shared.checkpoint_allowed(
                            row,
                            principal,
                            &proof.dir,
                            &proof.parent,
                        )
                    {
                        return Ok(Vec::new());
                    }
                    let account = state.accounts.get(&row.label);
                    // Wall time can move backward. Only the binding captured under the writer
                    // lock when this checkpoint was created can accept its completion.
                    if !proof.parent.matches(logins, &row.label) {
                        return Ok(Vec::new());
                    }
                    let is_limited = account.is_some_and(|a| a.limited_until(now).is_some());
                    let newer_auth =
                        account.is_some_and(|a| a.auth_at.is_some_and(|at| at >= proof.checked_at));
                    let newer_binding = logins.labels.get(&row.label).is_some_and(|binding| {
                        binding
                            .renewed_at
                            .iter()
                            .chain(binding.ended_at.iter())
                            .chain(binding.wrong_at.iter())
                            .chain(binding.served_at.iter())
                            .any(|at| *at >= proof.checked_at)
                            // A notice keeps an end, not its observation clock.
                            // An old checkpoint must not clear an undated notice.
                            || binding.notice_end.is_some()
                    });
                    let is_signed_out = state
                        .accounts
                        .get(&row.label)
                        .is_some_and(|a| a.auth == accounts::Auth::SignedOut);
                    let pending_shared =
                        cli == Cli::Claude && logins.shared.pending_for(row, principal);
                    let needs_recovery =
                        !enrolled.contains(&row.label) || is_signed_out || pending_shared;
                    if is_limited || newer_auth || newer_binding || !needs_recovery {
                        return Ok(Vec::new());
                    }
                    if cli == Cli::Claude && !logins.shared.is_empty() {
                        let generation = logins
                            .generation_clock
                            .checked_add(1)
                            .ok_or_else(|| "local binding generation exhausted".to_owned())?;
                        logins
                            .shared
                            .recover_checkpoint(row, principal, &proof, generation)?;
                    }
                    let own_dir = file_roster.row(&row.label).and_then(|r| r.dir.as_deref());
                    let events = accounts::note_renewed(
                        state,
                        logins,
                        &row.label,
                        own_dir,
                        &proof.dir,
                        proof.subscription.as_deref(),
                        proof.checked_at,
                    );
                    if !events.is_empty()
                        && let Some(proved_by) = proof.proved_by
                    {
                        accounts::note_identity(logins, &row.label, proved_by, Some(&proof.email));
                    }
                    Ok(events)
                },
            )
            .is_ok_and(|events| !events.is_empty())
        {
            recovered_labels.insert(proof.label);
            any_recovered = true;
        }
        drop(writer);
        on_locked();
    }
    any_recovered
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedSelection",
        action = "Progress",
        project = "aterm_agent::harness::accounts_sync::shared_selection_tests::selection_transition"
    )
)]
fn auto_renew_next_account(
    home: &Path,
    state_dir: &Path,
    roster_path: &Path,
    now: i64,
    fx: &UpkeepEffects<'_>,
) -> bool {
    if let Some(pending) = resume_pending_pin(home, state_dir, roster_path, now, fx) {
        return pending.progressed();
    }
    let store = accounts::Store::at(state_dir);
    let recovered = recover_interrupted_staging_renewals_for(
        Cli::Claude,
        &store,
        roster_path,
        fx.renew_guard,
        now,
    );
    let Ok(mut candidates) = read_renewal_candidates(Cli::Claude, &store, roster_path, now) else {
        return recovered;
    };
    if fx.shared.authority.is_some() {
        candidates.tiered = rank_renewal_candidates(
            Cli::Claude,
            Some(&store),
            &candidates.records.raw_roster,
            &candidates.records.state,
            &candidates.records.logins,
            now,
        );
    }
    let ladder = LadderCtx::of(fx, roster_path);
    // A row no session needs yet is signed in only where it is a Claude Code account by some
    // word: a source's or a person's row, or one Claude Code's own files show, never an address
    // discovery listed only because a credential names it.
    let unclaimed = if candidates
        .tiered
        .iter()
        .any(|(tier, _)| *tier >= UNNEEDED_TIER)
    {
        unclaimed_emails(home, state_dir, roster_path, now, fx.rediscovery)
    } else {
        BTreeSet::new()
    };
    for (tier, next_row) in candidates.tiered {
        if tier >= UNNEEDED_TIER
            && next_row
                .email
                .as_deref()
                .is_some_and(|em| unclaimed.iter().any(|u| accounts::email_matches(u, em)))
        {
            continue;
        }
        if spawn_row_renewal(
            accounts::Cli::Claude,
            home,
            state_dir,
            roster_path,
            candidates.records.raw_roster.clone(),
            &candidates.records.logins,
            next_row,
            (None, false),
            now,
            fx.renew_guard,
            fx.spawn_renewal,
            Some(&ladder),
        )
        .progressed()
        {
            return true;
        }
    }
    recovered
}

/// The addresses of the roster at `roster_path` that only a credential shows
/// ([`Shown::credentials_only`]), where its fleet is one discovery adds to
/// ([`fleet_takes_discovered`]): another pool's sign-in, a mailbox's own user, an address no
/// Claude Code login, transcript or profile here names. None where a source or a person wrote
/// the roster, since a row they list is a Claude Code account by their word. Read again at most
/// every [`REDISCOVER_EVERY`].
fn unclaimed_emails(
    home: &Path,
    state_dir: &Path,
    roster_path: &Path,
    now: i64,
    rediscovery: &std::sync::Mutex<Rediscovery>,
) -> BTreeSet<String> {
    let lock = || {
        rediscovery
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    };
    if let Some((at, emails)) = &lock().unclaimed
        && at.elapsed() < REDISCOVER_EVERY
    {
        return emails.clone();
    }
    let env = quiet_env(home, state_dir, &config_beside(roster_path), now);
    let emails = fleet_file_beside(roster_path)
        .and_then(|fleet_path| {
            let text = read_local_source_text(&fleet_path).ok()?;
            let fleet = FleetRoster::from_toml(&text, &fleet_path.display().to_string()).ok()?;
            let mut shown = None;
            fleet_takes_discovered(opt_in_of(&text), &fleet, || {
                shown.get_or_insert_with(|| local_shows(&env)).clone()
            })
            .then(|| {
                shown
                    .get_or_insert_with(|| local_shows(&env))
                    .credentials_only()
            })
        })
        .unwrap_or_default();
    lock().unclaimed = Some((Instant::now(), emails.clone()));
    emails
}

pub(super) fn recover_staging_renewals_for(
    cli: Cli,
    store: &accounts::Store,
    roster_path: &Path,
    now: i64,
) -> bool {
    recover_interrupted_staging_renewals_for(cli, store, roster_path, &AUTO_RENEW_GUARD, now)
}

/// Start an interactive non-blocking staging login for the next un-enrolled successor at a wall.
pub fn auto_renew_next_account_interactive(
    home: &Path,
    state_dir: &Path,
    roster_path: &Path,
    now: i64,
) -> bool {
    auto_renew_next_account_interactive_for(Cli::Claude, home, state_dir, roster_path, now)
}

/// [`auto_renew_next_account_interactive`] for `cli`'s pool.
pub fn auto_renew_next_account_interactive_for(
    cli: Cli,
    home: &Path,
    state_dir: &Path,
    roster_path: &Path,
    now: i64,
) -> bool {
    auto_renew_next_account_interactive_with(
        cli,
        home,
        state_dir,
        roster_path,
        now,
        &live_effects(),
    )
}

/// [`auto_renew_next_account_interactive_for`] making `fx`'s effects. The config beside the
/// roster holds it as it holds the window's upkeep ([`upkeep`]): `[harness] accounts = "off"`
/// starts nothing, and while `[harness] logins = "off"` or `enabled = false` turns the automatic
/// sign-ins off ([`sign_ins_on`]) only a saved explicit choice completes.
fn auto_renew_next_account_interactive_with(
    cli: Cli,
    home: &Path,
    state_dir: &Path,
    roster_path: &Path,
    now: i64,
    fx: &UpkeepEffects<'_>,
) -> bool {
    let config = config_beside(roster_path);
    if accounts_sync_off(Some(&config)) {
        return false;
    }
    if let Some(pending) = resume_pending_pin_for(cli, home, state_dir, roster_path, now, fx) {
        return pending.progressed();
    }
    if !sign_ins_on(&config) {
        return false;
    }
    let store = accounts::Store::at_for(state_dir, cli, home);
    let recovered =
        recover_interrupted_staging_renewals_for(cli, &store, roster_path, fx.renew_guard, now);
    let Ok(candidates) = read_renewal_candidates(cli, &store, roster_path, now) else {
        return recovered;
    };
    // A person's click starts only what a session needs now: a row no session needs is the
    // upkeep's to sign in, headless, never in a browser opened for a click.
    for (_, next_row) in candidates
        .tiered
        .into_iter()
        .filter(|(tier, _)| *tier < UNNEEDED_TIER)
    {
        if spawn_row_renewal(
            cli,
            home,
            state_dir,
            roster_path,
            candidates.records.raw_roster.clone(),
            &candidates.records.logins,
            next_row,
            (None, true),
            now,
            fx.renew_guard,
            fx.spawn_renewal,
            None,
        )
        .progressed()
        {
            return true;
        }
    }
    recovered
}

/// Admission or completion of a saved account selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenewalDispatch {
    /// A worker owns the renewal permit.
    Started,
    /// The selected account was already eligible and the exact saved request was pinned.
    Recovered,
    /// Another renewal or account writer owns admission; keep the request for a later upkeep.
    Held,
    /// Definite startup, authentication or policy failure; this attempt cannot proceed.
    Failed,
    /// Account records are unreadable or recovery is fenced; retain the saved selection
    /// and start no authentication until the store is healthy.
    Fenced,
    /// A newer selection or an explicit cancellation replaced this request.
    Superseded,
}

impl RenewalDispatch {
    fn store_refusal(why: &str) -> Self {
        if why == accounts::LOCK_HELD {
            Self::Held
        } else {
            Self::Fenced
        }
    }

    fn progressed(self) -> bool {
        matches!(self, Self::Started | Self::Recovered)
    }
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountQueuedSelection",
        action = "Lock",
        project = "aterm_agent::harness::accounts_sync::request_admission_tests::admission_transition"
    )
)]
fn renewal_signin_admission(store: &accounts::Store) -> Result<accounts::Lock, RenewalDispatch> {
    match store.signin_now() {
        Ok(Some(lock)) => Ok(lock),
        Ok(None) => Err(RenewalDispatch::Held),
        Err(_) => Err(RenewalDispatch::Fenced),
    }
}

fn queued_pin_admitted(
    store: &accounts::Store,
    policy: accounts::RenewalPolicy<'_>,
    request: Option<&accounts::PinRequest>,
    now: i64,
) -> bool {
    request.is_none_or(|_| {
        store
            .admit_renewal_attempt(policy, request, now)
            .unwrap_or(false)
    })
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountQueuedSelection",
        action = "AnotherProvider",
        reason = "The native conformance projection chooses one of four provider fixtures."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountQueuedSelection",
        action = "AnotherCase",
        reason = "The native projection chooses a bounded stale-work or local-refusal scenario."
    )
)]
#[doc(hidden)]
pub fn account_queued_selection_environment() {}

fn resume_pending_pin(
    home: &Path,
    state_dir: &Path,
    roster_path: &Path,
    now: i64,
    fx: &UpkeepEffects<'_>,
) -> Option<RenewalDispatch> {
    resume_pending_pin_for(Cli::Claude, home, state_dir, roster_path, now, fx)
}

fn resume_pending_pin_for(
    cli: Cli,
    home: &Path,
    state_dir: &Path,
    roster_path: &Path,
    now: i64,
    fx: &UpkeepEffects<'_>,
) -> Option<RenewalDispatch> {
    let store = accounts::Store::at_for(state_dir, cli, home);
    if let Err(why) = store.recover_sync_within(&accounts::LOCK_LADDER, roster_path) {
        return Some(RenewalDispatch::store_refusal(&why));
    }
    let request = match store.load(now) {
        Ok((state, _)) => state.pending_pin?,
        Err(why) => return Some(RenewalDispatch::store_refusal(&why)),
    };
    let result = renew_requested_pin_for(
        cli,
        home,
        state_dir,
        roster_path,
        request.clone(),
        now,
        fx.renew_guard,
        fx.spawn_renewal,
    );
    if result == RenewalDispatch::Failed {
        if let Err(why) = store.cancel_pin_request(&request, now) {
            return Some(RenewalDispatch::store_refusal(&why));
        }
        // A definite refusal may have retired an obsolete shared-account
        // selection. Continue useful work in this pass only after observing
        // that no newer selection took its place. Read failures stay fenced.
        return match store.load(now) {
            Ok((state, _)) if state.pending_pin.is_none() => None,
            Ok(_) => Some(RenewalDispatch::Superseded),
            Err(why) => Some(RenewalDispatch::store_refusal(&why)),
        };
    }
    Some(result)
}

/// Starts a staging login for a saved explicit selection, preserving held and newer requests.
pub fn auto_renew_account_by_label(
    home: &Path,
    state_dir: &Path,
    roster_path: &Path,
    request: accounts::PinRequest,
    now: i64,
) -> RenewalDispatch {
    auto_renew_account_by_label_for(Cli::Claude, home, state_dir, roster_path, request, now)
}

/// [`auto_renew_account_by_label`] for `cli`'s pool.
pub fn auto_renew_account_by_label_for(
    cli: Cli,
    home: &Path,
    state_dir: &Path,
    roster_path: &Path,
    request: accounts::PinRequest,
    now: i64,
) -> RenewalDispatch {
    renew_requested_pin_for(
        cli,
        home,
        state_dir,
        roster_path,
        request,
        now,
        &AUTO_RENEW_GUARD,
        &spawn_renewal_thread,
    )
}

#[cfg(test)]
fn renew_requested_pin(
    home: &Path,
    state_dir: &Path,
    roster_path: &Path,
    request: accounts::PinRequest,
    now: i64,
    renew_guard: &std::sync::Arc<std::sync::Mutex<AutoRenewGuard>>,
    spawn_renewal: &SpawnRenewal,
) -> RenewalDispatch {
    renew_requested_pin_for(
        Cli::Claude,
        home,
        state_dir,
        roster_path,
        request,
        now,
        renew_guard,
        spawn_renewal,
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "requested-pin renewal binds CLI, paths, saved request, clock, admission guard, and spawn seam"
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedSelection",
        action = "Resolve",
        project = "aterm_agent::harness::accounts_sync::shared_selection_tests::selection_transition"
    )
)]
fn renew_requested_pin_for(
    cli: Cli,
    home: &Path,
    state_dir: &Path,
    roster_path: &Path,
    request: accounts::PinRequest,
    now: i64,
    renew_guard: &std::sync::Arc<std::sync::Mutex<AutoRenewGuard>>,
    spawn_renewal: &SpawnRenewal,
) -> RenewalDispatch {
    let store = accounts::Store::at_for(state_dir, cli, home);
    if let Err(why) = store.recover_sync_within(&accounts::LOCK_LADDER, roster_path) {
        return RenewalDispatch::store_refusal(&why);
    }
    match store.load(now) {
        Ok((state, _)) if state.pending_pin.as_ref() == Some(&request) => {}
        Ok(_) => return RenewalDispatch::Superseded,
        Err(why) => return RenewalDispatch::store_refusal(&why),
    }
    recover_interrupted_staging_renewals_for(cli, &store, roster_path, renew_guard, now);
    let mut disposition = RenewalDispatch::Superseded;
    let mut target = None;
    if let Err(why) = store.transact_pool_logins_within(
        &accounts::LOCK_LADDER,
        roster_path,
        now,
        &mut None,
        |raw, st, logins| {
            if st.pending_pin.as_ref() != Some(&request) {
                return Ok(Vec::new());
            }
            let roster = accounts::bound(raw, logins);
            // A default row is here only while its CLI's default home holds the login, as
            // `Store::complete_requested_pin_at` reads it; with none, the row is signed in.
            let enrolled = accounts::enrolled_in(cli, &roster, home);
            let Some(next_row) = roster.row(request.label()).cloned() else {
                st.pending_pin = None;
                disposition = RenewalDispatch::Failed;
                return Ok(Vec::new());
            };
            let mut probe = st.clone();
            if accounts::pin(&mut probe, &roster, &enrolled, request.label(), now).is_err() {
                if logins.shared.required(&next_row)?.is_some() {
                    // This exact request predates shared custody (or an older
                    // writer queued it). An ordinary worker cannot service it.
                    st.pending_pin = None;
                    disposition = RenewalDispatch::Failed;
                    return Ok(Vec::new());
                }
                target = Some((raw.clone(), next_row, logins.clone()));
                return Ok(Vec::new());
            }
            disposition = RenewalDispatch::Recovered;
            Ok(
                accounts::complete_requested_pin(st, &roster, &enrolled, &request, now)
                    .into_iter()
                    .collect(),
            )
        },
    ) {
        return RenewalDispatch::store_refusal(&why);
    }
    let Some((file_roster, next_row, logins)) = target else {
        return disposition;
    };
    spawn_row_renewal(
        cli,
        home,
        state_dir,
        roster_path,
        file_roster,
        &logins,
        next_row,
        (Some(request), true),
        now,
        renew_guard,
        spawn_renewal,
        None,
    )
}

/// What the automatic ladder of [`spawn_row_renewal`] reaches with ([`UpkeepEffects`]' seams).
struct LadderCtx<'a> {
    /// The roster whose current row must still admit an observation.
    roster_path: PathBuf,
    find_browser: &'a (dyn Fn(&Path) -> bool + Sync),
    vault_sync: fn(&Path) -> super::sign_in_ladder::Vault,
    spawn_vault: &'a SpawnRenewal,
    mailbox: &'a LadderMailbox,
    log: SignInLog,
    run_attempt: Option<RunAttempt>,
    shared: shared_renewal::Effects<'a>,
    /// How many `[harness] hosts` the config beside the roster names.
    hosts: usize,
}

impl<'a> LadderCtx<'a> {
    fn of(fx: &UpkeepEffects<'a>, roster_path: &Path) -> Self {
        let config = config_beside(roster_path);
        Self {
            roster_path: roster_path.to_path_buf(),
            find_browser: fx.find_browser,
            vault_sync: fx.vault_sync,
            spawn_vault: fx.spawn_vault,
            mailbox: fx.mailbox,
            log: std::sync::Arc::clone(&fx.log),
            run_attempt: fx.run_attempt.clone(),
            shared: fx.shared.clone(),
            hosts: crate::supervise::SupervisorConfig::from_path(Some(&config))
                .0
                .hosts
                .len(),
        }
    }
}

/// Where the ladder of one row stands this upkeep ([`ladder_route`]).
enum LadderStep {
    /// A route is ready: run it.
    Go {
        route: super::sign_in_ladder::Route,
        read: super::sign_in_ladder::Read,
        /// What the ladder read, for the backoff a failed run sets.
        inputs: String,
    },
    /// Not now, and nothing to record: the row waits out its backoff, or the vault is refreshing.
    Held,
    /// No route: recorded, logged, and the row's backoff set.
    NoRoute,
}

/// A read-only cache context, never authority to use a vault or its credentials.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VaultInput {
    Missing,
    Unreadable,
    Read(super::sign_in_ladder::VaultContext),
}

fn vault_state_fields(text: &str) -> impl Iterator<Item = &str> {
    text.lines().map(str::trim).filter(|line| {
        line.split_once('=').is_some_and(|(key, _)| {
            matches!(
                key.trim(),
                "membership" | "seq" | "digest" | "source_identity"
            )
        })
    })
}

fn read_vault_state_text(path: &Path) -> io::Result<String> {
    // A client's inventory can exceed the account-source cap. Use its own format limit
    // where the vault client exists; other platforms cannot run that client.
    #[cfg(unix)]
    let state_max = aterm_secrets::client::MAX_STATE;
    #[cfg(not(unix))]
    let state_max = SOURCE_TEXT_MAX_BYTES;
    read_local_text_bounded(path, astream_sys::Links::Refuse, state_max)
}

/// Bind cached refresh observations to bounded source contents and the applied revision.
/// Heartbeats do not change this digest. Permissions do, so repairing a refused source ends
/// its cached failure. Separate file reads are observations, not a filesystem transaction.
fn vault_input(home: &Path) -> VaultInput {
    fn field(hash: &mut aterm_digest::Sha256, bytes: &[u8]) {
        hash.update((bytes.len() as u64).to_be_bytes());
        hash.update(bytes);
    }
    fn mode(hash: &mut aterm_digest::Sha256, path: &Path) -> Option<()> {
        let metadata = fs::symlink_metadata(path).ok()?;
        if !metadata.is_file() {
            return None;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            hash.update(metadata.mode().to_be_bytes());
            hash.update(metadata.uid().to_be_bytes());
        }
        #[cfg(not(unix))]
        hash.update([u8::from(metadata.permissions().readonly())]);
        Some(())
    }
    let root = home.join(".config/aterm/secrets");
    let source_path = root.join("source.toml");
    let source = match read_local_source_text(&source_path) {
        Ok(source) => source,
        Err(error)
            if error.kind() == io::ErrorKind::NotFound && !source_entry_present(&source_path) =>
        {
            return VaultInput::Missing;
        }
        Err(_) => return VaultInput::Unreadable,
    };
    let mut hash = aterm_digest::Sha256::new();
    hash.update(b"aterm-account-vault-context-v1");
    field(&mut hash, home.as_os_str().as_encoded_bytes());
    field(&mut hash, source.as_bytes());
    if mode(&mut hash, &source_path).is_none() {
        return VaultInput::Unreadable;
    }
    let state_path = root.join("state.toml");
    match read_vault_state_text(&state_path) {
        Ok(state) => {
            hash.update([1]);
            if mode(&mut hash, &state_path).is_none() {
                return VaultInput::Unreadable;
            }
            for line in vault_state_fields(&state) {
                field(&mut hash, line.as_bytes());
            }
        }
        Err(error)
            if error.kind() == io::ErrorKind::NotFound && !source_entry_present(&state_path) =>
        {
            hash.update([0])
        }
        Err(_) => return VaultInput::Unreadable,
    }
    VaultInput::Read(hash.finalize())
}

/// What the files a route reads hold now, as one line a row's backoff is kept by
/// ([`super::sign_in_ladder::Ladder::held`]): each file in `~/.secrets` (its name, size and
/// modification time); the vault's membership and the version it applied (`membership`, `seq`
/// and `digest` in `~/.config/aterm/secrets/state.toml`, never that file's own time, which the
/// vault's subscriber rewrites about once a minute only to say when it last heard the vault);
/// the vault's `source.toml`; the mailbox configurations; and the store's browser profiles.
/// Mail arriving in a mailbox folder is not among them, since a route's own run brings one, and
/// neither are the roster's own files, which a sync rewrites.
fn ladder_inputs(home: &Path, store: &accounts::Store) -> String {
    let stamp = |path: &Path| {
        fs::symlink_metadata(path)
            .ok()
            .map(|meta| (meta.len(), meta.modified().ok()))
    };
    let mut line = String::new();
    let secrets = home.join(".secrets");
    let mut held: Vec<(std::ffi::OsString, PathBuf)> = fs::read_dir(&secrets)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| (entry.file_name(), entry.path()))
                .collect()
        })
        .unwrap_or_default();
    held.sort();
    for (name, path) in held {
        line.push_str(&format!("{}={:?};", name.to_string_lossy(), stamp(&path)));
    }
    let vault = home.join(".config/aterm/secrets");
    if let Ok(text) = read_vault_state_text(&vault.join("state.toml")) {
        for kept in vault_state_fields(&text) {
            line.push_str(kept);
            line.push(';');
        }
    }
    for path in [
        vault.join("source.toml"),
        home.join(".config/aterm")
            .join(super::headless_oauth::MAILBOX_TOML_FILE),
        store.dir.join(super::headless_oauth::MAILBOX_TOML_FILE),
        store.dir.join(super::headless_oauth::BROWSER_PROFILES_DIR),
    ] {
        line.push_str(&format!("{:?};", stamp(&path)));
    }
    line
}

/// The process-local retry key includes the admitted row's definition as well as its
/// observed routes and files. It stays in memory; reports and events use route words.
pub(super) fn row_retry_inputs(row: &accounts::Row, observed: &str) -> String {
    format!("{row:?}|{observed}")
}

/// What the ladder reads of the machine for `row` before the vault is tried.
fn ladder_read(
    home: &Path,
    store: &accounts::Store,
    logins: &accounts::Logins,
    row: &accounts::Row,
    ctx: &LadderCtx<'_>,
) -> super::sign_in_ladder::Read {
    use super::sign_in_ladder::{Profile, Read};
    let email = row.email.as_deref();
    let profile = if !super::headless_oauth::has_browser_profile(&store.dir, &row.label, email) {
        Profile::None
    } else if logins.get(&row.label).and_then(|b| b.wrong_at).is_some() {
        Profile::WrongAccount
    } else {
        Profile::Ready
    };
    let (mailbox, mailbox_login) = (ctx.mailbox)(home, store, row);
    Read {
        profile,
        mailbox,
        mailbox_login,
        vault: None,
        browser: (ctx.find_browser)(home),
        row_browser: row.browser.is_some(),
        hosts: ctx.hosts,
    }
}

/// THE LADDER OF ONE ROW: read every route; while none is ready, refresh the LAN vault (a
/// member's bounded `aterm secrets sync`, on its own clock and off this thread) and read again;
/// then either the route to run, or a recorded error naming every route tried
/// ([`accounts::note_sign_in_attempt`], written once per distinct finding), one `aterm.log` line,
/// and the row's backoff. A row inside its backoff whose inputs have not changed is not read past
/// its cheap probes.
fn ladder_route(
    home: &Path,
    store: &accounts::Store,
    row: &accounts::Row,
    now: i64,
    renew_guard: &std::sync::Arc<std::sync::Mutex<AutoRenewGuard>>,
    ctx: &LadderCtx<'_>,
) -> LadderStep {
    ladder_route_with_clock(home, store, row, now, renew_guard, ctx, &Instant::now)
}

/// [`ladder_route`] with an injected monotonic clock for expiry checks.
fn ladder_route_with_clock(
    home: &Path,
    store: &accounts::Store,
    row: &accounts::Row,
    now: i64,
    renew_guard: &std::sync::Arc<std::sync::Mutex<AutoRenewGuard>>,
    ctx: &LadderCtx<'_>,
    clock: &dyn Fn() -> Instant,
) -> LadderStep {
    use super::sign_in_ladder::{Vault, VaultTurn};
    fn lock(
        guard: &std::sync::Arc<std::sync::Mutex<AutoRenewGuard>>,
    ) -> std::sync::MutexGuard<'_, AutoRenewGuard> {
        guard
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    // Another row's route observation can insert a binding and advance the global
    // counter. Capture this probe separately; its publication still checks the parent.
    let Ok((logins, _)) = store.load_logins(now) else {
        return LadderStep::Held;
    };
    let logins = &logins;
    let mut read = ladder_read(home, store, logins, row, ctx);
    let observed_vault = if read.route().is_none() {
        vault_input(home)
    } else {
        VaultInput::Missing
    };
    let inputs_of = |read: &super::sign_in_ladder::Read, observed: VaultInput| {
        row_retry_inputs(
            row,
            &format!(
                "{}|{observed:?}|{}",
                read.words(),
                ladder_inputs(home, store)
            ),
        )
    };
    let mut inputs = inputs_of(&read, observed_vault);
    let at = clock();
    {
        let mut guard = lock(renew_guard);
        if guard.ladder.held(&row.label, &inputs, at) {
            return LadderStep::Held;
        }
        if !guard.ladder.has_backoff(&row.label)
            && guard
                .ladder_owners
                .get(&row.label)
                .is_some_and(|owner| owner.matches(store, Cli::Claude))
        {
            guard.ladder_owners.remove(&row.label);
        }
    }
    if read.route().is_none() {
        let vault = match observed_vault {
            VaultInput::Read(key) => {
                let turn = lock(renew_guard).ladder.vault_turn(Some(key), at);
                match turn {
                    VaultTurn::Seen(seen) => seen,
                    VaultTurn::Running | VaultTurn::Unavailable => return LadderStep::Held,
                    VaultTurn::Due => {
                        let guard = std::sync::Arc::clone(renew_guard);
                        let (sync, home_owned) = (ctx.vault_sync, home.to_path_buf());
                        let started = (ctx.spawn_vault)(Box::new(move || {
                            let seen = sync(&home_owned);
                            guard
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .ladder
                                .vault_done(seen);
                        }));
                        if started.is_err() {
                            lock(renew_guard)
                                .ladder
                                .vault_done(Vault::Failed("it could not be started".to_owned()));
                        }
                        let Some(seen) = lock(renew_guard).ladder.vault_result(key, at) else {
                            return LadderStep::Held;
                        };
                        seen
                    }
                }
            }
            unknown @ (VaultInput::Missing | VaultInput::Unreadable) => {
                match lock(renew_guard).ladder.vault_turn(None, at) {
                    VaultTurn::Unavailable => {}
                    VaultTurn::Running | VaultTurn::Due | VaultTurn::Seen(_) => {
                        return LadderStep::Held;
                    }
                }
                if unknown == VaultInput::Missing {
                    Vault::NotMember
                } else {
                    Vault::Failed("vault context unreadable".to_owned())
                }
            }
        };
        if vault_input(home) != observed_vault {
            lock(renew_guard).ladder.invalidate_vault();
            return LadderStep::Held;
        }
        if vault.wrote_files() {
            read = ladder_read(home, store, logins, row, ctx);
            inputs = inputs_of(
                &read,
                if read.route().is_none() {
                    observed_vault
                } else {
                    VaultInput::Missing
                },
            );
        }
        read.vault = Some(vault);
    }
    if let Some(route) = read.route() {
        if read.mailbox == super::sign_in_ladder::Mailbox::Ready
            && let Some(line) = mailbox_need(home, &row.label, true)
        {
            (ctx.log)(&format!("{}: its mailbox need: {line}", row.label));
        }
        return LadderStep::Go {
            route,
            read,
            inputs,
        };
    }
    let words = read.words();
    if !note_attempt(
        store,
        (&ctx.roster_path, row, logins),
        accounts::AttemptOutcome::NoRoute,
        &words,
        now,
    ) {
        return LadderStep::Held;
    }
    if read.wants_mailbox()
        && let Some(line) = mailbox_need(home, &row.label, false)
    {
        (ctx.log)(&format!(
            "{}: the lab is asked for its mailbox password: {line}",
            row.label
        ));
    }
    let wait = {
        let mut guard = lock(renew_guard);
        let wait = guard.ladder.failed(&row.label, &inputs, at);
        guard.ladder_owners.insert(
            row.label.clone(),
            LoginOwner::new(store, Cli::Claude, &row.label, now),
        );
        wait
    };
    (ctx.log)(&format!(
        "{} (tried again in {} or when one of them changes)",
        super::sign_in_ladder::log_line(
            &row.label,
            accounts::AttemptOutcome::NoRoute.as_str(),
            &words
        ),
        super::upgrade::span(wait.as_secs())
    ));
    LadderStep::NoRoute
}

/// Ask the lab for `label`'s mailbox password, or record that the row's mailbox is now there
/// (`met`), through this Mac's need ledger (docs/DESIGN-credential-requests-2026-10-27.md
/// §12.3): the ledger posts the request once this Mac is a Commons Writer and asks a person only
/// when no Mac the lab reaches holds it. The need's line, for the log; `None` when nothing
/// moved (`met` with no need on file) or the ledger cannot be used here, as on every system but
/// unix, where no vault runs and so no ledger is kept.
fn mailbox_need(home: &Path, label: &str, met: bool) -> Option<String> {
    #[cfg(unix)]
    {
        use aterm_secrets::need::{self, Purpose, Want};
        let want = Want::parse(&format!("mailbox:{label}")).ok()?;
        let paths = aterm_secrets::client::Paths::for_home(home);
        let need = if met {
            let live = need::peek(&paths)
                .ok()?
                .iter()
                .any(|need| need.want == want && !need.state.ended());
            if !live {
                return None;
            }
            need::satisfied(&paths, &want, Purpose::HarnessSignIn).ok()??
        } else {
            need::raise(&paths, want, Purpose::HarnessSignIn).ok()?
        };
        Some(need.line)
    }
    #[cfg(not(unix))]
    {
        let _ = (home, label, met);
        None
    }
}

/// Admit an observation against its captured effective row and parent under the roster's
/// publication lock. Unrelated row edits do not change its account. Empty observations
/// retain the event-based wake rule and do not rewrite history.
/// # Errors
/// The writer is unavailable, the roster or records cannot be read, the row or parent
/// changed, or publication fails.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountAttemptPolicy",
        action = "RowA",
        reason = "The configured row returns to the captured definition."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountAttemptPolicy",
        action = "RowB",
        reason = "A different configured account row is observed under the same label."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountAttemptPolicy",
        action = "Replace",
        reason = "The captured binding parent is replaced independently of the worker."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountAttemptPolicy",
        action = "Unreadable",
        reason = "The configured roster becomes unreadable."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountAttemptPolicy",
        action = "Readable",
        reason = "The configured roster is readable again."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountAttemptPolicy",
        action = "Assess",
        project = "aterm_agent::harness::accounts_sync::attempt_policy_tests::transition"
    )
)]
fn with_current_attempt(
    store: &accounts::Store,
    (roster_path, row, observed): (&Path, &accounts::Row, &accounts::Logins),
    now: i64,
    edit: impl FnOnce(&mut accounts::Logins) -> Vec<accounts::Event>,
) -> Result<Vec<accounts::Event>, String> {
    let parent = accounts::RenewalParent::capture(observed, &row.label);
    store.transact_pool_logins_within(
        &accounts::LOCK_LADDER,
        roster_path,
        now,
        &mut None,
        |raw, _, current| {
            if accounts::bound(raw, current).row(&row.label) != Some(row) {
                return Err(format!(
                    "account `{}` changed during automatic sign-in; nothing recorded",
                    row.label
                ));
            }
            parent.require_current(current, &row.label)?;
            Ok(edit(current))
        },
    )
}

/// Records an attempt only while its row and captured parent still match. A duplicate
/// outcome is admitted without another history entry, so its retry clock can still advance.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountAttemptPolicy",
        action = "Record",
        project = "aterm_agent::harness::accounts_sync::attempt_policy_tests::transition"
    )
)]
fn note_attempt(
    store: &accounts::Store,
    captured: (&Path, &accounts::Row, &accounts::Logins),
    outcome: accounts::AttemptOutcome,
    routes: &str,
    now: i64,
) -> bool {
    with_current_attempt(store, captured, now, |current| {
        accounts::note_sign_in_attempt(current, &captured.1.label, outcome, routes, now)
    })
    .is_ok()
}

#[expect(
    clippy::too_many_arguments,
    reason = "row renewal binds paths, roster, logins, target row, pin/browser flags, clock, admission guard, spawn seam and the automatic ladder"
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedDisposition",
        action = "Run",
        project = "aterm_agent::harness::accounts_sync::automatic_shared_tests::disposition_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedDisposition",
        action = "Replay",
        project = "aterm_agent::harness::accounts_sync::automatic_shared_tests::disposition_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountSharedDisposition",
        action = "StopBefore",
        reason = "Authority can stop while an admitted worker is queued."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountSharedDisposition",
        action = "StopDuring",
        reason = "Authority can stop during provider work."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountSharedDisposition",
        action = "ProviderFail",
        reason = "The provider flow can fail independently of local authority."
    )
)]
fn spawn_row_renewal(
    cli: accounts::Cli,
    home: &Path,
    state_dir: &Path,
    roster_path: &Path,
    file_roster: Roster,
    logins: &accounts::Logins,
    next_row: accounts::Row,
    (pin_request, allow_default_browser): (Option<accounts::PinRequest>, bool),
    now: i64,
    renew_guard: &std::sync::Arc<std::sync::Mutex<AutoRenewGuard>>,
    spawn_renewal: &SpawnRenewal,
    ladder: Option<&LadderCtx<'_>>,
) -> RenewalDispatch {
    if cli != accounts::Cli::Claude {
        return spawn_vendor_renewal_with(
            cli,
            home,
            state_dir,
            roster_path,
            file_roster,
            logins,
            next_row,
            pin_request,
            now,
            renew_guard,
            spawn_renewal,
        );
    }
    let store = accounts::Store::at(state_dir);
    if let Err(why) = store.check_signin_state(now) {
        return RenewalDispatch::store_refusal(&why);
    }
    if next_row.email.is_none() {
        return RenewalDispatch::Failed;
    }
    let shared_authority = match logins.shared.required(&next_row) {
        Ok(None) => None,
        Ok(Some(source)) => {
            let Some(factory) = ladder
                .filter(|_| pin_request.is_none())
                .and_then(|ctx| ctx.shared.authority)
            else {
                return RenewalDispatch::Failed;
            };
            let Ok(authority) = factory(home, source, &next_row) else {
                return RenewalDispatch::Failed;
            };
            let Ok(grant) = authority.prepare(&next_row) else {
                return RenewalDispatch::Failed;
            };
            if grant.source != source
                || logins
                    .shared
                    .check_preparation(source, grant.generation, grant.expected_principal)
                    .is_err()
            {
                return RenewalDispatch::Failed;
            }
            Some(shared_renewal::automatic(
                authority,
                config_beside(roster_path),
            ))
        }
        Err(_) => return RenewalDispatch::Failed,
    };
    let reads = guard_reads(renew_guard);
    let mut mailbox_dir = resolve_mailbox_dir_with(&reads.var, Some(home), &store);
    // The automatic ladder ([`ladder_route`]) decides an upkeep's renewal and records how it
    // ends; a person's own choice (a pin request, a click) takes the routes it always took.
    let (has_mailbox, has_headless_profile, unattended) =
        match ladder.filter(|_| pin_request.is_none()) {
            Some(ctx) => match ladder_route(home, &store, &next_row, now, renew_guard, ctx) {
                LadderStep::Go {
                    route,
                    read,
                    inputs,
                } => {
                    mailbox_dir = resolve_mailbox_dir_with(&reads.var, Some(home), &store);
                    (
                        route == super::sign_in_ladder::Route::Mailbox,
                        route == super::sign_in_ladder::Route::Profile,
                        Some((route, read, inputs, std::sync::Arc::clone(&ctx.log))),
                    )
                }
                LadderStep::Held => return RenewalDispatch::Held,
                LadderStep::NoRoute => return RenewalDispatch::Failed,
            },
            None => {
                let has_mailbox = mailbox_dir.is_some()
                    || has_configured_mailbox_for_row_with(
                        &reads.var,
                        Some(home),
                        &store,
                        Some(&next_row.label),
                        next_row.email.as_deref(),
                    );
                let has_headless_profile = logins
                    .get(&next_row.label)
                    .and_then(|b| b.wrong_at)
                    .is_none()
                    && super::headless_oauth::has_browser_profile(
                        &store.dir,
                        &next_row.label,
                        next_row.email.as_deref(),
                    )
                    && reads.has_browser(home);
                if !allow_default_browser
                    && next_row.browser.is_none()
                    && !has_mailbox
                    && !has_headless_profile
                {
                    return RenewalDispatch::Failed;
                }
                (has_mailbox, has_headless_profile, None)
            }
        };
    let next_label = next_row.label.clone();
    let Some(next_email) = next_row.email.clone() else {
        return RenewalDispatch::Failed;
    };
    let mailbox_mtime = latest_mailbox_mtime_with(&reads.var, Some(home), &store);
    if pin_request.is_none()
        && let Some(wrong_at) = logins.get(&next_label).and_then(|b| b.wrong_at)
        && now.saturating_sub(wrong_at) < 900
    {
        let mailbox_updated_since = mailbox_mtime
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|d| i64::try_from(d.as_secs()).ok())
            .is_some_and(|mtime_s| mtime_s > wrong_at);
        if !mailbox_updated_since {
            return RenewalDispatch::Held;
        }
    }
    // The machine's one sign-in at a time, taken before anything is admitted: while another
    // sign-in (a vendor's renewal, `accounts browser`'s window, another process's) holds it this
    // renewal is Held, so a pin request stays pending for a later upkeep, with no cooldown
    // started and nothing woken, and the lock taken here is held by the work it starts.
    let signin = match renewal_signin_admission(&store) {
        Ok(signin) => signin,
        Err(disposition) => return disposition,
    };
    reconcile_browser_window_unpark(
        renew_guard,
        &next_label,
        browser_window_signed_in_token(&store, &next_label, 0),
    );
    let Some(mut permit) = AutoRenewPermit::claim_with_mailbox(
        renew_guard,
        &next_label,
        Instant::now(),
        pin_request.is_some(),
        mailbox_mtime,
    ) else {
        return RenewalDispatch::Held;
    };
    permit.wake_pending = Some(state_dir.to_path_buf());
    permit.attribute(&store, cli, &next_label, now);
    let home_owned = home.to_path_buf();
    let state_owned = state_dir.to_path_buf();
    let roster_owned = roster_path.to_path_buf();
    let ladder_guard = std::sync::Arc::clone(renew_guard);
    let started_by = unattended.as_ref().map(|(route, _, _, log)| {
        (
            route.as_str(),
            next_label.clone(),
            std::sync::Arc::clone(log),
        )
    });
    let run_seam = ladder.and_then(|ctx| ctx.run_attempt.clone());
    let shared_host = ladder.and_then(|ctx| ctx.shared.host.clone());
    let captured_logins = logins.clone();
    let started = start_auto_renew(
        permit,
        move || {
            use super::cli::Renewal;

            let store = accounts::Store::at(&state_owned);
            let _signin = signin;
            if !queued_pin_admitted(
                &store,
                accounts::RenewalPolicy {
                    roster_path: &roster_owned,
                    roster: &file_roster,
                    row: &next_row,
                    logins: &captured_logins,
                },
                pin_request.as_ref(),
                now,
            ) {
                return;
            }
            let deferred = std::cell::Cell::new(false);
            // How the last run's `claude auth login` exited, and what its opener's headless
            // sign-in met (its outcome file), for the words of one that made no login
            // (`sign_in_ladder::how_it_ended`).
            let exited: std::cell::Cell<Option<i32>> = std::cell::Cell::new(None);
            let met: std::cell::RefCell<Option<super::open_url::Met>> =
                std::cell::RefCell::new(None);
            let attempt_snapshot = std::cell::RefCell::new(None);
            let publication_claims = LoginClaims::capture(&ladder_guard);
            let live_attempt = |browser: &super::cli::RenewBrowser,
                                force_headless: bool,
                                attempt_logins: &accounts::Logins|
             -> Renewal {
                let policy = accounts::RenewalPolicy {
                    roster_path: &roster_owned,
                    roster: &file_roster,
                    row: &next_row,
                    logins: attempt_logins,
                };
                let Ok(staging) = store.claim_staging_dir(&next_label) else {
                    return Renewal::Deferred;
                };
                let Some(staging_text) = staging.to_str().map(str::to_owned) else {
                    let _ = fs::remove_dir_all(&staging);
                    return Renewal::Failed;
                };
                let env_var = |name: &str| {
                    if name == "HOME" {
                        return Some(home_owned.as_os_str().to_os_string());
                    }
                    std::env::var_os(name)
                        .or_else(|| (name == "USER").then(|| std::ffi::OsString::from("aterm")))
                };
                let claude_prog = resolve_claude_program(&home_owned);
                let ran = shared_renewal::sign_in(
                    &home_owned,
                    &staging_text,
                    &next_email,
                    browser,
                    force_headless,
                    mailbox_dir.as_deref(),
                );
                let ran = match ran {
                    Ok((ran, seen)) => {
                        met.replace(seen);
                        ran
                    }
                    Err(_) => {
                        let _ = fs::remove_dir_all(&staging);
                        return Renewal::Failed;
                    }
                };
                if ran.code != Some(0) || !accounts::login_made(&staging) {
                    let _ = fs::remove_dir_all(&staging);
                    exited.set(ran.code);
                    return Renewal::authentication_failed(ran.code);
                }
                let Ok(status_got) = super::cli::status_in_home_within(
                    claude_prog.as_os_str(),
                    &staging_text,
                    &env_var,
                    super::cli::AUTH_CHECK_BUDGET,
                ) else {
                    let _ = fs::remove_dir_all(&staging);
                    return Renewal::Failed;
                };
                let now_done = crate::supervise::limit::unix_now().max(now);
                complete_auto_staging_renewal(
                    &store,
                    policy,
                    &staging,
                    &staging_text,
                    (
                        &status_got,
                        unattended.as_ref().map(|(route, _, _, _)| route.as_str()),
                    ),
                    now_done,
                    (&ladder_guard, cli),
                )
            };
            let run_attempt = |browser: &super::cli::RenewBrowser, force_headless: bool| {
                let snapshot = &captured_logins;
                if let Some(authority) = shared_authority.as_deref() {
                    let env = quiet_env(
                        &home_owned,
                        &state_owned,
                        &config_beside(&roster_owned),
                        now,
                    );
                    let host = shared_renewal::BackgroundHost {
                        home: &home_owned,
                        mailbox: mailbox_dir.as_deref(),
                        headless: force_headless,
                    };
                    let result = super::cli::shared_renewal::background(
                        &env,
                        shared_host
                            .as_deref()
                            .map_or(&host as &dyn super::cli::AccountsHost, |h| h),
                        &store,
                        &_signin,
                        accounts::RenewalPolicy {
                            roster_path: &roster_owned,
                            roster: &file_roster,
                            row: &next_row,
                            logins: snapshot,
                        },
                        browser,
                        authority,
                    );
                    if matches!(result, Renewal::Busy | Renewal::Deferred) {
                        deferred.set(true);
                    }
                    return result;
                }
                if !store
                    .admit_renewal_attempt(
                        accounts::RenewalPolicy {
                            roster_path: &roster_owned,
                            roster: &file_roster,
                            row: &next_row,
                            logins: snapshot,
                        },
                        pin_request.as_ref(),
                        now,
                    )
                    .unwrap_or(false)
                {
                    deferred.set(true);
                    return Renewal::Deferred;
                }
                *attempt_snapshot.borrow_mut() = Some((*snapshot).clone());
                exited.set(None);
                met.replace(None);
                let result = {
                    let route = if force_headless && has_headless_profile {
                        "profile"
                    } else if has_mailbox {
                        "mailbox"
                    } else {
                        "browser"
                    };
                    let _activity = LoginActivityGuard::begin(
                        &ladder_guard,
                        &store,
                        cli,
                        &next_label,
                        Some(route),
                    );
                    match &run_seam {
                        Some(run) => run(browser, force_headless),
                        None => live_attempt(browser, force_headless, snapshot),
                    }
                };
                if matches!(result, Renewal::Busy | Renewal::Deferred) {
                    deferred.set(true);
                    return result;
                }
                if !result.published()
                    && with_current_attempt(
                        &store,
                        (&roster_owned, &next_row, snapshot),
                        now,
                        |_| Vec::new(),
                    )
                    .is_err()
                {
                    deferred.set(true);
                    Renewal::Deferred
                } else {
                    result
                }
            };
            let how = |renewal: Renewal| {
                super::sign_in_ladder::how_it_ended(renewal, exited.get(), met.borrow().as_ref())
            };
            let ran = (|| -> Result<Renewal, String> {
                store
                    .check_signin_state(now)
                    .inspect_err(|_| deferred.set(true))?;
                let opener = resolve_opener_link(&home_owned, &store);
                // Unattended, nothing that opens a window is ever a fallback: neither the row's
                // desk browser profile nor the system's default browser.
                let non_headless_fallback = if unattended.is_some() {
                    None
                } else {
                    select_auto_renew_browser(
                        next_row.browser.as_deref(),
                        mailbox_dir.as_deref(),
                        opener.clone(),
                    )
                    .or_else(|| allow_default_browser.then_some(super::cli::RenewBrowser::Default))
                };
                let browser = if (has_mailbox || has_headless_profile) && opener.is_some() {
                    super::cli::RenewBrowser::Remote { opener }
                } else {
                    let Some(b) = non_headless_fallback.clone() else {
                        return Err("no opener link could be made".to_owned());
                    };
                    b
                };
                let result = run_attempt(&browser, !has_mailbox && has_headless_profile);
                if result.published() {
                    return Ok(result);
                }
                if result.can_retry_authentication()
                    && !has_mailbox
                    && has_headless_profile
                    && let Some(fallback) = non_headless_fallback
                {
                    let again = run_attempt(&fallback, false);
                    return if again.published() {
                        Ok(again)
                    } else {
                        Err(how(again))
                    };
                }
                Err(how(result))
            })();
            if deferred.get() {
                return;
            }
            let renewed = matches!(&ran, Ok(Renewal::Renewed));
            let published = ran.is_ok();
            let now_done = crate::supervise::limit::unix_now().max(now);
            if let Some((route, read, inputs, log)) = &unattended {
                let (outcome, routes) = match &ran {
                    Ok(Renewal::Renewed) => (
                        accounts::AttemptOutcome::SignedIn,
                        route.as_str().to_owned(),
                    ),
                    Ok(other) => (
                        accounts::AttemptOutcome::Failed,
                        super::sign_in_ladder::failed_words(*route, &how(*other), read),
                    ),
                    Err(how) => (
                        accounts::AttemptOutcome::Failed,
                        super::sign_in_ladder::failed_words(*route, how, read),
                    ),
                };
                // A published login records both the matched success and any requested
                // mismatch atomically. Unpublished failures and a scripted requested success
                // still name their original parent.
                if (!published || (run_seam.is_some() && renewed))
                    && let Some(observed) = attempt_snapshot.borrow().as_ref()
                    && !note_attempt(
                        &store,
                        (&roster_owned, &next_row, observed),
                        outcome,
                        &routes,
                        now_done,
                    )
                {
                    return;
                }
                let mut guard = ladder_guard
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let line = super::sign_in_ladder::log_line(&next_label, outcome.as_str(), &routes);
                if renewed {
                    drop(guard);
                    note_published_login(
                        &ladder_guard,
                        &publication_claims,
                        &store,
                        cli,
                        &next_label,
                    );
                    log(&line);
                } else {
                    let wait = guard.ladder.failed(&next_label, inputs, Instant::now());
                    guard.ladder_owners.insert(
                        next_label.clone(),
                        LoginOwner::new(&store, cli, &next_label, now_done),
                    );
                    drop(guard);
                    login_activity_moved();
                    log(&format!(
                        "{line} (tried again in {} or when its inputs change)",
                        super::upgrade::span(wait.as_secs())
                    ));
                }
            }
            if let Some(request) = &pin_request {
                if !renewed {
                    let _ = store.cancel_pin_request(request, now_done);
                    return;
                }
                let _ =
                    store.complete_requested_pin_at(&roster_owned, &file_roster, request, now_done);
            }
        },
        spawn_renewal,
    );
    if started.is_ok()
        && let Some((route, label, log)) = started_by
    {
        log(&format!(
            "account {label}: automatic sign-in started by its {route} route"
        ));
    }
    started.map_or(RenewalDispatch::Failed, |()| RenewalDispatch::Started)
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn spawn_vendor_renewal(
    cli: accounts::Cli,
    home: &Path,
    state_dir: &Path,
    roster_path: &Path,
    file_roster: &Roster,
    logins: &accounts::Logins,
    next_row: accounts::Row,
    now: i64,
    renew_guard: &std::sync::Arc<std::sync::Mutex<AutoRenewGuard>>,
    spawn_renewal: &SpawnRenewal,
) -> RenewalDispatch {
    spawn_vendor_renewal_with(
        cli,
        home,
        state_dir,
        roster_path,
        file_roster.clone(),
        logins,
        next_row,
        None,
        now,
        renew_guard,
        spawn_renewal,
    )
}

/// [`spawn_row_renewal`] for another CLI's row: its renewal ([`super::cli::renew_one_for`]: the
/// vendor's own sign-in under the machine's sign-in lock, its page approved headlessly) started
/// in the background, where a headless browser can approve the CLI's page
/// ([`Cli::renews_headlessly`]), one is installed, the row has an email and the CLI has a
/// sign-in (one that opens the desk's browser runs under [`super::login_drive::NO_DESK`] there,
/// [`crate::ai::connector::SignIn::no_desk`]). A row whose last one stopped for a person, or met
/// Cloudflare's bot check, waits [`PERSON_BACKOFF`] ([`note_background_end`]; a bot check on
/// record holds it across a restart, [`bot_check_on_record_at`]). A Grok row's never starts,
/// from the upkeep, a saved selection or a click alike: each would request a live xAI device
/// code and then load accounts.x.ai's block page, which aterm never works around (design §5
/// (g)). Its records checked as every CLI's are (unreadable ones fence, keeping a saved
/// selection), it is [`RenewalDispatch::Failed`] before any lock, browser or sign-in is
/// reached, and the row is signed in by the one step every surface names, a person's `accounts
/// login <label> --cli grok`.
#[expect(
    clippy::too_many_arguments,
    reason = "vendor row renewal binds CLI, paths, roster, logins, target row, optional pin request, clock, admission guard, and spawn seam"
)]
fn spawn_vendor_renewal_with(
    cli: accounts::Cli,
    home: &Path,
    state_dir: &Path,
    roster_path: &Path,
    file_roster: Roster,
    logins: &accounts::Logins,
    next_row: accounts::Row,
    pin_request: Option<accounts::PinRequest>,
    now: i64,
    renew_guard: &std::sync::Arc<std::sync::Mutex<AutoRenewGuard>>,
    spawn_renewal: &SpawnRenewal,
) -> RenewalDispatch {
    let store = accounts::Store::at_for(state_dir, cli, home);
    if let Err(why) = store.check_signin_state(now) {
        return RenewalDispatch::store_refusal(&why);
    }
    if !cli.renews_headlessly() {
        return RenewalDispatch::Failed;
    }
    let reads = guard_reads(renew_guard);
    if next_row.email.is_none()
        || cli.accounts().signin.as_ref().is_none()
        || !reads.has_browser(home)
    {
        return RenewalDispatch::Failed;
    }
    let mailbox_mtime =
        latest_mailbox_mtime_with(&reads.var, Some(home), &accounts::Store::at(state_dir));
    if pin_request.is_none()
        && let Some(wrong_at) = logins.get(&next_row.label).and_then(|b| b.wrong_at)
        && now.saturating_sub(wrong_at) < 900
    {
        let mailbox_updated_since = mailbox_mtime
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|d| i64::try_from(d.as_secs()).ok())
            .is_some_and(|mtime_s| mtime_s > wrong_at);
        if !mailbox_updated_since {
            return RenewalDispatch::Held;
        }
    }
    let existing_binding = logins.get(&next_row.label);
    let browser_token = match existing_binding.and_then(|b| b.sign_in_task) {
        Some(task) => {
            let token = (task.action == accounts::SignInAction::Browser)
                .then(|| browser_window_signed_in_token(&store, &next_row.label, task.at))
                .flatten();
            if pin_request.is_none()
                && token.is_none()
                && now.saturating_sub(task.at) < PERSON_BACKOFF_SECS
            {
                return RenewalDispatch::Held;
            }
            token
        }
        None => browser_window_signed_in_token(&store, &next_row.label, 0),
    };
    // A bot check on record holds the row as long as a stop for a person, across a restart
    // too, where the guard's own hold is gone; it notes no task and asks nobody (design R-13).
    if pin_request.is_none()
        && bot_check_on_record_at(&store, &next_row.label)
            .is_some_and(|at| now.saturating_sub(at) < PERSON_BACKOFF_SECS)
    {
        return RenewalDispatch::Held;
    }
    match renewal_signin_admission(&store) {
        Ok(probe) => drop(probe),
        Err(disposition) => return disposition,
    }
    let key = format!("{}:{}", cli.id(), next_row.label);
    if let Some(binding) = existing_binding {
        if let Some(task) = binding.sign_in_task {
            if browser_token.is_some() {
                reconcile_browser_window_unpark(renew_guard, &key, browser_token);
            } else if now.saturating_sub(task.at) >= PERSON_BACKOFF_SECS {
                renew_guard
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .parked
                    .remove(&key);
            }
        } else {
            let mut guard = renew_guard
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if guard.parked.remove(&key).is_some() {
                guard.last_attempt.remove(&key);
            }
            drop(guard);
            reconcile_browser_window_unpark(renew_guard, &key, browser_token);
        }
    } else {
        reconcile_browser_window_unpark(renew_guard, &key, browser_token);
    }
    let Some(mut permit) = AutoRenewPermit::claim_with_mailbox(
        renew_guard,
        &key,
        Instant::now(),
        pin_request.is_some(),
        mailbox_mtime,
    ) else {
        return RenewalDispatch::Held;
    };
    permit.wake_pending = Some(state_dir.to_path_buf());
    permit.attribute(&store, cli, &next_row.label, now);
    let own_dir = file_roster.row(&next_row.label).and_then(|r| r.dir.clone());
    let roster_path = roster_path.to_path_buf();
    let parker = std::sync::Arc::clone(renew_guard);
    let captured_logins = logins.clone();
    start_auto_renew(
        permit,
        move || {
            if !queued_pin_admitted(
                &store,
                accounts::RenewalPolicy {
                    roster_path: &roster_path,
                    roster: &file_roster,
                    row: &next_row,
                    logins: &captured_logins,
                },
                pin_request.as_ref(),
                now,
            ) {
                return;
            }
            let renewal = {
                let _activity =
                    LoginActivityGuard::begin(&parker, &store, cli, &next_row.label, None);
                super::cli::renew_one_for(
                    cli,
                    now,
                    &super::cli::LiveAccounts,
                    &store,
                    (&next_row, &roster_path, &captured_logins),
                    own_dir.as_deref(),
                    &mut io::sink(),
                    &mut io::sink(),
                )
            };
            note_background_end(&parker, &key, renewal, Instant::now());
            if renewal == super::cli::Renewal::Renewed {
                parker
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .activity_owners
                    .remove(&key);
            }
            if let Some(request) = &pin_request {
                let now_done = crate::supervise::limit::unix_now().max(now);
                match renewal {
                    super::cli::Renewal::Renewed => {
                        let _ = store.complete_requested_pin_at(
                            &roster_path,
                            &file_roster,
                            request,
                            now_done,
                        );
                    }
                    super::cli::Renewal::Busy | super::cli::Renewal::Deferred => {}
                    _ => {
                        let _ = store.cancel_pin_request(request, now_done);
                    }
                }
            }
        },
        spawn_renewal,
    )
    .map_or(RenewalDispatch::Failed, |()| RenewalDispatch::Started)
}

/// Run `aterm harness accounts serve [--bind <addr:port>] [<source>] [--machine <mN>]`.
pub fn run_accounts_serve(
    env: &Env,
    bind: Option<&str>,
    source: Option<&str>,
    machine_override: Option<&str>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitCode {
    let bind_addr = bind.unwrap_or(DEFAULT_SERVE_BIND);
    let listener = match TcpListener::bind(bind_addr) {
        Ok(l) => l,
        Err(e) => {
            let _ = writeln!(
                err,
                "aterm harness accounts serve: could not bind {bind_addr}: {e}"
            );
            return ExitCode::FAILURE;
        }
    };
    let local_addr = listener
        .local_addr()
        .map_or_else(|_| bind_addr.to_owned(), |a| a.to_string());
    let _ = writeln!(
        out,
        "aterm accounts LAN service listening on http://{local_addr}/"
    );
    let _ = writeln!(
        out,
        "  LAN fleet config: http://{local_addr}/{FLEET_ROSTER_FILE}"
    );
    let _ = writeln!(
        out,
        "  peer sync cmd:    aterm harness accounts sync http://{local_addr}/{FLEET_ROSTER_FILE}"
    );
    let _ = out.flush();

    for stream in listener.incoming() {
        let Ok(mut stream) = stream else {
            continue;
        };
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
        let deadline = Instant::now() + Duration::from_secs(5);
        let req = read_request_head(|buf| {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "request timed out"));
            }
            stream.set_read_timeout(Some(remaining))?;
            stream.read(buf)
        });
        let Ok(req) = req else {
            let _ = write_http(
                &mut stream,
                "400 Bad Request",
                "text/plain",
                b"invalid request",
            );
            continue;
        };
        let request = stream
            .local_addr()
            .ok()
            .and_then(|addr| admit_request(&req, bind_addr, addr));
        let Some((method, path_and_query)) = request else {
            let _ = write_http(
                &mut stream,
                "403 Forbidden",
                "text/plain",
                b"request refused",
            );
            continue;
        };
        let (path, query) = path_and_query
            .split_once('?')
            .unwrap_or((path_and_query, ""));
        let params = parse_query(query);

        let mut fresh_env = env.clone();
        fresh_env.now = crate::supervise::limit::unix_now();

        match (method, path) {
            ("GET", "/fleet-accounts.toml") => match load_fleet_source(&fresh_env, source) {
                Ok(fleet) => {
                    let body = fleet.to_toml();
                    let _ = write_http(
                        &mut stream,
                        "200 OK",
                        "text/plain; charset=utf-8",
                        body.as_bytes(),
                    );
                }
                Err(why) => {
                    let _ = write_http(
                        &mut stream,
                        "500 Internal Server Error",
                        "text/plain; charset=utf-8",
                        why.as_bytes(),
                    );
                }
            },
            ("GET", "/accounts.toml") => {
                let req_machine = params
                    .get("machine")
                    .map(String::as_str)
                    .or(machine_override);
                let Some(machine) = detect_machine_id(req_machine, fresh_env.config.as_deref())
                else {
                    let _ = write_http(
                        &mut stream,
                        "400 Bad Request",
                        "text/plain; charset=utf-8",
                        b"missing ?machine=mN",
                    );
                    continue;
                };
                match load_fleet_source(&fresh_env, source).and_then(|fleet| {
                    let store = fresh_env.accounts_store();
                    let roster = build_machine_roster(&fleet, &machine, &store, None, None);
                    format_roster_toml(&roster)
                }) {
                    Ok(body) => {
                        let _ = write_http(
                            &mut stream,
                            "200 OK",
                            "text/plain; charset=utf-8",
                            body.as_bytes(),
                        );
                    }
                    Err(why) => {
                        let _ = write_http(
                            &mut stream,
                            "500 Internal Server Error",
                            "text/plain; charset=utf-8",
                            why.as_bytes(),
                        );
                    }
                }
            }
            ("GET", "/status.json") => {
                let mut report_out = Vec::new();
                let mut report_err = Vec::new();
                let reported = super::cli::run_accounts(
                    &super::cli::AccountsCmd::Report { json: true },
                    &fresh_env,
                    &super::cli::LiveAccounts,
                    &mut report_out,
                    &mut report_err,
                );
                if reported == ExitCode::SUCCESS {
                    let _ = write_http(
                        &mut stream,
                        "200 OK",
                        "application/json; charset=utf-8",
                        &report_out,
                    );
                } else {
                    let _ = write_http(
                        &mut stream,
                        "500 Internal Server Error",
                        "text/plain; charset=utf-8",
                        &report_err,
                    );
                }
            }
            ("POST", "/sync") => {
                let mut sync_out = Vec::new();
                let mut sync_err = Vec::new();
                let synced = run_accounts_sync(
                    &fresh_env,
                    source,
                    machine_override,
                    false,
                    &mut sync_out,
                    &mut sync_err,
                );
                let _ = write_action_reply(&mut stream, synced, &sync_err);
            }
            ("POST", "/use") => {
                let mut use_out = Vec::new();
                let mut use_err = Vec::new();
                let cmd = if params.get("auto").is_some_and(|v| v == "1") {
                    Some(super::cli::AccountsCmd::Auto)
                } else {
                    params
                        .get("label")
                        .cloned()
                        .map(|label| super::cli::AccountsCmd::Use { label })
                };
                let Some(cmd) = cmd else {
                    let _ = write_http(
                        &mut stream,
                        "400 Bad Request",
                        "text/plain; charset=utf-8",
                        b"missing label or auto=1",
                    );
                    continue;
                };
                let used = super::cli::run_accounts(
                    &cmd,
                    &fresh_env,
                    &super::cli::LiveAccounts,
                    &mut use_out,
                    &mut use_err,
                );
                let _ = write_action_reply(&mut stream, used, &use_err);
            }
            ("GET", "/") => {
                let html = render_dashboard_html(
                    &fresh_env,
                    source,
                    params
                        .get("machine")
                        .map(String::as_str)
                        .or(machine_override),
                    &local_addr,
                );
                let _ = write_http(
                    &mut stream,
                    "200 OK",
                    "text/html; charset=utf-8",
                    html.as_bytes(),
                );
            }
            _ => {
                let _ = write_http(
                    &mut stream,
                    "404 Not Found",
                    "text/plain; charset=utf-8",
                    b"not found",
                );
            }
        }
    }
    ExitCode::SUCCESS
}

fn read_request_head(mut read: impl FnMut(&mut [u8]) -> io::Result<usize>) -> io::Result<String> {
    let mut bytes = [0u8; 8192];
    let mut used = 0;
    while used < bytes.len() {
        let n = read(&mut bytes[used..])?;
        if n == 0 {
            break;
        }
        used += n;
        if let Some(end) = bytes[..used].windows(4).position(|b| b == b"\r\n\r\n") {
            return String::from_utf8(bytes[..end + 4].to_vec())
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid HTTP head"));
        }
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "incomplete or oversized HTTP head",
    ))
}

fn admit_request<'a>(head: &'a str, bind: &str, local: SocketAddr) -> Option<(&'a str, &'a str)> {
    let mut lines = head.strip_suffix("\r\n\r\n")?.split("\r\n");
    let mut first = lines.next()?.split_ascii_whitespace();
    let method = first.next()?;
    let target = first.next()?;
    let version = first.next()?;
    if first.next().is_some()
        || !matches!(version, "HTTP/1.0" | "HTTP/1.1")
        || !target.starts_with('/')
        || target.starts_with("//")
    {
        return None;
    }
    let mut host = None;
    let mut origin = None;
    let mut fetch_site = None;
    let mut action = None;
    for line in lines {
        if line.starts_with([' ', '\t']) {
            return None;
        }
        let (name, value) = line.split_once(':')?;
        let value = value.trim();
        let field = if name.eq_ignore_ascii_case("host") {
            &mut host
        } else if name.eq_ignore_ascii_case("origin") {
            &mut origin
        } else if name.eq_ignore_ascii_case("sec-fetch-site") {
            &mut fetch_site
        } else if name.eq_ignore_ascii_case("x-aterm-accounts-action") {
            &mut action
        } else {
            continue;
        };
        if field.replace(value).is_some() {
            return None;
        }
    }
    let host = host?;
    let authority = |s: &str| s.strip_suffix(":80").unwrap_or(s).to_ascii_lowercase();
    let host_key = authority(host);
    let bound_host = bind.rsplit_once(':')?.0;
    let bound = format!("{bound_host}:{}", local.port());
    let localhost = format!("localhost:{}", local.port());
    if host_key != authority(&local.to_string())
        && host_key != authority(&bound)
        && !(local.ip().is_loopback() && host_key == authority(&localhost))
    {
        return None;
    }
    if method == "POST" {
        if fetch_site.is_some_and(|site| !matches!(site, "same-origin" | "none")) {
            return None;
        }
        match origin {
            Some(value)
                if value
                    .strip_prefix("http://")
                    .is_some_and(|o| authority(o) == host_key) => {}
            None if action == Some("1") => {}
            _ => return None,
        }
    }
    Some((method, target))
}

fn parse_query(query: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for pair in query.split('&') {
        if let Some((k, v)) = pair.split_once('=')
            && !k.is_empty()
        {
            map.insert(k.to_owned(), v.to_owned());
        }
    }
    map
}

fn write_http(
    stream: &mut impl Write,
    status: &str,
    content_type: &str,
    body: &[u8],
) -> std::io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)?;
    stream.flush()
}

fn write_redirect(stream: &mut impl Write, location: &str) -> std::io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 303 See Other\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )?;
    stream.flush()
}

fn write_action_reply(
    stream: &mut impl Write,
    done: ExitCode,
    diagnostics: &[u8],
) -> std::io::Result<()> {
    if done == ExitCode::SUCCESS {
        write_redirect(stream, "/")
    } else {
        write_http(
            stream,
            "500 Internal Server Error",
            "text/plain; charset=utf-8",
            diagnostics,
        )
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn render_dashboard_html(
    env: &Env,
    source: Option<&str>,
    machine_override: Option<&str>,
    local_addr: &str,
) -> String {
    render_dashboard_html_with(env, source, machine_override, local_addr, || {})
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountDashboard",
        action = "Draw",
        project = "aterm_agent::harness::accounts_sync::dashboard_tests::dashboard_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountDashboard",
        action = "AnotherFault",
        reason = "Fixture selection supplies a clean, interrupted or refused record read."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountDashboard",
        action = "LaterChange",
        reason = "Fixture selection changes records after the report has been captured."
    )
)]
fn render_dashboard_html_with(
    env: &Env,
    source: Option<&str>,
    machine_override: Option<&str>,
    local_addr: &str,
    after_report: impl FnOnce(),
) -> String {
    let machine = detect_machine_id(machine_override, env.config.as_deref())
        .unwrap_or_else(|| "unknown".to_owned());
    let mut report_out = Vec::new();
    let mut report_err = Vec::new();
    let report = super::cli::write_accounts_report(
        (Cli::Claude, super::cli::Launch::default()),
        env,
        false,
        &mut report_out,
        &mut report_err,
    );
    let report_text = format!(
        "{}{}",
        String::from_utf8_lossy(&report_out),
        String::from_utf8_lossy(&report_err)
    );
    after_report();
    let rows_html = dashboard_rows_html(report.local.as_ref(), env.now);
    let pools = env
        .config
        .as_deref()
        .zip(env.home.as_deref())
        .map(|(config, home)| cli_pools(config, &env.state, home, env.now))
        .unwrap_or_default();
    let shared_html = shared_accounts_html(
        report.catalog.as_ref(),
        report.local.as_ref(),
        &pools,
        env.now,
    );

    let fleet_summary = match load_fleet_source(env, source) {
        Ok(fleet) => format!(
            "{} active Claude accounts across {} machines (source: {})",
            fleet.accounts.len(),
            fleet.machines().len(),
            html_escape(&fleet.source)
        ),
        Err(why) => format!("fleet source error: {}", html_escape(&why)),
    };

    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\">\
         <title>aterm Accounts ({machine})</title>\
         <style>\
         body{{font-family:-apple-system,system-ui,sans-serif;background:#11161d;color:#e6edf3;margin:24px;line-height:1.45}}\
         h1,h2{{margin:0 0 12px 0}} .card{{background:#161b22;border:1px solid #30363d;border-radius:8px;padding:16px;margin-bottom:16px}}\
         table{{width:100%;border-collapse:collapse}} th,td{{text-align:left;padding:8px 10px;border-bottom:1px solid #21262d}}\
         code,pre{{background:#0d1117;padding:3px 6px;border-radius:4px;font-family:ui-monospace,monospace}}\
         pre{{padding:12px;overflow-x:auto}} button{{background:#238636;color:#fff;border:0;padding:6px 12px;border-radius:6px;cursor:pointer;margin-right:8px}}\
         </style></head><body>\
         <h1>aterm Account Rotation &mdash; Machine <code>{machine}</code></h1>\
         <div class=\"card\">\
           <form method=\"POST\" action=\"/sync\" style=\"display:inline\"><button type=\"submit\">Sync from the configured source</button></form>\
           <form method=\"POST\" action=\"/use?auto=1\" style=\"display:inline\"><button type=\"submit\">Auto Rotate (--auto)</button></form>\
           <a href=\"/fleet-accounts.toml\" style=\"color:#58a6ff;margin-left:12px\">fleet-accounts.toml</a>\
           <a href=\"/accounts.toml?machine={machine}\" style=\"color:#58a6ff;margin-left:12px\">accounts.toml ({machine})</a>\
           <a href=\"/status.json\" style=\"color:#58a6ff;margin-left:12px\">status.json</a>\
           <p style=\"margin:10px 0 0 0;color:#8b949e\">{fleet_summary}<br>\
           LAN sync command for any machine: <code>aterm harness accounts sync http://{local_addr}/fleet-accounts.toml</code></p>\
         </div>\
         {shared_html}\
         <div class=\"card\"><h2>Machine <code>{machine}</code> Local Logins</h2>\
           <table><thead><tr><th>Label</th><th>Class</th><th>Condition</th><th>Role</th><th>Email / OAuth Sign-In Command</th><th>Action</th></tr></thead>\
           <tbody>{rows_html}</tbody></table>\
         </div>\
         {pools_html}\
         <div class=\"card\"><h2>Live Harness Report (<code>aterm harness accounts</code>)</h2>\
           <pre>{}</pre>\
         </div></body></html>",
        html_escape(&report_text),
        pools_html = pools_html(&pools, env.now),
    )
}

/// The shared list is read-only. Local runtime facts are joined for display;
/// controls below continue to address only admitted local roster rows.
fn shared_accounts_html(
    catalog: Option<&Result<super::account_catalog::Catalog, String>>,
    claude: Option<&super::cli::ReportedAccounts>,
    pools: &[CliPool],
    now: i64,
) -> String {
    let Some(catalog) = catalog else {
        return String::new();
    };
    let catalog = match catalog {
        Ok(catalog) => catalog,
        Err(why) => {
            return format!(
                "<div class=\"card\"><h2>Shared Accounts</h2><p>Unavailable: {}</p></div>",
                html_escape(why)
            );
        }
    };
    let mut rows = String::new();
    for account in &catalog.rows {
        let local = if catalog.history_complete {
            Cli::ALL
                .into_iter()
                .find(|cli| account.belongs_to(*cli))
                .and_then(|cli| {
                    let (roster, state, enrolled) = if cli == Cli::Claude {
                        let local = claude?;
                        (&local.roster, &local.state, &local.enrolled)
                    } else {
                        let pool = pools.iter().find(|pool| pool.cli == cli)?;
                        (pool.rows.as_ref().ok()?, &pool.state, &pool.enrolled)
                    };
                    let label = catalog.local_label(account, roster)?;
                    let row = roster.row(label)?;
                    let here = row.dir.is_none() || enrolled.contains(label);
                    Some(format!(
                        "{}: {}",
                        label,
                        accounts::condition(row, state, here, now).as_str()
                    ))
                })
        } else {
            None
        };
        let source = account
            .import
            .as_ref()
            .map(|source| {
                format!(
                    "Imported row {}, provider column {}, marker {}; machine hint {}",
                    source.record,
                    source
                        .provider_column
                        .map_or_else(|| "none".into(), |column| column.to_string()),
                    source.presence.map_or("unknown", |present| if present {
                        "present"
                    } else {
                        "not indicated"
                    }),
                    source.machine_hint.as_deref().unwrap_or("none"),
                )
            })
            .unwrap_or_else(|| "Shared proposal".into());
        let origin = if account.import.is_some() {
            "import-candidate"
        } else {
            "proposal"
        };
        rows.push_str(&format!(
            "<tr data-origin=\"{}\" data-state=\"{}\"><td>{}<br>{}<br><code>{}</code><br>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
            origin, account.state(), html_escape(&account.label), html_escape(account.email.as_deref().unwrap_or("Email not supplied")),
            html_escape(&account.key), html_escape(&source), html_escape(&account.provider),
            html_escape(account.method.as_deref().unwrap_or("Not supplied")),
            html_escape(local.as_deref().unwrap_or("Not configured here")), html_escape(account.state_words()),
        ));
    }
    if rows.is_empty() {
        rows = "<tr><td colspan=\"5\">No shared accounts or imported candidates.</td></tr>".into();
    }
    let history = if catalog.history_complete {
        ""
    } else {
        "<p>Shared account history is incomplete; local status is withheld.</p>"
    };
    format!(
        "<div class=\"card\"><h2>Shared Accounts</h2>{history}<table><thead><tr><th>Account</th><th>Provider</th><th>Sign-in method</th><th>Local state</th><th>Preparation</th></tr></thead><tbody>{rows}</tbody></table></div>"
    )
}

fn dashboard_rows_html(report: Option<&super::cli::ReportedAccounts>, now: i64) -> String {
    let Some(super::cli::ReportedAccounts {
        roster,
        state,
        enrolled,
    }) = report
    else {
        return "<tr><td colspan=\"6\">Pool state is unknown; see the report below.</td></tr>"
            .into();
    };
    let mut rows_html = String::new();
    for row in &roster.rows {
        let here = row.dir.is_none() || enrolled.contains(&row.label);
        let cond = match accounts::condition(row, state, here, now) {
            accounts::Condition::Limited { until } => {
                format!("limited until {}", super::usage::rfc3339_utc(until))
            }
            other => other.as_str().to_owned(),
        };
        let active_badge = if state.active.as_deref() == Some(row.label.as_str()) {
            if state.pinned { "pinned" } else { "active" }
        } else if state.active.is_none() && row.dir.is_none() {
            "default (in use)"
        } else {
            "-"
        };
        let login_cmd = format!("aterm harness accounts renew {}", row.label);
        rows_html.push_str(&format!(
            "<tr><td><code>{}</code></td><td>{}</td><td>{}</td><td>{}</td>\
             <td><code>{}</code></td>\
             <td><form method=\"POST\" action=\"/use?label={}\" style=\"display:inline\">\
             <button type=\"submit\">Use</button></form></td></tr>\n",
            html_escape(&row.label),
            html_escape(row.class.as_str()),
            html_escape(&cond),
            html_escape(active_badge),
            html_escape(&login_cmd),
            html_escape(&row.label),
        ));
    }

    rows_html
}

/// The dashboard's card for each other CLI's pool this machine keeps ([`cli_pools`]): its rows,
/// each with its vendor keys, condition and `--cli` sign-in ([`Cli::sign_in_verb`]); its pins and
/// sign-ins are its commands' (`aterm harness accounts use|renew|login <label> --cli <id>`),
/// never a button here.
/// Nothing where this machine keeps none.
fn pools_html(pools: &[CliPool], now: i64) -> String {
    let mut out = String::new();
    for pool in pools {
        let product = pool.cli.connector().product;
        let id = pool.cli.id();
        let roster = html_escape(&pool.roster.display().to_string());
        let rows = match &pool.rows {
            Err(why) => format!("<p>The pool is refused: {}</p>", html_escape(why)),
            Ok(rows) if rows.rows.is_empty() => format!(
                "<p>No accounts listed: <code>aterm harness accounts add &lt;label&gt; --cli \
                 {id}</code></p>"
            ),
            Ok(rows) => {
                let mut body = String::new();
                for row in &rows.rows {
                    let here = row.dir.is_none() || pool.enrolled.contains(&row.label);
                    let cond = match accounts::condition(row, &pool.state, here, now) {
                        accounts::Condition::Limited { until } => {
                            format!("limited until {}", super::usage::rfc3339_utc(until))
                        }
                        other => other.as_str().to_owned(),
                    };
                    let keys = [
                        row.vendor.region.map(Region::as_str),
                        row.vendor.flow.map(Flow::as_str),
                    ]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(" ");
                    let role = if pool.state.active.as_deref() == Some(row.label.as_str()) {
                        if pool.state.pinned {
                            "pinned"
                        } else {
                            "active"
                        }
                    } else {
                        "-"
                    };
                    body.push_str(&format!(
                        "<tr><td><code>{}</code></td><td>{}</td><td>{}</td><td>{}</td>\
                         <td>{}</td><td><code>aterm harness accounts {} {} --cli {id}</code>\
                         </td></tr>\n",
                        html_escape(&row.label),
                        html_escape(row.class.as_str()),
                        html_escape(&keys),
                        html_escape(&cond),
                        role,
                        pool.cli.sign_in_verb(row.email.is_some()),
                        html_escape(&row.label),
                    ));
                }
                format!(
                    "<table><thead><tr><th>Label</th><th>Class</th><th>Vendor</th>\
                     <th>Condition</th><th>Role</th><th>Sign-In Command</th></tr></thead>\
                     <tbody>{body}</tbody></table>"
                )
            }
        };
        out.push_str(&format!(
            "<div class=\"card\"><h2>{product} Accounts</h2>\
             <p style=\"color:#8b949e\">Roster: <code>{roster}</code> &middot; pin one with \
             <code>aterm harness accounts use &lt;label&gt; --cli {id}</code></p>{rows}</div>"
        ));
    }
    out
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountRecoveryPolicy",
        action = "AnotherProvider",
        reason = "Model bookkeeping: the model steps through its fixed providers, and the conformance test stages each provider's fixture for the bound Recover"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountRecoveryPolicy",
        action = "AnotherCase",
        reason = "Model bookkeeping: the model steps through its fixed cases, and the conformance test stages each case's fixture for the bound Recover"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountRecoveryPolicy",
        action = "Choose",
        reason = "Model bookkeeping: the model fixes the case it checks before the bound Recover"
    )
)]
#[doc(hidden)]
pub fn account_recovery_policy_environment() {}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountRecoveryLock",
        action = "AnotherCase",
        reason = "Model bookkeeping: conformance stages each contention and checkpoint fixture before running the bound Recover"
    )
)]
#[doc(hidden)]
pub fn account_recovery_lock_environment() {}

/// Environment choices for the current identity beside a saved renewal check.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountRecoveryIdentity",
        action = "Provider",
        reason = "The native fixture chooses Claude or Codex; recovery receives its provider."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountRecoveryIdentity",
        action = "Credential",
        reason = "The native fixture supplies unknown, matching or contradictory local identity metadata."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountRecoveryIdentity",
        action = "Timing",
        reason = "The native fixture replaces the login before scanning or after the sign-in lock is held."
    )
)]
#[doc(hidden)]
pub fn account_recovery_identity_environment() {}

#[cfg(test)]
#[path = "accounts_recovery_policy_tests.rs"]
mod recovery_policy_tests;

#[cfg(test)]
#[path = "accounts_recovery_lock_tests.rs"]
mod recovery_lock_tests;

#[cfg(test)]
#[path = "accounts_completion_identity_tests.rs"]
mod completion_identity_tests;

#[cfg(test)]
#[path = "accounts_sync_rename_tests.rs"]
mod rename_tests;

#[cfg(test)]
#[path = "accounts_alias_relabel_tests.rs"]
mod alias_relabel_tests;

#[cfg(test)]
#[path = "accounts_sync_report_tests.rs"]
mod sync_report_tests;

#[cfg(test)]
#[path = "accounts_dashboard_tests.rs"]
mod dashboard_tests;

#[cfg(test)]
#[path = "accounts_renewal_read_tests.rs"]
mod renewal_read_tests;

#[cfg(test)]
#[path = "accounts_shared_selection_tests.rs"]
mod shared_selection_tests;

#[cfg(test)]
#[path = "accounts_auto_shared_tests.rs"]
mod automatic_shared_tests;

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountRenewalRead",
        action = "AnotherProvider",
        reason = "Native fixture selection for the four pooled providers."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountRenewalRead",
        action = "AnotherFault",
        reason = "Native fixture selection for recovered, damaged and held records."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountRenewalRead",
        action = "LaterChange",
        reason = "Native fixture selection for publication after candidate capture."
    )
)]
#[doc(hidden)]
pub fn account_renewal_read_environment() {}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountSyncReport",
        action = "AnotherFault",
        reason = "Fixture selection for journal, roster and record failures before the bound read."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountSyncReport",
        action = "Published",
        reason = "Fixture selection for a sync which already changed its admitted source."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountSyncReport",
        action = "LaterEdit",
        reason = "Fixture selection for a later publication after the report was captured."
    )
)]
#[doc(hidden)]
pub fn account_sync_report_environment() {}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountAliasRelabel",
        action = "AnotherCase",
        reason = "Fixture selection for retained login identity and relocation scenarios."
    )
)]
#[doc(hidden)]
pub fn alias_relabel_environment() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_actions_require_complete_headers_and_the_dashboard_origin() {
        let local: SocketAddr = "127.0.0.1:8741".parse().unwrap();
        let request = |fields: &str| format!("POST /use?auto=1 HTTP/1.1\r\n{fields}\r\n");
        let valid = request("Host: localhost:8741\r\nOrigin: http://localhost:8741\r\n");
        let mut input = valid.as_bytes();
        let head = read_request_head(|buf| {
            let len = buf.len().min(3);
            input.read(&mut buf[..len])
        })
        .unwrap();
        assert_eq!(
            admit_request(&head, DEFAULT_SERVE_BIND, local),
            Some(("POST", "/use?auto=1"))
        );
        for fields in [
            "Host: localhost:8741\r\n",
            "Host: localhost:8741\r\nOrigin: http://elsewhere.test\r\n",
            "Host: elsewhere.test\r\nOrigin: http://elsewhere.test\r\n",
            "Host: localhost:8741\r\nOrigin: null\r\n",
            "Host: localhost:8741\r\nOrigin: http://localhost:8741\r\nOrigin: http://elsewhere.test\r\n",
            "Host: localhost:8741\r\nHost: elsewhere.test\r\nOrigin: http://localhost:8741\r\n",
            "Host: localhost:8741\r\nOrigin: http://localhost:8741\r\nSec-Fetch-Site: cross-site\r\n",
            "Host: localhost:8741\r\nOrigin: http://localhost:8741\r\nSec-Fetch-Site: same-site\r\n",
            "Host: localhost:8741\r\n Origin: http://localhost:8741\r\n",
            "Host: localhost:8741\r\nX-Aterm-Accounts-Action: 0\r\n",
            "Host: localhost:8741\r\nX-Aterm-Accounts-Action: 1\r\nOrigin: http://elsewhere.test\r\n",
        ] {
            assert_eq!(
                admit_request(&request(fields), DEFAULT_SERVE_BIND, local),
                None,
                "{fields}"
            );
        }
        let explicit = request("Host: localhost:8741\r\nX-Aterm-Accounts-Action: 1\r\n");
        assert!(admit_request(&explicit, DEFAULT_SERVE_BIND, local).is_some());
        let get = "GET /fleet-accounts.toml HTTP/1.1\r\nHost: localhost:8741\r\n\r\n";
        assert_eq!(
            admit_request(get, DEFAULT_SERVE_BIND, local),
            Some(("GET", "/fleet-accounts.toml"))
        );
        assert!(
            admit_request(
                &get.replace("localhost:8741", "elsewhere.test"),
                DEFAULT_SERVE_BIND,
                local
            )
            .is_none()
        );
        for (bind, addr, host) in [
            ("0.0.0.0:8741", "192.0.2.1:8741", "192.0.2.1:8741"),
            ("machine.local:0", "192.0.2.1:8741", "machine.local:8741"),
            ("[::1]:8741", "[::1]:8741", "[::1]:8741"),
            ("127.0.0.1:80", "127.0.0.1:80", "localhost"),
        ] {
            let req = request(&format!("Host: {host}\r\nOrigin: http://{host}\r\n"));
            assert!(
                admit_request(&req, bind, addr.parse().unwrap()).is_some(),
                "{req}"
            );
        }
        for raw in [
            b"POST /sync HTTP/1.1\r\nHost: localhost:8741\r\n".to_vec(),
            vec![b'x'; 8192],
        ] {
            let mut input = raw.as_slice();
            assert!(read_request_head(|buf| input.read(buf)).is_err());
        }
        let mut padded = format!(
            "GET / HTTP/1.1\r\nHost: localhost:8741\r\nX-Padding: {}\r\n\r\nbody",
            "a".repeat(8000)
        );
        let mut input = padded.as_bytes();
        let head = read_request_head(|buf| input.read(buf)).unwrap();
        assert!(!head.ends_with("body"));
        assert!(admit_request(&head, DEFAULT_SERVE_BIND, local).is_some());
        let padding = "a".repeat(8192 - head.len() + 8000);
        let exact = head.replace(&"a".repeat(8000), &padding);
        assert_eq!(exact.len(), 8192);
        let mut input = exact.as_bytes();
        assert!(read_request_head(|buf| input.read(buf)).is_ok());
        let oversized = exact.replacen("X-Padding: ", "X-Padding: a", 1);
        let mut input = oversized.as_bytes();
        assert!(read_request_head(|buf| input.read(buf)).is_err());
        padded.insert_str(0, "not-http ");
        assert!(admit_request(&padded, DEFAULT_SERVE_BIND, local).is_none());
    }

    const SAMPLE_CSV: &str = "\
m,email,computer,,claude,Open AI,google,meta,Payment,notes,,
,alpha@example.test,m8,,1,1,1,,,\"logged in, seems oK?\",,
,bravo.m8@example.test,m8.2,,1,,,,a card,,,
,charlie@example.test,*m12,,1,,1,,,claude account payment disabled. roll off this account.,,
,delta@example.test,m106,m38,1,,,,a card,delta,,
,echo-m31@example.test,m31,,0,1,,,,banned from Claude Code,,
,foxtrot.m38.106.2@example.test,m38.2,,1,,,,,a note,,
,,,,102,,\"$20,400\",,,,,
";

    #[test]
    fn parses_accounting_csv_and_assigns_machine_slots_and_shared_classes() {
        let mut boundary = b"email,computer,claude,notes\nwithin@example.test,m30,1,".to_vec();
        boundary.resize(SOURCE_TEXT_MAX_BYTES, b'x');
        let admitted = decode_source_text(boundary.clone(), "boundary.csv").unwrap();
        assert_eq!(admitted.len(), SOURCE_TEXT_MAX_BYTES);
        let fleet = parse_accounting_csv(&admitted, "boundary.csv").unwrap();
        assert_eq!(fleet.accounts.len(), 1);
        assert_eq!(fleet.accounts[0].email, "within@example.test");
        boundary.push(b'\n');
        assert!(decode_source_text(boundary, "oversized.csv").is_err());
        assert!(decode_source_text(vec![0xff], "invalid-utf8.csv").is_err());

        let owned = aterm_tempfile::Builder::new()
            .prefix("aterm-account-source-")
            .tempdir()
            .expect("exclusive source fixtures");
        let source = owned.path().join("source.csv");
        fs::write(&source, &admitted).unwrap();
        let text = read_local_source_text(&source).expect("the exact boundary is admitted");
        assert_eq!(text, admitted);
        let fleet = parse_accounting_csv(&text, "source.csv").unwrap();
        assert_eq!(fleet.accounts[0].email, "within@example.test");
        fs::OpenOptions::new()
            .write(true)
            .open(&source)
            .unwrap()
            .set_len((SOURCE_TEXT_MAX_BYTES + 1) as u64)
            .unwrap();
        assert!(read_local_source_text(&source).is_err());
        fs::write(&source, [0xff]).unwrap();
        assert!(read_local_source_text(&source).is_err());
        assert!(read_local_source_text(owned.path()).is_err());
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt as _;
            fs::write(&source, "ordinary@example.test").unwrap();
            let link = owned.path().join("source-link");
            std::os::unix::fs::symlink(&source, &link).unwrap();
            assert!(read_local_source_text(&link).is_err());
            let fifo = owned.path().join("source.fifo");
            let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
            // SAFETY: `name` is a NUL-terminated path in this test's exclusive directory, and
            // mkfifo retains no pointer.
            assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
            assert!(read_local_source_text(&fifo).is_err());
            assert_eq!(
                read_local_source_text(&source).unwrap(),
                "ordinary@example.test"
            );
        }

        for ending in ["\n", "\r\n"] {
            for marker in [
                "banned",
                "roll off this account",
                "payment disabled",
                "deactivated",
                "SuSpEnDeD",
            ] {
                let text = format!(
                    "email,computer,claude,notes{ending}\"blocked@example.test\",m20,1,\"reviewed{ending}{marker}\"{ending}ok@example.test,m21,1,usable{ending}"
                );
                let parsed = parse_accounting_csv(&text, "captured.csv").unwrap();
                assert_eq!(parsed.accounts.len(), 1, "{marker}: {ending:?}");
                assert_eq!(parsed.accounts[0].email, "ok@example.test");
                assert_eq!(parsed.accounts[0].label, "m21");
            }
        }

        let text = concat!(
            "\"email\",computer,claude,notes\r\n",
            "\"first@example.test\",\"m22\",\"1\",\"ready, \"\"quoted\"\"\r\n",
            "phantom@example.test,m99,1,still a note\r\n",
            "ready\"\r\n",
            "second@example.test,m23,1,\"ordinary\""
        );
        let parsed = parse_accounting_csv(text, "captured.csv").unwrap();
        assert_eq!(
            parsed
                .accounts
                .iter()
                .map(|a| a.label.as_str())
                .collect::<Vec<_>>(),
            ["m22", "m23"]
        );
        assert_eq!(
            parse_csv_line("\"a,b\",\"a\"\"b\",\"line1\r\nline2\","),
            ["a,b", "a\"b", "line1\r\nline2", ""]
        );

        for bad in [
            "bad@example.test,m24,1,\"unterminated\nnext line",
            "bad@example.test,m24,1,unquoted\"quote",
            "bad@example.test,m24,1,\"closed\"trailing",
            "bad@example.test,m24,1,\"closed\"\"",
        ] {
            let text = format!("email,computer,claude,notes\nok@example.test,m21,1,usable\n{bad}");
            assert!(parse_accounting_csv(&text, "malformed.csv").is_err());
        }
        assert!(parse_accounting_csv("\"email,computer,claude", "header.csv").is_err());

        let fleet = parse_accounting_csv(SAMPLE_CSV, "sample.csv").expect("valid csv");
        assert_eq!(fleet.accounts.len(), 4);

        let m8 = fleet.accounts_for_machine("m8");
        assert_eq!(m8.len(), 2);
        assert_eq!(m8[0].label, "m8");
        assert_eq!(m8[0].email, "alpha@example.test");
        assert_eq!(m8[0].class, Class::Dedicated);
        assert_eq!(m8[1].label, "m8-2");
        assert_eq!(m8[1].email, "bravo.m8@example.test");
        assert_eq!(m8[1].class, Class::Dedicated);

        let m38 = fleet.accounts_for_machine("m38");
        assert_eq!(m38.len(), 2);
        assert!(m38.iter().all(|a| a.class == Class::Shared));
        let m106 = fleet.accounts_for_machine("m106");
        assert_eq!(m106.len(), 2);

        let toml = fleet.to_toml();
        let reparsed = FleetRoster::from_toml(&toml, "fleet-accounts.toml").expect("valid toml");
        assert_eq!(reparsed.accounts, fleet.accounts);

        let mut fleet = fleet.clone();
        let controls: String = (0u8..=31).map(char::from).collect();
        fleet.accounts[0].browser = Some(format!("{controls}\u{7f}\"\\ profile \u{a0} \u{1f680}"));
        let reparsed = FleetRoster::from_toml(&fleet.to_toml(), "captured.toml").unwrap();
        assert_eq!(reparsed.accounts, fleet.accounts);

        let mut kimi_fleet = fleet.clone();
        for acct in &mut kimi_fleet.accounts {
            acct.vendor.region = Some(Region::Global);
        }
        let reparsed_kimi =
            FleetRoster::from_toml_for(Cli::Kimi, &kimi_fleet.to_toml(), "kimi.toml").unwrap();
        assert_eq!(reparsed_kimi.accounts, kimi_fleet.accounts);

        let mut codex_fleet = fleet.clone();
        for acct in &mut codex_fleet.accounts {
            acct.vendor.flow = Some(Flow::Device);
        }
        let reparsed_codex =
            FleetRoster::from_toml_for(Cli::Codex, &codex_fleet.to_toml(), "codex.toml").unwrap();
        assert_eq!(reparsed_codex.accounts, codex_fleet.accounts);

        let duplicates = "email,computer,claude\none@example.test,m8,1\ntwo@example.test,m8,1\n";
        let fleet = parse_accounting_csv(duplicates, "captured.csv").unwrap();
        assert_eq!(fleet.accounts[0].label, "m8");
        assert_eq!(fleet.accounts[1].label, "m8-2");

        let long = (100_000_000..100_000_008)
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join(".");
        let csv = format!("email,computer,claude\none@example.test,m{long},1\n");
        assert!(parse_accounting_csv(&csv, "captured.csv").is_err());
        let boundary = "m100000000.100000001.100000002.100000003.100000004.10000000";
        let csv = format!("email,computer,claude\none@example.test,{boundary},1\n");
        let fleet = parse_accounting_csv(&csv, "captured.csv").unwrap();
        assert_eq!(fleet.accounts[0].label.len(), accounts::LABEL_MAX);
        let csv = format!("{csv}two@example.test,{boundary},1\n");
        assert!(parse_accounting_csv(&csv, "captured.csv").is_err());
    }

    #[test]
    fn fleet_sync_keeps_an_explicit_claude_alias_only_for_the_same_login_address() {
        let w = World::new();
        let store = accounts::Store::at(&w.state);
        let mut fleet = parse_accounting_csv(
            "email,computer,claude\nfirst@example.test,m8,1\nalias@example.test,m8,1\n",
            "aliases.csv",
        )
        .unwrap();
        let mut existing = build_machine_roster(&fleet, "m8", &store, None, None);
        let alias = existing
            .rows
            .iter_mut()
            .find(|row| row.email.as_deref() == Some("alias@example.test"))
            .unwrap();
        alias.identity_email = Some("canonical@example.test".into());
        let label = alias.label.clone();
        let synced = build_machine_roster(
            &fleet,
            "m8",
            &store,
            Some(&existing),
            Some("canonical@example.test"),
        );
        assert_eq!(synced.rows.len(), 2, "no duplicate canonical account");
        assert_eq!(synced.row_for_login(None).unwrap().label, label);
        assert_eq!(
            synced.row(&label).unwrap().identity_email.as_deref(),
            Some("canonical@example.test")
        );
        assert_eq!(
            Roster::parse(&format_roster_toml(&synced).unwrap()).unwrap(),
            synced
        );

        let mut omitted = fleet.clone();
        omitted.accounts.retain(|row| row.label != label);
        let unlisted = build_machine_roster(
            &omitted,
            "m8",
            &store,
            Some(&synced),
            Some("canonical@example.test"),
        );
        assert_eq!(unlisted.rows.len(), 2);
        let own = unlisted.row_for_login(None).unwrap();
        assert_eq!(own.label, label);
        assert_eq!(own.email.as_deref(), Some("alias@example.test"));
        assert_eq!(
            own.identity_email.as_deref(),
            Some("canonical@example.test")
        );

        fleet
            .accounts
            .iter_mut()
            .find(|row| row.label == label)
            .unwrap()
            .email = "replacement@example.test".into();
        let changed = build_machine_roster(&fleet, "m8", &store, Some(&synced), None);
        assert_eq!(changed.row(&label).unwrap().identity_email, None);
    }

    #[test]
    fn builds_machine_roster_adopting_current_default_profile_email() {
        let fleet = parse_accounting_csv(SAMPLE_CSV, "sample.csv").expect("valid csv");
        let tmp = std::env::temp_dir().join(format!("aterm-sync-test-{}", std::process::id()));
        let store = accounts::Store::at(&tmp);

        let roster =
            build_machine_roster(&fleet, "m8", &store, None, Some("bravo.m8@example.test"));
        assert_eq!(roster.rows.len(), 2);
        assert_eq!(roster.rows[0].label, "m8-2");
        assert_eq!(roster.rows[0].dir, None);
        assert_eq!(roster.rows[1].label, "m8");
        assert_eq!(
            roster.rows[1].dir.as_deref(),
            Some(
                store
                    .login_dir("m8")
                    .expect("admissible label")
                    .to_string_lossy()
                    .as_ref()
            )
        );

        let toml = format_roster_toml(&roster).expect("valid roster text");
        let parsed = Roster::parse(&toml).expect("roster admits");
        assert_eq!(parsed, roster);

        let mut duplicate = roster.clone();
        duplicate.rows.push(duplicate.rows[0].clone());
        assert!(format_roster_toml(&duplicate).is_err());

        let owned = aterm_tempfile::Builder::new()
            .prefix("aterm-sync-publication-")
            .tempdir()
            .expect("exclusive owned files");
        let root = owned.path();
        let store = accounts::Store::at(&root.join("state"));
        let config = root.join("config");
        fs::create_dir(&config).unwrap();
        let roster_path = config.join(accounts::ACCOUNTS_FILE);
        let target = if cfg!(unix) {
            root.join("dotfiles").join(accounts::ACCOUNTS_FILE)
        } else {
            roster_path.clone()
        };
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        let login = root.join("existing-login").to_string_lossy().into_owned();
        let previous = Roster {
            rows: vec![
                Row {
                    label: "m8".into(),
                    dir: Some(login.clone()),
                    email: Some("one@example.test".into()),
                    browser: Some("kept profile".into()),
                    ..Row::default()
                },
                Row {
                    label: "m8-2".into(),
                    email: Some("two@example.test".into()),
                    ..Row::default()
                },
            ],
        };
        fs::write(&target, format_roster_toml(&previous).unwrap()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&target, fs::Permissions::from_mode(0o640)).unwrap();
            std::os::unix::fs::symlink(&target, &roster_path).unwrap();
        }
        let csv = "email,computer,claude\none@example.test,m8,1\ntwo@example.test,m8,1\n";
        let fleet = parse_accounting_csv(csv, "captured.csv").unwrap();
        let now = 1_800_000_000;
        let mut note = None;
        store
            .transact(now, &mut note, |state| {
                let until = now + 2 * 3600;
                let mut rows = accounts::note_wall(
                    state,
                    ACCOUNTS_OWN_LABEL,
                    accounts::WEEK_LIMIT,
                    Some(until),
                    now - 60,
                    now,
                );
                assert_eq!(
                    accounts::claim_reset(
                        state,
                        ACCOUNTS_OWN_LABEL,
                        (accounts::WEEK_LIMIT, until),
                        "s-before-adoption",
                        41,
                        "start",
                        now - 30
                    ),
                    accounts::ResetClaimed::Taken
                );
                rows.extend(accounts::reset_outcome(
                    state,
                    ACCOUNTS_OWN_LABEL,
                    until,
                    "s-before-adoption",
                    &accounts::ResetDone::Used {
                        kind: aterm_phase::ResetKind::Full,
                        left: None,
                    },
                    now - 10,
                ));
                rows.extend(accounts::note_wall(
                    state,
                    ACCOUNTS_OWN_LABEL,
                    accounts::SESSION_LIMIT,
                    Some(until),
                    now - 5,
                    now,
                ));
                Ok(rows)
            })
            .expect("observed before adoption");
        let empty_walls = BTreeMap::new();
        let saved = persist_accounts_sync(
            &store,
            &roster_path,
            (&fleet, OptIn::Named),
            ("m8", Some("two@example.test")),
            &empty_walls,
            now,
            (&[], &[]),
        )
        .expect("publish captured inputs");
        let (adopted, _) = store.load(now).expect("adopted state");
        let target_account = adopted.account("m8-2");
        assert!(
            !target_account.walls.contains_key(accounts::WEEK_LIMIT),
            "a scoped clear is not replayed as a standing wall"
        );
        assert!(
            target_account.walls[accounts::SESSION_LIMIT]
                .stands_until()
                .is_some(),
            "a different live wall is still adopted"
        );
        let saved_roster = Roster::load(&roster_path).unwrap();
        assert_eq!(saved_roster.row_for_login(None).unwrap().label, "m8-2");
        assert_eq!(
            saved_roster.row("m8").unwrap().dir.as_deref(),
            Some(login.as_str())
        );
        assert_eq!(
            saved_roster.row("m8").unwrap().browser.as_deref(),
            Some("kept profile")
        );
        assert_eq!(Roster::load(&target).unwrap(), saved_roster);
        assert!(
            store.try_acquire().unwrap().is_some(),
            "the publication released its lock"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert!(roster_path.symlink_metadata().unwrap().is_symlink());
            assert_eq!(
                fs::metadata(&target).unwrap().permissions().mode() & 0o777,
                0o640
            );
            assert_eq!(
                fs::metadata(&saved.fleet_path)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }

        persist_accounts_sync(
            &store,
            &roster_path,
            (&fleet, OptIn::Named),
            ("m8", None),
            &empty_walls,
            now,
            (&[], &[]),
        )
        .expect("preserve the current default without a profile hint");
        assert_eq!(
            Roster::load(&roster_path)
                .unwrap()
                .row_for_login(None)
                .unwrap()
                .label,
            "m8-2"
        );
        let before_roster = fs::read(&target).unwrap();
        let before_fleet = fs::read(&saved.fleet_path).unwrap();
        let malformed = b"[[account]\nlabel = \"broken\"\n";
        fs::write(&target, malformed).unwrap();
        assert!(
            persist_accounts_sync(
                &store,
                &roster_path,
                (&fleet, OptIn::Named),
                ("m8", None),
                &empty_walls,
                now,
                (&[], &[])
            )
            .is_err()
        );
        assert_eq!(fs::read(&target).unwrap(), malformed);
        assert_eq!(fs::read(&saved.fleet_path).unwrap(), before_fleet);
        fs::write(&target, &before_roster).unwrap();
        let mut changed = fleet.clone();
        changed.accounts[0].browser = Some("new profile".into());
        for path in [store.state_file(), store.logins_file()] {
            let before = fs::read(&path).unwrap();
            fs::write(&path, "{\"schema\":999}").unwrap();
            assert!(
                persist_accounts_sync(
                    &store,
                    &roster_path,
                    (&changed, OptIn::Named),
                    ("m8", None),
                    &empty_walls,
                    now,
                    (&[], &[]),
                )
                .is_err()
            );
            assert_eq!(fs::read(&target).unwrap(), before_roster);
            assert_eq!(fs::read(&saved.fleet_path).unwrap(), before_fleet);
            assert_eq!(fs::read(&path).unwrap(), b"{\"schema\":999}");
            fs::write(&path, before).unwrap();
        }
        let mut duplicated_fleet = fleet.clone();
        duplicated_fleet.accounts.push(fleet.accounts[0].clone());
        assert!(
            persist_accounts_sync(
                &store,
                &roster_path,
                (&duplicated_fleet, OptIn::Named),
                ("m8", None),
                &empty_walls,
                now,
                (&[], &[]),
            )
            .is_err()
        );
        assert_eq!(fs::read(&target).unwrap(), before_roster);
        assert_eq!(fs::read(&saved.fleet_path).unwrap(), before_fleet);

        fs::remove_file(&saved.fleet_path).unwrap();
        fs::create_dir(&saved.fleet_path).unwrap();
        assert!(
            persist_accounts_sync(
                &store,
                &roster_path,
                (&changed, OptIn::Named),
                ("m8", None),
                &empty_walls,
                now,
                (&[], &[]),
            )
            .is_err()
        );
        assert_eq!(
            fs::read(&target).unwrap(),
            before_roster,
            "an auxiliary publication failure preserves the admitted roster"
        );
        assert!(saved.fleet_path.is_dir());
        fs::remove_dir(&saved.fleet_path).unwrap();
        persist_accounts_sync(
            &store,
            &roster_path,
            (&fleet, OptIn::Named),
            ("m8", None),
            &empty_walls,
            now,
            (&[], &[]),
        )
        .unwrap();

        let reset = Roster::load(&roster_path).unwrap();

        let source = root.join("source.csv");
        fs::write(&source, csv).unwrap();
        fs::remove_file(store.wake_path()).unwrap();
        fs::create_dir(store.wake_path()).unwrap();
        let env = Env {
            state: root.join("state"),
            aterm_state: Some(root.join("aterm-state")),
            cwd: root.to_path_buf(),
            home: None,
            claude_dir: None,
            claude_config_unusable: None,
            claude_profile: None,
            claude_custom_oauth: false,
            claude_session: None,
            claude_pid: None,
            codex_thread: None,
            agy_conversation: None,
            codex_home: None,
            codex_login: None,
            now,
            tz_s: 0,
            sid: String::new(),
            sock: None,
            config: Some(config.join("aterm.toml")),
            claude_tmp: None,
            claude_running: None,
        };
        // A sync that finds what is already there writes nothing, the wake marker included.
        let (mut out, mut err) = (Vec::new(), Vec::new());
        assert_eq!(
            run_accounts_sync(&env, source.to_str(), Some("m8"), false, &mut out, &mut err),
            ExitCode::SUCCESS,
            "{}",
            String::from_utf8_lossy(&err)
        );
        // One that writes a file and cannot write the marker says so.
        fs::remove_file(&saved.fleet_path).unwrap();
        let mut out = Vec::new();
        let mut err = Vec::new();
        assert_eq!(
            run_accounts_sync(&env, source.to_str(), Some("m8"), false, &mut out, &mut err),
            ExitCode::FAILURE
        );
        assert!(out.is_empty());
        assert!(!err.is_empty());
        assert!(store.wake_path().is_dir());
        assert_eq!(Roster::load(&target).unwrap(), reset);
    }

    #[test]
    fn normalizes_machine_ids_from_hostnames() {
        assert_eq!(
            normalize_machine_id("m8-Macbook-Pro-Max.local"),
            Some("m8".to_owned())
        );
        assert_eq!(
            normalize_machine_id("MacbookAir-m27-M5-32GB-4TB.local"),
            Some("m27".to_owned())
        );
        assert_eq!(
            normalize_machine_id("MacbookAir-M27-M5-32GB-4TB"),
            Some("m27".to_owned())
        );
        assert_eq!(
            normalize_machine_id("MacbookAir-M8-32GB.local"),
            Some("m8".to_owned())
        );
        assert_eq!(
            normalize_machine_id("Andrew's MacBook Pro (m15)"),
            Some("m15".to_owned())
        );
        assert_eq!(normalize_machine_id("MacBook-Pro-M4-Max.local"), None);
        assert_eq!(normalize_machine_id("MacBook-Pro-m4-Max.local"), None);
        assert_eq!(normalize_machine_id("MacBook-Air-m3-32GB.local"), None);
        assert_eq!(normalize_machine_id("M109"), Some("m109".to_owned()));
        assert_eq!(normalize_machine_id("*m12"), None);
        assert_eq!(normalize_machine_id("macbook"), None);
        assert_eq!(
            email_machines_and_slot("sol-m27@example.test"),
            (vec!["m27".to_owned()], None)
        );
        assert_eq!(
            email_machines_and_slot("sun.m27.2@example.test"),
            (vec!["m27".to_owned()], Some(2))
        );
        assert_eq!(
            email_machines_and_slot("solar.m27.3@example.test"),
            (vec!["m27".to_owned()], Some(3))
        );
        assert_eq!(
            email_machines_and_slot("owner+m27.2@example.test"),
            (vec!["m27".to_owned()], Some(2))
        );
        assert_eq!(
            email_machines_and_slot("owner+m27@example.test"),
            (vec!["m27".to_owned()], None)
        );
        assert_eq!(
            email_machines_and_slot("sol-m27+2@example.test"),
            (vec!["m27".to_owned()], Some(2))
        );
        assert_eq!(
            email_machines_and_slot("sun.m27.2+claude@example.test"),
            (vec!["m27".to_owned()], Some(2)),
            "CONTROL: trailing service tag does not clobber base machine or slot"
        );
        assert_eq!(
            email_machines_and_slot("owner+claude@example.test"),
            (Vec::<String>::new(), None),
            "CONTROL: non-machine plus tag yields neither machine nor slot"
        );
    }

    #[test]
    fn this_macs_id_is_its_first_host_name_that_names_a_machine_and_its_record_last() {
        let names = |names: &[&str]| names.iter().map(|n| (*n).to_owned()).collect::<Vec<_>>();
        let past_the_match = std::iter::once_with(|| -> String {
            panic!("a host name named the machine, so no later one is asked")
        });
        assert_eq!(
            machine_id_among(
                names(&["MacBook-Pro-M4-Max.local\n", "m15-MacBook-M5-Max\n"])
                    .into_iter()
                    .chain(past_the_match),
                || panic!("a host name named the machine, so the record is not read"),
            ),
            Some("m15".to_owned())
        );
        assert_eq!(
            machine_id_among(names(&["MacBook-Pro-M4-Max.local"]), || Some(
                "m9".to_owned()
            )),
            Some("m9".to_owned())
        );
        assert_eq!(machine_id_among(names(&[]), || None), None);
    }

    #[test]
    fn a_sync_keeps_a_host_the_fleet_lists_and_else_takes_the_default_logins_row() {
        let account = |label: &str, email: &str| FleetAccount {
            label: label.to_owned(),
            machines: vec![label.to_owned()],
            class: Class::Dedicated,
            email: email.to_owned(),
            browser: None,
            vendor: Default::default(),
        };
        let fleet = FleetRoster {
            source: "test".to_owned(),
            accounts: vec![account("m27", "sol@x.test"), account("m28", "luna@x.test")],
        };
        let pick = |host: Option<&str>, default_email| {
            host_or_login_machine(host.map(str::to_owned), &fleet, default_email)
        };
        let m = |id: &str| Some(id.to_owned());
        assert_eq!(
            pick(Some("m30"), Some("sol@x.test")),
            m("m27"),
            "an unlisted host gives way"
        );
        assert_eq!(
            pick(Some("m28"), Some("sol@x.test")),
            m("m28"),
            "a listed host stays"
        );
        assert_eq!(pick(Some("m30"), None), m("m30"), "no login holds a row");
        assert_eq!(pick(Some("m30"), Some("nobody@x.test")), m("m30"));
        assert_eq!(pick(None, Some("sol@x.test")), m("m27"));
    }

    #[test]
    fn discovery_keeps_a_host_one_shown_email_admits_and_else_takes_an_email_hint() {
        let emails = |emails: &[&str]| emails.iter().map(|e| (*e).to_owned()).collect();
        let pick = |host: Option<&str>, shown: &[&str], default_email: Option<&str>| {
            discovery_machine(host.map(str::to_owned), &emails(shown), || {
                default_email.map(str::to_owned)
            })
        };
        let m = |id: &str| Some(id.to_owned());
        assert_eq!(
            pick(Some("m30"), &["sol-m27@x.test"], None),
            m("m27"),
            "no email admits m30"
        );
        assert_eq!(pick(Some("m27"), &["sol-m27@x.test"], None), m("m27"));
        assert_eq!(
            pick(Some("m30"), &["alice@x.test", "sol-m27@x.test"], None),
            m("m30"),
            "an email that names no machine admits the host"
        );
        assert_eq!(
            pick(Some("m27"), &["luna-m28@x.test", "sol-m27@x.test"], None),
            m("m27"),
            "an email that names the host admits it over the first email's hint"
        );
        assert_eq!(pick(None, &["alice@x.test"], None), None);
        assert_eq!(
            pick(Some("m30"), &["sol-m27@x.test"], Some("luna-m28@x.test")),
            m("m28"),
            "the default login's hint comes before the shown emails'"
        );
    }

    #[test]
    fn default_home_standing_wall_ignores_older_transcript_when_newer_answered() {
        let dir = aterm_tempfile::Builder::new()
            .prefix("aterm-standing-wall-")
            .tempdir()
            .expect("tempdir");
        let proj = dir.path().join("projects").join("ws");
        fs::create_dir_all(&proj).unwrap();
        let older = proj.join("older.jsonl");
        fs::write(
            &older,
            "{\"type\":\"assistant\",\"timestamp\":\"2026-10-19T06:00:00Z\",\"isApiErrorMessage\":true,\"error\":\"rate_limit\",\"message\":{\"model\":\"<synthetic>\",\"content\":[{\"type\":\"text\",\"text\":\"You've hit your weekly limit · resets Oct 23 at 8am\"}]},\"quotaLimits\":{\"status\":\"rejected\",\"resetsAt\":1792767600,\"rateLimitType\":\"seven_day\"}}\n",
        )
        .unwrap();
        let now = 1_792_400_000;
        let wall = find_default_home_standing_wall(dir.path(), now).expect("standing wall");
        assert_eq!(wall.until, Some(1_792_767_600));

        let newer = proj.join("newer.jsonl");
        fs::write(
            &newer,
            "{\"type\":\"assistant\",\"timestamp\":\"2026-10-20T07:00:00Z\",\"message\":{\"model\":\"claude-opus-5-5\",\"content\":[{\"type\":\"text\",\"text\":\"done\"}]}}\n",
        )
        .unwrap();
        assert_eq!(
            find_default_home_standing_wall(dir.path(), now),
            None,
            "a newer model turn in another transcript clears the older transcript's wall"
        );
    }

    #[test]
    fn transcript_walls_by_email_retains_prior_account_wall_after_in_session_login_switch() {
        let dir = aterm_tempfile::Builder::new()
            .prefix("aterm-walls-by-email-")
            .tempdir()
            .expect("tempdir");
        let proj = dir.path().join("projects").join("ws");
        fs::create_dir_all(&proj).unwrap();
        let transcript = proj.join("conv.jsonl");
        let lines = concat!(
            "{\"type\":\"attachment\",\"attachment\":{\"type\":\"session_context\",\"context\":{\"userEmail\":\"The user's email address is first@example.test. Use it only to identify the user.\"}}}\n",
            "{\"type\":\"assistant\",\"timestamp\":\"2026-10-20T06:50:53Z\",\"isApiErrorMessage\":true,\"error\":\"rate_limit\",\"message\":{\"model\":\"<synthetic>\",\"content\":[{\"type\":\"text\",\"text\":\"You've hit your weekly limit · resets Oct 23 at 8am\"}]},\"quotaLimits\":{\"status\":\"rejected\",\"resetsAt\":1792767600,\"rateLimitType\":\"seven_day\"}}\n",
            "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"<local-command-stdout>Login successful</local-command-stdout>\"}}\n",
            "{\"type\":\"attachment\",\"attachment\":{\"type\":\"session_context\",\"context\":{\"userEmail\":\"The user's email address is second@example.test. Use it only to identify the user.\"},\"changed\":true,\"reason\":\"account_change\"}}\n",
            "{\"type\":\"assistant\",\"timestamp\":\"2026-10-20T07:00:00Z\",\"message\":{\"model\":\"claude-opus-5-5\",\"content\":[{\"type\":\"text\",\"text\":\"done\"}]}}\n",
            "{\"type\":\"assistant\",\"timestamp\":\"2026-10-22T06:21:44Z\",\"isApiErrorMessage\":true,\"error\":\"rate_limit\",\"message\":{\"model\":\"<synthetic>\",\"content\":[{\"type\":\"text\",\"text\":\"You've hit your weekly limit · resets Oct 22 at 10pm\"}]},\"quotaLimits\":{\"status\":\"rejected\",\"resetsAt\":1792731600,\"rateLimitType\":\"seven_day\"}}\n"
        );
        fs::write(&transcript, lines).unwrap();
        let roster = Roster {
            rows: vec![
                Row {
                    label: "m15-2".into(),
                    dir: None,
                    email: Some("second@example.test".into()),
                    ..Row::default()
                },
                Row {
                    label: "m15".into(),
                    dir: Some("/tmp/m15".into()),
                    email: Some("first@example.test".into()),
                    ..Row::default()
                },
                Row {
                    label: "m15-3".into(),
                    dir: Some("/tmp/m15-3".into()),
                    email: Some("third@example.test".into()),
                    ..Row::default()
                },
            ],
        };
        let now = 1_792_660_000;
        let walls = find_transcript_walls_by_email(dir.path(), &roster, now);
        assert_eq!(
            walls.get("m15").and_then(|w| w.until),
            Some(1_792_767_600),
            "first account's weekly wall stands even after switching to second account"
        );
        assert_eq!(
            walls.get("m15-2").and_then(|w| w.until),
            Some(1_792_731_600),
            "second account's weekly wall is also recorded"
        );
        assert!(!walls.contains_key("m15-3"));
    }

    /// The 2026-10-27 displacement on m2, turned the other way: a person's `/login` in one tab
    /// makes `m2` (walled for the week) the default login in place of `m2-2`. Another tab goes on
    /// naming `m2-2` in its `session_context`, before the `/login` and again 40 minutes after it,
    /// while `m2`'s wall answers its requests. Neither of its walls is `m2-2`'s: one stands across
    /// the `/login` from the account it names, the other is booked after the newest `/login` to a
    /// row with no login on this machine. The `/login` tab's own wall is `m2`'s. Control: where
    /// `m2-2` holds a login in its own directory, a session on it is its own after the `/login`.
    #[test]
    fn a_stale_session_context_after_a_login_elsewhere_books_no_wall_to_the_displaced_row() {
        let dir = aterm_tempfile::Builder::new()
            .prefix("aterm-walls-displaced-")
            .tempdir()
            .expect("tempdir");
        let proj = dir.path().join("projects").join("ws");
        fs::create_dir_all(&proj).unwrap();
        let context = |at: &str, who: &str| {
            format!(
                "{{\"type\":\"attachment\",\"timestamp\":\"{at}\",\"attachment\":{{\"type\":\"session_context\",\"context\":{{\"userEmail\":\"The user's email address is {who}@example.test. Use it only to identify the user.\"}}}}}}\n"
            )
        };
        let wall = |at: &str| {
            format!(
                "{{\"type\":\"assistant\",\"timestamp\":\"{at}\",\"isApiErrorMessage\":true,\"error\":\"rate_limit\",\"message\":{{\"model\":\"<synthetic>\",\"content\":[{{\"type\":\"text\",\"text\":\"You've hit your weekly limit\"}}]}},\"quotaLimits\":{{\"status\":\"rejected\",\"resetsAt\":1793246400,\"rateLimitType\":\"seven_day\"}}}}\n"
            )
        };
        let answer = |at: &str| {
            format!(
                "{{\"type\":\"assistant\",\"timestamp\":\"{at}\",\"message\":{{\"model\":\"claude-opus-5-5\",\"content\":[{{\"type\":\"text\",\"text\":\"done\"}}]}}}}\n"
            )
        };
        let login = "{\"type\":\"user\",\"timestamp\":\"2026-10-27T16:02:57Z\",\"message\":{\"role\":\"user\",\"content\":\"<local-command-stdout>Login successful</local-command-stdout>\"}}\n";
        let untimed_context = |who: &str| {
            format!(
                "{{\"type\":\"attachment\",\"attachment\":{{\"type\":\"session_context\",\"context\":{{\"userEmail\":\"The user's email address is {who}@example.test. Use it only to identify the user.\"}}}}}}\n"
            )
        };
        fs::write(
            proj.join("login.jsonl"),
            [
                context("2026-10-27T10:00:00Z", "m2-2"),
                login.to_string(),
                untimed_context("m2"),
                wall("2026-10-27T16:05:00Z"),
            ]
            .concat(),
        )
        .unwrap();
        fs::write(
            proj.join("other.jsonl"),
            [
                context("2026-10-27T09:00:00Z", "m2-2"),
                answer("2026-10-27T15:00:00Z"),
                wall("2026-10-27T16:10:00Z"),
                context("2026-10-27T16:42:44Z", "m2-2"),
                wall("2026-10-27T16:50:00Z"),
            ]
            .concat(),
        )
        .unwrap();
        let own = dir.path().join("logins").join("m2-2");
        let roster = Roster {
            rows: vec![
                Row {
                    label: "m2".into(),
                    dir: None,
                    email: Some("m2@example.test".into()),
                    ..Row::default()
                },
                Row {
                    label: "m2-2".into(),
                    dir: Some(own.to_string_lossy().into_owned()),
                    email: Some("m2-2@example.test".into()),
                    ..Row::default()
                },
            ],
        };
        let now = 1_793_120_400;
        let walls = find_transcript_walls_by_email(dir.path(), &roster, now);
        assert_eq!(
            walls.get("m2").map(|w| w.at),
            Some(1_793_117_100),
            "the /login tab's wall is the new default login's"
        );
        assert_eq!(walls.get("m2-2"), None, "{walls:?}");

        fs::create_dir_all(&own).unwrap();
        fs::write(own.join(".credentials.json"), "{}").unwrap();
        let walls = find_transcript_walls_by_email(dir.path(), &roster, now);
        assert_eq!(
            walls.get("m2-2").map(|w| w.at),
            Some(1_793_119_800),
            "control: a row with its own login keeps the wall its session names after the /login"
        );
        assert_eq!(walls.get("m2").map(|w| w.at), Some(1_793_117_100));
    }

    #[test]
    fn a_refused_renewal_worker_releases_admission_without_running_it() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::{Arc, Mutex};

        let state = Arc::new(Mutex::new(AutoRenewGuard::default()));
        let now = Instant::now();
        let ran = Arc::new(AtomicBool::new(false));
        let called = Arc::clone(&ran);
        let permit = AutoRenewPermit::claim(&state, "first", now, false).expect("admitted");
        let result = start_auto_renew(
            permit,
            move || called.store(true, Ordering::SeqCst),
            |_work| Err(io::Error::other("injected spawn refusal")),
        );
        assert!(result.is_err());
        assert!(!ran.load(Ordering::SeqCst), "a refused spawn ran nothing");
        assert!(!state.lock().unwrap().in_flight, "refusal releases busy");
        assert!(
            AutoRenewPermit::claim(&state, "first", now, false).is_none(),
            "the per-label cooldown still prevents repeated background attempts"
        );
        let mtime_v1 = std::time::UNIX_EPOCH + Duration::from_secs(1_000);
        let mtime_permit =
            AutoRenewPermit::claim_with_mailbox(&state, "first", now, false, Some(mtime_v1))
                .expect("a new or updated mailbox config mtime clears the retry cooldown");
        drop(mtime_permit);
        assert!(
            AutoRenewPermit::claim_with_mailbox(&state, "first", now, false, Some(mtime_v1))
                .is_none(),
            "same mailbox mtime respects cooldown"
        );
        let manual_permit = AutoRenewPermit::claim(&state, "first", now, true)
            .expect("explicit operator selection bypasses the background cooldown when idle");
        assert!(
            AutoRenewPermit::claim(&state, "first", now, true).is_none(),
            "in_flight still serializes concurrent explicit selections"
        );
        drop(manual_permit);

        let permit = AutoRenewPermit::claim(&state, "second", now, false)
            .expect("spawn refusal cannot block another account");
        let called = Arc::clone(&ran);
        let mut queued = None;
        start_auto_renew(
            permit,
            move || called.store(true, Ordering::SeqCst),
            |work| {
                queued = Some(work);
                Ok(())
            },
        )
        .expect("accepted by injected builder");
        assert!(!ran.load(Ordering::SeqCst), "still queued");
        assert!(state.lock().unwrap().in_flight);
        assert!(AutoRenewPermit::claim(&state, "third", now, false).is_none());
        queued.take().expect("queued worker")();
        assert!(ran.load(Ordering::SeqCst), "accepted worker ran");
        assert!(!state.lock().unwrap().in_flight, "completion releases busy");
        let later = now + Duration::from_secs(900);
        let permit = AutoRenewPermit::claim(&state, "first", later, false)
            .expect("the original account can retry after its cooldown");
        drop(permit);
        assert!(!state.lock().unwrap().in_flight);
    }

    /// A background renewal that stopped for a person (a block page, a page that waits on
    /// one) parks its row: no background renewal of it starts again past the 15-minute cooldown
    /// until [`PERSON_BACKOFF`] has passed, and a renewal by hand still may. Any other end
    /// unparks it. CONTROL: a renewal that failed otherwise is tried again after the cooldown.
    #[test]
    fn a_background_renewal_that_stopped_for_a_person_is_left_alone_for_its_backoff() {
        use super::super::cli::Renewal;
        use std::sync::{Arc, Mutex};
        let state = Arc::new(Mutex::new(AutoRenewGuard::default()));
        let now = Instant::now();
        let after_cooldown = now + Duration::from_secs(901);
        drop(AutoRenewPermit::claim(&state, "grok:grok-1", now, false).expect("admitted"));
        note_background_end(&state, "grok:grok-1", Renewal::NeedsPerson, now);
        assert!(AutoRenewPermit::claim(&state, "grok:grok-1", after_cooldown, false).is_none());
        let by_hand = AutoRenewPermit::claim(&state, "grok:grok-1", after_cooldown, true);
        assert!(by_hand.is_some(), "a renewal by hand is no background one");
        drop(by_hand);
        let past = now + PERSON_BACKOFF;
        drop(AutoRenewPermit::claim(&state, "grok:grok-1", past, false).expect("backed off"));
        note_background_end(&state, "grok:grok-1", Renewal::Renewed, past);
        assert!(state.lock().unwrap().parked.is_empty(), "unparked");

        drop(AutoRenewPermit::claim(&state, "kimi:kimi-1", now, false).expect("admitted"));
        note_background_end(&state, "kimi:kimi-1", Renewal::Failed, now);
        assert!(
            AutoRenewPermit::claim(&state, "kimi:kimi-1", after_cooldown, false).is_some(),
            "CONTROL: another failure waits out the cooldown alone"
        );
    }

    /// A background renewal that met Cloudflare's bot check holds its row as long as one that
    /// stopped for a person, though nobody is asked (design R-13): not past the 15-minute
    /// cooldown, nor a minute before [`PERSON_BACKOFF`], and again once that has passed; the
    /// row's activity says when. A renewal by hand is admitted through it, and the row's next end
    /// decides: a plain failure lifts it. CONTROL: that failure waits out the cooldown alone.
    #[test]
    fn a_background_renewal_that_met_a_bot_check_is_left_alone_for_the_person_backoff() {
        use super::super::cli::Renewal;
        use std::sync::{Arc, Mutex};
        let state = Arc::new(Mutex::new(AutoRenewGuard::default()));
        let store = accounts::Store::at(Path::new("/synthetic/bot-check"));
        let now = Instant::now();
        let key = "kimi:kimi-1";
        let permit = AutoRenewPermit::claim(&state, key, now, false).expect("admitted");
        permit.attribute(&store, Cli::Kimi, "kimi-1", 1_000);
        drop(permit);
        note_background_end(&state, key, Renewal::BotCheck, now);
        let cooled = now + Duration::from_secs(901);
        for later in [cooled, now + PERSON_BACKOFF - Duration::from_secs(60)] {
            assert!(
                AutoRenewPermit::claim(&state, key, later, false).is_none(),
                "{:?} later",
                later - now
            );
        }
        for ago in [60, 901] {
            let activity = login_activity_of(
                &state.lock().unwrap(),
                &store,
                Cli::Kimi,
                1_000,
                now + Duration::from_secs(ago),
            );
            assert_eq!(
                activity.first().and_then(|row| row.next_at),
                Some(1_000 + PERSON_BACKOFF_SECS - i64::try_from(ago).unwrap()),
                "{ago} s later, inside the cooldown too: {activity:?}"
            );
        }
        let by_hand = AutoRenewPermit::claim(&state, key, cooled, true);
        assert!(by_hand.is_some(), "a renewal by hand is no background one");
        drop(by_hand);
        let past = now + PERSON_BACKOFF;
        drop(AutoRenewPermit::claim(&state, key, past, false).expect("backed off"));
        note_background_end(&state, key, Renewal::Failed, past);
        assert!(state.lock().unwrap().bot_checked.is_empty(), "lifted");
        assert!(
            AutoRenewPermit::claim(&state, key, past + Duration::from_secs(901), false).is_some(),
            "CONTROL: a plain failure waits out the cooldown alone"
        );
    }

    #[test]
    fn weekly_limit_rollover_selects_and_renews_next_account_via_mailbox_and_rotates_session() {
        let dir = aterm_tempfile::Builder::new()
            .prefix("aterm-rollover-mailbox-")
            .tempdir()
            .expect("tempdir");
        let root = dir.path();
        let home = root.join("home");
        let state_dir = root.join("state");
        let config_dir = root.join("config");
        let claude_dir = home.join(".claude");
        let projects_dir = claude_dir.join("projects").join("ws");
        let store = accounts::Store::at(&state_dir);
        let mailbox_dir = store.dir.join(super::super::open_url::DEFAULT_MAILBOX_DIR);
        fs::create_dir_all(&projects_dir).unwrap();
        fs::create_dir_all(&config_dir).unwrap();
        fs::create_dir_all(&mailbox_dir).unwrap();

        let csv_path = root.join("fleet.csv");
        fs::write(
            &csv_path,
            concat!(
                "email,computer,claude\n",
                "m15@example.test,m15,1\n",
                "m15-2@example.test,m15,1\n",
                "m15-3@example.test,m15,1\n"
            ),
        )
        .unwrap();

        let claude_profile = home.join(".claude.json");
        fs::write(
            &claude_profile,
            "{\"oauthAccount\":{\"emailAddress\":\"m15-2@example.test\"}}\n",
        )
        .unwrap();

        let transcript = projects_dir.join("session.jsonl");
        fs::write(
            &transcript,
            concat!(
                "{\"type\":\"attachment\",\"attachment\":{\"type\":\"session_context\",\"context\":{\"userEmail\":\"The user's email address is m15@example.test. Use it only to identify the user.\"}}}\n",
                "{\"type\":\"assistant\",\"timestamp\":\"2026-10-20T06:50:53Z\",\"isApiErrorMessage\":true,\"error\":\"rate_limit\",\"message\":{\"model\":\"<synthetic>\",\"content\":[{\"type\":\"text\",\"text\":\"You've hit your weekly limit · resets Oct 26 at 10am\"}]},\"quotaLimits\":{\"status\":\"rejected\",\"resetsAt\":1792999200,\"rateLimitType\":\"seven_day\"}}\n",
                "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"<local-command-stdout>Login successful</local-command-stdout>\"}}\n",
                "{\"type\":\"attachment\",\"attachment\":{\"type\":\"session_context\",\"context\":{\"userEmail\":\"The user's email address is m15-2@example.test. Use it only to identify the user.\"},\"changed\":true,\"reason\":\"account_change\"}}\n",
                "{\"type\":\"assistant\",\"timestamp\":\"2026-10-22T06:21:44Z\",\"isApiErrorMessage\":true,\"error\":\"rate_limit\",\"message\":{\"model\":\"<synthetic>\",\"content\":[{\"type\":\"text\",\"text\":\"You've hit your weekly limit · resets Oct 25 at 2pm\"}]},\"quotaLimits\":{\"status\":\"rejected\",\"resetsAt\":1792936800,\"rateLimitType\":\"seven_day\"}}\n"
            ),
        )
        .unwrap();

        let now = 1_792_660_000;
        let aterm_toml = config_dir.join("aterm.toml");
        let env = Env {
            state: state_dir,
            aterm_state: Some(root.join("aterm-state")),
            cwd: root.to_path_buf(),
            home: Some(home),
            claude_dir: Some(claude_dir),
            claude_config_unusable: None,
            claude_profile: Some(claude_profile),
            claude_custom_oauth: false,
            claude_session: None,
            claude_pid: None,
            codex_thread: None,
            agy_conversation: None,
            codex_home: None,
            codex_login: None,
            now,
            tz_s: 0,
            sid: String::new(),
            sock: None,
            config: Some(aterm_toml),
            claude_tmp: None,
            claude_running: None,
        };

        let mut sync_out = Vec::new();
        let mut sync_err = Vec::new();
        assert_eq!(
            run_accounts_sync(
                &env,
                csv_path.to_str(),
                Some("m15"),
                false,
                &mut sync_out,
                &mut sync_err
            ),
            ExitCode::SUCCESS,
            "sync failed: {}",
            String::from_utf8_lossy(&sync_err)
        );

        let roster_path = config_dir.join("accounts.toml");
        let file_roster = Roster::load(&roster_path).expect("accounts.toml written");
        let (logins_before, _) = store.load_logins(now).expect("logins loaded");

        let who_m15_2 = accounts::ProcessLogin {
            login: None,
            default_home: true,
            stored_login: true,
        };
        let standing_wall = super::super::upgrade::StandingWall {
            at: u64::try_from(now).unwrap(),
            until: Some(1_792_936_800),
            window: Some("seven_day".to_owned()),
            credits: None,
        };
        assert!(matches!(
            accounts::record_wall(
                &store,
                &store.read_usage_roster(Some(&roster_path), now).unwrap(),
                "sid-m15-live",
                &who_m15_2,
                &standing_wall,
                now - 60,
                now,
            ),
            accounts::Recorded::Wall { .. }
        ));

        let (state_walled, _) = store.load(now).unwrap();
        let next_row = next_auto_renew_target(&file_roster, &state_walled, &logins_before, now)
            .expect("must select m15-3 for auto-renewal");
        assert_eq!(next_row.label, "m15-3");
        assert_eq!(next_row.email.as_deref(), Some("m15-3@example.test"));

        let mut session_only_state = state_walled;
        session_only_state.active = None;
        assert_eq!(
            next_auto_renew_target(&file_roster, &session_only_state, &logins_before, now)
                .map(|r| r.label),
            Some("m15-3".to_owned()),
            "must select m15-3 even when state.active is None and only state.sessions names m15-2"
        );
        assert_eq!(
            select_auto_renew_browser(None, None, Some(PathBuf::from("/bin/aterm-open-url"))),
            None,
            "without a mailbox or profile, background auto-renew must never fall back to Default browser"
        );
        assert_eq!(
            select_auto_renew_browser(
                Some("Claude m15-3"),
                Some(&mailbox_dir),
                Some(PathBuf::from("/bin/aterm-open-url"))
            ),
            Some(super::super::cli::RenewBrowser::Remote {
                opener: Some(PathBuf::from("/bin/aterm-open-url")),
            }),
            "when a mailbox directory is present, headless Remote opener is preferred over GUI profile"
        );

        let staging = store
            .claim_staging_dir(&next_row.label)
            .expect("claim staging dir");
        let staging_text = staging.to_str().unwrap().to_owned();

        fs::write(
            mailbox_dir.join("00_decoy_spoof.eml"),
            concat!(
                "From: Claude <no-reply@claude.ai>\r\n",
                "To: <not-m15-3@example.test>\r\n\r\n",
                "https://claude.ai/magic?token=spoofed_wrong_account\r\n"
            ),
        )
        .unwrap();
        fs::write(
            mailbox_dir.join("01_magic_m15_3.eml"),
            concat!(
                "From: Claude <no-reply@claude.ai>\r\n",
                "To: Claude Fleet\r\n",
                "  <m15-3@example.test>\r\n",
                "Content-Transfer-Encoding: quoted-printable\r\n\r\n",
                "<html><body>\r\n",
                "<a href=3D\"https://claude.ai/unsubscribe\">Unsubscribe</a>\r\n",
                "<a href=3D\"https://claude.ai/magic?token=3Dtok_m15_3&amp;=\r\n",
                "hint=3Dm15-3\">Sign in to Claude.ai</a>\r\n",
                "</body></html>\r\n"
            ),
        )
        .unwrap();

        let authorize_url = "https://claude.ai/oauth/authorize?redirect_uri=http%3A%2F%2F127.0.0.1%3A9123%2Fcallback&state=st_m15_3&login_hint=m15-3%40example.test";
        let responses = [
            "HTTP/1.1 200 OK\r\nSet-Cookie: session=sess_m15_3; Secure; Path=/\r\n\r\nCheck your email",
            concat!(
                "HTTP/1.1 200 OK\r\n\r\n",
                "<form method=\"POST\" action=\"/oauth/approve\">",
                "<input type=\"hidden\" name=\"approve\" value=\"allow\"></form>"
            ),
            "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:9123/callback?code=code_m15_3&state=st_m15_3\r\n\r\n",
            "HTTP/1.1 200 OK\r\n\r\nLogin successful",
        ];
        let mut resp_iter = responses.iter();
        let mut seen_urls = Vec::new();
        super::super::open_url::authorize_using(
            authorize_url,
            next_row.email.as_deref(),
            |email| {
                super::super::open_url::poll_mailbox_for_magic_link(
                    &mailbox_dir,
                    email,
                    Duration::from_secs(60),
                )
            },
            &mut |_method, url, cookies, _body| {
                seen_urls.push(url.to_owned());
                super::super::open_url::parse_raw_http_response(
                    resp_iter.next().expect("expected request"),
                    cookies,
                )
                .ok_or("malformed response")
            },
        )
        .expect("OAuth + mailbox magic-link authorization must succeed");
        assert_eq!(
            seen_urls,
            vec![
                authorize_url.to_owned(),
                "https://claude.ai/magic?token=tok_m15_3&hint=m15-3".to_owned(),
                "https://claude.ai/oauth/approve".to_owned(),
                "http://127.0.0.1:9123/callback?code=code_m15_3&state=st_m15_3".to_owned(),
            ]
        );

        fs::write(
            staging.join(".claude.json"),
            "{\"oauthAccount\":{\"emailAddress\":\"m15-3@example.test\",\"subscriptionType\":\"max\"}}\n",
        )
        .unwrap();
        let status_got = super::super::align::Captured {
            stdout: b"{\"loggedIn\":true,\"authMethod\":\"claude.ai\",\"email\":\"m15-3@example.test\",\"subscriptionType\":\"max\"}\n".to_vec(),
            code: Some(0),
        };
        assert!(
            complete_staging_renewal(
                &store,
                &file_roster,
                &next_row.label,
                &staging,
                &staging_text,
                &status_got,
                now + 5,
            ),
            "complete_staging_renewal must bind m15-3"
        );
        assert!(
            store.wake_path().exists(),
            "complete_staging_renewal must touch wake so waiting sessions rotate immediately"
        );

        let (state_after_renew, _) = store.load(now + 5).unwrap();
        let (logins_after_renew, _) = store.load_logins(now + 5).unwrap();
        let bound_roster = accounts::bound(&file_roster, &logins_after_renew);
        let enrolled_after = accounts::enrolled(&bound_roster);
        let wall_point = accounts::Point::Wall {
            window: "seven_day".to_owned(),
            at: Some(1_792_936_800),
        };
        let me_after = who_m15_2.me(&bound_roster);
        assert_eq!(
            accounts::advise(
                &bound_roster,
                &state_after_renew,
                &enrolled_after,
                &me_after,
                &wall_point,
                accounts::Mode::Wall,
                now + 5,
            ),
            accounts::Advice::Move {
                to: "m15-3".to_owned(),
                ahead: false,
                rotates: true,
                takes_canary: false,
            }
        );

        let expect_active = state_after_renew.active.clone();
        let mut note = None;
        store
            .transact(now + 5, &mut note, |st| {
                let ev = accounts::begin_rotation(
                    st,
                    &bound_roster,
                    &enrolled_after,
                    expect_active.as_deref(),
                    accounts::Mover {
                        sid: "sid-m15-live",
                        account: "m15-2",
                    },
                    "m15-3",
                    now + 5,
                )
                .map_err(|lost| format!("rotation lost: {lost:?}"))?;
                Ok(vec![ev])
            })
            .expect("begin_rotation transaction");
        let (state_rotated, _) = store.load(now + 5).unwrap();
        assert_eq!(state_rotated.active.as_deref(), Some("m15-3"));
        let target_row = bound_roster.row("m15-3").expect("m15-3 row");
        assert_eq!(
            accounts::login_env(target_row),
            (accounts::LOGIN_ENV, staging_text)
        );
    }

    struct World {
        _dir: aterm_tempfile::TempDir,
        home: PathBuf,
        state: PathBuf,
        config: PathBuf,
    }

    impl World {
        fn new() -> Self {
            let dir = aterm_tempfile::Builder::new()
                .prefix("aterm-fleet-upkeep-")
                .tempdir()
                .expect("tempdir");
            let root = dir.path().to_path_buf();
            let home = root.join("home");
            fs::create_dir_all(&home).unwrap();
            Self {
                _dir: dir,
                home,
                state: root.join("state"),
                config: root.join("config"),
            }
        }

        fn aterm_toml(&self) -> PathBuf {
            self.config.join("aterm.toml")
        }

        fn roster(&self) -> PathBuf {
            self.config.join("accounts.toml")
        }

        fn fleet_file(&self) -> PathBuf {
            self.config.join(FLEET_ROSTER_FILE)
        }

        fn write_config(&self, text: &str) {
            fs::create_dir_all(&self.config).unwrap();
            fs::write(self.aterm_toml(), text).unwrap();
        }

        fn env(&self, now: i64) -> Env {
            Env {
                state: self.state.clone(),
                aterm_state: None,
                cwd: self.home.clone(),
                home: Some(self.home.clone()),
                claude_dir: Some(self.home.join(".claude")),
                claude_config_unusable: None,
                claude_profile: Some(self.home.join(".claude.json")),
                claude_custom_oauth: false,
                claude_session: None,
                claude_pid: None,
                codex_thread: None,
                agy_conversation: None,
                codex_home: None,
                codex_login: None,
                now,
                tz_s: 0,
                sid: String::new(),
                sock: None,
                config: Some(self.aterm_toml()),
                claude_tmp: None,
                claude_running: None,
            }
        }
    }

    struct Witness {
        fetched: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
        spawned: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        fetch: Box<Fetch<'static>>,
        spawn: Box<SpawnRenewal>,
        cooldown: std::sync::Mutex<Option<Instant>>,
        rediscovery: std::sync::Mutex<Rediscovery>,
        guard: std::sync::Arc<std::sync::Mutex<AutoRenewGuard>>,
        /// Whether a headless browser is installed: none unless a test says so.
        browser: Box<dyn Fn(&Path) -> bool + Sync>,
        /// The vault's sync: this machine is in none unless a test says so.
        vault_sync: fn(&Path) -> super::super::sign_in_ladder::Vault,
        /// The automatic sign-ins' lines, as the window's `aterm.log` would have them.
        logged: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
        /// One run of a route: none unless a test scripts one.
        run_attempt: Option<RunAttempt>,
    }

    /// A spawn seam that runs its work at once, on the caller's thread.
    fn run_inline(work: Box<dyn FnOnce() + Send>) -> io::Result<()> {
        work();
        Ok(())
    }

    impl Witness {
        fn answering(text: &'static str) -> Self {
            let fetched: std::sync::Arc<std::sync::Mutex<Vec<String>>> = std::sync::Arc::default();
            let spawned: std::sync::Arc<std::sync::atomic::AtomicUsize> = std::sync::Arc::default();
            let into = std::sync::Arc::clone(&fetched);
            let count = std::sync::Arc::clone(&spawned);
            Self {
                fetch: Box::new(move |_: &Env, source: &str| {
                    into.lock().unwrap().push(source.to_owned());
                    Ok(text.to_owned())
                }),
                spawn: Box::new(move |_work| {
                    count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Ok(())
                }),
                fetched,
                spawned,
                cooldown: std::sync::Mutex::new(None),
                rediscovery: std::sync::Mutex::new(Rediscovery::default()),
                guard: std::sync::Arc::new(std::sync::Mutex::new(AutoRenewGuard::default())),
                browser: Box::new(|_: &Path| false),
                vault_sync: |_: &Path| super::super::sign_in_ladder::Vault::NotMember,
                logged: std::sync::Arc::default(),
                run_attempt: None,
            }
        }

        fn fx(&self) -> UpkeepEffects<'_> {
            let logged = std::sync::Arc::clone(&self.logged);
            UpkeepEffects {
                fetch: &*self.fetch,
                spawn_renewal: &*self.spawn,
                quota_refresh: &|_, _, _, _| {},
                sync_cooldown: &self.cooldown,
                rediscovery: &self.rediscovery,
                renew_guard: &self.guard,
                find_browser: &*self.browser,
                vault_sync: self.vault_sync,
                spawn_vault: &run_inline,
                default_check: |_: &Path| {
                    crate::harness::cli::LoginCheck::CouldNotRun(String::new())
                },
                mailbox: &ladder_mailbox_files,
                log: std::sync::Arc::new(move |line: &str| {
                    logged.lock().unwrap().push(line.to_owned());
                }),
                run_attempt: self.run_attempt.clone(),
                shared: shared_renewal::Effects::default(),
            }
        }

        fn logged(&self) -> Vec<String> {
            self.logged.lock().unwrap().clone()
        }

        fn fetched(&self) -> Vec<String> {
            self.fetched.lock().unwrap().clone()
        }

        fn spawned(&self) -> usize {
            self.spawned.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    const LOOPBACK_SOURCE: &str = "http://127.0.0.1:9/fleet.csv";
    const ONE_ACCOUNT_CSV: &str = "email,computer,claude\nm1@example.test,m1,1\n";
    const UPKEEP_NOW: i64 = 1_792_660_000;
    const UNCONFIGURED: Upkept = Upkept::Unconfigured {
        fleet_file_ignored: false,
        renewal: false,
    };

    #[test]
    fn a_fresh_kimi_quota_observation_starts_no_refresh_or_claim() {
        let tmp = aterm_tempfile::Builder::new()
            .prefix("aterm-kimi-quota-fresh-")
            .tempdir()
            .unwrap();
        let home = tmp.path().join("home");
        let state_dir = tmp.path().join("state");
        let roster = tmp.path().join("accounts-kimi.toml");
        fs::write(
            &roster,
            accounts::append_row("", "kimi-m22", Class::Dedicated, None).unwrap(),
        )
        .unwrap();
        let store = accounts::Store::at_for(&state_dir, Cli::Kimi, &home);
        store
            .transact(UPKEEP_NOW, &mut None, |state| {
                let mut events = accounts::note_sample(
                    state,
                    "kimi-m22",
                    accounts::Sample {
                        session_pct: Some(36),
                        session_reset: Some(UPKEEP_NOW + 1500),
                        at: UPKEEP_NOW,
                        ..accounts::Sample::default()
                    },
                    UPKEEP_NOW,
                );
                events.extend(accounts::note_panel_window(
                    state,
                    "kimi-m22",
                    "monthly",
                    13,
                    Some(UPKEEP_NOW + 2_000_000),
                    UPKEEP_NOW,
                ));
                Ok(events)
            })
            .unwrap();
        assert_eq!(
            kimi_quota_label(&home, &state_dir, &roster, UPKEEP_NOW + 60),
            None
        );
        assert_eq!(
            kimi_quota_label(
                &home,
                &state_dir,
                &roster,
                UPKEEP_NOW + super::super::accounts_pool::USAGE_FRESH_S
            ),
            Some("kimi-m22".to_owned()),
            "CONTROL: a stale monthly window is due"
        );
    }

    #[test]
    fn an_unconfigured_upkeep_fetches_nothing_and_starts_nothing() {
        let w = World::new();
        let wit = Witness::answering(ONE_ACCOUNT_CSV);
        let upkept = |wit: &Witness| {
            upkeep(
                &w.home,
                &w.state,
                (&w.roster(), UPKEEP_NOW, UpkeepStep::Both),
                &wit.fx(),
            )
        };
        assert_eq!(upkept(&wit), UNCONFIGURED, "a fresh home");
        assert!(!w.config.exists(), "nothing made in a fresh home");
        for text in [
            "[fleet]\nmachine = \"m1\"\n",
            "[fleet]\naccounts_source = \"  \"\nmachine = \"m1\"\n",
            "accounts_source = \"http://127.0.0.1:9/fleet.csv\"\n[fleet]\nmachine = \"m1\"\n",
            "[fleet\naccounts_source = \"http://127.0.0.1:9/fleet.csv\"\n",
            "[fleet]\naccounts_source = 7\nmachine = \"m1\"\n",
        ] {
            w.write_config(text);
            assert_eq!(upkept(&wit), UNCONFIGURED, "{text}");
        }
        assert_eq!(wit.fetched(), Vec::<String>::new(), "nothing fetched");
        assert_eq!(wit.spawned(), 0, "no renewal started");
        assert!(!w.roster().exists() && !w.fleet_file().exists());
        assert!(!w.state.exists(), "the accounts store is never made");

        w.write_config(&format!(
            "[fleet]\naccounts_source = \"{LOOPBACK_SOURCE}\"\nmachine = \"m1\"\n"
        ));
        assert_eq!(
            upkept(&wit),
            Upkept::Configured {
                kind: "key",
                roster: true,
                renewal: false
            }
        );
        assert_eq!(wit.fetched(), vec![LOOPBACK_SOURCE.to_owned()]);
        let roster = fs::read_to_string(w.roster()).expect("the roster the source answered");
        assert!(roster.contains("m1@example.test"), "{roster}");
        assert_eq!(fleet_file_opt_in(&w.fleet_file()), Some(OptIn::Key));
    }

    #[test]
    fn a_sync_handed_no_source_reads_only_a_configured_one() {
        let w = World::new();
        w.write_config("[fleet]\nmachine = \"m1\"\n");
        let wit = Witness::answering(ONE_ACCOUNT_CSV);
        let sync = |w: &World| {
            let (mut out, mut err) = (Vec::new(), Vec::new());
            let how = SyncHow {
                fetch: &*wit.fetch,
                ladder: &[],
                fleet_snapshots: &[],
                heal: None,
                allow_empty_pool: false,
            };
            let code = run_accounts_sync_with(
                &w.env(UPKEEP_NOW),
                (None, None, false),
                (&mut out, &mut err),
                &how,
            );
            (code, String::from_utf8_lossy(&err).into_owned())
        };
        let (code, err) = sync(&w);
        assert_eq!(code, ExitCode::FAILURE);
        assert!(err.contains(NO_SOURCE), "{err}");
        assert_eq!(wit.fetched(), Vec::<String>::new());
        assert!(!w.roster().exists());

        w.write_config(&format!(
            "[fleet]\naccounts_source = \"{LOOPBACK_SOURCE}\"\nmachine = \"m1\"\n"
        ));
        let (code, err) = sync(&w);
        assert_eq!(code, ExitCode::SUCCESS, "{err}");
        assert_eq!(wit.fetched(), vec![LOOPBACK_SOURCE.to_owned()]);
        assert_eq!(fleet_file_opt_in(&w.fleet_file()), Some(OptIn::Key));
        assert_eq!(
            configured_source(Some(&w.aterm_toml()), Some(&w.roster())),
            Some(FleetSource::Key(LOOPBACK_SOURCE.to_owned()))
        );
        w.write_config("[fleet]\nmachine = \"m1\"\n");
        assert_eq!(
            configured_source(Some(&w.aterm_toml()), Some(&w.roster())),
            None
        );
    }

    #[test]
    fn a_source_is_fetched_only_as_it_is_written() {
        let w = World::new();
        let env = w.env(UPKEEP_NOW);
        fs::write(w.home.join("fleet.csv"), ONE_ACCOUNT_CSV).unwrap();
        let ran: std::sync::Arc<std::sync::Mutex<Vec<Ran>>> = std::sync::Arc::default();
        let run_as = |code: i32, stdout: &'static str| {
            let ran = std::sync::Arc::clone(&ran);
            move |cmd: aterm_uds::spawnfd::Command| {
                let lossy = |s: &std::ffi::OsStr| s.to_string_lossy().into_owned();
                ran.lock().unwrap().push((
                    lossy(cmd.get_program()),
                    cmd.get_args().map(lossy).collect(),
                ));
                Ok(super::super::align::Captured {
                    stdout: stdout.as_bytes().to_vec(),
                    code: Some(code),
                })
            }
        };
        let failing = run_as(22, "refused");
        let took = |source: &str, run: &RunFetcher| {
            let got = fetch_source_text_with(&env, source, run);
            (got, std::mem::take(&mut *ran.lock().unwrap()))
        };
        let sheet_export =
            "https://docs.google.com/spreadsheets/d/FAKE-SHEET/export?format=csv&gid=0";
        let (refused, ran) = took("rclone:gdrive:AI Email Accounting.csv", &failing);
        let refused = refused.unwrap_err();
        assert!(
            refused.contains("rclone remotes are not read")
                && refused.contains("/export?format=csv")
                && refused.contains(FLEET_ROSTER_FILE),
            "{refused}"
        );
        assert!(ran.is_empty(), "an rclone remote runs no program: {ran:?}");
        for (source, program, last) in [
            (
                "https://docs.google.com/spreadsheets/d/FAKE-SHEET/edit#gid=0",
                "curl",
                sheet_export,
            ),
            (LOOPBACK_SOURCE, "curl", LOOPBACK_SOURCE),
        ] {
            let (got, ran) = took(source, &failing);
            assert!(got.is_err(), "{source}: a failed fetch is the answer");
            assert_eq!(ran.len(), 1, "{source}: one program, then nothing: {ran:?}");
            let (exe, args) = &ran[0];
            assert_eq!(
                Path::new(exe).file_name().and_then(|n| n.to_str()),
                Some(program),
                "{source}"
            );
            assert_eq!(args.last().map(String::as_str), Some(last), "{source}");
        }
        let (got, ran) = took("~/fleet.csv", &failing);
        assert_eq!(got.as_deref(), Ok(ONE_ACCOUNT_CSV));
        assert!(ran.is_empty(), "a file runs no program: {ran:?}");
        let (got, ran) = took("remote:Fleet.csv", &failing);
        assert!(got.is_err(), "no such file here");
        assert!(ran.is_empty(), "a bare `remote:path` is a file: {ran:?}");

        let (got, ran) = took(LOOPBACK_SOURCE, &run_as(0, ONE_ACCOUNT_CSV));
        assert_eq!(got.as_deref(), Ok(ONE_ACCOUNT_CSV));
        assert_eq!(ran.len(), 1);
    }

    type Ran = (String, Vec<String>);

    #[test]
    fn a_fleet_file_no_one_here_named_a_source_for_opts_nothing_in() {
        let w = World::new();
        w.write_config("[fleet]\nmachine = \"m1\"\n");
        let wit = Witness::answering(ONE_ACCOUNT_CSV);
        let upkept = || {
            upkeep(
                &w.home,
                &w.state,
                (&w.roster(), UPKEEP_NOW, UpkeepStep::Both),
                &wit.fx(),
            )
        };
        let as_0_113_wrote_it = concat!(
            "# Managed by `aterm harness accounts sync` — shared LAN fleet account roster.\n",
            "# Run `aterm harness accounts sync <path-or-url>` on any machine to materialize\n",
            "# its local `accounts.toml`.\n\n",
            "[[account]]\n",
            "label = \"m1\"\n",
            "machines = [\"m1\"]\n",
            "class = \"dedicated\"\n",
            "email = \"m1@example.test\"\n\n",
        );
        let fleet = FleetRoster::from_toml(as_0_113_wrote_it, "old").expect("0.113.0's file");
        for text in [
            as_0_113_wrote_it.to_owned(),
            fleet.to_toml(),
            fleet.to_toml_for(Some(OptIn::Key)),
        ] {
            fs::write(w.fleet_file(), &text).unwrap();
            assert_eq!(
                upkept(),
                Upkept::Unconfigured {
                    fleet_file_ignored: true,
                    renewal: false,
                },
                "{text}"
            );
        }

        let mut oversized = format!("{OPT_IN_KEY} = \"named\"\n#");
        oversized.push_str(&"x".repeat(SOURCE_TEXT_MAX_BYTES + 1 - oversized.len()));
        fs::write(w.fleet_file(), oversized).unwrap();
        assert_eq!(
            configured_source(Some(&w.aterm_toml()), Some(&w.roster())),
            None
        );
        assert_eq!(
            upkept(),
            Upkept::Unconfigured {
                fleet_file_ignored: true,
                renewal: false,
            }
        );
        assert_eq!(wit.fetched(), Vec::<String>::new(), "nothing fetched");
        assert_eq!(wit.spawned(), 0, "no renewal started");
        assert!(!w.roster().exists(), "no roster written");

        let (mut out, mut err) = (Vec::new(), Vec::new());
        let synced = run_accounts_sync_with(
            &w.env(UPKEEP_NOW),
            (Some("~/fleet.csv"), None, false),
            (&mut out, &mut err),
            &SyncHow {
                fetch: &*wit.fetch,
                ladder: &[],
                fleet_snapshots: &[],
                heal: None,
                allow_empty_pool: false,
            },
        );
        assert_eq!(
            synced,
            ExitCode::SUCCESS,
            "{}",
            String::from_utf8_lossy(&err)
        );
        assert_eq!(fleet_file_opt_in(&w.fleet_file()), Some(OptIn::Named));
        assert_eq!(
            upkept(),
            Upkept::Configured {
                kind: "fleet-file",
                roster: true,
                renewal: false
            }
        );
    }

    #[test]
    fn a_walled_login_is_renewed_by_default_and_a_machine_without_a_route_says_so() {
        let w = World::new();
        w.write_config("[fleet]\nmachine = \"m15\"\n");
        fs::write(
            w.home.join(".claude.json"),
            "{\"oauthAccount\":{\"emailAddress\":\"m15-2@example.test\"}}\n",
        )
        .unwrap();
        let wit = Witness::answering(concat!(
            "email,computer,claude\n",
            "m15@example.test,m15,1\n",
            "m15-2@example.test,m15,1\n",
            "m15-3@example.test,m15,1\n"
        ));
        let now = UPKEEP_NOW;
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let synced = run_accounts_sync_with(
            &w.env(now),
            (Some("fleet.csv"), None, false),
            (&mut out, &mut err),
            &SyncHow {
                fetch: &*wit.fetch,
                ladder: &[],
                fleet_snapshots: &[],
                heal: None,
                allow_empty_pool: false,
            },
        );
        assert_eq!(
            synced,
            ExitCode::SUCCESS,
            "{}",
            String::from_utf8_lossy(&err)
        );
        let store = accounts::Store::at(&w.state);
        let mailbox_dir = store.dir.join(super::super::open_url::DEFAULT_MAILBOX_DIR);
        assert!(
            !mailbox_dir.exists(),
            "accounts sync must not fabricate an empty mailbox directory"
        );
        let file_roster = Roster::load(&w.roster()).unwrap();
        let wall = upgrade::StandingWall {
            at: u64::try_from(now).unwrap(),
            until: Some(1_792_936_800),
            window: Some("seven_day".to_owned()),
            credits: None,
        };
        let who = accounts::ProcessLogin {
            login: None,
            default_home: true,
            stored_login: true,
        };
        assert!(matches!(
            accounts::record_wall(
                &store,
                &store.read_usage_roster(Some(&w.roster()), now).unwrap(),
                "sid-m15",
                &who,
                &wall,
                now - 60,
                now
            ),
            accounts::Recorded::Wall { .. }
        ));
        assert_eq!(
            upkeep(
                &w.home,
                &w.state,
                (&w.roster(), now, UpkeepStep::Both),
                &wit.fx()
            ),
            Upkept::Configured {
                kind: "fleet-file",
                roster: true,
                renewal: false,
            }
        );
        assert_eq!(
            wit.spawned(),
            0,
            "without a mailbox or browser profile, upkeep starts no background sign-in"
        );
        // The incident of 2026-10-24: a walled session, its successors never signed in, and no
        // route on the machine. Each successor's attempt is an error on record naming every
        // route it tried, never a silent failure, and written once however often it is tried.
        let (logins, _) = store.load_logins(now).unwrap();
        let tried: Vec<(String, accounts::SignInAttempt)> =
            accounts::failing_sign_ins(&file_roster, &store.load(now).unwrap().0, &logins, now);
        assert!(!tried.is_empty(), "the successors' attempts are on record");
        for (label, attempt) in &tried {
            assert_eq!(
                attempt.outcome,
                accounts::AttemptOutcome::NoRoute,
                "{label}"
            );
            for route in [
                "profile(none)",
                "mailbox(none configured)",
                "vault(not a member)",
                "headless-browser(none installed)",
                "hosts(none configured)",
            ] {
                assert!(
                    attempt.routes.contains(route),
                    "{label}: {}",
                    attempt.routes
                );
            }
        }
        let sign_ins = || {
            store
                .tail(100)
                .unwrap()
                .into_iter()
                .filter(|e| e.kind == accounts::Kind::SignIn)
                .count()
        };
        assert_eq!(sign_ins(), tried.len(), "one ledger event per row");
        let no_route_lines = || {
            wit.logged()
                .into_iter()
                .filter(|line| line.contains("no-route"))
                .collect::<Vec<_>>()
        };
        let lines = no_route_lines();
        assert_eq!(lines.len(), tried.len(), "one log line per try: {lines:?}");
        for (label, _) in &tried {
            assert!(
                lines
                    .iter()
                    .any(|l| l.contains(&format!("account {label}:")) && l.contains("mailbox(")),
                "{label}: {lines:?}"
            );
        }
        let upkept = |step| upkeep(&w.home, &w.state, (&w.roster(), now, step), &wit.fx());
        upkept(UpkeepStep::Renew);
        assert_eq!(
            sign_ins(),
            tried.len(),
            "an unchanged failure is not written again"
        );
        assert_eq!(
            no_route_lines(),
            lines,
            "nothing logged while each row is held"
        );

        fs::create_dir_all(&mailbox_dir).unwrap();
        let fleet_aside = w.config.join("fleet-accounts.aside");
        fs::rename(w.fleet_file(), &fleet_aside).unwrap();

        assert_eq!(
            upkept(UpkeepStep::Both),
            Upkept::Unconfigured {
                fleet_file_ignored: false,
                renewal: true,
            },
            "a roster that stands is signed in with no source configured, by default"
        );
        assert_eq!(
            wit.spawned(),
            1,
            "the mailbox that appeared ends the backoff at once"
        );

        fs::rename(&fleet_aside, w.fleet_file()).unwrap();
        assert_eq!(
            upkept(UpkeepStep::Sync),
            Upkept::Configured {
                kind: "fleet-file",
                roster: true,
                renewal: false
            },
            "the sync step alone starts no sign-in"
        );
        assert_eq!(wit.spawned(), 1);
    }

    #[test]
    fn a_held_accounts_lock_fails_the_sync_and_says_so() {
        let w = World::new();
        w.write_config("[fleet]\nmachine = \"m1\"\n");
        let store = accounts::Store::at(&w.state);
        let sync_in_thread = |w: &World| {
            let env = w.env(UPKEEP_NOW);
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let fetch = |_: &Env, _: &str| Ok(ONE_ACCOUNT_CSV.to_owned());
                let (mut out, mut err) = (Vec::new(), Vec::new());
                let code = run_accounts_sync_with(
                    &env,
                    (Some("fleet.csv"), None, false),
                    (&mut out, &mut err),
                    &SyncHow {
                        fetch: &fetch,
                        ladder: &[Duration::from_millis(1); 3],
                        fleet_snapshots: &[],
                        heal: None,
                        allow_empty_pool: false,
                    },
                );
                let _ = tx.send((code, String::from_utf8_lossy(&err).into_owned()));
            });
            rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                .expect("a sync under a held lock gives up within its ladder")
        };
        let held = store.acquire().expect("the test holds the lock");
        let (code, err) = sync_in_thread(&w);
        assert_eq!(code, ExitCode::FAILURE);
        assert!(err.contains(accounts::LOCK_HELD), "{err}");
        assert!(!w.roster().exists(), "no roster under a held lock");

        drop(held);
        let (code, err) = sync_in_thread(&w);
        assert_eq!(code, ExitCode::SUCCESS, "{err}");
        assert!(w.roster().exists());
    }

    #[test]
    fn sync_accounts_for_init_preserves_key_opt_in_and_existing_fleet_roster() {
        let w = World::new();
        w.write_config("[fleet]\nmachine = \"m1\"\n");
        fs::write(
            w.home.join(".claude.json"),
            "{\"oauthAccount\":{\"emailAddress\":\"m1@example.test\"}}\n",
        )
        .unwrap();
        assert!(
            !has_init_accounts_source(&w.home, &w.state, &w.roster(), UPKEEP_NOW),
            "a single local account without a configured source must not trigger init sync"
        );

        let csv_path = w.home.join("fleet.csv");
        fs::write(
            &csv_path,
            "email,computer,claude\nm1@example.test,m1,1\nm1-2@example.test,m1,1\nm2@example.test,m2,1\n",
        )
        .unwrap();
        w.write_config(&format!(
            "[fleet]\naccounts_source = '{}'\nmachine = \"m1\"\n",
            csv_path.display()
        ));
        let (machine, m_count, enrolled, f_count) =
            sync_accounts_for_init(&w.home, &w.state, &w.roster(), UPKEEP_NOW)
                .expect("init sync from key");
        assert_eq!(machine, "m1");
        assert_eq!(m_count, 2);
        assert_eq!(enrolled, 1);
        assert_eq!(f_count, 3);
        assert_eq!(
            fleet_file_opt_in(&w.fleet_file()),
            Some(OptIn::Key),
            "sync_accounts_for_init from [fleet] accounts_source must record opt_in = \"key\""
        );

        w.write_config("[fleet]\nmachine = \"m1\"\n");
        assert_eq!(
            configured_source(Some(&w.aterm_toml()), Some(&w.roster())),
            None,
            "removing [fleet] accounts_source must opt the machine out"
        );
        assert!(
            has_init_accounts_source(&w.home, &w.state, &w.roster(), UPKEEP_NOW),
            "existing accounts.toml + fleet-accounts.toml can be upgraded by explicit init"
        );
        let (_, m_count2, _, f_count2) =
            sync_accounts_for_init(&w.home, &w.state, &w.roster(), UPKEEP_NOW)
                .expect("init sync from existing fleet-accounts.toml");
        assert_eq!(m_count2, 2);
        assert_eq!(
            f_count2, 3,
            "init sync must preserve all fleet rows in existing fleet-accounts.toml"
        );
        assert_eq!(fleet_file_opt_in(&w.fleet_file()), Some(OptIn::Named));
    }

    #[test]
    fn sync_accounts_for_init_merges_newly_signed_in_local_accounts() {
        let w = World::new();
        w.write_config("[fleet]\nmachine = \"m27\"\n");
        fs::write(
            w.home.join(".claude.json"),
            "{\"oauthAccount\":{\"emailAddress\":\"sol-m27@example.test\"}}\n",
        )
        .unwrap();
        fs::write(
            w.home.join(".claude.json.backup"),
            "{\"oauthAccount\":{\"emailAddress\":\"m27-2@example.test\"}}\n",
        )
        .unwrap();
        let (machine, count, enrolled, fleet_count) =
            sync_accounts_for_init(&w.home, &w.state, &w.roster(), UPKEEP_NOW)
                .expect("initial sync from local profiles");
        assert_eq!(machine, "m27");
        assert_eq!(count, 2);
        assert_eq!(enrolled, 1);
        assert_eq!(fleet_count, 2);

        fs::remove_file(w.home.join(".claude.json.backup")).unwrap();
        fs::write(
            w.home.join(".claude.json"),
            "{\"oauthAccount\":{\"emailAddress\":\"solar.m27.3@example.test\"}}\n",
        )
        .unwrap();
        let (machine2, count2, _, fleet_count2) =
            sync_accounts_for_init(&w.home, &w.state, &w.roster(), UPKEEP_NOW + 10)
                .expect("second sync merges new local account");
        assert_eq!(machine2, "m27");
        assert_eq!(count2, 3);
        assert_eq!(fleet_count2, 3);

        let before = Roster::load(&w.roster()).unwrap();
        fs::write(
            w.home.join(".claude.json"),
            "{\"oauthAccount\":{\"emailAddress\":\"new@example.test\"}}\n",
        )
        .unwrap();
        let (_, count, _, fleet_count) =
            sync_accounts_for_init(&w.home, &w.state, &w.roster(), UPKEEP_NOW + 20)
                .expect("merge a colliding fallback label");
        assert_eq!((count, fleet_count), (4, 4));
        let merged = Roster::load(&w.roster()).unwrap();
        for row in &before.rows {
            assert_eq!(merged.row(&row.label).unwrap().email, row.email);
        }
        assert_eq!(
            merged.row("m27-4").unwrap().email.as_deref(),
            Some("new@example.test")
        );
        assert_eq!(merged.row_for_login(None).unwrap().label, "m27-4");
        let (_, count, _, fleet_count) =
            sync_accounts_for_init(&w.home, &w.state, &w.roster(), UPKEEP_NOW + 21)
                .expect("the same discovered email is not another account");
        assert_eq!((count, fleet_count), (4, 4));
        assert_eq!(Roster::load(&w.roster()).unwrap(), merged);

        fs::write(
            w.home.join(".claude.json"),
            "{\"oauthAccount\":{\"emailAddress\":\"later@example.test\"}}\n",
        )
        .unwrap();
        let roster_bytes = fs::read(w.roster()).unwrap();
        let fleet_bytes = fs::read(w.fleet_file()).unwrap();
        let malformed = b"[[account]\nlabel = \"broken\"\n";
        fs::write(w.roster(), malformed).unwrap();
        assert!(sync_accounts_for_init(&w.home, &w.state, &w.roster(), UPKEEP_NOW + 30).is_err());
        assert_eq!(fs::read(w.roster()).unwrap(), malformed);
        assert_eq!(fs::read(w.fleet_file()).unwrap(), fleet_bytes);
        fs::write(w.roster(), &roster_bytes).unwrap();

        let store = accounts::Store::at(&w.state);
        for path in [store.state_file(), store.logins_file()] {
            let prior = path.exists().then(|| fs::read(&path).unwrap());
            fs::write(&path, "{\"schema\":999}").unwrap();
            assert!(
                sync_accounts_for_init(&w.home, &w.state, &w.roster(), UPKEEP_NOW + 30).is_err()
            );
            assert_eq!(fs::read(w.roster()).unwrap(), roster_bytes);
            assert_eq!(fs::read(w.fleet_file()).unwrap(), fleet_bytes);
            assert_eq!(fs::read(&path).unwrap(), b"{\"schema\":999}");
            match prior {
                Some(bytes) => fs::write(path, bytes).unwrap(),
                None => fs::remove_file(path).unwrap(),
            }
        }
        let (_, count, _, fleet_count) =
            sync_accounts_for_init(&w.home, &w.state, &w.roster(), UPKEEP_NOW + 30)
                .expect("publish after account state admits again");
        assert_eq!((count, fleet_count), (5, 5));

        let fresh = World::new();
        fresh.write_config("[fleet]\nmachine = \"m27\"\n");
        let fresh_secrets = fresh.home.join(".secrets");
        fs::create_dir(&fresh_secrets).unwrap();
        let fresh_source = fresh_secrets.join(FLEET_ROSTER_FILE);
        for (file, email) in [
            (".claude.json", "one@example.test"),
            (".claude.json.backup", "two@example.test"),
        ] {
            fs::write(
                fresh.home.join(file),
                format!("{{\"oauthAccount\":{{\"emailAddress\":\"{email}\"}}}}\n"),
            )
            .unwrap();
        }
        let store = accounts::Store::at(&fresh.state);
        fs::create_dir_all(&store.dir).unwrap();
        fs::write(store.state_file(), "{\"schema\":999}").unwrap();
        assert!(
            sync_accounts_for_init(&fresh.home, &fresh.state, &fresh.roster(), UPKEEP_NOW).is_err()
        );
        assert!(!fresh.roster().exists());
        assert!(!fresh.fleet_file().exists());
        assert!(!fresh_source.exists());
        assert_eq!(fs::read(store.state_file()).unwrap(), b"{\"schema\":999}");
        fs::remove_file(store.state_file()).unwrap();
        let (_, count, _, fleet_count) =
            sync_accounts_for_init(&fresh.home, &fresh.state, &fresh.roster(), UPKEEP_NOW)
                .expect("initial discovery publishes after admission");
        assert_eq!((count, fleet_count), (2, 2));
        assert_eq!(
            fs::read(&fresh_source).unwrap(),
            fs::read(fresh.fleet_file()).unwrap()
        );

        let captured = fs::read(w.fleet_file()).unwrap();
        let mut concurrent =
            FleetRoster::from_toml(std::str::from_utf8(&captured).unwrap(), "captured").unwrap();
        let mut another = concurrent.accounts[0].clone();
        another.label = "m27-other".to_owned();
        another.email = "concurrent@example.test".to_owned();
        concurrent.accounts.push(another);
        let replacement = concurrent.to_toml_for(Some(OptIn::Named));
        let sync_across_change =
            |w: &World, expected: Option<Vec<u8>>, checked_path: &Path, publish: Publish| {
                let state_dir = w.state.clone();
                let changed_path = checked_path.to_path_buf();
                let answer = String::from_utf8(captured.clone()).unwrap();
                let replacement = replacement.clone();
                let fetch = move |_: &Env, _: &str| {
                    let store = accounts::Store::at(&state_dir);
                    let _held = store.acquire().unwrap();
                    fs::write(&changed_path, &replacement).unwrap();
                    Ok(answer.clone())
                };
                let expected = FleetSnapshot {
                    path: checked_path.to_path_buf(),
                    bytes: expected,
                    publish,
                };
                let (mut out, mut err) = (Vec::new(), Vec::new());
                let source = w.fleet_file().to_string_lossy().into_owned();
                let code = run_accounts_sync_with(
                    &w.env(UPKEEP_NOW + 40),
                    (Some(&source), None, true),
                    (&mut out, &mut err),
                    &SyncHow {
                        fetch: &fetch,
                        ladder: &[],
                        fleet_snapshots: &[expected],
                        heal: None,
                        allow_empty_pool: false,
                    },
                );
                assert_eq!(code, ExitCode::FAILURE);
                assert!(String::from_utf8_lossy(&err).contains("changed after init discovery"));
            };
        let roster_bytes = fs::read(w.roster()).unwrap();
        sync_across_change(&w, Some(captured.clone()), &w.fleet_file(), Publish::No);
        assert_eq!(fs::read(w.roster()).unwrap(), roster_bytes);
        assert_eq!(fs::read(w.fleet_file()).unwrap(), replacement.as_bytes());

        let first = World::new();
        first.write_config("[fleet]\nmachine = \"m27\"\n");
        assert!(!first.fleet_file().exists());
        sync_across_change(&first, None, &first.fleet_file(), Publish::No);
        assert!(!first.roster().exists());
        assert_eq!(
            fs::read(first.fleet_file()).unwrap(),
            replacement.as_bytes()
        );
        let first_secrets = first.home.join(".secrets");
        fs::create_dir(&first_secrets).unwrap();
        let first_source = first_secrets.join(FLEET_ROSTER_FILE);
        sync_across_change(&first, None, &first_source, Publish::Fleet);
        assert!(!first.roster().exists());
        assert_eq!(fs::read(&first_source).unwrap(), replacement.as_bytes());
        assert_eq!(
            fs::read(first.fleet_file()).unwrap(),
            replacement.as_bytes()
        );

        for (secrets_source, merging) in
            [(true, false), (true, true), (false, false), (false, true)]
        {
            let race = World::new();
            race.write_config("[fleet]\nmachine = \"m27\"\n");
            let source = if secrets_source {
                let secrets = race.home.join(".secrets");
                fs::create_dir(&secrets).unwrap();
                secrets.join(FLEET_ROSTER_FILE)
            } else {
                race.fleet_file()
            };
            let mut initial =
                FleetRoster::from_toml(std::str::from_utf8(&captured).unwrap(), "initial").unwrap();
            initial.accounts.truncate(2);
            // The list discovery wrote: a named list takes only accounts this Mac shows
            // all of, and here only the first is still shown.
            fs::write(&source, initial.to_toml_for(Some(OptIn::Discovered))).unwrap();
            fs::write(
                race.home.join(".claude.json"),
                format!(
                    "{{\"oauthAccount\":{{\"emailAddress\":\"{}\"}}}}\n",
                    initial.accounts[0].email
                ),
            )
            .unwrap();
            let store = accounts::Store::at(&race.state);
            store.machine_id("0123456789abcdef").unwrap();
            sync_accounts_for_init(&race.home, &race.state, &race.roster(), UPKEEP_NOW + 50)
                .unwrap();
            if merging {
                fs::write(
                    race.home.join(".claude.json.backup"),
                    "{\"oauthAccount\":{\"emailAddress\":\"pending.m27.4@example.test\"}}\n",
                )
                .unwrap();
            }
            let checked = store.claim_staging_dir("m27-3").unwrap();
            let paths = [
                source.clone(),
                race.roster(),
                race.fleet_file(),
                store.state_file(),
                store.logins_file(),
                store.ledger_path(),
                store.machine_file(),
                checked.join(accounts::RENEWAL_CHECKPOINT_FILE),
            ];
            let snapshot = || {
                paths
                    .iter()
                    .map(|path| fs::read(path).unwrap())
                    .collect::<Vec<_>>()
            };
            let mut newer = None;
            let result = sync_accounts_for_init_with(
                &race.home,
                &race.state,
                &race.roster(),
                UPKEEP_NOW + 52,
                || {
                    // Q completes after P's real source capture, before its lock admission.
                    fs::write(
                        race.home.join(".claude.json.backup"),
                        "{\"oauthAccount\":{\"emailAddress\":\"newer.m27.3@example.test\"}}\n",
                    )
                    .unwrap();
                    fs::write(
                        checked.join(".claude.json"),
                        "{\"oauthAccount\":{\"emailAddress\":\"newer.m27.3@example.test\"}}\n",
                    )
                    .unwrap();
                    accounts::write_renewal_checkpoint(
                        "m27-3",
                        "newer.m27.3@example.test",
                        checked.to_str().unwrap(),
                        Some("max"),
                        UPKEEP_NOW + 51,
                        &store.load_logins(UPKEEP_NOW + 51).unwrap().0,
                    )
                    .unwrap();
                    let (_, rows, _, fleet_rows) = sync_accounts_for_init(
                        &race.home,
                        &race.state,
                        &race.roster(),
                        UPKEEP_NOW + 51,
                    )
                    .unwrap();
                    assert_eq!((rows, fleet_rows), (3, 3));
                    let (logins, _) = store.load_logins(UPKEEP_NOW + 51).unwrap();
                    assert_eq!(
                        logins.get("m27-3").unwrap().dir.as_deref(),
                        checked.to_str()
                    );
                    assert!(
                        fs::read_to_string(&source)
                            .unwrap()
                            .contains("newer.m27.3@example.test")
                    );
                    newer = Some(snapshot());
                },
                &fetch_source_text,
            );
            assert!(result.unwrap_err().contains("changed after init discovery"));
            assert_eq!(snapshot(), newer.unwrap());
        }
    }

    #[test]
    fn upkeep_converges_from_secrets_mailbox_and_proactively_renews_due_or_signed_out_login() {
        let w = World::new();
        // The mailbox's accounts are m27's: the world says it is m27 rather
        // than leaving discovery to read the host's own name.
        w.write_config("[fleet]\nmachine = \"m27\"\n");
        let secrets_dir = w.home.join(".secrets");
        fs::create_dir_all(&secrets_dir).unwrap();
        fs::write(
            secrets_dir.join(super::super::headless_oauth::SECRETS_MAILBOX_TOML_FILE),
            concat!(
                "[passwords]\n",
                "\"sol-m27@example.test\" = \"pw1\"\n",
                "\"sun.m27.2@example.test\" = \"pw2\"\n",
            ),
        )
        .unwrap();

        let wit = Witness::answering("");
        assert!(has_init_accounts_source(
            &w.home,
            &w.state,
            &w.roster(),
            UPKEEP_NOW
        ));
        assert_eq!(
            upkeep(
                &w.home,
                &w.state,
                (&w.roster(), UPKEEP_NOW, UpkeepStep::Sync),
                &wit.fx()
            ),
            Upkept::Configured {
                kind: "fleet-file",
                roster: true,
                renewal: false,
            },
            "upkeep auto-discovers accounts from ~/.secrets/ai-mailbox.toml even without .claude.json"
        );
        let file_roster = Roster::load(&w.roster()).expect("accounts.toml materialized");
        assert_eq!(file_roster.rows.len(), 2);
        assert_eq!(
            file_roster.rows[0].email.as_deref(),
            Some("sol-m27@example.test")
        );
        assert_eq!(
            file_roster.rows[1].email.as_deref(),
            Some("sun.m27.2@example.test")
        );
        assert!(
            !needs_init_accounts_sync(&w.home, &w.state, &w.roster(), UPKEEP_NOW),
            "needs_init_accounts_sync is false when accounts.toml and fleet-accounts.toml match mailbox"
        );

        // Adding a 3rd account password to ~/.secrets/ai-mailbox.toml after ~/.secrets/fleet-accounts.toml
        // already exists triggers needs_init_accounts_sync and merges the new account on upkeep.
        fs::write(
            secrets_dir.join(super::super::headless_oauth::SECRETS_MAILBOX_TOML_FILE),
            concat!(
                "[passwords]\n",
                "\"sol-m27@example.test\" = \"pw1\"\n",
                "\"sun.m27.2@example.test\" = \"pw2\"\n",
                "\"star.m27.3@example.test\" = \"pw3\"\n",
            ),
        )
        .unwrap();
        assert!(
            needs_init_accounts_sync(&w.home, &w.state, &w.roster(), UPKEEP_NOW),
            "needs_init_accounts_sync detects new mailbox account even when ~/.secrets/fleet-accounts.toml exists"
        );
        assert_eq!(
            upkeep(
                &w.home,
                &w.state,
                (&w.roster(), UPKEEP_NOW + 1, UpkeepStep::Sync),
                &wit.fx()
            ),
            Upkept::Configured {
                kind: "fleet-file",
                roster: true,
                renewal: false,
            }
        );
        let file_roster =
            Roster::load(&w.roster()).expect("accounts.toml updated with 3rd account");
        assert_eq!(file_roster.rows.len(), 3);
        assert_eq!(
            file_roster.rows[2].email.as_deref(),
            Some("star.m27.3@example.test")
        );
        assert!(
            fs::read_to_string(secrets_dir.join(FLEET_ROSTER_FILE))
                .unwrap()
                .contains("star.m27.3@example.test"),
            "~/.secrets/fleet-accounts.toml must also be updated with the newly discovered mailbox account"
        );
        assert!(!needs_init_accounts_sync(
            &w.home,
            &w.state,
            &w.roster(),
            UPKEEP_NOW + 1
        ));

        let store = accounts::Store::at(&w.state);
        let old_dir = store.claim_staging_dir("m27").unwrap();
        let old_dir_text = old_dir.to_str().unwrap().to_owned();
        fs::write(
            old_dir.join(".claude.json"),
            "{\"oauthAccount\":{\"emailAddress\":\"sol-m27@example.test\",\"subscriptionType\":\"max\"}}\n",
        )
        .unwrap();
        let mut note = None;
        store
            .transact_logins(UPKEEP_NOW, &mut note, |state, logins| {
                let mut ev = accounts::note_renewed(
                    state,
                    logins,
                    "m27",
                    file_roster.row("m27").and_then(|r| r.dir.as_deref()),
                    &old_dir_text,
                    Some("max"),
                    UPKEEP_NOW - 28 * 86_400,
                );
                accounts::note_login_notice(logins, "m27", 2, UPKEEP_NOW);
                ev.push(accounts::Event::new(UPKEEP_NOW, accounts::Kind::Use, "m27"));
                state.active = Some("m27".to_owned());
                Ok(ev)
            })
            .unwrap();

        let (state_due, _) = store.load(UPKEEP_NOW).unwrap();
        let (logins_due, _) = store.load_logins(UPKEEP_NOW).unwrap();
        let due_target = next_auto_renew_target(&file_roster, &state_due, &logins_due, UPKEEP_NOW)
            .expect("proactive renewal must select due enrolled account m27 before it expires");
        assert_eq!(due_target.label, "m27");

        let new_staging = store.claim_staging_dir("m27").unwrap();
        let new_staging_text = new_staging.to_str().unwrap().to_owned();
        fs::write(
            new_staging.join(".claude.json"),
            "{\"oauthAccount\":{\"emailAddress\":\"sol-m27@example.test\",\"subscriptionType\":\"max\"}}\n",
        )
        .unwrap();
        let status_got = super::super::align::Captured {
            stdout: b"{\"loggedIn\":true,\"authMethod\":\"claude.ai\",\"email\":\"sol-m27@example.test\",\"subscriptionType\":\"max\"}\n".to_vec(),
            code: Some(0),
        };
        assert!(complete_staging_renewal(
            &store,
            &file_roster,
            &due_target.label,
            &new_staging,
            &new_staging_text,
            &status_got,
            UPKEEP_NOW + 10,
        ));

        let (state_renewed, _) = store.load(UPKEEP_NOW + 10).unwrap();
        let (logins_renewed, _) = store.load_logins(UPKEEP_NOW + 10).unwrap();
        let bound_after = accounts::bound(&file_roster, &logins_renewed);
        let enrolled_after = accounts::enrolled(&bound_after);
        let who_old = accounts::ProcessLogin {
            login: Some(old_dir_text),
            default_home: true,
            stored_login: true,
        };
        let me_old = who_old.me(&bound_after);
        assert_eq!(
            me_old.left,
            Some("m27"),
            "running session on old staging dir must recognize its login was superseded by m27's renewal"
        );
        assert_eq!(
            accounts::advise(
                &bound_after,
                &state_renewed,
                &enrolled_after,
                &me_old,
                &accounts::Point::TurnEnd,
                accounts::Mode::Wall,
                UPKEEP_NOW + 10,
            ),
            accounts::Advice::Move {
                to: "m27".to_owned(),
                ahead: true,
                rotates: false,
                takes_canary: false,
            },
            "at the next turn end (Point::TurnEnd), the session follows m27 to the new staging login with zero wall downtime"
        );

        let mut note = None;
        store
            .transact_logins(UPKEEP_NOW + 20, &mut note, |state, logins| {
                Ok(accounts::note_login_end(
                    state,
                    logins,
                    "m27",
                    UPKEEP_NOW + 20,
                ))
            })
            .unwrap();
        let (state_signed_out, _) = store.load(UPKEEP_NOW + 20).unwrap();
        let (logins_signed_out, _) = store.load_logins(UPKEEP_NOW + 20).unwrap();
        let signed_out_target = next_auto_renew_target(
            &file_roster,
            &state_signed_out,
            &logins_signed_out,
            UPKEEP_NOW + 20,
        )
        .expect("when m27 is signed out and sun.m27.2 is unenrolled, next_auto_renew_target must select a renewal target");
        assert!(
            signed_out_target.label == "m27-2" || signed_out_target.label == "m27",
            "unexpected target: {}",
            signed_out_target.label
        );
    }

    #[cfg(unix)]
    #[test]
    fn renewal_checkpoints_preserve_obstructions_and_refuse_links_and_special_files() {
        use std::os::unix::ffi::OsStrExt as _;
        for kind in ["file", "directory", "symlink", "fifo"] {
            let w = World::new();
            let store = accounts::Store::at(&w.state);
            let staging = store.claim_staging_dir("m27").unwrap();
            let path = staging.join(accounts::RENEWAL_CHECKPOINT_FILE);
            let target = w.home.join("owner-work");
            accounts::write_renewal_checkpoint(
                "m27",
                "sol-m27@example.test",
                staging.to_str().unwrap(),
                Some("max"),
                UPKEEP_NOW,
                &store.load_logins(UPKEEP_NOW).unwrap().0,
            )
            .unwrap();
            let valid_bytes = fs::read(&path).unwrap();
            fs::remove_file(&path).unwrap();
            fs::write(&target, &valid_bytes).unwrap();
            match kind {
                "file" => fs::write(&path, b"unknown content").unwrap(),
                "directory" => fs::create_dir(&path).unwrap(),
                "symlink" => std::os::unix::fs::symlink(&target, &path).unwrap(),
                "fifo" => {
                    let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
                    // SAFETY: `name` is NUL-terminated and lives in this test's
                    // exclusive directory. mkfifo retains no pointer.
                    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
                }
                _ => unreachable!(),
            }
            let before = fs::symlink_metadata(&path).unwrap();
            assert!(
                accounts::write_renewal_checkpoint(
                    "m27",
                    "sol-m27@example.test",
                    staging.to_str().unwrap(),
                    Some("max"),
                    UPKEEP_NOW,
                    &store.load_logins(UPKEEP_NOW).unwrap().0,
                )
                .is_err(),
                "{kind}"
            );
            assert!(
                accounts::read_renewal_checkpoint(
                    &staging,
                    &fs::canonicalize(&staging).unwrap(),
                    UPKEEP_NOW,
                )
                .is_none(),
                "{kind}"
            );
            assert_eq!(
                fs::symlink_metadata(&path).unwrap().file_type(),
                before.file_type()
            );
            if kind == "file" {
                assert_eq!(fs::read(&path).unwrap(), b"unknown content");
            }
            assert_eq!(fs::read(&target).unwrap(), valid_bytes);
        }
    }

    #[test]
    fn recovery_refuses_unproven_or_stale_checkpoints_without_changing_accounts() {
        for case in [
            "missing",
            "malformed",
            "schema",
            "legacy-schema",
            "parent-missing",
            "parent-negative",
            "parent-clock-missing",
            "parent-clock-null",
            "parent-exceeds-clock",
            "program",
            "label",
            "email",
            "dir",
            "future",
            "zero",
            "subscription",
            "oversized",
            "directory",
            "auth-equal",
            "auth-newer",
            "renewed-equal",
            "renewed-newer",
            "ended-equal",
            "wrong-equal",
            "served-newer",
            "notice",
            "limited",
        ] {
            let w = World::new();
            let store = accounts::Store::at(&w.state);
            store.machine_id("0123456789abcdef").unwrap();
            let row = Row {
                label: "m27".into(),
                class: Class::Dedicated,
                dir: None,
                was: Vec::new(),
                vendor: Default::default(),
                email: Some("sol-m27@example.test".into()),
                identity_email: None,
                browser: None,
                held_by: None,
            };
            let roster = Roster { rows: vec![row] };
            let staging = store.claim_staging_dir("m27").unwrap();
            let profile = br#"{"oauthAccount":{"emailAddress":"sol-m27@example.test"}}"#;
            fs::write(staging.join(".claude.json"), profile).unwrap();
            let checked_at = UPKEEP_NOW - 10;
            // Keep the parent unchanged while testing independent timestamp, limit and
            // checkpoint-shape guards; adding a binding must not mask those refusals.
            store
                .transact_logins(UPKEEP_NOW, &mut None, |_, logins| {
                    logins
                        .labels
                        .insert("m27".into(), accounts::Binding::default());
                    Ok(Vec::new())
                })
                .unwrap();
            accounts::write_renewal_checkpoint(
                "m27",
                "sol-m27@example.test",
                staging.to_str().unwrap(),
                Some("max"),
                checked_at,
                &store.load_logins(UPKEEP_NOW).unwrap().0,
            )
            .unwrap();
            let proof_path = staging.join(accounts::RENEWAL_CHECKPOINT_FILE);
            let mut proof: Value =
                aterm_json::from_str(&fs::read_to_string(&proof_path).unwrap()).unwrap();
            match case {
                "missing" => fs::remove_file(&proof_path).unwrap(),
                "malformed" => fs::write(&proof_path, b"unfinished {").unwrap(),
                "oversized" => fs::write(&proof_path, vec![b' '; 4097]).unwrap(),
                "directory" => {
                    fs::remove_file(&proof_path).unwrap();
                    fs::create_dir(&proof_path).unwrap();
                }
                "schema"
                | "legacy-schema"
                | "parent-missing"
                | "parent-negative"
                | "parent-clock-missing"
                | "parent-clock-null"
                | "parent-exceeds-clock"
                | "program"
                | "label"
                | "email"
                | "dir"
                | "future"
                | "zero"
                | "subscription" => {
                    let (key, value) = match case {
                        "schema" => ("schema", Value::from(3)),
                        "legacy-schema" => ("schema", Value::from(1)),
                        "parent-missing" => ("parent_generation", Value::Null),
                        "parent-negative" => ("parent_generation", Value::from(-1)),
                        "parent-clock-missing" | "parent-clock-null" => {
                            ("parent_clock", Value::Null)
                        }
                        "parent-exceeds-clock" => ("parent_generation", Value::from(1)),
                        "program" => ("program", Value::from("other")),
                        "label" => ("label", Value::from("m28")),
                        "email" => ("email", Value::from("someone@example.test")),
                        "dir" => ("dir", Value::from(w.home.to_str().unwrap())),
                        "future" => ("checked_at", Value::from(UPKEEP_NOW + 1)),
                        "zero" => ("checked_at", Value::from(0)),
                        _ => ("subscription", Value::from("x".repeat(33))),
                    };
                    proof.as_object_mut().unwrap().insert(key.into(), value);
                    if case == "parent-missing" {
                        proof.as_object_mut().unwrap().remove("parent_generation");
                    }
                    if case == "parent-clock-missing" {
                        proof.as_object_mut().unwrap().remove("parent_clock");
                    }
                    fs::write(&proof_path, proof.to_string()).unwrap();
                }
                _ => {
                    store
                        .transact_logins(UPKEEP_NOW, &mut None, |state, logins| {
                            let account = state.accounts.entry("m27".into()).or_default();
                            let binding = logins.labels.entry("m27".into()).or_default();
                            match case {
                                "auth-equal" | "auth-newer" => {
                                    account.auth = accounts::Auth::SignedOut;
                                    account.auth_at =
                                        Some(checked_at + i64::from(case == "auth-newer"));
                                }
                                "renewed-equal" | "renewed-newer" => {
                                    binding.renewed_at =
                                        Some(checked_at + i64::from(case == "renewed-newer"))
                                }
                                "ended-equal" => binding.ended_at = Some(checked_at),
                                "wrong-equal" => binding.wrong_at = Some(checked_at),
                                "served-newer" => binding.served_at = Some(checked_at + 1),
                                "notice" => binding.notice_end = Some(UPKEEP_NOW + 100),
                                "limited" => {
                                    account.walls.insert(
                                        "seven_day".into(),
                                        accounts::Wall {
                                            until: Some(UPKEEP_NOW + 100),
                                            at: UPKEEP_NOW - 1,
                                            by: "aa".into(),
                                            attempts: 0,
                                            cleared: false,
                                        },
                                    );
                                }
                                _ => unreachable!(),
                            }
                            Ok(Vec::new())
                        })
                        .unwrap();
                }
            }
            let state = store.load(UPKEEP_NOW).unwrap().0;
            let logins = store.load_logins(UPKEEP_NOW).unwrap().0;
            let ledger = store.tail(10).unwrap();
            let guard = std::sync::Arc::new(std::sync::Mutex::new(AutoRenewGuard::default()));
            assert!(
                !recover_interrupted_staging_renewals(&store, &roster, &guard, UPKEEP_NOW),
                "{case}"
            );
            assert_eq!(store.load(UPKEEP_NOW).unwrap().0, state, "{case}");
            assert_eq!(store.load_logins(UPKEEP_NOW).unwrap().0, logins, "{case}");
            assert_eq!(store.tail(10).unwrap(), ledger, "{case}");
            assert_eq!(
                fs::read(staging.join(".claude.json")).unwrap(),
                profile,
                "{case}"
            );
            assert!(staging.exists(), "{case}");
        }
    }

    #[test]
    fn recovery_prefers_the_latest_checked_account_across_prefixes_and_numeric_ties() {
        for case in ["cross-prefix", "numeric-tie", "verbatim-dot"] {
            let cross_prefix = case == "cross-prefix";
            let numeric_tie = case == "numeric-tie";
            let w = World::new();
            let store = accounts::Store::at(&w.state);
            let roster = Roster {
                rows: [
                    ("m27", "sol-m27@example.test"),
                    ("m27-2", "sun.m27.2@example.test"),
                ]
                .into_iter()
                .map(|(label, email)| Row {
                    label: label.into(),
                    class: Class::Dedicated,
                    dir: Some(store.login_dir(label).unwrap().to_str().unwrap().into()),
                    was: Vec::new(),
                    vendor: Default::default(),
                    email: Some(email.into()),
                    identity_email: None,
                    browser: None,
                    held_by: None,
                })
                .collect(),
            };
            let old_prefix = if cross_prefix { "m27" } else { "m27-2" };
            if numeric_tie {
                store.claim_staging_dir(old_prefix).unwrap();
            }
            let older = store.claim_staging_dir(old_prefix).unwrap();
            if numeric_tie {
                for _ in 3..10 {
                    store.claim_staging_dir(old_prefix).unwrap();
                }
            }
            let newer = store.claim_staging_dir("m27-2").unwrap();
            let newer_text = if case == "verbatim-dot" {
                format!("{}/./", newer.display())
            } else {
                newer.to_str().unwrap().to_owned()
            };
            // Synthetic crash points after the real checkpoint writer and
            // before binding; each status must pass the production fact reader.
            let newer_at = UPKEEP_NOW - 5;
            for (dir, checked_at, subscription) in [
                (&older, newer_at - i64::from(!numeric_tie), "pro"),
                (&newer, newer_at, "max"),
            ] {
                fs::write(dir.join(".claude.json"), b"private login fixture").unwrap();
                let status = super::super::align::Captured {
                    stdout: format!(r#"{{"loggedIn":true,"authMethod":"claude.ai","email":"sun.m27.2@example.test","subscriptionType":"{subscription}"}}"#).into_bytes(),
                    code: Some(0),
                };
                let login_text = if dir == &newer {
                    newer_text.as_str()
                } else {
                    dir.to_str().unwrap()
                };
                let row = &roster.rows[1];
                let email = row.email.as_deref().unwrap();
                let facts = super::super::cli::status_facts(&status, email).unwrap();
                assert!(facts.renews());
                accounts::write_renewal_checkpoint(
                    &row.label,
                    email,
                    login_text,
                    facts.subscription.as_deref(),
                    checked_at,
                    &store.load_logins(UPKEEP_NOW).unwrap().0,
                )
                .unwrap();
                assert!(!store.state_file().exists());
            }
            let guard = std::sync::Arc::new(std::sync::Mutex::new(AutoRenewGuard::default()));
            assert!(recover_interrupted_staging_renewals(
                &store, &roster, &guard, UPKEEP_NOW
            ));
            let logins = store.load_logins(UPKEEP_NOW).unwrap().0;
            let binding = &logins.labels["m27-2"];
            assert_eq!(binding.dir.as_deref(), Some(newer_text.as_str()));
            assert_eq!(binding.renewed_at, Some(newer_at));
            assert_eq!(binding.subscription.as_deref(), Some("max"));
            assert!(!logins.labels.contains_key("m27"));
            assert!(older.join(accounts::RENEWAL_CHECKPOINT_FILE).is_file());
            let events = store.tail(10).unwrap();
            assert_eq!(events.len(), 2);
            assert!(events.iter().all(|event| event.t == newer_at));
            assert!(!recover_interrupted_staging_renewals(
                &store,
                &roster,
                &guard,
                UPKEEP_NOW + 20
            ));
            assert_eq!(store.load_logins(UPKEEP_NOW + 20).unwrap().0, logins);
        }
    }

    #[test]
    fn claude_alias_completion_and_recovery_require_the_canonical_identity() {
        for recover in [false, true] {
            for reported in ["canonical@example.test", "alias@example.test"] {
                let w = World::new();
                let store = accounts::Store::at(&w.state);
                let roster = Roster {
                    rows: vec![Row {
                        label: "m27".into(),
                        email: Some("alias@example.test".into()),
                        identity_email: Some("canonical@example.test".into()),
                        dir: Some(store.login_dir("m27").unwrap().to_str().unwrap().into()),
                        ..Row::default()
                    }],
                };
                let staging = store.claim_staging_dir("m27").unwrap();
                let staging_text = staging.to_str().unwrap();
                fs::write(
                    staging.join(".claude.json"),
                    format!(r#"{{"oauthAccount":{{"emailAddress":"{reported}"}}}}"#),
                )
                .unwrap();
                let accepted = if recover {
                    accounts::write_renewal_checkpoint(
                        "m27",
                        reported,
                        staging_text,
                        Some("max"),
                        UPKEEP_NOW - 1,
                        &store.load_logins(UPKEEP_NOW).unwrap().0,
                    )
                    .unwrap();
                    let guard =
                        std::sync::Arc::new(std::sync::Mutex::new(AutoRenewGuard::default()));
                    recover_interrupted_staging_renewals(&store, &roster, &guard, UPKEEP_NOW)
                } else {
                    let status = super::super::align::Captured {
                        stdout: format!(
                            r#"{{"loggedIn":true,"authMethod":"claude.ai","email":"{reported}","subscriptionType":"max"}}"#
                        )
                        .into_bytes(),
                        code: Some(0),
                    };
                    complete_staging_renewal(
                        &store,
                        &roster,
                        "m27",
                        &staging,
                        staging_text,
                        &status,
                        UPKEEP_NOW,
                    )
                };
                let canonical = reported == "canonical@example.test";
                assert_eq!(
                    accepted, canonical,
                    "recover={recover}, reported={reported}"
                );
                let logins = store.load_logins(UPKEEP_NOW).unwrap().0;
                assert_eq!(
                    logins.get("m27").and_then(|binding| binding.dir.as_deref()),
                    canonical.then_some(staging_text)
                );
                if canonical {
                    let proof = accounts::read_renewal_checkpoint(
                        &staging,
                        &fs::canonicalize(&staging).unwrap(),
                        UPKEEP_NOW,
                    )
                    .unwrap();
                    assert_eq!(proof.email, "canonical@example.test");
                }
            }
        }
    }

    #[test]
    fn shared_checkpoint_recovery_commits_identity_and_consumed_outcome_together() {
        let w = World::new();
        let store = accounts::Store::at(&w.state);
        let row = Row {
            label: "m3.2".into(),
            email: Some("owner@example.test".into()),
            ..Row::default()
        };
        let roster = Roster {
            rows: vec![row.clone()],
        };
        let staging = store.claim_staging_dir(&row.label).unwrap();
        let source = accounts::shared::Source {
            network: [1; 32],
            account: [2; 32],
        };
        store
            .transact_logins_within(
                &accounts::LOCK_LADDER,
                UPKEEP_NOW - 10,
                &mut None,
                |_, logins| {
                    logins.shared.begin(
                        accounts::shared::Intent {
                            source,
                            decision: [3; 32],
                            generation: 1,
                            configuration: [4; 32],
                            request: [5; 16],
                            label: row.label.clone(),
                            identity: row.email.clone().unwrap(),
                            staging: staging.to_str().unwrap().into(),
                            before_generation: None,
                            begun_at: UPKEEP_NOW - 10,
                        },
                        None,
                    )?;
                    Ok(Vec::new())
                },
            )
            .unwrap();
        fs::write(staging.join(".claude.json"), r#"{"oauthAccount":{"emailAddress":"owner@example.test","accountUuid":"stable-provider","organizationType":"claude_max"}}"#).unwrap();
        let logins = store.load_logins(UPKEEP_NOW).unwrap().0;
        // Model a crash after the provider proof and durable checkpoint but
        // before the atomic binding/outcome transaction reached disk.
        accounts::write_renewal_checkpoint(
            &row.label,
            row.email.as_deref().unwrap(),
            staging.to_str().unwrap(),
            Some("max"),
            UPKEEP_NOW - 5,
            &logins,
        )
        .unwrap();
        let guard = std::sync::Arc::new(std::sync::Mutex::new(AutoRenewGuard::default()));
        assert!(recover_interrupted_staging_renewals(
            &store, &roster, &guard, UPKEEP_NOW
        ));
        let recovered = store.load_logins(UPKEEP_NOW).unwrap().0;
        let principal = super::super::profile::principal_id("stable-provider").unwrap();
        assert_eq!(recovered.shared.principal_source(principal), Some(source));
        let published = recovered.labels[&row.label].renewal_generation;
        assert_eq!(
            recovered.shared.latest(source).unwrap().outcome,
            accounts::shared::Outcome::Published {
                generation: published
            }
        );
        assert!(!recover_interrupted_staging_renewals(
            &store,
            &roster,
            &guard,
            UPKEEP_NOW + 1
        ));
        let mut retry = recovered.shared.latest(source).unwrap().intent.clone();
        retry.request = [6; 16];
        let mut state = recovered.shared.clone();
        assert!(state.begin(retry, None).is_err());
    }

    #[test]
    fn recover_interrupted_staging_renewals_adopts_completed_login_and_preserves_other_work() {
        let w = World::new();
        let store = accounts::Store::at(&w.state);
        let roster = Roster {
            rows: vec![
                Row {
                    label: "m27".to_owned(),
                    class: Class::Dedicated,
                    dir: Some(
                        store
                            .logins_dir()
                            .join("m27")
                            .to_string_lossy()
                            .into_owned(),
                    ),
                    was: Vec::new(),
                    vendor: Default::default(),
                    email: Some("sol-m27@example.test".to_owned()),
                    identity_email: None,
                    browser: None,
                    held_by: None,
                },
                Row {
                    label: "m27-2".to_owned(),
                    class: Class::Dedicated,
                    dir: Some(
                        store
                            .logins_dir()
                            .join("m27-2")
                            .to_string_lossy()
                            .into_owned(),
                    ),
                    was: Vec::new(),
                    vendor: Default::default(),
                    email: Some("sun.m27.2@example.test".to_owned()),
                    identity_email: None,
                    browser: None,
                    held_by: None,
                },
            ],
        };
        let aborted = store.claim_staging_dir("m27").unwrap();
        fs::write(
            aborted.join(".claude.json"),
            "{\"firstStartTime\":\"2026-10-22T23:54:34.139Z\",\"userID\":\"3bbbc341\"}\n",
        )
        .unwrap();
        let another = store.claim_staging_dir("m27-2").unwrap();
        fs::write(
            another.join(".claude.json"),
            r#"{"oauthAccount":{"emailAddress":"sun.m27.2@example.test","organizationType":"claude_pro"}}"#,
        )
        .unwrap();
        let completed = store.claim_staging_dir("m27-2").unwrap();
        fs::write(
            completed.join(".claude.json"),
            "{\"oauthAccount\":{\"emailAddress\":\"sun.m27.2@example.test\",\"organizationType\":\"claude_max\"}}\n",
        )
        .unwrap();
        // Synthetic crash point after the real writer, before binding. The
        // production status reader still guards the checkpoint's identity.
        let checked_at = UPKEEP_NOW - 5;
        let status = super::super::align::Captured {
            stdout: br#"{"loggedIn":true,"authMethod":"claude.ai","email":"sun.m27.2@example.test","subscriptionType":"max"}"#.to_vec(),
            code: Some(0),
        };
        let row = &roster.rows[1];
        let email = row.email.as_deref().unwrap();
        let facts = super::super::cli::status_facts(&status, email).unwrap();
        assert!(facts.renews());
        accounts::write_renewal_checkpoint(
            &row.label,
            email,
            completed.to_str().unwrap(),
            facts.subscription.as_deref(),
            checked_at,
            &store.load_logins(UPKEEP_NOW).unwrap().0,
        )
        .unwrap();
        assert!(completed.join(accounts::RENEWAL_CHECKPOINT_FILE).is_file());
        assert!(!store.state_file().exists());
        let malformed = store.claim_staging_dir("m27").unwrap();
        fs::write(malformed.join(".claude.json"), b"unfinished profile {").unwrap();
        fs::write(malformed.join("credentials"), b"keep these bytes").unwrap();
        for dir in [&aborted, &malformed, &another] {
            fs::OpenOptions::new()
                .write(true)
                .open(dir.join(".claude.json"))
                .unwrap()
                .set_times(std::fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH))
                .unwrap();
        }
        let unrelated = store.claim_staging_dir("m27").unwrap();
        fs::write(unrelated.join("owner-note"), b"unrelated work").unwrap();
        let wrong_account = store.claim_staging_dir("m27").unwrap();
        fs::write(
            wrong_account.join(".claude.json"),
            r#"{"oauthAccount":{"emailAddress":"unlisted@example.test"}}"#,
        )
        .unwrap();

        let guard = std::sync::Arc::new(std::sync::Mutex::new(AutoRenewGuard::default()));
        assert!(
            recover_interrupted_staging_renewals(&store, &roster, &guard, UPKEEP_NOW),
            "completed staging login for m27-2 must be recovered"
        );
        assert!(aborted.exists(), "an unfinished login is preserved");
        let (state, _) = store.load(UPKEEP_NOW).unwrap();
        let (logins, _) = store.load_logins(UPKEEP_NOW).unwrap();
        assert_eq!(
            state.accounts.get("m27-2").map(|a| a.auth),
            Some(accounts::Auth::Ok)
        );
        assert_eq!(
            logins.labels.get("m27-2").and_then(|l| l.dir.as_deref()),
            completed.to_str(),
            "the newest completed staging sequence is adopted before older credentials"
        );
        assert_eq!(
            logins
                .labels
                .get("m27-2")
                .and_then(|l| l.subscription.as_deref()),
            Some("max")
        );
        assert_eq!(logins.labels["m27-2"].renewed_at, Some(checked_at));
        assert_eq!(state.accounts["m27-2"].auth_at, Some(checked_at));

        assert!(
            !recover_interrupted_staging_renewals(&store, &roster, &guard, UPKEEP_NOW + 10),
            "already-recovered login is not recovered a second time"
        );
        assert!(
            aborted.exists(),
            "time does not grant permission to remove an unfinished login"
        );
        assert!(
            completed.exists(),
            "active enrolled staging directory is kept"
        );
        assert_eq!(
            fs::read(another.join(".claude.json")).unwrap(),
            br#"{"oauthAccount":{"emailAddress":"sun.m27.2@example.test","organizationType":"claude_pro"}}"#,
            "older completed credentials are retained"
        );
        assert_eq!(
            fs::read(malformed.join(".claude.json")).unwrap(),
            b"unfinished profile {"
        );
        assert_eq!(
            fs::read(malformed.join("credentials")).unwrap(),
            b"keep these bytes"
        );
        assert_eq!(
            fs::read(unrelated.join("owner-note")).unwrap(),
            b"unrelated work"
        );
        assert!(wrong_account.join(".claude.json").exists());
        assert!(
            !logins.labels.contains_key("m27"),
            "an unmatched email is not adopted"
        );

        assert!(!recover_interrupted_staging_renewals(
            &store,
            &roster,
            &guard,
            UPKEEP_NOW + 20,
        ));
        assert_eq!(store.load_logins(UPKEEP_NOW + 20).unwrap().0, logins);

        // Already-bound aliases and historical directories cannot be adopted
        // as a new renewal merely because the current account is signed out.
        accounts::write_renewal_checkpoint(
            "m27-2",
            "sun.m27.2@example.test",
            another.to_str().unwrap(),
            Some("max"),
            UPKEEP_NOW + 35,
            &store.load_logins(UPKEEP_NOW + 35).unwrap().0,
        )
        .unwrap();
        let alias = completed.join(".").to_string_lossy().into_owned();
        store
            .transact_logins(UPKEEP_NOW + 30, &mut None, |state, logins| {
                let binding = logins.labels.get_mut("m27-2").unwrap();
                binding.dir = Some(alias.clone());
                binding
                    .was
                    .push(Some(another.to_string_lossy().into_owned()));
                Ok(accounts::note_auth(
                    state,
                    "m27-2",
                    accounts::Auth::SignedOut,
                    UPKEEP_NOW + 30,
                ))
            })
            .unwrap();
        let before = store.load_logins(UPKEEP_NOW + 30).unwrap().0;
        assert!(!recover_interrupted_staging_renewals(
            &store,
            &roster,
            &guard,
            UPKEEP_NOW + 40,
        ));
        assert_eq!(store.load_logins(UPKEEP_NOW + 40).unwrap().0, before);
        assert_eq!(
            store.load(UPKEEP_NOW + 40).unwrap().0.accounts["m27-2"].auth,
            accounts::Auth::SignedOut
        );
    }

    #[test]
    fn held_account_selections_resume_the_latest_request_without_automatic_source_opt_in() {
        use std::sync::{Arc, Mutex};

        let w = World::new();
        w.write_config("[fleet]\nmachine = \"m27\"\n");
        let store = accounts::Store::at(&w.state);
        store.machine_id("0123456789abcdef").unwrap();
        let roster = Roster {
            rows: ["m27", "m27-2"]
                .into_iter()
                .map(|label| Row {
                    label: label.into(),
                    dir: Some(store.logins_dir().join(label).to_str().unwrap().into()),
                    email: Some(format!("{label}@example.test")),
                    ..Row::default()
                })
                .collect(),
        };
        fs::write(w.roster(), format_roster_toml(&roster).unwrap()).unwrap();
        let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
        type Work = Box<dyn FnOnce() + Send>;
        let queued: Arc<Mutex<Vec<Work>>> = Arc::new(Mutex::new(Vec::new()));
        let into_queue = Arc::clone(&queued);
        let spawn = move |work| {
            into_queue.lock().unwrap().push(work);
            Ok(())
        };
        let fetch = |_: &Env, _: &str| -> Result<String, String> {
            panic!("an explicit saved request must not fetch an automatic source")
        };
        let cooldown = Mutex::new(None);
        let fx = UpkeepEffects {
            fetch: &fetch,
            spawn_renewal: &spawn,
            quota_refresh: &|_, _, _, _| {},
            sync_cooldown: &cooldown,
            rediscovery: &Mutex::new(Rediscovery::default()),
            renew_guard: &guard,
            find_browser: &|_: &Path| false,
            vault_sync: |_: &Path| super::super::sign_in_ladder::Vault::NotMember,
            spawn_vault: &|_work| Ok(()),
            default_check: |_: &Path| crate::harness::cli::LoginCheck::CouldNotRun(String::new()),
            mailbox: &ladder_mailbox_files,
            log: std::sync::Arc::new(|_: &str| {}),
            run_attempt: None,
            shared: shared_renewal::Effects::default(),
        };
        let select = |label| match store
            .select_account(&roster, &w.roster(), label, UPKEEP_NOW)
            .unwrap()
        {
            accounts::PinSelection::Renew(request) => request,
            accounts::PinSelection::Pinned => panic!("the fixture login is absent"),
        };
        let first = select("m27");
        let mut held = AutoRenewPermit::claim(&guard, "other", Instant::now(), false).unwrap();
        held.wake_pending = Some(w.state.clone());
        assert_eq!(
            renew_requested_pin(
                &w.home,
                &w.state,
                &w.roster(),
                first.clone(),
                UPKEEP_NOW,
                &guard,
                &spawn
            ),
            RenewalDispatch::Held
        );
        assert!(queued.lock().unwrap().is_empty());
        assert_eq!(
            store.load(UPKEEP_NOW).unwrap().0.pending_pin,
            Some(first.clone())
        );

        let latest = select("m27-2");
        assert_eq!(
            renew_requested_pin(
                &w.home,
                &w.state,
                &w.roster(),
                first.clone(),
                UPKEEP_NOW,
                &guard,
                &spawn
            ),
            RenewalDispatch::Superseded
        );
        drop(held);
        assert!(!guard.lock().unwrap().in_flight);
        assert!(store.wake_path().is_file());
        assert_eq!(
            upkeep(
                &w.home,
                &w.state,
                (&w.roster(), UPKEEP_NOW, UpkeepStep::Renew),
                &fx
            ),
            Upkept::Unconfigured {
                fleet_file_ignored: false,
                renewal: true,
            },
            "the saved selection progressed with no source configured"
        );
        assert_eq!(queued.lock().unwrap().len(), 1);
        {
            let admitted = guard.lock().unwrap();
            assert!(admitted.in_flight);
            assert!(admitted.last_attempt.contains_key("m27-2"));
            assert!(!admitted.last_attempt.contains_key("m27"));
        }
        store.cancel_pin_request(&first, UPKEEP_NOW).unwrap();
        assert_eq!(
            store.load(UPKEEP_NOW).unwrap().0.pending_pin,
            Some(latest.clone())
        );

        // Dropping injected work releases admission without executing authentication.
        queued.lock().unwrap().clear();
        assert!(!guard.lock().unwrap().in_flight);
        assert_eq!(
            spawn_row_renewal(
                accounts::Cli::Claude,
                &w.home,
                &w.state,
                &w.roster(),
                roster.clone(),
                &accounts::Logins::default(),
                roster.rows[1].clone(),
                (None, true),
                UPKEEP_NOW,
                &guard,
                &spawn,
                None,
            ),
            RenewalDispatch::Held,
            "ordinary interactive rotation still observes the per-label cooldown"
        );
        assert!(queued.lock().unwrap().is_empty());
        let refusal =
            |_work: Box<dyn FnOnce() + Send>| Err(io::Error::other("injected spawn refusal"));
        let refused_fx = UpkeepEffects {
            spawn_renewal: &refusal,
            ..fx
        };
        assert_eq!(
            resume_pending_pin(&w.home, &w.state, &w.roster(), UPKEEP_NOW, &refused_fx),
            None,
            "a failed startup leaves no pending choice to block the rest of upkeep"
        );
        assert!(store.load(UPKEEP_NOW).unwrap().0.pending_pin.is_none());
        assert!(!guard.lock().unwrap().in_flight);

        let final_request = select("m27");
        let login = Path::new(roster.rows[0].dir.as_deref().unwrap());
        fs::create_dir_all(login).unwrap();
        fs::write(login.join(".credentials.json"), "private fixture").unwrap();
        assert_eq!(
            resume_pending_pin(&w.home, &w.state, &w.roster(), UPKEEP_NOW, &refused_fx),
            Some(RenewalDispatch::Recovered)
        );
        let pinned = store.load(UPKEEP_NOW).unwrap().0;
        assert_eq!(pinned.active.as_deref(), Some("m27"));
        assert!(pinned.pinned);
        assert!(pinned.pending_pin.is_none());
        store
            .cancel_pin_request(&final_request, UPKEEP_NOW)
            .unwrap();
        assert_eq!(store.load(UPKEEP_NOW).unwrap().0, pinned);

        let retry = store.request_pin("m27", UPKEEP_NOW).unwrap();
        let before_fault = store.load(UPKEEP_NOW).unwrap().0;
        let ledger = store.ledger_path();
        let retained = ledger.with_extension("retained");
        fs::rename(&ledger, &retained).unwrap();
        fs::create_dir(&ledger).unwrap();
        assert_eq!(
            resume_pending_pin(&w.home, &w.state, &w.roster(), UPKEEP_NOW, &refused_fx),
            Some(RenewalDispatch::Fenced)
        );
        assert_eq!(store.load(UPKEEP_NOW).unwrap().0, before_fault);
        assert_eq!(before_fault.pending_pin.as_ref(), Some(&retry));
        fs::rename(&ledger, ledger.with_extension("obstruction")).unwrap();
        fs::rename(&retained, &ledger).unwrap();
        assert_eq!(
            resume_pending_pin(&w.home, &w.state, &w.roster(), UPKEEP_NOW, &refused_fx),
            Some(RenewalDispatch::Recovered)
        );
        assert!(store.load(UPKEEP_NOW).unwrap().0.pending_pin.is_none());
    }

    /// A PERSON'S PIN OF A CLAUDE CODE ROW WAITS OUT ANOTHER CLI'S SIGN-IN: while a vendor's
    /// sign-in (another process's, so this one's admission is free) holds the machine's sign-in
    /// lock, the pin's renewal is Held before anything is admitted, its request stays pending
    /// for a later upkeep, no cooldown starts and no work is queued. CONTROL: once the lock is
    /// free the same request starts its renewal (dropped here unrun), holding the lock itself.
    #[test]
    fn a_vendor_sign_in_holding_the_lock_keeps_a_claude_pin_request_pending() {
        use std::sync::{Arc, Mutex};

        let w = World::new();
        w.write_config("[fleet]\nmachine = \"m27\"\n");
        let store = accounts::Store::at(&w.state);
        store.machine_id("0123456789abcdef").unwrap();
        let roster = Roster {
            rows: ["m27", "m27-2"]
                .into_iter()
                .map(|label| Row {
                    label: label.into(),
                    dir: Some(store.logins_dir().join(label).to_str().unwrap().into()),
                    email: Some(format!("{label}@example.test")),
                    ..Row::default()
                })
                .collect(),
        };
        fs::write(w.roster(), format_roster_toml(&roster).unwrap()).unwrap();
        let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
        type Work = Box<dyn FnOnce() + Send>;
        let queued: Arc<Mutex<Vec<Work>>> = Arc::new(Mutex::new(Vec::new()));
        let into_queue = Arc::clone(&queued);
        let spawn = move |work| {
            into_queue.lock().unwrap().push(work);
            Ok(())
        };
        let request = match store
            .select_account(&roster, &w.roster(), "m27", UPKEEP_NOW)
            .unwrap()
        {
            accounts::PinSelection::Renew(request) => request,
            accounts::PinSelection::Pinned => panic!("the fixture login is absent"),
        };
        let kimi = accounts::Store::at_for(&w.state, accounts::Cli::Kimi, &w.home);
        let vendor = kimi
            .signin_now()
            .unwrap()
            .expect("the vendor's sign-in takes the lock");
        assert!(
            store.signin_now().unwrap().is_none(),
            "the lock is held against this store too"
        );
        let renew = || {
            renew_requested_pin(
                &w.home,
                &w.state,
                &w.roster(),
                request.clone(),
                UPKEEP_NOW,
                &guard,
                &spawn,
            )
        };
        assert_eq!(renew(), RenewalDispatch::Held);
        assert!(queued.lock().unwrap().is_empty(), "nothing queued");
        assert_eq!(
            store.load(UPKEEP_NOW).unwrap().0.pending_pin,
            Some(request.clone()),
            "the person's pin stays pending"
        );
        {
            let admitted = guard.lock().unwrap();
            assert!(!admitted.in_flight);
            assert!(!admitted.last_attempt.contains_key("m27"), "no cooldown");
        }

        drop(vendor);
        assert_eq!(renew(), RenewalDispatch::Started, "CONTROL");
        assert_eq!(queued.lock().unwrap().len(), 1);
        assert!(
            kimi.signin_now().unwrap().is_none(),
            "CONTROL: the queued renewal holds the lock"
        );
        queued.lock().unwrap().clear();
        assert!(!guard.lock().unwrap().in_flight);
        assert!(
            kimi.signin_now().unwrap().is_some(),
            "released with its work"
        );
        store.cancel_pin_request(&request, UPKEEP_NOW).unwrap();
    }

    #[test]
    fn recovering_another_login_keeps_a_pending_pin_until_its_account_is_eligible() {
        let w = World::new();
        let store = accounts::Store::at(&w.state);
        store.machine_id("0123456789abcdef").unwrap();
        let roster = Roster {
            rows: [
                ("m27", "sol-m27@example.test"),
                ("m27-2", "sun.m27.2@example.test"),
            ]
            .into_iter()
            .map(|(label, email)| Row {
                label: label.into(),
                class: Class::Dedicated,
                dir: Some(store.logins_dir().join(label).to_str().unwrap().into()),
                was: Vec::new(),
                vendor: Default::default(),
                email: Some(email.into()),
                identity_email: None,
                browser: None,
                held_by: None,
            })
            .collect(),
        };
        let request = store.request_pin("m27", UPKEEP_NOW).unwrap();
        let staging = store.claim_staging_dir("m27-2").unwrap();
        fs::write(staging.join(".credentials.json"), b"private login fixture").unwrap();
        accounts::write_renewal_checkpoint(
            "m27-2",
            "sun.m27.2@example.test",
            staging.to_str().unwrap(),
            Some("max"),
            UPKEEP_NOW - 5,
            &store.load_logins(UPKEEP_NOW).unwrap().0,
        )
        .unwrap();
        let guard = std::sync::Arc::new(std::sync::Mutex::new(AutoRenewGuard::default()));
        assert!(recover_interrupted_staging_renewals(
            &store, &roster, &guard, UPKEEP_NOW
        ));
        assert_eq!(
            store.load_logins(UPKEEP_NOW).unwrap().0.labels["m27-2"]
                .dir
                .as_deref(),
            staging.to_str()
        );

        let complete = |request: &accounts::PinRequest, now| {
            let bound = store.bound(&roster, now).unwrap();
            let enrolled = accounts::enrolled(&bound);
            store
                .transact_within(&accounts::LOCK_LADDER, now, &mut None, |state| {
                    Ok(
                        accounts::complete_requested_pin(state, &bound, &enrolled, request, now)
                            .into_iter()
                            .collect(),
                    )
                })
                .unwrap()
        };
        let before = store.load(UPKEEP_NOW).unwrap().0;
        assert_eq!(before.pending_pin.as_ref(), Some(&request));
        assert!(complete(&request, UPKEEP_NOW).is_empty());
        assert_eq!(
            accounts::Store::at(&w.state).load(UPKEEP_NOW).unwrap().0,
            before,
            "recovering m27-2 cannot consume the still-unenrolled m27 selection"
        );

        let requested_home = Path::new(roster.rows[0].dir.as_deref().unwrap());
        fs::create_dir_all(requested_home).unwrap();
        fs::write(
            requested_home.join(".credentials.json"),
            b"private login fixture",
        )
        .unwrap();
        store
            .transact(UPKEEP_NOW + 1, &mut None, |state| {
                Ok(accounts::note_auth(
                    state,
                    "m27",
                    accounts::Auth::SignedOut,
                    UPKEEP_NOW + 1,
                ))
            })
            .unwrap();
        let signed_out = store.load(UPKEEP_NOW + 1).unwrap().0;
        assert!(complete(&request, UPKEEP_NOW + 1).is_empty());
        assert_eq!(
            accounts::Store::at(&w.state)
                .load(UPKEEP_NOW + 1)
                .unwrap()
                .0,
            signed_out,
            "a signed-out account also keeps its pending request"
        );
        store
            .transact(UPKEEP_NOW + 2, &mut None, |state| {
                Ok(accounts::note_auth(
                    state,
                    "m27",
                    accounts::Auth::Ok,
                    UPKEEP_NOW + 2,
                ))
            })
            .unwrap();
        let events = complete(&request, UPKEEP_NOW + 2);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, accounts::Kind::Use);
        let pinned = store.load(UPKEEP_NOW + 2).unwrap().0;
        assert_eq!(pinned.active.as_deref(), Some("m27"));
        assert!(pinned.pinned);
        assert!(pinned.pending_pin.is_none());
        assert!(complete(&request, UPKEEP_NOW + 2).is_empty());
        assert_eq!(store.load(UPKEEP_NOW + 2).unwrap().0, pinned);

        let newer = store.request_pin("m27-2", UPKEEP_NOW + 3).unwrap();
        let superseded = store.load(UPKEEP_NOW + 3).unwrap().0;
        assert!(complete(&request, UPKEEP_NOW + 3).is_empty());
        assert_eq!(store.load(UPKEEP_NOW + 3).unwrap().0, superseded);
        let events = complete(&newer, UPKEEP_NOW + 3);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, accounts::Kind::Use);
        let pinned = accounts::Store::at(&w.state)
            .load(UPKEEP_NOW + 3)
            .unwrap()
            .0;
        assert_eq!(pinned.active.as_deref(), Some("m27-2"));
        assert!(pinned.pinned);
        assert!(pinned.pending_pin.is_none());
    }

    #[test]
    fn sync_use_and_renew_due_honor_bound_staging_logins_and_recover_checkpoints() {
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        let home = root.join("home");
        let cfg_dir = home.join(".config").join("aterm");
        let state_dir = root.join("state");
        fs::create_dir_all(&cfg_dir).unwrap();
        fs::write(cfg_dir.join("aterm.toml"), "[fleet]\nmachine = \"m27\"\n").unwrap();
        let source_path = root.join("accounts.csv");
        fs::write(
            &source_path,
            concat!(
                "email,computer,claude,notes\n",
                "sol-m27@example.test,m27,1,usable\n",
                "sun.m27.2@example.test,m27,1,usable\n",
                "solar.m27.3@example.test,m27,1,usable\n",
            ),
        )
        .unwrap();

        let store = accounts::Store::at(&state_dir);
        store
            .transact(UPKEEP_NOW - 20, &mut None, |state| {
                Ok(accounts::note_auth(
                    state,
                    "m27",
                    accounts::Auth::SignedOut,
                    UPKEEP_NOW - 20,
                ))
            })
            .unwrap();
        let staged_m27 = store.claim_staging_dir("m27").unwrap();
        fs::write(staged_m27.join(".credentials.json"), b"{}").unwrap();
        accounts::write_renewal_checkpoint(
            "m27",
            "sol-m27@example.test",
            staged_m27.to_str().unwrap(),
            Some("max"),
            UPKEEP_NOW - 10,
            &store.load_logins(UPKEEP_NOW).unwrap().0,
        )
        .unwrap();

        let env = Env {
            state: state_dir.clone(),
            aterm_state: Some(root.join("aterm-state")),
            cwd: root.to_path_buf(),
            home: Some(home.clone()),
            claude_dir: Some(home.join(".claude")),
            claude_config_unusable: None,
            claude_profile: Some(home.join(".claude.json")),
            claude_custom_oauth: false,
            claude_session: None,
            claude_pid: None,
            codex_thread: None,
            agy_conversation: None,
            codex_home: None,
            codex_login: None,
            now: UPKEEP_NOW,
            tz_s: 0,
            sid: String::new(),
            sock: None,
            config: Some(cfg_dir.join("aterm.toml")),
            claude_tmp: None,
            claude_running: None,
        };
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run_accounts_sync(
            &env,
            Some(source_path.to_str().unwrap()),
            Some("m27"),
            true,
            &mut out,
            &mut err,
        );
        assert_eq!(
            code,
            ExitCode::SUCCESS,
            "stderr: {}",
            String::from_utf8_lossy(&err)
        );
        let sync_json: Value =
            aterm_json::from_str(&String::from_utf8_lossy(&out)).expect("sync json");
        let m27_entry = sync_json["machine_accounts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["label"].as_str() == Some("m27"))
            .expect("m27 row in sync output");
        assert_eq!(m27_entry["enrolled"].as_bool(), Some(true));
        assert_eq!(m27_entry["condition"].as_str(), Some("usable"));
        let needs_renew: Vec<&str> = sync_json["needs_renew"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert!(
            !needs_renew.contains(&"m27"),
            "recovered and bound staging login m27 must not be in needs_renew: {needs_renew:?}"
        );

        // `accounts use m27` succeeds even though `logins/m27` is empty and the login is in `m27+1`.
        out.clear();
        err.clear();
        let use_code = super::super::cli::run_accounts(
            &super::super::cli::AccountsCmd::Use {
                label: "m27".to_owned(),
            },
            &env,
            &super::super::cli::LiveAccounts,
            &mut out,
            &mut err,
        );
        assert_eq!(
            use_code,
            ExitCode::SUCCESS,
            "accounts use m27 failed: {}",
            String::from_utf8_lossy(&err)
        );

        // Now write a staging checkpoint for m27-2 while m27-3 is limited and m27 is limited,
        // and verify `accounts renew --due` recovers m27-2 without needing an interactive renew flow.
        store
            .transact(UPKEEP_NOW, &mut None, |st| {
                let mut ev = accounts::note_wall(
                    st,
                    "m27-3",
                    "seven_day",
                    Some(UPKEEP_NOW + 3600),
                    UPKEEP_NOW,
                    UPKEEP_NOW,
                );
                ev.extend(accounts::note_wall(
                    st,
                    "m27",
                    "seven_day",
                    Some(UPKEEP_NOW + 3600),
                    UPKEEP_NOW,
                    UPKEEP_NOW,
                ));
                Ok(ev)
            })
            .unwrap();
        let staged_m27_2 = store.claim_staging_dir("m27-2").unwrap();
        fs::write(staged_m27_2.join(".credentials.json"), b"{}").unwrap();
        accounts::write_renewal_checkpoint(
            "m27-2",
            "sun.m27.2@example.test",
            staged_m27_2.to_str().unwrap(),
            Some("max"),
            UPKEEP_NOW,
            &store.load_logins(UPKEEP_NOW).unwrap().0,
        )
        .unwrap();
        out.clear();
        err.clear();
        let renew_code = super::super::cli::run_accounts(
            &super::super::cli::AccountsCmd::Renew {
                labels: Vec::new(),
                due: true,
                on: None,
            },
            &env,
            &super::super::cli::LiveAccounts,
            &mut out,
            &mut err,
        );
        assert_eq!(
            renew_code,
            ExitCode::SUCCESS,
            "renew --due failed: {}",
            String::from_utf8_lossy(&err)
        );
        assert!(
            String::from_utf8_lossy(&out).contains("nothing is due on this machine"),
            "stdout: {}",
            String::from_utf8_lossy(&out)
        );
        let (logins_after, _) = store.load_logins(UPKEEP_NOW).unwrap();
        assert_eq!(
            logins_after.get("m27-2").and_then(|b| b.dir.as_deref()),
            staged_m27_2.to_str(),
            "renew --due should recover interrupted staging checkpoint for m27-2"
        );

        // Verify `discover_local_fleet` finds emails from `<store>/logins/` checkpoints and `.claude.json`.
        fs::write(
            home.join(".claude.json"),
            r#"{"oauthAccount":{"emailAddress":"solar.m27.3@example.test"}}"#,
        )
        .unwrap();
        let discovered = discover_local_fleet(&env).expect("discover local fleet from logins_dir");
        let discovered_emails: BTreeSet<&str> = discovered
            .accounts
            .iter()
            .map(|a| a.email.as_str())
            .collect();
        assert!(discovered_emails.contains("sol-m27@example.test"));
        assert!(discovered_emails.contains("sun.m27.2@example.test"));
        assert!(discovered_emails.contains("solar.m27.3@example.test"));

        // Mark default row `m27-3` as SignedOut, then re-sync: `SignedOut` must not be overwritten
        // merely because `~/.claude.json` still contains `solar.m27.3@example.test`.
        store
            .transact_logins(UPKEEP_NOW + 5, &mut None, |st, lg| {
                Ok(accounts::note_login_end(st, lg, "m27-3", UPKEEP_NOW + 5))
            })
            .unwrap();
        out.clear();
        err.clear();
        assert_eq!(
            run_accounts_sync(
                &env,
                Some(source_path.to_str().unwrap()),
                Some("m27"),
                false,
                &mut out,
                &mut err,
            ),
            ExitCode::SUCCESS
        );
        let (state_resynced, _) = store.load(UPKEEP_NOW + 5).unwrap();
        assert_eq!(
            state_resynced.accounts["m27-3"].auth,
            accounts::Auth::SignedOut,
            "re-syncing must not overwrite a SignedOut default row with Auth::Ok"
        );
    }

    #[test]
    fn automatic_attempt_summary_refuses_a_replacement_binding() {
        for changed in [false, true] {
            let w = World::new();
            let store = accounts::Store::at(&w.state);
            let path = store.login_dir("main").unwrap();
            let raw = Roster {
                rows: vec![Row {
                    label: "main".into(),
                    email: Some("main@example.test".into()),
                    dir: Some(path.to_string_lossy().into_owned()),
                    ..Row::default()
                }],
            };
            fs::create_dir_all(&w.config).unwrap();
            fs::write(w.roster(), format_roster_toml(&raw).unwrap()).unwrap();
            let renew = || {
                store
                    .transact_logins(UPKEEP_NOW, &mut None, |state, logins| {
                        Ok(accounts::note_renewed(
                            state,
                            logins,
                            "main",
                            path.to_str(),
                            path.to_str().unwrap(),
                            None,
                            UPKEEP_NOW,
                        ))
                    })
                    .unwrap()
            };
            renew();
            let captured = store.load_logins(UPKEEP_NOW).unwrap().0;
            let captured_row = accounts::bound(&raw, &captured)
                .row("main")
                .unwrap()
                .clone();
            if changed {
                renew();
            }
            let files = [
                store.state_file(),
                store.logins_file(),
                store.ledger_path(),
                store.wake_path(),
            ];
            let before = files.each_ref().map(|path| fs::read(path).ok());
            note_attempt(
                &store,
                (&w.roster(), &captured_row, &captured),
                accounts::AttemptOutcome::Failed,
                "fixture route",
                UPKEEP_NOW,
            );
            if changed {
                assert_eq!(files.each_ref().map(|path| fs::read(path).ok()), before);
            } else {
                assert_eq!(
                    store
                        .load_logins(UPKEEP_NOW)
                        .unwrap()
                        .0
                        .get("main")
                        .unwrap()
                        .sign_in_attempt
                        .as_ref()
                        .unwrap()
                        .outcome,
                    accounts::AttemptOutcome::Failed
                );
            }
        }
    }

    #[test]
    fn automatic_completion_checks_the_generation_of_the_account_it_actually_matches() {
        for changed in [None, Some("main"), Some("other")] {
            let w = World::new();
            let store = accounts::Store::at(&w.state);
            let raw = Roster {
                rows: [
                    ("main", "first@example.test"),
                    ("other", "matched@example.test"),
                ]
                .into_iter()
                .map(|(label, email)| Row {
                    label: label.into(),
                    email: Some(email.into()),
                    dir: Some(store.login_dir(label).unwrap().to_str().unwrap().into()),
                    ..Default::default()
                })
                .collect(),
            };
            let path = w.roster();
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, format_roster_toml(&raw).unwrap()).unwrap();
            store
                .transact_logins(UPKEEP_NOW - 10, &mut None, |state, logins| {
                    let mut events = Vec::new();
                    for row in &raw.rows {
                        events.extend(accounts::note_renewed(
                            state,
                            logins,
                            &row.label,
                            row.dir.as_deref(),
                            row.dir.as_deref().unwrap(),
                            None,
                            UPKEEP_NOW - 10,
                        ));
                    }
                    Ok(events)
                })
                .unwrap();
            let captured_logins = store.load_logins(UPKEEP_NOW).unwrap().0;
            let captured_rows = accounts::bound(&raw, &captured_logins);
            if let Some(label) = changed {
                store
                    .transact_logins(UPKEEP_NOW - 10, &mut None, |state, logins| {
                        let old = logins.get(label).unwrap().clone();
                        let events = accounts::note_renewed(
                            state,
                            logins,
                            label,
                            old.dir.as_deref(),
                            old.dir.as_deref().unwrap(),
                            None,
                            UPKEEP_NOW - 10,
                        );
                        logins.labels.get_mut(label).unwrap().was = old.was;
                        Ok(events)
                    })
                    .unwrap();
            }
            assert_eq!(
                accounts::bound(&raw, &store.load_logins(UPKEEP_NOW).unwrap().0),
                captured_rows
            );
            let staging = store.claim_staging_dir("main").unwrap();
            fs::write(staging.join(".claude.json"), b"verified alternate account").unwrap();
            let files = [
                store.state_file(),
                store.logins_file(),
                store.ledger_path(),
                store.wake_path(),
            ];
            let before = files.each_ref().map(|path| fs::read(path).ok());
            let status = super::super::align::Captured {
                stdout: br#"{"loggedIn":true,"authMethod":"claude.ai","email":"matched@example.test","subscriptionType":"max"}"#.to_vec(),
                code: Some(0),
            };
            let outcome = complete_staging_renewal_result(
                &store,
                accounts::RenewalPolicy {
                    roster_path: &path,
                    roster: &raw,
                    row: captured_rows.row("main").unwrap(),
                    logins: &captured_logins,
                },
                &staging,
                staging.to_str().unwrap(),
                (&status, Some("fixture route")),
                UPKEEP_NOW,
                &|_| {},
            );
            if changed.is_none() {
                assert_eq!(outcome, super::super::cli::Renewal::OtherRenewed);
                assert_eq!(
                    store
                        .load_logins(UPKEEP_NOW)
                        .unwrap()
                        .0
                        .get("main")
                        .unwrap()
                        .sign_in_attempt
                        .as_ref()
                        .unwrap()
                        .outcome,
                    accounts::AttemptOutcome::Failed,
                );
                assert_eq!(
                    store
                        .load_logins(UPKEEP_NOW)
                        .unwrap()
                        .0
                        .get("other")
                        .unwrap()
                        .dir
                        .as_deref(),
                    staging.to_str()
                );
            } else {
                assert_eq!(outcome, super::super::cli::Renewal::Failed);
                assert_eq!(files.each_ref().map(|path| fs::read(path).ok()), before);
                assert!(!staging.join(accounts::RENEWAL_CHECKPOINT_FILE).exists());
            }
            assert_eq!(
                fs::read(staging.join(".claude.json")).unwrap(),
                b"verified alternate account"
            );
        }
    }

    #[test]
    fn automatic_completion_refuses_removed_policy_without_recoverable_evidence() {
        for email in ["owner@example.test", "foreign@example.test"] {
            let w = World::new();
            let store = accounts::Store::at(&w.state);
            let raw = Roster {
                rows: vec![Row {
                    label: "main".into(),
                    email: Some("owner@example.test".into()),
                    ..Row::default()
                }],
            };
            let roster_path = w.roster();
            fs::create_dir_all(roster_path.parent().unwrap()).unwrap();
            fs::write(&roster_path, format_roster_toml(&raw).unwrap()).unwrap();
            let captured = raw.row("main").unwrap();
            let staging = store.claim_staging_dir("main").unwrap();
            fs::write(staging.join(".claude.json"), b"completed private login").unwrap();
            fs::write(&roster_path, "").unwrap();
            let status = super::super::align::Captured {
                stdout: format!(
                    r#"{{"loggedIn":true,"authMethod":"claude.ai","email":"{email}"}}"#
                )
                .into_bytes(),
                code: Some(0),
            };
            assert_eq!(
                complete_staging_renewal_result(
                    &store,
                    accounts::RenewalPolicy {
                        roster_path: &roster_path,
                        roster: &raw,
                        row: captured,
                        logins: &accounts::Logins::default(),
                    },
                    &staging,
                    staging.to_str().unwrap(),
                    (&status, None),
                    UPKEEP_NOW,
                    &|_| panic!("removed policy cannot publish"),
                ),
                super::super::cli::Renewal::Failed
            );
            assert!(!store.logins_file().exists());
            assert!(!store.state_file().exists());
            assert!(!staging.join(accounts::RENEWAL_CHECKPOINT_FILE).exists());
            assert_eq!(
                fs::read(staging.join(".claude.json")).unwrap(),
                b"completed private login"
            );
        }
    }

    #[test]
    fn staging_completion_failures_stop_fallback_and_keep_verified_logins() {
        use super::super::cli::Renewal;

        for case in [
            "checkpoint",
            "ledger",
            "missing-snapshot",
            "wrong-account",
            "signed-out",
            "interrupted-status",
        ] {
            let dir = aterm_tempfile::Builder::new()
                .prefix("aterm-renewal-completion-")
                .tempdir()
                .unwrap();
            let store = accounts::Store::at(dir.path());
            store.machine_id("0123456789abcdef").unwrap();
            let roster = Roster {
                rows: vec![Row {
                    label: "main".to_owned(),
                    email: Some("owner@example.test".to_owned()),
                    ..Row::default()
                }],
            };
            let roster_path = dir.path().join(accounts::ACCOUNTS_FILE);
            fs::write(&roster_path, format_roster_toml(&roster).unwrap()).unwrap();
            let staging = store.claim_staging_dir("main").unwrap();
            let profile = b"private verified login fixture";
            fs::write(staging.join(".claude.json"), profile).unwrap();
            let checkpoint = staging.join(accounts::RENEWAL_CHECKPOINT_FILE);
            if case == "checkpoint" {
                fs::write(&checkpoint, b"unrecognized private work").unwrap();
            } else if case == "ledger" {
                let mut state = accounts::State::default();
                accounts::note_auth(
                    &mut state,
                    "main",
                    accounts::Auth::SignedOut,
                    UPKEEP_NOW - 1,
                );
                store.save(&state).unwrap();
                fs::create_dir(store.ledger_path()).unwrap();
            } else if case == "missing-snapshot" {
                fs::create_dir(store.ledger_path()).unwrap();
            }
            let status = super::super::align::Captured {
                stdout: if case == "signed-out" {
                    br#"{"loggedIn":false,"authMethod":"none"}"#.to_vec()
                } else {
                    let email = if case == "wrong-account" {
                        "foreign@example.test"
                    } else {
                        "owner@example.test"
                    };
                    format!(
                        r#"{{"loggedIn":true,"authMethod":"claude.ai","email":"{email}","subscriptionType":"max"}}"#
                    )
                    .into_bytes()
                },
                code: if case == "interrupted-status" {
                    None
                } else {
                    Some(if case == "signed-out" { 1 } else { 0 })
                },
            };
            let callbacks = std::cell::Cell::new(0);
            let outcome = complete_staging_renewal_result(
                &store,
                accounts::RenewalPolicy {
                    roster_path: &roster_path,
                    roster: &roster,
                    row: roster.row("main").unwrap(),
                    logins: &accounts::Logins::default(),
                },
                &staging,
                staging.to_str().unwrap(),
                (&status, None),
                UPKEEP_NOW,
                &|_| callbacks.set(callbacks.get() + 1),
            );
            assert_eq!(callbacks.get(), 0, "{case}: refused completion callback");
            match case {
                "checkpoint" | "ledger" | "missing-snapshot" => {
                    assert_eq!(outcome, Renewal::Failed);
                    assert!(!outcome.can_retry_authentication());
                    assert_eq!(fs::read(staging.join(".claude.json")).unwrap(), profile);
                    if case == "checkpoint" {
                        assert_eq!(fs::read(&checkpoint).unwrap(), b"unrecognized private work");
                        assert!(!store.logins_file().exists() && !store.state_file().exists());
                        assert!(store.tail(10).unwrap().is_empty());
                    } else if case == "missing-snapshot" {
                        assert!(!store.logins_file().exists() && !store.state_file().exists());
                        assert!(store.ledger_path().is_dir());
                    } else {
                        let binding = store.load_logins(UPKEEP_NOW).unwrap().0;
                        assert!(binding.get("main").is_none());
                        assert_eq!(
                            store.load(UPKEEP_NOW).unwrap().0.account("main").auth,
                            accounts::Auth::SignedOut
                        );
                        let proof = accounts::read_renewal_checkpoint(
                            &staging,
                            &fs::canonicalize(&staging).unwrap(),
                            UPKEEP_NOW,
                        )
                        .unwrap();
                        assert_eq!(proof.dir, staging.to_str().unwrap());
                        assert_eq!(proof.checked_at, UPKEEP_NOW);
                        assert!(store.ledger_path().is_dir());
                        fs::rename(
                            store.ledger_path(),
                            dir.path().join("retained-ledger-obstruction"),
                        )
                        .unwrap();
                        assert!(recover_interrupted_staging_renewals_with(
                            Cli::Claude,
                            &store,
                            &roster_path,
                            &std::sync::Arc::new(std::sync::Mutex::new(AutoRenewGuard::default())),
                            UPKEEP_NOW,
                            &[],
                            || {},
                        ));
                        assert_eq!(
                            store
                                .load_logins(UPKEEP_NOW)
                                .unwrap()
                                .0
                                .get("main")
                                .unwrap()
                                .dir
                                .as_deref(),
                            staging.to_str()
                        );
                        assert_eq!(
                            store.load(UPKEEP_NOW).unwrap().0.account("main").auth,
                            accounts::Auth::Ok
                        );
                        assert_eq!(fs::read(staging.join(".claude.json")).unwrap(), profile);
                    }
                }
                "wrong-account" => {
                    assert_eq!(outcome, Renewal::WrongAccount);
                    assert!(outcome.can_retry_authentication());
                    assert!(!staging.exists());
                    let bindings = store.load_logins(UPKEEP_NOW).unwrap().0;
                    let binding = bindings.get("main").unwrap();
                    assert_eq!(binding.dir, None);
                    assert_eq!(binding.wrong_at, Some(UPKEEP_NOW));
                }
                "interrupted-status" => {
                    assert_eq!(outcome, Renewal::Failed);
                    assert!(!outcome.can_retry_authentication());
                    assert!(!staging.exists());
                    assert!(!store.logins_file().exists() && !store.state_file().exists());
                }
                _ => {
                    assert_eq!(outcome, Renewal::AuthFailed);
                    assert!(outcome.can_retry_authentication());
                    assert!(!staging.exists());
                    assert!(!store.logins_file().exists() && !store.state_file().exists());
                }
            }
        }
    }

    #[test]
    fn complete_staging_renewal_records_wrong_account_and_auto_renew_advances_past_cooldown_or_unconfigured_row()
     {
        let w = World::new();
        let store = accounts::Store::at(&w.state);
        let m27_dir = store.logins_dir().join("m27");
        fs::create_dir_all(&m27_dir).unwrap();
        fs::write(
            m27_dir.join(".claude.json"),
            "{\"oauthAccount\":{\"emailAddress\":\"sol-m27@example.test\"}}\n",
        )
        .unwrap();

        let roster = Roster {
            rows: vec![
                Row {
                    label: "m27".to_owned(),
                    class: Class::Dedicated,
                    dir: Some(m27_dir.to_string_lossy().into_owned()),
                    was: Vec::new(),
                    vendor: Default::default(),
                    email: Some("sol-m27@example.test".to_owned()),
                    identity_email: None,
                    browser: None,
                    held_by: None,
                },
                Row {
                    label: "m27-2".to_owned(),
                    class: Class::Dedicated,
                    dir: Some(
                        store
                            .logins_dir()
                            .join("m27-2")
                            .to_string_lossy()
                            .into_owned(),
                    ),
                    was: Vec::new(),
                    vendor: Default::default(),
                    email: Some("sun.m27.2@example.test".to_owned()),
                    identity_email: None,
                    browser: None,
                    held_by: None,
                },
                Row {
                    label: "m27-3".to_owned(),
                    class: Class::Dedicated,
                    dir: Some(
                        store
                            .logins_dir()
                            .join("m27-3")
                            .to_string_lossy()
                            .into_owned(),
                    ),
                    was: Vec::new(),
                    vendor: Default::default(),
                    email: Some("star.m27.3@example.test".to_owned()),
                    identity_email: None,
                    browser: Some("Claude m27-3".to_owned()),
                    held_by: None,
                },
            ],
        };
        let mut roster_toml = String::new();
        for r in &roster.rows {
            roster_toml.push_str(&accounts::row_text(r).unwrap());
            roster_toml.push('\n');
        }
        fs::create_dir_all(&w.config).unwrap();
        fs::write(w.roster(), &roster_toml).unwrap();

        let mut note = None;
        store
            .transact_logins(UPKEEP_NOW, &mut note, |state, _logins| {
                let mut ev = accounts::note_auth(state, "m27", accounts::Auth::Ok, UPKEEP_NOW);
                let acc = state.accounts.entry("m27".to_owned()).or_default();
                acc.sample = Some(accounts::Sample {
                    session_pct: Some(95),
                    session_reset: Some(UPKEEP_NOW + 3600),
                    week_pct: Some(50),
                    week_reset: Some(UPKEEP_NOW + 86_400),
                    at: UPKEEP_NOW,
                    by: "m27".to_owned(),
                    ..accounts::Sample::default()
                });
                state.active = Some("m27".to_owned());
                ev.push(accounts::Event::new(UPKEEP_NOW, accounts::Kind::Use, "m27"));
                Ok(ev)
            })
            .unwrap();

        let staging_m27_2 = store.claim_staging_dir("m27-2").unwrap();
        let staging_m27_2_text = staging_m27_2.to_str().unwrap().to_owned();
        fs::write(
            staging_m27_2.join(".claude.json"),
            "{\"oauthAccount\":{\"emailAddress\":\"sol-m27@example.test\"}}\n",
        )
        .unwrap();
        let wrong_status = super::super::align::Captured {
            stdout: b"{\"loggedIn\":true,\"authMethod\":\"claude.ai\",\"email\":\"sol-m27@example.test\",\"subscriptionType\":\"max\"}\n".to_vec(),
            code: Some(0),
        };
        assert!(
            !complete_staging_renewal(
                &store,
                &roster,
                &roster.rows[1].label,
                &staging_m27_2,
                &staging_m27_2_text,
                &wrong_status,
                UPKEEP_NOW + 5,
            ),
            "signing into already-enrolled low account m27 during m27-2's renewal must be refused as wrong-account"
        );
        assert!(
            !staging_m27_2.exists(),
            "refused wrong-account staging directory must be removed"
        );
        let (logins_after_wrong, _) = store.load_logins(UPKEEP_NOW + 5).unwrap();
        assert!(
            logins_after_wrong.get("m27").is_none(),
            "m27's binding must not be overwritten by m27-2's failed staging login"
        );
        assert_eq!(
            logins_after_wrong.get("m27-2").and_then(|b| b.wrong_at),
            Some(UPKEEP_NOW + 5),
            "wrong_at must be recorded on m27-2 when renewal signs into a different account"
        );

        let (state_now, _) = store.load(UPKEEP_NOW + 10).unwrap();
        let cands_before_wrong: Vec<String> = auto_renew_candidates(
            &roster,
            &state_now,
            &accounts::Logins::default(),
            UPKEEP_NOW + 10,
        )
        .into_iter()
        .map(|r| r.label)
        .collect();
        assert_eq!(
            cands_before_wrong,
            vec!["m27-2".to_owned(), "m27-3".to_owned()],
            "without wrong_at, unenrolled successors must be returned in roster order"
        );
        let cands: Vec<String> =
            auto_renew_candidates(&roster, &state_now, &logins_after_wrong, UPKEEP_NOW + 10)
                .into_iter()
                .map(|r| r.label)
                .collect();
        assert_eq!(
            cands,
            vec!["m27-3".to_owned(), "m27-2".to_owned()],
            "clean successor m27-3 must be ordered ahead of m27-2 after m27-2 recorded wrong_at"
        );

        // m27-3's desk browser profile opens a window on the owner's screen, so an upkeep never
        // drives it: with nothing else on the machine, no sign-in starts, and each row's
        // attempt names the routes it tried.
        let wit = Witness::answering("");
        assert!(
            !auto_renew_next_account(&w.home, &w.state, &w.roster(), UPKEEP_NOW + 10, &wit.fx()),
            "a desk browser is never driven unattended"
        );
        assert_eq!(wit.spawned(), 0);
        let (logins_tried, _) = store.load_logins(UPKEEP_NOW + 10).unwrap();
        let attempt = |logins: &accounts::Logins, label: &str| {
            logins
                .get(label)
                .and_then(|b| b.sign_in_attempt.clone())
                .unwrap_or_else(|| panic!("{label}'s attempt is on record"))
        };
        let m27_3_tried = attempt(&logins_tried, "m27-3");
        assert_eq!(m27_3_tried.outcome, accounts::AttemptOutcome::NoRoute);
        assert!(
            m27_3_tried
                .routes
                .contains("desk-browser(opens a window: not used unattended)"),
            "{}",
            m27_3_tried.routes
        );
        assert_eq!(
            attempt(&logins_tried, "m27-2").outcome,
            accounts::AttemptOutcome::NoRoute
        );

        // A headless browser and a stored profile for m27-3 are a route: it is the one signed in.
        let m27_3_profile =
            super::super::headless_oauth::browser_profile_dir(&store.dir, Some("m27-3"));
        fs::create_dir_all(&m27_3_profile).unwrap();
        let wit = Witness {
            browser: Box::new(|_: &Path| true),
            ..Witness::answering("")
        };
        assert!(
            auto_renew_next_account(&w.home, &w.state, &w.roster(), UPKEEP_NOW + 10, &wit.fx()),
            "auto_renew_next_account must spawn renewal for m27-3"
        );
        assert_eq!(wit.spawned.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(
            wit.guard.lock().unwrap().last_attempt.contains_key("m27-3"),
            "m27-3 must be the account claimed by AutoRenewPermit"
        );
        fs::remove_dir_all(&m27_3_profile).unwrap();

        // Even with a fresh AutoRenewGuard (as after a process restart or CLI run),
        // if m27-2 has a route and m27-3 has none, background auto_renew_next_account
        // must respect m27-2's persisted 15-minute wrong_at cooldown before retrying it.
        let mut roster_m27_2_mailbox = roster.clone();
        roster_m27_2_mailbox.rows[2].browser = None;
        let mut roster_m27_2_toml = String::new();
        for r in &roster_m27_2_mailbox.rows {
            roster_m27_2_toml.push_str(&accounts::row_text(r).unwrap());
            roster_m27_2_toml.push('\n');
        }
        fs::write(w.roster(), &roster_m27_2_toml).unwrap();
        let m27_2_mailbox = store
            .dir
            .join(super::super::headless_oauth::MAILBOX_TOML_FILE);
        fs::write(
            &m27_2_mailbox,
            "imap_url = \"imaps://127.0.0.1:9/INBOX\"\n[passwords]\n\"sun.m27.2@example.test\" = \"pw\"\n",
        )
        .unwrap();
        let before_wrong =
            std::time::UNIX_EPOCH + Duration::from_secs(u64::try_from(UPKEEP_NOW - 100).unwrap());
        fs::OpenOptions::new()
            .append(true)
            .open(&m27_2_mailbox)
            .unwrap()
            .set_modified(before_wrong)
            .unwrap();
        let wit_cooldown = Witness::answering("");
        assert!(
            !auto_renew_next_account(
                &w.home,
                &w.state,
                &w.roster(),
                UPKEEP_NOW + 10,
                &wit_cooldown.fx()
            ),
            "background auto_renew_next_account must hold m27-2 during its 15-minute wrong_at cooldown"
        );
        assert_eq!(
            wit_cooldown
                .spawned
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        assert!(
            auto_renew_next_account(
                &w.home,
                &w.state,
                &w.roster(),
                UPKEEP_NOW + 910,
                &wit_cooldown.fx()
            ),
            "background auto_renew_next_account must retry m27-2 once its 15-minute wrong_at cooldown expires"
        );
        assert_eq!(
            wit_cooldown
                .spawned
                .load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        assert!(
            wit_cooldown
                .guard
                .lock()
                .unwrap()
                .last_attempt
                .contains_key("m27-2")
        );
        fs::remove_file(&m27_2_mailbox).unwrap();

        // Configure a mailbox password for m27-3 only (and remove its browser profile so only
        // its mailbox credential makes it headless-capable).
        let mut roster_no_browser = roster.clone();
        roster_no_browser.rows[2].browser = None;
        let mut roster_no_browser_toml = String::new();
        for r in &roster_no_browser.rows {
            roster_no_browser_toml.push_str(&accounts::row_text(r).unwrap());
            roster_no_browser_toml.push('\n');
        }
        fs::write(w.roster(), &roster_no_browser_toml).unwrap();
        let secrets = w.home.join(".secrets");
        fs::create_dir_all(&secrets).unwrap();
        fs::write(
            secrets.join(super::super::headless_oauth::SECRETS_MAILBOX_TOML_FILE),
            "webmail_url = \"https://mail.proton.me/u/0/inbox\"\n[passwords]\nm27-3 = \"secret-pw\"\n",
        )
        .unwrap();
        let arr_cfg = super::super::headless_oauth::MailboxConfig::from_toml_str(
            r#"
[[accounts]]
label = "m27-2"
email = "sun.m27.2@example.test"
password = "arr-pw"
totp = "JBSWY3DPEHPK3PXP"

[passwords]
"*" = "fallback-pw"
"#,
            None,
        )
        .expect("parse [[accounts]] and wildcard passwords");
        assert_eq!(
            arr_cfg
                .password_for(Some("sun.m27.2@example.test"))
                .as_deref(),
            Some("arr-pw")
        );
        assert_eq!(
            arr_cfg
                .totp_secret_for(Some("sun.m27.2@example.test"))
                .as_deref(),
            Some("JBSWY3DPEHPK3PXP")
        );
        assert_eq!(
            arr_cfg.password_for(Some("other@example.test")).as_deref(),
            Some("fallback-pw")
        );

        struct RecordingHost {
            tty: bool,
            flows: std::sync::Mutex<Vec<(String, super::super::cli::RenewBrowser)>>,
        }
        impl super::super::cli::AccountsHost for RecordingHost {
            fn stdin_is_terminal(&self) -> bool {
                self.tty
            }
            fn auth_status(&self, _: &str) -> Result<super::super::align::Captured, String> {
                Err("unused".to_owned())
            }
            fn unaskable(&self) -> Option<String> {
                None
            }
            fn login(&self, _: Option<&str>) -> Result<i32, String> {
                Err("unused".to_owned())
            }
            fn now(&self) -> i64 {
                UPKEEP_NOW + 20
            }
            fn renew_flow(
                &self,
                home: &str,
                email: &str,
                browser: &super::super::cli::RenewBrowser,
            ) -> Result<i32, String> {
                let _ = fs::write(Path::new(home).join(".credentials.json"), b"{}");
                self.flows
                    .lock()
                    .unwrap()
                    .push((email.to_owned(), browser.clone()));
                Ok(0)
            }
            fn status_in_home(&self, _: &str) -> Result<super::super::align::Captured, String> {
                Ok(super::super::align::Captured {
                    stdout: br#"{"loggedIn":true,"authMethod":"claude.ai","email":"star.m27.3@example.test","subscriptionType":"max"}"#.to_vec(),
                    code: Some(0),
                })
            }
            fn current_exe(&self) -> Option<PathBuf> {
                None
            }
        }

        let env = w.env(UPKEEP_NOW + 20);
        let non_tty_host = RecordingHost {
            tty: false,
            flows: std::sync::Mutex::new(Vec::new()),
        };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code_unconfigured = super::super::cli::run_accounts(
            &super::super::cli::AccountsCmd::Renew {
                labels: vec!["m27-2".to_owned()],
                due: false,
                on: None,
            },
            &env,
            &non_tty_host,
            &mut out,
            &mut err,
        );
        assert_eq!(
            code_unconfigured,
            ExitCode::from(1),
            "a named row left as it was is no renewal"
        );
        assert!(
            non_tty_host.flows.lock().unwrap().is_empty(),
            "non-terminal renew of m27-2 (which has no mailbox password) must not run renew_flow"
        );
        assert!(
            String::from_utf8_lossy(&out).contains("due here: m27-2"),
            "stdout: {}",
            String::from_utf8_lossy(&out)
        );

        out.clear();
        err.clear();
        let code_due = super::super::cli::run_accounts(
            &super::super::cli::AccountsCmd::Renew {
                labels: Vec::new(),
                due: true,
                on: None,
            },
            &env,
            &non_tty_host,
            &mut out,
            &mut err,
        );
        assert_eq!(
            code_due,
            ExitCode::SUCCESS,
            "stderr: {}",
            String::from_utf8_lossy(&err)
        );
        let recorded = non_tty_host.flows.lock().unwrap().clone();
        assert_eq!(
            recorded,
            vec![(
                "star.m27.3@example.test".to_owned(),
                super::super::cli::RenewBrowser::Remote { opener: None }
            )],
            "renew --due must skip unconfigured m27-2 and run headless-capable m27-3 with Remote browser"
        );

        let secret_file = w.home.join(".secrets/m27_2_password");
        fs::write(&secret_file, "secret-m27-2\n").unwrap();
        let mtime_with_secret = latest_mailbox_mtime(Some(&w.home), &store).expect("mtime");
        let secret_meta_mtime = fs::metadata(&secret_file).unwrap().modified().unwrap();
        assert!(
            mtime_with_secret >= secret_meta_mtime,
            "latest_mailbox_mtime must include individual secret files under ~/.secrets"
        );
        let _ = fs::remove_file(&secret_file);

        let maildir_new = store.dir.join("mailbox/new");
        fs::create_dir_all(&maildir_new).unwrap();
        let eml_file = maildir_new.join("msg1.eml");
        fs::write(
            &eml_file,
            "To: <sun.m27.2@example.test>\r\n\r\nCode: 123456\r\n",
        )
        .unwrap();
        let eml_meta_mtime = fs::metadata(&eml_file).unwrap().modified().unwrap();
        let mtime_with_eml = latest_mailbox_mtime(Some(&w.home), &store).expect("eml mtime");
        assert!(
            mtime_with_eml >= eml_meta_mtime,
            "latest_mailbox_mtime must include individual unclicked message files in Maildir"
        );
        let _ = fs::remove_dir_all(store.dir.join("mailbox"));

        // A cached headless browser profile for m27-2 must not be treated as headless-capable
        // when m27-2 already has wrong_at recorded and no mailbox is configured for m27-2.
        let m27_2_browser_dir =
            super::super::headless_oauth::browser_profile_dir(&store.dir, Some("m27-2"));
        fs::create_dir_all(&m27_2_browser_dir).unwrap();
        non_tty_host.flows.lock().unwrap().clear();
        out.clear();
        err.clear();
        let code_wrong_profile = super::super::cli::run_accounts(
            &super::super::cli::AccountsCmd::Renew {
                labels: vec!["m27-2".to_owned()],
                due: false,
                on: None,
            },
            &env,
            &non_tty_host,
            &mut out,
            &mut err,
        );
        assert_eq!(
            code_wrong_profile,
            ExitCode::from(1),
            "a named row left as it was is no renewal"
        );
        assert!(
            non_tty_host.flows.lock().unwrap().is_empty(),
            "row with wrong_at and no mailbox must not run headless via stale browser profile"
        );
        let _ = fs::remove_dir_all(&m27_2_browser_dir);

        let status = super::super::align::Captured {
            stdout: br#"{"loggedIn":true,"authMethod":"claude.ai","email":"sun.m27.2@example.test","subscriptionType":"max"}"#.to_vec(),
            code: Some(0),
        };
        // Failed store reads cannot publish a proof that recovery could later adopt.
        let state_bytes = fs::read(store.state_file()).unwrap();
        let login_bytes = fs::read(store.logins_file()).unwrap();
        let ledger = store.tail(20).unwrap();
        for blocked_file in [store.state_file(), store.logins_file()] {
            let kept = blocked_file.with_extension("saved");
            fs::rename(&blocked_file, &kept).unwrap();
            fs::create_dir(&blocked_file).unwrap();
            let staging = store.claim_staging_dir("m27-2").unwrap();
            let profile = b"private verified sign-in fixture";
            fs::write(staging.join(".claude.json"), profile).unwrap();
            assert!(!complete_staging_renewal(
                &store,
                &roster,
                "m27-2",
                &staging,
                staging.to_str().unwrap(),
                &status,
                UPKEEP_NOW + 20,
            ));
            assert_eq!(fs::read(staging.join(".claude.json")).unwrap(), profile);
            assert!(!staging.join(accounts::RENEWAL_CHECKPOINT_FILE).exists());
            assert!(blocked_file.is_dir());
            fs::remove_dir(&blocked_file).unwrap();
            fs::rename(&kept, &blocked_file).unwrap();
            assert_eq!(fs::read(store.state_file()).unwrap(), state_bytes);
            assert_eq!(fs::read(store.logins_file()).unwrap(), login_bytes);
            assert_eq!(store.tail(20).unwrap(), ledger);
        }

        let state_file = store.state_file();
        fs::write(&state_file, [0xff]).unwrap();
        let staging = store.claim_staging_dir("m27-2").unwrap();
        fs::write(staging.join(".claude.json"), b"checked sign-in fixture").unwrap();
        assert!(!complete_staging_renewal(
            &store,
            &roster,
            "m27-2",
            &staging,
            staging.to_str().unwrap(),
            &status,
            UPKEEP_NOW + 20,
        ));
        assert_eq!(fs::read(&state_file).unwrap(), [0xff]);
        assert_eq!(fs::read(store.logins_file()).unwrap(), login_bytes);
        assert_eq!(store.tail(20).unwrap(), ledger);
        assert_eq!(
            fs::read(staging.join(".claude.json")).unwrap(),
            b"checked sign-in fixture"
        );
        assert!(!staging.join(accounts::RENEWAL_CHECKPOINT_FILE).exists());
        fs::write(&state_file, &state_bytes).unwrap();

        let staging = store.claim_staging_dir("m27").unwrap();
        fs::write(staging.join(".claude.json"), b"new fallback login").unwrap();
        let staging_text = staging.to_str().unwrap();
        assert!(complete_staging_renewal(
            &store,
            &roster,
            "m27",
            &staging,
            staging_text,
            &status,
            UPKEEP_NOW + 30,
        ));
        let bound = store.load_logins(UPKEEP_NOW + 30).unwrap().0;
        let renewed = bound.get("m27-2").unwrap().clone();
        assert_eq!(renewed.dir.as_deref(), Some(staging_text));
        assert_eq!(renewed.renewed_at, Some(UPKEEP_NOW + 30));
        let proof = accounts::read_renewal_checkpoint(
            &staging,
            &fs::canonicalize(&staging).unwrap(),
            UPKEEP_NOW + 30,
        )
        .unwrap();
        assert_eq!(proof.label, "m27-2");
        assert_eq!(proof.dir, staging_text);

        for limited in [false, true] {
            if limited {
                store
                    .transact_logins(UPKEEP_NOW + 35, &mut None, |state, _| {
                        Ok(accounts::note_wall(
                            state,
                            "m27-2",
                            "seven_day",
                            Some(UPKEEP_NOW + 3600),
                            UPKEEP_NOW + 35,
                            UPKEEP_NOW + 35,
                        ))
                    })
                    .unwrap();
            }
            let before_state = store.load(UPKEEP_NOW + 40).unwrap().0;
            let staging = store.claim_staging_dir("m27").unwrap();
            fs::write(staging.join(".claude.json"), b"wrong-account fixture").unwrap();
            assert!(!complete_staging_renewal(
                &store,
                &roster,
                "m27",
                &staging,
                staging.to_str().unwrap(),
                &status,
                UPKEEP_NOW + 40,
            ));
            assert!(!staging.exists());
            assert_eq!(store.load(UPKEEP_NOW + 40).unwrap().0, before_state);
            assert_eq!(
                store.load_logins(UPKEEP_NOW + 40).unwrap().0.get("m27-2"),
                Some(&renewed),
            );
        }

        // An interrupted staging renewal with a valid checkpoint must be recovered by `accounts use`.
        store
            .transact(UPKEEP_NOW + 45, &mut None, |state| {
                Ok(accounts::note_auth(
                    state,
                    "m27-3",
                    accounts::Auth::SignedOut,
                    UPKEEP_NOW + 45,
                ))
            })
            .unwrap();
        let use_staging = store.claim_staging_dir("m27-3").unwrap();
        fs::write(use_staging.join(".claude.json"), b"checkpointed m27-3").unwrap();
        let use_staging_text = use_staging.to_str().unwrap();
        accounts::write_renewal_checkpoint(
            "m27-3",
            "star.m27.3@example.test",
            use_staging_text,
            Some("max"),
            UPKEEP_NOW + 50,
            &store.load_logins(UPKEEP_NOW + 50).unwrap().0,
        )
        .unwrap();
        let env_use = w.env(UPKEEP_NOW + 55);
        out.clear();
        err.clear();
        let use_code = super::super::cli::run_accounts(
            &super::super::cli::AccountsCmd::Use {
                label: "m27-3".to_owned(),
            },
            &env_use,
            &non_tty_host,
            &mut out,
            &mut err,
        );
        assert_eq!(use_code, ExitCode::SUCCESS);
        let recovered_m27_3 = store
            .load_logins(UPKEEP_NOW + 55)
            .unwrap()
            .0
            .get("m27-3")
            .cloned()
            .expect("m27-3 login recovered by accounts use");
        assert_eq!(recovered_m27_3.dir.as_deref(), Some(use_staging_text));
        assert_eq!(recovered_m27_3.renewed_at, Some(UPKEEP_NOW + 50));

        // A single-account machine with only a per-account `~/.secrets/m27_password` file
        // (and no `ai-mailbox.toml` or `fleet-accounts.toml`) must be recognized by
        // `has_init_accounts_source` and converged by `sync_accounts_for_init`.
        let single_w = World::new();
        fs::write(
            single_w.home.join(".claude.json"),
            "{\"oauthAccount\":{\"emailAddress\":\"sol-m27@example.test\"}}\n",
        )
        .unwrap();
        let single_secrets = single_w.home.join(".secrets");
        fs::create_dir_all(&single_secrets).unwrap();
        fs::write(single_secrets.join("m27_password"), "single-secret\n").unwrap();
        assert!(has_init_accounts_source(
            &single_w.home,
            &single_w.state,
            &single_w.roster(),
            UPKEEP_NOW,
        ));
        let (mach, rows_n, _, _) = sync_accounts_for_init(
            &single_w.home,
            &single_w.state,
            &single_w.roster(),
            UPKEEP_NOW,
        )
        .expect("sync single account with per-account secret file");
        assert_eq!(mach, "m27");
        assert_eq!(rows_n, 1);

        // When `~/.secrets/ai-accounts.csv` gains a new account row, `needs_init_accounts_sync`
        // must detect the un-converged CSV row.
        let _ = fs::remove_file(single_secrets.join(FLEET_ROSTER_FILE));
        fs::write(
            single_secrets.join("ai-accounts.csv"),
            concat!(
                "email,computer,claude\n",
                "sol-m27@example.test,m27,1\n",
                "sun.m27.2@example.test,m27.2,1\n",
            ),
        )
        .unwrap();
        assert!(needs_init_accounts_sync(
            &single_w.home,
            &single_w.state,
            &single_w.roster(),
            UPKEEP_NOW + 60,
        ));
    }

    #[test]
    fn auto_renew_candidates_keeps_wall_recovery_ahead_of_due_rows_even_after_historical_wrong_at()
    {
        let dir = aterm_tempfile::tempdir().unwrap();
        let m27_2_dir = dir.path().join("m27-2-empty");
        let m27_3_dir = dir.path().join("m27-3-empty");
        fs::create_dir_all(&m27_2_dir).unwrap();
        fs::create_dir_all(&m27_3_dir).unwrap();
        let roster = Roster {
            rows: vec![
                accounts::Row {
                    label: "m27".to_owned(),
                    email: Some("m27@example.test".to_owned()),
                    class: Class::Dedicated,
                    ..accounts::Row::default()
                },
                accounts::Row {
                    label: "m27-2".to_owned(),
                    dir: Some(m27_2_dir.display().to_string()),
                    email: Some("m27-2@example.test".to_owned()),
                    class: Class::Dedicated,
                    ..accounts::Row::default()
                },
                accounts::Row {
                    label: "m27-3".to_owned(),
                    dir: Some(m27_3_dir.display().to_string()),
                    email: Some("m27-3@example.test".to_owned()),
                    class: Class::Dedicated,
                    ..accounts::Row::default()
                },
            ],
        };
        let now = UPKEEP_NOW;
        let mut state = accounts::State {
            active: Some("m27".to_owned()),
            proven: true,
            ..accounts::State::default()
        };
        let _ = accounts::note_sample(
            &mut state,
            "m27",
            accounts::Sample {
                week_pct: Some(accounts::LOW_WEEK_PCT),
                week_reset: Some(now + 3 * 86_400),
                at: now - 30,
                ..accounts::Sample::default()
            },
            now,
        );
        let mut logins = accounts::Logins::default();
        logins.labels.insert(
            "m27".to_owned(),
            accounts::Binding {
                renewed_at: Some(now - 360 * 86_400),
                ..accounts::Binding::default()
            },
        );
        logins.labels.insert(
            "m27-2".to_owned(),
            accounts::Binding {
                wrong_at: Some(now - 86_400),
                ..accounts::Binding::default()
            },
        );

        let labels: Vec<String> = auto_renew_candidates(&roster, &state, &logins, now)
            .into_iter()
            .map(|r| r.label)
            .collect();
        assert_eq!(
            labels,
            vec!["m27-3".to_owned(), "m27-2".to_owned(), "m27".to_owned(),],
            "wall-recovery rows (clean m27-3 first, then m27-2 with old wrong_at) stay ahead of background due row m27"
        );

        // When m27 is healthy (not low or walled), its due renewal comes first, and the rows
        // still to sign in follow it at once: a sign-in is never left for a wall to ask for.
        let mut healthy = state.clone();
        healthy.accounts.clear();
        let due_first: Vec<String> = auto_renew_candidates(&roster, &healthy, &logins, now)
            .into_iter()
            .map(|r| r.label)
            .collect();
        assert_eq!(
            due_first,
            vec!["m27".to_owned(), "m27-3".to_owned(), "m27-2".to_owned()]
        );

        // Claude Code's own warning that m27's week is filling is pressure enough, at any
        // percent and long before m27 is low: the rows that would take its sessions go first.
        let mut warned = healthy.clone();
        warned
            .accounts
            .entry("m27".to_owned())
            .or_default()
            .warned_week = Some(accounts::WindowSaid {
            pct: Some(80),
            reset: Some(now + 3 * 86_400),
            at: now - 30,
        });
        assert!(!warned.accounts["m27"].low(now));
        let warned_first: Vec<String> = auto_renew_candidates(&roster, &warned, &logins, now)
            .into_iter()
            .map(|r| r.label)
            .collect();
        assert_eq!(
            warned_first,
            vec!["m27-3".to_owned(), "m27-2".to_owned(), "m27".to_owned()]
        );
    }

    /// A session whose own row signed out waits on that row's sign-in (its lost login follows
    /// only its own row), so the row is signed in first, ahead of another signed-out row before it
    /// in the roster and even where another row is ready to run now; a signed-out row no session
    /// runs on keeps its place among the ended logins.
    #[test]
    fn a_signed_out_row_a_session_runs_on_is_signed_in_first() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let made = |name: &str| {
            let at = dir.path().join(name);
            fs::create_dir_all(&at).unwrap();
            fs::write(at.join(".claude.json"), "{}").unwrap();
            at.display().to_string()
        };
        let empty = dir.path().join("m2-3-empty");
        fs::create_dir_all(&empty).unwrap();
        let row = |label: &str, dir: Option<String>| accounts::Row {
            label: label.to_owned(),
            dir,
            email: Some(format!("{label}@example.test")),
            class: Class::Dedicated,
            ..accounts::Row::default()
        };
        let roster = Roster {
            rows: vec![
                row("m2", None),
                row("m2-3", Some(empty.display().to_string())),
                row("m2-4", Some(made("m2-4"))),
                row("m2-1", Some(made("m2-1"))),
                row("m2-2", Some(made("m2-2"))),
            ],
        };
        let now = UPKEEP_NOW;
        let mut state = accounts::State {
            active: Some("m2".to_owned()),
            proven: true,
            ..accounts::State::default()
        };
        for label in ["m2-1", "m2-2"] {
            let _ = accounts::note_auth(&mut state, label, accounts::Auth::SignedOut, now - 60);
        }
        let logins = accounts::Logins::default();
        let order = |state: &accounts::State| -> Vec<String> {
            auto_renew_candidates_tiered(Cli::Claude, None, &roster, state, &logins, now)
                .into_iter()
                .map(|(tier, row)| format!("{tier}:{}", row.label))
                .collect()
        };
        assert_eq!(
            order(&state),
            ["1:m2-1", "1:m2-2", "2:m2-3"],
            "the control: no session runs on m2-2, and m2-4 is ready, so nothing is tier 0"
        );
        let mut running = state.clone();
        running.sessions.insert(
            "s-1".to_owned(),
            accounts::SessionRow {
                account: "m2-2".to_owned(),
                at: now - 30,
                held: None,
            },
        );
        assert_eq!(
            order(&running),
            ["0:m2-2", "1:m2-1", "2:m2-3"],
            "the session's own signed-out row goes first, though m2-4 is ready"
        );
        let mut back = running.clone();
        let _ = accounts::note_auth(&mut back, "m2-2", accounts::Auth::Ok, now - 10);
        assert_eq!(
            order(&back),
            ["1:m2-1", "2:m2-3"],
            "a row signed in again is no candidate"
        );
        // With no other row ready, the walled session's successors are tier 0 too, and its own
        // row still goes first though its last renewal answered another account; another row's
        // wrong account still sorts it last (the control).
        let unready = Roster {
            rows: roster
                .rows
                .iter()
                .filter(|row| row.label != "m2-4")
                .cloned()
                .collect(),
        };
        let order_of = |logins: &accounts::Logins| -> Vec<String> {
            auto_renew_candidates_tiered(Cli::Claude, None, &unready, &running, logins, now)
                .into_iter()
                .map(|(tier, row)| format!("{tier}:{}", row.label))
                .collect()
        };
        let mut wrong = accounts::Logins::default();
        let _ = accounts::note_wrong_account(&mut wrong, "m2-2", now - 20);
        assert_eq!(
            order_of(&wrong),
            ["0:m2-2", "0:m2-3", "0:m2-1"],
            "the session's own row first, its wrong account notwithstanding"
        );
        let mut wrong = accounts::Logins::default();
        let _ = accounts::note_wrong_account(&mut wrong, "m2-3", now - 20);
        assert_eq!(
            order_of(&wrong),
            ["0:m2-2", "0:m2-1", "0:m2-3"],
            "the control: another row's wrong account sorts it last"
        );
    }

    fn vault_ladder_fixture() -> (World, Row) {
        let w = World::new();
        let row = Row {
            label: "m2".to_owned(),
            class: Class::Dedicated,
            email: Some("m2@example.test".to_owned()),
            ..Row::default()
        };
        fs::create_dir_all(&w.config).unwrap();
        fs::write(w.roster(), accounts::row_text(&row).unwrap()).unwrap();
        let source = w.home.join(".config/aterm/secrets/source.toml");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::write(source, "# vault A\n").unwrap();
        (w, row)
    }

    fn vault_ladder_pass(w: &World, witness: &Witness, row: &Row) -> LadderStep {
        let mut roster = Roster::load(&w.roster()).unwrap();
        if roster.row(&row.label) != Some(row) {
            if let Some(current) = roster
                .rows
                .iter_mut()
                .find(|current| current.label == row.label)
            {
                *current = row.clone();
            } else {
                roster.rows.push(row.clone());
            }
            fs::write(w.roster(), format_roster_toml(&roster).unwrap()).unwrap();
        }
        let effects = witness.fx();
        let context = LadderCtx::of(&effects, &w.roster());
        ladder_route(
            &w.home,
            &accounts::Store::at(&w.state),
            row,
            UPKEEP_NOW,
            &witness.guard,
            &context,
        )
    }

    /// A changed vault source is retried with the same guard, without waiting out the old
    /// source's failed refresh. An unchanged source still shares that cached failure.
    #[test]
    fn a_changed_vault_source_does_not_reuse_the_previous_sources_failure() {
        use super::super::sign_in_ladder::Vault;
        use std::sync::atomic::{AtomicUsize, Ordering};
        static OLD: AtomicUsize = AtomicUsize::new(0);
        static NEW: AtomicUsize = AtomicUsize::new(0);
        fn old_source(_: &Path) -> Vault {
            OLD.fetch_add(1, Ordering::SeqCst);
            Vault::Failed("old source unavailable".to_owned())
        }
        fn new_source(_: &Path) -> Vault {
            NEW.fetch_add(1, Ordering::SeqCst);
            Vault::Unchanged(83)
        }
        OLD.store(0, Ordering::SeqCst);
        NEW.store(0, Ordering::SeqCst);
        let (w, row) = vault_ladder_fixture();
        let store = accounts::Store::at(&w.state);
        let source = w.home.join(".config/aterm/secrets/source.toml");
        let stamp = fs::metadata(&source).unwrap().modified().unwrap();
        let mut witness = Witness {
            vault_sync: old_source,
            ..Witness::answering("")
        };
        let pass = |witness: &Witness| vault_ladder_pass(&w, witness, &row);
        assert!(matches!(pass(&witness), LadderStep::NoRoute));
        assert!(matches!(pass(&witness), LadderStep::Held));
        assert_eq!(OLD.load(Ordering::SeqCst), 1);
        fs::write(&source, "# vault B\n").unwrap();
        fs::File::options()
            .write(true)
            .open(&source)
            .unwrap()
            .set_modified(stamp)
            .unwrap();
        witness.vault_sync = new_source;
        assert!(matches!(pass(&witness), LadderStep::NoRoute));
        assert_eq!(
            NEW.load(Ordering::SeqCst),
            1,
            "the new source must refresh before the previous source's wait expires"
        );
        assert_eq!(OLD.load(Ordering::SeqCst), 1);
        let second = Row {
            label: "m2-2".to_owned(),
            email: Some("m2-2@example.test".to_owned()),
            dir: Some(w.home.join("second-login").to_string_lossy().into_owned()),
            ..row.clone()
        };
        assert!(matches!(
            vault_ladder_pass(&w, &witness, &second),
            LadderStep::NoRoute
        ));
        assert_eq!(
            NEW.load(Ordering::SeqCst),
            1,
            "another row reuses the new source's cache"
        );
        let (logins, _) = store.load_logins(UPKEEP_NOW).unwrap();
        assert!(
            logins
                .get("m2")
                .unwrap()
                .sign_in_attempt
                .as_ref()
                .unwrap()
                .routes
                .contains("vault(seq 83, nothing new)")
        );
    }

    #[test]
    fn a_vault_revision_changes_the_cache_context_but_a_heartbeat_does_not() {
        let (w, _) = vault_ladder_fixture();
        let state = w.home.join(".config/aterm/secrets/state.toml");
        fs::write(&state, "membership = \"in\"\nseq = 1\nheard = 1\n").unwrap();
        let initial = vault_input(&w.home);
        assert!(matches!(initial, VaultInput::Read(_)));
        fs::write(&state, "membership = \"in\"\nseq = 1\nheard = 2\n").unwrap();
        assert_eq!(vault_input(&w.home), initial);
        fs::write(&state, "membership = \"in\"\nseq = 2\nheard = 2\n").unwrap();
        assert_ne!(vault_input(&w.home), initial);
    }

    #[test]
    fn a_changed_row_does_not_inherit_the_previous_identitys_retry_wait() {
        use super::super::sign_in_ladder::Vault;
        for changed in ["email", "identity", "browser", "directory"] {
            let (w, mut row) = vault_ladder_fixture();
            row.browser = Some("Profile 1".to_owned());
            let witness = Witness {
                vault_sync: |_| Vault::Unchanged(83),
                ..Witness::answering("")
            };
            assert!(matches!(
                vault_ladder_pass(&w, &witness, &row),
                LadderStep::NoRoute
            ));
            assert!(matches!(
                vault_ladder_pass(&w, &witness, &row),
                LadderStep::Held
            ));
            let mut replacement = row.clone();
            match changed {
                "email" => replacement.email = Some("replacement@example.test".to_owned()),
                "identity" => {
                    replacement.identity_email = Some("provider@example.test".to_owned());
                }
                "browser" => replacement.browser = Some("Profile 2".to_owned()),
                "directory" => {
                    replacement.dir =
                        Some(w.home.join("replacement").to_string_lossy().into_owned());
                }
                _ => unreachable!(),
            }
            assert!(
                matches!(
                    vault_ladder_pass(&w, &witness, &replacement),
                    LadderStep::NoRoute
                ),
                "changed {changed} must be examined without inheriting the old row's wait"
            );
            assert!(matches!(
                vault_ladder_pass(&w, &witness, &replacement),
                LadderStep::Held
            ));
            assert!(matches!(
                vault_ladder_pass(&w, &witness, &row),
                LadderStep::NoRoute
            ));
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_large_vault_inventory_uses_the_client_state_limit() {
        let (w, _) = vault_ladder_fixture();
        let store = accounts::Store::at(&w.state);
        let root = w.home.join(".config/aterm/secrets");
        let state = root.join("state.toml");
        let mut text = String::from("membership = \"in\"\nseq = 1\nheard = 1\n# ");
        text.push_str(&"x".repeat(SOURCE_TEXT_MAX_BYTES));
        fs::write(&state, &text).unwrap();
        let initial = vault_input(&w.home);
        let row_inputs = ladder_inputs(&w.home, &store);
        assert!(matches!(initial, VaultInput::Read(_)));
        fs::write(&state, text.replacen("heard = 1", "heard = 2", 1)).unwrap();
        assert_eq!(vault_input(&w.home), initial);
        assert_eq!(ladder_inputs(&w.home, &store), row_inputs);
        fs::write(&state, text.replacen("seq = 1", "seq = 2", 1)).unwrap();
        assert_ne!(vault_input(&w.home), initial);
        assert_ne!(ladder_inputs(&w.home, &store), row_inputs);

        fs::OpenOptions::new()
            .write(true)
            .open(&state)
            .unwrap()
            .set_len(aterm_secrets::client::MAX_STATE as u64 + 1)
            .unwrap();
        assert_eq!(vault_input(&w.home), VaultInput::Unreadable);
        fs::write(&state, "membership = \"in\"\nseq = 1\n").unwrap();
        fs::OpenOptions::new()
            .write(true)
            .open(root.join("source.toml"))
            .unwrap()
            .set_len(SOURCE_TEXT_MAX_BYTES as u64 + 1)
            .unwrap();
        assert_eq!(vault_input(&w.home), VaultInput::Unreadable);
    }

    #[test]
    fn an_unreadable_vault_source_is_not_reported_as_missing_and_can_recover() {
        use super::super::sign_in_ladder::Vault;
        let (w, row) = vault_ladder_fixture();
        let source = w.home.join(".config/aterm/secrets/source.toml");
        let saved = source.with_extension("saved");
        fs::rename(&source, &saved).unwrap();
        fs::create_dir(&source).unwrap();
        assert_eq!(vault_input(&w.home), VaultInput::Unreadable);
        let witness = Witness {
            vault_sync: |_| Vault::Unchanged(83),
            ..Witness::answering("")
        };
        assert!(matches!(
            vault_ladder_pass(&w, &witness, &row),
            LadderStep::NoRoute
        ));
        let store = accounts::Store::at(&w.state);
        let (logins, _) = store.load_logins(UPKEEP_NOW).unwrap();
        let routes = &logins
            .get("m2")
            .unwrap()
            .sign_in_attempt
            .as_ref()
            .unwrap()
            .routes;
        assert!(routes.contains("vault context unreadable"), "{routes}");
        fs::rename(&source, source.with_extension("directory")).unwrap();
        fs::rename(&saved, &source).unwrap();
        assert!(matches!(
            vault_ladder_pass(&w, &witness, &row),
            LadderStep::NoRoute
        ));
        let (logins, _) = store.load_logins(UPKEEP_NOW).unwrap();
        assert!(
            logins
                .get("m2")
                .unwrap()
                .sign_in_attempt
                .as_ref()
                .unwrap()
                .routes
                .contains("vault(seq 83, nothing new)")
        );
    }

    #[test]
    fn a_vault_source_changed_during_refresh_defers_its_old_result() {
        use super::super::sign_in_ladder::Vault;
        fn changed(home: &Path) -> Vault {
            fs::write(
                home.join(".config/aterm/secrets/source.toml"),
                "# vault B\n",
            )
            .unwrap();
            Vault::Unchanged(1)
        }
        let (w, row) = vault_ladder_fixture();
        let mut witness = Witness {
            vault_sync: changed,
            ..Witness::answering("")
        };
        assert!(matches!(
            vault_ladder_pass(&w, &witness, &row),
            LadderStep::Held
        ));
        assert!(
            accounts::Store::at(&w.state)
                .load_logins(UPKEEP_NOW)
                .unwrap()
                .0
                .get("m2")
                .is_none()
        );
        witness.vault_sync = |_| Vault::Unchanged(83);
        assert!(matches!(
            vault_ladder_pass(&w, &witness, &row),
            LadderStep::NoRoute
        ));
        let (logins, _) = accounts::Store::at(&w.state)
            .load_logins(UPKEEP_NOW)
            .unwrap();
        assert!(
            logins
                .get("m2")
                .unwrap()
                .sign_in_attempt
                .as_ref()
                .unwrap()
                .routes
                .contains("vault(seq 83, nothing new)")
        );
    }

    #[test]
    fn a_vault_completion_invalidated_inline_leaves_no_unstarted_refresh() {
        use super::super::sign_in_ladder::Vault;
        let (w, row) = vault_ladder_fixture();
        let witness = Witness {
            vault_sync: |_| Vault::Unchanged(83),
            ..Witness::answering("")
        };
        let guard = std::sync::Arc::clone(&witness.guard);
        let spawn = move |work: Box<dyn FnOnce() + Send>| {
            work();
            guard.lock().unwrap().ladder.moved(Instant::now());
            Ok(())
        };
        let mut effects = witness.fx();
        effects.spawn_vault = &spawn;
        let context = LadderCtx::of(&effects, &w.roster());
        assert!(matches!(
            ladder_route(
                &w.home,
                &accounts::Store::at(&w.state),
                &row,
                UPKEEP_NOW,
                &witness.guard,
                &context
            ),
            LadderStep::Held
        ));
        assert!(matches!(
            vault_ladder_pass(&w, &witness, &row),
            LadderStep::NoRoute
        ));
    }

    #[test]
    fn an_async_vault_change_cannot_restore_a_rows_obsolete_wait() {
        use super::super::sign_in_ladder::Vault;
        use std::sync::atomic::{AtomicUsize, Ordering};
        type Work = Box<dyn FnOnce() + Send>;
        static CALLS: AtomicUsize = AtomicUsize::new(0);
        fn synced(_: &Path) -> Vault {
            CALLS.fetch_add(1, Ordering::SeqCst);
            Vault::Unchanged(83)
        }
        for (other_row, unreadable) in [(false, false), (true, false), (false, true)] {
            CALLS.store(0, Ordering::SeqCst);
            let (w, row) = vault_ladder_fixture();
            let second = Row {
                label: "m2-2".to_owned(),
                email: Some("m2-2@example.test".to_owned()),
                dir: Some(w.home.join("second-login").to_string_lossy().into_owned()),
                ..row.clone()
            };
            let witness = Witness {
                vault_sync: synced,
                ..Witness::answering("")
            };
            assert!(matches!(
                vault_ladder_pass(&w, &witness, &row),
                LadderStep::NoRoute
            ));
            if other_row {
                assert!(matches!(
                    vault_ladder_pass(&w, &witness, &second),
                    LadderStep::NoRoute
                ));
            }
            let source = w.home.join(".config/aterm/secrets/source.toml");
            let stamp = fs::metadata(&source).unwrap().modified().unwrap();
            let queue = std::sync::Arc::new(std::sync::Mutex::<Option<Work>>::new(None));
            let pending = std::sync::Arc::clone(&queue);
            let spawn = move |work: Work| {
                assert!(
                    pending.lock().unwrap().replace(work).is_none(),
                    "only one refresh may run"
                );
                Ok(())
            };
            let queued = |row: &Row| {
                let mut effects = witness.fx();
                effects.spawn_vault = &spawn;
                let context = LadderCtx::of(&effects, &w.roster());
                ladder_route(
                    &w.home,
                    &accounts::Store::at(&w.state),
                    row,
                    UPKEEP_NOW,
                    &witness.guard,
                    &context,
                )
            };
            fs::write(&source, "# vault B\n").unwrap();
            assert!(matches!(
                queued(if other_row { &second } else { &row }),
                LadderStep::Held
            ));
            let saved = source.with_extension("saved");
            if unreadable {
                fs::rename(&source, &saved).unwrap();
                fs::create_dir(&source).unwrap();
            } else {
                fs::write(&source, "# vault A\n").unwrap();
                fs::File::options()
                    .write(true)
                    .open(&source)
                    .unwrap()
                    .set_modified(stamp)
                    .unwrap();
            }
            assert!(
                matches!(queued(&row), LadderStep::Held),
                "no outcome while the refresh runs"
            );
            assert_eq!(CALLS.load(Ordering::SeqCst), 1);
            let work = queue.lock().unwrap().take().unwrap();
            work();
            if unreadable {
                fs::rename(&source, source.with_extension("obstruction")).unwrap();
                fs::rename(&saved, &source).unwrap();
                fs::write(&source, "# vault A\n").unwrap();
                fs::File::options()
                    .write(true)
                    .open(&source)
                    .unwrap()
                    .set_modified(stamp)
                    .unwrap();
            }
            assert!(matches!(
                vault_ladder_pass(&w, &witness, &row),
                LadderStep::NoRoute
            ));
            assert_eq!(
                CALLS.load(Ordering::SeqCst),
                3,
                "A must refresh after the old B completion"
            );
        }
    }

    #[test]
    fn an_applied_vault_revision_converges_on_the_next_upkeep() {
        use super::super::sign_in_ladder::Vault;
        use std::sync::atomic::{AtomicUsize, Ordering};
        static CALLS: AtomicUsize = AtomicUsize::new(0);
        fn applied(home: &Path) -> Vault {
            CALLS.fetch_add(1, Ordering::SeqCst);
            fs::write(
                home.join(".config/aterm/secrets/state.toml"),
                "membership = \"in\"\nseq = 83\n",
            )
            .unwrap();
            if home.join("bring-mailbox").is_file() {
                let secrets = home.join(".secrets");
                fs::create_dir_all(&secrets).unwrap();
                fs::write(
                    secrets.join(super::super::headless_oauth::SECRETS_MAILBOX_TOML_FILE),
                    "http_url = \"http://127.0.0.1:9/inbox\"\n",
                )
                .unwrap();
            }
            Vault::Applied(83)
        }
        for with_mailbox in [false, true] {
            CALLS.store(0, Ordering::SeqCst);
            let (w, row) = vault_ladder_fixture();
            if with_mailbox {
                fs::write(w.home.join("bring-mailbox"), "").unwrap();
            }
            let witness = Witness {
                vault_sync: applied,
                ..Witness::answering("")
            };
            assert!(matches!(
                vault_ladder_pass(&w, &witness, &row),
                LadderStep::Held
            ));
            assert_eq!(CALLS.load(Ordering::SeqCst), 1);
            let next = vault_ladder_pass(&w, &witness, &row);
            if with_mailbox {
                assert!(matches!(next, LadderStep::Go { .. }));
                assert_eq!(
                    CALLS.load(Ordering::SeqCst),
                    1,
                    "the delivered route needs no second refresh"
                );
            } else {
                assert!(matches!(next, LadderStep::NoRoute));
                assert_eq!(CALLS.load(Ordering::SeqCst), 2);
                assert!(matches!(
                    vault_ladder_pass(&w, &witness, &row),
                    LadderStep::Held
                ));
                assert_eq!(
                    CALLS.load(Ordering::SeqCst),
                    2,
                    "stable revision stops the immediate retries"
                );
            }
        }
    }

    #[test]
    fn a_partial_vault_apply_backs_off_against_its_delivered_inputs() {
        use super::super::sign_in_ladder::Vault;
        use std::sync::atomic::{AtomicUsize, Ordering};
        static CALLS: AtomicUsize = AtomicUsize::new(0);
        fn partial(home: &Path) -> Vault {
            CALLS.fetch_add(1, Ordering::SeqCst);
            let secrets = home.join(".secrets");
            fs::create_dir_all(&secrets).unwrap();
            fs::write(secrets.join("delivered.txt"), "delivered").unwrap();
            if home.join("bring-mailbox").is_file() {
                fs::write(
                    secrets.join(super::super::headless_oauth::SECRETS_MAILBOX_TOML_FILE),
                    "http_url = \"http://127.0.0.1:9/inbox\"\n",
                )
                .unwrap();
            }
            Vault::AppliedPartly(83)
        }
        for with_mailbox in [false, true] {
            CALLS.store(0, Ordering::SeqCst);
            let (w, row) = vault_ladder_fixture();
            if with_mailbox {
                fs::write(w.home.join("bring-mailbox"), "").unwrap();
            }
            let witness = Witness {
                vault_sync: partial,
                ..Witness::answering("")
            };
            let first = vault_ladder_pass(&w, &witness, &row);
            if with_mailbox {
                let LadderStep::Go { inputs, .. } = first else {
                    panic!("the delivered mailbox must be ready");
                };
                // The sign-in fails after it uses the delivered route.
                witness
                    .guard
                    .lock()
                    .unwrap()
                    .ladder
                    .failed(&row.label, &inputs, Instant::now());
            } else {
                assert!(matches!(first, LadderStep::NoRoute));
            }
            assert!(matches!(
                vault_ladder_pass(&w, &witness, &row),
                LadderStep::Held
            ));
            assert_eq!(CALLS.load(Ordering::SeqCst), 1);
        }
    }

    #[cfg(unix)]
    #[test]
    fn dangling_vault_paths_are_unreadable_not_absent() {
        use std::os::unix::fs::symlink;
        let (w, _) = vault_ladder_fixture();
        let root = w.home.join(".config/aterm/secrets");
        let source = root.join("source.toml");
        let saved = root.join("source.saved");
        fs::rename(&source, &saved).unwrap();
        symlink(root.join("missing"), &source).unwrap();
        assert_eq!(vault_input(&w.home), VaultInput::Unreadable);
        fs::rename(&source, root.join("dangling-source")).unwrap();
        fs::rename(&saved, &source).unwrap();
        symlink(root.join("missing"), root.join("state.toml")).unwrap();
        assert_eq!(vault_input(&w.home), VaultInput::Unreadable);
    }

    /// A row to sign in on a machine with no route, as m2 was on 2026-10-24, whose session on
    /// m2-2 waits at its wall: the vault is refreshed once per wait, the failure is on record,
    /// the mailbox a refresh brings signs the row in, and the session at the wall moves to it.
    #[test]
    fn a_vault_refresh_that_brings_a_mailbox_signs_in_the_row_a_walled_session_moves_to() {
        use super::super::sign_in_ladder::Vault;
        static SYNCS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        fn vault_brings_nothing(_: &Path) -> Vault {
            SYNCS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Vault::Unchanged(51)
        }
        fn vault_brings_a_mailbox(home: &Path) -> Vault {
            let secrets = home.join(".secrets");
            fs::create_dir_all(&secrets).unwrap();
            fs::write(
                secrets.join(super::super::headless_oauth::SECRETS_MAILBOX_TOML_FILE),
                "http_url = \"http://127.0.0.1:9/inbox\"\n",
            )
            .unwrap();
            // A refresh that wrote the mailbox but failed another file still brings it: the
            // row is read again after a partial apply too (`Vault::wrote_files`).
            Vault::AppliedPartly(52)
        }

        let w = World::new();
        let store = accounts::Store::at(&w.state);
        let m2_2_dir = store.logins_dir().join("m2-2");
        fs::create_dir_all(&m2_2_dir).unwrap();
        fs::write(
            m2_2_dir.join(".claude.json"),
            "{\"oauthAccount\":{\"emailAddress\":\"m2-2@example.test\"}}\n",
        )
        .unwrap();
        let mut roster_toml = String::new();
        for label in ["m2", "m2-2"] {
            let row = Row {
                label: label.to_owned(),
                class: Class::Dedicated,
                dir: Some(
                    store
                        .logins_dir()
                        .join(label)
                        .to_string_lossy()
                        .into_owned(),
                ),
                email: Some(format!("{label}@example.test")),
                ..Row::default()
            };
            roster_toml.push_str(&accounts::row_text(&row).unwrap());
            roster_toml.push('\n');
        }
        fs::create_dir_all(&w.config).unwrap();
        fs::write(w.roster(), &roster_toml).unwrap();
        let wall_at = UPKEEP_NOW - 60;
        store
            .transact(UPKEEP_NOW, &mut None, |st| {
                st.active = Some("m2-2".to_owned());
                Ok(accounts::note_wall(
                    st,
                    "m2-2",
                    "seven_day",
                    Some(UPKEEP_NOW + 86_400),
                    wall_at,
                    UPKEEP_NOW,
                ))
            })
            .unwrap();
        let secrets_config = w.home.join(".config/aterm/secrets");
        fs::create_dir_all(&secrets_config).unwrap();
        fs::write(secrets_config.join("source.toml"), "# a vault member\n").unwrap();

        let wit = Witness {
            vault_sync: vault_brings_nothing,
            ..Witness::answering("")
        };
        let pass = |wit: &Witness| {
            auto_renew_next_account(&w.home, &w.state, &w.roster(), UPKEEP_NOW, &wit.fx())
        };
        assert!(!pass(&wit), "no route: nothing started");
        assert_eq!(
            (
                wit.spawned(),
                SYNCS.load(std::sync::atomic::Ordering::SeqCst)
            ),
            (0, 1)
        );
        let tried = store
            .load_logins(UPKEEP_NOW)
            .unwrap()
            .0
            .get("m2")
            .and_then(|b| b.sign_in_attempt.clone())
            .expect("the attempt is on record");
        assert_eq!(tried.outcome, accounts::AttemptOutcome::NoRoute);
        assert!(
            tried.routes.contains("vault(seq 51, nothing new)"),
            "{}",
            tried.routes
        );
        assert!(!pass(&wit));
        assert_eq!(
            SYNCS.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "inside its backoff the row is not read again, and the vault not refreshed again"
        );

        let wit = Witness {
            vault_sync: vault_brings_a_mailbox,
            ..Witness::answering("")
        };
        assert!(pass(&wit), "the mailbox the vault brought is a route");
        assert_eq!(wit.spawned(), 1);
        assert!(wit.guard.lock().unwrap().last_attempt.contains_key("m2"));

        // The sign-in that started ends with m2's login made. A session at m2-2's wall, which
        // had nowhere to go, now moves to m2; the wake file a renewal touches is what makes
        // each waiting session's supervisor re-advise at once (`account_news`, turn_end_loop).
        let file_roster = Roster::load(&w.roster()).unwrap();
        let m2_2_text = m2_2_dir.to_string_lossy().into_owned();
        let advise_at = |at: i64| {
            let (state, _) = store.load(at).unwrap();
            let (logins, _) = store.load_logins(at).unwrap();
            let bound = accounts::bound(&file_roster, &logins);
            let enrolled = accounts::enrolled(&bound);
            let who = accounts::ProcessLogin {
                login: Some(m2_2_text.clone()),
                default_home: true,
                stored_login: true,
            };
            accounts::advise(
                &bound,
                &state,
                &enrolled,
                &who.me(&bound),
                &accounts::Point::Wall {
                    window: "seven_day".to_owned(),
                    at: Some(wall_at),
                },
                accounts::Mode::Wall,
                at,
            )
        };
        assert!(
            !matches!(advise_at(UPKEEP_NOW), accounts::Advice::Move { .. }),
            "{:?}",
            advise_at(UPKEEP_NOW)
        );
        let _ = fs::remove_file(store.wake_path());
        let staging = store.claim_staging_dir("m2").unwrap();
        let staging_text = staging.to_str().unwrap().to_owned();
        fs::write(
            staging.join(".claude.json"),
            "{\"oauthAccount\":{\"emailAddress\":\"m2@example.test\",\"subscriptionType\":\"max\"}}\n",
        )
        .unwrap();
        let status = super::super::align::Captured {
            stdout: b"{\"loggedIn\":true,\"authMethod\":\"claude.ai\",\"email\":\"m2@example.test\",\"subscriptionType\":\"max\"}\n".to_vec(),
            code: Some(0),
        };
        assert!(complete_staging_renewal(
            &store,
            &file_roster,
            "m2",
            &staging,
            &staging_text,
            &status,
            UPKEEP_NOW + 5,
        ));
        assert!(store.wake_path().exists(), "the waiting sessions are woken");
        assert_eq!(
            advise_at(UPKEEP_NOW + 5),
            accounts::Advice::Move {
                to: "m2".to_owned(),
                ahead: false,
                rotates: true,
                takes_canary: false,
            }
        );
        let (logins, _) = store.load_logins(UPKEEP_NOW + 5).unwrap();
        assert_eq!(
            logins.get("m2").and_then(|b| b.sign_in_attempt.clone()),
            None,
            "the login made ends the failure on record"
        );
    }

    /// A row with no mailbox, on a vault member whose refresh gave none, asks the lab for the
    /// row's mailbox password through the need ledger at its no-route outcome; the refresh that
    /// brings the mailbox meets the need, so its ask comes down.
    #[cfg(unix)]
    #[test]
    fn a_row_with_no_mailbox_asks_the_lab_for_it_and_its_arrival_meets_the_need() {
        use super::super::sign_in_ladder::Vault;
        use aterm_secrets::need::{self, Purpose, Want};
        fn nothing_new(_: &Path) -> Vault {
            Vault::Unchanged(3)
        }
        fn brings_a_mailbox(home: &Path) -> Vault {
            let secrets = home.join(".secrets");
            fs::create_dir_all(&secrets).unwrap();
            fs::write(
                secrets.join(super::super::headless_oauth::SECRETS_MAILBOX_TOML_FILE),
                "http_url = \"http://127.0.0.1:9/inbox\"\n",
            )
            .unwrap();
            Vault::Applied(4)
        }
        let w = World::new();
        let store = accounts::Store::at(&w.state);
        let row = Row {
            label: "m29".to_owned(),
            class: Class::Dedicated,
            dir: Some(
                store
                    .logins_dir()
                    .join("m29")
                    .to_string_lossy()
                    .into_owned(),
            ),
            email: Some("m29@example.test".to_owned()),
            ..Row::default()
        };
        fs::create_dir_all(&w.config).unwrap();
        fs::write(
            w.roster(),
            format!("{}\n", accounts::row_text(&row).unwrap()),
        )
        .unwrap();
        let paths = aterm_secrets::client::Paths::for_home(&w.home);
        let source = w.home.join("lab.vault");
        aterm_secrets::client::test_create_unbound(
            &paths,
            source.to_str().unwrap(),
            None,
            Some("m1"),
        )
        .unwrap();
        need::test_alab_policy(&paths);
        let mailbox = Want::parse("mailbox:m29").unwrap();
        let pass = |wit: &Witness| {
            auto_renew_next_account(&w.home, &w.state, &w.roster(), UPKEEP_NOW, &wit.fx())
        };

        let wit = Witness {
            vault_sync: nothing_new,
            ..Witness::answering("")
        };
        assert!(!pass(&wit), "no route");
        let needs = need::peek(&paths).unwrap();
        let asked = needs
            .iter()
            .find(|need| need.want == mailbox)
            .expect("the no-route outcome raised the mailbox need");
        assert_eq!(asked.purpose, Purpose::HarnessSignIn);
        assert!(!asked.state.ended(), "{}", asked.line);
        assert!(
            wit.logged()
                .iter()
                .any(|line| line.contains("the lab is asked for its mailbox password")),
            "{:?}",
            wit.logged()
        );

        let wit = Witness {
            vault_sync: brings_a_mailbox,
            ..Witness::answering("")
        };
        assert!(pass(&wit), "the mailbox the vault brought is a route");
        let met = need::peek(&paths).unwrap();
        let met = met.iter().find(|need| need.want == mailbox).unwrap();
        assert!(
            met.state.ended(),
            "the mailbox's arrival meets the need: {}",
            met.line
        );
        assert!(need::exceptions(&need::peek(&paths).unwrap()).is_empty());
    }
}

#[cfg(test)]
#[path = "accounts_sync_cli_tests.rs"]
mod cli_pool_tests;

#[cfg(test)]
#[path = "accounts_sync_login_tests.rs"]
mod login_tests;

#[cfg(test)]
#[path = "accounts_sync_resync_tests.rs"]
mod resync_tests;

#[cfg(test)]
#[path = "accounts_source_refresh_tests.rs"]
mod source_refresh_tests;

#[cfg(test)]
#[path = "accounts_source_path_tests.rs"]
mod source_path_tests;

#[cfg(test)]
#[path = "accounts_sync_ladder_tests.rs"]
mod ladder_tests;

#[cfg(test)]
#[path = "accounts_attempt_policy_tests.rs"]
mod attempt_policy_tests;

#[cfg(test)]
#[path = "accounts_request_admission_tests.rs"]
mod request_admission_tests;

#[cfg(test)]
#[path = "accounts_sheet_columns_tests.rs"]
mod sheet_columns_tests;

#[cfg(test)]
#[path = "accounts_fleet_assignment_tests.rs"]
mod fleet_assignment_tests;
#[cfg(all(test, unix))]
#[path = "accounts_sync_vault_sheet_tests.rs"]
mod vault_sheet_tests;

#[cfg(all(test, unix))]
#[path = "accounts_hardening_tests.rs"]
mod hardening_tests;
