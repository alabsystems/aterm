// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Fleet and machine account synchronization.
//!
//! Traced by the accounts keeper: what this file reads of the file system itself, it reads
//! through `aterm_uds::trace::fs` (census `traced-reads`).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, BufRead, Read, Seek, SeekFrom, Write};
use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use aterm_json::{Map, Value};
use aterm_uds::trace::{self, Watched};

use super::account_names::Word;
use super::account_sync_error::{Retry, SyncError};
use super::accounts::{self, Class, Cli, Roster, Row};
use super::accounts_step::Step;
use super::accounts_upkeep_memo::{self as memo, Duty};
use super::alab_roster::Chosen;
use super::cli::{ACCOUNTS_OWN_LABEL, Env};
use super::sign_in_ladder::SignIn;
use super::upgrade;
use crate::ai::connector::{Flow, Region, RowKeys};

#[path = "accounts_auto_shared.rs"]
pub mod shared_renewal;

#[path = "accounts_returned.rs"]
mod returned;

#[path = "accounts_source_admission.rs"]
mod source_admission;
#[cfg(test)]
pub(super) use source_admission::MANUAL_EARLY_FLEET;
use source_admission::{AutomaticSource, SourceRequest};
pub(super) use source_admission::{ManualRosterPlan, protect_manual_roster};

#[path = "accounts_provider_source.rs"]
mod provider_source;
use provider_source::ProviderSource;

#[path = "accounts_source_selection.rs"]
mod source_selection;
use source_selection::{Scope as SourceScope, Selection as SelectedSource};

/// Changes outside one source admission are supplied by the environment.
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "AccountSourceCapture",
    action = "Origin",
    reason = "The caller supplies explicit input or an automatic source candidate."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "AccountSourceCapture",
    action = "Change",
    reason = "The source cache may refresh between capture and admission."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "AccountSourceCapture",
    action = "Config",
    reason = "An external writer may change the admitted configuration."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "AccountSourceSelection",
    action = "Automatic",
    reason = "The caller chooses automatic init or a source-less manual sync."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "AccountSourceSelection",
    action = "Key",
    reason = "Configuration can name an explicit source key."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "AccountSourceSelection",
    action = "Named",
    reason = "The beside file can record a source opt-in."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "AccountSourceSelection",
    action = "Beside",
    reason = "An unannotated beside file can be present."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "AccountSourceSelection",
    action = "Pool",
    reason = "The default pool entry can be present."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "AccountSourceSelection",
    action = "Vault",
    reason = "A vault fleet entry can be present."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "AccountSourceSelection",
    action = "Sheet",
    reason = "An accounting sheet entry can be present."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "AccountSourceSelection",
    action = "Retire",
    reason = "The environment supplies the retained source-policy image."
)]
pub fn source_capture_inputs() {}

/// The identity-completion model enumerates supplied observations before each sync.
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "AccountIdentityCompletion",
    action = "AnotherCase",
    reason = "Bounded fixture enumeration; the conformance test executes each selected observation."
)]
pub fn identity_completion_inputs() {}

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

/// Shared fleet config file written beside `accounts.toml`
/// ([`aterm_secrets::entry_class::FLEET_ACCOUNTS`]).
pub const FLEET_ROSTER_FILE: &str = aterm_secrets::entry_class::FLEET_ACCOUNTS;

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

/// One active account in a fleet roster: Claude Code's ([`FleetRoster::from_toml`],
/// [`parse_accounting_csv`]), or another pooled CLI's ([`FleetRoster::from_toml_for`],
/// [`parse_accounting_csv_for`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetAccount {
    /// Canonical roster label (`m8`, `m8-2`, `m106-m38`, …).
    pub label: String,
    /// Normalized machine IDs (`m8`, `m106`, `m38`, …) that own or share this row.
    pub machines: Vec<String>,
    /// Its order on the machines that run it ([`Class`]): from a sheet or fleet file, `Pool`
    /// where it names several machines; from the lab, `Preferred` where this Mac prefers it.
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
    /// The sheet's rows that give one address conflicting assignments ([`SheetConflict`]), each
    /// address in no machine's pool until they agree; none from a fleet file.
    pub conflicts: Vec<SheetConflict>,
    /// The addresses those rows name, lowercased, each with its [`SheetConflict`]: a roster that
    /// already lists one keeps its row as it is where the conflict's member rows name that
    /// machine or none ([`carry_conflicting`]). Never written or said: [`Self::conflicts`] names
    /// the rows.
    pub(crate) conflicting: BTreeMap<String, SheetConflict>,
    /// This Mac's bound registry identity, where the lab's preferences made this fleet
    /// ([`super::alab_roster::fleet_for`]): a sync then keeps every row it has
    /// ([`super::alab_roster::retain`]) and writes no fleet file beside the roster.
    pub alab: Option<super::alab_roster::MachineRef>,
    /// The registry projection that produced this native fleet. A sync reads one snapshot.
    pub(crate) lab_snapshot: Option<std::sync::Arc<super::alab_roster::Lab>>,
    /// The lab's other accounts of this CLI that may join, which this Mac does not prefer, each
    /// under its lab label ([`super::alab_roster::fleet_for`]): never added unasked, only the name the
    /// default login's account takes ([`build_machine_roster`]). Never written or said.
    pub(crate) known: Vec<FleetAccount>,
    /// Whether discovery made this fleet ([`OptIn::Discovered`]): over a roster that lists rows, a
    /// sync then keeps every row as a person listed it and only adds an account no row holds
    /// ([`keep_discovered_rows`]). Set from the file's `opt_in` as a sync reads it, never written.
    pub(crate) keeps_rows: bool,
    /// Each account the lab prefers here that was left out because an existing row of another
    /// account holds its label: `(label, address)` ([`super::alab_roster::fleet_for`]). A sync's
    /// receipt names them ([`CliSynced::clashes`]); never written.
    pub(crate) clashes: Vec<(String, String)>,
}

impl FleetRoster {
    /// Filter by exact native key in preference order, or by a legacy machine label in slot order.
    #[must_use]
    pub fn accounts_for_machine(&self, machine: &str) -> Vec<&FleetAccount> {
        if self.alab.is_some() {
            // Registry keys are opaque. Keep the preference order established by fleet_for.
            let mut rows: Vec<_> = self
                .accounts
                .iter()
                .filter(|account| account.machines.iter().any(|key| key == machine))
                .collect();
            rows.sort_by_key(|account| account.class);
            return rows;
        }
        let norm = normalize_machine_id(machine).unwrap_or_else(|| machine.to_ascii_lowercase());
        let mut rows: Vec<&FleetAccount> = self
            .accounts
            .iter()
            .filter(|a| a.machines.iter().any(|m| m == &norm))
            .collect();
        rows.sort_by_key(|a| (a.class, a.label.clone()));
        rows
    }

    /// The conflicts ([`Self::conflicts`]) `machine`'s pool skips: those whose member rows name
    /// it or name none ([`conflict_names`]; every one where `machine` is unknown).
    #[must_use]
    pub fn conflicts_for(&self, machine: Option<&str>) -> Vec<SheetConflict> {
        self.conflicts
            .iter()
            .filter(|conflict| conflict_names(conflict, machine))
            .cloned()
            .collect()
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
            out.push_str(&format!(
                "class = {}\n",
                toml_quote(acct.class.legacy_word())
            ));
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

    /// Parse `fleet-accounts.toml` text back into a [`FleetRoster`]. A row's order is the one its
    /// machine list derives ([`fleet_assignment`]); a `class` word the file stores is not read.
    /// # Errors
    /// Returns a human-readable error if the TOML is invalid, `account` is not an array of tables,
    /// or any row is missing required fields, carries a refused label or email, or has a
    /// malformed machine assignment.
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
                ..Self::default()
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
            ..Self::default()
        })
    }
}

/// Row `n`'s machine list, admitted whole or refused, normalized before stable deduplication so
/// repeated spellings of one host cannot change its order, and the order the list derives:
/// [`Class::Preferred`] for one machine, [`Class::Pool`] for several. An order alone, never a
/// limit, and never read from a `class` word (design §0; `FleetMachineAssignment`'s file area).
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
        action = "StoredClass",
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
    // The order is the list's, as every legacy reader infers it with no `class` written; a
    // `class` word in the file is an older writer's and is not read (design §0: no class).
    let class = if machines.len() > 1 {
        Class::Pool
    } else {
        Class::Preferred
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

/// Parse the accounting CSV into a [`FleetRoster`]. Rows that give one address conflicting
/// assignments are skipped, that address alone ([`SheetConflict`], [`sheet_conflicts`]); the
/// rest of the sheet is read.
/// # Errors
/// Returns an error if the CSV has no header row, missing or ambiguous identity/membership
/// columns, an ambiguous required machine-layout marker, or malformed quoting.
/// Invalid generated account labels and exhaustion of their disambiguating suffixes also fail.
/// LF and CRLF separate records only outside quotes.
pub fn parse_accounting_csv(csv_text: &str, source: &str) -> Result<FleetRoster, String> {
    parse_sheet(csv_text, source, Cli::Claude).map(|sheet| sheet.roster)
}

/// Rows of the sheet that give one address conflicting assignments for one CLI's pool: rows for
/// other machines, a slot or a vendor setting each, or a row that is a member beside one that
/// disqualifies it (a note, a `0`, a value the column does not take). Source order cannot choose
/// between them, so no machine's pool adds the address until its rows agree (a roster that
/// already lists it keeps its row where the member rows name its machine or none,
/// [`carry_conflicting`]), and the rest of the sheet is read
/// ([`parse_accounting_csv_for`]). No address is kept here: the rows name it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SheetConflict {
    /// The CLI whose column the rows conflict in.
    pub cli: Cli,
    /// The sheet's rows that name the address, in order (the header is row 1).
    pub rows: Vec<usize>,
    /// The machines its member rows name.
    pub machines: Vec<String>,
}

/// One reading of the sheet for one CLI's column ([`parse_sheet`]).
struct Sheet {
    roster: FleetRoster,
    held: Vec<FleetAccount>,
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
/// vault's own sheet, or `grok`) for Grok Build; `kimi` for Kimi Code; `agy` or `antigravity`
/// for Antigravity. Google account status alone does not enroll an Antigravity subscription.
/// A header word is matched
/// whole; a note's vendor words have no lone `x`, so `x` scopes no note.
#[must_use]
pub const fn csv_columns(cli: Cli) -> &'static [&'static str] {
    match cli {
        Cli::Claude => &["claude"],
        Cli::Codex => &["open ai", "openai", "codex"],
        Cli::Grok => &["xai", "x", "grok"],
        Cli::Kimi => &["kimi"],
        Cli::Agy => &["agy", "antigravity"],
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
/// Rows that give one address conflicting assignments leave that address alone out
/// ([`FleetRoster::conflicts`]).
/// # Errors
/// As [`parse_accounting_csv`], with `cli`'s column in place of `claude`.
pub fn parse_accounting_csv_for(
    cli: Cli,
    csv_text: &str,
    source: &str,
) -> Result<CliFleet, String> {
    parse_sheet(csv_text, source, cli).map(|sheet| CliFleet {
        cli,
        roster: sheet.roster,
        held: sheet.held,
    })
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

/// What one sheet row says of an address for a CLI's pool ([`parse_sheet`]): a member's
/// assignment, or `None` for a row that claims or explicitly disqualifies it, which an active
/// row of the same address then conflicts with rather than replaces.
struct SheetRow {
    row: usize,
    email: String,
    account: Option<SheetAccount>,
}

/// The addresses (lowercased) `rows` give conflicting assignments, each as its
/// [`SheetConflict`] for `cli`.
fn sheet_row_conflicts(cli: Cli, rows: &[SheetRow]) -> BTreeMap<String, SheetConflict> {
    let mut by_address: BTreeMap<String, Vec<&SheetRow>> = BTreeMap::new();
    for row in rows {
        by_address
            .entry(row.email.to_ascii_lowercase())
            .or_default()
            .push(row);
    }
    by_address
        .into_iter()
        .filter(|(_, rows)| rows.iter().any(|row| row.account != rows[0].account))
        .map(|(address, rows)| {
            let mut machines: Vec<String> = Vec::new();
            for machine in rows
                .iter()
                .filter_map(|row| row.account.as_ref())
                .flat_map(|account| account.machines.iter())
            {
                if !machines.contains(machine) {
                    machines.push(machine.clone());
                }
            }
            let conflict = SheetConflict {
                cli,
                rows: rows.iter().map(|row| row.row).collect(),
                machines,
            };
            (address, conflict)
        })
        .collect()
}

fn member_of(cli: Cli, cell: &str) -> Member {
    let word = cell.to_ascii_lowercase();
    let vendor = |region, flow| Member::Yes(accounts::Vendor { region, flow });
    match (cli.accounts().keys, word.as_str()) {
        (RowKeys::Region, "1" | "1.0") => Member::Held,
        (RowKeys::Region, "global" | "kimi.ai") => vendor(Some(Region::Global), None),
        (RowKeys::Region, "mainland-cn" | "cn" | "kimi.com") => {
            vendor(Some(Region::MainlandCn), None)
        }
        (RowKeys::Flow(_), word) if Flow::parse(word).is_some() => vendor(None, Flow::parse(word)),
        (RowKeys::Plain | RowKeys::Flow(_), "1" | "1.0" | "0.5" | "2.5") => vendor(None, None),
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
/// Where `google`'s words sit in [`NOTE_VENDORS`].
const GOOGLE_NOTES: usize = 4;
/// Where `meta`'s words sit in [`NOTE_VENDORS`].
const META_NOTES: usize = 5;

/// Whether `notes` disqualify a row's membership of `cli`'s pool: a disqualifying word
/// ([`notes_disqualify`]) in a note that names no vendor, or names `cli`'s ([`NOTE_VENDORS`],
/// whole words).
fn notes_disqualify_for(cli: Cli, notes: &str) -> bool {
    NOTE_VENDORS
        .iter()
        .position(|(vendor, _)| *vendor == Some(cli))
        .is_some_and(|at| notes_disqualify_at(at, notes))
}

/// Whether `notes` disqualify the vendor at `at` in [`NOTE_VENDORS`]: a disqualifying word in a
/// note that names no vendor, or names that one (whole words).
fn notes_disqualify_at(at: usize, notes: &str) -> bool {
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
    let named: Vec<usize> = NOTE_VENDORS
        .iter()
        .enumerate()
        .filter(|(_, (_, phrases))| phrases.iter().any(|p| names(p)))
        .map(|(index, _)| index)
        .collect();
    named.is_empty() || named.contains(&at)
}

/// One of the accounting sheet's provider columns as the lab importer reads it (`aterm db alab
/// import`, docs/DESIGN-alab-lab-model-2026-11-01.md §9): a pooled CLI's ([`csv_columns`]), or
/// one of the two no pool reads, which a note scopes by its own words ([`NOTE_VENDORS`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SheetColumn {
    /// A pooled CLI's column.
    Pool(Cli),
    /// `google`.
    Google,
    /// `meta`.
    Meta,
}

/// What one provider cell of the sheet says beside its row's notes ([`read_sheet_cell`]), as
/// the harness's own reading of the sheet takes it ([`parse_accounting_csv_for`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SheetCell {
    /// Empty, `0`, `no` or `false`: not a member.
    Absent,
    /// A member, with the vendor keys the cell names.
    Member(accounts::Vendor),
    /// A member the row's notes disqualify for this vendor (`kimi banned`), or for every vendor.
    Disqualified(accounts::Vendor),
    /// A Kimi Code member with no region (a bare `1`), held out of the pool
    /// ([`CliFleet::held`]): a sign-in on the wrong region can make a new account.
    Held,
    /// A value the column does not take: free text, which the importer keeps as its coordinates.
    Unread,
}

/// Read one provider cell of the accounting sheet beside its row's `notes` (the row's cells
/// from its `notes` column on, joined by spaces, as [`parse_accounting_csv_for`] joins them),
/// with the harness's own rules: `member_of` for a pooled CLI's column (`claude` takes only
/// `1`, as [`parse_accounting_csv`] does), `1` for `google` and `meta`; a disqualifying note
/// drops Claude Code's membership whatever vendor it names, and any other column's only when
/// it names that vendor or none (`notes_disqualify_for`). A held cell its notes disqualify
/// reads as disqualified with no region.
#[must_use]
pub fn read_sheet_cell(column: SheetColumn, cell: &str, notes: &str) -> SheetCell {
    let cell = cell.trim();
    let plain = || {
        if matches!(cell, "1" | "1.0" | "0.5" | "2.5") {
            Member::Yes(accounts::Vendor::default())
        } else {
            Member::No
        }
    };
    let (member, disqualified) = match column {
        SheetColumn::Pool(Cli::Claude) => (plain(), notes_disqualify(notes)),
        SheetColumn::Pool(cli) => (member_of(cli, cell), notes_disqualify_for(cli, notes)),
        SheetColumn::Google => (plain(), notes_disqualify_at(GOOGLE_NOTES, notes)),
        SheetColumn::Meta => (plain(), notes_disqualify_at(META_NOTES, notes)),
    };
    match member {
        Member::No
            if cell.is_empty()
                || ["0", "0.0", "no", "false"]
                    .iter()
                    .any(|word| cell.eq_ignore_ascii_case(word)) =>
        {
            SheetCell::Absent
        }
        Member::No => SheetCell::Unread,
        Member::Held if disqualified => SheetCell::Disqualified(accounts::Vendor::default()),
        Member::Held => SheetCell::Held,
        Member::Yes(vendor) if disqualified => SheetCell::Disqualified(vendor),
        Member::Yes(vendor) => SheetCell::Member(vendor),
    }
}

/// The one reading of the accounting sheet, for `cli`'s column ([`parse_accounting_csv`],
/// [`parse_accounting_csv_for`]): its rows, the rows it held ([`CliFleet::held`]), and the
/// addresses whose rows conflict ([`FleetRoster::conflicts`]), each of which takes the label its
/// first member row gives it and is then left out. So a conflicting row below that one, or one
/// that disqualifies the address, moves no other account's label; a conflicting member row above
/// it can, as any row added above another can.
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
fn parse_sheet(csv_text: &str, source: &str, cli: Cli) -> Result<Sheet, String> {
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

    let mut observed: Vec<SheetRow> = Vec::new();
    for (index, record) in records.enumerate() {
        let row = index + 2;
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
            Cli::Claude if matches!(member_val, "1" | "1.0" | "0.5" | "2.5") => {
                Member::Yes(accounts::Vendor::default())
            }
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
                || ["0", "0.0", "no", "false"]
                    .iter()
                    .any(|word| member_val.eq_ignore_ascii_case(word));
            if disqualified || !inactive {
                observed.push(SheetRow {
                    row,
                    email: email.to_owned(),
                    account: None,
                });
            }
            continue;
        }
        if disqualified {
            observed.push(SheetRow {
                row,
                email: email.to_owned(),
                account: None,
            });
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
            observed.push(SheetRow {
                row,
                email: email.to_owned(),
                account: None,
            });
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

        observed.push(SheetRow {
            row,
            email: email.to_owned(),
            account: Some(SheetAccount {
                machines,
                slot: slot.filter(|slot| *slot > 1),
                member,
            }),
        });
    }

    let conflicting = sheet_row_conflicts(cli, &observed);
    let mut labelled = BTreeSet::new();
    let mut used_labels = BTreeSet::new();
    let mut accounts = Vec::new();
    let mut held = Vec::new();
    for seen in &observed {
        let Some(SheetAccount {
            machines,
            slot,
            member,
        }) = &seen.account
        else {
            continue;
        };
        let address = seen.email.to_ascii_lowercase();
        if !labelled.insert(address.clone()) {
            continue;
        }
        let (machines, slot, member) = (machines.clone(), *slot, *member);
        let email = seen.email.as_str();
        let conflicted = conflicting.contains_key(&address);
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
        let mut refused = None;
        loop {
            if let Some(why) = accounts::label_refusal(&label) {
                refused = Some(format!(
                    "CSV at {source} generates an account label that {why}; \
                     give this machine assignment an explicit label in fleet-accounts.toml"
                ));
                break;
            }
            if !used_labels.contains(&label) {
                break;
            }
            label = format!("{base_label}-{suffix}");
            let Some(next) = suffix.checked_add(1) else {
                refused = Some(format!("CSV at {source} exhausts account label suffixes"));
                break;
            };
            suffix = next;
        }
        match refused {
            // A conflicting address is left out whatever label it would take.
            Some(_) if conflicted => continue,
            Some(why) => return Err(why),
            None => {}
        }
        used_labels.insert(label.clone());
        if conflicted {
            continue;
        }

        let class = if machines.len() > 1 {
            Class::Pool
        } else {
            Class::Preferred
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

    Ok(Sheet {
        roster: FleetRoster {
            source: source.to_owned(),
            accounts,
            conflicts: conflicting.values().cloned().collect(),
            conflicting,
            alab: None,
            lab_snapshot: None,
            known: Vec::new(),
            keeps_rows: false,
            clashes: Vec::new(),
        },
        held,
    })
}

fn config_machine_id(config: Option<&Path>) -> Option<String> {
    let cfg_path = config?;
    let text = super::ai_pool::read_bounded_text(cfg_path, 2 * 1024 * 1024)?;
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

#[cfg(test)]
thread_local! {
    /// How often this thread asked the host its name ([`detect_machine_id`]).
    static HOST_ASKED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Detect this machine's canonical ID from an override, config file, or system hostname. Never
/// from `~/.aterm/machine.toml`: the vault hands every Mac the same file, whose `id` names a
/// release machine, so a Mac with a default hostname would take that machine's accounts. This
/// crate's tests read neither the host's names nor the person's home: they name the machine by an
/// override or a config, as `ai_pool`'s host names are none in them.
#[must_use]
pub fn detect_machine_id(override_machine: Option<&str>, config: Option<&Path>) -> Option<String> {
    if let Some(m) = override_machine {
        return normalize_machine_id(m);
    }
    if let Some(m) = config_machine_id(config) {
        return Some(m);
    }
    #[cfg(test)]
    HOST_ASKED.with(|asked| asked.set(asked.get() + 1));
    if cfg!(test) {
        return None;
    }
    #[cfg(windows)]
    {
        machine_id_among(windows_host_names())
    }
    #[cfg(not(windows))]
    {
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
        machine_id_among(names)
    }
}

/// This machine's own names on Windows, read in process (`GetComputerNameExW`): its DNS host
/// name, then its NetBIOS name, which is what `%COMPUTERNAME%` holds. Windows has neither the
/// `scutil` nor the `/bin/hostname` the other hosts are asked through, so nothing is started.
#[cfg(windows)]
pub(super) fn windows_host_names() -> Vec<String> {
    [
        aterm_uds::process::kernel_host_name(),
        aterm_uds::process::netbios_host_name(),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// [`detect_machine_id`]'s choice past an override and a config: the first of `names` (the
/// host's, each asked only once those before it named no machine) that [`normalize_machine_id`]
/// reads as one.
fn machine_id_among(names: impl IntoIterator<Item = String>) -> Option<String> {
    names
        .into_iter()
        .find_map(|name| normalize_machine_id(&name))
}

/// The address the profile of the home `env` names is signed in as.
pub(crate) fn read_default_profile_email(env: &Env) -> Option<String> {
    signed_in_email(&default_profile_path(env)?)
}

fn default_profile_path(env: &Env) -> Option<PathBuf> {
    env.claude_profile.as_deref().map(|own| {
        super::profile::effective(own, env.claude_dir().as_deref(), env.claude_custom_oauth)
    })
}

/// The default login's address at `home`, as a sync from init reads it ([`quiet_env`]).
fn home_default_email(home: &Path) -> Option<String> {
    signed_in_email(&super::profile::effective(
        &home.join(".claude.json"),
        Some(&home.join(".claude")),
        false,
    ))
}

/// Whether the default login's profile under the person's home `home` names `email`: a known
/// match ([`accounts::email_matches`]). A profile that names another account, does not read or
/// names none answers no (`login_adoption::Witnesses`).
pub(super) fn default_profile_names(home: &Path, email: &str) -> bool {
    home_default_email(home).is_some_and(|found| accounts::email_matches(email, &found))
}

/// Match a checked default login to its current row without trusting its label
/// as provider identity. A row with no expected identity denotes the default
/// login itself; named identities require a readable matching provider record.
pub(crate) fn default_login_identity_matches(cli: Cli, home: &Path, row: &Row) -> bool {
    if row.dir.is_some() {
        return false;
    }
    let Some(expected) = row.expected_identity_email() else {
        return true;
    };
    let found = match cli {
        Cli::Claude => home.parent().and_then(home_default_email),
        Cli::Codex => super::codex_app_login::read_account_email(home),
        Cli::Grok | Cli::Kimi | Cli::Agy => None,
    };
    found.is_some_and(|found| accounts::email_matches(expected, &found))
}

/// Policy and credential parent captured before an external login or review.
#[derive(Clone, Debug)]
pub(crate) struct LoginWitness {
    store: PathBuf,
    roster: PathBuf,
    row: Row,
    parent: accounts::RenewalParent,
    observed_at: i64,
    sign_in_receipt: Option<u64>,
    /// Local file routing before the run; this does not prove what an external CLI cached.
    auth_context: Option<accounts::AuthContext>,
}

/// A review's account, or the unassigned default login whose scope was observed.
#[derive(Clone, Debug)]
pub(crate) enum ReviewWitness {
    Listed(LoginWitness),
    Unmanaged {
        source: accounts::UsageRoster,
        roster: Roster,
        generation: u64,
        sign_in_clock: u64,
    },
}

impl ReviewWitness {
    #[must_use]
    pub(crate) fn row(&self) -> Option<&Row> {
        match self {
            Self::Listed(witness) => Some(&witness.row),
            Self::Unmanaged { .. } => None,
        }
    }

    #[must_use]
    pub(crate) fn login(&self, cli: Cli) -> (&'static str, Option<String>) {
        self.row().map_or_else(
            || accounts::login_env_for(cli, &Row::default()),
            |row| accounts::login_env_for(cli, row),
        )
    }
}

/// The login witness and recovery advice from one account-writer snapshot.
#[derive(Clone, Debug)]
pub(crate) struct ReviewCapture {
    pub(crate) witness: ReviewWitness,
    pub(crate) pinned: Option<String>,
    pub(crate) default_row: Option<super::codex_alab::DefaultRow>,
}

/// Resolve the serving login from current records before a review wave starts.
/// An unassigned default is observed without creating an implicit account record.
/// # Errors
/// The provider differs from the store, records cannot be read or recovered, the
/// writer is unavailable, or the selected account cannot serve a review.
pub(crate) fn capture_current_review(
    store: &accounts::Store,
    cli: Cli,
    roster_path: Option<&Path>,
    mode: accounts::Mode,
    program: Option<&Path>,
    now: i64,
) -> Result<ReviewCapture, String> {
    if store.cli() != cli {
        return Err("review provider differs from its account store".into());
    }
    let source = store
        .read_usage_roster(roster_path, now)
        .map_err(|why| match why {
            accounts::RosterReadError::Roster(why) | accounts::RosterReadError::Records(why) => why,
        })?;
    store.inspect_usage_within(&accounts::LOCK_LADDER, &source, now, |records| {
        let enrolled = store.enrolled_here(&records.roster);
        let selected = if cli == Cli::Claude {
            if records.raw_roster.rows.is_empty() {
                None
            } else if mode == accounts::Mode::Off {
                // Rotation off picks no row of the pool: a review starts on the login it is
                // started with, this machine's own, as a new session does.
                records.roster.row_for_login(None)
            } else {
                let (row, ready) = crate::ai::facts::select_claude_serving_row(
                    &records.roster,
                    &records.state,
                    &enrolled,
                    now,
                )
                .ok_or_else(|| "the Claude roster has no serving row".to_owned())?;
                if !ready {
                    return Err("no Claude account can serve a review now".into());
                }
                Some(row)
            }
        } else if cli == Cli::Codex {
            let home = store
                .user_home()
                .ok_or("the Codex review home is unavailable")?;
            let sync =
                roster_path.map(|roster| super::accounts_pool::SyncHere { store, roster, now });
            super::accounts_pool::launch_observed(
                (cli, false, mode),
                &records.roster,
                &records.state,
                &enrolled,
                program.or(Some(Path::new("codex"))),
                now,
                (home, sync),
            )?
            .or_else(|| records.roster.row_for_login(None))
        } else {
            super::accounts_pool::launch_now(
                (cli, false, mode),
                &records.roster,
                &records.state,
                &enrolled,
                program.or(Some(Path::new("codex"))),
                now,
            )?
            .or_else(|| records.roster.row_for_login(None))
        };
        let witness = if let Some(row) = selected {
            records
                .state
                .execution_verdict(&row.label)
                .map_err(|why| format!("{} account {}: {}", cli.id(), row.label, why.as_str()))?;
            // The launch policy already judged readiness. In its no-row case a
            // lone default login runs as it stands, even while signed out; retain
            // its witness without inventing a stricter policy for reviews.
            let path = roster_path.ok_or_else(|| "listed account has no roster path".to_owned())?;
            ReviewWitness::Listed(witness_from_row(
                store,
                path,
                row.clone(),
                &records.logins,
                now,
            ))
        } else {
            ReviewWitness::Unmanaged {
                source: source.clone(),
                roster: records.roster.clone(),
                generation: records.logins.generation_clock,
                sign_in_clock: records.logins.sign_in_clock,
            }
        };
        let default_row =
            (cli == Cli::Codex).then(|| {
                match super::accounts_pool::default_serving(
                    &records.roster,
                    &records.state,
                    &enrolled,
                    now,
                ) {
                    Ok(row) => super::codex_alab::DefaultRow::serves(row, &records.roster.rows),
                    Err(why) => why,
                }
            });
        Ok(ReviewCapture {
            witness,
            pinned: records
                .state
                .active
                .clone()
                .filter(|_| records.state.pinned),
            default_row,
        })
    })
}

/// Commit an outcome only while the login observed by this wave remains current.
/// # Errors
/// The provider, account or source changed, the writer is unavailable, or the
/// account records cannot be read, recovered or committed.
pub(crate) fn record_current_review(
    store: &accounts::Store,
    cli: Cli,
    witness: &ReviewWitness,
    home: &Path,
    outcome: crate::ai::store::Outcome,
    now: i64,
) -> Result<Vec<accounts::Event>, String> {
    if store.cli() != cli {
        return Err("review provider differs from its account store".into());
    }
    match witness {
        ReviewWitness::Listed(witness) => {
            record_review_login(store, cli, &witness.roster, witness, home, outcome, now)
        }
        ReviewWitness::Unmanaged {
            source,
            roster,
            generation,
            sign_in_clock,
        } => {
            let none =
                store.inspect_usage_within(&accounts::LOCK_LADDER, source, now, |records| {
                    if records.roster != *roster
                        || records.logins.generation_clock != *generation
                        || records.logins.sign_in_clock != *sign_in_clock
                    {
                        return Err(
                            "default login scope changed during the review; nothing recorded"
                                .into(),
                        );
                    }
                    Ok(Vec::new())
                })?;
            // No row lists this login, so the store holds no record of it; what the review
            // learned of its account still reaches the journal, under the account's own key.
            super::journal::publish(&unlisted_login_notes(cli, home, outcome, now));
            Ok(none)
        }
    }
}

/// What a check of `cli`'s default login at `home` learned of the account its own files name,
/// where no roster row lists that login: a login that ended, or one that served. The accounts
/// store keys its records by row, and an unlisted login has none (an implicit record under a
/// synthetic label would be taken for a row's by whatever row later gets that label), so the
/// fact goes to the journal under the account's own key, as an unlisted tab's quota does. None
/// where the files name no account (Grok, Kimi Code and agy keep none aterm reads; a home
/// nobody signed in) or a usage limit was all it learned.
pub(crate) fn unlisted_login_notes(
    cli: Cli,
    home: &Path,
    outcome: crate::ai::store::Outcome,
    now: i64,
) -> Vec<super::journal::Note> {
    let working = match outcome {
        crate::ai::store::Outcome::Worked => true,
        crate::ai::store::Outcome::Ended(_) => false,
        crate::ai::store::Outcome::Limited(_) => return Vec::new(),
    };
    let email = match cli {
        Cli::Claude => home.parent().and_then(home_default_email),
        Cli::Codex => super::codex_app_login::read_account_email(home),
        Cli::Grok | Cli::Kimi | Cli::Agy => None,
    };
    email
        .and_then(|email| super::journal::unlisted_login_note(cli, &email, working, now))
        .into_iter()
        .collect()
}

/// Capture the current default row for a local sign-in outside shared custody.
pub(crate) fn capture_default_login(
    store: &accounts::Store,
    roster_path: &Path,
    label: &str,
    now: i64,
) -> Option<LoginWitness> {
    let records = store.read_account_records(Some(roster_path), now).ok()?;
    let row = records.roster.row(label)?.clone();
    if row.dir.is_some() || records.logins.shared.required(&row).ok()?.is_some() {
        return None;
    }
    Some(witness_from_row(
        store,
        roster_path,
        row,
        &records.logins,
        now,
    ))
}

/// Capture only the account whose current login seam the review will actually use.
#[cfg(test)]
pub(crate) fn capture_review_login(
    store: &accounts::Store,
    cli: Cli,
    roster_path: &Path,
    label: &str,
    login: &(&'static str, Option<String>),
    now: i64,
) -> Option<LoginWitness> {
    capture_login(store, roster_path, label, now)
        .filter(|witness| accounts::login_env_for(cli, &witness.row) == *login)
}

#[cfg(test)]
fn capture_login(
    store: &accounts::Store,
    roster_path: &Path,
    label: &str,
    now: i64,
) -> Option<LoginWitness> {
    let records = store.read_account_records(Some(roster_path), now).ok()?;
    let row = records.roster.row(label)?.clone();
    Some(witness_from_row(
        store,
        roster_path,
        row,
        &records.logins,
        now,
    ))
}

fn witness_from_row(
    store: &accounts::Store,
    roster_path: &Path,
    row: Row,
    logins: &accounts::Logins,
    now: i64,
) -> LoginWitness {
    LoginWitness {
        store: store.dir.clone(),
        roster: roster_path.to_path_buf(),
        parent: accounts::RenewalParent::capture(logins, &row.label),
        sign_in_receipt: logins.sign_in_receipts.get(&row.label).copied(),
        auth_context: store.capture_auth_context(&row),
        row,
        observed_at: now,
    }
}

/// Captures the credentials before a fresh status process, retaining the original
/// sign-in operation's policy and receipt fences across its intentional replacement.
pub(crate) fn capture_status_context(
    store: &accounts::Store,
    witness: &LoginWitness,
) -> LoginWitness {
    let mut status = witness.clone();
    status.auth_context = store.capture_auth_context(&witness.row);
    status
}

/// Record a checked default login against its pre-run witness. Only a turn
/// (`served`) refreshes usage metadata; sign-in alone never creates a binding.
/// A signed-out row needs current provider identity before its auth can heal.
#[cfg(test)]
pub(crate) fn record_default_sign_back(
    store: &accounts::Store,
    cli: Cli,
    roster_path: &Path,
    witness: &LoginWitness,
    home: &Path,
    served: bool,
    now: i64,
) -> Result<Vec<accounts::Event>, String> {
    transact_login_observation(store, roster_path, witness, now, |row, _, state, logins| {
        if row.dir.is_some() {
            return Err("default login observation names an isolated login".into());
        }
        if logins.shared.required(row)?.is_some() {
            return Err("this account requires a fresh root-authorized shared sign-in".into());
        }
        observe_default_success(cli, row, state, logins, home, served, now)
    })
}

/// Whether a checked provider login belongs to the selected account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DefaultSignIn {
    /// The verified login and its new success receipt were committed.
    Accepted,
    /// The identity was unverified; the current account was recorded signed out.
    WrongIdentity,
}

fn checked_codex_auth_context(
    store: &accounts::Store,
    row: &Row,
) -> Option<(accounts::AuthContext, bool)> {
    let user_home = store.user_home()?;
    let default = store.default_home()?;
    let home = row
        .dir
        .as_deref()
        .filter(|dir| !dir.is_empty())
        .map_or(default.as_path(), Path::new);
    let observed = super::codex_app_login::current_login_context(
        home,
        user_home,
        row.expected_identity_email(),
    )?;
    Some((
        accounts::AuthContext::new(row, observed.context),
        observed.identity_matches,
    ))
}

/// Commit an explicit default-home sign-in after its successful provider status check.
/// Every success verifies identity and advances the sign-in receipt, including a
/// first login or replacement of credentials removed outside aterm.
/// A wrong identity records the current row signed out and cannot heal it.
/// # Errors
/// The captured account or sign-in changed, or
/// its account transaction cannot be committed.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountExplicitSignIn",
        action = "Binding",
        reason = "Select whether the sign-in fixture has an existing login binding."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountExplicitSignIn",
        action = "Case",
        reason = "Select the identity and receipt fixture before completing the sign-in."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountExplicitSignIn",
        action = "Complete",
        project = "aterm_agent::harness::accounts_sync::signback_receipt_tests::explicit_signin_transition"
    )
)]
pub(crate) fn record_default_sign_in(
    store: &accounts::Store,
    cli: Cli,
    roster_path: &Path,
    witness: &LoginWitness,
    home: &Path,
    now: i64,
) -> Result<DefaultSignIn, String> {
    if store.cli() != cli {
        return Err("login provider differs from its account store".into());
    }
    let mut outcome = DefaultSignIn::WrongIdentity;
    transact_login_observation(
        store,
        roster_path,
        witness,
        now,
        |row, raw_row, state, logins| {
            if row.dir.is_some() {
                return Err("default login observation names an isolated login".into());
            }
            if logins.shared.required(row)?.is_some() {
                return Err("this account requires a fresh root-authorized shared sign-in".into());
            }
            if logins.sign_in_receipts.get(&row.label).copied() != witness.sign_in_receipt {
                return Err("a newer sign-in superseded this login; nothing recorded".into());
            }
            if state.accounts.get(&row.label).is_some_and(|account| {
                (state.contextual_auth || account.current_auth(row) != accounts::Auth::Unseen)
                    && account.auth_at.is_some_and(|held| held > now)
            }) {
                return Err(
                    "a newer authentication observation superseded this login; nothing recorded"
                        .into(),
                );
            }
            if cli == Cli::Codex {
                let (context, identity_matches) = checked_codex_auth_context(store, row)
                    .ok_or("sign-in credentials cannot be read; nothing recorded")?;
                if witness.auth_context.as_ref() != Some(&context) {
                    return Err(
                        "credentials changed during sign-in status; nothing recorded".into(),
                    );
                }
                if !identity_matches {
                    accounts::stage_login_end(state, raw_row, logins, context, now);
                    return Ok(Vec::new());
                }
                accounts::stage_checked_sign_in(state, raw_row, logins, context, now);
                outcome = DefaultSignIn::Accepted;
                return Ok(Vec::new());
            }
            if !default_login_identity_matches(cli, home, row) {
                return Ok(accounts::note_login_end(state, logins, &row.label, now));
            }
            let events = accounts::note_checked_sign_in(state, logins, &row.label, now);
            if events.is_empty() {
                return Err("the verified sign-in could not be recorded".into());
            }
            outcome = DefaultSignIn::Accepted;
            Ok(events)
        },
    )?;
    Ok(outcome)
}

fn observe_default_success(
    cli: Cli,
    row: &Row,
    state: &mut accounts::State,
    logins: &mut accounts::Logins,
    home: &Path,
    served: bool,
    now: i64,
) -> Result<Vec<accounts::Event>, String> {
    let label = &row.label;
    // Where the word on record signs the row out (or is of a replaced login), what this check
    // found signs the row back in only once the default login's identity is the row's.
    if state.accounts.get(label).is_some_and(|account| {
        account.auth == accounts::Auth::SignedOut
            || (!state.contextual_auth
                && account.auth != accounts::Auth::Unseen
                && account.current_auth(row) == accounts::Auth::Unseen)
    }) {
        if !default_login_identity_matches(cli, home, row) {
            return Err(
                "default login identity changed or cannot be verified; nothing recorded".into(),
            );
        }
        let events = accounts::note_checked_sign_in(state, logins, label, now);
        if served {
            accounts::note_served_login(logins, label, now);
        }
        return Ok(events);
    }
    if served
        && state.accounts.get(label).is_none_or(|account| {
            (!state.contextual_auth && account.current_auth(row) == accounts::Auth::Unseen)
                || account.auth_at.is_none_or(|at| at <= now)
        })
    {
        accounts::note_served_login(logins, label, now);
    }
    Ok(Vec::new())
}

/// Records an explicit sign-in's final signed-out status, which may describe newly
/// written credentials. The pre-run policy and sign-in receipt still fence this result.
///
/// # Errors
///
/// The writer or records are unavailable, policy or a newer sign-in superseded this
/// operation, or credentials changed or became unavailable during the status read.
pub(crate) fn record_default_sign_out(
    store: &accounts::Store,
    cli: Cli,
    roster_path: &Path,
    witness: &LoginWitness,
    home: &Path,
    now: i64,
) -> Result<Vec<accounts::Event>, String> {
    if cli != Cli::Codex {
        return record_review_login(
            store,
            cli,
            roster_path,
            witness,
            home,
            crate::ai::store::Outcome::Ended(crate::ai::store::Ended::StatusSignedOut),
            now,
        );
    }
    transact_login_observation(
        store,
        roster_path,
        witness,
        now,
        |row, raw, state, logins| {
            if logins.sign_in_receipts.get(&row.label).copied() != witness.sign_in_receipt {
                return Err("a newer sign-in superseded this status; nothing recorded".into());
            }
            if !had_a_login(cli, row, state, logins, home) {
                return Ok(Vec::new());
            }
            let context = store
                .capture_auth_context(row)
                .ok_or("sign-in credentials cannot be read; nothing recorded")?;
            if witness.auth_context.as_ref() != Some(&context) {
                return Err("credentials changed during sign-in status; nothing recorded".into());
            }
            accounts::stage_login_end(state, raw, logins, context, now);
            Ok(Vec::new())
        },
    )
}

/// Attribute every review outcome to the policy and login captured before it ran.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountReviewObservation",
        action = "Provider",
        reason = "Select the provider fixture before recording the review outcome."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountReviewObservation",
        action = "Location",
        reason = "Select whether the reviewed login has a directory of its own before the review runs."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountReviewObservation",
        action = "Outcome",
        reason = "Select the review outcome fixture before it is recorded."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountReviewObservation",
        action = "Case",
        reason = "Select the account and credential-generation fixture the review observed."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountReviewObservation",
        action = "Record",
        project = "aterm_agent::harness::accounts_sync::review_observation_tests::review_observation_transition"
    )
)]
pub(crate) fn record_review_login(
    store: &accounts::Store,
    cli: Cli,
    roster_path: &Path,
    witness: &LoginWitness,
    home: &Path,
    outcome: crate::ai::store::Outcome,
    now: i64,
) -> Result<Vec<accounts::Event>, String> {
    use crate::ai::store::Outcome;
    transact_login_observation(
        store,
        roster_path,
        witness,
        now,
        |row, raw_row, state, logins| {
            let label = &row.label;
            if logins.sign_in_receipts.get(label).copied() != witness.sign_in_receipt {
                return Err("a newer sign-in superseded this review; nothing recorded".into());
            }
            if matches!(outcome, Outcome::Ended(_)) && !had_a_login(cli, row, state, logins, home) {
                return Ok(Vec::new());
            }
            if cli == Cli::Codex {
                let context = witness
                    .auth_context
                    .as_ref()
                    .ok_or("review credentials were unavailable; nothing recorded")?;
                if store.capture_auth_context(row).as_ref() != Some(context) {
                    return Err("credentials changed or became unavailable during the review; nothing recorded".into());
                }
                match outcome {
                    Outcome::Worked => {
                        let (checked, identity_matches) = checked_codex_auth_context(store, row)
                            .ok_or("review credentials cannot be read; nothing recorded")?;
                        if checked != *context || !identity_matches {
                            return Err("default login identity changed or cannot be verified; nothing recorded".into());
                        }
                        accounts::stage_signed_back(state, raw_row, logins, context.clone(), now);
                        return Ok(Vec::new());
                    }
                    Outcome::Ended(_) => {
                        accounts::stage_login_end(state, raw_row, logins, context.clone(), now);
                        return Ok(Vec::new());
                    }
                    Outcome::Limited(_) => {
                        accounts::stage_auth_check(state, raw_row, logins, context.clone(), now);
                    }
                }
            }
            match outcome {
                Outcome::Worked if row.dir.is_none() => {
                    observe_default_success(cli, row, state, logins, home, true, now)
                }
                Outcome::Worked => {
                    let events = if accounts::sign_back_due(state, label) {
                        accounts::note_checked_sign_in(state, logins, label, now)
                    } else {
                        Vec::new()
                    };
                    accounts::note_served_login(logins, label, now);
                    Ok(events)
                }
                // No login here and none ever bound: nothing ended, and the row stays never signed in.
                Outcome::Ended(_) if !had_a_login(cli, row, state, logins, home) => Ok(Vec::new()),
                Outcome::Ended(_) => Ok(accounts::note_login_end(state, logins, label, now)),
                Outcome::Limited(until) => {
                    if state.accounts.get(label).is_some_and(|account| {
                        account
                            // Tied seconds cannot establish that this result
                            // followed the clear. Preserve the explicit clear.
                            .cleared_since("five_hour", witness.observed_at.saturating_sub(1), now)
                            .is_some()
                    }) {
                        return Err(
                            "quota observation predates or ties an account clear; nothing recorded"
                                .into(),
                        );
                    }
                    Ok(accounts::note_wall(
                        state,
                        label,
                        "five_hour",
                        until,
                        witness.observed_at,
                        now,
                    ))
                }
            }
        },
    )
}

/// Whether `row` has held a login: one is stored in its home (`home` for the row on the default
/// login), Claude Code's profile there names an account (a Mac keeps that login in its keychain,
/// with no file, as the accounts report reads it), or its records say one was renewed, served or
/// ended, or it was seen signed out.
fn had_a_login(
    cli: Cli,
    row: &Row,
    state: &accounts::State,
    logins: &accounts::Logins,
    home: &Path,
) -> bool {
    let dir = row.dir.as_deref().map_or(home, Path::new);
    let profile_names_one = || match &row.dir {
        Some(_) => super::profile::key_in_home(dir).is_some(),
        None => home
            .parent()
            .and_then(super::profile::default_home_key)
            .is_some(),
    };
    crate::ai::connector::stored_login_file(cli.connector(), dir).is_some()
        || (cli == Cli::Claude && profile_names_one())
        || logins.get(&row.label).is_some_and(|binding| {
            binding.renewed_at.is_some()
                || binding.served_at.is_some()
                || binding.ended_at.is_some()
        })
        || state
            .accounts
            .get(&row.label)
            .is_some_and(|account| account.auth == accounts::Auth::SignedOut)
}

fn transact_login_observation(
    store: &accounts::Store,
    roster_path: &Path,
    witness: &LoginWitness,
    now: i64,
    record: impl FnOnce(
        &Row,
        &Row,
        &mut accounts::State,
        &mut accounts::Logins,
    ) -> Result<Vec<accounts::Event>, String>,
) -> Result<Vec<accounts::Event>, String> {
    if store.dir != witness.store || roster_path != witness.roster {
        return Err("login observation belongs to another store or roster".into());
    }
    store.transact_pool_logins_within(
        &accounts::LOCK_LADDER,
        roster_path,
        now,
        &mut None,
        |raw, state, logins| {
            let label = &witness.row.label;
            let current = accounts::bound(raw, logins);
            if current.row(label) != Some(&witness.row) || !witness.parent.matches(logins, label) {
                return Err("login policy changed during the run; nothing recorded".into());
            }
            record(
                &witness.row,
                raw.row(label).ok_or("observed account disappeared")?,
                state,
                logins,
            )
        },
    )
}

/// The address Claude Code's profile at `path` is signed in as.
fn profile_email(path: &Path, read: &LocalSourceRead<'_>) -> Option<String> {
    email_of_profile(&read(path).ok()?)
}

/// The address the Claude Code profile `text` is signed in as.
fn email_of_profile(text: &str) -> Option<String> {
    let val: Value = aterm_json::from_str(text).ok()?;
    val.get("oauthAccount")?
        .get("emailAddress")?
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// [`profile_email`] of the profile at `path` as this Mac reads it ([`read_local_source_text`]).
/// With a trace scope armed it records a standing of that address alone, or that there was none
/// (`aterm_uds::trace::standing`), and no stamp of the file: Claude Code rewrites its profile with
/// its usage, its tips and its projects while the account stays, and nothing that reads this
/// decides by more than the address. A read that failed for a reason no stamp decides records a
/// retry beside.
fn signed_in_email(path: &Path) -> Option<String> {
    if !trace::armed() {
        return profile_email(path, &read_local_source_text);
    }
    let text = trace::unrecorded(|| read_local_source_text(path));
    if let Err(error) = &text {
        trace::fs::failed(path, error);
    }
    let email = text.ok().as_deref().and_then(email_of_profile);
    trace::standing(
        path,
        Some(signed_in_standing(email.as_deref())),
        signed_in_now,
    );
    email
}

/// The digest a [`signed_in_email`] records of the address it read.
fn signed_in_standing(email: Option<&str>) -> [u8; 32] {
    let mut digest = aterm_digest::Sha256::new();
    digest.update(b"aterm-agent/accounts/profile-email/v1\0");
    match email {
        Some(email) => {
            digest.update([1]);
            digest.update(email.as_bytes());
        }
        None => digest.update([0]),
    }
    digest.finalize()
}

/// What a [`signed_in_email`] of `path` would record now: its standing's replay.
fn signed_in_now(path: &Path) -> Option<[u8; 32]> {
    let email = read_local_source_text(path)
        .ok()
        .as_deref()
        .and_then(email_of_profile);
    Some(signed_in_standing(email.as_deref()))
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

/// The store's [`accounts::State::login_dir_spans`] for the transcript scan
/// ([`find_transcript_walls_by_email`]), each open stretch whose noted process `alive` finds
/// still running seen at `now` ([`accounts::LoginDirSpans::seen_running`]) and saved under the
/// store's lock, so the sighting outlives that process: a stretch a scan found running lapses an
/// hour after that look, not after its host's last note. `None` where the record cannot be read
/// or saved within `ladder`, and the scan then books nothing.
fn login_dir_spans_seen(
    store: &accounts::Store,
    ladder: &[std::time::Duration],
    now: i64,
    alive: impl Fn(&accounts::NotedProcess) -> bool,
) -> Option<BTreeMap<String, accounts::LoginDirSpans>> {
    let mut seen = None;
    store
        .transact_within(ladder, now, &mut None, |state| {
            for noted in state.login_dir_spans.values_mut() {
                noted.seen_running(now, &alive);
            }
            seen = Some(state.login_dir_spans.clone());
            Ok(Vec::new())
        })
        .ok()?;
    seen
}

/// [`login_dir_spans_seen`] as an accounts sync looks: a noted process runs on where the process
/// of its pid still runs and started at its start ([`super::restart_stamp::alive_as`]), so a pid
/// reused since is no sighting.
fn login_dir_spans_live(
    store: &accounts::Store,
    ladder: &[std::time::Duration],
    now: i64,
) -> Option<BTreeMap<String, accounts::LoginDirSpans>> {
    login_dir_spans_seen(store, ladder, now, |process| {
        super::restart_stamp::alive_as(process.pid, &process.start)
    })
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
        .and_then(super::unix_time::parse_utc)
}

/// Fold one transcript line into `rows` and `last_at`.
fn fold_transcript_row(
    line: &str,
    roster: &Roster,
    now: i64,
    last_at: &mut u64,
    rows: &mut Vec<TranscriptRow>,
) {
    if line.contains("\"session_context\"") && line.contains("\"userEmail\"") {
        if let Some(matched) = match_session_context_label(line, roster) {
            let at = transcript_row_at(line).unwrap_or(*last_at);
            *last_at = (*last_at).max(at);
            rows.push(TranscriptRow::Context(matched, at));
        }
    } else if line.contains("<local-command-stdout>Login successful</local-command-stdout>") {
        let at = transcript_row_at(line);
        if let Some(at) = at {
            *last_at = (*last_at).max(at);
        }
        rows.push(TranscriptRow::Login(at));
    } else if line.contains("\"assistant\"") {
        if line.contains("\"quotaLimits\"") && line.contains("\"rejected\"") {
            if let Some(wall) = upgrade::transcript_standing_wall(line) {
                *last_at = (*last_at).max(wall.at);
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
            && let Some(ans_at) = upgrade::transcript_last_answer(line, u64::MAX)
        {
            *last_at = (*last_at).max(ans_at);
            rows.push(TranscriptRow::Answer(ans_at));
        }
    }
}

/// The byte cap on what one [`find_transcript_walls_by_email`] scan reads over all its
/// transcripts, newest first. The window's per-read `footer::FOLD_BUDGET` is no cap on one
/// transcript (booking its newest rows needs its `session_context` and `/login` rows from its
/// start, however long it is), but each transcript still to be read keeps that much of this, so
/// one long transcript never leaves the older ones less than the fold budget they had.
const TRANSCRIPT_WALLS_SCAN_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// What the next transcript of a scan may read: the scan's `left`, less the fold budget each of
/// the `after` transcripts still to be read keeps.
fn transcript_allowance(left: u64, after: usize) -> u64 {
    let kept = super::footer::FOLD_BUDGET.saturating_mul(u64::try_from(after).unwrap_or(u64::MAX));
    left.saturating_sub(kept)
}

/// The rows of one transcript [`find_transcript_walls_by_email`] books, read from its start, line
/// by line ([`super::usage::MAX_LINE_BYTES`]), spending at most `budget` bytes (what it reads is
/// taken off). A last line with no newline (one still being written, or one the budget cut) books
/// nothing.
fn transcript_rows(path: &Path, roster: &Roster, now: i64, budget: &mut u64) -> Vec<TranscriptRow> {
    let Some(f) = super::footer::open_regular(path) else {
        return Vec::new();
    };
    let mut reader = io::BufReader::new(f.take(*budget));
    let mut rows = Vec::new();
    let mut last_at = 0;
    // The rows before the last line handed over, and that line's length.
    let mut last = (0, 0);
    let through = each_bounded_line(&mut reader, super::usage::MAX_LINE_BYTES, |raw| {
        last = (rows.len(), raw.len());
        if let Ok(line) = std::str::from_utf8(raw) {
            fold_transcript_row(line, roster, now, &mut last_at, &mut rows);
        }
    });
    let left = reader.into_inner().limit();
    let read = budget.saturating_sub(left);
    *budget = left;
    // `each_bounded_line` hands over an unterminated last line no longer than the line bound;
    // one longer than any line it hands over cannot be the last one handed.
    if let Ok(through) = through {
        let tail = read.saturating_sub(through);
        if tail > 0 && u64::try_from(last.1).is_ok_and(|len| len == tail) {
            rows.truncate(last.0);
        }
    }
    rows
}

/// The transcripts a scan reads: the [`TRANSCRIPT_SCAN_FILES`] most recently written under
/// `claude_dir`'s `projects`, newest first, each with when it was last written; `None` where
/// that directory cannot be listed.
fn newest_transcripts(claude_dir: &Path) -> Option<Vec<(std::time::SystemTime, PathBuf)>> {
    let projects = claude_dir.join("projects");
    let entries = trace::fs::read_dir(&projects).ok()?;
    let mut files_with_mtime: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    for proj in entries {
        if !proj.file_type().is_ok_and(|ft| ft.is_dir()) {
            continue;
        }
        let Ok(files) = trace::fs::read_dir(proj.path()) else {
            continue;
        };
        for file in files {
            let path = file.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let mtime = trace::fs::symlink_metadata(&path)
                .and_then(|m| m.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            files_with_mtime.push((mtime, path));
        }
    }
    files_with_mtime.sort_by_key(|a| std::cmp::Reverse(a.0));
    files_with_mtime.truncate(TRANSCRIPT_SCAN_FILES);
    Some(files_with_mtime)
}

/// How many transcripts one scan reads ([`newest_transcripts`]).
const TRANSCRIPT_SCAN_FILES: usize = 16;

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
///
/// No row a conversation wrote in its stretches under a login of a row's own directory
/// (`spans`, the store's [`accounts::State::login_dir_spans`]) is booked: such a process keeps
/// the default profile, whose address its `session_context` names whatever login served its
/// turns. On 2026-11-03 the lab's `claude-rotation-proof` moved a conversation onto `lab-c`'s
/// login, and `lab-c`'s wall in it was booked to `lab-a`, the row its `session_context` named, so
/// the next move waited out a wall `lab-a` never met. `spans` is read after the transcripts.
/// aterm notes a stretch before each launch it makes onto such a login (a move, a cold
/// relaunch), so a row read here of a process it launched was written after its note. A
/// conversation that began in a process on such a login by itself (one `/clear` opened, a
/// worker that inherited its login) is noted from its process's start at its host's first
/// account point, its wall's own sync after the note; a scan between its first rows and that
/// point books them by its `session_context`. An open stretch runs on an hour past its last
/// sighting ([`accounts::LOGIN_DIR_NOTED_S`]): its host's note, or the sync's, which finds the
/// process its host last saw run it still running and saves that before the scan reads the
/// record ([`login_dir_spans_seen`]), so a turn hours long with no account point stays covered
/// once that process is gone. The rows of a conversation whose process is gone (aterm quit, a
/// crash) and which is resumed where no host sees it are booked by its `session_context` again
/// once it lapses. Where `spans` cannot be read (`None`) the scan books nothing.
#[must_use]
pub fn find_transcript_walls_by_email(
    claude_dir: &Path,
    roster: &Roster,
    spans: impl FnOnce() -> Option<BTreeMap<String, accounts::LoginDirSpans>>,
    now: i64,
) -> BTreeMap<String, upgrade::StandingWall> {
    let Some(files_with_mtime) = newest_transcripts(claude_dir) else {
        return BTreeMap::new();
    };

    let mut budget = TRANSCRIPT_WALLS_SCAN_BYTES;
    let read: Vec<(Option<String>, Vec<TranscriptRow>)> = files_with_mtime
        .iter()
        .enumerate()
        .map(|(at, (_, path))| {
            let conversation = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .map(str::to_owned);
            let after = files_with_mtime.len() - at - 1;
            let mut allowance = transcript_allowance(budget, after);
            let rows = transcript_rows(path, roster, now, &mut allowance);
            budget -= transcript_allowance(budget, after) - allowance;
            (conversation, rows)
        })
        .collect();
    let Some(spans) = spans() else {
        return BTreeMap::new();
    };
    let transcripts: Vec<Vec<TranscriptRow>> = read
        .into_iter()
        .map(|(conversation, rows)| {
            let Some(noted) = conversation.and_then(|c| spans.get(&c)) else {
                return rows;
            };
            let under = |at: u64| noted.covers(i64::try_from(at).unwrap_or(i64::MAX));
            // A `/login` is kept: it ends every naming before it, whoever's login served it.
            rows.into_iter()
                .filter(|row| match row {
                    TranscriptRow::Login(_) => true,
                    TranscriptRow::Context(_, at) | TranscriptRow::Answer(at) => !under(*at),
                    TranscriptRow::Wall(wall) => !under(wall.at),
                })
                .collect()
        })
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
            class: Class::Preferred,
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

/// Whether the rows `was` and `now` are one account's: each expects a provider identity
/// ([`Row::expected_identity_email`]: its `identity_email`, else its login address) and it is the
/// same one, the identity [`super::accounts_pool::check_login_identity`] holds a launch to. A
/// login address can change while the identity stays (a source that lists an alias's account by
/// the address the provider names it by).
pub(super) fn same_account(was: &Row, now: &Row) -> bool {
    was.expected_identity_email()
        .zip(now.expected_identity_email())
        .is_some_and(|(was, now)| accounts::email_matches(was, now))
}

/// The login directory `was`, a label's row in the previous roster, passes to `row`, the row a
/// source now gives that label: its own where it was the same account ([`same_account`]), or
/// where it was a legacy row, which names no login address (a sync assigns it one only where
/// nothing is on record under its label, `Store::check_new_labels`); nothing where it was
/// another account's, whose login that directory can hold ([`rows_to_choose`]).
fn label_dir_kept<'a>(was: &'a Row, row: &Row) -> Option<&'a str> {
    was.dir
        .as_deref()
        .filter(|_| was.email.is_none() || same_account(was, row))
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

/// The canonical provider identity of a uniquely identified native fleet account.
fn native_fleet_identity<'a>(fleet: &'a FleetRoster, account: &FleetAccount) -> Option<&'a str> {
    let lab = fleet.lab_snapshot.as_deref()?;
    let mut matches = lab
        .of(Cli::Claude)
        .filter(|candidate| accounts::email_matches(&candidate.email, &account.email));
    let found = matches.next()?;
    matches
        .next()
        .is_none()
        .then_some(found.identity.as_deref())
        .flatten()
}

/// Build the machine's [`Roster`] from `fleet` for `machine`. The row whose email is
/// `default_email` is the default login's (no `dir`); where none of `machine`'s rows has that
/// email, the default login holds an account they do not list, and the roster lists it too, as
/// the default login's row: the fleet's own row for it where another machine's lists it, else
/// the label `existing` already gives that account where the fleet gives it to no account, else
/// a new one ([`local_account`], unique among the fleet's labels and `existing`'s), so no label
/// is kept on an account it is not and an account keeps its label while it can.
/// Every other row is given the login directory its label's row in `existing` names, else its
/// label's own (`logins/<label>`); the sync places again each row whose label's row named none or
/// held another login address, by whose login each directory holds (`chosen_dirs`).
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
    // Under the lab a Mac that prefers nothing here still lists its default login's account.
    if fleet_rows.is_empty() && fleet.alab.is_none() {
        return existing.cloned().unwrap_or_default();
    }
    let default_email = default_identity_hint(existing, default_email);
    let matches_identity = |acct: &FleetAccount, email: &str| {
        let expected = retained_login_row(existing, &acct.email)
            .and_then(Row::expected_identity_email)
            .or_else(|| native_fleet_identity(fleet, acct))
            .unwrap_or(&acct.email);
        accounts::email_matches(expected, email)
    };
    let keeps_rows = fleet.keeps_rows && existing.is_some_and(|ex| !ex.rows.is_empty());
    let unlisted = default_email
        .filter(|de| !fleet_rows.iter().any(|r| matches_identity(r, de)))
        .and_then(|de| {
            let kept = existing.and_then(|ex| {
                ex.rows.iter().find(|r| {
                    r.expected_identity_email()
                        .is_some_and(|re| accounts::email_matches(re, de))
                })
            });
            // A dirless row that names no address (the first `add`'s `main`) already lists
            // the default login: retain keeps it as it is, and the account is not listed
            // beside it as a second row with no login directory (a roster nobody can parse).
            // No address is written into it here, since what is on record under it may be another
            // account's (`check_new_labels` refuses that assignment): a sync of a CLI whose login
            // names its address may write one where that is known ([`merge_default_twin`]).
            let listed = kept.is_none()
                && existing
                    .and_then(|ex| ex.row_for_login(None))
                    .is_some_and(|row| row.expected_identity_email().is_none());
            if listed && (fleet.alab.is_some() || keeps_rows) {
                return None;
            }
            if fleet.alab.is_some() {
                return alab_default_account(fleet, existing, kept, de, &matches_identity);
            }
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
    if fleet_rows.is_empty() {
        return existing.cloned().unwrap_or_default();
    }

    let default_label: Option<String> = default_email
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
        // Under the lab, and over the rows a discovered fleet keeps, no row is made the default
        // login unasked: a row that has a login directory keeps it (taking it away would leave
        // that login behind, signed out), and a Mac whose default login the lab does not name has
        // no dirless row. The sheet path makes its first row the default login, as it always did.
        .or_else(|| (fleet.alab.is_none() && !keeps_rows).then(|| fleet_rows[0].label.clone()));

    let mut rows = Vec::with_capacity(fleet_rows.len());
    for acct in &fleet_rows {
        let existing_row = existing.and_then(|ex| ex.row(&acct.label));
        let dir = if default_label.as_deref() == Some(acct.label.as_str()) {
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
                .and_then(|row| row.identity_email.clone())
                .or_else(|| native_fleet_identity(fleet, acct).map(str::to_owned)),
            browser,
            held_by: None,
            was: Vec::new(),
            vendor: Default::default(),
        });
    }

    if fleet.alab.is_some() {
        rows.sort_by_key(|row| (row.dir.is_some(), row.class));
    } else {
        rows.sort_by_key(|row| (row.dir.is_some(), row.class, row.label.clone()));
    }

    Roster { rows }
}

/// The row the default login (address `de`) takes under the lab's `fleet` where none of this
/// Mac's preferred accounts is it: the label `kept`, the existing row of that account, gives it,
/// else the lab's label for the account ([`FleetRoster::known`]); none where the lab does not
/// know the account and no row names it, or another account's row or fleet row holds that label,
/// since the lab is what names an account here and a label is never taken from the row that
/// holds it. Never a label with a machine in it, and never a slot read from an address
/// ([`local_account`] is the sheet path's alone).
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "FleetMachineAssignment",
        action = "SyncLabels",
        project = "aterm_agent::harness::alab_roster::assignment_tests::labels_transition"
    )
)]
fn alab_default_account(
    fleet: &FleetRoster,
    existing: Option<&Roster>,
    kept: Option<&Row>,
    de: &str,
    matches_identity: &dyn Fn(&FleetAccount, &str) -> bool,
) -> Option<FleetAccount> {
    let machine = fleet.alab.as_ref()?.key.as_str().to_owned();
    let known = fleet.known.iter().find(|a| matches_identity(a, de));
    let label_free = |label: &str| {
        !fleet.accounts.iter().any(|a| a.label == label)
            && !existing.is_some_and(|roster| {
                roster.rows.iter().any(|row| {
                    row.label == label
                        && !row
                            .expected_identity_email()
                            .is_some_and(|held| accounts::email_matches(held, de))
                })
            })
    };
    match (kept.filter(|row| label_free(&row.label)), known) {
        (Some(row), known) => Some(FleetAccount {
            label: row.label.clone(),
            machines: vec![machine],
            class: known.map_or(row.class, |known| known.class),
            email: row.email.clone().unwrap_or_else(|| de.to_owned()),
            browser: row
                .browser
                .clone()
                .or_else(|| known.and_then(|k| k.browser.clone())),
            vendor: accounts::Vendor::default(),
        }),
        (None, Some(known)) if label_free(&known.label) => Some(known.clone()),
        _ => None,
    }
}

/// `fleet` with each row of `existing` whose address the sheet's rows give conflicting
/// assignments ([`FleetRoster::conflicting`]) whose member rows name `machine` or name none
/// ([`conflict_names`]) back in it as `machine`'s, under the label, class, browser and vendor
/// keys that row has, where no account of the fleet's has that label and `machine` has room: the
/// sheet cannot say whether that account stays, so a roster that lists it keeps it
/// ([`build_machine_roster`] then keeps its login directory, as for any row it lists) until the
/// rows agree. Where every member row names another machine the sheet moved the account off this
/// one, so its row drops, as it does for one row naming another machine, and the sync names no
/// conflict here. Claude Code's sync alone needs it: it rebuilds the roster from the fleet, where
/// another CLI's appends the rows it lacks and never drops one. The fleet a sync writes beside
/// the roster is the sheet's, without these rows.
fn carry_conflicting(fleet: &FleetRoster, machine: &str, existing: &Roster) -> FleetRoster {
    let mut carried = fleet.clone();
    if fleet.conflicting.is_empty() {
        return carried;
    }
    let Some(norm) = normalize_machine_id(machine) else {
        return carried;
    };
    for row in &existing.rows {
        let conflicted = row.email.as_deref().is_some_and(|email| {
            fleet.conflicting.iter().any(|(address, conflict)| {
                accounts::email_matches(address, email) && conflict_names(conflict, Some(&norm))
            })
        });
        if !conflicted || carried.accounts.iter().any(|acct| acct.label == row.label) {
            continue;
        }
        let Some(email) = row.email.clone() else {
            continue;
        };
        carried.accounts.push(FleetAccount {
            label: row.label.clone(),
            machines: vec![norm.clone()],
            class: row.class,
            email,
            browser: row.browser.clone(),
            vendor: row.vendor,
        });
    }
    carried
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
    /// The lab registry's preferences ([`super::alab_roster`]): every row this machine has stays,
    /// and the accounts it prefers join.
    Alab,
    /// The expected registry source could not be read. No fallback was selected.
    AlabPending(String),
    /// Local source policy is unknown; automatic source fallback is refused.
    SourcePending(String),
    /// No fleet file and no source: `accounts.toml` is the list, as it was written.
    Roster,
    /// The local fleet projection beside the roster, which explicit add/adopt may edit.
    /// `opt_in` is the word the selected list records: [`OptIn::Discovered`]
    /// where the list was found on this Mac, from its own logins, profiles and transcripts;
    /// [`OptIn::Named`] where a sync was handed a source (and where discovery wrote it before it
    /// recorded `discovered`); none for a file that opts nothing in, which a sync from init
    /// still reads.
    Local {
        /// Local projection files; never a vault input.
        files: Vec<PathBuf>,
        /// The word they record.
        opt_in: Option<OptIn>,
    },
    /// `[fleet] accounts_source`, which this machine reads and does not write.
    Key(String),
    /// The vault's pool, fleet roster or sheet, by its entry's name, read in place from this
    /// Mac's store. This machine reads the input without modifying it; row contents alone never
    /// make it a local edit destination.
    Vault(String),
}

impl FleetOrigin {
    /// One word for it: `roster`, `discovered`, `named`, `fleet-file`, `key` or `vault`.
    #[must_use]
    pub const fn word(&self) -> &'static str {
        match self {
            Self::Alab | Self::AlabPending(_) => "alab",
            Self::SourcePending(_) => "source",
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
            Self::Alab | Self::AlabPending(_) => Some(super::alab_roster::SOURCE.to_owned()),
            Self::Roster | Self::SourcePending(_) => None,
            Self::Local { files, .. } => files.first().map(|f| f.display().to_string()),
            Self::Key(source) => Some(source.clone()),
            Self::Vault(name) => Some(crate::vault_entry::source_name(name)),
        }
    }

    /// What `aterm harness accounts` says of the list.
    #[must_use]
    pub fn words(&self) -> String {
        let source = self.source().unwrap_or_default();
        match self {
            Self::Alab => {
                "the lab registry's preferences for this Mac; every row it has stays".to_owned()
            }
            Self::AlabPending(why) | Self::SourcePending(why) => why.clone(),
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
            Self::Vault(name) => {
                format!(
                    "the vault's `{name}` ({source}), which this machine reads and does not write"
                )
            }
        }
    }
}

/// [`FleetOrigin`] of the roster at `roster`, for `home` and the config file `config`.
#[must_use]
pub fn fleet_origin(
    store: &accounts::Store,
    home: Option<&Path>,
    config: Option<&Path>,
    roster: &Path,
) -> FleetOrigin {
    match super::alab_roster::chosen(Cli::Claude, Some(roster), None, config) {
        Ok(Chosen::Lab(_)) => return FleetOrigin::Alab,
        Ok(Chosen::Pending(why)) | Err(why) => return FleetOrigin::AlabPending(why),
        Ok(Chosen::Unnamed) => {}
    }
    source_selection::select_for(store, home, config, Some(roster), SourceScope::Init)
        .map_or_else(FleetOrigin::SourcePending, |selected| selected.origin())
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

/// A vault entry an account sync reads, as this Mac's store answered for it when it was read
/// ([`crate::vault_entry`]): a precedence decides on this answer, and the sync that follows reads
/// this same text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct VaultSource {
    pub(super) name: String,
    pub(super) read: crate::vault_entry::Read,
}

impl VaultSource {
    /// The entry `name` under `home`, as the store answers now.
    pub(super) fn read(home: &Path, name: &str) -> Self {
        Self {
            name: name.to_owned(),
            read: crate::vault_entry::read_text(home, name, SOURCE_TEXT_MAX_BYTES),
        }
    }

    /// Whether the entry holds its place in a precedence: present, usable or not. Source
    /// selection skips an absent entry, never one this Mac cannot read now.
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "AccountSourcePresence",
            action = "Primary",
            project = "aterm_agent::harness::accounts_sync::cli_pool_tests::source_presence::source_transition"
        )
    )]
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "AccountSourcePresence",
            action = "Fallback",
            project = "aterm_agent::harness::accounts_sync::cli_pool_tests::source_presence::source_transition"
        )
    )]
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "AccountSourcePresence",
            action = "PrimaryUnavailable",
            project = "aterm_agent::harness::accounts_sync::cli_pool_tests::source_presence::source_transition"
        )
    )]
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "AccountSourcePresence",
            action = "FallbackUnavailable",
            project = "aterm_agent::harness::accounts_sync::cli_pool_tests::source_presence::source_transition"
        )
    )]
    pub(super) fn occupies(&self) -> bool {
        self.read.occupies()
    }

    /// How it prints where a source is named (`vault:<entry>`).
    pub(super) fn source(&self) -> String {
        crate::vault_entry::source_name(&self.name)
    }

    /// Its text.
    /// # Errors
    /// The vault holds no such entry for this Mac, or it cannot be read now.
    pub(super) fn text(&self) -> Result<String, String> {
        match &self.read {
            crate::vault_entry::Read::Present(text) => Ok(text.clone()),
            crate::vault_entry::Read::Absent => {
                Err(format!("the vault holds no `{}` for this Mac", self.name))
            }
            crate::vault_entry::Read::Unavailable(why) => Err(why.clone()),
        }
    }
}

/// Whether the vault's `sheet` gives this machine rows, the machine a sync from init takes them
/// for ([`machine_for`]: an explicit local configuration, else this host's id). An unreadable
/// or refused sheet counts as giving them, so the sync that reads it says why. Where the lab
/// names this Mac the sheet is not read ([`sheet_read_here`]) and gives it nothing.
fn sheet_names_this_machine(config: Option<&Path>, sheet: &VaultSource) -> bool {
    if !sheet_read_here() {
        return false;
    }
    let Ok(fleet) = sheet
        .text()
        .and_then(|text| parse_accounting_csv(&text, ACCOUNTS_CSV_FILE))
    else {
        return true;
    };
    machine_for(config, None)
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

/// Whether the harness may read the vault's sheet (`ai-accounts.csv`) on this Mac: only where
/// the lab does not name it (§0: the sheet is the lab importer's input,
/// `crates/aterm/src/alab_sheet.rs`; where the lab names this Mac its rows, labels, mail and
/// upkeep come from the lab). A Mac without the registry keeps reading it, so it keeps its rows;
/// one whose registry could not be read yet ([`super::alab_roster::pending`]) reads it only once
/// a read shows the lab does not name it.
pub(super) fn sheet_read_here() -> bool {
    super::alab_roster::snapshot().permits_sheet()
}

/// The refusal of a sheet named as a source of `cli`'s pool on a Mac the lab names
/// ([`sheet_read_here`]).
fn sheet_refused(source: &str, cli: Cli) -> String {
    let flag = if cli == Cli::Claude {
        String::new()
    } else {
        format!(" --cli {}", cli.id())
    };
    format!(
        "{source} is an accounts sheet, which the lab registry's importer reads (`aterm db alab \
         import`): the lab names this Mac, so its accounts come from the lab (`aterm harness \
         accounts sync alab{flag}`); no account was read"
    )
}

/// What a sync of `cli` with no source named reads ([`super::alab_roster::chosen`]): the lab
/// where it names this Mac and, for Claude Code, no `[fleet] accounts_source` in `config` names
/// another source; nothing but this Mac's own default login where the lab is not read yet.
fn lab_chosen(cli: Cli, roster: &Path, config: &Path) -> Chosen {
    // With no source named the choice refuses nothing; a refusal could only say the lab is unread.
    super::alab_roster::chosen(cli, Some(roster), None, Some(config))
        .unwrap_or_else(Chosen::Pending)
}

pub(super) fn source_key(config: &Path) -> Option<String> {
    let text = super::ai_pool::read_bounded_text(config, 2 * 1024 * 1024)?;
    source_key_text(&text)
}

pub(super) fn source_key_text(text: &str) -> Option<String> {
    let table: aterm_toml::Table = aterm_toml::from_str(text).ok()?;
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
type LocalSourceRead<'r> = dyn Fn(&Path) -> io::Result<String> + 'r;

#[derive(Debug, Clone)]
pub(super) struct PoolSyncCtx<P = super::ai_pool::Pool, I = super::ai_pool::LocalPoolIdentity> {
    pool: P,
    identity: I,
}

impl<
    P: std::borrow::Borrow<super::ai_pool::Pool>,
    I: std::borrow::Borrow<super::ai_pool::LocalPoolIdentity>,
> PoolSyncCtx<P, I>
{
    fn holder_for_row(&self, cli: Cli, row: &Row) -> Option<String> {
        let email = row.expected_identity_email().or(row.email.as_deref())?;
        let Some(acct) = self.pool.borrow().accounts.iter().find(|a| {
            a.provider == cli
                && accounts::same_provider_realm(cli, row.vendor.region, a.vendor.region)
                && accounts::email_matches(&a.email, email)
        }) else {
            return Some(super::ai_pool::POOL_HOLDER_UNASSIGNED.to_owned());
        };
        (!acct.is_warm_for(self.identity.borrow())).then(|| acct.holder_word().to_owned())
    }

    fn is_home_for_row(&self, cli: Cli, row: &Row) -> bool {
        let Some(email) = row.expected_identity_email().or(row.email.as_deref()) else {
            return false;
        };
        self.pool.borrow().accounts.iter().any(|a| {
            a.provider == cli
                && accounts::same_provider_realm(cli, row.vendor.region, a.vendor.region)
                && accounts::email_matches(&a.email, email)
                && a.home_mac()
                    .is_some_and(|h| self.identity.borrow().matches(h))
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
        ..FleetRoster::default()
    }
}

/// Load a [`FleetRoster`] from `source`, or from [`configured_source`] when `source` is `None`.
/// # Errors
/// No source; a config path that names no file or does not resolve to an absolute path; a
/// configured key whose config path has no directory; a source that could not be read or parsed;
/// or, for an AI pool source, a machine id (`m<N>`) that cannot be determined.
pub fn load_fleet_source(env: &Env, source: Option<&str>) -> Result<FleetRoster, String> {
    load_fleet_source_for_machine(env, source, None, &fetch_source_text).map(|(fleet, _, _)| fleet)
}

/// Whether automatic setup owes a Claude roster to this machine. A shared vault source can
/// describe other machines without assigning an account here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InitAccountsAssignment {
    /// A source or existing login must be reconciled, with its ordinary errors preserved.
    Required,
    /// The selected vault source is valid but gives this fresh machine no Claude account.
    Unassigned {
        /// The machine whose assignments were read.
        machine: String,
        /// Accounts in the shared source, none assigned here.
        fleet_accounts: usize,
    },
}

/// Read this machine's automatic Claude assignment without fetching a remote source or
/// changing any roster. Existing rosters, default logins and explicitly configured sources
/// keep their ordinary reconciliation. Conflicting assignments are never read as unassigned.
///
/// # Errors
/// The selected local vault source cannot be read or parsed, or the machine id is unknown.
pub fn init_accounts_assignment(env: &Env) -> Result<InitAccountsAssignment, String> {
    let Some(roster) = env.accounts_roster() else {
        return Ok(InitAccountsAssignment::Required);
    };
    // The lab's sync, or one that keeps the roster while the lab is unread, is the sync owed.
    if !matches!(
        super::alab_roster::chosen(Cli::Claude, Some(&roster), None, env.config.as_deref())?,
        Chosen::Unnamed
    ) {
        return Ok(InitAccountsAssignment::Required);
    }
    let selected = source_selection::select_for(
        &env.accounts_store(),
        env.home.as_deref(),
        env.config.as_deref(),
        Some(&roster),
        SourceScope::Init,
    )?;
    init_accounts_assignment_from(env, &selected.origin())
}

fn init_accounts_assignment_from(
    env: &Env,
    origin: &FleetOrigin,
) -> Result<InitAccountsAssignment, String> {
    let (Some(home), Some(roster)) = (env.home.as_deref(), env.accounts_roster()) else {
        return Ok(InitAccountsAssignment::Required);
    };
    match trace::fs::symlink_metadata(&roster) {
        Ok(_) => return Ok(InitAccountsAssignment::Required),
        Err(why) if why.kind() == io::ErrorKind::NotFound => {}
        Err(why) => return Err(format!("cannot inspect Claude account roster: {why}")),
    }
    if read_default_profile_email(env).is_some() {
        return Ok(InitAccountsAssignment::Required);
    }
    let FleetOrigin::Vault(name) = origin else {
        return Ok(InitAccountsAssignment::Required);
    };
    let raw = VaultSource::read(home, name).text()?;
    let source = crate::vault_entry::source_name(name);
    // A valid shared sheet can serve other pools without a Claude membership column.
    // Parse a column it does name, so malformed input is still an error rather than absence.
    let fleet = if name == ACCOUNTS_CSV_FILE
        && !csv_names_column(Cli::Claude, &raw)
        && let Some(cli) = OTHER_CLIS
            .into_iter()
            .find(|cli| csv_names_column(*cli, &raw))
    {
        parse_accounting_csv_for(cli, &raw, &source)?;
        FleetRoster::default()
    } else {
        let captured = |_: &Env, _: &str| Ok(raw.clone());
        let (fleet, _, pool) = load_fleet_source_for_machine(env, Some(&source), None, &captured)?;
        // The unified pool has its own empty-assignment settlement, including the fleet
        // projection and swarm identity it publishes even without a Claude roster.
        if pool.is_some() {
            return Ok(InitAccountsAssignment::Required);
        }
        fleet
    };
    let machine = fleet
        .alab
        .as_ref()
        .map(|machine| machine.key.as_str().to_owned())
        .or_else(|| machine_for(env.config.as_deref(), None))
        .ok_or("could not determine machine id (`m<N>`); pass `--machine m8`")?;
    if fleet.accounts_for_machine(&machine).is_empty()
        && fleet.conflicts_for(Some(&machine)).is_empty()
    {
        Ok(InitAccountsAssignment::Unassigned {
            machine,
            fleet_accounts: fleet.accounts.len(),
        })
    } else {
        Ok(InitAccountsAssignment::Required)
    }
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
    let native = match super::alab_roster::chosen(
        Cli::Claude,
        env.accounts_roster().as_deref(),
        source,
        env.config.as_deref(),
    )? {
        Chosen::Lab(fleet) => Some(fleet),
        Chosen::Unnamed => None,
        // A lab not read yet gives no fleet, and no other source stands in for it.
        Chosen::Pending(why) => return Err(why),
    };
    load_selected_fleet_source(env, source, machine_override, fetch, native)
}

fn load_selected_fleet_source(
    env: &Env,
    source: Option<&str>,
    machine_override: Option<&str>,
    fetch: &Fetch<'_>,
    native: Option<FleetRoster>,
) -> Result<(FleetRoster, OptIn, Option<PoolSyncCtx>), String> {
    if let Some(fleet) = native {
        return Ok((fleet, OptIn::Named, None));
    }
    let (chosen, opt_in) = match source.map(str::trim).filter(|s| !s.is_empty()) {
        Some(named) => (named.to_owned(), OptIn::Named),
        None => {
            let selected = source_selection::select_for(
                &env.accounts_store(),
                env.home.as_deref(),
                env.config.as_deref(),
                env.accounts_roster().as_deref(),
                SourceScope::Configured,
            )?;
            (
                selected.source().ok_or_else(|| NO_SOURCE.to_owned())?,
                selected.opt_in(),
            )
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
        )?
        .ok_or_else(|| "could not determine machine id (`m<N>`); pass `--machine m8`".to_owned())?;
        let fleet = project_pool_fleet(&pool, Cli::Claude, Some(&identity));
        return Ok((fleet, opt_in, Some(PoolSyncCtx { pool, identity })));
    }
    let toml = chosen.ends_with(".toml")
        || raw.trim_start().starts_with('#')
        || raw.contains("[[account]]");
    let fleet = if toml {
        FleetRoster::from_toml(&raw, &chosen)
    } else if sheet_read_here() {
        parse_accounting_csv(&raw, &chosen)
    } else {
        Err(sheet_refused(&chosen, Cli::Claude))
    }?;
    // A discovered fleet stays discovery's through every sync of the file or entry that holds it.
    let opt_in = if opt_in == OptIn::Named
        && toml
        && matches!(
            fetch_plan(env, &chosen),
            Ok(FetchPlan::File(_) | FetchPlan::Entry(_))
        )
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
    discovered_fleet(
        env,
        &local_shows(env, &read_local_source_text),
        min_accounts,
    )
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
fn local_shows(env: &Env, read: &LocalSourceRead<'_>) -> Shown {
    let mut shown = Shown::default();
    let record = |into: &mut BTreeSet<String>, candidate: &str| {
        let trimmed = candidate.trim();
        if !trimmed.is_empty() && accounts::email_refusal(trimmed).is_none() {
            into.insert(trimmed.to_owned());
        }
    };
    let default = default_profile_path(env)
        .and_then(|path| profile_email(&path, read).map(|email| (path, email)));
    if let Some((_, de)) = &default {
        record(&mut shown.claude, de);
    }
    if let Some(home) = &env.home {
        if let Ok(entries) = trace::fs::read_dir(home) {
            for entry in entries {
                let name = entry.file_name();
                let Some(name_str) = name.to_str() else {
                    continue;
                };
                if !name_str.starts_with(".claude.json")
                    || !entry.file_type().is_ok_and(|ft| ft.is_file())
                {
                    continue;
                }
                let path = entry.path();
                // Keep every backup or distinct effective profile, and retry an earlier
                // failed read. Only this call's successful exact lexical path is paid.
                if default
                    .as_ref()
                    .is_none_or(|(seen, _)| seen.as_os_str() != path.as_os_str())
                    && let Ok(text) = read(&path)
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
        // Muse Code's login store where a Muse Code started by this command would keep it.
        let muse_env: Vec<String> = env
            .xdg_config_home
            .iter()
            .map(|xdg| format!("XDG_CONFIG_HOME={xdg}"))
            .collect();
        let muse_auth =
            super::profile::muse_config_home(&muse_env, home).join(super::profile::MUSE_AUTH_FILE);
        if let Ok(text) = read(&muse_auth)
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
    if let Ok(entries) = trace::fs::read_dir(accounts::Store::at(&env.state).logins_dir()) {
        for entry in entries {
            if !entry.file_type().is_ok_and(|ft| ft.is_dir()) {
                continue;
            }
            let dir_path = entry.path();
            if let Ok(text) = read(&dir_path.join(".claude.json"))
                && let Ok(v) = aterm_json::from_str::<Value>(&text)
                && let Some(em) = v
                    .get("oauthAccount")
                    .and_then(|o| o.get("emailAddress"))
                    .and_then(Value::as_str)
            {
                record(&mut shown.claude, em);
            }
            if let Ok(text) = read(&dir_path.join(accounts::RENEWAL_CHECKPOINT_FILE))
                && let Ok(v) = aterm_json::from_str::<Value>(&text)
                && let Some(em) = v.get("email").and_then(Value::as_str)
            {
                record(&mut shown.claude, em);
            }
        }
    }
    if let Some(claude_dir) = env.claude_dir() {
        if let Ok(backups) = trace::fs::read_dir(claude_dir.join("backups")) {
            for entry in backups {
                if entry.file_type().is_ok_and(|ft| ft.is_file())
                    && let Ok(text) = read(&entry.path())
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
        if let Ok(entries) = trace::fs::read_dir(claude_dir.join("projects")) {
            let mut files_with_mtime: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
            for proj in entries {
                if !proj.file_type().is_ok_and(|ft| ft.is_dir()) {
                    continue;
                }
                let Ok(files) = trace::fs::read_dir(proj.path()) else {
                    continue;
                };
                for file in files {
                    let path = file.path();
                    if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                        continue;
                    }
                    let mtime = trace::fs::symlink_metadata(&path)
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

/// The address the Claude Code profile in the login directory `dir` names, where it names one.
fn login_dir_address(dir: &Path) -> Option<String> {
    let text = read_local_source_text(&dir.join(".claude.json")).ok()?;
    let doc = aterm_json::from_str::<Value>(&text).ok()?;
    doc.get("oauthAccount")?
        .get("emailAddress")?
        .as_str()
        .map(str::to_owned)
}

/// The row of `roster` that holds `email`, whichever way a row can: its own login address or
/// provider identity first ([`accounts::email_matches`]), then the address the login it runs on
/// names, which an address of its own does not (`login_of`, the row's login directory's profile,
/// or the default login's for a row on it that names no address). So one row's login is never
/// listed as a second account (an alias with no `identity_email`, or a login that names someone
/// else: [`note_wrong_account_logins`] reports that one). A row on the default login that names
/// an address is not matched by what that login names: the login moved off it, and the sync's
/// `displaced` step follows the account.
fn row_holding<'a>(
    roster: &'a Roster,
    email: &str,
    login_of: &dyn Fn(&Row) -> Option<String>,
) -> Option<&'a Row> {
    let names = |held: Option<&str>| held.is_some_and(|held| accounts::email_matches(held, email));
    roster
        .rows
        .iter()
        .find(|row| names(row.email.as_deref()) || names(row.identity_email.as_deref()))
        .or_else(|| {
            roster
                .rows
                .iter()
                .filter(|row| row.dir.is_some() || row.expected_identity_email().is_none())
                .find(|row| names(login_of(row).as_deref()))
        })
}

/// The fleet discovery makes of what `shown` holds ([`Shown::all`]) for `env`'s machine, where
/// it lists at least `min_accounts`. An address a row of this Mac's roster holds ([`row_holding`])
/// is that row's, under the label, order, browser profile and vendor keys it has, and a row that
/// names no address has no line of it here and stays as it is ([`keep_discovered_rows`]); only an
/// address no row holds is labelled from the machine and the address's own slot, under a label no
/// row has.
fn discovered_fleet(env: &Env, shown: &Shown, min_accounts: usize) -> Option<FleetRoster> {
    let emails = shown.all();
    if emails.is_empty() {
        return None;
    }
    let machine = machine_for(env.config.as_deref(), None)?;
    let existing = env
        .accounts_roster()
        .and_then(|path| Roster::load(&path).ok())
        .unwrap_or_default();
    let default_address = read_default_profile_email(env);
    let login_of = |row: &Row| match row.dir.as_deref() {
        Some(dir) => login_dir_address(Path::new(dir)),
        None => default_address.clone(),
    };
    let mut held: Vec<&Row> = Vec::new();
    let mut rows_with_slot: Vec<(Option<u32>, Vec<String>, String)> = Vec::new();
    for email in emails {
        if let Some(row) = row_holding(&existing, &email, &login_of) {
            if !held.iter().any(|kept| kept.label == row.label) {
                held.push(row);
            }
            continue;
        }
        let (mut machines, slot) = email_machines_and_slot(&email);
        if machines.is_empty() {
            machines.push(machine.clone());
        }
        if !machines.contains(&machine) {
            continue;
        }
        rows_with_slot.push((slot, machines, email));
    }
    rows_with_slot.sort_by_key(|(slot, _, email)| (slot.unwrap_or(1), email.clone()));
    let mut used_labels: BTreeSet<String> =
        existing.rows.iter().map(|row| row.label.clone()).collect();
    let mut accounts: Vec<FleetAccount> = held
        .into_iter()
        .filter_map(|row| {
            Some(FleetAccount {
                label: row.label.clone(),
                machines: vec![machine.clone()],
                class: row.class,
                email: row.email.clone()?,
                browser: row.browser.clone(),
                vendor: row.vendor,
            })
        })
        .collect();
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
            Class::Pool
        } else {
            Class::Preferred
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
        ..FleetRoster::default()
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
pub(crate) fn session_context_email(line: &str) -> Option<String> {
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
pub(crate) fn each_bounded_line(
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
        trace::fs::opening(path);
        let file = match astream_sys::open_nonblocking(path, astream_sys::Links::Refuse) {
            Ok(file) => file,
            Err(error) => {
                trace::fs::failed(path, &error);
                continue;
            }
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

/// The text of the regular file at `path`, at most `max_bytes` of it, opened as `links` says.
/// With a trace scope armed it records the file's stamp before its open
/// ([`trace::fs::opening`]), and a retry where the open, the look at the opened file or the read
/// failed for a reason no stamp decides.
fn read_local_text_bounded(
    path: &Path,
    links: astream_sys::Links,
    max_bytes: usize,
) -> io::Result<String> {
    trace::fs::opening(path);
    let failed = |error: io::Error| {
        trace::fs::failed(path, &error);
        error
    };
    let not_regular = || {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "local account source is not a regular file",
        )
    };
    // Windows opens no directory as a file ("Access is denied"): it is refused as what it is.
    let file = astream_sys::open_nonblocking(path, links).map_err(|e| {
        if std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir()) {
            not_regular()
        } else {
            failed(e)
        }
    })?;
    let metadata = file.metadata().map_err(failed)?;
    if !metadata.is_file() {
        return Err(not_regular());
    }
    if metadata.len() > max_bytes as u64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "local account source exceeds the byte limit",
        ));
    }
    let mut bytes = Vec::with_capacity(
        usize::try_from(metadata.len())
            .unwrap_or(0)
            .min(max_bytes)
            .saturating_add(1),
    );
    file.take((max_bytes as u64).saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(failed)?;
    decode_source_text_bounded(bytes, &path.to_string_lossy(), max_bytes)
        .map_err(|why| io::Error::new(io::ErrorKind::InvalidData, why))
}

// A local source (the fleet file beside the roster, the vault's own state files) may be skipped
// when absent, never when unreadable. When an ancestor is missing, distinguish a normal absent
// directory from a dangling link or a non-directory obstruction. This probe grants no
// read/write authority; the bounded reader and publication snapshots still judge what they open.
fn source_entry_present(path: &Path) -> bool {
    let mut at = path;
    loop {
        match trace::fs::symlink_metadata(at) {
            Ok(_) if at == path => return true,
            Ok(_) => return trace::fs::metadata(at).map_or(true, |meta| !meta.is_dir()),
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

/// Whether the vault holds a source a sync from init reads under `home`: its pool, its fleet
/// (unless `store`'s policy retired it) or, where this Mac reads it, its sheet, from one version
/// of the vault; an entry this Mac cannot read now counts, so its sync says why.
fn has_fleet_source(home: &Path, store: &accounts::Store) -> bool {
    let Ok(policy) = store.source_policy() else {
        // Admit one diagnostic attempt; the ordinary failed-input hold paces it.
        return true;
    };
    let [pool, fleet, sheet] = source_selection::vault_sources(Some(home));
    let held = |entry: Option<VaultSource>| entry.is_some_and(|entry| entry.occupies());
    held(pool) || (!policy.retired() && held(fleet)) || (sheet_read_here() && held(sheet))
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum FetchPlan {
    Http(String),
    File(PathBuf),
    /// A vault entry, named `vault:<entry>`, read in place from this Mac's store.
    Entry(String),
}

/// Whether `path` lies in `home`'s `~/.secrets`, as spelled or as it resolves through links:
/// the files older builds wrote there are the owner's backup, which no source reads and no
/// projection writes. Resolving reads names, never what a file holds.
fn in_secrets_backup(home: &Path, path: &Path) -> bool {
    let backups = [
        Some(home.join(".secrets")),
        trace::fs::canonicalize(home)
            .ok()
            .map(|home| home.join(".secrets")),
    ];
    let in_backup = |path: &Path| backups.iter().flatten().any(|b| path.starts_with(b));
    in_backup(path) || trace::fs::canonicalize(path).is_ok_and(|resolved| in_backup(&resolved))
}

/// The refusal of a source path in the backup ([`in_secrets_backup`]), naming the vault entry
/// that stands for it.
fn secrets_backup_refusal(home: &Path, path: &Path) -> String {
    let entry = path
        .strip_prefix(home.join(".secrets"))
        .ok()
        .and_then(|rest| rest.to_str())
        .filter(|rest| !rest.is_empty())
        .unwrap_or("<entry>");
    format!(
        "{}: the files in ~/.secrets are the owner's backup and nothing reads them; name the \
         vault entry instead (`{}`)",
        path.display(),
        crate::vault_entry::source_name(entry)
    )
}

/// The refusal of a configured or named `source` whose path lies in `home`'s backup
/// ([`in_secrets_backup`]), resolved as [`fetch_plan`] resolves it (against `config`'s directory).
fn refuse_backup_source(home: &Path, config: Option<&Path>, source: &str) -> Result<(), String> {
    if source.starts_with("http://")
        || source.starts_with("https://")
        || crate::vault_entry::source_entry(source).is_some()
    {
        return Ok(());
    }
    let path = if let Some(relative) = source.strip_prefix("~/") {
        home.join(relative)
    } else {
        let base = config.and_then(Path::parent).unwrap_or(home);
        std::path::absolute(base)
            .map_err(|why| why.to_string())?
            .join(source)
    };
    if in_secrets_backup(home, &path) {
        return Err(secrets_backup_refusal(home, &path));
    }
    Ok(())
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
    if let Some(entry) = crate::vault_entry::source_entry(source) {
        if entry.starts_with("~/") || !aterm_secrets::text::valid_entry_name(entry) {
            return Err(format!("{source}: not a vault entry's name"));
        }
        return Ok(FetchPlan::Entry(entry.to_owned()));
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
    if let Some(home) = env.home.as_deref()
        && in_secrets_backup(home, &path)
    {
        return Err(secrets_backup_refusal(home, &path));
    }
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
        FetchPlan::Entry(name) => VaultSource::read(
            env.home
                .as_deref()
                .ok_or("a vault account source needs a home")?,
            &name,
        )
        .text(),
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
    /// What it did to rows it kept ([`RowChanges`]).
    rows_changed: RowChanges,
    /// Each account the lab prefers here left out for a label clash ([`FleetRoster::clashes`]).
    clashes: Vec<(String, String)>,
}

/// How a sync's receipt names `cli`'s accounts (`docs/DESIGN-shared-files-2026-11-11.md` §10): each
/// row of the roster the sync left by its account under the lab this process holds
/// ([`super::account_names::Listing`]), in its words by name and slug and in its `--json` by key,
/// slug and name; a row with no address by its label, marked `(no address)` in its words. A roster
/// label is this Mac's key for a row, which no receipt prints for a row with an address.
struct Naming {
    cli: Cli,
    rows: super::account_names::Listing,
    lab: Option<std::sync::Arc<super::alab_roster::Lab>>,
}

impl Naming {
    /// `cli`'s rows `rows`, as the sync left them, named under the lab this process holds.
    fn of(cli: Cli, rows: &[Row]) -> Self {
        let lab = super::alab_roster::current();
        Self {
            cli,
            rows: super::account_names::Listing::of(cli, rows, lab.as_deref()),
            lab,
        }
    }

    /// [`Naming::of`] the roster at `path`, as it stands; no row where it cannot be read.
    fn at(cli: Cli, path: &Path) -> Self {
        Self::of(cli, &Roster::load_for(cli, path).unwrap_or_default().rows)
    }

    /// The row labelled `label`, as the receipt's words name it.
    fn shown<'a>(&'a self, label: &'a str) -> std::borrow::Cow<'a, str> {
        self.rows.shown(label)
    }

    /// The rows labelled `labels`, as the receipt's words name them, one after another.
    fn shown_all(&self, labels: &[&str]) -> String {
        labels
            .iter()
            .map(|label| self.shown(label))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// The row labelled `label` as the receipt's `--json` names it: an object of its account's
    /// key, slug and name, and its label only where it has no key.
    fn json(&self, label: &str) -> Value {
        let mut m = Map::new();
        super::cli::put_account_names(&mut m, &self.rows, label);
        Value::Object(m)
    }

    /// The rows labelled `labels`, each as [`Naming::json`] names it.
    fn json_all<'a>(&self, labels: impl IntoIterator<Item = &'a String>) -> Value {
        Value::Array(labels.into_iter().map(|label| self.json(label)).collect())
    }

    /// The account of a source's row that lists `email` in Kimi Code's `region`, named as no row
    /// here holds it ([`super::account_names::names_of_listed`]): `None` for a Kimi Code row the
    /// source places in no region.
    fn listed(
        &self,
        region: Option<Region>,
        email: &str,
    ) -> Option<(String, aterm_file::AccountNames)> {
        super::account_names::names_of_listed(self.cli, region, email, self.lab.as_deref())
    }

    /// A source's row labelled `label`, listing `email` in `region`, as the receipt's words name
    /// it: its account's name and slug, else its label marked so (`(no region)` for a Kimi Code
    /// row no region places, [`super::account_names::unkeyed`]).
    fn listed_shown(&self, label: &str, region: Option<Region>, email: &str) -> String {
        self.listed(region, email).map_or_else(
            || {
                let mark = super::account_names::unkeyed(self.cli, region, !email.is_empty());
                format!("{label} {mark}")
            },
            |(pk, names)| {
                super::account_names::Named {
                    pk,
                    row: None,
                    names,
                }
                .shown()
            },
        )
    }

    /// A source's row as [`Naming::listed_shown`] names it, in the `--json`: its account's key,
    /// slug and name, and its label only where it has no key.
    fn listed_json(&self, label: &str, region: Option<Region>, email: &str) -> Value {
        let named = self.listed(region, email);
        let mut m = Map::new();
        super::cli::put_names(
            &mut m,
            named.as_ref().map(|(pk, names)| (pk.as_str(), names)),
            label,
        );
        Value::Object(m)
    }

    /// The Kimi Code region of the account the lab prefers here that a sync left out for a label
    /// clash, its lab label `label` and address `email` ([`FleetRoster::clashes`]): the region the
    /// lab's account of that label and address is in, which names that account.
    fn clashed(&self, label: &str, email: &str) -> Option<Region> {
        self.lab.as_deref().and_then(|lab| {
            lab.of(self.cli)
                .find(|account| account.label == label && account.email == email)
                .and_then(|account| account.vendor.region)
        })
    }

    /// The accounts a sync left out for a label clash, in its `--json`: each by its account's
    /// key, slug and name, with the row here that holds the label it would take (`held_by`), by
    /// its own.
    fn clashes_json(&self, clashes: &[(String, String)]) -> Value {
        Value::Array(
            clashes
                .iter()
                .map(|(label, email)| {
                    let region = self.clashed(label, email);
                    let mut account = self.listed_json(label, region, email);
                    if let Some(m) = account.as_object_mut() {
                        m.insert("held_by".to_owned(), self.json(label));
                    }
                    account
                })
                .collect(),
        )
    }

    /// The accounts a sync left out for a label clash, one line each in its receipt: the account
    /// by its names, and the row here that holds the label it would take by its own.
    fn write_clash_lines(&self, clashes: &[(String, String)], out: &mut dyn Write) {
        for (label, email) in clashes {
            let region = self.clashed(label, email);
            let _ = writeln!(
                out,
                "left out: {}, which the lab prefers here, since this Mac's row of {} holds the \
                 label a row of it would take",
                self.listed_shown(label, region, email),
                self.shown(label)
            );
        }
    }
}

/// Why a sync did not follow Codex's default login to the row of the address it names
/// ([`reassign_codex_default`]), in the words of each place the refusal reaches
/// (`docs/DESIGN-shared-files-2026-11-11.md` §10): `said`, the sync's receipt to a person, which
/// names each account by its name and slug and may say an address; and `logged`, the words that
/// leave the receipt (`aterm init`'s journal through [`sync_cli_accounts_for_init`], a launch's
/// refusal and its facts through [`default_move_verdict`]), which name each account by its key and
/// slug and say no address. A refusal that names no account reads the same in both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveRefusal {
    /// The receipt's words.
    pub said: String,
    /// The words of a log, a journal and a launch's facts.
    pub logged: String,
}

impl MoveRefusal {
    /// The refusal with `more` after both its forms (`; no row moved`).
    fn and(self, more: &str) -> Self {
        Self {
            said: format!("{}{more}", self.said),
            logged: format!("{}{more}", self.logged),
        }
    }
}

impl From<String> for MoveRefusal {
    fn from(why: String) -> Self {
        Self {
            said: why.clone(),
            logged: why,
        }
    }
}

impl From<&str> for MoveRefusal {
    fn from(why: &str) -> Self {
        why.to_owned().into()
    }
}

/// What a sync of Codex read of its default login and did about it: one account listed twice
/// beside it ([`merge_default_twin`]), or a row it moved off ([`reassign_codex_default`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DefaultLogin {
    /// The address the default login names, where one can be read
    /// ([`accounts::default_login_address`]).
    pub address: Option<String>,
    /// The row on the default login, which now names the address, and the row of that address the
    /// sync folded into it, which held no login or record.
    pub merged: Option<(String, String)>,
    /// The row on the default login, the row of its address, and what keeps them both.
    pub twin: Option<(String, String, TwinKept)>,
    /// The row that took the default login, the one of the address it names.
    pub reassigned: Option<String>,
    /// The row still on the default login though that login names another address, and the row's
    /// own address ([`accounts::default_login_moved_off`]).
    pub moved_off: Option<(String, String)>,
    /// Whether [`Self::moved_off`]'s row has an address and therefore renews unattended.
    moved_off_addressed: bool,
    /// The word [`Self::moved_off`]'s row's sign-in names it by
    /// ([`super::account_names::sign_in_word`]).
    moved_off_word: Option<Word>,
    /// Why the login could not be followed to the row of its address, in the receipt's words and
    /// a log's ([`MoveRefusal`]). What the sync wrote before that stands, and its receipt says so.
    pub refused: Option<MoveRefusal>,
}

impl DefaultLogin {
    /// Whether the login names another account than its row, which a sync with no source to read
    /// still answers for, since following it reads none.
    fn stands(&self) -> bool {
        self.reassigned.is_some() || self.moved_off.is_some() || self.refused.is_some()
    }

    /// Its fields in a sync's `--json`, each row by its account under `names` (the roster the
    /// sync left, [`Naming`]): its key, slug and name, and its label only where it has no key, as
    /// the row on the default login before it names an address.
    fn put_json(&self, names: &Naming, o: &mut Map) {
        o.insert(
            "default_login_names".to_owned(),
            self.address.clone().map_or(Value::Null, Value::from),
        );
        // The row folded away was the same account as the row on the default login, which now
        // names it: the account is that row's.
        o.insert(
            "twin_merged".to_owned(),
            self.merged
                .as_ref()
                .map_or(Value::Null, |(row, _)| names.json(row)),
        );
        o.insert(
            "twin".to_owned(),
            self.twin.as_ref().map_or(Value::Null, |(row, twin, kept)| {
                let mut m = Map::new();
                m.insert("row".to_owned(), names.json(row));
                m.insert("twin".to_owned(), names.json(twin));
                m.insert("kept".to_owned(), Value::from(kept.as_str().to_owned()));
                Value::Object(m)
            }),
        );
        o.insert(
            "own_row".to_owned(),
            self.reassigned
                .as_ref()
                .map_or(Value::Null, |row| names.json(row)),
        );
        o.insert(
            "default_login_moved_off".to_owned(),
            self.moved_off.as_ref().map_or(Value::Null, |(row, email)| {
                let mut m = Map::new();
                m.insert("row".to_owned(), names.json(row));
                m.insert("email".to_owned(), Value::from(email.clone()));
                Value::Object(m)
            }),
        );
        o.insert(
            "default_login_refused".to_owned(),
            self.refused
                .as_ref()
                .map_or(Value::Null, |refused| Value::from(refused.said.clone())),
        );
    }

    /// Its lines in a sync's receipt, each row by its account under `names` ([`Naming`]): its name
    /// and slug, and the row on the default login before it names an address by its label marked
    /// `(no address)`.
    fn write_lines(&self, names: &Naming, program: Option<&Path>, out: &mut dyn Write) {
        let address = self.address.as_deref().unwrap_or("another account");
        if let Some((row, _)) = &self.merged {
            let _ = writeln!(
                out,
                "one account: the row on the default login named no address and another row was \
                 {address}; that one held no login or record, so {} on the default login now \
                 stands for both",
                names.shown(row)
            );
        }
        if let Some((row, twin, kept)) = &self.twin {
            let (row, twin) = (names.shown(row), names.shown(twin));
            let _ = writeln!(
                out,
                "one account: {row} on the default login and {twin} are both {address}; {}, so \
                 both stay, and no weekly limit moves a session between them",
                kept.words(&row, &twin)
            );
        }
        if let (Some((row, email)), None) = (&self.moved_off, &self.refused) {
            let _ = writeln!(
                out,
                "default login: it names {address}, and {} is on it: no launch takes it until \
                 that login names {email} again (`{}`)",
                names.shown(row),
                self.moved_off_step(program)
            );
        }
    }

    /// The step that signs [`Self::moved_off`]'s row in again, by the word its sign-in names it
    /// by.
    fn moved_off_step(&self, program: Option<&Path>) -> Step {
        super::accounts_pool::default_row_sign_in_step(
            Cli::Codex,
            self.moved_off_word.clone().unwrap_or_else(Word::chosen),
            self.moved_off_addressed,
            program,
        )
    }

    /// Why the sync did not follow the login to the row of its address, in the words of its error
    /// line, with the step every such refusal leaves; each row by its account under `names`.
    fn refusal_words(&self, names: &Naming, program: Option<&Path>) -> Option<String> {
        let why = &self.refused.as_ref()?.said;
        Some(match (&self.moved_off, &self.address) {
            (Some((row, email)), Some(address)) => format!(
                "the default login names {address}, and {} stays on it, so no launch takes it \
                 until that login names {email} again (`{}`): {why}",
                names.shown(row),
                self.moved_off_step(program)
            ),
            _ => why.to_owned(),
        })
    }
}

/// What keeps both rows of one account listed twice beside the default login
/// ([`default_twin_verdict`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TwinKept {
    /// The twin holds a login in its directory, or a record under its label.
    TwinHolds,
    /// A renewal's completion on this Mac names the twin, or the place they are kept could not be
    /// read, so nothing shows the twin owns nothing ([`local_checkpoint_holds_twin`]).
    TwinCheckpoint,
    /// The twin is held for another Mac or under shared custody.
    TwinHeld,
    /// The row on the default login is held for another Mac or under shared custody.
    RowHeld,
    /// The source gives the row on the default login's label another address.
    SourceNamesRow,
    /// The row on the default login is known signed out, which makes the login a stale one.
    RowSignedOut,
    /// No sync of this pool reads a source here, or none it can resolve this machine in.
    NoSync,
}

impl TwinKept {
    /// The word a sync's and a report's `--json` give it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TwinHolds => "twin-holds",
            Self::TwinCheckpoint => "twin-checkpoint",
            Self::TwinHeld => "twin-held",
            Self::RowHeld => "row-held",
            Self::SourceNamesRow => "source-names-row",
            Self::RowSignedOut => "row-signed-out",
            Self::NoSync => "no-sync",
        }
    }

    /// Why both rows stay, in a receipt's or a report's words, each row as the caller names it (by
    /// its account's name and slug, the row on the default login by its label marked `(no
    /// address)`): `row` the row on the default login, `twin` the other.
    #[must_use]
    pub fn words(self, row: &str, twin: &str) -> String {
        match self {
            Self::TwinHolds => format!("{twin} holds a login or a record of its own"),
            Self::TwinCheckpoint => {
                format!("a renewal on this Mac may have completed for {twin}")
            }
            Self::TwinHeld => format!("{twin} is another Mac's or in shared custody"),
            Self::RowHeld => format!("{row} is another Mac's or in shared custody"),
            Self::SourceNamesRow => format!("the source gives {row} another address"),
            Self::RowSignedOut => format!("{row} is known signed out"),
            Self::NoSync => "no sync reads a source here".to_owned(),
        }
    }
}

/// THE ONE RULE of whether a sync folds `twin` into `own`, the row on the default login, whose login
/// names `login`, on `fleet`, the source that sync reads: `Ok` where it does, else what keeps both.
/// [`merge_default_twin`] acts on it and the report's `one account:` names that sync by it, each on
/// the rows as that sync's source leaves them ([`rows_under_source`]). A source is read for what it
/// says of `own`'s label; a row known signed out holds a stale login.
fn default_twin_verdict(
    cli: Cli,
    (own, twin): (&Row, &Row),
    login: &str,
    fleet: &FleetRoster,
    (state, logins, store): (&accounts::State, &accounts::Logins, &accounts::Store),
    now: i64,
) -> Result<(), TwinKept> {
    default_row_may_name(own, login, fleet, (state, logins), now)?;
    if !row_free_here(twin, logins) {
        return Err(TwinKept::TwinHeld);
    }
    if twin
        .dir
        .as_deref()
        .is_none_or(|dir| accounts::login_made(Path::new(dir)))
        || accounts::label_is_retained(state, logins, &twin.label)
    {
        return Err(TwinKept::TwinHolds);
    }
    if local_checkpoint_holds_twin(cli, store, twin, now) {
        return Err(TwinKept::TwinCheckpoint);
    }
    Ok(())
}

/// Whether `own`, the row on the default login, may take the address that login names, or what
/// keeps it from that: it is free here, the source gives its label no other, and it is not known
/// signed out, which would make the login a stale one.
fn default_row_may_name(
    own: &Row,
    login: &str,
    fleet: &FleetRoster,
    (state, logins): (&accounts::State, &accounts::Logins),
    now: i64,
) -> Result<(), TwinKept> {
    if !row_free_here(own, logins) {
        return Err(TwinKept::RowHeld);
    }
    if fleet.accounts.iter().any(|account| {
        account.label == own.label && !accounts::email_matches(&account.email, login)
    }) {
        return Err(TwinKept::SourceNamesRow);
    }
    if accounts::condition(own, state, true, now) == accounts::Condition::SignedOut {
        return Err(TwinKept::RowSignedOut);
    }
    Ok(())
}

/// `rows` as a sync of `cli` from this source leaves their holds and orders before it reads them
/// for anything else; whether any changed. A pool file names each row's holder; under the lab nothing
/// limits who runs an account, so a hold an old pool file set goes, and a row the lab knows takes its
/// derived order. Any other source changes none.
fn rows_under_source(
    cli: Cli,
    fleet: &FleetRoster,
    pool_ctx: Option<&PoolSyncCtx>,
    rows: &mut [Row],
) -> bool {
    let mut changed = false;
    if let Some(ctx) = pool_ctx {
        for row in rows {
            if row.email.is_some() || row.identity_email.is_some() {
                let want = ctx.holder_for_row(cli, row);
                if row.held_by != want {
                    row.held_by = want;
                    changed = true;
                }
            }
        }
    } else if let Some(bound) = &fleet.alab {
        let lab = fleet.lab_snapshot.as_deref();
        for row in &mut *rows {
            changed |= row.held_by.take().is_some();
            if let Some(class) = lab
                .and_then(|lab| lab.account_of(cli, row))
                .map(|account| {
                    if account.preferred_on(&bound.key).is_some() {
                        Class::Preferred
                    } else {
                        Class::Pool
                    }
                })
                .filter(|class| *class != row.class)
            {
                row.class = class;
                changed = true;
            }
        }
        changed |= super::alab_roster::order_rows(lab, cli, &bound.key, rows);
    }
    changed
}

/// What a sync of `cli` with no source named would do with the row on the default login `own` and
/// its twin, the rows of `roster`, on the records as they stand, reading what that sync reads
/// ([`load_cli_fleet_source`], fetching nothing) and writing nothing: the report's `one account:`.
pub(super) fn sync_twin_verdict(
    cli: Cli,
    env: &Env,
    (own, twin, login): (&str, &str, &str),
    roster: &Roster,
    (state, logins): (&accounts::State, &accounts::Logins),
    now: i64,
) -> Result<(), TwinKept> {
    let home = env.home.as_deref().ok_or(TwinKept::NoSync)?;
    let no_network = |_: &Env, _: &str| Err("a report fetches no source".to_owned());
    let (fleet, pool_ctx, machine) = load_cli_fleet_source(cli, env, home, None, None, &no_network)
        .ok()
        .flatten()
        .ok_or(TwinKept::NoSync)?;
    // A sync of a roster that stands refuses where no machine resolves.
    machine.ok_or(TwinKept::NoSync)?;
    let mut rows = roster.rows.clone();
    rows_under_source(cli, &fleet.roster, pool_ctx.as_ref(), &mut rows);
    let rows = accounts::bound(&Roster { rows }, logins);
    let (Some(own), Some(twin)) = (rows.row(own), rows.row(twin)) else {
        return Err(TwinKept::NoSync);
    };
    let store = env.accounts_store_for(cli).ok_or(TwinKept::NoSync)?;
    default_twin_verdict(
        cli,
        (own, twin),
        login,
        &fleet.roster,
        (state, logins, &store),
        now,
    )
}

/// Where the row on the default login names no address and another row names the one that login
/// names (Codex's `auth.json` under a file store, [`accounts::default_login_address`]: no keyring
/// is read and no CLI runs), one account is listed twice. A fold keeps the default row's label,
/// assigns the address and removes the matching row or proposal. Where policy or local state
/// prevents folding, both stay and the receipt says so.
/// A row held for another Mac, under shared custody or bound to a directory elsewhere is left as it
/// is, as is every row where nothing can be read. A default row that names another address is
/// [`reassign_codex_default`]'s.
fn merge_default_twin(
    cli: Cli,
    fleet: &FleetRoster,
    store: &accounts::Store,
    (kept, new, added): (&mut Vec<Row>, &mut Vec<Row>, &mut Vec<String>),
    now: impl Into<trace::Now>,
) -> Result<DefaultLogin, String> {
    let now: trace::Now = now.into();
    let Some(login) = store
        .user_home()
        .and_then(|home| accounts::default_login_address(cli, home))
    else {
        return Ok(DefaultLogin::default());
    };
    let mut follow = DefaultLogin {
        address: Some(login.clone()),
        ..DefaultLogin::default()
    };
    let (logins, _) = store.load_logins(now)?;
    let bound = accounts::bound(&Roster { rows: kept.clone() }, &logins);
    let Some(own) = bound.row_for_login(None) else {
        return Ok(follow);
    };
    if own.expected_identity_email().is_some() {
        return Ok(follow);
    }
    let holds = |row: &Row| {
        row.expected_identity_email()
            .is_some_and(|held| accounts::email_matches(held, &login))
    };
    let mut state = store.load(now)?.0;
    store.hydrate_auth_contexts(&mut state, &bound);
    let own_label = own.label.clone();
    if let Some(twin) = new.iter().find(|row| holds(row)).cloned() {
        // A row this sync would add holds nothing yet but a local completion of its label. A
        // twin is judged: its verdict compares the reading as a number.
        let now = now.raw();
        let verdict =
            default_row_may_name(own, &login, fleet, (&state, &logins), now).and_then(|()| {
                if local_checkpoint_holds_twin(cli, store, &twin, now) {
                    Err(TwinKept::TwinCheckpoint)
                } else {
                    Ok(())
                }
            });
        match verdict {
            Ok(()) => {
                new.retain(|row| row.label != twin.label);
                added.retain(|label| *label != twin.label);
                name_row(kept, &own_label, &login);
                follow.merged = Some((own_label, twin.label));
            }
            Err(why) => follow.twin = Some((own_label, twin.label, why)),
        }
    } else if let Some(twin) = bound.rows.iter().find(|row| holds(row)).cloned() {
        // A twin is judged: its verdict compares the reading as a number.
        let now = now.raw();
        match default_twin_verdict(
            cli,
            (own, &twin),
            &login,
            fleet,
            (&state, &logins, store),
            now,
        ) {
            Ok(()) => {
                name_row(kept, &own_label, &login);
                kept.retain(|row| row.label != twin.label);
                follow.merged = Some((own_label, twin.label));
            }
            Err(why) => follow.twin = Some((own_label, twin.label, why)),
        }
    }
    Ok(follow)
}

/// `label`'s row of `rows` takes the address `email`.
fn name_row(rows: &mut [Row], label: &str, email: &str) {
    if let Some(row) = rows.iter_mut().find(|row| row.label == label) {
        row.email = Some(email.to_owned());
    }
}

/// Whether a sync may change `row` as this Mac's own: no other Mac's hold, and no shared custody.
fn row_free_here(row: &Row, logins: &accounts::Logins) -> bool {
    row.held_by.is_none() && matches!(logins.shared.required(row), Ok(None))
}

/// A local completion retains its actual checked owner, not the requested directory prefix.
/// This is only a retention decision: recovery still checks parent, identity and custody.
fn local_checkpoint_holds_twin(cli: Cli, store: &accounts::Store, twin: &Row, now: i64) -> bool {
    match local_renewal_checkpoints(cli, store, now) {
        Ok(scan) => scan.holds_twin(cli, twin),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        // An unreadable local root cannot establish that the row owns nothing.
        Err(_) => true,
    }
}

struct LocalRenewalCheckpoint {
    prefix: String,
    sequence: u64,
    dir: PathBuf,
    actual_dir: PathBuf,
    proof: accounts::RenewalCheckpoint,
}

struct LocalRenewalScan {
    checkpoints: Vec<LocalRenewalCheckpoint>,
    /// All entries and candidate I/O were available; absent or invalid proofs remain complete.
    complete: bool,
}

impl LocalRenewalScan {
    fn holds_twin(&self, cli: Cli, twin: &Row) -> bool {
        !self.complete
            || self.checkpoints.iter().any(|candidate| {
                candidate.proof.label == twin.label
                    && accounts::checkpoint_identity_matches(cli, twin, &candidate.proof)
            })
    }
}

fn staged_login_present(cli: Cli, dir: &Path) -> bool {
    staged_login_present_checked(cli, dir).unwrap_or(false)
}

fn staged_login_present_checked(cli: Cli, dir: &Path) -> std::io::Result<bool> {
    if cli == Cli::Claude {
        return accounts::login_made_checked(dir);
    }
    let mut unavailable = None;
    for file in cli.connector().stores {
        match trace::fs::metadata(dir.join(file)) {
            Ok(metadata) if metadata.is_file() => return Ok(true),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => unavailable = Some(error),
        }
    }
    unavailable.map_or(Ok(false), Err)
}

/// Decode the same completed local work for retention and recovery. No provider is queried.
/// Each checkpoint's read takes the reading `now` raw, so a scan that found none compares
/// nothing.
fn local_renewal_checkpoints(
    cli: Cli,
    store: &accounts::Store,
    now: impl Into<trace::Now>,
) -> std::io::Result<LocalRenewalScan> {
    let now: trace::Now = now.into();
    Ok(decode_local_renewal_checkpoints(
        cli,
        trace::fs::read_dir(store.logins_dir())?.into_iter().map(Ok),
        std::fs::DirEntry::file_type,
        |dir, actual| accounts::read_renewal_checkpoint_checked_for(cli, dir, actual, now.raw()),
    ))
}

fn decode_local_renewal_checkpoints(
    cli: Cli,
    entries: impl IntoIterator<Item = std::io::Result<std::fs::DirEntry>>,
    entry_type: impl Fn(&std::fs::DirEntry) -> std::io::Result<std::fs::FileType>,
    checkpoint_read: impl Fn(&Path, &Path) -> std::io::Result<Option<accounts::RenewalCheckpoint>>,
) -> LocalRenewalScan {
    let mut complete = true;
    let checkpoints = entries
        .into_iter()
        .filter_map(|entry| {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => {
                    complete = false;
                    return None;
                }
            };
            let name = entry.file_name().into_string().ok()?;
            let (prefix, suffix) = name.rsplit_once('+')?;
            if prefix.is_empty() || suffix.is_empty() || !suffix.bytes().all(|b| b.is_ascii_digit())
            {
                return None;
            }
            let sequence = suffix.parse().ok()?;
            let kind = match entry_type(&entry) {
                Ok(kind) => kind,
                Err(_) => {
                    complete = false;
                    return None;
                }
            };
            if !kind.is_dir() {
                return None;
            }
            let dir = entry.path();
            match staged_login_present_checked(cli, &dir) {
                Ok(true) => {}
                Ok(false) => return None,
                Err(_) => {
                    complete = false;
                    return None;
                }
            }
            let actual_dir = match trace::fs::canonicalize(&dir) {
                Ok(dir) => dir,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
                Err(_) => {
                    complete = false;
                    return None;
                }
            };
            // A vendor profile alone is only a hint. A valid completed check names the owner.
            let proof = match checkpoint_read(&dir, &actual_dir) {
                Ok(proof) => proof?,
                Err(_) => {
                    complete = false;
                    return None;
                }
            };
            Some(LocalRenewalCheckpoint {
                prefix: prefix.into(),
                sequence,
                dir,
                actual_dir,
                proof,
            })
        })
        .collect();
    LocalRenewalScan {
        checkpoints,
        complete,
    }
}

/// What a sync did to rows it kept, so its receipt can name each: a hold it cleared or set, and an
/// order it changed. A row's hold decides whether aterm signs it in here, so a change to one is
/// never left for a person to find in the roster.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RowChanges {
    /// Each row whose hold changed: its label, the Mac it was held for, the Mac it is held for.
    pub holds: Vec<(String, Option<String>, Option<String>)>,
    /// Each row whose order changed: its label, the order it had, the order it has.
    pub orders: Vec<(String, Class, Class)>,
}

impl RowChanges {
    /// What differs between `was` and `now` for each row in both, by label.
    fn between(was: &[Row], now: &[Row]) -> Self {
        let mut changes = Self::default();
        for prev in was {
            let Some(row) = now.iter().find(|row| row.label == prev.label) else {
                continue;
            };
            if prev.held_by != row.held_by {
                changes.holds.push((
                    prev.label.clone(),
                    prev.held_by.clone(),
                    row.held_by.clone(),
                ));
            }
            if prev.class != row.class {
                changes
                    .orders
                    .push((prev.label.clone(), prev.class, row.class));
            }
        }
        changes
    }

    /// Their `holds_changed` and `orders_changed` in a sync's `--json`, each row by its account
    /// under `names` ([`Naming`]): its key, slug and name, and its label only where it has no key.
    fn put_json(&self, names: &Naming, o: &mut Map) {
        let word = |held: &Option<String>| held.clone().map_or(Value::Null, Value::from);
        let changed = |label: &str, was: Value, now: Value| {
            let mut m = Map::new();
            super::cli::put_account_names(&mut m, &names.rows, label);
            m.insert("was".to_owned(), was);
            m.insert("now".to_owned(), now);
            Value::Object(m)
        };
        o.insert(
            "holds_changed".to_owned(),
            Value::Array(
                self.holds
                    .iter()
                    .map(|(label, was, now)| changed(label, word(was), word(now)))
                    .collect(),
            ),
        );
        o.insert(
            "orders_changed".to_owned(),
            Value::Array(
                self.orders
                    .iter()
                    .map(|(label, was, now)| {
                        changed(
                            label,
                            Value::from(was.as_str().to_owned()),
                            Value::from(now.as_str().to_owned()),
                        )
                    })
                    .collect(),
            ),
        );
    }

    /// Their lines in a sync's receipt, one for each row, each row by its account under `names`
    /// ([`Naming`]).
    fn write_lines(&self, names: &Naming, out: &mut dyn Write) {
        for (label, was, now) in &self.holds {
            let row = names.shown(label);
            let _ = match (was, now) {
                (Some(was), None) => writeln!(
                    out,
                    "hold cleared: {row} was held for {was}; this Mac may sign it in and run it \
                     now"
                ),
                (_, Some(now)) => writeln!(out, "held: {row} is held for {now} now"),
                (None, None) => Ok(()),
            };
        }
        for (label, was, now) in &self.orders {
            let _ = writeln!(
                out,
                "order changed: {} from {} to {}",
                names.shown(label),
                was.as_str(),
                now.as_str()
            );
        }
    }
}

/// Where a captured fleet input was read: a local file, or a vault entry read in place.
#[derive(Clone, Debug, PartialEq, Eq)]
enum SnapshotAt {
    File(PathBuf),
    Entry(String),
}

impl std::fmt::Display for SnapshotAt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::File(path) => write!(f, "{}", path.display()),
            Self::Entry(name) => f.write_str(&crate::vault_entry::source_name(name)),
        }
    }
}

#[derive(Clone)]
struct FleetSnapshot {
    at: SnapshotAt,
    bytes: Option<Vec<u8>>,
}

/// What `at` holds now, `None` where it is not there.
fn snapshot_bytes(home: Option<&Path>, at: &SnapshotAt) -> Result<Option<Vec<u8>>, String> {
    match at {
        SnapshotAt::File(path) => match read_local_source_text(path) {
            Ok(text) => Ok(Some(text.into_bytes())),
            Err(why) if why.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(why) => Err(why.to_string()),
        },
        SnapshotAt::Entry(name) => {
            let home = home.ok_or("a vault account source needs a home")?;
            Ok(
                crate::vault_entry::read_text(home, name, SOURCE_TEXT_MAX_BYTES)
                    .into_text()?
                    .map(String::into_bytes),
            )
        }
    }
}

fn validate_fleet_snapshots(
    home: Option<&Path>,
    snapshots: &[FleetSnapshot],
) -> Result<(), SyncError> {
    for expected in snapshots {
        let current = match snapshot_bytes(home, &expected.at) {
            Ok(bytes) => bytes,
            Err(why) => {
                return Err(SyncError::retry(
                    Retry::Unavailable,
                    format!(
                        "could not check captured fleet at {}: {why}; nothing was synced",
                        expected.at
                    ),
                ));
            }
        };
        if current != expected.bytes {
            return Err(SyncError::retry(
                Retry::Changed,
                "fleet accounts changed after init discovery; nothing was synced".to_owned(),
            ));
        }
    }
    Ok(())
}

/// Inputs stay separate from generated outputs, even for explicitly requested imports: no
/// output lies in `home`'s `~/.secrets` (the owner's backup, which nothing writes), and none
/// aliases a local input the sync read (`read_only`). A vault entry is read by name and no
/// output can alias it.
#[derive(Default)]
struct SourceProtection {
    home: Option<PathBuf>,
    read_only: Vec<PathBuf>,
}

impl SourceProtection {
    fn check(&self, outputs: &[&Path]) -> Result<(), String> {
        #[cfg(test)]
        if SOURCE_WRITES_MUTANT.get() == 3 {
            return Ok(());
        }
        if let Some(home) = &self.home {
            // The backup as spelled and as resolved; each output as spelled and as it resolves,
            // through a link at its leaf where it exists, else through its parent.
            let backups = [
                Some(home.join(".secrets")),
                trace::fs::canonicalize(home)
                    .ok()
                    .map(|home| home.join(".secrets")),
            ];
            for output in outputs {
                let absolute = std::path::absolute(output).map_err(|why| why.to_string())?;
                let resolved = trace::fs::canonicalize(&absolute).ok().or_else(|| {
                    absolute
                        .parent()
                        .and_then(|parent| trace::fs::canonicalize(parent).ok())
                        .zip(absolute.file_name())
                        .map(|(parent, name)| parent.join(name))
                });
                let in_backup = |path: &Path| backups.iter().flatten().any(|b| path.starts_with(b));
                if in_backup(&absolute) || resolved.as_deref().is_some_and(in_backup) {
                    return Err(
                        "an account projection would be written in ~/.secrets, which nothing \
                         writes"
                            .into(),
                    );
                }
            }
        }
        for input in &self.read_only {
            for output in outputs {
                if accounts::source_policy::aliases(output, input)? {
                    return Err("account projection aliases a preserved source input".into());
                }
            }
        }
        Ok(())
    }
}

fn sync_lock_ladder() -> Vec<Duration> {
    accounts::LOCK_LADDER
        .iter()
        .copied()
        .chain(std::iter::repeat_n(Duration::from_millis(500), 16))
        .collect()
}

/// The labels a sync moves this Mac's account state between where `next`, the roster it builds,
/// gives the default login's account another label than `existing` gave it: `(from, to)` pairs,
/// the default login's first; then, while a pair takes over a label `existing` gives another
/// account, that row's, to the label `next` gives its account, until a pair takes a label
/// `existing` does not use or one a pair leaves (a swap, or any cycle of labels). Empty where the
/// default login keeps its label or now holds another account (its row was displaced, not moved).
/// The default login's state follows its account whatever its new label held; each row moved
/// after it keeps its login directory where it holds a login ([`relocated_dirs`]).
/// # Errors
/// A label taken over is another account's that `next` gives no other row of its own (an
/// account the source no longer lists on this Mac, or a row with no unique login address), so
/// what is on record of that account has no label to move to.
fn default_relocation(existing: &Roster, next: &Roster) -> Result<Vec<(String, String)>, String> {
    let Some((was, now)) = existing.row_for_login(None).zip(next.row_for_login(None)) else {
        return Ok(Vec::new());
    };
    if was.label == now.label
        || !was
            .email
            .as_deref()
            .zip(now.email.as_deref())
            .is_some_and(|(was, now)| accounts::email_matches(was, now))
    {
        return Ok(Vec::new());
    }
    let mut moves = vec![(was.label.clone(), now.label.clone())];
    while let Some(held) = moves.last().and_then(|(_, to)| existing.row(to)) {
        if moves.iter().any(|(from, _)| *from == held.label) {
            break;
        }
        let goes = held
            .email
            .as_deref()
            .and_then(|email| retained_login_row(Some(next), email))
            .filter(|row| {
                row.dir.is_some() && row.label != held.label && moves.len() < existing.rows.len()
            });
        let Some(goes) = goes else {
            return Err(format!(
                "cannot relabel `{}` to `{}`: the previous roster already uses `{}` for another \
                 account, which the source gives no other label on this Mac; give the default \
                 login's account an unused label in the source, or list that account there under \
                 a label of its own; no new sync was published",
                was.label, now.label, held.label
            ));
        };
        moves.push((held.label.clone(), goes.label.clone()));
    }
    Ok(moves)
}

/// Every relabel a sync makes as a move: the default login's chain ([`default_relocation`])
/// first, then one pair for each other account in both rosters whose label changed
/// ([`same_account`]: the roster gives a provider identity one row), in the previous roster's
/// order, no label left or taken twice. A pair runs [`accounts::move_accounts`], so the
/// account's record and binding follow it to its new label rather than standing under the old
/// one for the account the source gives it (ATERM-KI-1028-6). An account the source no longer
/// lists has no pair: what it left at a destination is forgotten before the moves, and a binding
/// it left at any other label keeps its directory only in `was`, so no row is served it.
fn relabel_moves(
    existing: &Roster,
    next: &Roster,
    chain: &[(String, String)],
) -> Vec<(String, String)> {
    let mut moves = chain.to_vec();
    for was in &existing.rows {
        if moves.iter().any(|(from, _)| from == &was.label) {
            continue;
        }
        let Some(now) = next.rows.iter().find(|row| same_account(was, row)) else {
            continue;
        };
        if now.label != was.label && !moves.iter().any(|(_, to)| to == &now.label) {
            moves.push((was.label.clone(), now.label.clone()));
        }
    }
    moves
}

/// The labels of `existing` whose account the source no longer lists: no row of `next` is the
/// same account ([`same_account`]), under any label ([`relabel_moves`] pairs the rest).
fn departed_labels(existing: &Roster, next: &Roster) -> Vec<String> {
    existing
        .rows
        .iter()
        .filter(|was| !next.rows.iter().any(|row| same_account(was, row)))
        .map(|was| was.label.clone())
        .collect()
}

/// Whether `dir` holds an entry, as [`accounts::login_made`] reads a login, except that a
/// directory that cannot be read is an error rather than one that holds none: only an absent
/// directory holds none.
fn holds_entry(dir: &Path) -> io::Result<bool> {
    match trace::fs::read_dir(dir) {
        Ok(entries) => Ok(!entries.is_empty()),
        Err(why) if why.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(why) => Err(why),
    }
}

/// The bindings `logins` holds once every move of `moves` ([`default_relocation`]) is made: each
/// follows its account from the label it leaves to the one it takes, as the sync's transaction
/// moves them ([`accounts::move_accounts`]).
fn bindings_after(logins: &accounts::Logins, moves: &[(String, String)]) -> accounts::Logins {
    let mut after = logins.clone();
    let follow: Vec<_> = moves
        .iter()
        .map(|(from, to)| (to.clone(), after.labels.remove(from)))
        .collect();
    for (to, binding) in follow {
        if let Some(binding) = binding {
            after.labels.insert(to, binding);
        }
    }
    after
}

/// Whose login a login directory holds for a row, as the profile in it says: the provider
/// identity a launch under that directory reads ([`super::profile::identity_in_home`]), which
/// [`super::accounts_pool::check_login_identity`] holds the row's launch to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Held {
    /// The directory is absent or empty.
    Nothing,
    /// It holds a login whose profile names the row's account.
    Own,
    /// It holds an entry, and its profile names another account or none that can be read.
    Other,
}

/// What `dir` holds for `row` ([`Held`]).
/// # Errors
/// `dir` cannot be read (other than being absent), as [`holds_entry`], or the profile a launch
/// under it reads is there but cannot be read ([`super::profile::unreadable_profile_in_home`]):
/// either way whose login it holds is unknown.
fn held_for(dir: &Path, row: &Row) -> io::Result<Held> {
    if !holds_entry(dir)? {
        return Ok(Held::Nothing);
    }
    if let Some(profile) = super::profile::unreadable_profile_in_home(dir) {
        return Err(io::Error::other(format!(
            "its profile {} cannot be read",
            profile.display()
        )));
    }
    let own = row.expected_identity_email().is_some_and(|email| {
        super::profile::identity_in_home(dir, email).email_matches == Some(true)
    });
    Ok(if own { Held::Own } else { Held::Other })
}

/// The login directory each row [`default_relocation`] moves after the default login has in the
/// roster the sync publishes, by its new label, or `None` for a row [`chosen_dirs`] places. A
/// sync moves no login, and roster directories need only be distinct, so a row keeps its own
/// directory where that is empty or holds its own account's login ([`held_for`]); one whose own
/// directory holds another account's login, or entries whose profile names no account, keeps
/// none and is placed by whose login each directory holds, as a row given one by its label
/// alone is. A row whose own directory holds nothing and whose renewal-bound login (`logins`,
/// the bindings before the moves) holds nothing either takes its new label's own login directory
/// (`logins/<label>`), as a new row would, where that directory is absent or empty or holds its
/// own account's login and no other row of `next` and no binding filed under another label once
/// every move is made (`after`, [`bindings_after`]) names it; otherwise it keeps its own too. So
/// a moved row never takes the directory `next` gave its new label by that label alone where it
/// holds another account's login, and no login is moved, deleted or handed to another row.
///
/// A directory a row keeps stays its own across syncs, not only within this one: the roster
/// admits no two rows with one directory, a row arrives only at a directory that holds nothing
/// or its own account's login, and every other row is placed where its label's previous row was
/// another account's or named none ([`rows_to_choose`]), taking only a directory that holds
/// nothing or its own account's login and that no row of another account in the previous roster
/// names ([`chosen_dirs`]).
/// # Errors
/// A moved row's directory, its renewal-bound login, or its new label's directory where nothing
/// else names that, cannot be read ([`held_for`]), so whose login it holds is unknown: the message
/// names the labels, the directory, and what a person does.
fn relocated_dirs(
    store: &accounts::Store,
    existing: &Roster,
    next: &Roster,
    (logins, after): (&accounts::Logins, &accounts::Logins),
    moves: &[(String, String)],
) -> Result<Vec<(String, Option<String>)>, String> {
    let Some(((first_from, first_to), rows)) = moves.split_first() else {
        return Ok(Vec::new());
    };
    let unreadable = |from: &str, to: &str, dir: &str, why: io::Error| {
        format!(
            "cannot relabel `{first_from}` to `{first_to}`: the source also moves `{from}`'s \
             account to `{to}`, and the login directory {dir} cannot be read ({why}), so whose \
             login it holds is unknown; make that directory readable, or keep these accounts' \
             current labels in the source, and run the sync again; no new sync was published"
        )
    };
    let mut placed = next.clone();
    let mut empty = Vec::new();
    for (from, to) in rows {
        let Some(row) = next.row(to) else {
            continue;
        };
        let own = existing.row(from).and_then(|row| row.dir.clone());
        let held = match own.as_deref() {
            Some(dir) => {
                held_for(Path::new(dir), row).map_err(|why| unreadable(from, to, dir, why))?
            }
            None => Held::Nothing,
        };
        let bound = match logins.get(from).and_then(|binding| binding.dir.as_deref()) {
            Some(dir) => {
                holds_entry(Path::new(dir)).map_err(|why| unreadable(from, to, dir, why))?
            }
            None => false,
        };
        if let Some(row) = placed.rows.iter_mut().find(|row| row.label == *to) {
            row.dir = own.filter(|_| held != Held::Other);
        }
        if held == Held::Nothing && !bound {
            empty.push((from.as_str(), to.as_str(), row));
        }
    }
    // A row that takes its new label's directory can free the one another wants (a cycle). A
    // directory a row can take is read only once nothing else names it; one that cannot be read
    // then refuses, as it may hold the row's own login, which a row left on a new directory would
    // never find again.
    loop {
        let mut took = false;
        for (from, to, row) in &empty {
            let Some(wanted) = store
                .login_dir(to)
                .and_then(|dir| dir.to_str().map(str::to_owned))
            else {
                continue;
            };
            let taken = placed
                .row(to)
                .is_none_or(|row| row.dir.as_deref() == Some(wanted.as_str()))
                || !matches!(
                    accounts::retained_directory_owner(after, &placed.rows, to, &wanted),
                    Ok(None)
                );
            if taken {
                continue;
            }
            let held = held_for(Path::new(&wanted), row)
                .map_err(|why| unreadable(from, to, &wanted, why))?;
            if let Some(row) = placed.rows.iter_mut().find(|row| row.label == *to)
                && held != Held::Other
            {
                row.dir = Some(wanted);
                took = true;
            }
        }
        if !took {
            break;
        }
    }
    Ok(rows
        .iter()
        .map(|(_, to)| (to.clone(), placed.row(to).and_then(|row| row.dir.clone())))
        .collect())
}

/// The labels of the rows of `next` whose login directory [`chosen_dirs`] chooses: each row with
/// a directory that [`build_machine_roster`] gave by its label alone (`logins/<label>`), because
/// its label's row in `existing` names none (a row new to this Mac, or the default login's
/// previous row), or that it gave as the directory of its label's row in `existing` where that
/// row was another account's ([`label_dir_kept`]: a label the source gave another account,
/// whose directory holds that account's login), other than a row [`default_relocation`] moves
/// after the default login, which [`relocated_dirs`] places.
fn rows_to_choose(existing: &Roster, next: &Roster, moves: &[(String, String)]) -> Vec<String> {
    next.rows
        .iter()
        .filter(|row| {
            row.dir.is_some()
                && existing
                    .row(&row.label)
                    .and_then(|was| label_dir_kept(was, row))
                    .is_none()
                && !moves.iter().skip(1).any(|(_, to)| *to == row.label)
        })
        .map(|row| row.label.clone())
        .collect()
}

/// The login directory each row of `labels` ([`rows_to_choose`], and a moved row
/// [`relocated_dirs`] gave none) has in the roster the sync publishes, chosen by whose login each
/// directory holds ([`held_for`]). A relabel leaves a login where it is, so a label's own
/// directory can be the one a row of another label keeps, or hold the login of an account whose
/// row named it before. Each row takes the first of these that is free for it: the directory its
/// account's row in `existing` names ([`same_account`], under whatever label and login address:
/// the roster gives a provider identity one row), its label's own (`logins/<label>`), then
/// `logins/<label>~<n>` (`n` from 1: `~` is in no label, so no label's directory, and no
/// renewal's `<label>+<n>` staging name, is one of these). A directory is free for a row where no
/// row of `existing` that was another account's, no other row of `next` and no binding filed
/// under another label once every move is made (`after`, [`bindings_after`]: its `dir` or any of
/// its `was`) names it, nor any whose ownership cannot be established, and it is absent or empty
/// or its profile names that row's account; one that holds entries whose profile names another
/// account or none is not. So no row is given another account's login, or a directory another
/// row names, and an account that leaves and comes back (or whose roster was lost) finds its own
/// login again when that login is in `logins/<label>` or a `logins/<label>~<n>`.
/// # Errors
/// A directory that nothing else names cannot be read ([`held_for`]), so whose login it holds is
/// unknown and the row's own could be in it: the message names the label and the directory, and
/// asks for it to be made readable. Or no name up to `<label>~9999` is free (each holds another
/// account's login or is named), so the row has no directory to take; the message names the
/// label and the directory.
fn chosen_dirs(
    store: &accounts::Store,
    existing: &Roster,
    next: &mut Roster,
    after: &accounts::Logins,
    labels: &[String],
) -> Result<(), String> {
    for row in next
        .rows
        .iter_mut()
        .filter(|row| labels.contains(&row.label))
    {
        row.dir = None;
    }
    let root = store.logins_dir();
    for label in labels {
        let Some(row) = next.row(label).cloned() else {
            continue;
        };
        let named = |next: &Roster, dir: &str| {
            !matches!(
                accounts::naming_row(
                    existing.rows.iter().filter(|was| !same_account(was, &row)),
                    dir
                ),
                Ok(None)
            ) || !matches!(
                accounts::retained_directory_owner(after, &next.rows, label, dir),
                Ok(None)
            )
        };
        let own = existing
            .rows
            .iter()
            .find(|was| same_account(was, &row))
            .and_then(|was| was.dir.clone());
        let mut chosen = None;
        for dir in own.into_iter().chain(
            std::iter::once(label.clone())
                .chain((1..=9999).map(|n| format!("{label}~{n}")))
                .filter_map(|name| root.join(name).to_str().map(str::to_owned)),
        ) {
            if named(next, &dir) {
                continue;
            }
            match held_for(Path::new(&dir), &row) {
                Ok(Held::Nothing | Held::Own) => {
                    chosen = Some(dir);
                    break;
                }
                Ok(Held::Other) => {}
                Err(why) => {
                    return Err(format!(
                        "cannot give `{label}` a login directory: {dir} cannot be read ({why}), \
                         so whose login it holds is unknown, and it may be this account's own; \
                         make that directory readable and run the sync again; no new sync was \
                         published"
                    ));
                }
            }
        }
        let dir = chosen.ok_or_else(|| {
            format!(
                "cannot give `{label}` a login directory: {} and every name beside it up to \
                 `{label}~9999` holds another account's login or is named by another row or \
                 binding; no new sync was published",
                root.join(label).display()
            )
        })?;
        if let Some(row) = next.rows.iter_mut().find(|row| row.label == *label) {
            row.dir = Some(dir);
        }
    }
    Ok(())
}

/// The roster a sync writes for `machine` from `fleet` over the roster `existing` it replaces:
/// the fleet with this Mac's conflicting rows carried, built ([`build_machine_roster`]), the pool's
/// holders applied, then, under the lab (`fleet.alab`), every row this Mac has kept and ordered by
/// this Mac's preference alone (`alab_roster::retain` with `lab`, the lab the sync holds), and the
/// locally retained shared rows held to what they were. A sync and the `FleetMachineAssignment`
/// conformance test run this one composition.
///
/// # Errors
/// A locally retained shared row would change or has no room ([`retain_shared_rows`]).
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "FleetMachineAssignment",
        action = "Sync",
        project = "aterm_agent::harness::alab_roster::assignment_tests::order_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "FleetMachineAssignment",
        action = "SyncLabels",
        project = "aterm_agent::harness::alab_roster::assignment_tests::labels_transition"
    )
)]
pub(super) fn compose_roster_with_logins(
    fleet: &FleetRoster,
    (machine, default_email): (&str, Option<&str>),
    (store, existing): (&accounts::Store, &Roster),
    pool_ctx: Option<&PoolSyncCtx>,
    lab: Option<&super::alab_roster::Lab>,
    logins: &accounts::Logins,
) -> Result<Roster, String> {
    let carried = carry_conflicting(fleet, machine, existing);
    let mut roster = build_machine_roster(&carried, machine, store, Some(existing), default_email);
    if let Some(ctx) = pool_ctx {
        roster = apply_pool_to_claude_roster(roster, existing, store, ctx);
    }
    // The lab's preferences order a pool and never limit it: every row this Mac has stays.
    if let Some(bound) = &fleet.alab {
        roster = super::alab_roster::retain(roster, existing, lab, Cli::Claude, &bound.key);
    }
    if fleet.keeps_rows {
        roster = keep_discovered_rows(roster, existing);
    }
    retain_shared_rows(&mut roster, existing, logins, fleet.alab.is_some())?;
    Ok(roster)
}

/// `roster`, built from a fleet discovery made, with every row of `existing` kept as a person
/// listed it: discovery found these rows' addresses in this Mac's own files, which says nothing of
/// the order, hold, browser profile or vendor keys a row was given. A row that names no address,
/// which a fleet cannot list, stays too, and so does a row a person renamed: the account a discovery
/// still lists under the old label is that row. The rows keep their sequence in the file, and a new
/// one follows them. A row's login directory is the build's: where the default login moved, it
/// follows the account ([`default_relocation`]).
fn keep_discovered_rows(mut roster: Roster, existing: &Roster) -> Roster {
    // Each row a person renamed, and where the build puts its account's login where that is the
    // default login's own place or the row left it: any other directory stays the row's.
    let mut followed: Vec<(String, Option<String>)> = Vec::new();
    roster.rows.retain(|row| {
        let renamed = existing
            .rows
            .iter()
            .find(|prev| prev.label != row.label && same_account(prev, row));
        if let Some(prev) = renamed
            && (row.dir.is_none() || prev.dir.is_none())
        {
            followed.push((prev.label.clone(), row.dir.clone()));
        }
        renamed.is_none()
    });
    for prev in &existing.rows {
        match roster.rows.iter_mut().find(|row| row.label == prev.label) {
            Some(row) if same_account(prev, row) => {
                row.class = prev.class;
                row.browser.clone_from(&prev.browser);
                row.held_by.clone_from(&prev.held_by);
                row.vendor = prev.vendor;
                row.was.clone_from(&prev.was);
            }
            Some(_) => {}
            None => {
                let mut kept = prev.clone();
                if let Some((_, dir)) = followed.iter().find(|(label, _)| *label == prev.label) {
                    kept.dir.clone_from(dir);
                }
                roster.rows.push(kept);
            }
        }
    }
    roster.rows.sort_by_key(|row| {
        existing
            .rows
            .iter()
            .position(|prev| prev.label == row.label)
            .unwrap_or(existing.rows.len())
    });
    roster
}

/// Records each row of `roster` whose own login directory's profile names another account than
/// the row expects as a wrong-account login, as a renewal that signed in the wrong account does
/// ([`accounts::note_wrong_account`]: the report's `last renewal: wrong account`, `accounts::wrong_accounts`
/// and a `wrong-account` line in the ledger), once for each finding. The row stays on its
/// directory and a launch stays refused ([`super::accounts_pool::check_login_identity`]) until a
/// sign-in names the row's account.
fn note_wrong_account_logins(
    roster: &Roster,
    logins: &mut accounts::Logins,
    now: i64,
) -> Vec<accounts::Event> {
    let mut events = Vec::new();
    for row in &roster.rows {
        let Some((dir, expected)) = row.dir.as_deref().zip(row.expected_identity_email()) else {
            continue;
        };
        let wrong =
            super::profile::identity_in_home(Path::new(dir), expected).email_matches == Some(false);
        if wrong && logins.get(&row.label).is_none_or(|b| b.wrong_at.is_none()) {
            events.extend(accounts::note_wrong_account(logins, &row.label, now));
        }
    }
    events
}

/// `label`, or where `taken` holds it, the first of `label-2`, `label-3`, … that `taken` does not
/// hold, with the end of `label` cut where its `-N` would carry it past [`accounts::LABEL_MAX`]:
/// the label an admissible one moves to is admissible too, however long the first one was.
fn untaken_label(label: &str, taken: impl Fn(&str) -> bool) -> String {
    let mut next = label.to_owned();
    let mut suffix = 2u32;
    while taken(&next) {
        let tail = format!("-{suffix}");
        let stem = super::truncate_bytes(label, accounts::LABEL_MAX.saturating_sub(tail.len()));
        next = format!("{stem}{tail}");
        suffix = suffix.saturating_add(1);
    }
    next
}

#[cfg(test)]
#[path = "accounts_sync_untaken_label_tests.rs"]
mod untaken_label_tests;

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
        if already_in {
            continue;
        }
        let label = untaken_label(&prev.label, |l| roster.rows.iter().any(|r| r.label == l));
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

fn retain_shared_rows(
    next: &mut Roster,
    existing: &Roster,
    logins: &accounts::Logins,
    class_derived: bool,
) -> Result<(), String> {
    for row in &existing.rows {
        if logins.shared.required(row)?.is_none() {
            continue;
        }
        if let Some(proposed) = next.rows.iter().find(|proposed| {
            proposed.label == row.label
                || row
                    .expected_identity_email()
                    .is_some_and(|email| proposed.matches_email(email))
        }) {
            let mut expected = row.clone();
            // Warm-set ownership is refreshed by the pool independently of
            // the retained provider configuration.
            expected.held_by.clone_from(&proposed.held_by);
            // Under the lab a row's class is derived from the preferences and only orders the
            // roster (`alab_roster::retain`), so it is no part of the retained configuration.
            if class_derived {
                expected.class = proposed.class;
            }
            if proposed != &expected {
                return Err("source would change a locally retained shared account".into());
            }
        } else {
            next.rows.push(row.clone());
        }
    }
    Ok(())
}

fn apply_pool_execution(
    state: &mut accounts::State,
    rows: &[Row],
    cli: Cli,
    pool: &PoolSyncCtx,
) -> bool {
    let before = state.prepared_accounts.len();
    for row in rows {
        if pool_executes(row, cli, pool) {
            state.prepared_accounts.remove(&row.label);
        }
    }
    before != state.prepared_accounts.len()
}

fn pool_executes<
    P: std::borrow::Borrow<super::ai_pool::Pool>,
    I: std::borrow::Borrow<super::ai_pool::LocalPoolIdentity>,
>(
    row: &Row,
    cli: Cli,
    pool: &PoolSyncCtx<P, I>,
) -> bool {
    row.expected_identity_email().is_some() && pool.holder_for_row(cli, row).is_none()
}

fn pool_execution_pending<
    P: std::borrow::Borrow<super::ai_pool::Pool>,
    I: std::borrow::Borrow<super::ai_pool::LocalPoolIdentity>,
>(
    state: &accounts::State,
    rows: &[Row],
    cli: Cli,
    pool: &PoolSyncCtx<P, I>,
) -> bool {
    rows.iter()
        .any(|row| state.prepared_accounts.contains(&row.label) && pool_executes(row, cli, pool))
}

#[cfg(test)]
#[path = "accounts_shared_retention_tests.rs"]
mod shared_retention_tests;

#[cfg(test)]
#[path = "accounts_directory_cleanup_tests.rs"]
mod directory_cleanup_tests;

#[cfg(test)]
thread_local! {
    static DIRECTORY_CLEANUP_MUTANT: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
    static SOURCE_WRITES_MUTANT: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn source_write_negative_control(home: Option<&Path>, fleet: &str) {
    match SOURCE_WRITES_MUTANT.get() {
        1 => {
            let path = home.unwrap().join(".secrets").join(FLEET_ROSTER_FILE);
            if !path.exists() {
                fs::write(path, fleet).unwrap();
            }
        }
        2 => {
            let path = home.unwrap().join(".secrets").join(FLEET_ROSTER_FILE);
            fs::write(path, fleet).unwrap();
        }
        _ => {}
    }
}

fn cleanup_vacated_login_dirs(dirs: Vec<PathBuf>) {
    #[cfg(test)]
    if DIRECTORY_CLEANUP_MUTANT.get() == 2 {
        return;
    }
    for dir in dirs {
        // A login arriving since selection makes the directory nonempty and
        // preserves it. Cleanup is never necessary for receipt recovery.
        let _ = fs::remove_dir(dir);
    }
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
        (store, &SourceProtection::default()),
        roster_path,
        (fleet, opt_in, None),
        (machine, default_email),
        standing_walls,
        now,
        (ladder, fleet_snapshots, None),
    )
    .map_err(SyncError::into_message)
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountIdentityCompletion",
        action = "Complete",
        project = "aterm_agent::harness::accounts_sync::alab_tests::native_identity_tests::completion::completion_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountIdentityCompletion",
        action = "Repeat",
        project = "aterm_agent::harness::accounts_sync::alab_tests::native_identity_tests::completion::completion_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountSyncCleanup",
        action = "Cut",
        reason = "The environment chooses a durable replay interruption."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSyncCleanup",
        action = "Sync",
        project = "aterm_agent::harness::accounts_sync::directory_cleanup_tests::transition"
    )
)]
fn persist_accounts_sync_with_pool(
    (store, protection): (&accounts::Store, &SourceProtection),
    roster_path: &Path,
    (fleet, opt_in, pool_ctx): (&FleetRoster, OptIn, Option<&PoolSyncCtx>),
    (machine, default_email): (&str, Option<&str>),
    standing_walls: &BTreeMap<String, upgrade::StandingWall>,
    now: i64,
    (ladder, fleet_snapshots, admission): (&[Duration], &[FleetSnapshot], Option<&AutomaticSource>),
) -> Result<SyncedAccounts, SyncError> {
    let held = store
        .acquire_within(ladder)
        .map_err(|why| format!("{}: {why}", store.dir.display()))?
        .ok_or_else(|| {
            SyncError::retry(
                Retry::Busy,
                format!(
                    "{} at {} after {} ms: another `aterm harness accounts` or a window's \
                 supervisor holds it; nothing was synced, so run the sync again",
                    accounts::LOCK_HELD,
                    store.dir.display(),
                    ladder.iter().sum::<Duration>().as_millis()
                ),
            )
        })?;
    let recovered = store.recover_sync_held(&held, roster_path)?;
    if let Some(admission) = admission {
        admission.validate()?;
    }
    let receipt_source = if fleet.alab.is_some() {
        Some(
            admission
                .ok_or("native account source was not captured")?
                .native_guard()?,
        )
    } else {
        None
    };
    let (write_to, keep_mode) = super::cli::roster_target(roster_path)?;
    let fleet_path = roster_path
        .parent()
        .map(|p| p.join(FLEET_ROSTER_FILE))
        .unwrap_or_else(|| PathBuf::from(FLEET_ROSTER_FILE));
    protection.check(&[&write_to])?;
    if fleet.alab.is_none() {
        protection.check(&[&fleet_path])?;
    }
    validate_fleet_snapshots(protection.home.as_deref(), fleet_snapshots)?;
    let original_existing = Roster::load(&write_to).map_err(|why| {
        format!(
            "the roster at {} is refused: {why}; the roster is unchanged",
            roster_path.display()
        )
    })?;
    let lab = fleet.lab_snapshot.as_deref();
    let mut projected_existing = original_existing.clone();
    let (retained_state, _) = store.load(now)?;
    let (logins, _) = store.load_logins(now)?;
    // Under the lab every row keeps its label (no sync renames one), and the final rows are
    // derived from the same lab as the roster this sync compares them with.
    let native_fleet = if let Some(lab) = lab {
        for row in &mut projected_existing.rows {
            // Retention compares provider identities. Complete a missing alias expectation
            // from the same unique native account before that comparison; the original
            // roster still fences whether its old authentication applies to this identity.
            if row.identity_email.is_none() {
                row.identity_email = lab
                    .account_of(Cli::Claude, row)
                    .and_then(|account| account.identity.clone());
            }
        }
        super::alab_roster::fleet_for(lab, Cli::Claude, Some(&projected_existing))
    } else {
        None
    };
    let fleet = native_fleet.as_ref().unwrap_or(fleet);
    let mut roster = compose_roster_with_logins(
        fleet,
        (machine, default_email),
        (store, &projected_existing),
        pool_ctx,
        lab,
        &logins,
    )?;
    // A pinned shared row must be compared to its original configuration: an identity
    // completed in memory grants no permission to rewrite that shared authority.
    retain_shared_rows(&mut roster, &original_existing, &logins, lab.is_some())?;
    let existing = projected_existing;
    // Under the lab a default login neither the lab nor a row of this Mac names gets no row
    // (`alab_default_account`): nothing is lost, since the roster never listed it.
    let unnamed_here = |expected: &str| {
        fleet.alab.is_some()
            && !fleet.accounts.iter().chain(&fleet.known).any(|a| {
                accounts::email_matches(&a.email, expected)
                    || native_fleet_identity(fleet, a)
                        .is_some_and(|identity| accounts::email_matches(identity, expected))
            })
            && !existing.rows.iter().any(|row| row.matches_email(expected))
    };
    // Under the lab a dirless row that names no address (the first `add`'s `main`) is kept as
    // the default login's, as it was (`build_machine_roster`): it still lists it.
    let keeps_rows = fleet.alab.is_some() || fleet.keeps_rows;
    let listed_bare = keeps_rows
        && roster
            .row_for_login(None)
            .is_some_and(|row| row.expected_identity_email().is_none());
    if let Some(expected) = default_identity_hint(Some(&existing), default_email)
        && !unnamed_here(expected)
        && !listed_bare
        && !roster
            .row_for_login(None)
            .and_then(Row::expected_identity_email)
            .is_some_and(|actual| accounts::email_matches(actual, expected))
    {
        return Err("the generated roster cannot represent the known default login; retain that account or free a row; no new sync was published".into());
    }
    // A lab that prefers nothing here and knows no default login leaves an empty roster as it is.
    if roster.rows.is_empty() && fleet.alab.is_some() {
        let retired = if admission.is_some_and(AutomaticSource::retires_legacy) {
            store.retire_source_admitted_held(&held, now, receipt_source)?
        } else {
            false
        };
        return Ok(SyncedAccounts {
            fleet_path,
            note: Some("the lab prefers no account of this Mac's yet".to_owned()),
            changed: recovered || retired,
            rows_changed: RowChanges::default(),
            clashes: fleet.clashes.clone(),
        });
    }
    if roster.rows.is_empty() {
        return Err(with_conflicts(
            format!(
                "no active Claude accounts found for machine `{machine}` in {}",
                fleet.source
            ),
            &fleet.conflicts_for(Some(machine)),
        )
        .into());
    }
    // A default login's state follows its account to its new label, whether or not the old
    // label is reused, and a row whose label it takes over moves to the label the source gives
    // that row's account. Only an unchanged login address permits this metadata relocation.
    // Every other account whose label changed moves the same way (`relabel_moves`): all the
    // pairs move records and bindings, but only the default login's chain is placed by
    // `relocated_dirs`; a row another pair moves is placed as a row given a directory by its
    // label alone is, its account's own directory first. An absent account can return by
    // matching its retained provider key and profile; its verified directory follows it.
    let chain = default_relocation(&existing, &roster)?;
    let mut moves = relabel_moves(&existing, &roster, &chain);
    let retained = retained_state;
    let returns = returned::find(store, &existing, &roster, &retained, &logins, &moves)?;
    moves.extend(
        returns
            .iter()
            .map(|found| (found.from.clone(), found.to.clone())),
    );
    let mut departed = departed_labels(&existing, &roster);
    if keeps_rows {
        // A row `alab_roster::retain` kept that names no address is known by its label alone:
        // it has not departed. A label another account now holds has.
        departed.retain(|label| {
            !existing.row(label).is_some_and(|was| {
                was.expected_identity_email().is_none() && roster.row(label).is_some()
            })
        });
    }
    // A row's login directory goes by whose login it holds, as its profile says. Each row moved
    // after the default login keeps its own where that holds nothing or its own account's login,
    // or takes its new label's where that is free; every other row given a directory by its label
    // alone (new here, the default login's previous row, a label the source gave another account,
    // or a moved row whose directory holds another's login) takes its account's directory, its
    // label's or a new one, whichever first holds nothing or its own login and nothing else names.
    // It reads the bindings under the lock this sync holds through publication, as the
    // transaction below reads the ones it edits, whose directory check still refuses a row given
    // a directory another binding retains.
    // Only a row new here or repointed needs directory discovery (`rows_to_choose`): a label
    // its account still holds keeps that row's directory (`label_dir_kept`), so an opaque but
    // retained login is never traded for a different, empty one.
    let mut choose = rows_to_choose(&existing, &roster, &chain);
    for found in &returns {
        choose.retain(|label| label != &found.to);
        if let Some(row) = roster.rows.iter_mut().find(|row| row.label == found.to) {
            row.dir = Some(found.dir.clone());
        }
    }
    if moves.len() > 1 || !choose.is_empty() || !returns.is_empty() {
        let mut after = bindings_after(&logins, &moves);
        for found in &returns {
            found.restore(&mut after, &found.to);
        }
        for (label, dir) in relocated_dirs(store, &existing, &roster, (&logins, &after), &chain)? {
            if dir.is_none() {
                choose.push(label.clone());
            }
            if let Some(row) = roster.rows.iter_mut().find(|row| row.label == label) {
                row.dir = dir;
            }
        }
        chosen_dirs(store, &existing, &mut roster, &after, &choose)?;
    }
    let roster_toml = format_roster_toml(&roster)
        .map_err(|why| format!("generated roster refused admission: {why}"))?;
    let (fleet_write_to, _) = super::cli::roster_target(&fleet_path)?;
    // The fleet projection must not replace the account authority whose before images
    // the local commit is about to retain, even through an existing path alias.
    let destination = |path: &Path| -> Result<PathBuf, String> {
        accounts::usage_target(Some(path))?.ok_or_else(|| "sync destination is missing".into())
    };
    let authority_dir = destination(&store.dir)?;
    let authority_roster = destination(&write_to)?;
    let target = destination(&fleet_write_to)?;
    if target.starts_with(&authority_dir) || target == authority_roster {
        return Err(format!(
            "sync fleet projection {} overlaps the account store or roster; no new sync was published",
            fleet_write_to.display()
        ).into());
    }
    let fleet_toml = fleet.to_toml_for(Some(opt_in));
    let default_row_label = roster.row_for_login(None).map(|r| r.label.clone());
    // The labels the fleet gave another account than the one the roster gave them (another
    // provider identity: a login address changed for the same identity is the same account,
    // `same_account`): what is on record under each is the account it held before, never the
    // one it holds now, unless a relocation moves the account it holds now there.
    let relabelled: Vec<String> = roster
        .rows
        .iter()
        .filter(|row| {
            existing.row(&row.label).is_some_and(|was| {
                match (was.email.as_deref(), row.email.as_deref()) {
                    (Some(_), Some(_)) => !same_account(was, row),
                    (None, Some(_)) => true,
                    _ => false,
                }
            })
        })
        .map(|row| row.label.clone())
        .collect();
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
    // Completing an alias's canonical identity changes its authentication context.
    // A matching observation of the same default login can establish that new context;
    // old authentication alone cannot. Vendor-policy changes are not this completion.
    let completed_default_identity = original_existing
        .row_for_login(None)
        .zip(roster.row_for_login(None))
        .is_some_and(|(before, after)| {
            before.identity_email.is_none()
                && after.identity_email.is_some()
                && before.vendor == after.vendor
                && before
                    .email
                    .as_deref()
                    .zip(after.email.as_deref())
                    .is_some_and(|(before, after)| accounts::email_matches(before, after))
                && before
                    .expected_identity_email()
                    .zip(after.expected_identity_email())
                    .is_some_and(|(before, after)| !accounts::email_matches(before, after))
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
    // A sync that finds what is already there writes nothing: each file is replaced only where
    // its bytes differ, so a periodic caller rewrites no file and wakes no watcher.
    let roster_held = accounts::roster_text(&write_to)?;
    let fleet_held = super::ai_pool::read_bounded_bytes(&fleet_write_to, 4 * 1024 * 1024);
    let fleet_before = fleet_held
        .as_deref()
        .map(std::str::from_utf8)
        .transpose()
        .map_err(|why| format!("the local fleet is not UTF-8: {why}"))?;
    let changed = recovered
        || roster.rows.iter().any(|row| {
            row.dir.as_deref().map(Path::new).is_some_and(|path| {
                path.parent() == Some(store.logins_dir().as_path()) && !trace::fs::exists(path)
            })
        })
        || (admission.is_some_and(AutomaticSource::retires_legacy)
            && !store.source_policy()?.retired())
        || roster_held.as_deref() != Some(roster_toml.as_str())
        || (fleet.alab.is_none() && fleet_held.as_deref() != Some(fleet_toml.as_bytes()));
    let mut state_changed = false;
    // The login directories under the store that moved rows leave and nothing names any longer.
    let left_dirs = std::cell::Cell::new(Vec::<PathBuf>::new());
    let events = store
        .transact_sync_source_held(&held, now, &mut note, (accounts::SyncTarget {
            fleet: fleet.alab.is_none().then_some(accounts::FleetTarget {
                roster: roster_path, path: &target, before: fleet_before, after: &fleet_toml,
            }),
            path: &write_to,
            before: roster_held.as_deref(),
            after: Some(&roster_toml),
            mode: keep_mode,
        }, receipt_source), (admission.is_some_and(AutomaticSource::retires_legacy), |state, logins| {
            if let Some(admission) = admission {
                admission.validate()?;
            }
            validate_fleet_snapshots(protection.home.as_deref(), fleet_snapshots)?;
            let before = (state.clone(), logins.clone());
            for found in &returns {
                found.recheck(state, &roster)?;
                found.restore(logins, &found.from);
            }
            if let Some(ctx) = pool_ctx {
                apply_pool_execution(state, &roster.rows, Cli::Claude, ctx);
            }
            // What a departed account left at a move's destination is forgotten first, binding
            // and all, so the move is not refused for retaining it and no row is served its
            // login; the directory itself stays where it is, unnamed. A checked source that
            // leaves in the same move set must survive until the simultaneous move takes it.
            for (_, to) in &moves {
                if departed.contains(to) && !moves.iter().any(|(from, _)| from == to) {
                    accounts::forget_account(state, logins, to);
                    logins.labels.remove(to);
                }
            }
            let pairs: Vec<(&str, &str)> =
                moves.iter().map(|(from, to)| (from.as_str(), to.as_str())).collect();
            accounts::move_accounts(state, logins, &pairs)?;
            for label in &relabelled {
                if !moves.iter().any(|(_, to)| to == label) {
                    accounts::forget_account(state, logins, label);
                }
            }
            // A departed account's binding at any other label keeps its directory only in
            // `was`: no row is given it, and it is no longer served to the label's next
            // account (`accounts::bound`). Its record stands, for its return.
            for label in &departed {
                if moves.iter().any(|(_, to)| to == label) {
                    continue;
                }
                if let Some(binding) = logins.labels.get_mut(label)
                    && let Some(dir) = binding.dir.take()
                {
                    // Keep the most recent old logins within the store's bound, and put the
                    // last current login where a returning account finds it, even if it was
                    // already in history. Only metadata retires; no login file is removed.
                    binding.was.retain(|was| was.as_deref() != Some(dir.as_str()));
                    binding.was.truncate(accounts::MAX_LEFT - 1);
                    binding.was.push(Some(dir));
                }
            }
            let logins_root = store.logins_dir();
            left_dirs.set(
                moves
                    .iter()
                    .skip(1)
                    .filter_map(|(from, _)| existing.row(from)?.dir.clone())
                    .filter(|dir| {
                        Path::new(dir).parent() == Some(logins_root.as_path())
                            && !roster
                                .rows
                                .iter()
                                .any(|row| row.dir.as_deref() == Some(dir.as_str()))
                            && !logins.labels.values().any(|binding| {
                                binding.dir.as_deref() == Some(dir.as_str())
                                    || binding.was.iter().flatten().any(|was| was == dir)
                            })
                    })
                    .map(PathBuf::from)
                    .collect(),
            );
            for row in &roster.rows {
                if let Some(dir) = row.dir.as_deref()
                    && let Some(owner) = accounts::retained_directory_owner(logins, &roster.rows, &row.label, dir)?
                {
                    return Err(format!("cannot use `{}`: its login directory is retained by `{owner}`; choose an unused label; nothing was written", row.label).into());
                }
            }
            let mut events = Vec::new();
            if let Some(default_row) = default_row_label
                .as_deref()
                .and_then(|label| roster.row(label))
            {
                let label = &default_row.label;
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
                    .filter(|a| a.current_auth(default_row) == accounts::Auth::SignedOut)
                    .and_then(|a| a.auth_at)
                    .into_iter()
                    .chain(accounts::login_ended_at(default_row, logins, state))
                    .any(|at| at >= now);
                // A row bound to a renewal directory of its own launches from that directory:
                // the vendor's sign-in made a second login of its account on the default login
                // and left the row's own as it was, so what is on record of it stands, while the
                // sessions that ran on the default login (and `active`, where it named the row
                // the default login held) are its account's now.
                let bound_elsewhere = logins.get(label).is_some_and(|b| b.dir.is_some());
                // Identity completion is not a new sign-in. Read the pre-completion
                // context under its relocated label so an old sign-out or applicable
                // end cannot be hidden by the new canonical identity.
                let completed_default_ready = completed_default_identity
                    && original_existing.row_for_login(None).is_some_and(|before| {
                        let mut before = before.clone();
                        before.label.clone_from(label);
                        state.accounts.get(label).is_some_and(|account| {
                            account.current_auth(&before) == accounts::Auth::Ok
                        }) && accounts::login_ended_at(&before, logins, state).is_none()
                    });
                let made_now = default_matched
                    && repointed_from.is_some()
                    && !ended_since
                    && !bound_elsewhere;
                let was = repointed_from
                    .as_deref()
                    .filter(|was| logins.get(was).is_none_or(|b| b.dir.is_none()));
                if made_now {
                    events.extend(accounts::note_default_login_made_observed(
                        state, logins, default_row, was, now,
                    ));
                } else if default_matched
                    && !bound_elsewhere
                    && (unseen_default || (completed_default_ready && !ended_since))
                {
                    accounts::note_served_login_for_observed_row(logins, default_row, now);
                    events.extend(accounts::note_auth_for_observed_row(
                        state, logins, default_row, accounts::Auth::Ok, now,
                    ));
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
                if let Some(dir) = dir
                    && !accounts::login_made(Path::new(&dir))
                    && state
                        .accounts
                        .get(&row.label)
                        .is_some_and(|a| {
                            a.auth == accounts::Auth::Ok
                                || (a.auth != accounts::Auth::Unseen
                                    && a.current_auth(row) == accounts::Auth::Unseen)
                                || displaced.as_deref() == Some(row.label.as_str())
                        })
                {
                    let mut observed_row = row.clone();
                    observed_row.dir = Some(dir);
                    events.extend(accounts::note_auth_for_observed_row(
                        state,
                        logins,
                        &observed_row,
                        accounts::Auth::SignedOut,
                        now,
                    ));
                }
            }
            if opt_in == OptIn::Discovered {
                events.extend(note_wrong_account_logins(&roster, logins, now));
            }
            for (wall_label, wall) in standing_walls {
                let wall_label = moves.iter()
                    .find(|(from, _)| from == wall_label)
                    .map_or(wall_label.as_str(), |(_, to)| to.as_str());
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
            #[cfg(test)]
            {
                if DIRECTORY_CLEANUP_MUTANT.get() == 1 {
                    cleanup_vacated_login_dirs(left_dirs.take());
                }
                source_write_negative_control(protection.home.as_deref(), &fleet_toml);
            }
            Ok(events)
        }))
        .map_err(|why| why.context(|why| format!("account sync publication did not complete: {why}")))?;
    // Only the completed receipt makes these directories unreferenced. An
    // interrupted replay keeps them for the old roster and for recovery.
    cleanup_vacated_login_dirs(left_dirs.take());
    let changed = changed || state_changed || !events.is_empty();
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
        rows_changed: RowChanges::between(&existing.rows, &roster.rows),
        clashes: fleet.clashes.clone(),
    })
}

/// Run `aterm harness accounts sync [alab | <source>] [--machine <mN>] [--json]`. `host` is the
/// machine its receipt judges each row's route on before it promises a sign-in by itself
/// ([`super::cli::claude_rows_no_route`]); with none it promises none.
pub fn run_accounts_sync(
    env: &Env,
    host: Option<&dyn super::cli::AccountsHost>,
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
    run_accounts_sync_with(
        (env, host),
        (source, machine_override, json),
        (out, err),
        &how,
    )
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

/// A named native account is an explicit selection; automatic source-key
/// precedence does not apply, but caller policy and retained state still do.
pub(super) fn capture_native_named_account(
    env: &Env,
    cli: Cli,
    name: &str,
) -> Result<Option<(Row, super::alab_roster::ReceiptGuard)>, String> {
    let store = env
        .accounts_store_for(cli)
        .ok_or_else(|| super::cli::no_home_for(cli))?;
    let config = env
        .config
        .clone()
        .or_else(|| {
            env.accounts_roster_for(cli)
                .map(|path| config_beside(&path))
        })
        .ok_or("no account configuration is known")?;
    let captured = AutomaticSource::capture_requested(&config)
        .map_err(SyncError::into_message)?
        .with_policy(store.source_policy()?, false);
    captured
        .named_account(cli, name)?
        .map(|row| {
            Ok((
                row,
                captured
                    .native_guard()
                    .map_err(SyncError::into_message)?
                    .clone(),
            ))
        })
        .transpose()
}

fn run_accounts_sync_with(
    (env, host): (&Env, Option<&dyn super::cli::AccountsHost>),
    (source, machine_override, json): (Option<&str>, Option<&str>, bool),
    (out, err): (&mut dyn Write, &mut dyn Write),
    how: &SyncHow<'_>,
) -> ExitCode {
    let mut report = match sync_accounts_with(env, source, machine_override, how) {
        Ok(report) => report,
        Err(why) => {
            let _ = writeln!(err, "aterm harness accounts sync: {why}");
            return ExitCode::FAILURE;
        }
    };
    report.no_route = host.and_then(|host| {
        super::cli::claude_rows_no_route(env, host, &report.records, &report.enrolled)
    });
    write_sync_report(&report, env.now, json, out, err)
}

/// The source this sync admitted and whether its publication or recovery wrote data.
struct SyncPublication {
    machine: String,
    machine_label: Option<String>,
    roster_path: PathBuf,
    fleet_path: PathBuf,
    fleet_accounts: usize,
    changed: bool,
    /// The sheet's conflicts this machine's pool skips ([`conflict_names`]).
    conflicts: Vec<SheetConflict>,
    /// Whether the source was the lab's: `fleet_accounts` then counts what the lab prefers here,
    /// and no fleet file was written ([`persist_accounts_sync_with_pool`]).
    lab: bool,
    /// What this sync did to rows it kept ([`RowChanges`]).
    rows_changed: RowChanges,
    /// Each account the lab prefers here left out for a label clash ([`FleetRoster::clashes`]).
    clashes: Vec<(String, String)>,
    /// Why the lab was not read, where it was not ([`Chosen::Pending`]): no source was read, and
    /// the report's one status line says so ([`sync_claude_pending`]).
    pending: Option<String>,
}

/// One observation after sync and sign-in recovery. The source metadata describes
/// this sync; the account records describe one completed local publication. Login
/// file presence is observed once afterward, outside the local commit protocol.
struct SyncReport {
    publication: SyncPublication,
    records: accounts::AccountRecords,
    enrolled: BTreeSet<String>,
    needs_renew: Vec<String>,
    /// The rows with no login here whose account may be another Mac's ([`rows_of_other_macs`]):
    /// `needs_renew` leaves out those the source gives other Macs alone, and no door admits any.
    other_macs: accounts::OtherMacs,
    /// The rows with no login here that no route signs in with nobody at this Mac, judged on the
    /// machine the sync ran for ([`super::cli::claude_rows_no_route`]); `None` where none was
    /// named, so the receipt promises no sign-in by itself.
    no_route: Option<BTreeSet<String>>,
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
                        .is_none_or(|account| {
                            account.current_auth(row) != accounts::Auth::SignedOut
                        })
            })
            .count();
        InitSynced {
            machine: self
                .publication
                .machine_label
                .unwrap_or(self.publication.machine),
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
) -> Result<SyncReport, SyncError> {
    let source = source.map(str::trim).filter(|source| !source.is_empty());
    if source == Some(super::alab_roster::SOURCE) {
        let env = source_env(env)?;
        let roster = env
            .accounts_roster()
            .ok_or("no account roster location is known")?;
        let store = env.accounts_store();
        let recovered = store.recover_sync_within(how.ladder, &roster)?;
        let config = env.config.clone().unwrap_or_else(|| config_beside(&roster));
        let admission =
            AutomaticSource::capture_requested(&config)?.with_policy(store.source_policy()?, false);
        let mut report = sync_accounts_request(
            &env,
            SourceRequest::RequestedNative(&admission),
            machine_override,
            how,
        )?;
        report.publication.changed |= recovered;
        return Ok(report);
    }
    if source.is_some() {
        return sync_accounts_request(env, SourceRequest::Requested(source), machine_override, how);
    }
    let env = source_env(env)?;
    let roster = env
        .accounts_roster()
        .ok_or("no account roster location is known")?;
    let store = env.accounts_store();
    let recovered = store.recover_sync_within(how.ladder, &roster)?;
    let config = env.config.clone().unwrap_or_else(|| config_beside(&roster));
    let admission = AutomaticSource::capture_requested(&config)?;
    let native = match (SourceRequest::Automatic {
        candidate: None,
        admission: &admission,
    })
    .chosen(Cli::Claude, &roster, env.config.as_deref())?
    {
        Chosen::Lab(_) => true,
        Chosen::Unnamed => false,
        // A lab not read yet selects no other source: the roster stands as it is.
        Chosen::Pending(why) => {
            let mut report = sync_claude_pending(&env, &store, (&roster, &why))?;
            report.publication.changed |= recovered;
            return Ok(report);
        }
    };
    let policy = store.source_policy()?;
    let selected = if native {
        SelectedSource::Absent
    } else {
        source_selection::select_observed(
            env.home.as_deref(),
            env.config.as_deref(),
            Some(&roster),
            SourceScope::Configured,
            &policy,
        )?
    };
    let retire = native || selected.retires_legacy(env.config.as_deref());
    let admission = admission.with_policy(policy, retire);
    let admission = if native {
        admission
    } else {
        admission.with_selection(&env, &roster, SourceScope::Configured, selected)
    };
    let mut report = sync_accounts_request(
        &env,
        SourceRequest::Automatic {
            candidate: None,
            admission: &admission,
        },
        machine_override,
        how,
    )?;
    report.publication.changed |= recovered;
    Ok(report)
}

fn sync_accounts_request(
    env: &Env,
    request: SourceRequest<'_>,
    machine_override: Option<&str>,
    how: &SyncHow<'_>,
) -> Result<SyncReport, SyncError> {
    let normalized = source_env(env)?;
    let env = &normalized;
    let roster_path = env
        .accounts_roster()
        .ok_or("no config directory is known (set HOME or XDG_CONFIG_HOME)")?;
    let store = env.accounts_store();
    let recovered_commit = store.recover_sync_within(how.ladder, &roster_path)?;
    request.validate()?;
    let native = match request.chosen(Cli::Claude, &roster_path, env.config.as_deref())? {
        Chosen::Lab(fleet) => Some(fleet),
        Chosen::Unnamed => None,
        Chosen::Pending(why) => {
            let mut report = sync_claude_pending(env, &store, (&roster_path, &why))?;
            report.publication.changed |= recovered_commit;
            return Ok(report);
        }
    };
    let reads = std::sync::Mutex::new(Vec::new());
    let capture = |context: &Env, source: &str| {
        let text = (how.fetch)(context, source)?;
        let at = match fetch_plan(context, source)? {
            FetchPlan::File(path) => Some(SnapshotAt::File(path)),
            FetchPlan::Entry(name) => Some(SnapshotAt::Entry(name)),
            FetchPlan::Http(_) => None,
        };
        if let Some(at) = at {
            reads
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push((at, text.clone()));
        }
        Ok(text)
    };
    let (mut fleet, opt_in, pool_ctx) =
        load_selected_fleet_source(env, request.candidate(), machine_override, &capture, native)?;
    let mut protection = SourceProtection {
        home: env.home.clone(),
        read_only: Vec::new(),
    };
    let mut snapshots = how.fleet_snapshots.to_vec();
    let local_fleet = fleet_file_beside(&roster_path);
    for (at, text) in reads
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
    {
        // A vault entry is read by name: no output can alias it.
        if let SnapshotAt::File(path) = &at {
            let generated_here = snapshots
                .iter()
                .any(|saved| saved.at == at && saved.bytes.is_none());
            let mutable_profile = request
                .admission()
                .is_some_and(|admission| admission.owns_local_projection(path));
            if local_fleet.as_ref() != Some(path) || !(generated_here || mutable_profile) {
                protection.read_only.push(path.clone());
            }
        }
        // Init may already retain the original bytes while fetching its merged projection.
        if request.admission().is_some() && !snapshots.iter().any(|saved| saved.at == at) {
            snapshots.push(FleetSnapshot {
                at,
                bytes: Some(text.into_bytes()),
            });
        }
    }
    fleet.keeps_rows = opt_in == OptIn::Discovered;
    let default_email = read_default_profile_email(env);
    let machine = if let Some(ctx) = &pool_ctx {
        ctx.identity.primary.clone()
    } else if let Some(me) = &fleet.alab {
        // The machine this Mac's facts row binds: the lab's identity, never a host name.
        me.key.as_str().to_owned()
    } else {
        machine_for(env.config.as_deref(), machine_override)
            .ok_or("could not determine machine id (`m<N>`); pass `--machine m8`")?
    };
    let fleet_accounts = pool_ctx.as_ref().map_or(fleet.accounts.len(), |ctx| {
        ctx.pool.accounts_for_cli(Cli::Claude).len()
    });
    if let Some(ctx) = &pool_ctx
        && how.allow_empty_pool
        && fleet.accounts_for_machine(&machine).is_empty()
        && default_email.is_none()
        && !trace::fs::exists(&roster_path)
    {
        request.validate()?;
        let held = store
            .acquire_within(how.ladder)
            .map_err(|why| why.to_string())?
            .ok_or_else(|| SyncError::retry(Retry::Busy, accounts::LOCK_HELD))?;
        store.recover_sync_held(&held, &roster_path)?;
        request.validate()?;
        validate_fleet_snapshots(env.home.as_deref(), &snapshots)?;
        let fleet_path = roster_path
            .parent()
            .map(|p| p.join(FLEET_ROSTER_FILE))
            .unwrap_or_else(|| PathBuf::from(FLEET_ROSTER_FILE));
        protection.check(&[&roster_path, &fleet_path])?;
        let fleet_toml = fleet.to_toml_for(Some(opt_in));
        let changed = store
            .transact_fleet_state_policy_held(
                &held,
                env.now,
                &mut None,
                (&roster_path, &fleet_toml),
                (
                    request
                        .admission()
                        .is_some_and(AutomaticSource::retires_legacy),
                    |state| {
                        request.validate()?;
                        validate_fleet_snapshots(env.home.as_deref(), &snapshots)?;
                        if accounts::roster_text(&roster_path)?.is_some() {
                            return Err(SyncError::retry(
                                Retry::Changed,
                                "account roster appeared after empty-pool planning; sync again",
                            ));
                        }
                        let target_label = Some(ctx.identity.primary.clone());
                        if state.swarm_label != target_label
                            || state.swarm_aliases != ctx.identity.aliases
                            || !state.home_accounts.is_empty()
                        {
                            state.swarm_label = target_label;
                            state.swarm_aliases.clone_from(&ctx.identity.aliases);
                            state.home_accounts.clear();
                        }
                        Ok(Vec::new())
                    },
                ),
            )
            .map_err(|why| why.context(|why| format!("could not update account state: {why}")))?;
        drop(held);
        return read_sync_report(
            env,
            &store,
            SyncPublication {
                machine,
                machine_label: None,
                roster_path,
                fleet_path,
                fleet_accounts,
                changed: changed || recovered_commit,
                // A pool's fleet reads no sheet, so it skips no sheet conflict.
                conflicts: Vec::new(),
                lab: false,
                rows_changed: RowChanges::default(),
                clashes: Vec::new(),
                pending: None,
            },
            None,
        )
        .map_err(Into::into);
    }

    // The walls a transcript shows go to the rows the sync publishes, the rows a conflict
    // carries among them ([`carry_conflicting`]); the roster is read again under the lock.
    let listed = super::cli::roster_target(&roster_path)
        .ok()
        .and_then(|(write_to, _)| Roster::load(&write_to).ok());
    let prelim_fleet = listed.as_ref().map_or_else(
        || fleet.clone(),
        |listed| carry_conflicting(&fleet, &machine, listed),
    );
    let prelim_roster = build_machine_roster(
        &prelim_fleet,
        &machine,
        &store,
        None,
        default_email.as_deref(),
    );
    let standing_walls = env
        .claude_dir()
        .map(|dir| {
            let spans = || login_dir_spans_live(&store, how.ladder, env.now);
            find_transcript_walls_by_email(&dir, &prelim_roster, spans, env.now)
        })
        .unwrap_or_default();
    let SyncedAccounts {
        fleet_path,
        note,
        changed,
        rows_changed,
        clashes,
    } = persist_accounts_sync_with_pool(
        (&store, &protection),
        &roster_path,
        (&fleet, opt_in, pool_ctx.as_ref()),
        (&machine, default_email.as_deref()),
        &standing_walls,
        env.now,
        (how.ladder, &snapshots, request.admission()),
    )?;
    let recovered = recover_interrupted_staging_renewals_for(
        Cli::Claude,
        &store,
        &roster_path,
        &AUTO_RENEW_GUARD,
        env.now,
        shared_renewal::CheckpointAuthority::live(env.home.as_deref(), &roster_path),
    );
    let conflicts = fleet.conflicts_for(Some(&machine));
    let healed = how.heal.zip(env.home.as_deref()).and_then(|(check, home)| {
        heal_default_login_now(home, &store, &roster_path, env.now, check)
    });
    let mut report = read_sync_report(
        env,
        &store,
        SyncPublication {
            machine,
            machine_label: fleet
                .alab
                .as_ref()
                .and_then(|machine| machine.label.clone()),
            roster_path,
            fleet_path,
            fleet_accounts,
            changed: changed
                || recovered_commit
                || recovered
                || healed.as_ref().is_some_and(|(healed, _)| *healed),
            conflicts,
            lab: fleet.alab.is_some(),
            rows_changed,
            clashes,
            pending: None,
        },
        note,
    )?;
    if let Some((_, line)) = healed {
        report.records.notes.push(line);
    }
    Ok(report)
}

/// Claude Code's sync while the lab is not read yet (`why`, [`Chosen::Pending`]): it reads no
/// source (no sheet, fleet file or pool among them) and writes nothing. Every row stands byte for
/// byte: none is
/// removed, relabelled or signed out, no login directory moves, and no row is added, the default
/// login's account included: once the lab is read, its own sync lists that login under the lab's
/// rules, so an account the lab bars (paused, retired) stays out ([`alab_default_account`]), and
/// keeps the label of every row it finds ([`super::alab_roster::fleet_for`]). The report's one
/// status line says why the lab is not read.
///
/// # Errors
/// The roster that stands does not parse, as it fails a sync with a source, or its target or the
/// report's records cannot be read.
fn sync_claude_pending(
    env: &Env,
    store: &accounts::Store,
    (roster_path, why): (&Path, &str),
) -> Result<SyncReport, SyncError> {
    let (write_to, _) = super::cli::roster_target(roster_path)?;
    if let Some(text) = accounts::roster_text(&write_to)? {
        Roster::parse_for(Cli::Claude, &text).map_err(|refused| {
            format!(
                "the roster at {} is refused: {refused}; the roster is unchanged",
                roster_path.display()
            )
        })?;
    }
    read_sync_report(
        env,
        store,
        SyncPublication {
            machine: machine_for(env.config.as_deref(), None).unwrap_or_default(),
            machine_label: None,
            roster_path: roster_path.to_owned(),
            fleet_path: fleet_file_beside(roster_path).unwrap_or_default(),
            fleet_accounts: 0,
            changed: false,
            conflicts: Vec::new(),
            lab: false,
            rows_changed: RowChanges::default(),
            clashes: Vec::new(),
            pending: Some(why.to_owned()),
        },
        None,
    )
    .map_err(Into::into)
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
    env: &Env,
    store: &accounts::Store,
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
        .recover_account_records(Some(&publication.roster_path), env.now)
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
    let other_macs = rows_of_other_macs(Cli::Claude, env, &records.raw_roster, &records.logins);
    let needs_renew: Vec<String> = roster
        .rows
        .iter()
        .filter(|r| {
            r.held_by.is_none()
                && r.dir.is_some()
                && !enrolled.contains(&r.label)
                && !other_macs.named.contains_key(&r.label)
                && !state
                    .accounts
                    .get(&r.label)
                    .is_some_and(|a| a.limited_until(env.now).is_some())
        })
        .map(|r| r.label.clone())
        .collect();
    Ok(SyncReport {
        publication,
        records,
        enrolled,
        needs_renew,
        other_macs,
        no_route: None,
    })
}

/// The `accounts-sync` JSON's schema, `accounts sync`'s and `accounts sync --cli <pool>`'s: 2
/// names each row by its account's key, slug and name (`ac`, `slug`, `name`) where 1 named it by
/// its roster label, which only a row with no address keeps; `needs_renew`, `added`, `held`,
/// `kept` and `own_row` are objects of those, and a label clash names the account left out and
/// the row here that holds its label (`held_by`).
const SYNC_SCHEMA: u64 = 2;

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
                machine_label,
                roster_path,
                fleet_path,
                fleet_accounts,
                changed,
                conflicts,
                lab,
                rows_changed,
                clashes,
                pending,
            },
        records,
        enrolled,
        needs_renew,
        other_macs,
        no_route,
    } = report;
    let roster = &records.roster;
    let state = &records.state;
    let names = Naming::of(Cli::Claude, &roster.rows);
    for note in &records.notes {
        let _ = writeln!(err, "aterm harness accounts: {note}");
    }
    if let Some(words) = sheet_conflicts_words(conflicts) {
        let _ = writeln!(err, "aterm harness accounts sync: {words}");
    }
    if json {
        let mut o = Map::new();
        o.insert("schema".to_owned(), Value::from(SYNC_SCHEMA));
        o.insert("kind".to_owned(), Value::from("accounts-sync".to_owned()));
        o.insert("machine".to_owned(), Value::from(machine.clone()));
        o.insert(
            "machine_label".to_owned(),
            machine_label
                .as_deref()
                .map_or(Value::Null, |label| Value::from(label.to_owned())),
        );
        o.insert(
            "roster".to_owned(),
            Value::from(roster_path.display().to_string()),
        );
        o.insert(
            "fleet_roster".to_owned(),
            if *lab || pending.is_some() {
                Value::Null
            } else {
                Value::from(fleet_path.display().to_string())
            },
        );
        if let Some(why) = pending {
            o.insert("lab_pending".to_owned(), Value::from(why.clone()));
        }
        o.insert(
            "fleet_accounts".to_owned(),
            Value::from(u64::try_from(*fleet_accounts).unwrap_or(0)),
        );
        o.insert("changed".to_owned(), Value::from(*changed));
        rows_changed.put_json(&names, &mut o);
        o.insert("label_clashes".to_owned(), names.clashes_json(clashes));
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
                        super::cli::put_account_names(&mut m, &names.rows, &r.label);
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
        o.insert("needs_renew".to_owned(), names.json_all(needs_renew));
        o.insert("sheet_conflicts".to_owned(), conflicts_json(conflicts));
        let _ = writeln!(
            out,
            "{}",
            aterm_json::to_string(&Value::Object(o)).unwrap_or_default()
        );
    } else {
        if let Some(why) = pending {
            let _ = writeln!(
                out,
                "synced this Mac's own roster ({} accounts on this machine{}): {why}",
                roster.rows.len(),
                if *changed { "" } else { "; no account changes" }
            );
        } else {
            let _ = writeln!(
                out,
                "synced machine `{}` ({} accounts on this machine, {} {}){}",
                machine_label.as_deref().unwrap_or(machine),
                roster.rows.len(),
                fleet_accounts,
                if *lab {
                    "the lab prefers here"
                } else {
                    "across fleet"
                },
                if *changed { "" } else { "; no account changes" }
            );
        }
        let _ = writeln!(out, "roster: {}", roster_path.display());
        // Neither the lab's source nor a lab not read yet writes a fleet file beside the roster.
        if !*lab && pending.is_none() {
            let _ = writeln!(out, "fleet:  {}", fleet_path.display());
        }
        rows_changed.write_lines(&names, out);
        names.write_clash_lines(clashes, out);
        // Each row by its account's name and slug, in a column as wide as the widest of them.
        let width = roster
            .rows
            .iter()
            .map(|row| names.shown(&row.label).chars().count())
            .max()
            .unwrap_or(0)
            .max(8);
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
            } + &accounts::model_walls_clause(state.accounts.get(&row.label), now);
            let where_word = if row.dir.is_none() {
                "default login (adopted)"
            } else if here {
                "isolated login (enrolled)"
            } else {
                "isolated login (not signed in yet)"
            };
            let _ = writeln!(
                out,
                "  {:width$}  {:9}  {:24}  {}",
                names.shown(&row.label),
                row.class.as_str(),
                cond,
                where_word
            );
        }
        // A row in shared custody takes the root's step, which the receipt names, before any
        // page's stop: no sign-in here can make its login.
        let custody = |label: &str| {
            roster.row(label).is_some_and(|row| {
                super::cli::shared_custody_words(
                    Cli::Claude,
                    row,
                    (&records.raw_roster.rows, None),
                    &records.logins,
                )
                .is_some()
            })
        };
        let (stopped, waiting): (Vec<&str>, Vec<&str>) =
            needs_renew.iter().map(String::as_str).partition(|label| {
                !custody(label)
                    && records
                        .raw_roster
                        .row(label)
                        .is_some_and(|raw| accounts::current_sign_in_blocked(raw, &records.logins))
            });
        if !stopped.is_empty() {
            let _ = writeln!(
                out,
                "not signed in yet: {}; {}",
                names.shown_all(&stopped),
                super::cli::stopped_step(
                    Cli::Claude,
                    stopped
                        .iter()
                        .map(|label| super::account_names::sign_in_word_of(
                            Cli::Claude,
                            label,
                            &roster.rows,
                            super::alab_roster::current().as_deref(),
                        ))
                        .collect()
                )
            );
        }
        let (addressed, bare): (Vec<&str>, Vec<&str>) =
            waiting.iter().copied().partition(|label| {
                custody(label) || roster.row(label).is_some_and(|row| row.email.is_some())
            });
        let off = sign_ins_off_for(&config_beside(roster_path), Cli::Claude);
        write_waiting_receipt(
            Cli::Claude,
            &addressed,
            (records, other_macs),
            (off.as_deref(), no_route.as_ref()),
            now,
            out,
        );
        if !bare.is_empty() {
            let _ = writeln!(
                out,
                "not signed in yet: {}; no email names them, so a person signs each in with `{}`",
                names.shown_all(&bare),
                Step::sign_in(Cli::Claude, Word::chosen(), false)
            );
        }
    }
    ExitCode::SUCCESS
}

/// The receipt's lines for `labels`, rows with an email and no login here that a person need not
/// open first, each as [`not_next_phrase`](super::cli)'s report says it, in one line per reason:
/// a row in shared custody names the root's step; a row neither door of the upkeep admits
/// ([`super::sign_in_ladder::upkeep_signs_in`], read from `records` with `others`, the rows whose
/// account may be another Mac's) waits for `accounts renew`, with the condition it fails
/// ([`super::sign_in_ladder::upkeep_waits_because`]); of the rest, where `off` names the switch
/// that keeps aterm's unattended sign-ins off here it says so, a row no route signs in with
/// nobody here (`no_route`) names `accounts browser`, and only a row with a route is told aterm
/// signs it in by itself. Where `no_route` is `None` nothing here judged the routes, and the line
/// promises nothing. Each row is named by its account's name and slug under the lab this process
/// holds ([`super::account_names::Listing`]), and each step types its account's word.
fn write_waiting_receipt(
    cli: Cli,
    labels: &[&str],
    (records, others): (&accounts::AccountRecords, &accounts::OtherMacs),
    (off, no_route): (Option<&str>, Option<&BTreeSet<String>>),
    now: i64,
    out: &mut dyn Write,
) {
    #[derive(PartialEq, Eq)]
    enum Says {
        Custody(String),
        Waits(&'static str),
        Off,
        NoRoute,
        Unjudged,
        ByItself,
    }
    let lab = super::alab_roster::current();
    let says = |label: &str| {
        let Some(row) = records.roster.row(label) else {
            return Says::Waits("it is not in the roster");
        };
        if let Some(custody) = super::cli::shared_custody_words(
            cli,
            row,
            (&records.raw_roster.rows, lab.as_deref()),
            &records.logins,
        ) {
            Says::Custody(custody)
        } else if !super::sign_in_ladder::upkeep_signs_in(
            row,
            (&records.logins, &records.state, others),
            now,
        ) {
            Says::Waits(super::sign_in_ladder::upkeep_waits_because(
                row,
                &records.state,
                others,
                now,
            ))
        } else if off.is_some() {
            Says::Off
        } else {
            match no_route.map(|rows| rows.contains(label)) {
                None => Says::Unjudged,
                Some(true) => Says::NoRoute,
                Some(false) => Says::ByItself,
            }
        }
    };
    let mut lines: Vec<(Says, Vec<&str>)> = Vec::new();
    for &label in labels {
        let said = says(label);
        match lines.iter_mut().find(|(line, _)| *line == said) {
            Some((_, rows)) => rows.push(label),
            None => lines.push((said, vec![label])),
        }
    }
    let names = super::account_names::Listing::of(cli, &records.roster.rows, lab.as_deref());
    for (said, rows) in lines {
        let words = rows
            .iter()
            .map(|label| {
                super::account_names::sign_in_word_of(
                    cli,
                    label,
                    &records.roster.rows,
                    lab.as_deref(),
                )
            })
            .collect();
        let listed = rows
            .iter()
            .map(|label| names.shown(label))
            .collect::<Vec<_>>()
            .join(", ");
        let renew = Step::Renew(cli, words);
        let _ = match said {
            Says::Custody(custody) => writeln!(out, "not signed in yet: {listed}; {custody}"),
            Says::Waits(because) => writeln!(
                out,
                "not signed in yet: {listed}; each waits for `{renew}`, since {because}"
            ),
            Says::Off => writeln!(
                out,
                "not signed in yet: {listed}; {} keeps aterm's unattended sign-ins off here \
                 (`{renew}` runs one now)",
                off.unwrap_or_default()
            ),
            Says::NoRoute => writeln!(
                out,
                "not signed in yet: {listed}; no route signs each in with nobody at this Mac, so \
                 `{renew}` needs a terminal here, and `{}` gives one a route",
                Step::Browser(cli, Word::chosen())
            ),
            Says::Unjudged => writeln!(out, "not signed in yet: {listed}; `{renew}` signs each in"),
            Says::ByItself => writeln!(
                out,
                "not signed in yet: {listed}; aterm signs each in by itself, and `{renew}` runs \
                 that now"
            ),
        };
    }
}

/// The local machine for account sync and discovery: an explicit override, local configuration,
/// or this host's name. Account assignments and provider logins supply no machine identity.
/// The host's id is read once per process ([`detected_machine`]).
fn machine_for(config: Option<&Path>, machine_override: Option<&str>) -> Option<String> {
    machine_override
        .and_then(normalize_machine_id)
        .or_else(|| config_machine_id(config))
        .or_else(detected_machine)
}

#[derive(Debug)]
pub(super) struct LocalFleetEdit {
    pub(super) path: PathBuf,
    destination: PathBuf,
    before: String,
    after: String,
}

impl LocalFleetEdit {
    pub(super) fn changed(&self) -> bool {
        self.before != self.after
    }

    pub(super) fn target<'a>(&'a self, roster: &'a Path) -> accounts::FleetTarget<'a> {
        accounts::FleetTarget {
            roster,
            path: &self.destination,
            before: Some(&self.before),
            after: &self.after,
        }
    }
}

/// Plan one owned local fleet image, retaining the exact input even when the row
/// is already listed. The caller publishes this image with its account roster.
/// # Errors
/// `row` has no email (a fleet lists each account by its email), a file cannot be read or is
/// refused, it lists `row`'s label as another account or `row`'s account under another label,
/// or this machine's id is unknown.
pub(super) fn local_fleet_edit(
    env: &Env,
    file: &Path,
    row: &Row,
) -> Result<LocalFleetEdit, String> {
    SourceProtection {
        home: env.home.clone(),
        ..SourceProtection::default()
    }
    .check(&[file])?;
    let email = row.email.as_deref().ok_or_else(|| {
        format!(
            "{} lists each account by its email, and a sync keeps only the accounts it lists: \
             pass `--email <address>`; nothing was written",
            file.display()
        )
    })?;
    let destination = trace::fs::canonicalize(file).map_err(|why| why.to_string())?;
    let text = read_local_source_text(file)
        .map_err(|why| format!("{}: {why}; nothing was written", file.display()))?;
    let mut fleet = FleetRoster::from_toml(&text, &file.display().to_string())
        .map_err(|why| format!("{why}; nothing was written"))?;
    let machine = machine_for(env.config.as_deref(), None).ok_or_else(|| {
        "could not determine this machine's id (`m<N>`) for the fleet row: set `[fleet] \
             machine` in aterm.toml; nothing was written"
            .to_owned()
    })?;
    let same_email = |acct: &FleetAccount| accounts::email_matches(&acct.email, email);
    if let Some(listed) = fleet.accounts.iter().find(|a| a.label == row.label) {
        if same_email(listed) && listed.machines.contains(&machine) {
            return Ok(LocalFleetEdit {
                path: file.to_owned(),
                destination,
                before: text.clone(),
                after: text,
            });
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
    Ok(LocalFleetEdit {
        path: file.to_owned(),
        destination,
        after: fleet.to_toml_for(opt_in_of(&text)),
        before: text,
    })
}

/// When the window's upkeep last claimed a sync's ten-minute cooldown ([`SyncCooldownClaim`]),
/// watched, so a claim or its release on any thread ends a trace that read it.
static AUTO_SYNC_COOLDOWN: Watched<Option<Instant>> =
    Watched::new("accounts_sync/sync_cooldown", None);

/// How often upkeep rediscovers local accounts and revalidates configured local sources whose
/// metadata is unchanged ([`init_sync_needed`], [`key_file_changed`]).
const REDISCOVER_EVERY: Duration = Duration::from_secs(15 * 60);

/// How long a window's upkeep waits before it syncs again for the same default login the roster
/// still does not give its default row ([`default_login_unlisted`]).
const UNLISTED_RETRY: Duration = Duration::from_secs(10 * 60);

/// How long a claim of the missing roster's sync holds the next one back ([`SyncCooldownClaim`]).
const SYNC_COOLDOWN: Duration = Duration::from_secs(10 * 60);

/// What a window's upkeep or the setup pass's resync last did to keep the roster up with this
/// machine's own logins and sources ([`sync_called_for`]), in this process only. Each is kept
/// [`Watched`] ([`REDISCOVERY`], [`RESYNC_READ`]): every field moves the generation.
#[derive(Debug, Default, Hash)]
struct Rediscovery {
    /// When it last read this machine's logins and transcripts for new accounts.
    read_at: Option<Instant>,
    /// The default login's email it last synced for because the roster's default row was another
    /// account, and when.
    synced_for: Option<(String, Instant)>,
    /// The local source observation last scheduled for an attempt, whether it succeeded or
    /// failed. Even an unchanged observation is retried after `REDISCOVER_EVERY`.
    key_attempt: Option<LocalSourceAttempt>,
    /// What the last sync that failed read ([`sync_inputs`]) and when it ran ([`periodic_sync`]).
    failed: Option<FailedSync>,
    /// The sheet's conflicts the upkeep last logged ([`log_sheet_conflicts`]).
    sheet_said: Vec<SheetConflict>,
}

enum SyncScheduleChange {
    Read(Option<Instant>, Option<Instant>),
    Default(Option<(String, Instant)>, Option<(String, Instant)>),
    Key(Option<LocalSourceAttempt>, Option<LocalSourceAttempt>),
}

/// Only observations installed by this operation can be restored on retry or cancellation.
struct SyncScheduleClaim<'a> {
    rediscovery: &'a Watched<Rediscovery>,
    scope: Option<&'a super::account_work::Scope>,
    changes: Vec<SyncScheduleChange>,
    retry: bool,
}

impl<'a> SyncScheduleClaim<'a> {
    fn new(
        rediscovery: &'a Watched<Rediscovery>,
        scope: Option<&'a super::account_work::Scope>,
    ) -> Self {
        Self {
            rediscovery,
            scope,
            changes: Vec::new(),
            retry: false,
        }
    }

    fn outcome<T>(&mut self, result: &Result<T, SyncError>) {
        self.retry |= result
            .as_ref()
            .is_err_and(|why| why.retry_reason().is_some());
    }
}

impl Drop for SyncScheduleClaim<'_> {
    fn drop(&mut self) {
        if !self.retry
            && !self
                .scope
                .is_some_and(super::account_work::Scope::cancelled)
        {
            return;
        }
        let mut held = self
            .rediscovery
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for change in self.changes.drain(..).rev() {
            match change {
                SyncScheduleChange::Read(before, own) if held.read_at == own => {
                    held.read_at = before
                }
                SyncScheduleChange::Default(before, own) if held.synced_for == own => {
                    held.synced_for = before
                }
                SyncScheduleChange::Key(before, own) if held.key_attempt == own => {
                    held.key_attempt = before
                }
                _ => {}
            }
        }
    }
}

struct SyncCooldownClaim<'a> {
    held: &'a Watched<Option<Instant>>,
    scope: Option<&'a super::account_work::Scope>,
    before: Option<Instant>,
    own: Instant,
    retry: bool,
}

impl<'a> SyncCooldownClaim<'a> {
    fn take(
        held: &'a Watched<Option<Instant>>,
        scope: Option<&'a super::account_work::Scope>,
    ) -> Option<Self> {
        let mut guard = held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if guard.is_some_and(|last| {
            !trace::Since::new(last).elapsed_at_least(SYNC_COOLDOWN, Instant::now())
        }) {
            return None;
        }
        let before = *guard;
        let own = Instant::now();
        *guard = Some(own);
        Some(Self {
            held,
            scope,
            before,
            own,
            retry: false,
        })
    }

    fn outcome<T>(&mut self, result: &Result<T, SyncError>) {
        self.retry |= result
            .as_ref()
            .is_err_and(|why| why.retry_reason().is_some());
    }
}

impl Drop for SyncCooldownClaim<'_> {
    fn drop(&mut self) {
        if self.retry
            || self
                .scope
                .is_some_and(super::account_work::Scope::cancelled)
        {
            let mut held = self
                .held
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if *held == Some(self.own) {
                *held = self.before;
            }
        }
    }
}

/// What the window's upkeep last did to keep the roster up ([`Rediscovery`]), watched.
static REDISCOVERY: Watched<Rediscovery> = Watched::new(
    "accounts_sync/rediscovery",
    Rediscovery {
        read_at: None,
        synced_for: None,
        key_attempt: None,
        failed: None,
        sheet_said: Vec::new(),
    },
);

/// A periodic caller's sync that failed ([`periodic_sync`]).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct FailedSync {
    /// What it read ([`sync_inputs`]).
    inputs: [u8; 32],
    /// When it ran.
    at: Instant,
    /// Why, which the next failure is logged against ([`note_sync_result`]).
    why: String,
    /// Whether this rejected input holds another attempt back. Transient source or
    /// custody failures establish no verdict about the captured input.
    holds: bool,
}

impl FailedSync {
    /// Whether it holds back a sync that would read `inputs` now: one of the same inputs, within
    /// [`REDISCOVER_EVERY`] of this attempt, where its failure rejected the input.
    fn holds_back(&self, inputs: &[u8; 32]) -> bool {
        self.holds
            && self.inputs == *inputs
            && !trace::Since::new(self.at).elapsed_at_least(REDISCOVER_EVERY, Instant::now())
    }
}

/// Keeps what a periodic caller's sync of `inputs` did in `failed` ([`periodic_sync`]), and
/// returns whether its failure is news, which a caller logs: one of other inputs or for another
/// reason than the last failure's. A sync that succeeds forgets the last failure.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSyncRetry",
        action = "Finish",
        project = "aterm_agent::harness::accounts_sync::retry_tests::retry_transition"
    )
)]
fn note_sync_result(
    failed: &mut Option<FailedSync>,
    inputs: [u8; 32],
    result: Result<(), &SyncError>,
) -> bool {
    let Err(why) = result else {
        *failed = None;
        return false;
    };
    let news = failed
        .as_ref()
        .is_none_or(|last| last.inputs != inputs || last.why != why.to_string());
    *failed = Some(FailedSync {
        inputs,
        at: Instant::now(),
        why: why.to_string(),
        holds: why.retry_reason().is_none(),
    });
    news
}

/// Whether [`REDISCOVER_EVERY`] has passed since the upkeep last read this machine's logins and
/// transcripts for new accounts; when it has, that read is counted from now.
fn rediscover_due(rediscovery: &Watched<Rediscovery>, claim: &mut SyncScheduleClaim<'_>) -> bool {
    let mut held = rediscovery
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if held
        .read_at
        .is_some_and(|at| !trace::Since::new(at).elapsed_at_least(REDISCOVER_EVERY, Instant::now()))
    {
        return false;
    }
    let before = held.read_at;
    held.read_at = Some(Instant::now());
    claim
        .changes
        .push(SyncScheduleChange::Read(before, held.read_at));
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
    now: impl Into<trace::Now>,
    rediscovery: &Watched<Rediscovery>,
    claim: Option<&mut SyncScheduleClaim<'_>>,
) -> bool {
    let now: trace::Now = now.into();
    if configured_source(Some(config), Some(roster_path)).is_none()
        && matches!(
            lab_chosen(Cli::Claude, roster_path, config),
            Chosen::Unnamed
        )
    {
        return false;
    }
    let Ok(roster) = Roster::load(roster_path) else {
        return false;
    };
    if roster.rows.is_empty() {
        return false;
    }
    let Some(email) = read_default_profile_email(&traced_env(home, state_dir, config, now)) else {
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
        accounts::email_matches(was, &email)
            && !trace::Since::new(*at).elapsed_at_least(UNLISTED_RETRY, Instant::now())
    }) {
        return false;
    }
    let before = held.synced_for.clone();
    held.synced_for = Some((email, Instant::now()));
    if let Some(claim) = claim {
        claim
            .changes
            .push(SyncScheduleChange::Default(before, held.synced_for.clone()));
    }
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
    scope: Option<super::account_work::Scope>,
    fetch: &'a Fetch<'a>,
    spawn_renewal: &'a SpawnRenewal<'a>,
    sync_cooldown: &'a Watched<Option<Instant>>,
    /// When the upkeep last caught its roster up with this machine's own logins
    /// ([`rediscover_due`], [`default_login_unlisted`]).
    rediscovery: &'a Watched<Rediscovery>,
    renew_guard: &'a std::sync::Arc<Watched<AutoRenewGuard>>,
    /// Whether a headless-capable browser is installed for `home` ([`browser_installed`]).
    find_browser: &'a (dyn Fn(&Path) -> bool + Sync),
    /// One bounded `aterm secrets sync` for `home` ([`run_vault_sync`]).
    vault_sync: fn(&Path, Option<&super::account_work::Scope>) -> super::sign_in_ladder::Vault,
    /// Where that sync, and the default login's one check ([`Self::default_check`]), run, off
    /// the caller's thread ([`spawn_vault_thread`]).
    spawn_vault: &'a SpawnRenewal<'a>,
    /// One read-only check of this machine's own login, the default home's
    /// ([`default_login_check_live`]).
    default_check: fn(&Path, Option<&super::account_work::Scope>) -> super::cli::LoginCheck,
    /// What the configured mailbox offers a row's sign-in ([`ladder_mailbox_live`]).
    mailbox: &'a LadderMailbox,
    /// Where automatic sign-ins and source refresh refusals go ([`log_sign_in`]).
    log: SignInLog,
    /// One run of `claude auth login` for a route, where it is not the live one
    /// ([`spawn_row_renewal`]).
    run_attempt: Option<RunAttempt>,
    shared: shared_renewal::Effects<'a>,
    /// Which rows of a roster run on their vault token here, read value-free from the vault
    /// under `home` ([`rows_on_token_live`]).
    tokens_here: TokensRead,
}

type SpawnRenewal<'a> = dyn Fn(Box<dyn FnOnce() + Send>) -> io::Result<()> + Sync + 'a;

/// [`UpkeepEffects::tokens_here`]: the rows of `roster` whose vault token is in use here, each
/// with its token's end, as [`super::accounts_tokens::rows_on_token_read`] returns them for
/// `home`, `store` and `now`; `None` where the vault could not be read just now.
type TokensRead = fn(&Path, &accounts::Store, &Roster, trace::Now) -> Option<BTreeMap<String, u64>>;

/// The live [`UpkeepEffects::tokens_here`]: one value-free read of the vault under `home`, which
/// first settles every split pair whose turn is this Mac's, with no session of its account needed
/// ([`super::accounts_tokens::rows_on_token_settled`]). Where no launch starts on a token
/// ([`super::token_route::LAUNCHES_ON_TOKEN`]: the judgment's platform step), no row runs on its
/// token here whatever the vault holds, so none is read and the reading names none (which removes
/// a kept one). No vault runs off Unix, so none is read.
fn rows_on_token_live(
    home: &Path,
    store: &accounts::Store,
    roster: &Roster,
    now: trace::Now,
) -> Option<BTreeMap<String, u64>> {
    if !super::token_route::LAUNCHES_ON_TOKEN {
        let _ = (home, store, roster, now);
        return Some(BTreeMap::new());
    }
    #[cfg(unix)]
    {
        super::accounts_tokens::rows_on_token_settled(
            &aterm_secrets::client::Paths::for_home(home),
            store,
            roster,
            now,
        )
    }
    #[cfg(not(unix))]
    {
        let _ = (home, store, roster, now);
        None
    }
}

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
/// its stdin empty, at most [`VAULT_SYNC_BUDGET`] (the LAN vault's fetch and its verification,
/// which refreshes the verified view the ladder then reads its mailbox entries from).
fn run_vault_sync(
    home: &Path,
    scope: Option<&super::account_work::Scope>,
) -> super::sign_in_ladder::Vault {
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
    let ran = match scope {
        Some(scope) => {
            if scope.cancelled() {
                return Vault::Failed("account work was cancelled".to_owned());
            }
            super::align::capture_bounded_owned_watching(cmd, VAULT_SYNC_BUDGET, 16 * 1024, |_| {
                scope.cancelled()
            })
            .and_then(|ended| match ended {
                super::align::Ended::Exited(ran) => Ok(ran),
                super::align::Ended::Stopped => Err("account work was cancelled".to_owned()),
            })
        }
        None => super::align::capture_bounded_owned(cmd, VAULT_SYNC_BUDGET, 16 * 1024),
    };
    match ran {
        Ok(ran) => Vault::of_sync(&String::from_utf8_lossy(&ran.stdout), ran.code),
        Err(why) => Vault::of_run_error(&why, VAULT_SYNC_BUDGET),
    }
}

/// The live [`UpkeepEffects::default_check`]: this machine's own default login, answered by its
/// default home's stored login file alone (`~/.claude/.credentials.json`,
/// [`crate::ai::connector::stored_login_held`]). It runs no process and reads no keychain,
/// network or browser, so it contacts no provider and takes no [`super::provider_read::Use`]:
/// local state answers whether an account is signed in here, and a process probe is no local
/// answer (`DESIGN-alab-one-mesh-2026-11-05.md` §4.15 rule 6). The window's upkeep makes this
/// check on a timer, which holds no account in use, so no door could mint a token for a probe
/// here either. A file holding a token is signed in; one holding neither is signed out; no
/// readable file is no answer, which records nothing, so the sign-out on record stands until the
/// login's own use shows otherwise. Where Claude Code keeps the default login in the keychain
/// (macOS, [`crate::ai::connector::claude_default_login_in_file`]) the file is not that login, as
/// [`accounts::Store::default_login_home`] reads it too, so a stale file there is no answer
/// either and never heals a sign-out the keychain-held login may still have.
fn default_login_check_live(home: &Path) -> super::cli::LoginCheck {
    use super::cli::LoginCheck;
    if !crate::ai::connector::claude_default_login_in_file() {
        return LoginCheck::CouldNotRun(DEFAULT_LOGIN_IN_KEYCHAIN.to_owned());
    }
    match crate::ai::connector::stored_login_held(Cli::Claude.connector(), &home.join(".claude")) {
        Some(true) => LoginCheck::SignedIn,
        Some(false) => LoginCheck::SignedOut,
        None => LoginCheck::CouldNotRun(DEFAULT_LOGIN_UNREAD.to_owned()),
    }
}

/// Why [`default_login_check_live`] had no answer.
const DEFAULT_LOGIN_UNREAD: &str =
    "the default home keeps no login file it can read; its keychain is never read";

/// Why [`default_login_check_live`] had no answer where the default login is the keychain's.
const DEFAULT_LOGIN_IN_KEYCHAIN: &str =
    "the default login is in the keychain, which is never read, and its home's file is not it";

/// Lab seam: whether the default login's heal is due in the world at `home`, `state_dir` and
/// `roster_path` ([`default_login_heal_due`]), with the row it is due for.
#[cfg(any(test, debug_assertions))]
#[must_use]
pub fn default_login_heal_due_for_lab(
    home: &Path,
    state_dir: &Path,
    roster_path: &Path,
    now: i64,
) -> Option<String> {
    let roster = Roster::load(roster_path).ok()?;
    let store = accounts::Store::at(state_dir);
    default_login_heal_due(home, &store, &roster, now).map(|heal| heal.label)
}

/// Lab seam: the default login's heal as `accounts sync` makes it ([`heal_default_login_now`]
/// with the live check, [`default_login_check_live`]) on the caller's thread: whether it healed
/// and its log line, where it was due.
#[cfg(any(test, debug_assertions))]
#[must_use]
pub fn heal_default_login_for_lab(
    home: &Path,
    state_dir: &Path,
    roster_path: &Path,
    now: i64,
) -> Option<(bool, String)> {
    let store = accounts::Store::at(state_dir);
    heal_default_login_now(home, &store, roster_path, now, default_login_check_live)
}

/// How many of the ledger's newest events [`default_login_heal_due`] reads for the default
/// login's last move.
const HEAL_LEDGER_TAIL: usize = 2_000;

/// A default row the default login's one check is due for ([`default_login_heal_due`]).
#[derive(Debug, Clone, PartialEq, Eq)]
struct DefaultHeal {
    /// The roster's default row.
    label: String,
    /// The provider identity the status check was requested for.
    expected_identity: String,
    /// When its sign-out on record was written.
    signed_out_at: i64,
    /// The row the default login held before it moved onto `label`, where that row holds no
    /// login bound elsewhere: the sessions on record under it ran on the default login.
    was: Option<String>,
}

/// An unstarted or cancelled check must not suppress the next admitted check.
struct DefaultCheckClaim {
    guard: std::sync::Arc<Watched<AutoRenewGuard>>,
    key: (String, i64),
    completed: bool,
}

impl DefaultCheckClaim {
    fn complete(&mut self) {
        self.completed = true;
    }
}

struct VaultWorkClaim {
    guard: std::sync::Arc<Watched<AutoRenewGuard>>,
    context: super::sign_in_ladder::VaultContext,
    completed: bool,
}

impl VaultWorkClaim {
    fn complete(&mut self) {
        self.completed = true;
    }
}

impl Drop for VaultWorkClaim {
    fn drop(&mut self) {
        if !self.completed {
            self.guard
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .ladder
                .vault_cancelled(self.context);
        }
    }
}

impl Drop for DefaultCheckClaim {
    fn drop(&mut self) {
        if !self.completed {
            let mut guard = self
                .guard
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if guard.default_checked.as_ref() == Some(&self.key) {
                guard.default_checked = None;
            }
        }
    }
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
    now: impl Into<trace::Now>,
) -> Option<DefaultHeal> {
    let now: trace::Now = now.into();
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
    if account.current_auth(row) != accounts::Auth::SignedOut {
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
        expected_identity: row.expected_identity_email()?.to_owned(),
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
    home: &Path,
    roster_path: &Path,
    heal: &DefaultHeal,
    check: &super::cli::LoginCheck,
    now: i64,
) -> bool {
    if *check != super::cli::LoginCheck::SignedIn {
        return false;
    }
    let mut healed = false;
    let mut note = None;
    let committed = store.transact_pool_logins_within(
        &accounts::LOCK_LADDER,
        roster_path,
        now,
        &mut note,
        |raw, state, logins| {
            let current = accounts::bound(raw, logins);
            let standing = current.row(&heal.label).is_some_and(|row| {
                state.accounts.get(&heal.label).is_some_and(|account| {
                    account.current_auth(row) == accounts::Auth::SignedOut
                        && account.auth_at == Some(heal.signed_out_at)
                })
            }) && logins.get(&heal.label).is_none_or(|b| b.dir.is_none());
            if !standing {
                return Ok(Vec::new());
            }
            if !current.row(&heal.label).is_some_and(|row| {
                row.expected_identity_email().is_some_and(|expected| {
                    accounts::email_matches(expected, &heal.expected_identity)
                }) && default_login_identity_matches(Cli::Claude, &home.join(".claude"), row)
            }) {
                return Ok(Vec::new());
            }
            let events = accounts::note_default_login_made(
                state,
                logins,
                &heal.label,
                heal.was.as_deref(),
                now,
            );
            healed = events
                .iter()
                .any(|event| event.kind == accounts::Kind::SignedIn);
            Ok(events)
        },
    );
    committed.is_ok() && healed
}

/// The line of one default-login check, the `aterm.log` line of [`heal_default_login_in_upkeep`]
/// and a note of `accounts sync`'s receipt ([`heal_default_login_now`]), each row as `named`
/// names the row labelled with its label: the default row, and the row the default login held
/// before.
fn default_heal_line(
    heal: &DefaultHeal,
    named: &dyn Fn(&str) -> String,
    check: &super::cli::LoginCheck,
    healed: bool,
) -> String {
    let at = super::usage::rfc3339_utc(heal.signed_out_at);
    if healed {
        format!(
            "account {}: the default login checked signed in, so its sign-out of {at} was an \
             earlier login's; on record as signed in{}",
            named(&heal.label),
            heal.was
                .as_deref()
                .map(|was| format!(", with the sessions on record under {}", named(was)))
                .unwrap_or_default()
        )
    } else {
        format!(
            "account {}: the default login's check after its move answered {}; its sign-out of \
             {at} stands",
            named(&heal.label),
            check.words()
        )
    }
}

/// How the line of a default-login check names `roster`'s row labelled `label` under `lab`: in a
/// sync's receipt (`logged` false) by its account's name and slug, in `aterm.log` by its key and
/// slug (`docs/DESIGN-shared-files-2026-11-11.md` §10); a row with no address by its label marked
/// so, and a label no row holds (the row the default login held before, since removed) marked
/// `(no longer listed)`.
fn heal_row_name(
    roster: &Roster,
    label: &str,
    lab: Option<&super::alab_roster::Lab>,
    logged: bool,
) -> String {
    roster.row(label).map_or_else(
        || format!("{label} (no longer listed)"),
        |row| {
            if logged {
                super::account_names::logged_row(Cli::Claude, row, lab)
            } else {
                super::account_names::shown_row(Cli::Claude, row, lab)
            }
        },
    )
}

/// THE DEFAULT LOGIN'S HEAL in the window's upkeep: where [`default_login_heal_due`] holds and
/// this process has not checked that row's sign-out yet, one read-only check of the default
/// login ([`UpkeepEffects::default_check`]) off the caller's thread, then
/// [`apply_default_heal`] and one `aterm.log` line. Once per row and sign-out on record.
fn heal_default_login_in_upkeep(
    home: &Path,
    state_dir: &Path,
    roster_path: &Path,
    now: impl Into<trace::Now>,
    fx: &UpkeepEffects<'_>,
) {
    let now: trace::Now = now.into();
    if fx
        .scope
        .as_ref()
        .is_some_and(super::account_work::Scope::cancelled)
    {
        return;
    }
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
    // The log names each row as this process's lab names it now, before the check runs apart.
    let lab = super::alab_roster::current();
    let names: BTreeMap<String, String> = std::iter::once(heal.label.as_str())
        .chain(heal.was.as_deref())
        .map(|label| {
            (
                label.to_owned(),
                heal_row_name(&roster, label, lab.as_deref(), true),
            )
        })
        .collect();
    let mut claim = DefaultCheckClaim {
        guard: fx.renew_guard.clone(),
        key: (heal.label.clone(), heal.signed_out_at),
        completed: false,
    };
    let (check, log, home, roster_path) = (
        fx.default_check,
        std::sync::Arc::clone(&fx.log),
        home.to_path_buf(),
        roster_path.to_path_buf(),
    );
    let scope = fx.scope.clone();
    let _ = (fx.spawn_vault)(Box::new(move || {
        if scope
            .as_ref()
            .is_some_and(super::account_work::Scope::cancelled)
        {
            return;
        }
        let found = check(&home, scope.as_ref());
        if scope
            .as_ref()
            .is_some_and(super::account_work::Scope::cancelled)
        {
            return;
        }
        // The heal is written off the pass's thread, stamped with the pass's reading.
        let healed = apply_default_heal(&store, &home, &roster_path, &heal, &found, now.raw());
        claim.complete();
        let named = |label: &str| names.get(label).cloned().unwrap_or_default();
        log(&default_heal_line(&heal, &named, &found, healed));
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
    let healed = apply_default_heal(store, home, roster_path, &heal, &found, now);
    let lab = super::alab_roster::current();
    let named = |label: &str| heal_row_name(&roster, label, lab.as_deref(), false);
    Some((healed, default_heal_line(&heal, &named, &found, healed)))
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
/// one that names only a webmail inbox needs a headless browser, and says whether it holds the
/// row's password; an entry with no password yet is named as such and never tried (the email's
/// own entry, which the lab may be asked for, or a shared one, which it is not); none for the
/// row is none.
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
    } else if cfg.holds_for(super::alab_roster::Part::Password, email) {
        (Mailbox::WebmailOnly, login)
    } else {
        (Mailbox::WebmailNoPassword, login)
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

/// The window's network moved or a peer was heard (docs/DESIGN-fluid-swarm-2026-10-28.md F3c): the
/// automatic sign-in ladder's waits end but that of a row a page stopped
/// ([`super::sign_in_ladder::Ladder::moved`]), so the next upkeep asks the vault and tries each of
/// the others at once. The waits end as of the guard's clock now ([`MachineReads::clock`]).
pub fn network_moved() {
    let mut guard = AUTO_RENEW_GUARD
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let now = (guard.reads.clock)();
    guard.ladder.moved(now);
}

/// THE WINDOW'S FLEET ACCOUNTS' UPKEEP, the one way a window reaches this module: [`upkeep`] at
/// the wall reading `now`.
pub fn upkeep_live(
    home: &Path,
    state_dir: &Path,
    roster_path: &Path,
    now: trace::Now,
    step: UpkeepStep,
) -> Upkept {
    let lifetime = super::account_work::Lifetime::default();
    let scope = lifetime.enter().expect("new account lifetime admits work");
    upkeep_live_scoped(scope, home, state_dir, roster_path, now, step)
}

/// Runs App-owned upkeep under its already admitted lifetime, including detached work, at the
/// wall reading `now`, which every duty of the pass compares through ([`upkeep`]).
pub fn upkeep_live_scoped(
    scope: super::account_work::Scope,
    home: &Path,
    state_dir: &Path,
    roster_path: &Path,
    now: trace::Now,
    step: UpkeepStep,
) -> Upkept {
    if scope.cancelled() {
        return Upkept::Off;
    }
    with_live_scope(scope, |fx| {
        upkeep(home, state_dir, (roster_path, now, step), fx)
    })
}

fn with_live_scope<T>(
    scope: super::account_work::Scope,
    run: impl FnOnce(&UpkeepEffects<'_>) -> T,
) -> T {
    with_scope_effects(scope, live_effects(), run)
}

/// `run` over `fx` under the admitted `scope`: each renewal or vault sync it starts and each
/// fetch it makes is the scope's work, refused once the scope is cancelled. Each is recorded in
/// the trace scope armed on the starting thread (`aterm_uds::trace`): a work started is a
/// [`trace::Effect::Spawn`], a fetch a [`trace::Effect::Fetch`] and a line logged a
/// [`trace::Effect::Log`]; a work that could not start, cancelled or refused, is a retry. A run
/// that did one of those is never kept as a duty's verdict (`accounts_upkeep_memo`).
fn with_scope_effects<T>(
    scope: super::account_work::Scope,
    fx: UpkeepEffects<'_>,
    run: impl FnOnce(&UpkeepEffects<'_>) -> T,
) -> T {
    let renewal_scope = scope.clone();
    let vault_scope = scope.clone();
    let renewal_spawn = fx.spawn_renewal;
    let vault_spawn = fx.spawn_vault;
    let fetch_source = fx.fetch;
    let spawn_renewal = |work: Box<dyn FnOnce() + Send>| {
        let work = Box::new(renewal_scope.clone().wrap(work));
        if renewal_scope.cancelled() {
            drop(work);
            trace::retry(WORK_CANCELLED);
            return Err(io::ErrorKind::Interrupted.into());
        }
        traced_start(renewal_spawn(work))
    };
    let spawn_vault = |work: Box<dyn FnOnce() + Send>| {
        let work = Box::new(vault_scope.clone().wrap(work));
        if vault_scope.cancelled() {
            drop(work);
            trace::retry(WORK_CANCELLED);
            return Err(io::ErrorKind::Interrupted.into());
        }
        traced_start(vault_spawn(work))
    };
    let fetch_scope = scope.clone();
    let fetch = |env: &Env, source: &str| {
        if fetch_scope.cancelled() {
            trace::retry(WORK_CANCELLED);
            return Err("account work was cancelled".to_owned());
        }
        trace::effect(trace::Effect::Fetch);
        let result = fetch_source(env, source);
        if fetch_scope.cancelled() {
            trace::retry(WORK_CANCELLED);
            return Err("account work was cancelled".to_owned());
        }
        result
    };
    let log_line = std::sync::Arc::clone(&fx.log);
    let fx = UpkeepEffects {
        scope: Some(scope),
        spawn_renewal: &spawn_renewal,
        spawn_vault: &spawn_vault,
        fetch: &fetch,
        log: std::sync::Arc::new(move |line: &str| {
            trace::effect(trace::Effect::Log);
            log_line(line);
        }),
        ..fx
    };
    run(&fx)
}

/// Why a run of the upkeep is not kept where its account work was cancelled under it: it may
/// have stopped short ([`trace::retry`]).
const WORK_CANCELLED: &str = "the account work was cancelled";

/// Record what starting a work answered in the armed trace scope: a [`trace::Effect::Spawn`]
/// where it started, and a retry where it could not.
fn traced_start(started: io::Result<()>) -> io::Result<()> {
    match &started {
        Ok(()) => trace::effect(trace::Effect::Spawn),
        Err(why) => trace::retry(format!("a work of the upkeep could not start: {why}")),
    }
    started
}

/// Whether the account work of the upkeep's pass `fx` is cancelled; where it is, the armed trace
/// scope records a retry ([`WORK_CANCELLED`]), since what the pass made may have stopped short.
fn cancelled(fx: &UpkeepEffects<'_>) -> bool {
    let cancelled = fx
        .scope
        .as_ref()
        .is_some_and(super::account_work::Scope::cancelled);
    if cancelled {
        trace::retry(WORK_CANCELLED);
    }
    cancelled
}

/// Make `duty` of the upkeep's pass through its verdict ([`memo::run`]), given `given`: a run
/// that ends with the pass's account work cancelled is not kept ([`cancelled`]).
fn duty<G, R>(fx: &UpkeepEffects<'_>, duty: memo::Duty, given: G, work: impl FnOnce() -> R) -> R
where
    G: PartialEq + 'static,
    R: Clone + 'static,
{
    memo::run(duty, given, || {
        let answer = work();
        let _ = cancelled(fx);
        answer
    })
}

/// The effects the live upkeep makes.
fn live_effects<'a>() -> UpkeepEffects<'a> {
    UpkeepEffects {
        scope: None,
        fetch: &fetch_source_text,
        spawn_renewal: &spawn_renewal_thread,
        sync_cooldown: &AUTO_SYNC_COOLDOWN,
        rediscovery: &REDISCOVERY,
        renew_guard: &AUTO_RENEW_GUARD,
        find_browser: &browser_installed,
        vault_sync: run_vault_sync,
        spawn_vault: &spawn_vault_thread,
        default_check: |home, _scope| default_login_check_live(home),
        mailbox: &ladder_mailbox_live,
        log: std::sync::Arc::new(log_sign_in),
        run_attempt: None,
        shared: shared_renewal::Effects::live(),
        tokens_here: rows_on_token_live,
    }
}

/// Claude Code's pool's upkeep ([`upkeep_claude`]), then the other CLIs' ([`upkeep_pools`]),
/// which the result does not report, then, where it syncs, the sheet's conflicts every pool's
/// sync skips, logged once per change ([`log_sheet_conflicts`]); [`Upkept::Off`] where
/// [`accounts_sync_off`] holds.
///
/// The pass compares through `now`, read once at its entry, and its verdicts are judged at that
/// reading and at the renewal guard's clock as the pass begins ([`MachineReads::clock`]), the
/// clock every hold and wait the guard keeps is compared against. Each of its duties ([`Duty`])
/// is made through its verdict ([`memo::pass`], [`duty`]), which on the keeper's thread skips a
/// duty whose kept trace stands. Every duty compares through `now` ([`trace::Now`]) and through
/// [`trace::Since`] for the holds it keeps in memory (a sync's cooldown, a failed sync's back-off,
/// rediscovery's schedule), so a run that found nothing to do is kept and stands until a read
/// moves, a hold ends or the first second that could open a door it compared. A duty takes the
/// reading raw only where it goes on to work (a sync, a heal, a choice, a checkpoint, a
/// candidate, a vault standing, a write), and is poisoned where it reads something no trace
/// holds (a minted machine id at its first draw, Codex's default login read past the trace), so
/// that run is not kept and the duty runs again at the next pass. An [`Env`] a duty reads its
/// sources under is made at the reading ([`traced_env`]).
fn upkeep(
    home: &Path,
    state_dir: &Path,
    (roster_path, now, step): (&Path, trace::Now, UpkeepStep),
    fx: &UpkeepEffects<'_>,
) -> Upkept {
    super::accounts_upkeep_counts::count_pass();
    memo::pass(
        || {
            (
                home.to_path_buf(),
                state_dir.to_path_buf(),
                roster_path.to_path_buf(),
                step,
            )
        },
        now,
        guard_now(fx.renew_guard),
        || upkeep_duties(home, state_dir, (roster_path, now, step), fx),
    )
}

/// [`upkeep`]'s duties, in their order.
fn upkeep_duties(
    home: &Path,
    state_dir: &Path,
    (roster_path, now, step): (&Path, trace::Now, UpkeepStep),
    fx: &UpkeepEffects<'_>,
) -> Upkept {
    if cancelled(fx) {
        return Upkept::Off;
    }
    let config = config_beside(roster_path);
    let Ok((policy, _)) = crate::supervise::config::SupervisorConfig::read_path(Some(&config))
    else {
        return Upkept::Off;
    };
    if policy.accounts == accounts::Mode::Off {
        return Upkept::Off;
    }
    let observe = |cli: Cli| {
        duty(fx, Duty::Observe(cli), (), || {
            let store = accounts::Store::at_for(state_dir, cli, home);
            observe_committed_logins(&store, cli, fx.renew_guard, now);
        });
    };
    if !policy.enabled {
        if step.renews() {
            for cli in [Cli::Claude].into_iter().chain(OTHER_CLIS) {
                observe(cli);
            }
        }
        return Upkept::Off;
    }
    if step.renews() && harness_sign_ins_on(&config) {
        duty(fx, Duty::Prepare, (), || {
            shared_renewal::adopt_first(home, state_dir, roster_path, now, fx);
        });
    }
    if cancelled(fx) {
        return Upkept::Off;
    }
    let claude = upkeep_claude(home, state_dir, (roster_path, now, step), fx);
    upkeep_pools(home, state_dir, (roster_path, now, step), fx);
    if cancelled(fx) {
        return claude;
    }
    if step.syncs() {
        duty(fx, Duty::SheetLog, (), || {
            log_sheet_conflicts(home, state_dir, &config, fx);
        });
    }
    if step.renews() {
        for cli in [Cli::Claude].into_iter().chain(OTHER_CLIS) {
            if !sign_ins_on(&config, cli) {
                observe(cli);
            }
        }
    }
    claude
}

/// Both explicit local switches must remain on throughout first adoption.
pub(in crate::harness) fn shared_adoption_on(roster_path: &Path) -> bool {
    let config = config_beside(roster_path);
    shared_preparation_enabled(&config)
}

/// Read the local switches that gate automatic shared preparation. This
/// observation grants no root authority or provider credential access.
#[must_use]
pub fn shared_preparation_enabled(config: &Path) -> bool {
    !accounts_sync_off(Some(config)) && harness_sign_ins_on(config)
}

/// Claude Code's pool's duties of the upkeep's pass ([`upkeep`]): its sync, the default login's
/// heal, the mailbox needs, the reading of the rows on their vault token here, a saved choice's
/// completion, the sync of a missing roster and the next due login's renewal.
fn upkeep_claude(
    home: &Path,
    state_dir: &Path,
    (roster_path, now, step): (&Path, trace::Now, UpkeepStep),
    fx: &UpkeepEffects<'_>,
) -> Upkept {
    if cancelled(fx) {
        return Upkept::Off;
    }
    let config = config_beside(roster_path);
    let syncs = step.syncs() && !accounts_sync_off(Some(&config));
    let mut scheduling = SyncScheduleClaim::new(fx.rediscovery, fx.scope.as_ref());
    if syncs {
        duty(fx, Duty::ClaudeSync, (), || {
            if sync_called_for_recording(
                home,
                state_dir,
                (roster_path, &config),
                now,
                fx.rediscovery,
                &mut scheduling,
            ) && let Some((synced, news)) =
                periodic_sync(home, state_dir, (roster_path, now), fx)
            {
                scheduling.outcome(&synced);
                if news && let Err(why) = synced {
                    (fx.log)(&format!("accounts sync: {why}"));
                }
            }
        });
    }
    if cancelled(fx) {
        return Upkept::Off;
    }
    if step.syncs() {
        duty(fx, Duty::Heal, (), || {
            heal_default_login_in_upkeep(home, state_dir, roster_path, now, fx);
        });
    }
    // Before any renewal, and whether or not the ladder signs rows in: a login a person made
    // in a tab, or one a tab was served by, since its sign-out was recorded is adopted here,
    // with no tab supervisor and whatever any tab's Autopilot says (plan package FL-174). The
    // pass reads its witnesses once, here, and the ladder below asks the same ones.
    let live = super::login_adoption::Pass::default();
    if step.renews() {
        adopt_live_logins_in_upkeep(
            (home, state_dir, roster_path),
            now,
            fx.renew_guard,
            &live,
            &*fx.log,
        );
    }
    // Before any renewal and whatever it starts: a row needs no renewal for its mailbox's
    // arrival to end the lab's ask for it.
    if step.renews() {
        duty(fx, Duty::MailboxNeeds, (), || {
            settle_mailbox_needs(home, state_dir, roster_path, now, fx);
        });
    }
    // Rotation reads which rows run on their vault token here whether or not the ladder signs
    // rows in, so every pass that renews reads the vault for it: the reading is written where
    // it changed, and the window's holder vouches for one that did not. A pass that skips the
    // read, its kept trace standing, notes the reading confirmed again, as the read it skips
    // would have.
    if step.renews() && trace::fs::is_file(roster_path) {
        memo::run_replaying(
            Duty::TokensHere,
            (),
            || {
                let confirmed = note_tokens_here(home, state_dir, roster_path, now, fx.tokens_here);
                let _ = cancelled(fx);
                confirmed
            },
            |&confirmed| {
                if confirmed {
                    accounts::Store::at_for(state_dir, Cli::Claude, home).confirm_tokens_here();
                }
            },
        );
    }
    // A saved explicit choice is independent of automatic fleet-source admission.
    let pending = step
        .renews()
        .then(|| {
            duty(fx, Duty::ClaudePin, (), || {
                resume_pending_pin(home, state_dir, roster_path, now, fx)
            })
        })
        .flatten();
    // The ladder signs rows in by default; a person's own `[harness] logins = "off"`,
    // `enabled = false` or `[harness.claude] enabled = false` turns it off (a saved explicit
    // choice above still completes).
    let signs_in = step.renews() && sign_ins_on(&config, Cli::Claude);
    let renew = || {
        duty(fx, Duty::ClaudeRenew, (), || {
            auto_renew_next_account_in(home, state_dir, roster_path, now, fx, &live)
        })
    };
    let Some(source) = configured_source(Some(&config), Some(roster_path)) else {
        // A roster written by hand or kept from before is signed in like a fetched one: the
        // ladder needs only its rows, never a source.
        let renewal = pending.map_or_else(
            || signs_in && trace::fs::is_file(roster_path) && renew(),
            RenewalDispatch::progressed,
        );
        return Upkept::Unconfigured {
            fleet_file_ignored: fleet_file_ignored(roster_path),
            renewal,
        };
    };
    let roster = if syncs {
        duty(fx, Duty::ClaudeMissing, (), || {
            sync_if_missing(home, state_dir, roster_path, &config, now, fx)
        })
    } else {
        trace::fs::exists(roster_path)
    };
    let renewal = pending.map_or_else(|| signs_in && renew(), RenewalDispatch::progressed);
    Upkept::Configured {
        kind: source.kind(),
        roster,
        renewal,
    }
}

/// THE UPKEEP'S ADOPTION STEP AND WHAT IT ENDS: adopts each Claude Code login the credential-free
/// files show live after its recorded sign-out (`login_adoption::adopt_live_logins`, from
/// `live`'s witnesses), then settles what the step did ([`settle_adoption`]); returns the step.
fn adopt_live_logins_in_upkeep(
    (home, state_dir, roster_path): (&Path, &Path, &Path),
    now: impl Into<trace::Now>,
    renew_guard: &std::sync::Arc<Watched<AutoRenewGuard>>,
    live: &super::login_adoption::Pass,
    log: &dyn Fn(&str),
) -> super::login_adoption::Step {
    let step =
        super::login_adoption::adopt_live_logins(home, state_dir, roster_path, now, live, log);
    settle_adoption(renew_guard, &step, live, log);
    step
}

/// WHAT FOLLOWS AN ADOPTION STEP: each lift ends that account's running unattended sign-in
/// ([`supersede_unattended`]), and each row a file shows live that the step could not lift
/// (`login_adoption::Refusal`: no later pass ends that by itself) is noted in `live`, so
/// evidence hides it from no ladder (`login_adoption::retain_not_live`), and its reason is
/// logged once per change, by label alone. A lift the accounts lock held up says nothing: the
/// next pass makes it. Baselines that could not be kept are logged once, until they are. A
/// step that could not read the records asked about no row and settles nothing.
fn settle_adoption(
    renew_guard: &std::sync::Arc<Watched<AutoRenewGuard>>,
    step: &super::login_adoption::Step,
    live: &super::login_adoption::Pass,
    log: &dyn Fn(&str),
) {
    use super::login_adoption::Lift;
    if step.unread {
        return;
    }
    let mut lifted = Vec::new();
    let mut lines = Vec::new();
    {
        let mut guard = renew_guard
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // What this step was refused replaces what the last one was: a row with no refusal
        // now is said again if one comes back.
        let mut refused = BTreeMap::new();
        for (label, lift) in &step.lifts {
            match lift {
                Lift::Landed(_) => lifted.push(label.clone()),
                Lift::Refused(why) => {
                    live.refuse(label);
                    refused.insert(label.clone(), why.words());
                }
                // Neither says the reason is over: what was said of the row stands.
                Lift::Busy | Lift::Already => {
                    if let Some(said) = guard.adoption_refused.get(label) {
                        refused.insert(label.clone(), said.clone());
                    }
                }
            }
        }
        for (label, why) in &refused {
            if guard.adoption_refused.get(label) != Some(why) {
                lines.push(format!(
                    "account {label}: a tab on its login looks live again, but its recorded \
                     sign-out stands: {why}; the sign-in ladder goes on"
                ));
            }
        }
        guard.adoption_refused = refused;
        if step.baselines_unkept && !guard.baselines_unkept_said {
            lines.push(
                "accounts: the usage-tap baselines could not be kept, so no login a tab is \
                 served by is adopted until they can be"
                    .to_owned(),
            );
        }
        guard.baselines_unkept_said = step.baselines_unkept;
    }
    for line in &lines {
        log(line);
    }
    supersede_unattended(renew_guard, &lifted, log);
}

/// A pass's witnesses for a caller with no adoption step of its own (a person's click that
/// names no row, a Next selection): it knows the rows the upkeep's last step could not lift
/// ([`settle_adoption`]), so evidence hides none of them from it either.
fn live_logins_known(
    renew_guard: &std::sync::Arc<Watched<AutoRenewGuard>>,
) -> super::login_adoption::Pass {
    super::login_adoption::Pass::knowing(
        renew_guard
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .adoption_refused
            .keys()
            .cloned(),
    )
}

/// An unattended sign-in the upkeep's ladder started and that runs now, as a lift of its
/// account ends it ([`supersede_unattended`]). Only the ladder's own runs are marked: a
/// sign-in a person started (a click, a pin request, a saved choice, a Next selection) is never
/// marked, so nothing here can cancel it.
#[derive(Clone)]
struct UnattendedRun {
    /// Set once the run is superseded; its worker then counts it neither failed nor blocked.
    superseded: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Ends the run's work and nothing else: the cancellation of the run's own operation
    /// ([`RunMark::scope`], a child of the upkeep step that started it), which its capture
    /// watches and ends its child on, by the child's own identity
    /// (`align::capture_bounded_owned_watching`); the opener's end closes its window. `None`
    /// where the run was started under no scope.
    cancel: Option<super::account_work::Cancellation>,
}

/// A run counts in the guard's generation ([`AutoRenewGuard`]) by its label, the map's key, and
/// whether it runs under a scope, not by the identity of its flag or its cancellation. Its
/// supersession is set outside the guard's lock by the lift, on the lifting thread
/// ([`supersede_unattended`], whose swap reads the flag only to set it once), and decides only
/// how the run's own worker records its end ([`RunMark::superseded`]), nothing a reader of the
/// guard decides, so it moves nothing here.
impl std::hash::Hash for UnattendedRun {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.cancel.is_some().hash(state);
    }
}

/// The mark of one unattended run in the guard, removed when its work ends or is dropped unrun.
struct RunMark {
    guard: std::sync::Arc<Watched<AutoRenewGuard>>,
    label: String,
    run: UnattendedRun,
    /// THE RUN'S OWN OPERATION (`account_work::Scope::child`): the scope its work runs under
    /// in place of its upkeep step's, so cancelling it ends this one sign-in and leaves the
    /// rest of that step (its fetch, its vault refresh, another CLI's saved choice) running,
    /// while everything that stops the step still stops the run. `None` where the run was
    /// started under no scope.
    scope: Option<super::account_work::Scope>,
}

impl RunMark {
    /// Marks the ladder's own run of `label` (`unattended`) in `guard`, entering its own
    /// operation under `scope`; `None` for a run a person started, which is never marked.
    fn of(
        guard: &std::sync::Arc<Watched<AutoRenewGuard>>,
        label: &str,
        unattended: bool,
        scope: Option<&super::account_work::Scope>,
    ) -> Option<Self> {
        if !unattended {
            return None;
        }
        // A scope whose count cannot grow gives no child: the run then keeps the step's
        // scope, and a lift marks it superseded without ending it.
        let scope = scope.and_then(super::account_work::Scope::child);
        let run = UnattendedRun {
            superseded: std::sync::Arc::default(),
            cancel: scope.as_ref().map(super::account_work::Scope::cancellation),
        };
        guard
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .unattended_runs
            .insert(label.to_owned(), run.clone());
        Some(Self {
            guard: std::sync::Arc::clone(guard),
            label: label.to_owned(),
            run,
            scope,
        })
    }

    /// Whether a lift of the account superseded this run.
    fn superseded(&self) -> bool {
        self.run
            .superseded
            .load(std::sync::atomic::Ordering::Acquire)
    }
}

impl Drop for RunMark {
    fn drop(&mut self) {
        let mut guard = self
            .guard
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if guard
            .unattended_runs
            .get(&self.label)
            .is_some_and(|run| std::sync::Arc::ptr_eq(&run.superseded, &self.run.superseded))
        {
            guard.unattended_runs.remove(&self.label);
        }
    }
}

/// A LIFT CANCELS THAT ACCOUNT'S RUNNING UNATTENDED UPKEEP SIGN-IN (plan package FL-174): each
/// marked run ([`RunMark`]) of a label in `lifted` is superseded: its own operation is
/// cancelled (the staging child ends by its own identity and its window closes with it;
/// nothing else its upkeep step started is touched) and `sign-in superseded` is logged. Its
/// worker then records no failure, and gives its spacing back where it never reached the
/// provider. A sign-in a person started carries no mark and is never touched.
fn supersede_unattended(
    guard: &std::sync::Arc<Watched<AutoRenewGuard>>,
    lifted: &[String],
    log: &dyn Fn(&str),
) {
    for label in lifted {
        let Some(run) = guard
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .unattended_runs
            .get(label)
            .cloned()
        else {
            continue;
        };
        if run
            .superseded
            .swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            continue;
        }
        if let Some(cancel) = &run.cancel {
            cancel.cancel();
        }
        log(&format!(
            "account {label}: automatic sign-in superseded: its login is live again, so the \
             unattended sign-in is ended"
        ));
    }
}

/// THE LAST ASK BEFORE AN UNATTENDED SIGN-IN STARTS, made under the sign-in lock (plan package
/// FL-174): whether `label`'s own login was signed in or served after its recorded sign-out
/// (`login_adoption::Witnesses::live_login_here`, from the pass's witnesses `live`) AND its
/// record can be lifted. Where the files show it live, the adoption step runs now
/// ([`adopt_live_logins_in_upkeep`]) and nothing starts only where the lift landed, another
/// process had made it, or the accounts lock held it up (the next pass makes it). A row the
/// step is refused (`login_adoption::Refusal`) is signed out on record and stays so: its
/// sign-in starts, and the reason is in the log.
fn login_live_again(
    (home, state_dir, roster_path): (&Path, &Path, &Path),
    label: &str,
    now: i64,
    renew_guard: &std::sync::Arc<Watched<AutoRenewGuard>>,
    live: &super::login_adoption::Pass,
    log: &dyn Fn(&str),
) -> bool {
    use super::login_adoption::Lift;
    if live.refused(label) {
        return false;
    }
    let store = accounts::Store::at_for(state_dir, Cli::Claude, home);
    let Ok(records) = store.read_account_records(Some(roster_path), now) else {
        return false;
    };
    if live
        .live(
            (home, state_dir),
            &store,
            (&records.roster, &records.state),
            label,
            now,
        )
        .is_none()
    {
        return false;
    }
    let step =
        adopt_live_logins_in_upkeep((home, state_dir, roster_path), now, renew_guard, live, log);
    matches!(
        step.lift_of(label),
        Some(Lift::Landed(_) | Lift::Already | Lift::Busy)
    )
}

/// Whether the config at `config` lets the harness sign `cli`'s rows in by itself: no switch
/// keeps them off ([`sign_ins_switch`]): accounts not turned off, and its login lifecycle on
/// ([`crate::supervise::config::SupervisorConfig::logins_for`], the window's reading too: the
/// harness on, `[harness] logins` on, and `cli`'s own table not switched off).
pub(crate) fn sign_ins_on(config: &Path, cli: Cli) -> bool {
    let (cfg, _) = crate::supervise::config::SupervisorConfig::from_path(Some(config));
    sign_ins_switch(&cfg, Some(cli)).is_none()
}

/// Whether the config at `config` lets the harness sign every pool's rows in by itself: none of
/// the harness's own switches keeps them off ([`sign_ins_switch`]). A CLI's own table keeps that
/// CLI's alone off ([`sign_ins_on`]).
pub(crate) fn harness_sign_ins_on(config: &Path) -> bool {
    let (cfg, _) = crate::supervise::config::SupervisorConfig::from_path(Some(config));
    sign_ins_switch(&cfg, None).is_none()
}

/// The switch `cfg` leaves off that keeps the harness from signing `cli`'s rows in by itself
/// (every pool's, with no `cli`), where one does: the master switch, account rotation and the
/// login lifecycle, which keep every pool's off, else `cli`'s own table (`[harness.claude]`,
/// `[harness.codex]` or `[harness.agy]`; Grok Build and Kimi Code have no table of their own,
/// so nothing switches them off alone), which keeps that CLI's off and no other's.
fn sign_ins_switch(
    cfg: &crate::supervise::config::SupervisorConfig,
    cli: Option<Cli>,
) -> Option<crate::supervise::config::Switch> {
    use crate::supervise::config::Switch;
    [Switch::Harness, Switch::Accounts, Switch::Logins]
        .into_iter()
        .chain(cli.map(|cli| Switch::Agent(cli.program())))
        .find(|switch| !cfg.switch_on(*switch))
}

/// What in the config at `config` keeps the harness from signing `cli`'s rows in by itself
/// ([`sign_ins_on`]), in aterm.toml's own words, where something does: the first setting that
/// turns [`sign_ins_switch`]'s switch off ([`crate::supervise::config::OffSetting::words`]); an
/// aterm.toml that cannot be read, which turns the harness off
/// ([`crate::supervise::config::SupervisorConfig::from_path`]); or one that is not valid TOML,
/// whose lines are read one by one. `aterm ai` then leaves a row's sign-in to a person, as the
/// upkeep does.
pub(crate) fn sign_ins_off_for(config: &Path, cli: Cli) -> Option<String> {
    use crate::supervise::config::SupervisorConfig;
    let text = match aterm_types::dirs::read_aterm_config(config) {
        Ok(None) => return None,
        Ok(Some(bytes)) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(why) => return Some(format!("an aterm.toml that cannot be read ({why})")),
    };
    let (cfg, _) = SupervisorConfig::from_aterm_toml(&text);
    let switch = sign_ins_switch(&cfg, Some(cli))?;
    Some(match SupervisorConfig::switched_off_by(&text) {
        Ok(off_by) => off_by
            .into_iter()
            .find(|off| off.switch == switch)
            .map_or_else(|| "aterm.toml".to_string(), |off| off.by.words()),
        Err(why) => {
            format!("an aterm.toml that is not valid TOML ({why}) and is read line by line")
        }
    })
}

/// THE SHEET'S CONFLICTS THIS MACHINE'S POOLS SKIP ([`SheetConflict`]), for the window's
/// exceptions digest, the upkeep's log ([`upkeep`]) and `aterm init`'s `base.ai.accounts` row
/// (none where [`accounts_sync_off`] holds, since no sync reads the sheet):
/// for each CLI whose fleet the vault's sheet (its entry `ai-accounts.csv`) is
/// (Claude Code's where [`fleet_origin`] says so; another CLI's where this machine keeps its
/// roster or the CLI syncs by default ([`syncs_by_default`]), and no `ai-accounts-<cli>.toml`
/// outranks the sheet),
/// the addresses its rows give conflicting assignments whose member rows name this machine or
/// none (all of them where this machine's id is unknown). Nothing where the sheet is absent or
/// refused: the sync's own refusal says why.
#[must_use]
pub fn sheet_conflicts(home: &Path, state_dir: &Path, config: &Path) -> Vec<SheetConflict> {
    sheet_conflicts_look(home, state_dir, config).conflicts
}

/// One look at the sheet's conflicts ([`sheet_conflicts_look`]).
#[derive(Debug, Clone, PartialEq, Eq)]
struct SheetLook {
    /// The conflicts this machine's pools skip ([`sheet_conflicts`]).
    conflicts: Vec<SheetConflict>,
    /// Whether the sheet was read whole: present and readable, read for some pool, and admitted
    /// for every pool that reads it. No conflicts from a sheet read whole is rows that agree;
    /// none from one absent, refused, read for no pool or refused for one says nothing of them.
    whole: bool,
}

/// [`sheet_conflicts`], and whether the sheet was read whole ([`SheetLook::whole`]).
fn sheet_conflicts_look(home: &Path, state_dir: &Path, config: &Path) -> SheetLook {
    let unread = SheetLook {
        conflicts: Vec::new(),
        whole: false,
    };
    // Where the lab registry names this Mac, or may and could not be read yet, the sheet is the
    // importer's input alone ([`sheet_read_here`]): upkeep reads the lab, and no sheet conflict
    // is this Mac's to report.
    if accounts_sync_off(Some(config)) || !sheet_read_here() {
        return unread;
    }
    // The sheet and each CLI's fleet, from one version of the vault.
    let cli_files = OTHER_CLIS.map(cli_fleet_file);
    let names = std::iter::once(ACCOUNTS_CSV_FILE)
        .chain(cli_files.iter().map(String::as_str))
        .collect::<Vec<_>>();
    let mut reads = crate::vault_entry::read_set(home, &names, SOURCE_TEXT_MAX_BYTES).into_iter();
    let Some(Ok(Some(text))) = reads.next().map(crate::vault_entry::Read::into_text) else {
        return unread;
    };
    let cli_held = OTHER_CLIS
        .into_iter()
        .zip(reads)
        .map(|(cli, read)| (cli, read.occupies()))
        .collect::<BTreeMap<_, _>>();
    let reads_sheet = |cli: Cli| {
        let Some(roster) = accounts::roster_beside_for(config, cli) else {
            return false;
        };
        match cli {
            Cli::Claude => {
                fleet_origin(
                    &accounts::Store::at(state_dir),
                    Some(home),
                    Some(config),
                    &roster,
                ) == FleetOrigin::Vault(ACCOUNTS_CSV_FILE.to_owned())
            }
            _ => {
                (syncs_by_default(cli) || trace::fs::is_file(&roster))
                    && !cli_held.get(&cli).copied().unwrap_or(true)
            }
        }
    };
    let machine = config_machine_id(Some(config)).or_else(detected_machine);
    let (mut read, mut refused) = (false, false);
    let mut conflicts = Vec::new();
    for cli in std::iter::once(Cli::Claude)
        .chain(OTHER_CLIS)
        .filter(|&cli| csv_names_column(cli, &text) && reads_sheet(cli))
    {
        match parse_accounting_csv_for(cli, &text, ACCOUNTS_CSV_FILE) {
            Ok(fleet) => {
                read = true;
                conflicts.extend(fleet.roster.conflicts_for(machine.as_deref()));
            }
            Err(_) => refused = true,
        }
    }
    SheetLook {
        conflicts,
        whole: read && !refused,
    }
}

/// Whether this machine's pool skips `conflict`: its member rows name `machine`, or name none
/// (every one, where this machine's id is unknown).
fn conflict_names(conflict: &SheetConflict, machine: Option<&str>) -> bool {
    conflict.machines.is_empty()
        || machine.and_then(normalize_machine_id).is_none_or(|mine| {
            conflict
                .machines
                .iter()
                .any(|named| normalize_machine_id(named).as_deref() == Some(mine.as_str()))
        })
}

/// `conflicts` as a sync's `--json` report lists them: each one's CLI and sheet rows.
fn conflicts_json(conflicts: &[SheetConflict]) -> Value {
    Value::Array(
        conflicts
            .iter()
            .map(|conflict| {
                let mut o = Map::new();
                o.insert("cli".to_owned(), Value::from(conflict.cli.id().to_owned()));
                o.insert(
                    "rows".to_owned(),
                    Value::Array(
                        conflict
                            .rows
                            .iter()
                            .map(|row| Value::from(u64::try_from(*row).unwrap_or(0)))
                            .collect(),
                    ),
                );
                Value::Object(o)
            })
            .collect(),
    )
}

/// `conflicts` ([`SheetConflict`]) in one line for a report or a log: each CLI with its sheet
/// rows, never an address. `None` for none.
#[must_use]
pub fn sheet_conflicts_words(conflicts: &[SheetConflict]) -> Option<String> {
    if conflicts.is_empty() {
        return None;
    }
    let each: Vec<String> = conflicts
        .iter()
        .map(|conflict| {
            let rows: Vec<String> = conflict.rows.iter().map(usize::to_string).collect();
            format!("{} rows {}", conflict.cli.id(), rows.join(", "))
        })
        .collect();
    let what = if conflicts.len() == 1 {
        "an address"
    } else {
        "addresses"
    };
    Some(format!(
        "the account sheet gives {what} conflicting assignments ({}): no pool adds one until \
         its rows agree, and a roster that lists one keeps its row",
        each.join("; ")
    ))
}

/// `why` a sync took nothing, with `conflicts` ([`sheet_conflicts_words`]) after it where there
/// are any: a row of this machine's that conflicts is often the reason.
fn with_conflicts(why: String, conflicts: &[SheetConflict]) -> String {
    match sheet_conflicts_words(conflicts) {
        Some(words) => format!("{why}; {words}"),
        None => why,
    }
}

/// Logs the sheet's conflicts this machine's pools skip ([`sheet_conflicts`]) through
/// `fx.log` once per change, and once when the last of them ends: a Mac with no window to
/// show the digest says it too. An end says the rows agree only where the sheet was read whole
/// ([`SheetLook::whole`]); one that is gone, refused or unparsable says the rows are not known.
fn log_sheet_conflicts(home: &Path, state_dir: &Path, config: &Path, fx: &UpkeepEffects<'_>) {
    let SheetLook {
        conflicts: now,
        whole,
    } = sheet_conflicts_look(home, state_dir, config);
    let changed = {
        let mut held = fx
            .rediscovery
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if held.sheet_said == now {
            false
        } else {
            held.sheet_said.clone_from(&now);
            true
        }
    };
    if changed {
        let words = sheet_conflicts_words(&now).unwrap_or_else(|| {
            if whole {
                "the account sheet's conflicting rows agree now"
            } else {
                "the account sheet is not read whole now (absent, refused or unparsable), so \
                 whether its rows conflict is not known"
            }
            .to_owned()
        });
        (fx.log)(&format!("accounts sync: {words}"));
    }
}

/// This host's id ([`detect_machine_id`] with no override or config), read once per process,
/// so a look that runs often runs no `scutil` each time ([`sheet_conflicts`], and
/// [`fleet_origin`] through [`machine_for`]).
fn detected_machine() -> Option<String> {
    static DETECTED: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    DETECTED
        .get_or_init(|| detect_machine_id(None, None))
        .clone()
}

/// Why automatic source sync and sign-in work is held back by [`accounts_sync_off`].
pub const ACCOUNTS_SYNC_OFF: &str =
    "the harness or account-sync policy is off or unavailable; automatic accounts sync is disabled";

/// THE ACCOUNTS' OFF SWITCH (`docs/DESIGN-init-on-by-default-2026-10-25.md` §8.5,
/// `docs/DESIGN-alab-swarm-2026-10-27.md` §9): whether the master harness switch is off (also
/// when its config cannot be admitted or read), or `[harness] accounts = "off"`. Then no
/// automatic sync or upkeep reads a source, writes a roster,
/// or starts a sign-in of any pool: not the window's upkeep ([`upkeep_live`]), the setup's resync
/// ([`resync_for_setup`]), [`sync_accounts_for_init`] or [`sync_cli_accounts_for_init`] (what
/// `aterm init`'s `base.ai.accounts` and `aterm secrets join` run), nor the converge pass's
/// `sync.accounts`. A person's own `aterm harness accounts sync` still syncs, and while accounts
/// are not `"off"`, `[harness] logins` switches the sign-in ladder alone ([`sign_ins_on`]).
/// An admitted disabled master still allows renewal upkeep to observe already committed
/// local logins for idle claims, without journal recovery or automatic source work.
/// An unreadable configuration or explicit accounts-off policy permits no such observation.
#[must_use]
pub fn accounts_sync_off(config: Option<&Path>) -> bool {
    let policy = crate::supervise::config::SupervisorConfig::from_path(config).0;
    !policy.enabled || policy.accounts == accounts::Mode::Off
}

/// Whether a sync of `cli` from the lab would change the roster at `roster_path`
/// ([`super::alab_roster::sync_due`]), or Claude Code's `store` has not retired its legacy
/// source yet.
fn lab_sync_due(cli: Cli, store: &accounts::Store, roster_path: &Path) -> bool {
    (cli == Cli::Claude && !store.source_policy().is_ok_and(|policy| policy.retired()))
        || super::alab_roster::sync_due(cli, roster_path)
}

/// THE ONE RULE OF WHEN A PERIODIC CALLER SYNCS a roster that stands (a window's upkeep, the
/// setup pass's [`resync_for_setup`]): when its inputs changed or need revalidation. That is
/// the vault holding a fleet, a sheet or a mailbox where no source is configured yet; a vault
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
    rediscovery: &Watched<Rediscovery>,
) -> bool {
    sync_called_for_recording(
        home,
        state_dir,
        (roster_path, config),
        now,
        rediscovery,
        &mut SyncScheduleClaim::new(rediscovery, None),
    )
}

fn sync_called_for_recording(
    home: &Path,
    state_dir: &Path,
    (roster_path, config): (&Path, &Path),
    now: impl Into<trace::Now>,
    rediscovery: &Watched<Rediscovery>,
    claim: &mut SyncScheduleClaim<'_>,
) -> bool {
    let now: trace::Now = now.into();
    if accounts::Store::at(state_dir).sync_needs_recovery() {
        return true;
    }
    if claude_unassigned(home, state_dir, roster_path, now) {
        return false;
    }
    // Where the lab is the source, its projection and this Mac's own default login are what
    // move a roster: read in memory, with no sheet, fleet or pool file. While the lab is not read
    // yet nothing moves it.
    match lab_chosen(Cli::Claude, roster_path, config) {
        Chosen::Lab(_) => {
            return lab_sync_due(Cli::Claude, &accounts::Store::at(state_dir), roster_path)
                || default_login_unlisted(
                    home,
                    state_dir,
                    (roster_path, config),
                    now,
                    rediscovery,
                    Some(claim),
                );
        }
        // A sync while the lab is not read yet writes nothing ([`sync_claude_pending`]), so none
        // is called for until the lab is read, whose projection moves the inputs.
        Chosen::Pending(_) => return false,
        Chosen::Unnamed => {}
    }
    let configured = configured_source(Some(config), Some(roster_path));
    // A mode change invalidates this process's observation. Returning to the same path
    // must not inherit the timer from a source which stopped being authoritative.
    let key_changed = if matches!(configured, Some(FleetSource::Key(_))) {
        key_file_changed(home, state_dir, config, now, rediscovery, Some(&mut *claim))
    } else {
        let mut held = rediscovery
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(before) = held.key_attempt.take() {
            claim
                .changes
                .push(SyncScheduleChange::Key(Some(before), None));
        }
        false
    };
    let unconfigured_with_secrets = configured.is_none()
        && (has_fleet_source(home, &accounts::Store::at(state_dir))
            || vault_mailbox_held(home)
            || (crate::vault_entry::configured(home)
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
            rediscover_due(rediscovery, claim),
        )
        || default_login_unlisted(
            home,
            state_dir,
            (roster_path, config),
            now,
            rediscovery,
            Some(claim),
        )
        || key_changed
}

/// THE ONE RETRY RULE of a periodic caller's sync, once a missing roster or [`sync_called_for`]
/// calls for one: a sync whose inputs ([`sync_inputs`]) are those of the last one that failed
/// waits [`REDISCOVER_EVERY`] from that attempt, so a sheet that names no account of this
/// machine is not read again at every upkeep, while changed inputs (an edited sheet, a roster or
/// fleet file written since, another default login or `[fleet] machine`) are read at once. A
/// retryable source or custody failure holds nothing back, and the recovery
/// of a pending local commit is never held. Returns what the sync did and whether its failure is
/// news ([`note_sync_result`]), which a caller logs; none while it waits.
fn periodic_sync(
    home: &Path,
    state_dir: &Path,
    (roster_path, now): (&Path, trace::Now),
    fx: &UpkeepEffects<'_>,
) -> Option<(Result<InitSynced, SyncError>, bool)> {
    if fx
        .scope
        .as_ref()
        .is_some_and(super::account_work::Scope::cancelled)
    {
        return None;
    }
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
    // A sync runs: Claude Code's sync from init reads and compares its reading as a number.
    let synced =
        sync_accounts_for_init_with(home, state_dir, roster_path, now.raw(), || {}, fx.fetch);
    if fx
        .scope
        .as_ref()
        .is_some_and(super::account_work::Scope::cancelled)
    {
        return Some((synced, false));
    }
    let news = note_sync_result(
        &mut fx
            .rediscovery
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .failed,
        inputs,
        synced.as_ref().map(|_| ()),
    );
    Some((synced, news))
}

/// One digest of what a sync from init reads that a periodic caller reads cheaply
/// ([`periodic_sync`]): the vault's pool, fleet, sheet and mailbox entries (one in-process
/// digest of them, [`crate::vault_entry::digest`]), the roster, the fleet file beside it, the
/// config (`[fleet] machine`, `accounts_source`) and a local file that source names, and the
/// default login's address. What else discovery reads (logins, transcripts, other mailbox
/// configurations), a URL and the host's name are left out: [`REDISCOVER_EVERY`] reads them
/// again.
fn sync_inputs(
    home: &Path,
    state_dir: &Path,
    roster_path: &Path,
    now: impl Into<trace::Now>,
) -> [u8; 32] {
    let now: trace::Now = now.into();
    let config = config_beside(roster_path);
    let env = traced_env(home, state_dir, &config, now);
    let mut hash = aterm_digest::Sha256::new();
    hash.update(b"aterm-accounts-sync-inputs-v2");
    let mut field = |value: Option<&[u8]>| match value {
        None => hash.update([0]),
        Some(value) => {
            hash.update([1]);
            hash.update((value.len() as u64).to_be_bytes());
            hash.update(value);
        }
    };
    let policy = match env.accounts_store().source_policy() {
        Ok(policy) => {
            field(policy.encoded());
            policy
        }
        Err(why) => {
            field(Some(why.as_bytes()));
            return hash.finalize();
        }
    };
    // Where the lab is the source, or is not read yet, what it holds stands for the vault's files:
    // no sheet, fleet or pool file is read.
    if !matches!(
        lab_chosen(Cli::Claude, roster_path, &config),
        Chosen::Unnamed
    ) {
        field(
            super::alab_roster::fleet_digest(Cli::Claude)
                .as_ref()
                .map(<[u8; 32]>::as_slice),
        );
        for path in [roster_path.to_path_buf(), config.clone()] {
            field(
                read_local_source_text(&path)
                    .ok()
                    .as_deref()
                    .map(str::as_bytes),
            );
        }
        field(
            read_default_profile_email(&env)
                .as_deref()
                .map(str::as_bytes),
        );
        return hash.finalize();
    }
    let selected = match source_selection::select_observed(
        Some(home),
        Some(&config),
        Some(roster_path),
        SourceScope::Init,
        &policy,
    ) {
        Ok(selected) => selected,
        Err(why) => {
            field(Some(why.as_bytes()));
            return hash.finalize();
        }
    };
    field(selected.source().as_deref().map(str::as_bytes));
    let vault = [
        Some(super::ai_pool::AI_POOL_FILE),
        (!policy.retired()).then_some(FLEET_ROSTER_FILE),
        sheet_read_here().then_some(ACCOUNTS_CSV_FILE),
        Some(super::headless_oauth::SECRETS_MAILBOX_TOML_FILE),
        Some(super::headless_oauth::MAILBOX_TOML_FILE),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    field(Some(&crate::vault_entry::digest(
        home,
        &vault,
        SOURCE_TEXT_MAX_BYTES,
    )));
    for path in [
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
    if matches!(selected, SelectedSource::Pool(_)) {
        let id_bytes = super::ai_pool::resolve_local_pool_identity(Some(home), Some(&config), None)
            .map(|id| id.map(|id| format!("{}:{}", id.primary, id.aliases.join(",")).into_bytes()))
            .unwrap_or_else(|why| Some(why.into_bytes()));
        field(id_bytes.as_deref());
    }
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
    now: impl Into<trace::Now>,
    rediscovery: &Watched<Rediscovery>,
    claim: Option<&mut SyncScheduleClaim<'_>>,
) -> bool {
    let now: trace::Now = now.into();
    let path = key_source_file(&traced_env(home, state_dir, config, now));
    let version = path.map(|path| {
        let stamp = trace::fs::metadata(&path)
            .and_then(|meta| meta.modified().map(|modified| (modified, meta.len())))
            .ok();
        (path, stamp)
    });
    let mut held = rediscovery
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let before = held.key_attempt.clone();
    let changed = if let Some(version) = version {
        schedule_local_source(&mut held.key_attempt, version, Instant::now())
    } else {
        held.key_attempt = None;
        false
    };
    if let Some(claim) = claim
        && held.key_attempt != before
    {
        claim
            .changes
            .push(SyncScheduleChange::Key(before, held.key_attempt.clone()));
    }
    changed
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

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
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
        last.version == version
            && !trace::Since::new(last.at).elapsed_at_least(REDISCOVER_EVERY, now)
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

/// The source outcome and concurrent scheduler activity are environment inputs.
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "AccountSyncRetry",
    action = "Scenario",
    reason = "The source supplies a successful, rejected or retryable result."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "AccountSyncRetry",
    action = "Supersede",
    reason = "Another operation can install newer scheduling claims before completion."
)]
pub fn account_sync_retry_environment() {}

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
    now: impl Into<trace::Now>,
    fx: &UpkeepEffects<'_>,
) -> bool {
    let now: trace::Now = now.into();
    if fx
        .scope
        .as_ref()
        .is_some_and(super::account_work::Scope::cancelled)
    {
        return false;
    }
    let store = accounts::Store::at(state_dir);
    if store.sync_needs_recovery()
        && store
            .recover_sync_within(&sync_lock_ladder(), roster_path)
            .is_err()
    {
        return false;
    }
    if trace::fs::exists(roster_path) {
        return true;
    }
    if AutomaticSource::capture(config).is_err() {
        return false;
    }
    let Some(mut cooldown) = SyncCooldownClaim::take(fx.sync_cooldown, fx.scope.as_ref()) else {
        return false;
    };
    // Missing-roster upkeep uses the same automatic capture and source selection as init.
    // A source-less manual request deliberately has a narrower set of inputs.
    // A sync runs: Claude Code's sync from init reads and compares its reading as a number.
    let synced =
        sync_accounts_for_init_with(home, state_dir, roster_path, now.raw(), || {}, fx.fetch);
    cooldown.outcome(&synced);
    trace::fs::exists(roster_path)
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
    if !matches!(
        lab_chosen(Cli::Claude, roster_path, &config_path),
        Chosen::Unnamed
    ) {
        return true;
    }
    let Ok(selected) = source_selection::select_for(
        &accounts::Store::at(state_dir),
        Some(home),
        Some(&config_path),
        Some(roster_path),
        SourceScope::Init,
    ) else {
        return true;
    };
    let configured = match selected {
        SelectedSource::Absent => false,
        SelectedSource::Beside { opt_in, .. } if !opt_in.is_some_and(OptIn::opts_in_by_file) => {
            // An explicit init can read this file. Presence alone does not opt a fresh
            // roster into upkeep; an existing roster can still be upgraded from it.
            Roster::load(roster_path).is_ok_and(|roster| !roster.rows.is_empty())
        }
        _ => true,
    };
    if configured {
        return true;
    }
    if vault_mailbox_held(home) {
        return true;
    }
    let has_discovered_secrets_mailbox = crate::vault_entry::configured(home)
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
        xdg_config_home: None,
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

/// Whether the vault holds either mailbox configuration for this Mac (`ai-mailbox.toml`,
/// `mailbox.toml`), usable now or not, read from one version of the vault.
fn vault_mailbox_held(home: &Path) -> bool {
    crate::vault_entry::read_set(
        home,
        &[
            super::headless_oauth::SECRETS_MAILBOX_TOML_FILE,
            super::headless_oauth::MAILBOX_TOML_FILE,
        ],
        super::headless_oauth::MAILBOX_TOML_MAX_BYTES,
    )
    .iter()
    .any(crate::vault_entry::Read::occupies)
}

/// Whether the vault holds an updated fleet or accounting sheet not yet converged into
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
    now: impl Into<trace::Now>,
    discover: bool,
) -> bool {
    let now: trace::Now = now.into();
    match lab_chosen(Cli::Claude, roster_path, &config_beside(roster_path)) {
        Chosen::Lab(_) => {
            return lab_sync_due(Cli::Claude, &accounts::Store::at(state_dir), roster_path);
        }
        // A sync while the lab is not read yet writes nothing ([`sync_claude_pending`]).
        Chosen::Pending(_) => return false,
        Chosen::Unnamed => {}
    }
    let config_path = config_beside(roster_path);
    let Ok(selected) = source_selection::select_for(
        &accounts::Store::at(state_dir),
        Some(home),
        Some(&config_path),
        Some(roster_path),
        SourceScope::Init,
    ) else {
        return true;
    };
    if matches!(selected, SelectedSource::Key(_) | SelectedSource::Absent) {
        // Configured inputs have their own change detector; no source permits no discovery here.
        return false;
    }
    let Some(fb) = fleet_file_beside(roster_path) else {
        return false;
    };
    if !fleet_file_opt_in(&fb).is_some_and(OptIn::opts_in_by_file) {
        return !matches!(selected, SelectedSource::Beside { .. });
    }
    if selected.retires_legacy(Some(&config_path))
        && accounts::Store::at(state_dir)
            .source_policy()
            .is_ok_and(|policy| !policy.retired())
    {
        return true;
    }
    if let SelectedSource::Pool(vault_pool) = &selected {
        let Ok(existing_text) = read_local_source_text(&fb) else {
            return true;
        };
        let Ok(existing_fleet) = FleetRoster::from_toml(&existing_text, FLEET_ROSTER_FILE) else {
            return true;
        };
        let Ok(raw) = vault_pool.text() else {
            return true;
        };
        let Ok(pool) = super::ai_pool::Pool::from_toml(&raw, super::ai_pool::AI_POOL_FILE) else {
            return true;
        };
        let Ok(Some(identity)) =
            super::ai_pool::resolve_local_pool_identity(Some(home), Some(&config_path), None)
        else {
            return true;
        };
        let projected = project_pool_fleet(&pool, Cli::Claude, Some(&identity));
        if projected.accounts != existing_fleet.accounts {
            return true;
        }
        if !projected.accounts_for_machine(&identity.primary).is_empty()
            && !trace::fs::exists(roster_path)
        {
            return true;
        }
        let ctx = PoolSyncCtx { pool, identity };
        let (expected_home, execution_rows) = if trace::fs::exists(roster_path) {
            let Ok(existing_roster) = Roster::load(roster_path) else {
                return true;
            };
            if existing_roster.rows.iter().any(|row| {
                (row.email.is_some() || row.identity_email.is_some())
                    && row.held_by != ctx.holder_for_row(Cli::Claude, row)
            }) {
                return true;
            }
            let home = existing_roster
                .rows
                .iter()
                .filter(|row| row.held_by.is_none() && ctx.is_home_for_row(Cli::Claude, row))
                .map(|row| row.label.clone())
                .collect::<BTreeSet<_>>();
            (home, existing_roster.rows)
        } else {
            (BTreeSet::new(), Vec::new())
        };
        return accounts::Store::at(state_dir)
            .load(now)
            .map(|(state, _)| {
                state.swarm_label.as_deref() != Some(ctx.identity.primary.as_str())
                    || state.swarm_aliases != ctx.identity.aliases
                    || state.home_accounts != expected_home
                    || pool_execution_pending(&state, &execution_rows, Cli::Claude, &ctx)
            })
            .unwrap_or(true);
    }
    let Ok(existing_text) = read_local_source_text(&fb) else {
        return true;
    };
    let Ok(existing_fleet) = FleetRoster::from_toml(&existing_text, "fleet-accounts.toml") else {
        return true;
    };
    let opt_in = opt_in_of(&existing_text);
    if let SelectedSource::VaultFleet { entry } = &selected {
        // A source owned outside the local projection is authoritative for its
        // rows. Compare parsed rows: formatting and the generated opt-in header
        // are not new account data, and local discovery must not extend this input.
        return entry
            .text()
            .ok()
            .and_then(|text| {
                let source = FleetRoster::from_toml(&text, &entry.source()).ok()?;
                let source_opt_in = if opt_in_of(&text) == Some(OptIn::Discovered) {
                    OptIn::Discovered
                } else {
                    OptIn::Named
                };
                Some((source, source_opt_in))
            })
            .is_none_or(|(source, source_opt_in)| {
                source.accounts != existing_fleet.accounts || opt_in != Some(source_opt_in)
            });
    }
    // A sync from the sheet writes its accounts as they are: any edit (a row added, removed, or
    // moved to another machine) is one the fleet file does not hold yet.
    if let SelectedSource::Sheet { entry } = &selected {
        let Ok(csv_text) = entry.text() else {
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
        && !vault_mailbox_held(home)
        && !(crate::vault_entry::configured(home)
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
        xdg_config_home: None,
        codex_login: None,
        // Discovery reads this machine's logins and transcripts, comparing as numbers.
        now: now.raw(),
        tz_s: 0,
        sid: String::new(),
        sock: None,
        config: Some(config_path),
        claude_tmp: None,
        claude_running: None,
    };
    let shown = local_shows(&env, &read_local_source_text);
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
/// checks it against. Only the local fleet projection receives discovered additions;
/// the captured source remains unchanged.
fn capture_init_fleet(
    env: &Env,
    selected: &SelectedSource,
    merge: bool,
) -> Result<(String, FleetSnapshot), String> {
    let (text, at) = match selected {
        SelectedSource::Beside { path, .. } => (
            read_local_source_text(path).map_err(|why| format!("{}: {why}", path.display()))?,
            SnapshotAt::File(path.clone()),
        ),
        SelectedSource::Pool(entry)
        | SelectedSource::VaultFleet { entry }
        | SelectedSource::Sheet { entry } => (
            entry
                .text()
                .map_err(|why| format!("{}: {why}", entry.source()))?,
            SnapshotAt::Entry(entry.name.clone()),
        ),
        SelectedSource::Key(_) | SelectedSource::Absent => {
            return Err("no local account source was selected".to_owned());
        }
    };
    let mut answer = text.clone();
    if merge {
        let mut fleet = FleetRoster::from_toml(&text, &at.to_string())?;
        let opt_in = opt_in_of(&text);
        let mut shown = None;
        if fleet_takes_discovered(opt_in, &fleet, || {
            shown
                .get_or_insert_with(|| local_shows(env, &read_local_source_text))
                .clone()
        }) && let Some(local) = discovered_fleet(
            env,
            shown.get_or_insert_with(|| local_shows(env, &read_local_source_text)),
            1,
        ) && merge_discovered_accounts(&mut fleet, local)?
        {
            answer = fleet.to_toml_for(opt_in);
        }
    }
    Ok((
        answer,
        FleetSnapshot {
            at,
            bytes: Some(text.into_bytes()),
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
        .map_err(SyncError::into_message)
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
    /// The rows it left with no login made here that no source gives other Macs alone (its
    /// `needs_renew`), which the automatic
    /// sign-in ladder signs in ([`resync_with`]).
    to_sign_in: Vec<String>,
}

/// [`sync_accounts_for_init`], running `before_sync` once its sources are captured and reading
/// any source it did not capture (a `[fleet] accounts_source`) with `fetch`.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSourcePreservation",
        action = "Sync",
        project = "aterm_agent::harness::accounts_sync::source_preservation_tests::transition"
    )
)]
fn sync_accounts_for_init_with(
    home: &Path,
    state_dir: &Path,
    roster_path: &Path,
    now: i64,
    before_sync: impl FnOnce(),
    fetch_uncaptured: &Fetch<'_>,
) -> Result<InitSynced, SyncError> {
    let config_path = config_beside(roster_path);
    if accounts_sync_off(Some(&config_path)) {
        return Err(ACCOUNTS_SYNC_OFF.into());
    }
    let recovered_commit =
        accounts::Store::at(state_dir).recover_sync_within(&sync_lock_ladder(), roster_path)?;
    let automatic = AutomaticSource::capture(&config_path)?;
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
        xdg_config_home: None,
        codex_login: None,
        now,
        tz_s: 0,
        sid: String::new(),
        sock: None,
        config: Some(config_path),
        claude_tmp: None,
        claude_running: None,
    };
    let chosen = SourceRequest::Automatic {
        candidate: None,
        admission: &automatic,
    }
    .chosen(Cli::Claude, roster_path, env.config.as_deref())?;
    // A lab not read yet captures and reads no vault entry: its sync keeps the roster as it is.
    if chosen.pending().is_some() {
        before_sync();
        let synced = sync_accounts_request(
            &env,
            SourceRequest::Automatic {
                candidate: None,
                admission: &automatic,
            },
            None,
            &SyncHow {
                fetch: fetch_uncaptured,
                ladder: &sync_lock_ladder(),
                fleet_snapshots: &[],
                heal: None,
                allow_empty_pool: true,
            },
        )?
        .into_init(recovered_commit);
        return Ok(synced);
    }
    let native = chosen.lab();
    let policy = env.accounts_store().source_policy()?;
    let selected = if native.is_none() {
        source_selection::select_observed(
            Some(home),
            env.config.as_deref(),
            Some(roster_path),
            SourceScope::Init,
            &policy,
        )?
    } else {
        SelectedSource::Absent
    };
    let retire_legacy = native.is_some() || selected.retires_legacy(env.config.as_deref());
    let automatic = automatic.with_policy(policy, retire_legacy);
    let automatic = if native.is_some() {
        automatic
    } else {
        automatic.with_selection(&env, roster_path, SourceScope::Init, selected.clone())
    };
    if let InitAccountsAssignment::Unassigned {
        machine,
        fleet_accounts,
    } = init_accounts_assignment_from(&env, &selected.origin())?
    {
        before_sync();
        let store = env.accounts_store();
        let held = store
            .acquire_within(&sync_lock_ladder())
            .map_err(|why| why.to_string())?
            .ok_or_else(|| SyncError::retry(Retry::Busy, accounts::LOCK_HELD))?;
        let recovered = store.recover_sync_held(&held, roster_path)?;
        automatic.validate()?;
        if init_accounts_assignment_from(&env, &selected.origin())?
            != (InitAccountsAssignment::Unassigned {
                machine: machine.clone(),
                fleet_accounts,
            })
        {
            return Err(SyncError::retry(
                Retry::Changed,
                "account assignment changed after capture; sync again",
            ));
        }
        let retired = if automatic.retires_legacy() {
            let source = if native.is_some() {
                Some(automatic.native_guard()?)
            } else {
                None
            };
            store.retire_source_admitted_held(&held, now, source)?
        } else {
            false
        };
        return Ok(InitSynced {
            machine,
            rows: 0,
            enrolled: 0,
            fleet: fleet_accounts,
            changed: recovered_commit || recovered || retired,
            to_sign_in: Vec::new(),
        });
    }
    // Where the lab is the source no vault entry is captured or read.
    if native.is_some() {
        before_sync();
        let report = sync_accounts_request(
            &env,
            SourceRequest::Automatic {
                candidate: None,
                admission: &automatic,
            },
            None,
            &SyncHow {
                fetch: fetch_uncaptured,
                ladder: &sync_lock_ladder(),
                fleet_snapshots: &[],
                heal: None,
                allow_empty_pool: true,
            },
        );
        let synced = report?.into_init(recovered_commit);
        if synced.changed {
            let _ = accounts::Store::at(state_dir).touch_wake();
        }
        return Ok(synced);
    }
    let fleet_path = roster_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(FLEET_ROSTER_FILE);
    let mut captured_fleet = None;
    let mut fleet_snapshots = Vec::new();
    let init_source = if matches!(selected, SelectedSource::Key(_)) {
        None
    } else if let Some(source) = selected.source() {
        let merge = matches!(&selected, SelectedSource::Beside { opt_in: Some(opt_in), .. } if opt_in.opts_in_by_file());
        let (text, snapshot) = capture_init_fleet(&env, &selected, merge)?;
        captured_fleet = Some((source.clone(), text));
        fleet_snapshots.push(snapshot);
        Some(source)
    } else {
        let min_accounts = if vault_mailbox_held(home)
            || (crate::vault_entry::configured(home)
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
                at: SnapshotAt::File(fleet_path.clone()),
                bytes: None,
            });
            source
        })
    };
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
    let report = sync_accounts_request(
        &env,
        SourceRequest::Automatic {
            candidate: init_source.as_deref(),
            admission: &automatic,
        },
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
    if !matches!(
        lab_chosen(Cli::Claude, roster_path, &config_path),
        Chosen::Unnamed
    ) || !matches!(
        source_selection::select_for(
            &accounts::Store::at(state_dir),
            Some(home),
            Some(&config_path),
            Some(roster_path),
            SourceScope::Init
        ),
        Ok(SelectedSource::Pool(_))
    ) {
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
    !trace::fs::exists(roster_path)
        && has_claude_resync_source(home, state_dir, roster_path, now)
        && !empty_claude_pool_settled(home, state_dir, roster_path, now)
        && !claude_unassigned(home, state_dir, roster_path, now)
}

fn claude_unassigned(
    home: &Path,
    state_dir: &Path,
    roster_path: &Path,
    now: impl Into<trace::Now>,
) -> bool {
    let now: trace::Now = now.into();
    let config = config_beside(roster_path);
    let env = traced_env(home, state_dir, &config, now);
    matches!(
        init_accounts_assignment(&env),
        Ok(InitAccountsAssignment::Unassigned { .. })
    )
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
    let mut cooldown = claude_roster_missing(home, state_dir, roster_path, now)
        .then(|| SyncCooldownClaim::take(fx.sync_cooldown, fx.scope.as_ref()))
        .flatten();
    let missing = cooldown.is_some();
    let mut scheduling = SyncScheduleClaim::new(fx.rediscovery, fx.scope.as_ref());
    let mut done = Resync::default();
    if (missing
        || sync_called_for_recording(
            home,
            state_dir,
            (roster_path, &config),
            now,
            fx.rediscovery,
            &mut scheduling,
        ))
        && let Some((synced, _)) =
            periodic_sync(home, state_dir, (roster_path, trace::Now::at(now)), fx)
    {
        scheduling.outcome(&synced);
        if let Some(cooldown) = &mut cooldown {
            cooldown.outcome(&synced);
        }
        done.synced = true;
        match synced {
            Ok(synced) => {
                done.changed = synced.changed;
                done.to_sign_in = synced.to_sign_in;
            }
            Err(why) => done.failed = Some(why.into_message()),
        }
    }
    for (cli, synced) in sync_cli_accounts_with(home, state_dir, roster_path, now, fx.fetch) {
        match synced {
            Ok(CliSync::Synced(synced)) => {
                done.synced = true;
                done.changed |= synced.changed;
            }
            Ok(CliSync::NoSourceDefaultLogin(login)) if login.reassigned.is_some() => {
                done.synced = true;
                done.changed = true;
            }
            Ok(
                CliSync::NoSourceDefaultLogin(_)
                | CliSync::LabPending { .. }
                | CliSync::Off
                | CliSync::Left { .. }
                | CliSync::NoSource
                | CliSync::Unlisted { .. },
            ) => {}
            Err(why) => done.cli_failed.push((cli, why)),
        }
    }
    if sign_in && sign_ins_on(&config, Cli::Claude) && !done.to_sign_in.is_empty() {
        done.renewal = auto_renew_next_account(home, state_dir, roster_path, now, fx);
    }
    done
}

/// What [`read_resync`] has read in this process: kept apart from the syncs' own
/// ([`REDISCOVERY`]), so a read never uses up a sync's turn, and kept across a converge pass's
/// probe and its read-back, so the read after a sync reads what that sync left.
static RESYNC_READ: Watched<Rediscovery> = Watched::new(
    "accounts_sync/resync_read",
    Rediscovery {
        read_at: None,
        synced_for: None,
        key_attempt: None,
        failed: None,
        sheet_said: Vec::new(),
    },
);

/// One read-only setup probe's source availability and work, with no sync or sign-in started.
#[derive(Debug, PartialEq, Eq)]
pub enum ResyncRead {
    /// No source is configured or found, including default pools this machine left or is not in.
    NoSource,
    /// The available sources and local account records need no sync.
    Current,
    /// A sync is due, including a selected source or local record whose read is refused.
    Due(String),
}

/// Read availability and due work together. The caller still checks its own automatic policy
/// before this read; the due rule also reads the configuration beside the roster as usual.
#[must_use]
pub fn read_resync(home: &Path, state_dir: &Path, roster_path: &Path, now: i64) -> ResyncRead {
    read_resync_with(home, state_dir, roster_path, now, &RESYNC_READ)
}

fn read_resync_with(
    home: &Path,
    state_dir: &Path,
    roster_path: &Path,
    now: i64,
    rediscovery: &Watched<Rediscovery>,
) -> ResyncRead {
    let config = config_beside(roster_path);
    let env = quiet_env(home, state_dir, &config, now);
    let mut sources = ResyncSources::new(&env, home);
    let mut pools = Vec::new();
    let mut due_allowed = None;
    let mut available = has_init_accounts_source(home, state_dir, roster_path, now);
    if !available {
        for cli in OTHER_CLIS {
            let pool =
                read_cli_resync(cli, state_dir, &config, now, &mut sources, &mut due_allowed);
            available = pool.available;
            pools.push(pool);
            if available {
                break;
            }
        }
    }
    if !available {
        return ResyncRead::NoSource;
    }
    // The due policy uses the configuration beside the roster. The application's
    // supplied-configuration Off check already ran before source availability.
    if !*due_allowed.get_or_insert_with(|| !accounts_sync_off(Some(&config))) {
        return ResyncRead::Current;
    }
    let mut why = claude_resync_due(home, state_dir, roster_path, now, rediscovery);
    for (index, cli) in OTHER_CLIS.into_iter().enumerate() {
        if pools.len() <= index {
            pools.push(read_cli_resync(
                cli,
                state_dir,
                &config,
                now,
                &mut sources,
                &mut due_allowed,
            ));
        }
        add_cli_resync_due(&mut why, cli, &pools[index].due);
    }
    if why.is_empty() {
        ResyncRead::Current
    } else {
        ResyncRead::Due(why.join("; "))
    }
}

struct CliResyncRead {
    available: bool,
    due: Result<Vec<String>, String>,
}

fn read_cli_resync(
    cli: Cli,
    state_dir: &Path,
    config: &Path,
    now: i64,
    sources: &mut ResyncSources<'_>,
    due_allowed: &mut Option<bool>,
) -> CliResyncRead {
    let absent = || CliResyncRead {
        available: false,
        due: Ok(Vec::new()),
    };
    let refused = |why| CliResyncRead {
        available: true,
        due: Err(why),
    };
    let Some(roster) = accounts::roster_beside_for(config, cli) else {
        return CliResyncRead {
            available: false,
            due: Err(format!("{} has no directory", config.display())),
        };
    };
    let by_default = !trace::fs::is_file(&roster);
    if by_default && !syncs_by_default(cli) {
        return absent();
    }
    let store = accounts::Store::at_for(state_dir, cli, sources.home);
    if store.sync_needs_recovery() {
        return CliResyncRead {
            available: true,
            due: Ok(vec!["recovery".to_owned()]),
        };
    }
    let loaded = match sources.load(cli) {
        Ok(Some(loaded)) => loaded,
        Ok(None) => return absent(),
        Err(why) => return refused(why),
    };
    let (fleet, pool, machine) = loaded;
    let machine = if by_default {
        match by_default_admits(&store, &fleet, machine, now) {
            Ok(Ok(machine)) => machine,
            Ok(Err(_)) => return absent(),
            Err(why) => return refused(why),
        }
    } else {
        match machine {
            Some(machine) => machine,
            None => {
                return refused(
                    "could not determine machine id (`m<N>`); pass `--machine m8`".to_owned(),
                );
            }
        }
    };
    // Availability admits only the source/default pool. Preserve the due-side Off boundary
    // before reading a kept roster or its account state, including with a different caller config.
    if !*due_allowed.get_or_insert_with(|| !accounts_sync_off(Some(config))) {
        return CliResyncRead {
            available: true,
            due: Ok(Vec::new()),
        };
    }
    CliResyncRead {
        available: true,
        due: cli_roster_due(cli, &store, &roster, now, &fleet, pool.as_ref(), &machine),
    }
}

fn claude_resync_due(
    home: &Path,
    state_dir: &Path,
    roster_path: &Path,
    now: i64,
    rediscovery: &Watched<Rediscovery>,
) -> Vec<String> {
    // Read whether or not the roster stands, so the read after a sync that wrote it compares
    // with this one rather than reading every input as new. Claude discovery and scheduling
    // keep their existing reads; ResyncSources only captures the other CLIs' selected inputs.
    let called = sync_called_for(
        home,
        state_dir,
        (roster_path, &config_beside(roster_path)),
        now,
        rediscovery,
    );
    if claude_roster_missing(home, state_dir, roster_path, now) {
        vec!["the roster is missing".to_owned()]
    } else if called {
        vec!["a source it is made from changed or is due a fresh read".to_owned()]
    } else {
        Vec::new()
    }
}

fn add_cli_resync_due(why: &mut Vec<String>, cli: Cli, due: &Result<Vec<String>, String>) {
    match due {
        Ok(added) if added.is_empty() => {}
        Ok(added) => why.push(format!(
            "{}'s pool has account changes ({})",
            cli.id(),
            added.join(", ")
        )),
        Err(error) => why.push(format!("{}'s pool cannot be synced: {error}", cli.id())),
    }
}

fn cli_roster_due<
    P: std::borrow::Borrow<super::ai_pool::Pool>,
    I: std::borrow::Borrow<super::ai_pool::LocalPoolIdentity>,
>(
    cli: Cli,
    store: &accounts::Store,
    roster_path: &Path,
    now: i64,
    fleet: &CliFleet,
    pool_ctx: Option<&PoolSyncCtx<P, I>>,
    machine: &str,
) -> Result<Vec<String>, String> {
    let (read_from, _) = super::cli::roster_target(roster_path)?;
    let existing_text = accounts::roster_text(&read_from)?.unwrap_or_default();
    let existing = Roster::parse_for(cli, &existing_text)
        .map_err(|why| format!("the roster at {} is refused: {why}", roster_path.display()))?;
    let mut added: Vec<String> = fleet
        .roster
        .accounts_for_machine(machine)
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
    if existing.rows.is_empty() {
        first_sync_own_row(cli, &fleet.roster, store, &mut Vec::new(), &mut added);
    }
    if fleet.roster.alab.is_some() {
        let mut projected = existing.rows.clone();
        if rows_under_source(cli, &fleet.roster, None, &mut projected) {
            for (at, row) in projected.iter().enumerate() {
                if existing.rows.get(at) != Some(row) && !added.contains(&row.label) {
                    added.push(row.label.clone());
                }
            }
        }
    }
    if added.is_empty()
        && let Some(ctx) = pool_ctx
    {
        for row in &existing.rows {
            if (row.email.is_some() || row.identity_email.is_some())
                && row.held_by != ctx.holder_for_row(cli, row)
            {
                added.push(row.label.clone());
            }
        }
        if added.is_empty() && (!existing.rows.is_empty() || trace::fs::exists(&store.dir)) {
            let identity = ctx.identity.borrow();
            let home_accounts: BTreeSet<String> = existing
                .rows
                .iter()
                .filter(|row| row.held_by.is_none() && ctx.is_home_for_row(cli, row))
                .map(|row| row.label.clone())
                .collect();
            let (state, _) = store
                .load(now)
                .map_err(|why| format!("{}: {why}", store.dir.display()))?;
            if state.swarm_label.as_deref() != Some(identity.primary.as_str())
                || state.swarm_aliases != identity.aliases
                || state.home_accounts != home_accounts
                || pool_execution_pending(&state, &existing.rows, cli, ctx)
            {
                added.push(identity.primary.clone());
            }
        }
    }
    Ok(added)
}

/// The vault entry holding the accounting sheet (`DESIGN-secrets-2026-10-20.md` §11;
/// [`aterm_secrets::entry_class::AI_ACCOUNTS_CSV`]), read in place from this Mac's store.
pub const ACCOUNTS_CSV_FILE: &str = aterm_secrets::entry_class::AI_ACCOUNTS_CSV;

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

/// The vault entry holding `cli`'s fleet (`ai-accounts-codex.toml`, `ai-accounts-grok.toml`,
/// `ai-accounts-kimi.toml`), which aterm reads in place from this Mac's store and never writes
/// ([`sync_cli_with`]).
#[must_use]
pub fn cli_fleet_file(cli: Cli) -> String {
    aterm_secrets::entry_class::cli_accounts(cli.id())
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
        /// The first row the sync would add that the store still holds records of, by its label,
        /// which a roster restored to rejoin must write (`docs/DESIGN-shared-files-2026-11-11.md`
        /// §10 leaves the re-key of those records to SF-10).
        label: String,
        /// The region and address the source lists that row's account at, which a message names
        /// it by ([`Naming::listed_shown`]); `None` for this machine's own login's row (`main`),
        /// which the sync adds with no address.
        listed: Option<(Option<Region>, String)>,
    },
    /// Opted in or synced by default, and no fleet of the CLI's is there to read: neither its
    /// fleet file ([`cli_fleet_file`]) nor its column of the sheet ([`ACCOUNTS_CSV_FILE`]).
    NoSource,
    /// [`Self::NoSource`], with a default login that names another account than its row: what the
    /// sync came to of that, which takes no source ([`reassign_codex_default`]).
    NoSourceDefaultLogin(Box<DefaultLogin>),
    /// The lab may name this Mac and is not read yet ([`super::alab_roster::Chosen::Pending`]),
    /// for the reason `why`: no source was read and the roster stands as it is, no row moved on
    /// or off the default login among them; `login`, where that login names another account than
    /// its row, says which row it leaves there ([`sync_without_source`], whose move waits for the
    /// lab: [`default_move_asked`]).
    LabPending {
        /// Why the lab is not read.
        why: String,
        /// What following the default login came to.
        login: Option<Box<DefaultLogin>>,
    },
    /// Synced by default with no roster kept here, and neither a preferred account nor a
    /// registry-known native default login belongs here, or no machine names this Mac:
    /// nothing was written, not even a lock (but
    /// for the recovery of an interrupted sync of the pool's own, which runs first).
    Unlisted {
        /// The machine the fleet was read for, where one is known.
        machine: Option<String>,
        /// Where the fleet came from.
        source: String,
        /// The sheet's conflicts this machine's pool skips ([`FleetRoster::conflicts_for`]),
        /// often why it lists none.
        conflicts: Vec<SheetConflict>,
    },
    /// The fleet was read.
    Synced(Box<CliSynced>),
}

/// A sync that read the fleet ([`CliSync::Synced`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliSynced {
    /// The CLI.
    pub cli: Cli,
    /// The native registry key, or legacy machine label, the rows were taken for.
    pub machine: String,
    /// The registry display label captured with the key; presentation only.
    pub machine_label: Option<String>,
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
    /// The Kimi Code region and the address of each fleet row [`Self::held`] and [`Self::kept`]
    /// name, by its label: the account its receipt names each by, since a label is a key no
    /// receipt prints.
    pub listed: BTreeMap<String, (Option<Region>, String)>,
    /// The sheet's conflicts this machine's pool skips ([`FleetRoster::conflicts`]).
    pub conflicts: Vec<SheetConflict>,
    /// Whether the sync changed the roster, account state or a recovered staging renewal.
    pub changed: bool,
    /// The row this sync discovers on the default login, either on first sync or after a
    /// checked Codex default-login reassignment. Its login file already exists.
    pub own_row: Option<String>,
    /// Whether the fleet is the lab's and this Mac prefers no account of the CLI: the sync
    /// added nothing and kept every row (§0: no preference limits nothing).
    pub prefers_none: bool,
    /// What the sync did to rows it kept: holds it cleared or set, orders it changed.
    pub rows_changed: RowChanges,
    /// Each account the lab prefers here that the sync left out because a row of another account
    /// holds its label: `(label, address)` ([`FleetRoster::clashes`]).
    pub clashes: Vec<(String, String)>,
    /// What it read of the default login and did about it ([`DefaultLogin`]).
    pub default_login: DefaultLogin,
}

/// [`quiet_env`] for a run the accounts keeper may trace, at the reading `now`, which no run
/// under it reads raw: its `now` holds the reading as a number, taken unrecorded
/// ([`trace::unrecorded`]). That is sound because each reader of an [`Env`]'s `now` these runs
/// reach hands it on to a comparison through [`trace::Now`] (a store's load, a login's state),
/// which records what it compared exactly as the reading itself would, or to a write, whose
/// effect keeps the run's verdict from being kept. The readers that make one
/// ([`claude_unassigned`], [`default_login_unlisted`], [`key_file_changed`], [`sync_inputs`],
/// [`rows_of_other_macs_at`] and Codex's default login's completion) read sources, rosters and
/// profiles under it, which read no `now` at all; [`sync_cli_with_replan`]'s sync
/// ([`sync_cli_in_env`]) reads it in [`reassign_codex_default`] alone (a load, then its write).
/// Audited 2026-11-16 by renaming the field and reading each of the lib's readers the compiler
/// named. A reader these runs reach
/// that compares `env.now` as a number takes the reading raw instead ([`trace::Now::raw`]).
fn traced_env(home: &Path, state_dir: &Path, config: &Path, now: trace::Now) -> Env {
    quiet_env(home, state_dir, config, trace::unrecorded(|| now.raw()))
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
        xdg_config_home: None,
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
/// reads), else the vault's [`cli_fleet_file`], else its [`ACCOUNTS_CSV_FILE`] where the header
/// names `cli`'s column ([`csv_names_column`]); `None` where none is there. Nothing is written.
fn load_cli_fleet(
    cli: Cli,
    env: &Env,
    home: &Path,
    source: Option<&str>,
    fetch: &Fetch<'_>,
) -> Result<Option<CliFleet>, String> {
    load_cli_fleet_source(cli, env, home, source, None, fetch).map(|res| res.map(|(f, _, _)| f))
}

/// A fleet [`load_cli_fleet_source`] loaded: the fleet, the pool and local identity where its
/// source is a pool, and the machine it was read for.
type LoadedFleet<P = super::ai_pool::Pool, I = super::ai_pool::LocalPoolIdentity> =
    (CliFleet, Option<PoolSyncCtx<P, I>>, Option<String>);

/// The vault entry `cli`'s pool syncs from by default ([`local_cli_source_observed`]), as this
/// Mac's store read it when it was selected.
#[derive(Clone, Debug, PartialEq, Eq)]
struct LocalCliSource {
    entry: VaultSource,
    csv: bool,
}

fn local_cli_source(cli: Cli, env: &Env, home: &Path) -> Result<Option<LocalCliSource>, String> {
    let policy = accounts::Store::at_for(&env.state, cli, home).source_policy()?;
    Ok(local_cli_source_observed(cli, home, &policy))
}

/// `cli`'s default source, in order: the vault's pool, its `ai-accounts-<cli>.toml` (and, for
/// Claude Code, its fleet roster) unless the retained policy retired them, else its sheet where
/// this Mac reads it: all from one version of the vault, and an entry this Mac cannot read now
/// keeps its place, so its sync says why rather than a lower source standing in for it.
fn local_cli_source_observed(
    cli: Cli,
    home: &Path,
    policy: &accounts::source_policy::Snapshot,
) -> Option<LocalCliSource> {
    let toml = cli_fleet_file(cli);
    let names = [
        super::ai_pool::AI_POOL_FILE,
        toml.as_str(),
        FLEET_ROSTER_FILE,
        ACCOUNTS_CSV_FILE,
    ];
    let mut reads = crate::vault_entry::read_set(home, &names, SOURCE_TEXT_MAX_BYTES)
        .into_iter()
        .zip(names)
        .map(|(read, name)| VaultSource {
            name: name.to_owned(),
            read,
        });
    let (pool, toml, claude, csv) = (reads.next()?, reads.next()?, reads.next()?, reads.next()?);
    let (entry, csv) = if pool.occupies() {
        (pool, false)
    } else if !policy.retired() && toml.occupies() {
        (toml, false)
    } else if cli == Cli::Claude && !policy.retired() && claude.occupies() {
        (claude, false)
    } else if sheet_read_here() && csv.occupies() {
        (csv, true)
    } else {
        return None;
    };
    Some(LocalCliSource { entry, csv })
}

/// `cli`'s lab fleet where the lab names this Mac ([`super::alab_roster::chosen`]), `None` where
/// it does not, so `cli`'s own sources are read.
///
/// # Errors
/// [`super::alab_roster::SOURCE`] was named and the lab names no machine here; or the lab is not
/// read yet ([`Chosen::Pending`], its words): no other source stands in for it meanwhile.
fn cli_lab_source(cli: Cli, env: &Env, named: Option<&str>) -> Result<Option<CliFleet>, String> {
    let roster = env
        .config
        .as_deref()
        .and_then(|config| accounts::roster_beside_for(config, cli));
    match super::alab_roster::chosen(cli, roster.as_deref(), named, env.config.as_deref())? {
        Chosen::Lab(roster) => Ok(Some(CliFleet {
            cli,
            roster,
            held: Vec::new(),
        })),
        Chosen::Unnamed => Ok(None),
        Chosen::Pending(why) => Err(why),
    }
}

/// Why the lab is not read yet for `cli`'s pool ([`Chosen::Pending`]), where it is not: a sync of
/// that pool then keeps its roster as it stands, and its table lists that roster alone.
#[must_use]
pub fn lab_pending(cli: Cli, env: &Env) -> Option<String> {
    let roster = env.accounts_roster_for(cli);
    match super::alab_roster::chosen(cli, roster.as_deref(), None, env.config.as_deref()) {
        Ok(Chosen::Pending(why)) | Err(why) => Some(why),
        Ok(Chosen::Lab(_) | Chosen::Unnamed) => None,
    }
}

fn parse_cli_source(cli: Cli, chosen: &str, raw: &str) -> Result<CliFleet, String> {
    if chosen.ends_with(".toml") || raw.trim_start().starts_with('#') || raw.contains("[[account]]")
    {
        Ok(CliFleet {
            cli,
            roster: FleetRoster::from_toml_for(cli, raw, chosen)?,
            held: Vec::new(),
        })
    } else if sheet_read_here() {
        parse_accounting_csv_for(cli, raw, chosen)
    } else {
        Err(sheet_refused(chosen, cli))
    }
}

struct CapturedLocalSource {
    raw: String,
    pool: Option<Result<Option<super::ai_pool::Pool>, String>>,
}

/// Only one availability/due operation owns these observations. They confer no sync authority:
/// every later probe and every applying sync reopens its inputs through their ordinary paths.
/// Claude's discovery/scheduler reads are deliberately outside this OTHER_CLIS capture.
struct ResyncSources<'a> {
    env: &'a Env,
    home: &'a Path,
    local: BTreeMap<String, Result<CapturedLocalSource, String>>,
    identity: Option<Result<Option<super::ai_pool::LocalPoolIdentity>, String>>,
}

impl<'a> ResyncSources<'a> {
    fn new(env: &'a Env, home: &'a Path) -> Self {
        Self {
            env,
            home,
            local: BTreeMap::new(),
            identity: None,
        }
    }

    fn load(
        &mut self,
        cli: Cli,
    ) -> Result<
        Option<LoadedFleet<&super::ai_pool::Pool, &super::ai_pool::LocalPoolIdentity>>,
        String,
    > {
        if lab_pending(cli, self.env).is_some() {
            // A lab not read yet gives this pool no source: its sync keeps the roster.
            return Ok(None);
        }
        if let Some(fleet) = cli_lab_source(cli, self.env, None)? {
            let machine = fleet
                .roster
                .alab
                .as_ref()
                .map(|machine| machine.key.as_str().to_owned());
            return Ok(Some((fleet, None, machine)));
        }
        let Some(selected) = local_cli_source(cli, self.env, self.home)? else {
            return Ok(None);
        };
        let chosen = selected.entry.source();
        let captured = self
            .local
            .entry(selected.entry.name.clone())
            .or_insert_with(|| {
                selected
                    .entry
                    .text()
                    .map(|raw| CapturedLocalSource { raw, pool: None })
                    .map_err(|why| format!("{chosen}: {why}"))
            });
        let captured = captured.as_mut().map_err(|why| why.clone())?;
        if selected.csv && !csv_names_column(cli, &captured.raw) {
            return Ok(None);
        }
        let pool = captured.pool.get_or_insert_with(|| {
            if super::ai_pool::Pool::looks_like_pool(&chosen, &captured.raw) {
                super::ai_pool::Pool::from_toml(&captured.raw, &chosen).map(Some)
            } else {
                Ok(None)
            }
        });
        if let Some(pool) = pool.as_ref().map_err(Clone::clone)? {
            let identity = self
                .identity
                .get_or_insert_with(|| {
                    super::ai_pool::resolve_local_pool_identity(
                        Some(self.home),
                        self.env.config.as_deref(),
                        None,
                    )
                })
                .as_ref()
                .map_err(Clone::clone)?;
            let machine = identity.as_ref().map(|identity| identity.primary.clone());
            let roster = project_pool_fleet(pool, cli, identity.as_ref());
            return Ok(Some((
                CliFleet {
                    cli,
                    roster,
                    held: Vec::new(),
                },
                identity
                    .as_ref()
                    .map(|identity| PoolSyncCtx { pool, identity }),
                machine,
            )));
        }
        Ok(Some((
            parse_cli_source(cli, &chosen, &captured.raw)?,
            None,
            machine_for(self.env.config.as_deref(), None),
        )))
    }
}

fn load_cli_fleet_source(
    cli: Cli,
    env: &Env,
    home: &Path,
    source: Option<&str>,
    machine_override: Option<&str>,
    fetch: &Fetch<'_>,
) -> Result<Option<LoadedFleet>, String> {
    if let Some(fleet) = cli_lab_source(cli, env, source)? {
        let machine = fleet
            .roster
            .alab
            .as_ref()
            .map(|machine| machine.key.as_str().to_owned());
        return Ok(Some((fleet, None, machine)));
    }
    let (chosen, raw) = match source.map(str::trim).filter(|s| !s.is_empty()) {
        Some(named) => (named.to_owned(), fetch(env, named)?),
        None => {
            let Some(selected) = local_cli_source(cli, env, home)? else {
                return Ok(None);
            };
            let chosen = selected.entry.source();
            let raw = selected
                .entry
                .text()
                .map_err(|why| format!("{chosen}: {why}"))?;
            if selected.csv && !csv_names_column(cli, &raw) {
                return Ok(None);
            }
            (chosen, raw)
        }
    };
    if super::ai_pool::Pool::looks_like_pool(&chosen, &raw) {
        let pool = super::ai_pool::Pool::from_toml(&raw, &chosen)?;
        let identity = super::ai_pool::resolve_local_pool_identity(
            Some(home),
            env.config.as_deref(),
            machine_override,
        )?;
        let machine = identity.as_ref().map(|id| id.primary.clone());
        let roster = project_pool_fleet(&pool, cli, identity.as_ref());
        return Ok(Some((
            CliFleet {
                cli,
                roster,
                held: Vec::new(),
            },
            identity.map(|identity| PoolSyncCtx { pool, identity }),
            machine,
        )));
    }
    let fleet = parse_cli_source(cli, &chosen, &raw)?;
    let machine = machine_for(env.config.as_deref(), machine_override);
    Ok(Some((fleet, None, machine)))
}

/// A local source snapshot for missing default-login metadata. Keep complete pool bytes:
/// projecting only warm rows would lose cold-account holds and policy-only changes.
#[derive(Debug, PartialEq)]
pub(super) struct CodexCompletionSource {
    pub fleet: Option<CliFleet>,
    pub held_by: Option<String>,
    pool_text: Option<String>,
    pub(super) guard: Option<super::alab_roster::ReceiptGuard>,
}

/// What the local source says of a Codex row's metadata ([`codex_completion_read`]): the source,
/// or why the lab, which may name this Mac, is not read yet, so no source vouches for it.
#[derive(Debug, PartialEq)]
pub(super) enum CodexCompletion {
    /// The source as read.
    Read(CodexCompletionSource),
    /// The lab is not read yet ([`Chosen::Pending`]), in these words.
    LabPending(String),
}

/// [`codex_completion_read`] for the callers that act on an account's custody, metadata or
/// receipt: a lab not read yet vouches for no account's, so that is a refusal, in its words.
///
/// # Errors
/// As [`codex_completion_read`], and a lab not read yet.
pub(super) fn codex_completion_source(
    env: &Env,
    row: &Row,
) -> Result<CodexCompletionSource, String> {
    match codex_completion_read(env, row)? {
        CodexCompletion::Read(source) => Ok(source),
        CodexCompletion::LabPending(why) => Err(why),
    }
}

/// Read only native local source files; no network or source publication is permitted. A lab
/// not read yet reads no source and answers why ([`CodexCompletion::LabPending`]): an automatic
/// follow of the default login or completion of its row then has nothing to do, while every
/// caller that acts on custody refuses ([`codex_completion_source`]).
///
/// # Errors
/// No home or Codex roster directory is known, or the source cannot be captured, read or
/// validated.
pub(super) fn codex_completion_read(env: &Env, row: &Row) -> Result<CodexCompletion, String> {
    let home = env.home.as_deref().ok_or("no home directory")?;
    let mut source_env = env.clone();
    let config = source_env
        .config
        .get_or_insert_with(|| env.state.join("aterm.toml"))
        .clone();
    let roster =
        accounts::roster_beside_for(&config, Cli::Codex).ok_or("no Codex roster directory")?;
    let store = accounts::Store::at_for(&env.state, Cli::Codex, home);
    let no_network =
        |_: &Env, _: &str| Err("metadata completion cannot fetch a remote source".to_owned());
    let captured = ProviderSource::capture(
        Cli::Codex,
        &source_env,
        &store,
        &roster,
        (None, None),
        false,
        &no_network,
    )
    .map_err(SyncError::into_message)?;
    if let Some(why) = captured.lab_pending() {
        return Ok(CodexCompletion::LabPending(why.to_owned()));
    }
    let loaded = captured
        .load(Cli::Codex, &roster)
        .map_err(SyncError::into_message)?;
    let pool_text = loaded.as_ref().and_then(|_| captured.pool_text());
    let mut held_by = None;
    if let Some(raw) = pool_text {
        let pool = super::ai_pool::Pool::from_toml(raw, super::ai_pool::AI_POOL_FILE)?;
        let identity = loaded
            .as_ref()
            .and_then(|(_, context, _)| context.as_ref())
            .map(|context| &context.identity);
        let matched = pool.accounts.iter().find(|account| {
            account.provider == Cli::Codex
                && row
                    .expected_identity_email()
                    .is_some_and(|email| accounts::email_matches(email, &account.email))
        });
        held_by = matched.and_then(|account| {
            if identity.is_some_and(|identity| account.is_warm_for(identity)) {
                None
            } else {
                Some(account.holder_word().to_owned())
            }
        });
    }
    captured
        .validate(Cli::Codex, &source_env)
        .map_err(SyncError::into_message)?;
    Ok(CodexCompletion::Read(CodexCompletionSource {
        fleet: loaded.map(|(fleet, _, _)| fleet),
        held_by,
        pool_text: pool_text.map(str::to_owned),
        guard: captured
            .receipt_guard()
            .map_err(SyncError::into_message)?
            .cloned(),
    }))
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

#[cfg(test)]
fn sync_cli_with(
    cli: Cli,
    paths: (&Path, &Path, &Path),
    source: (Option<&str>, Option<&str>),
    now: i64,
    reads: (&Fetch<'_>, &[Duration]),
) -> Result<CliSync, String> {
    sync_cli_with_replan(cli, paths, source, SyncCaller::Person, now, reads, || {})
        .map_err(SyncError::into_message)
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
/// preferred rows or a registry-known native default login of this machine's
/// ([`CliSync::Unlisted`] where it has neither, before any lock or
/// directory is made; an interrupted sync's recovery runs first, as for any sync), this
/// machine's fleet rows the roster does not list are added to it, each
/// with a login directory under `~/.aterm/logins/<cli>/` ([`accounts::Store::at_for`]), and a roster
/// with no rows first gets this machine's own login (`main`, no dir), as `accounts add` gives it.
/// It writes the roster and the CLI's store alone, never a vault entry; a row the roster has is
/// never changed or removed. A no-op requires exactly one identity match with the same label
/// and vendor settings; conflicting or ambiguous matches are left out ([`CliSynced::kept`]).
/// Under the CLI's store lock, waited for on `ladder`.
/// New labels require readable, unoccupied local state and an idle sign-in lock.
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::refines(
        machine = "AccountSyncNewLabels",
        action = "Sync",
        project = "aterm_agent::harness::accounts_sync::cli_pool_tests::new_label_tests::sync_transition"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::refines(
        machine = "AccountProviderRealm",
        action = "Sync",
        project = "aterm_agent::harness::accounts_sync::cli_pool_tests::provider_realm_tests::realm_transition"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::refines(
        machine = "AccountAliasAdmission",
        action = "Sync",
        project = "aterm_agent::harness::accounts_sync::cli_pool_tests::alias_admission_tests::alias_transition"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::refines(
        machine = "AccountProviderCommit",
        action = "Recover",
        project = "aterm_agent::harness::accounts_sync::cli_pool_tests::provider_commit_tests::transition"
    )
)]
fn sync_cli_with_replan(
    cli: Cli,
    (home, state_dir, config): (&Path, &Path, &Path),
    (source, machine_override): (Option<&str>, Option<&str>),
    caller: SyncCaller,
    now: impl Into<trace::Now>,
    (fetch, ladder): (&Fetch<'_>, &[Duration]),
    after_unlock: impl FnOnce(),
) -> Result<CliSync, SyncError> {
    let now: trace::Now = now.into();
    let env = traced_env(home, state_dir, config, now);
    sync_cli_in_env(
        cli,
        (home, state_dir, config),
        (source, machine_override),
        caller,
        now,
        (&env, fetch, ladder),
        after_unlock,
    )
}

/// Who started a pool's sync ([`sync_cli_in_env`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SyncCaller {
    /// A person's verb (`aterm harness accounts sync --cli`), or a test standing in for one.
    Person,
    /// `aterm init`'s accounts item, the setup pass's resync and the sync that follows a vault
    /// verb ([`sync_cli_accounts_with`]): no person named this pool; the first two report what
    /// the sync came to, and fail on its error.
    Init,
    /// The window's upkeep ([`upkeep_pools`]): no person named this pool, and nothing reads
    /// what the sync came to but whether the roster stands after it.
    Upkeep,
}

impl SyncCaller {
    /// Whether no person asked for this pool's sync: [`accounts_sync_off`] stops it, and its
    /// source is captured as an automatic one ([`ProviderSource::capture`]).
    fn automatic(self) -> bool {
        self != Self::Person
    }

    /// Which plans this caller's roster publication asks its source's receipt guard to
    /// admit ([`accounts::Admits`]). The upkeep's sync asks only for a plan that changes
    /// something: nothing reads what its sync came to but whether the roster stands after
    /// it, which "nothing changed" leaves as it was. So its "nothing changed" is the captured
    /// source's, not the registry's: it stands
    /// for as long as the source this process holds lags the registry or the registry
    /// refuses a strict read, and a later pass plans from a move only once its capture
    /// carries it. A person's verb and init's sync report what the sync came to, so their
    /// "nothing changed" comes from a source the guard admitted.
    fn admits(self) -> accounts::Admits {
        match self {
            Self::Person | Self::Init => accounts::Admits::EveryPlan,
            Self::Upkeep => accounts::Admits::Changes,
        }
    }
}

fn sync_cli_in_env(
    cli: Cli,
    (home, state_dir, config): (&Path, &Path, &Path),
    (source, machine_override): (Option<&str>, Option<&str>),
    caller: SyncCaller,
    now: impl Into<trace::Now>,
    (env, fetch, ladder): (&Env, &Fetch<'_>, &[Duration]),
    after_unlock: impl FnOnce(),
) -> Result<CliSync, SyncError> {
    let now: trace::Now = now.into();
    let automatic = caller.automatic();
    let product = cli.connector().product;
    if automatic && accounts_sync_off(env.config.as_deref()) {
        return Err(ACCOUNTS_SYNC_OFF.into());
    }
    let roster_path = accounts::roster_beside_for(config, cli)
        .ok_or_else(|| format!("{} has no directory", config.display()))?;
    let named = source.is_some_and(|s| !s.trim().is_empty());
    // With no local roster or explicit source, only a provider enabled for default sync
    // reads its fleet. An empty native assignment can still retire the legacy source.
    let by_default = !named && !trace::fs::is_file(&roster_path);
    if by_default && !syncs_by_default(cli) {
        return Ok(CliSync::Off);
    }
    let store = accounts::Store::at_for(state_dir, cli, home);
    let mut recovered_commit = store.recover_sync_within(ladder, &roster_path)?;
    let captured = ProviderSource::capture(
        cli,
        env,
        &store,
        &roster_path,
        (source, machine_override),
        automatic,
        fetch,
    )?;
    // A lab not read yet reads no source: the roster stands, and this Mac's own default login is
    // read as a sync with no source reads it, which moves no row while the lab is unread.
    if let Some(why) = captured.lab_pending() {
        let login = match sync_without_source(cli, (env, &store), &roster_path, ladder, now)? {
            CliSync::NoSourceDefaultLogin(login) => Some(login),
            _ => None,
        };
        return Ok(CliSync::LabPending {
            why: why.to_owned(),
            login,
        });
    }
    // Read-only absence and non-membership need no writer or new store directory.
    // Replacement retirement is a write, including an empty native/pool assignment.
    let Some((observed, _, observed_machine)) = captured.load(cli, &roster_path)? else {
        return sync_without_source(cli, (env, &store), &roster_path, ladder, now)
            .map_err(Into::into);
    };
    if by_default
        && let Err(instead) = by_default_admits(&store, &observed, observed_machine, now)?
        && (matches!(instead, CliSync::Left { .. }) || !captured.needs_retirement())
    {
        return Ok(instead);
    }
    let take_writer = || {
        store
            .acquire_within(ladder)
            .map_err(|why| format!("{}: {why}", store.dir.display()))?
            .ok_or_else(|| {
                SyncError::retry(Retry::Busy, format!(
                    "{} at {}: another `aterm harness accounts` or a window holds it; nothing was \
                 synced, so run the sync again",
                    accounts::LOCK_HELD,
                    store.dir.display()
                ))
            })
    };
    let mut lock = take_writer()?;
    let mut creation_signin = None;
    let mut after_unlock = Some(after_unlock);
    let roster_wrote;
    let state_wrote;
    let (
        write_to,
        total,
        added,
        kept,
        mut final_rows,
        mut own_row,
        rows_changed,
        mut default_login,
        fleet,
        machine,
        fleet_accounts,
        held,
        prefers_none,
    ) = loop {
        recovered_commit |= store.recover_sync_held(&lock, &roster_path)?;
        captured.validate(cli, env)?;
        let (write_to, keep_mode) = super::cli::roster_target(&roster_path)?;
        captured.protect(env, &write_to)?;
        let Some((fleet, pool_ctx, machine)) = captured.load(cli, &roster_path)? else {
            drop(lock);
            return sync_without_source(cli, (env, &store), &roster_path, ladder, now)
                .map_err(Into::into);
        };
        let retire = captured.needs_retirement();
        let machine = if !named && !trace::fs::is_file(&roster_path) {
            match by_default_admits(&store, &fleet, machine, now)? {
                Ok(machine) => machine,
                Err(instead) => {
                    captured.validate(cli, env)?;
                    if retire && !matches!(instead, CliSync::Left { .. }) {
                        // The retirement is written, stamped with the reading.
                        store.retire_source_admitted_held(
                            &lock,
                            now.raw(),
                            captured.receipt_guard()?,
                        )?;
                    }
                    return Ok(instead);
                }
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
        // Native empty assignments preserve local rows while clearing obsolete holds.
        let lab_fleet = fleet.roster.alab.is_some();
        if named && mine.is_empty() && !lab_fleet {
            return Err(with_conflicts(
                format!(
                    "no active {product} accounts found for machine `{machine}` in {}",
                    fleet.roster.source
                ),
                &fleet.roster.conflicts_for(Some(&machine)),
            )
            .into());
        }
        let held: Vec<String> = fleet
            .held
            .iter()
            .filter(|account| {
                let norm = normalize_machine_id(&machine).unwrap_or_else(|| machine.clone());
                account.machines.contains(&norm)
            })
            .map(|account| account.label.clone())
            .collect();
        let existing_image = accounts::roster_text(&write_to)?;
        let existing_text = existing_image.as_deref().unwrap_or_default();
        let existing = Roster::parse_for(cli, existing_text).map_err(|why| {
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
        let mut merged_existing = existing.rows.clone();
        let mut held_changed =
            rows_under_source(cli, &fleet.roster, pool_ctx.as_ref(), &mut merged_existing);
        let default_login = merge_default_twin(
            cli,
            &fleet.roster,
            &store,
            (&mut merged_existing, &mut rows, &mut added),
            now,
        )?;
        held_changed |= default_login.merged.is_some();
        if let Some(bound) = &fleet.roster.alab {
            held_changed |= super::alab_roster::order_rows(
                fleet.roster.lab_snapshot.as_deref(),
                cli,
                &bound.key,
                &mut merged_existing,
            );
        }
        let mut total = merged_existing.len();
        let mut own_row = None;
        let mut next_image = existing_image.clone();
        if merged_existing.is_empty() {
            own_row = first_sync_own_row(cli, &fleet.roster, &store, &mut rows, &mut added);
            if own_row.is_none() && !rows.is_empty() {
                rows.insert(
                    0,
                    Row {
                        label: ACCOUNTS_OWN_LABEL.to_owned(),
                        ..Row::default()
                    },
                );
            }
        }
        if !rows.is_empty() {
            let text = if held_changed || lab_fleet {
                let mut all_rows = merged_existing.clone();
                all_rows.extend(rows.iter().cloned());
                if let Some(bound) = &fleet.roster.alab {
                    super::alab_roster::order_rows(
                        fleet.roster.lab_snapshot.as_deref(),
                        cli,
                        &bound.key,
                        &mut all_rows,
                    );
                }
                accounts::append_rows_for(cli, "", &all_rows)
            } else {
                accounts::append_rows_for(cli, existing_text, &rows)
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
                            SyncError::retry(
                                Retry::Busy,
                                format!(
                                    "{}; no new account rows were synced",
                                    accounts::SIGNIN_HELD
                                ),
                            )
                        })?,
                );
                lock = take_writer()?;
                continue;
            }
            // New rows are written: their check compares the reading as a number.
            store.check_new_rows(&rows, &existing, now.raw())?;
            next_image = Some(text);
            total = merged_existing.len() + rows.len();
            merged_existing.extend(rows);
            if let Some(bound) = &fleet.roster.alab {
                super::alab_roster::order_rows(
                    fleet.roster.lab_snapshot.as_deref(),
                    cli,
                    &bound.key,
                    &mut merged_existing,
                );
            }
        } else if held_changed {
            let text = accounts::append_rows_for(cli, "", &merged_existing).map_err(|why| {
                format!("the synced rows do not admit: {why}; the roster is unchanged")
            })?;
            next_image = Some(text);
        }
        // Roster admission and the pool identity it describes share one pending receipt.
        // A crash must not leave a new roster paired with the previous machine or warm set.
        roster_wrote = next_image != existing_image;
        captured.validate(cli, env)?;
        captured.protect(env, &write_to)?;
        let target = accounts::SyncTarget {
            path: &write_to,
            before: existing_image.as_deref(),
            after: next_image.as_deref(),
            mode: keep_mode,
            fleet: None,
        };
        state_wrote = if let Some(ctx) = &pool_ctx {
            store.transact_roster_state_held(
                &lock,
                now,
                &mut None,
                target,
                (retire, |state| {
                    let home_accounts = merged_existing
                        .iter()
                        .filter(|row| row.held_by.is_none() && ctx.is_home_for_row(cli, row))
                        .map(|row| row.label.clone())
                        .collect();
                    apply_pool_execution(state, &merged_existing, cli, ctx);
                    let target_label = Some(ctx.identity.primary.clone());
                    if state.swarm_label != target_label
                        || state.swarm_aliases != ctx.identity.aliases
                        || state.home_accounts != home_accounts
                    {
                        state.swarm_label = target_label;
                        state.swarm_aliases.clone_from(&ctx.identity.aliases);
                        state.home_accounts = home_accounts;
                    }
                    Ok(Vec::new())
                }),
            )?
        } else {
            // The receipt also owns missing login directories. Equal roster
            // bytes alone do not establish that materialization is complete.
            let guard = captured.receipt_guard()?;
            if caller == SyncCaller::Upkeep && guard.is_some() {
                super::accounts_upkeep_counts::count(
                    super::accounts_upkeep_counts::Event::PoolSyncGuarded,
                );
            }
            store.transact_roster_admitted_held(
                &lock,
                now,
                &mut None,
                target,
                retire,
                (guard, caller.admits()),
            )?
        };
        let rows_changed = RowChanges::between(&existing.rows, &merged_existing);
        let prefers_none = lab_fleet && mine.is_empty();
        break (
            write_to,
            total,
            added,
            kept,
            merged_existing,
            own_row,
            rows_changed,
            default_login,
            fleet,
            machine,
            fleet_accounts,
            held,
            prefers_none,
        );
    };
    drop(lock);
    // Recovery takes the sign-in lock too and must see this publication ended.
    drop(creation_signin);
    let reassigned = follow_default_login(
        cli,
        (env, &store),
        (&roster_path, &write_to),
        ladder,
        fleet
            .roster
            .lab_snapshot
            .as_deref()
            .zip(fleet.roster.alab.as_ref())
            .map(|(lab, me)| (lab, &me.key)),
        &mut default_login,
    );
    let default_changed = reassigned.is_some();
    if let Some(roster) = reassigned {
        final_rows = roster.rows;
        own_row.clone_from(&default_login.reassigned);
    }
    note_moved_off(&mut default_login, &store, final_rows.clone(), now);
    let recovered = recover_interrupted_staging_renewals_for(
        cli,
        &store,
        &write_to,
        &AUTO_RENEW_GUARD,
        now,
        shared_renewal::CheckpointAuthority::live(Some(home), &write_to),
    );
    let changed = !added.is_empty()
        || roster_wrote
        || default_changed
        || state_wrote
        || recovered_commit
        || recovered;
    // A sync that added nothing wrote nothing, and wakes no watcher.
    if changed {
        let _ = store.touch_wake();
    }
    let conflicts = fleet.roster.conflicts_for(Some(&machine));
    let mut listed = BTreeMap::new();
    for (labels, accounts) in [
        (&held, fleet.held.iter().collect::<Vec<_>>()),
        (&kept, fleet.roster.accounts_for_machine(&machine)),
    ] {
        for account in accounts {
            if labels.contains(&account.label) {
                listed
                    .entry(account.label.clone())
                    .or_insert_with(|| (account.vendor.region, account.email.clone()));
            }
        }
    }
    Ok(CliSync::Synced(Box::new(CliSynced {
        cli,
        machine,
        machine_label: fleet
            .roster
            .alab
            .as_ref()
            .and_then(|machine| machine.label.clone()),
        roster: roster_path,
        source: fleet.roster.source.clone(),
        fleet_accounts,
        rows: total,
        added,
        own_row,
        held,
        kept,
        listed,
        conflicts,
        changed,
        prefers_none,
        rows_changed,
        clashes: fleet.roster.clashes.clone(),
        default_login,
    })))
}

/// Follows `cli`'s default login to the row of the address it names, where it moved
/// ([`reassign_codex_default`]): the roster that came of it, or `None`, with `follow` saying which
/// row took it or why none did. A refusal is the receipt's to say beside what the sync wrote before
/// it, so it fails no sync here.
fn follow_default_login(
    cli: Cli,
    (env, store): (&Env, &accounts::Store),
    (configured, path): (&Path, &Path),
    ladder: &[Duration],
    lab: Option<(&super::alab_roster::Lab, &super::alab_roster::MachineKey)>,
    follow: &mut DefaultLogin,
) -> Option<Roster> {
    if cli != Cli::Codex {
        return None;
    }
    match reassign_codex_default(env, store, configured, path, ladder, lab, || {}) {
        Ok(Some((roster, label))) => {
            follow.reassigned = Some(label);
            Some(roster)
        }
        Ok(None) => None,
        Err(why) => {
            follow.refused = Some(why.and("; no row moved"));
            None
        }
    }
}

/// `follow` names the row still on the default login among `rows`, as the store binds them, where
/// that login names another account ([`accounts::default_login_moved_off`]). Bindings it cannot read
/// leave nothing named, as a launch refuses on them anyway.
fn note_moved_off(
    follow: &mut DefaultLogin,
    store: &accounts::Store,
    rows: Vec<Row>,
    now: impl Into<trace::Now>,
) {
    let now: trace::Now = now.into();
    if follow.address.is_none() {
        return;
    }
    let Ok((logins, _)) = store.load_logins(now) else {
        return;
    };
    let roster = accounts::bound(&Roster { rows }, &logins);
    follow.moved_off = accounts::default_login_moved_off(&roster, follow.address.as_deref())
        .and_then(|row| {
            follow.moved_off_addressed = row.email.is_some();
            let lab = super::alab_roster::current();
            follow.moved_off_word = Some(super::account_names::sign_in_word(
                Cli::Codex,
                row,
                &roster.rows,
                lab.as_deref(),
            ));
            Some((row.label.clone(), row.expected_identity_email()?.to_owned()))
        });
}

/// A sync with no fleet to read ([`CliSync::NoSource`]): where this Mac's default login names an
/// address and it keeps a roster, what comes of following that login, which reads no source.
/// A roster that stands and does not parse fails this sync as it fails one with a source, whatever
/// the default login names, so a setup's or doctor's account step does not turn on that login.
///
/// # Errors
/// The roster that stands is refused, or its target or records cannot be read.
fn sync_without_source(
    cli: Cli,
    (env, store): (&Env, &accounts::Store),
    roster_path: &Path,
    ladder: &[Duration],
    now: impl Into<trace::Now>,
) -> Result<CliSync, String> {
    let now: trace::Now = now.into();
    if !trace::fs::is_file(roster_path) {
        return Ok(CliSync::NoSource);
    }
    let (write_to, _) = super::cli::roster_target(roster_path)?;
    let existing = accounts::roster_text(&write_to)?.map_or(Ok(Vec::new()), |text| {
        Roster::parse_for(cli, &text)
            .map(|roster| roster.rows)
            .map_err(|why| {
                format!(
                    "the roster at {} is refused: {why}; the roster is unchanged",
                    roster_path.display()
                )
            })
    })?;
    let address = store
        .user_home()
        .and_then(|home| accounts::default_login_address(cli, home));
    if address.is_none() {
        return Ok(CliSync::NoSource);
    }
    let mut follow = DefaultLogin {
        address,
        ..DefaultLogin::default()
    };
    let rows = follow_default_login(
        cli,
        (env, store),
        (roster_path, &write_to),
        ladder,
        None,
        &mut follow,
    )
    .map_or(existing, |roster| roster.rows);
    note_moved_off(&mut follow, store, rows, now);
    Ok(if follow.stands() {
        CliSync::NoSourceDefaultLogin(Box::new(follow))
    } else {
        CliSync::NoSource
    })
}

/// What keeps a row displaced from the default login out of its managed login directory
/// ([`displaced_dir_refusal`]).
#[derive(Debug, Clone, PartialEq, Eq)]
enum DisplacedDir {
    /// Its login names another account, here this address.
    NamesAnother(String),
    /// Its login names the row's own account, in a directory the row's binding never left.
    OwnNeverLeft,
    /// A login file is there, and it names no address aterm can read.
    Unreadable,
    /// Its `config.toml` keeps logins in the OS keyring, so no file says whose it holds.
    KeyringStore,
    /// Files are there, none of them a login or aterm's own bookkeeping.
    OtherFiles,
}

impl DisplacedDir {
    /// The refusal of `from`, the row displaced from the default login, whose address is `email`
    /// and whose managed login directory is `dir`, with what ends it, in both its forms
    /// ([`MoveRefusal`]): the receipt names the row by its account's name and slug
    /// ([`super::account_names::shown_row`]) and says the address a login there names; a log names
    /// the row by its account's key and slug ([`super::account_names::logged_row`]) and another
    /// account a login there names by its own, and says no address.
    fn refusal(
        &self,
        from: &Row,
        email: &str,
        dir: &str,
        lab: Option<&super::alab_roster::Lab>,
    ) -> MoveRefusal {
        let another = |address: &str| {
            super::account_names::names_of_login(Cli::Codex, from.vendor.region, address, lab)
                .map_or_else(
                    || "an address aterm cannot key".to_owned(),
                    |(pk, names)| {
                        super::account_names::Named {
                            pk,
                            row: None,
                            names,
                        }
                        .logged()
                    },
                )
        };
        MoveRefusal {
            said: self.words(
                &super::account_names::shown_row(Cli::Codex, from, lab),
                Some(email),
                &str::to_owned,
                dir,
            ),
            logged: self.words(
                &super::account_names::logged_row(Cli::Codex, from, lab),
                None,
                &another,
                dir,
            ),
        }
    }

    /// The refusal's words for `row`, the row as they name it, whose address they say where
    /// `email` is one, `another` naming the account a login there names by its address, and what
    /// ends it. A login that names the row is the row's, so a person is not told to move it away.
    fn words(
        &self,
        row: &str,
        email: Option<&str>,
        another: &dyn Fn(&str) -> String,
        dir: &str,
    ) -> String {
        let away = "move that directory away and sync again";
        match self {
            Self::NamesAnother(address) => format!(
                "the managed directory {dir} of {row} holds the login of {}, another account: \
                 {away}",
                another(address)
            ),
            Self::OwnNeverLeft => format!(
                "the managed directory {dir} of {row} holds a login of {}, in a directory no \
                 record says it left, so aterm does not take it on the login's word: a sign-in \
                 of it on the default login ends this",
                email.map_or_else(
                    || "its own account".to_owned(),
                    |email| format!("{email}, its own")
                )
            ),
            Self::Unreadable => format!(
                "the managed directory {dir} of {row} holds a login that names no address aterm \
                 can read: {away}"
            ),
            Self::KeyringStore => format!(
                "the managed directory {dir} of {row} keeps its logins in the OS keyring, so no \
                 file says whose it holds: {away}"
            ),
            Self::OtherFiles => {
                format!(
                    "the managed directory {dir} of {row} holds files that are no login: {away}"
                )
            }
        }
    }
}

/// Why a row displaced from the default login may not take `dir`, its managed login directory, or
/// `None` where it may: it holds nothing but aterm's own bookkeeping ([`accounts::login_made`]'s
/// rule), or it is a directory the row left (`was`, [`accounts::Binding::was`]) whose login names
/// the row's `email`, which it takes back with nothing lost. Anything else stays where it is.
fn displaced_dir_refusal(
    dir: &str,
    was: &[Option<String>],
    email: &str,
) -> Result<Option<DisplacedDir>, String> {
    let home = Path::new(dir);
    let entries = match trace::fs::read_dir(home) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    if entries
        .iter()
        .all(|entry| accounts::login_bookkeeping(&entry.file_name()))
    {
        return Ok(None);
    }
    if !crate::ai::facts::codex_file_store(home) {
        return Ok(Some(DisplacedDir::KeyringStore));
    }
    match super::codex_app_login::read_account_email(home) {
        Some(named) if !accounts::email_matches(&named, email) => {
            Ok(Some(DisplacedDir::NamesAnother(named)))
        }
        Some(_) if accounts::lists_directory(was, dir)? => Ok(None),
        Some(_) => Ok(Some(DisplacedDir::OwnNeverLeft)),
        None if trace::fs::exists(home.join("auth.json")) => Ok(Some(DisplacedDir::Unreadable)),
        None => Ok(Some(DisplacedDir::OtherFiles)),
    }
}

/// What a sync with no source named does with the row on the default login, on the records as they
/// stand, by the rules that sync acts on ([`reassign_codex_default`]), writing nothing and taking no
/// lock: what a surface asks before it names that sync.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DefaultMove {
    /// It moves the row off that login, to the row of the account that login names.
    Moves,
    /// It leaves the row there: the login names the row's own account, or one no row holds.
    Stays,
    /// It refuses to move the row, in these words: a log's ([`MoveRefusal::logged`]), since a
    /// launch's refusal and its facts keep them, which name each account by its key and slug and
    /// say no address.
    Refused(String),
}

/// A default login that names another account than its row, and the row of that account, by what
/// a sync reads before it takes a lock ([`default_move_asked`]).
struct MoveAsked<'a> {
    from: &'a Row,
    to: &'a Row,
    expected: &'a str,
    home: PathBuf,
    email: String,
    policy: CodexCompletionSource,
}

/// [`MoveAsked`] of `roster` and the default login: `None` where no such move is asked. A row or a
/// source that keeps the move from being made is the refusal.
fn default_move_asked<'r>(
    env: &Env,
    store: &accounts::Store,
    roster: &'r Roster,
) -> Result<Option<MoveAsked<'r>>, String> {
    let Some(from) = roster.row_for_login(None) else {
        return Ok(None);
    };
    let Some(expected) = from.expected_identity_email() else {
        return Ok(None);
    };
    let Some(home) = store.default_home() else {
        return Ok(None);
    };
    if !crate::ai::facts::codex_file_store(&home) {
        return Ok(None);
    }
    let Some(email) = super::codex_app_login::read_account_email(&home) else {
        return Ok(None);
    };
    if accounts::email_matches(expected, &email) {
        return Ok(None);
    }
    let mut candidates = roster.rows.iter().filter(|row| {
        row.expected_identity_email()
            .is_some_and(|expected| accounts::email_matches(expected, &email))
            && accounts::same_provider_realm(Cli::Codex, from.vendor.region, row.vendor.region)
    });
    let Some(to) = candidates.next() else {
        return Ok(None);
    };
    // A lab not read yet asks no move: no login directory moves until it is read, and the row
    // left on the default login is named in the receipt ([`note_moved_off`]), as no refusal.
    let read = codex_completion_read(env, to);
    if matches!(read, Ok(CodexCompletion::LabPending(_))) {
        return Ok(None);
    }
    if candidates.next().is_some() {
        return Err("the default Codex login matches multiple declared accounts".into());
    }
    if from.held_by.is_some() || to.held_by.is_some() {
        return Err("the default Codex reassignment includes a held account".into());
    }
    super::codex_app_login::check_login_identity(&home, Some(&email)).map_err(|e| e.to_string())?;
    let CodexCompletion::Read(policy) = read? else {
        return Ok(None);
    };
    if policy.held_by.is_some() {
        return Err("the source holds the default Codex account".into());
    }
    if let Some(fleet) = &policy.fleet {
        let mut declared = fleet.roster.accounts.iter().filter(|account| {
            account.label == to.label && fleet_matches_identity(Cli::Codex, to, account)
        });
        if declared.next().is_none() || declared.next().is_some() {
            return Err("the source does not uniquely declare the default Codex account".into());
        }
    }
    Ok(Some(MoveAsked {
        from,
        to,
        expected,
        home,
        email,
        policy,
    }))
}

/// Where the records let [`MoveAsked`] go: the displaced row's directory, whether its binding
/// already keeps one elsewhere, what its binding left, and the roster after the move.
struct MoveAdmitted {
    independent: Option<String>,
    directory: String,
    left: Vec<Option<String>>,
    updated: Roster,
}

/// [`MoveAdmitted`] of `asked` under `logins`, or why the records refuse it, in a receipt's words
/// and a log's ([`MoveRefusal`]).
fn default_move_admitted(
    store: &accounts::Store,
    roster: &Roster,
    asked: &MoveAsked<'_>,
    logins: &accounts::Logins,
) -> Result<MoveAdmitted, MoveRefusal> {
    let MoveAsked {
        from,
        to,
        expected,
        home,
        ..
    } = asked;
    let mut updated = roster.clone();
    let independent = logins
        .get(&from.label)
        .and_then(|binding| binding.dir.clone());
    let directory = independent
        .clone()
        .or_else(|| {
            store
                .login_dir(&from.label)
                .and_then(|path| path.to_str().map(str::to_owned))
        })
        .ok_or("the displaced account has no managed login location")?;
    if accounts::retained_directory_owner(logins, &roster.rows, &from.label, &directory)?.is_some()
        || accounts::shared_staging_retained(logins, &directory)?
    {
        return Err("the displaced account's directory has another owner".into());
    }
    if accounts::naming_row(
        std::iter::once(&Row {
            dir: Some(directory.clone()),
            ..Row::default()
        }),
        home.to_str().ok_or("the default Codex home is not UTF-8")?,
    )?
    .is_some()
    {
        return Err("the displaced account's directory aliases the default login".into());
    }
    let left: Vec<Option<String>> = logins
        .get(&from.label)
        .map(|binding| binding.was.clone())
        .unwrap_or_default();
    if independent.is_none()
        && let Some(refusal) = displaced_dir_refusal(&directory, &left, expected)?
    {
        let lab = super::alab_roster::current();
        return Err(refusal.refusal(from, expected, &directory, lab.as_deref()));
    }
    updated
        .rows
        .iter_mut()
        .find(|row| row.label == from.label)
        .ok_or("the displaced account left the roster")?
        .dir = Some(directory.clone());
    updated
        .rows
        .iter_mut()
        .find(|row| row.label == to.label)
        .ok_or("the default account left the roster")?
        .dir = None;
    if accounts::retained_directory_owner(
        logins,
        &updated.rows,
        &to.label,
        home.to_str().ok_or("the default Codex home is not UTF-8")?,
    )?
    .is_some()
        || accounts::shared_staging_retained(
            logins,
            home.to_str().ok_or("the default Codex home is not UTF-8")?,
        )?
    {
        return Err("the default Codex directory has another retained owner".into());
    }
    Ok(MoveAdmitted {
        independent,
        directory,
        left,
        updated,
    })
}

/// [`DefaultMove`] of the pool `sync` names, as a sync with no source named would read it.
#[must_use]
pub fn default_move_verdict(sync: super::accounts_pool::SyncHere<'_>) -> DefaultMove {
    let (store, roster_path) = (sync.store, sync.roster);
    if store.cli() != Cli::Codex {
        return DefaultMove::Stays;
    }
    let (Some(home), Some(state)) = (store.user_home(), store.dir.parent()) else {
        return DefaultMove::Stays;
    };
    let env = quiet_env(home, state, &config_beside(roster_path), sync.now);
    let verdict = || -> Result<DefaultMove, MoveRefusal> {
        let (path, _) = super::cli::roster_target(roster_path)?;
        let Some(text) = accounts::roster_text(&path)? else {
            return Ok(DefaultMove::Stays);
        };
        let roster = Roster::parse_for(Cli::Codex, &text)?;
        let Some(asked) = default_move_asked(&env, store, &roster)? else {
            return Ok(DefaultMove::Stays);
        };
        let (logins, note) = store.load_logins(sync.now)?;
        if note.is_some() {
            return Err("account records could not be read consistently".into());
        }
        default_move_admitted(store, &roster, &asked, &logins)?;
        Ok(DefaultMove::Moves)
    };
    // A launch's refusal and its facts keep these words: a log's, which say no address.
    verdict().unwrap_or_else(|why| DefaultMove::Refused(why.logged))
}

/// Reassign a named default login only after observing another uniquely declared account.
/// This changes login locations, not account identity, provider authority or existing sessions.
/// The sync receipt publishes the roster and binding fences together; no credential is copied.
fn reassign_codex_default(
    env: &Env,
    store: &accounts::Store,
    configured: &Path,
    path: &Path,
    ladder: &[Duration],
    lab: Option<(&super::alab_roster::Lab, &super::alab_roster::MachineKey)>,
    before_publish: impl FnOnce(),
) -> Result<Option<(Roster, String)>, MoveRefusal> {
    store
        .recover_sync_within(ladder, configured)
        .map_err(SyncError::into_message)?;
    let (target, mode) = super::cli::roster_target(configured)?;
    if target != path {
        return Err("the configured roster target changed before Codex reassignment".into());
    }
    let Some(text) = accounts::roster_text(path)? else {
        return Ok(None);
    };
    let roster = Roster::parse_for(Cli::Codex, &text)?;
    let Some(asked) = default_move_asked(env, store, &roster)? else {
        return Ok(None);
    };
    let config = env
        .config
        .as_deref()
        .ok_or("the account configuration is unavailable")?;
    let configuration = accounts::roster_text(config)?;
    let _signing = store
        .signin_now()
        .map_err(|e| e.to_string())?
        .ok_or(accounts::SIGNIN_HELD)?;
    let held = store
        .acquire_within(ladder)
        .map_err(|e| e.to_string())?
        .ok_or(accounts::LOCK_HELD)?;
    store.recover_sync_held(&held, configured)?;
    let _auth = super::codex_app_login::AuthLock::try_take(&asked.home.join("auth.json"))
        .map_err(|e| e.to_string())?
        .ok_or("the default Codex login is being changed")?;
    let (mut state, state_note) = store.load(env.now)?;
    let (logins, logins_note) = store.load_logins(env.now)?;
    if state_note.is_some() || logins_note.is_some() {
        return Err("account records could not be read consistently".into());
    }
    // The transaction applies this same machine-name normalization before its edit.
    if let Ok(machine) = store.machine_id(&state.machine) {
        state.machine = machine;
    }
    let MoveAdmitted {
        independent,
        directory,
        left,
        mut updated,
    } = default_move_admitted(store, &roster, &asked, &logins)?;
    let MoveAsked {
        from,
        to,
        expected,
        home,
        email,
        policy,
    } = asked;
    if let Some((lab, key)) = lab {
        super::alab_roster::order_rows(Some(lab), Cli::Codex, key, &mut updated.rows);
    }
    let next_text = accounts::append_rows_for(Cli::Codex, "", &updated.rows)?;
    before_publish();
    let recheck = || -> Result<(), String> {
        if super::cli::roster_target(configured)? != (target.clone(), mode)
            || accounts::roster_text(path)?.as_deref() != Some(&text)
            || accounts::roster_text(config)? != configuration
            || !crate::ai::facts::codex_file_store(&home)
            || !super::codex_app_login::read_account_email(&home)
                .is_some_and(|now| accounts::email_matches(&now, &email))
            || codex_completion_source(env, to)? != policy
        {
            return Err("Codex account identity or policy changed before reassignment".into());
        }
        if accounts::retained_directory_owner(&logins, &roster.rows, &from.label, &directory)?
            .is_some()
            || accounts::retained_directory_owner(
                &logins,
                &updated.rows,
                &to.label,
                home.to_str().ok_or("the default Codex home is not UTF-8")?,
            )?
            .is_some()
            || accounts::shared_staging_retained(&logins, &directory)?
            || accounts::shared_staging_retained(
                &logins,
                home.to_str().ok_or("the default Codex home is not UTF-8")?,
            )?
        {
            return Err("Codex login directory ownership changed".into());
        }
        if independent.is_none() && displaced_dir_refusal(&directory, &left, expected)?.is_some() {
            return Err("the displaced account's directory became occupied".into());
        }
        super::codex_app_login::check_login_identity(&home, Some(&email)).map_err(|e| e.to_string())
    };
    recheck()?;
    store
        .transact_sync_source_held(
            &held,
            env.now,
            &mut None,
            (
                accounts::SyncTarget {
                    fleet: None,
                    path,
                    before: Some(&text),
                    after: Some(&next_text),
                    mode,
                },
                policy.guard.as_ref(),
            ),
            (false, |current, bindings| {
                if *current != state || *bindings != logins {
                    return Err("account custody changed before Codex reassignment".into());
                }
                recheck()?;
                for label in [&from.label, &to.label] {
                    bindings.pending_sign_in_invalidations.insert(label.clone());
                }
                if independent.is_none() {
                    let mut was = bindings
                        .get(&from.label)
                        .map(|binding| binding.was.clone())
                        .unwrap_or_default();
                    was.retain(|kept| kept.as_deref().is_some_and(|kept| kept != directory));
                    was.truncate(accounts::MAX_LEFT - 1);
                    was.push(None);
                    bindings.labels.insert(
                        from.label.clone(),
                        accounts::Binding {
                            dir: Some(directory.clone()),
                            was,
                            ..accounts::Binding::default()
                        },
                    );
                } else if let Some(binding) = bindings.labels.get_mut(&from.label) {
                    binding.was.retain(Option::is_some);
                    binding.was.truncate(accounts::MAX_LEFT - 1);
                    binding.was.push(None);
                }
                let previous = bindings.get(&to.label).cloned().unwrap_or_default();
                let mut was = previous.was;
                if let Some(directory) = previous.dir.or_else(|| to.dir.clone()) {
                    was.retain(|kept| kept.as_deref().is_some_and(|kept| kept != directory));
                    was.truncate(accounts::MAX_LEFT - 1);
                    was.push(Some(directory));
                }
                bindings.labels.insert(
                    to.label.clone(),
                    accounts::Binding {
                        was,
                        ..accounts::Binding::default()
                    },
                );
                Ok(Vec::new())
            }),
        )
        .map_err(SyncError::into_message)?;
    Ok(Some((updated, to.label.clone())))
}

/// A first sync under the lab, whose roster has no rows yet, of a CLI whose default login names
/// its account (Codex's `auth.json`): the row of that account is this machine's own login's, under
/// its lab label and with its address, so one account is listed once. A row this sync adds for
/// that account gives up the login directory it would have had; an account the lab knows and does
/// not prefer here joins as a pool row. `None` where the identity cannot be read (Grok Build's and
/// Kimi Code's logins name none, and a login kept in the OS keyring is not read, whatever account a
/// leftover `auth.json` beside it names) or the lab does not know the account: the caller lists the
/// default login as `main`, as for any other source.
fn first_sync_own_row(
    cli: Cli,
    fleet: &FleetRoster,
    store: &accounts::Store,
    rows: &mut Vec<Row>,
    added: &mut Vec<String>,
) -> Option<String> {
    let email = first_sync_native_email(cli, fleet, store)?;
    if let Some(at) = rows.iter().position(|row| {
        row.email
            .as_deref()
            .is_some_and(|e| accounts::email_matches(e, &email))
    }) {
        let mut row = rows.remove(at);
        row.dir = None;
        let label = row.label.clone();
        rows.insert(0, row);
        return Some(label);
    }
    let known = alab_default_account(fleet, None, None, &email, &|account, email| {
        accounts::email_matches(&account.email, email)
    })?;
    let label = known.label.clone();
    rows.insert(
        0,
        Row {
            label: known.label,
            class: known.class,
            dir: None,
            email: Some(known.email),
            identity_email: None,
            browser: known.browser,
            held_by: None,
            was: Vec::new(),
            vendor: known.vendor,
        },
    );
    added.push(label.clone());
    Some(label)
}

/// A native default identity is independent of this Mac's preferred account assignments.
/// Keyring-backed logins never enroll from a leftover file.
fn first_sync_native_email(
    cli: Cli,
    fleet: &FleetRoster,
    store: &accounts::Store,
) -> Option<String> {
    fleet.alab.as_ref()?;
    if cli != Cli::Codex {
        return None;
    }
    let default_home = store.default_home()?;
    if !crate::ai::facts::codex_file_store(&default_home) {
        return None;
    }
    super::codex_app_login::read_account_email(&default_home)
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

#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountProviderCommit",
        action = "Provider",
        reason = "The caller selects the provider whose real sync is exercised."
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountProviderCommit",
        action = "Cut",
        reason = "The test selects a durable replay interruption, not a production mutation."
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountProviderCommit",
        action = "Fault",
        reason = "The environment supplies a conflicting roster edit or another caller scope."
    )
)]
#[doc(hidden)]
pub fn account_provider_commit_environment() {}

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
/// ([`sync_cli_in_env`]).
pub fn run_cli_accounts_sync(
    cli: Cli,
    (env, program): (&Env, Option<&Path>),
    (source, machine_override, json): (Option<&str>, Option<&str>, bool),
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitCode {
    run_cli_accounts_sync_with(
        cli,
        (env, program),
        (source, machine_override, json),
        (out, err),
        &fetch_source_text,
    )
}

pub(super) fn run_cli_accounts_sync_with(
    cli: Cli,
    (env, program): (&Env, Option<&Path>),
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
    // The fetch and its later byte/path validation use the same captured session context.
    let synced = sync_cli_in_env(
        cli,
        (home, &env.state, config),
        (source, machine_override),
        SyncCaller::Person,
        env.now,
        (env, fetch, &sync_lock_ladder()),
        || {},
    );
    let file = cli.accounts().roster_file;
    let synced = match synced {
        Ok(CliSync::Synced(synced)) => synced,
        Ok(CliSync::Left { label, listed }) => {
            // The row by its account; its label only where the restored file must write it.
            let row = listed.map_or_else(
                || format!("{label} (no address)"),
                |(region, email)| Naming::of(cli, &[]).listed_shown(&label, region, &email),
            );
            let _ = writeln!(
                err,
                "aterm harness accounts sync {flag}: nothing was synced: this machine kept a \
                 {product} roster ({file}) and removed it, which takes a machine out of the \
                 pool; its store still holds the records of {row}, a row the sync would add, \
                 which no new row may inherit; to rejoin, restore {file} with the rows it held, \
                 the row labelled `{label}` among them"
            );
            return ExitCode::FAILURE;
        }
        Ok(CliSync::Off) => {
            let _ = writeln!(
                err,
                "aterm harness accounts sync {flag}: nothing was read: this machine keeps no \
                 {product} roster ({file}) and no source was named; name one (`aterm harness \
                 accounts sync {flag} {}`), or list an account (`{}`)",
                crate::vault_entry::source_name(&cli_fleet_file(cli)),
                Step::Add(cli)
            );
            return ExitCode::FAILURE;
        }
        Ok(CliSync::NoSource) => {
            let _ = writeln!(
                err,
                "aterm harness accounts sync {flag}: {}; the roster is unchanged",
                nothing_to_read_words(cli)
            );
            return ExitCode::FAILURE;
        }
        Ok(CliSync::NoSourceDefaultLogin(login)) => {
            let names = accounts::roster_beside_for(config, cli)
                .map_or_else(|| Naming::of(cli, &[]), |path| Naming::at(cli, &path));
            return no_source_default_login_receipt(
                cli,
                (&login, &names),
                (program, json),
                (out, err),
            );
        }
        Ok(CliSync::LabPending { why, login }) => {
            let names = accounts::roster_beside_for(config, cli)
                .map_or_else(|| Naming::of(cli, &[]), |path| Naming::at(cli, &path));
            if json {
                let mut o = Map::new();
                o.insert("schema".to_owned(), Value::from(SYNC_SCHEMA));
                o.insert("kind".to_owned(), Value::from("accounts-sync".to_owned()));
                o.insert("cli".to_owned(), Value::from(cli.id().to_owned()));
                o.insert(
                    "source".to_owned(),
                    Value::from(super::alab_roster::SOURCE.to_owned()),
                );
                o.insert("lab_pending".to_owned(), Value::from(why.clone()));
                if let Some(login) = &login {
                    login.put_json(&names, &mut o);
                }
                let _ = writeln!(
                    out,
                    "{}",
                    aterm_json::to_string(&Value::Object(o)).unwrap_or_default()
                );
            } else {
                if let Some(login) = &login {
                    login.write_lines(&names, program, out);
                }
                let _ = writeln!(out, "{product} roster ({file}): {why}");
            }
            // A lab not read yet asks no move of the default login, so a refusal here is one any
            // sync fails on (its records could not be read), as the upkeep's does.
            return login.as_ref().map_or(ExitCode::SUCCESS, |login| {
                write_default_refusal(login, &names, program, &flag, err)
            });
        }
        Ok(CliSync::Unlisted {
            machine,
            source,
            conflicts,
        }) => {
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
            if let Some(words) = sheet_conflicts_words(&conflicts) {
                let _ = writeln!(err, "aterm harness accounts sync {flag}: {words}");
            }
            return ExitCode::FAILURE;
        }
        Err(why) => {
            let _ = writeln!(err, "aterm harness accounts sync {flag}: {why}");
            return ExitCode::FAILURE;
        }
    };
    if let Some(words) = sheet_conflicts_words(&synced.conflicts) {
        let _ = writeln!(err, "aterm harness accounts sync {flag}: {words}");
    }
    // Each row the receipt names, by its account among the roster the sync wrote and the lab; the
    // steps name the added rows by their words among the same.
    let written = accounts::Roster::load_for(cli, &synced.roster).unwrap_or_default();
    let names = Naming::of(cli, &written.rows);
    // A fleet row `held` or `kept` names, by the account its address names.
    let listed = |label: &str| {
        synced
            .listed
            .get(label)
            .map_or((None, ""), |(region, email)| (*region, email.as_str()))
    };
    if json {
        let fleet_rows = |labels: &[String]| {
            Value::Array(
                labels
                    .iter()
                    .map(|label| {
                        let (region, email) = listed(label);
                        names.listed_json(label, region, email)
                    })
                    .collect(),
            )
        };
        let mut o = Map::new();
        o.insert("schema".to_owned(), Value::from(SYNC_SCHEMA));
        o.insert("kind".to_owned(), Value::from("accounts-sync".to_owned()));
        o.insert("cli".to_owned(), Value::from(cli.id().to_owned()));
        o.insert("machine".to_owned(), Value::from(synced.machine.clone()));
        o.insert(
            "machine_label".to_owned(),
            synced
                .machine_label
                .as_deref()
                .map_or(Value::Null, |label| Value::from(label.to_owned())),
        );
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
        o.insert("added".to_owned(), names.json_all(&synced.added));
        o.insert("held".to_owned(), fleet_rows(&synced.held));
        o.insert("kept".to_owned(), fleet_rows(&synced.kept));
        o.insert("prefers_none".to_owned(), Value::from(synced.prefers_none));
        o.insert(
            "sheet_conflicts".to_owned(),
            conflicts_json(&synced.conflicts),
        );
        synced.rows_changed.put_json(&names, &mut o);
        o.insert(
            "label_clashes".to_owned(),
            names.clashes_json(&synced.clashes),
        );
        synced.default_login.put_json(&names, &mut o);
        // A first sync's own row names no move of the default login, so it stands beside one.
        if let Some(own) = &synced.own_row {
            o.insert("own_row".to_owned(), names.json(own));
        }
        let _ = writeln!(
            out,
            "{}",
            aterm_json::to_string(&Value::Object(o)).unwrap_or_default()
        );
        return write_default_refusal(&synced.default_login, &names, program, &flag, err);
    }
    let elsewhere = if synced.source == super::alab_roster::SOURCE {
        "the lab prefers here"
    } else {
        "across the fleet"
    };
    let _ = writeln!(
        out,
        "synced {product}'s pool for machine `{}` ({} added; {} rows in the roster, {} {elsewhere})",
        synced.machine_label.as_deref().unwrap_or(&synced.machine),
        synced.added.len(),
        synced.rows,
        synced.fleet_accounts
    );
    if let Some(own) = &synced.own_row {
        let _ = writeln!(out, "on this machine's own login: {}", names.shown(own));
    }
    let _ = writeln!(out, "roster: {}", synced.roster.display());
    let _ = writeln!(out, "source: {}", synced.source);
    synced.rows_changed.write_lines(&names, out);
    names.write_clash_lines(&synced.clashes, out);
    synced.default_login.write_lines(&names, program, out);
    let fleet_rows = |labels: &[String]| {
        labels
            .iter()
            .map(|label| {
                let (region, email) = listed(label);
                names.listed_shown(label, region, email)
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    if synced.prefers_none {
        let _ = writeln!(
            out,
            "the lab prefers no {product} account on `{}`: nothing was added, and every row the \
             roster lists stays",
            synced.machine_label.as_deref().unwrap_or(&synced.machine)
        );
    }
    if !synced.held.is_empty() {
        let _ = writeln!(
            out,
            "held (the sheet names no region; write `global` or `mainland-cn` in its cell): {}",
            fleet_rows(&synced.held)
        );
    }
    if !synced.kept.is_empty() {
        let _ = writeln!(
            out,
            "left as the roster lists them (conflicting accounts or sign-in settings): {}",
            fleet_rows(&synced.kept)
        );
    }
    let unsigned: Vec<&str> = synced
        .added
        .iter()
        .map(String::as_str)
        .filter(|label| synced.own_row.as_deref() != Some(label))
        .collect();
    let beside = synced.roster.parent().map(|dir| dir.join("aterm.toml"));
    let off = env
        .config
        .as_deref()
        .or(beside.as_deref())
        .and_then(|config| sign_ins_off_for(config, cli));
    let renew = |labels: &[&str]| {
        Step::Renew(
            cli,
            labels
                .iter()
                .map(|label| {
                    super::account_names::sign_in_word_of(
                        cli,
                        label,
                        &written.rows,
                        names.lab.as_deref(),
                    )
                })
                .collect(),
        )
    };
    if let (false, true, Some(switch)) = (unsigned.is_empty(), cli.renews_headlessly(), off) {
        let _ = writeln!(
            out,
            "not signed in yet: {}; {switch} keeps aterm's unattended sign-ins off here \
             (`{}` runs one now)",
            names.shown_all(&unsigned),
            renew(&unsigned)
        );
    } else if !unsigned.is_empty() && cli.renews_headlessly() && !OTHER_CLIS.contains(&cli) {
        let _ = writeln!(
            out,
            "not signed in yet: {}; `{}` signs them in",
            names.shown_all(&unsigned),
            renew(&unsigned)
        );
    } else if !unsigned.is_empty() && cli.renews_headlessly() {
        let records = env.accounts_store_for(cli).and_then(|store| {
            let records = store
                .read_account_records(Some(&synced.roster), env.now)
                .ok()?;
            Some((store, records))
        });
        let (unattended, by_person): (Vec<&str>, Vec<&str>) =
            unsigned.iter().copied().partition(|label| {
                records.as_ref().is_some_and(|(store, records)| {
                    records.raw_roster.row(label).is_some_and(|raw| {
                        page_row_signs_in_unattended(
                            cli,
                            store,
                            &records.raw_roster,
                            raw,
                            &records.logins,
                        )
                    })
                })
            });
        if !by_person.is_empty() {
            let _ = writeln!(
                out,
                "not signed in yet: {}; a person opens each once with `{}`, and `{}` then signs \
                 them in",
                names.shown_all(&by_person),
                Step::Browser(cli, Word::chosen()),
                renew(&by_person)
            );
        }
        if let Some((_, records)) = &records {
            // Each of `unattended` has its route: its page signs it in with nobody here.
            let others = rows_of_other_macs(cli, env, &records.raw_roster, &records.logins);
            write_waiting_receipt(
                cli,
                &unattended,
                (records, &others),
                (None, Some(&BTreeSet::new())),
                env.now,
                out,
            );
        }
    } else if !unsigned.is_empty() {
        let _ = writeln!(
            out,
            "not signed in yet: {}; {product}'s sign-in page answers aterm's headless browser \
             with a block page, so a person signs each in: `{}`",
            names.shown_all(&synced.added.iter().map(String::as_str).collect::<Vec<_>>()),
            Step::sign_in(cli, Word::chosen(), true)
        );
    }
    write_default_refusal(&synced.default_login, &names, program, &flag, err)
}

/// What a sync of `cli` found no source in, in its error line's words.
fn nothing_to_read_words(cli: Cli) -> String {
    format!(
        "nothing to read: the vault holds neither {}, nor {}, nor a `{}` column in \
         {ACCOUNTS_CSV_FILE} for this Mac",
        super::ai_pool::AI_POOL_FILE,
        cli_fleet_file(cli),
        csv_columns(cli).first().copied().unwrap_or(cli.id())
    )
}

/// The error line of a sync that could not follow the default login, each row by its account
/// under `names`, and its exit code; success where none stands.
fn write_default_refusal(
    login: &DefaultLogin,
    names: &Naming,
    program: Option<&Path>,
    flag: &str,
    err: &mut dyn Write,
) -> ExitCode {
    let Some(words) = login.refusal_words(names, program) else {
        return ExitCode::SUCCESS;
    };
    let _ = writeln!(err, "aterm harness accounts sync {flag}: {words}");
    ExitCode::FAILURE
}

/// The receipt of a sync with no source to read whose default login names another account than its
/// row: a row that took that login is what the sync came to do, so it succeeds; else it fails as a
/// sync with nothing to read does, and says what stands. Each row is named by its account under
/// `names`, the roster as the sync left it ([`Naming`]).
fn no_source_default_login_receipt(
    cli: Cli,
    (login, names): (&DefaultLogin, &Naming),
    (program, json): (Option<&Path>, bool),
    (out, err): (&mut dyn Write, &mut dyn Write),
) -> ExitCode {
    let flag = format!("--cli {}", cli.id());
    let nothing = nothing_to_read_words(cli);
    if json {
        let mut o = Map::new();
        o.insert("schema".to_owned(), Value::from(SYNC_SCHEMA));
        o.insert("kind".to_owned(), Value::from("accounts-sync".to_owned()));
        o.insert("cli".to_owned(), Value::from(cli.id().to_owned()));
        o.insert("source".to_owned(), Value::Null);
        login.put_json(names, &mut o);
        let _ = writeln!(
            out,
            "{}",
            aterm_json::to_string(&Value::Object(o)).unwrap_or_default()
        );
    } else {
        if let Some(row) = &login.reassigned {
            let _ = writeln!(out, "on this machine's own login: {}", names.shown(row));
        }
        login.write_lines(names, program, out);
        // With no source in the way, a row of the login's address is all a sync needs.
        if let (Some(_), Some(_), None) = (&login.address, &login.moved_off, &login.refused) {
            let _ = writeln!(
                out,
                "or list that account, and a sync moves the login to it: `{}`",
                Step::Add(cli)
            );
        }
    }
    if login.reassigned.is_some() {
        let _ = writeln!(
            err,
            "aterm harness accounts sync {flag}: {nothing}, so only the default login was followed"
        );
        return ExitCode::SUCCESS;
    }
    let _ = write_default_refusal(login, names, program, &flag, err);
    let _ = writeln!(err, "aterm harness accounts sync {flag}: {nothing}");
    ExitCode::FAILURE
}

/// THE DEFAULT SYNC'S ADMISSION, for a pool syncing by default ([`syncs_by_default`]) with no
/// roster of it here and no source named: `machine`, where the sync takes rows for it, or what
/// the sync is instead. That is [`CliSync::Unlisted`] where neither a preferred row nor a
/// registry-known native default login belongs here, or no machine names it, and
/// [`CliSync::Left`] where this machine kept the pool's
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
    now: impl Into<trace::Now>,
) -> Result<Result<String, CliSync>, String> {
    let now: trace::Now = now.into();
    let listed = match machine.as_deref() {
        Some(m) => fleet.roster.accounts_for_machine(m),
        None => Vec::new(),
    };
    let own = first_sync_native_email(store.cli(), &fleet.roster, store).and_then(|email| {
        alab_default_account(&fleet.roster, None, None, &email, &|account, email| {
            accounts::email_matches(&account.email, email)
        })
    });
    let machine = match machine {
        Some(machine) if !listed.is_empty() || own.is_some() => machine,
        machine => {
            return Ok(Err(CliSync::Unlisted {
                conflicts: fleet.roster.conflicts_for(machine.as_deref()),
                machine,
                source: fleet.roster.source.clone(),
            }));
        }
    };
    // Each row the sync would add: the own login's (`main`, no address) and each of the source's,
    // with the region and address it lists.
    let adds: Vec<(&str, Option<&FleetAccount>)> = std::iter::once((ACCOUNTS_OWN_LABEL, None))
        .chain(
            listed
                .iter()
                .copied()
                .chain(own.as_ref())
                .map(|account| (account.label.as_str(), Some(account))),
        )
        .collect();
    let labels: Vec<&str> = adds.iter().map(|(label, _)| *label).collect();
    if trace::fs::exists(&store.dir)
        && let Some(label) = store.retained(&labels, now)?
    {
        let listed = adds
            .iter()
            .find(|(added, _)| *added == label)
            .and_then(|(_, account)| *account)
            .map(|account| (account.vendor.region, account.email.clone()));
        return Ok(Err(CliSync::Left {
            label: label.to_owned(),
            listed,
        }));
    }
    Ok(Ok(machine))
}

/// The pools syncing by default ([`syncs_by_default`]) that this machine keeps no roster of
/// beside `config` and whose next automatic sync ([`sync_cli_accounts_for_init`]) takes rows or
/// fails: its source lists preferred rows or a registry-known native default login here,
/// and it has not left the pool ([`by_default_admits`]), or that source or the pool's store
/// cannot be read, which
/// the sync then reports. `aterm init`'s `base.ai.accounts` probe names them. It only reads.
#[must_use]
pub fn cli_pools_due(config: &Path, state_dir: &Path, home: &Path, now: i64) -> Vec<Cli> {
    let env = quiet_env(home, state_dir, config, now);
    OTHER_CLIS
        .into_iter()
        .filter(|&cli| {
            syncs_by_default(cli)
                && accounts::roster_beside_for(config, cli)
                    .is_some_and(|roster| !trace::fs::is_file(&roster))
                // A lab not read yet takes no rows: its sync keeps the roster as it stands.
                && lab_pending(cli, &env).is_none()
                && match load_cli_fleet_source(cli, &env, home, None, None, &fetch_source_text) {
                    Ok(None) => false,
                    Ok(Some((fleet, _, machine))) => !matches!(
                        by_default_admits(
                            &accounts::Store::at_for(state_dir, cli, home),
                            &fleet,
                            machine,
                            now,
                        ),
                        Ok(Err(_))
                    ),
                    Err(_) => true,
                }
        })
        .collect()
}

/// [`sync_accounts_for_init`] for the other pooled CLIs ([`OTHER_CLIS`]), each through
/// [`sync_cli_with`] with no source: a CLI whose roster this machine keeps beside `roster_path`,
/// or whose pool syncs by default ([`syncs_by_default`]), syncs from its fleet in the vault;
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
            let synced = sync_cli_with_replan(
                cli,
                (home, state_dir, &config),
                (None, None),
                SyncCaller::Init,
                now,
                (fetch, &ladder),
                || {},
            )
            .and_then(|synced| default_refusal_fails(synced).map_err(SyncError::from));
            let synced = if cli == Cli::Codex && synced.is_ok() {
                let env = quiet_env(home, state_dir, &config, now);
                super::cli::default_completion::complete(
                    &env,
                    None,
                    super::codex_app_login::read_account_email,
                )
                .map_err(SyncError::from)
                .and(synced)
            } else {
                synced
            };
            (cli, synced.map_err(SyncError::into_message))
        })
        .collect()
}

/// `synced`, or the refusal it carries of following the default login, for a caller that prints no
/// receipt: what the sync wrote before it stands and has woken the pool's watchers.
/// Its error is the refusal's logged words ([`MoveRefusal::logged`]), which `aterm init`'s journal
/// keeps: they name each account by its key and slug and say no address.
fn default_refusal_fails(synced: CliSync) -> Result<CliSync, String> {
    let refused = match &synced {
        CliSync::Synced(synced) => synced.default_login.refused.as_ref(),
        CliSync::NoSourceDefaultLogin(login)
        | CliSync::LabPending {
            login: Some(login), ..
        } => login.refused.as_ref(),
        _ => None,
    }
    .map(|refused| refused.logged.clone());
    refused.map_or(Ok(synced), Err)
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
    /// The labels whose logins are made on this machine ([`accounts::enrolled_in`] its home: a
    /// default row is one only while the CLI's default home holds its login file), the set
    /// [`rows_of_other_macs`] reads.
    pub enrolled: BTreeSet<String>,
    /// Its store's state, for each row's condition. Use only when the rows field is Ok.
    pub state: accounts::State,
}

/// The pools of [`OTHER_CLIS`] whose roster stands beside `config`, plus refused
/// stores whose records cannot establish absence. Clean absent pools stay absent.
/// The doctor and dashboard use one recovered snapshot for each pool. Each store's records are
/// read as of `now` ([`trace::Now`]).
#[must_use]
pub fn cli_pools(
    config: &Path,
    state_dir: &Path,
    home: &Path,
    now: impl Into<trace::Now>,
) -> Vec<CliPool> {
    let now: trace::Now = now.into();
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
                .map(|bound| accounts::enrolled_in(cli, bound, home))
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
/// without waiting on its lock (unless
/// [`accounts_sync_off`] holds), and, once its roster stands (this pass's sync may have written
/// it) and its login lifecycle renews ([`sign_ins_on`]), starts one due login's renewal in the
/// background ([`spawn_row_renewal`]); any other CLI whose roster this machine does not keep is
/// left alone. Each CLI's sync, Codex's default login's completion and each CLI's renewal are
/// duties of the pass ([`Duty::PoolSync`], [`Duty::CodexDefault`], [`Duty::PoolRenew`]).
///
/// A pool's sync runs at every pass it is made in: nothing in memory holds one back. On the
/// keeper's thread, a sync that took nothing and wrote nothing (a fleet that lists no row of this
/// machine's, a pool this machine left, no source, a refused input) is kept as its duty's
/// verdict, so it runs again only once something it read or compared moves; a retryable failure
/// is a retry (`SyncError::retry`), which the keeper runs again on its back-off.
fn upkeep_pools(
    home: &Path,
    state_dir: &Path,
    (roster_path, now, step): (&Path, trace::Now, UpkeepStep),
    fx: &UpkeepEffects<'_>,
) {
    let config = config_beside(roster_path);
    let syncs = step.syncs() && !accounts_sync_off(Some(&config));
    for cli in OTHER_CLIS {
        if cancelled(fx) {
            return;
        }
        let Some(path) = accounts::roster_beside_for(&config, cli) else {
            continue;
        };
        let mut has_roster = trace::fs::is_file(&path);
        if syncs && (syncs_by_default(cli) || has_roster) {
            // Whether the roster stands after the sync; `None` where the work was cancelled
            // under it.
            let synced = duty(fx, Duty::PoolSync(cli), has_roster, || {
                super::accounts_upkeep_counts::count(
                    super::accounts_upkeep_counts::Event::PoolSync,
                );
                // What the sync came to is read by nothing but whether the roster stands.
                let _ = sync_cli_with_replan(
                    cli,
                    (home, state_dir, &config),
                    (None, None),
                    SyncCaller::Upkeep,
                    now,
                    (fx.fetch, &[]),
                    || {},
                );
                if cancelled(fx) {
                    return None;
                }
                Some(trace::fs::is_file(&path))
            });
            let Some(synced) = synced else {
                return;
            };
            has_roster = synced;
        }
        if !has_roster {
            continue;
        }
        if cli == Cli::Codex && sign_ins_on(&config, cli) {
            duty(fx, Duty::CodexDefault, (), || {
                let env = traced_env(home, state_dir, &config, now);
                let _ = super::cli::default_completion::complete(
                    &env,
                    None,
                    super::codex_app_login::read_account_email,
                );
            });
        }
        let renews = step.renews() && sign_ins_on(&config, cli);
        if step.renews() {
            duty(fx, Duty::PoolRenew(cli), renews, || {
                if resume_pending_pin_for(cli, home, state_dir, &path, now, fx).is_none() && renews
                {
                    let _ = renew_pool_due(cli, home, state_dir, &path, now, fx);
                }
            });
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
    now: impl Into<trace::Now>,
    fx: &UpkeepEffects<'_>,
) -> bool {
    let now: trace::Now = now.into();
    if fx
        .scope
        .as_ref()
        .is_some_and(super::account_work::Scope::cancelled)
    {
        return false;
    }
    if let Some(pending) = resume_pending_pin_for(cli, home, state_dir, roster_path, now, fx) {
        return pending.progressed();
    }
    let store = accounts::Store::at_for(state_dir, cli, home);
    let recovered = recover_interrupted_staging_renewals_for(
        cli,
        &store,
        roster_path,
        fx.renew_guard,
        now,
        shared_renewal::CheckpointAuthority::background(
            Some(home),
            fx.shared.checkpoint_authority,
            roster_path,
        ),
    );
    if !cli.renews_headlessly() {
        return recovered;
    }
    // Another CLI's rows: the liveness reading is of Claude Code's logins alone, so its pass
    // reads no witness.
    let Ok(candidates) = read_renewal_candidates(
        cli,
        &store,
        (home, state_dir),
        roster_path,
        now,
        &super::login_adoption::Pass::default(),
    ) else {
        return recovered;
    };
    for (_, row) in candidates.tiered {
        // A candidate: its sign-in and its renewal compare and stamp the reading as a number.
        let Some(sign_in) = upkeep_sign_in(
            cli,
            (&candidates.records, &candidates.others),
            &row,
            now.raw(),
        ) else {
            continue;
        };
        if spawn_row_renewal(
            cli,
            home,
            state_dir,
            roster_path,
            candidates.records.raw_roster.clone(),
            &candidates.records.logins,
            (row, sign_in),
            (None, false),
            now.raw(),
            fx.renew_guard,
            fx.spawn_renewal,
            None,
            fx.scope.as_ref(),
            &mut None,
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
/// home (`/Applications`, `PATH`), and the monotonic clock its holds are kept in. The window's
/// guard ([`AUTO_RENEW_GUARD`]) reads the machine ([`MachineReads::live`]); every other guard, a
/// test's among them, reads nothing of it but the clock ([`MachineReads::default`]), so what it
/// decides comes from its home and store alone, and a test may script that clock.
#[derive(Clone, Copy, Hash)]
struct MachineReads {
    /// The monotonic clock the guard's holds are kept in and judged by ([`Instant::now`]): each
    /// permit's cooldown, each park and bot check ([`AutoRenewPermit::claim_with_context`]) and
    /// the sign-in ladder's backoffs and vault wait ([`super::sign_in_ladder::Ladder`]). Every
    /// hold the guard keeps is set at a reading of it, and every one it answers is judged at a
    /// reading of it through `aterm_uds::trace::Since`, so a traced run held by one records when
    /// it ends.
    clock: fn() -> Instant,
    /// One environment variable.
    var: fn(&str) -> Option<std::ffi::OsString>,
    /// Whether a browser outside the home counts.
    system_browsers: bool,
    /// Whether this machine has a desk aterm's sign-in window can open on: macOS.
    desk: bool,
    /// Whether another Mac's fleet drive typed into the session this process runs in
    /// ([`super::cli::AccountsHost::driven_over_the_fleet`]).
    driven: fn() -> bool,
    /// Whether this process's session is a desktop one aterm's sign-in window opens in
    /// (`aterm_signin_window::desktop_session`): a token the upkeep makes with nobody there
    /// opens a page only where it does ([`upkeep_tokens`]).
    #[cfg_attr(not(unix), allow(dead_code))]
    graphic: fn() -> bool,
    /// This machine's platform id ([`aterm_types::machine_name::PlatformId`]), which
    /// spreads the token upkeep's starts over the fleet ([`token_delay_s`]).
    #[cfg_attr(not(unix), allow(dead_code))]
    machine: fn() -> Option<String>,
    /// The accounts host a token the upkeep makes runs under ([`upkeep_tokens`]); `None`: the
    /// upkeep makes no token.
    #[cfg_attr(not(unix), allow(dead_code))]
    tokens: Option<TokenHost>,
}

/// [`MachineReads::tokens`]: the accounts host of a token capture the upkeep starts, in the
/// upkeep's account-work scope.
type TokenHost = fn(Option<super::account_work::Scope>) -> Box<dyn super::cli::AccountsHost>;

impl Default for MachineReads {
    fn default() -> Self {
        Self {
            clock: Instant::now,
            var: |_| None,
            system_browsers: false,
            desk: false,
            driven: || false,
            graphic: || false,
            machine: || None,
            tokens: None,
        }
    }
}

impl MachineReads {
    /// The process's environment, every installed browser, this machine's desk and what its
    /// aterm instances say of this process's session, asked once, and the monotonic clock.
    fn live() -> Self {
        Self {
            clock: Instant::now,
            var: |name| std::env::var_os(name),
            system_browsers: true,
            desk: cfg!(target_os = "macos"),
            driven: || {
                static DRIVEN: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
                *DRIVEN.get_or_init(super::fleet_session::an_instance_says_another_mac_drives)
            },
            #[cfg(target_os = "macos")]
            graphic: aterm_signin_window::desktop_session,
            #[cfg(not(target_os = "macos"))]
            graphic: || false,
            machine: || aterm_types::machine_name::PlatformId::Host.platform_id(),
            tokens: Some(|scope| Box::new(super::cli::LiveAccounts::background(scope))),
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
fn guard_reads(guard: &std::sync::Arc<Watched<AutoRenewGuard>>) -> MachineReads {
    guard
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .reads
}

/// `guard`'s clock now ([`MachineReads::clock`]). It takes the guard's lock for the read, so it
/// is never called while this thread holds that lock.
fn guard_now(guard: &std::sync::Arc<Watched<AutoRenewGuard>>) -> Instant {
    (guard_reads(guard).clock)()
}

/// What a window's renewals and preparations hold in this process, kept [`Watched`] behind an
/// `Arc` ([`AutoRenewGuard::shared`]): a change of any field a guard leaves otherwise (a permit
/// claimed or dropped, a row parked, a backoff set, an owner recorded) moves its generation and
/// rings the trace's bell, on whichever thread it is made. A preparation pass that admits nothing
/// leaves it as it found it ([`shared_renewal::PreparationOrder`]'s hash). An owner's claim token
/// counts by the claim it carries, not by its identity: an `Arc<()>` hashes nothing.
#[derive(Default, Hash)]
struct AutoRenewGuard {
    /// What this guard's renewals read of the machine ([`MachineReads`]), its clock among them:
    /// every `Instant` below is a reading of that clock.
    reads: MachineReads,
    in_flight: bool,
    preparation_order: shared_renewal::PreparationOrder,
    /// When each key's last background attempt was admitted: none is admitted again before
    /// [`PERMIT_COOLDOWN`] has passed, unless its inputs changed
    /// ([`AutoRenewPermit::claim_with_context`]).
    last_attempt: BTreeMap<String, Instant>,
    shared_preparations: BTreeMap<String, (accounts::shared::Source, [u8; 32])>,
    native_preparations: BTreeMap<String, [u8; 32]>,
    /// The line native preparation last logged, `None` after a pass that said nothing
    /// ([`shared_renewal::prepare_native`]), so a lasting refusal is logged once per change.
    native_said: Option<String>,
    mailbox_mtimes: BTreeMap<String, std::time::SystemTime>,
    /// The rows whose last background renewal stopped where only a person can go on and no bot
    /// protection stopped it ([`super::cli::Renewal::NeedsPerson`]), and since when: none is
    /// tried again in the background before [`PERSON_BACKOFF`] has passed. A bot protection's
    /// stop holds on no clock, on record ([`left_to_a_person`]) or in this process
    /// (`unrecorded_checks`).
    parked: BTreeMap<String, Instant>,
    /// The newest `(at, same_second_count)` signed-in `accounts browser` window event already used
    /// to lift a row's background hold once.
    browser_unparked: BTreeMap<String, (Option<accounts::WorkflowToken>, i64, usize)>,
    /// The automatic sign-in ladder's backoffs and vault clock ([`super::sign_in_ladder::Ladder`]).
    ladder: super::sign_in_ladder::Ladder,
    /// The bot protections this process met whose record it could not write yet
    /// ([`hold_for_a_person`]), by the renewal's key (a Claude row's label, another CLI's
    /// `<cli>:<label>`): each holds its row, on no clock and at every admission
    /// ([`AutoRenewPermit::claim_with_context`]), until the record is written, which every ladder
    /// pass and every upkeep pass of the row tries again ([`write_check_stop`]), a login is
    /// published for it, or a signed-in `accounts browser` window answers it.
    unrecorded_checks: BTreeMap<String, PendingCheck>,
    /// The default row and the sign-out on record the default login's one check last ran for
    /// ([`heal_default_login_in_upkeep`]): it runs once per pair in this process.
    default_checked: Option<(String, i64)>,
    /// Attribution only; these do not participate in admission or retry policy.
    activity_owners: BTreeMap<String, LoginOwner>,
    ladder_owners: BTreeMap<String, LoginOwner>,
    active_login: Option<ActiveLogin>,
    /// Manual operation observations, partitioned by exact account namespace and subject.
    manual_selections: BTreeMap<(PathBuf, Cli, ManualSubject), QueuedOwner>,
    /// The ladder's own sign-ins that run now, by label ([`RunMark`]): what a lift of the
    /// account ends ([`supersede_unattended`]). A person's sign-in is never here.
    unattended_runs: BTreeMap<String, UnattendedRun>,
    /// Each row a file shows live that the last adoption step could not lift, by label, with
    /// the reason as it was logged ([`settle_adoption`]): said once per change.
    adoption_refused: BTreeMap<String, String>,
    /// The last adoption step could not keep its baselines, and that was logged
    /// ([`settle_adoption`]).
    baselines_unkept_said: bool,
    /// What the token upkeep remembers in this process ([`upkeep_tokens`]).
    #[cfg(unix)]
    token_tier: TokenTier,
}

/// What the token upkeep (plan package FL-138, [`upkeep_tokens`]) remembers in this process.
#[cfg(unix)]
#[derive(Default, Hash)]
struct TokenTier {
    /// The gap each row was found with, by label, while it stands. Which gaps a capture already
    /// ran for is on record, not here ([`super::token_steps::Tried`]): no restart runs one again.
    found: BTreeMap<String, TokenGap>,
    /// A capture the upkeep started runs now.
    running: bool,
    /// This Mac's vault route was not seated at the last look, and the log said so.
    not_seated: bool,
}

/// A row's gap the token upkeep found ([`TokenTier::found`]).
#[cfg(unix)]
#[derive(Hash)]
struct TokenGap {
    finding: super::accounts_tokens::Finding,
    /// When it was found (Unix seconds): its capture waits [`token_wait_s`] from then.
    at: i64,
    /// When a pass first found that wait over: only a vault refresh that started then or later
    /// reads the vault fresh enough to make the token on.
    ripe: Option<Instant>,
}

#[derive(Clone, Debug, Hash)]
struct LoginOwner {
    store: PathBuf,
    cli: Cli,
    label: String,
    observed_at: i64,
    token: std::sync::Arc<()>,
    success_generation: Option<(u64, u64)>,
    diagnostic: Option<LoginDiagnostic>,
    identity: Option<LoginIdentity>,
    workflow: Option<accounts::WorkflowToken>,
}

impl LoginOwner {
    fn new(store: &accounts::Store, cli: Cli, label: &str, observed_at: i64) -> Self {
        Self {
            store: store.dir.clone(),
            cli,
            label: label.into(),
            observed_at,
            token: std::sync::Arc::new(()),
            success_generation: None,
            diagnostic: None,
            identity: None,
            workflow: None,
        }
    }

    fn observing(mut self, logins: &accounts::Logins) -> Self {
        self.success_generation = Some((
            logins
                .get(&self.label)
                .map_or(0, |binding| binding.last_success_generation),
            logins
                .sign_in_receipts
                .get(&self.label)
                .copied()
                .unwrap_or(0),
        ));
        self
    }

    fn for_row(mut self, row: &accounts::Row) -> Self {
        self.identity = Some(LoginIdentity::of(row));
        self
    }

    fn for_workflow(mut self, raw: &accounts::Row, logins: &accounts::Logins) -> Self {
        self.workflow = accounts::current_workflow(raw, logins);
        self
    }

    fn matches(&self, store: &accounts::Store, cli: Cli) -> bool {
        self.store == store.dir && self.cli == cli
    }
}

/// Claim identities captured before publication. Holding a token prevents a
/// later claim from reusing its identity, even when their wall times coincide.
#[derive(Clone)]
struct LoginClaims {
    activity: BTreeMap<String, LoginOwner>,
    ladder: BTreeMap<String, LoginOwner>,
}

impl LoginClaims {
    fn capture(guard: &std::sync::Arc<Watched<AutoRenewGuard>>) -> Self {
        let state = guard
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Self::from_state(&state)
    }

    fn from_state(state: &AutoRenewGuard) -> Self {
        Self {
            activity: state.activity_owners.clone(),
            ladder: state.ladder_owners.clone(),
        }
    }
}

#[derive(Hash)]
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
    /// A prior provider invocation, independent of active work and retry policy.
    pub diagnostic: Option<LoginDiagnostic>,
    /// Exact observed roster identity; absent means the label alone is unconfirmed.
    pub identity: Option<LoginIdentity>,
    /// The current manual request's observed FIFO place, never provider activity or an ETA.
    pub queued: Option<QueuedSignin>,
}

/// A queue observation fenced to one exact persisted manual selection.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct QueuedSignin {
    selection: accounts::PendingSelection,
    position: accounts::SigninPosition,
}

impl QueuedSignin {
    /// Whether this observation belongs to the exact currently persisted intent.
    #[must_use]
    pub fn matches_selection(&self, selection: &accounts::PendingSelection) -> bool {
        self.selection == *selection
    }

    /// Live owners ahead and the observed blocker. Zero ahead is not a grant.
    #[must_use]
    pub const fn position(&self) -> accounts::SigninPosition {
        self.position
    }

    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn for_test(
        selection: accounts::PendingSelection,
        position: accounts::SigninPosition,
    ) -> Self {
        Self {
            selection,
            position,
        }
    }
}

#[derive(Clone, Debug, Hash)]
struct QueuedOwner {
    selection: accounts::PendingSelection,
    token: std::sync::Arc<()>,
    position: Option<accounts::SigninPosition>,
    identity: Option<LoginIdentity>,
    observed_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum ManualSubject {
    Account(String),
    Next,
}

impl QueuedOwner {
    fn observation(&self) -> Option<QueuedSignin> {
        self.position.map(|position| QueuedSignin {
            selection: self.selection.clone(),
            position,
        })
    }
}

/// The complete roster row captured by an activity owner. Its contents are not
/// diagnostics: readers can compare them without disclosing login paths or addresses.
#[derive(Clone, PartialEq, Eq)]
pub struct LoginIdentity {
    row: accounts::Row,
}

impl std::fmt::Debug for LoginIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoginIdentity").finish_non_exhaustive()
    }
}

impl std::hash::Hash for LoginIdentity {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.row.label.hash(state);
    }
}

impl LoginIdentity {
    fn of(row: &accounts::Row) -> Self {
        Self { row: row.clone() }
    }

    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn for_test(row: &accounts::Row) -> Self {
        Self::of(row)
    }
}

impl LoginActivity {
    /// A retained or live observation belongs only to this exact captured row.
    #[must_use]
    pub fn matches_row(&self, row: &accounts::Row) -> bool {
        self.label == row.label
            && self
                .identity
                .as_ref()
                .is_some_and(|identity| identity.row == *row)
    }
}

/// A safe historical provider observation bound to the exact captured roster row.
/// No provider output, URL, credential or filesystem path is exposed by its formatter.
#[derive(Clone, PartialEq, Eq)]
pub struct LoginDiagnostic {
    pub completed_at: i64,
    row: accounts::Row,
    /// How the run ended, which with `observation` says it ([`Self::words`]).
    renewal: super::cli::Renewal,
    observation: super::cli::RenewalObservation,
}

impl std::fmt::Debug for LoginDiagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoginDiagnostic")
            .field("completed_at", &self.completed_at)
            .field("renewal", &self.renewal)
            .field("observation", &self.observation)
            .finish_non_exhaustive()
    }
}

impl std::hash::Hash for LoginDiagnostic {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.completed_at.hash(state);
        self.row.label.hash(state);
        self.renewal.hash(state);
        self.observation.hash(state);
    }
}

impl LoginDiagnostic {
    /// The account label captured by this historical attempt.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.row.label
    }

    /// How the run ended, in the words its row's record says too
    /// (`cli::RenewalObservation::ended`, the one wording of a run's end).
    #[must_use]
    pub fn words(&self) -> String {
        self.observation.ended(self.renewal)
    }

    /// A same-label replacement is not the account this invocation observed.
    #[must_use]
    pub fn matches_row(&self, row: &accounts::Row) -> bool {
        &self.row == row
    }

    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn from_exit_for_test(
        row: &accounts::Row,
        completed_at: i64,
        code: i32,
        met: Option<super::open_url::Met>,
    ) -> Self {
        Self {
            completed_at,
            row: row.clone(),
            renewal: super::cli::Renewal::authentication_failed(Some(code)),
            observation: super::cli::RenewalObservation::from_outcome(&Ok(
                super::cli::RenewalExit { code, met },
            )),
        }
    }
}

/// One process-local observation of at most two exact account namespaces.
/// Rows remain scoped even when both namespaces reuse the same account label.
#[derive(Clone)]
pub struct LoginActivityBatch {
    scopes: [Option<ScopedLoginActivity>; 2],
}

#[derive(Clone)]
struct ScopedLoginActivity {
    store: PathBuf,
    cli: Cli,
    rows: Vec<LoginActivity>,
    queued_next: Option<QueuedSignin>,
}

impl LoginActivityBatch {
    /// A next-selection queue observation in this exact requested namespace.
    #[must_use]
    pub fn queued_next_for(&self, store: &accounts::Store, cli: Cli) -> Option<&QueuedSignin> {
        self.scopes
            .iter()
            .flatten()
            .find(|scope| scope.store == store.dir && scope.cli == cli)
            .and_then(|scope| scope.queued_next.as_ref())
    }

    /// Attach an exact next-selection observation to an already requested fixture namespace.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn with_queued_next_for_test(
        mut self,
        store: &accounts::Store,
        cli: Cli,
        queued: QueuedSignin,
    ) -> Self {
        if let Some(scope) = self
            .scopes
            .iter_mut()
            .flatten()
            .find(|scope| scope.store == store.dir && scope.cli == cli)
        {
            scope.queued_next = Some(queued);
        }
        self
    }
    /// The requested namespace's observation. An unrequested namespace is `None`,
    /// distinct from a requested namespace with no attributed records.
    #[must_use]
    pub fn for_store(&self, store: &accounts::Store, cli: Cli) -> Option<&[LoginActivity]> {
        self.scopes
            .iter()
            .flatten()
            .find(|scope| scope.store == store.dir && scope.cli == cli)
            .map(|scope| scope.rows.as_slice())
    }

    /// A scoped observation fixture; duplicate namespaces must describe the same snapshot.
    ///
    /// # Panics
    /// Duplicate namespaces carry different activity rows.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn for_test(stores: [Option<(&accounts::Store, Vec<LoginActivity>)>; 2], cli: Cli) -> Self {
        let mut batch = Self {
            scopes: [None, None],
        };
        for (index, requested) in stores.into_iter().enumerate() {
            let Some((store, rows)) = requested else {
                continue;
            };
            if let Some(previous) = batch.for_store(store, cli) {
                assert_eq!(
                    previous,
                    rows.as_slice(),
                    "one namespace has one activity snapshot"
                );
                continue;
            }
            batch.scopes[index] = Some(ScopedLoginActivity {
                store: store.dir.clone(),
                cli,
                rows,
                queued_next: None,
            });
        }
        batch
    }
}

/// Read only this process's attributed work for both requested stores under one try-lock
/// and monotonic clock sample. No filesystem, route, policy or credential operation runs.
/// `None` means the observation lock is busy; there is no partially observed batch.
#[must_use]
pub fn login_activity_batch(
    stores: [Option<&accounts::Store>; 2],
    cli: Cli,
    now: i64,
) -> Option<LoginActivityBatch> {
    login_activity_batch_using(AUTO_RENEW_GUARD.as_ref(), stores, cli, now, Instant::now)
}

fn login_activity_batch_using(
    observation: &Watched<AutoRenewGuard>,
    stores: [Option<&accounts::Store>; 2],
    cli: Cli,
    now: i64,
    clock: impl FnOnce() -> Instant,
) -> Option<LoginActivityBatch> {
    let guard = match observation.try_lock() {
        Ok(guard) => guard,
        Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner(),
        Err(std::sync::TryLockError::WouldBlock) => return None,
    };
    Some(login_activity_batch_of(&guard, stores, cli, now, clock()))
}

fn login_activity_batch_of(
    guard: &AutoRenewGuard,
    stores: [Option<&accounts::Store>; 2],
    cli: Cli,
    now: i64,
    clock: Instant,
) -> LoginActivityBatch {
    let mut batch = LoginActivityBatch {
        scopes: [None, None],
    };
    for (index, requested) in stores.into_iter().enumerate() {
        let Some(store) = requested else {
            continue;
        };
        if batch.for_store(store, cli).is_some() {
            continue;
        }
        batch.scopes[index] = Some(ScopedLoginActivity {
            store: store.dir.clone(),
            cli,
            rows: login_activity_of(guard, store, cli, now, clock),
            queued_next: guard
                .manual_selections
                .get(&(store.dir.clone(), cli, ManualSubject::Next))
                .and_then(QueuedOwner::observation),
        });
    }
    batch
}

/// Read only this process's attributed work for the exact store and CLI. No filesystem
/// read, route probe, policy decision or credential operation is performed.
/// Returns `None` while another thread holds the observation lock.
#[must_use]
pub fn login_activity(store: &accounts::Store, cli: Cli, now: i64) -> Option<Vec<LoginActivity>> {
    let [requested, _] = login_activity_batch([Some(store), None], cli, now)?.scopes;
    requested.map(|scope| scope.rows)
}

/// [`login_activity`] for a decision off the window's thread: it waits for the observation
/// lock, which each holder takes for one step on this process's own record and no renewal
/// holds while it runs, so the rows it answers are the record's and never another thread's
/// instant on the lock.
#[must_use]
pub fn login_activity_waited(store: &accounts::Store, cli: Cli, now: i64) -> Vec<LoginActivity> {
    let guard = AUTO_RENEW_GUARD
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let clock = (guard.reads.clock)();
    login_activity_of(&guard, store, cli, now, clock)
}

/// Why a scope armed around [`login_activity_of`] is not kept where a hold it names stands: the
/// time left to that hold moves with the clock, and no comparison states it.
const HOLD_LEFT_RAW: &str = "the time left to a renewal's hold was read raw";

/// This process's attributed work for `store` and `cli` at wall `now` and guard clock reading
/// `clock`: each row's next try is the latest of its holds, the time left to each judged at
/// `clock`. Whether a hold still stands is judged through `aterm_uds::trace::Since`; the time
/// left to one that does is read raw ([`HOLD_LEFT_RAW`]).
fn login_activity_of(
    guard: &AutoRenewGuard,
    store: &accounts::Store,
    cli: Cli,
    now: i64,
    clock: Instant,
) -> Vec<LoginActivity> {
    let mut rows = Vec::<LoginActivity>::new();
    let mut add = |owner: &LoginOwner, until: Option<Instant>| {
        if !owner.matches(store, cli) {
            return;
        }
        let Some(until) = until
            .filter(|until| !trace::Since::new(*until).elapsed_at_least(Duration::ZERO, clock))
        else {
            return;
        };
        trace::poison(HOLD_LEFT_RAW);
        let remaining = until.saturating_duration_since(clock);
        let seconds = remaining
            .as_secs()
            .saturating_add(u64::from(remaining.subsec_nanos() > 0));
        let next = now.saturating_add(i64::try_from(seconds).unwrap_or(i64::MAX));
        let row = activity_row(&mut rows, owner);
        row.observed_at = row.observed_at.max(owner.observed_at);
        // With unchanged inputs, every hold on this captured account must clear.
        row.next_at = Some(row.next_at.map_or(next, |held| held.max(next)));
        if let Some(diagnostic) = &owner.diagnostic
            && row
                .diagnostic
                .as_ref()
                .is_none_or(|old| old.completed_at <= diagnostic.completed_at)
        {
            row.diagnostic = Some(diagnostic.clone());
        }
    };
    for (key, owner) in &guard.activity_owners {
        // A bot protection this process met and has not recorded holds the row on no clock: it
        // has no next try to show.
        if guard.unrecorded_checks.contains_key(key) {
            continue;
        }
        // A claim is refused while any of these holds stands, so the row's next try is at the
        // latest of them.
        let held = [
            guard.last_attempt.get(key).map(|at| *at + PERMIT_COOLDOWN),
            guard.parked.get(key).map(|at| *at + PERSON_BACKOFF),
        ]
        .into_iter()
        .flatten()
        .max();
        add(owner, held);
    }
    for (key, owner) in &guard.ladder_owners {
        add(owner, guard.ladder.retry_at(key, clock));
    }
    for ((directory, provider, subject), queued) in &guard.manual_selections {
        if directory != &store.dir || *provider != cli {
            continue;
        }
        let ManualSubject::Account(label) = subject else {
            continue;
        };
        if let Some(observation) = queued.observation() {
            let mut owner = LoginOwner::new(store, cli, label, queued.observed_at);
            owner.identity.clone_from(&queued.identity);
            activity_row(&mut rows, &owner).queued = Some(observation);
        }
    }
    if let Some(active) = &guard.active_login
        && active.owner.matches(store, cli)
    {
        let row = activity_row(&mut rows, &active.owner);
        row.active = true;
        row.route.clone_from(&active.route);
        row.observed_at = active.owner.observed_at;
    }
    rows.sort_by(|a, b| a.label.cmp(&b.label));
    rows
}

fn activity_row<'a>(rows: &'a mut Vec<LoginActivity>, owner: &LoginOwner) -> &'a mut LoginActivity {
    // A label may be reused while its previous worker or retry remains. Never merge
    // different identities into one apparently current activity or retry deadline.
    let at = rows
        .iter()
        .position(|row| row.label == owner.label && row.identity == owner.identity)
        .unwrap_or_else(|| {
            rows.push(LoginActivity {
                label: owner.label.clone(),
                route: None,
                active: false,
                observed_at: owner.observed_at,
                next_at: None,
                diagnostic: None,
                identity: owner.identity.clone(),
                queued: None,
            });
            rows.len() - 1
        });
    &mut rows[at]
}

/// One process's exact manual-selection owner. The kernel ticket orders work;
/// this token only prevents duplicate local workers and fences activity updates.
pub(in crate::harness) struct ManualSelectionOwner {
    guard: std::sync::Arc<Watched<AutoRenewGuard>>,
    key: (PathBuf, Cli, ManualSubject),
    selection: accounts::PendingSelection,
    token: std::sync::Arc<()>,
}

impl std::fmt::Debug for ManualSelectionOwner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ManualSelectionOwner")
            .finish_non_exhaustive()
    }
}

impl ManualSelectionOwner {
    fn claim(
        guard: &std::sync::Arc<Watched<AutoRenewGuard>>,
        store: &accounts::Store,
        cli: Cli,
        selection: accounts::PendingSelection,
        row: Option<&accounts::Row>,
        now: i64,
    ) -> Result<Self, RenewalDispatch> {
        let subject = selection.account().map_or(ManualSubject::Next, |request| {
            ManualSubject::Account(request.label().to_owned())
        });
        if selection
            .account()
            .is_some_and(|request| row.is_some_and(|row| row.label != request.label()))
        {
            return Err(RenewalDispatch::Fenced);
        }
        let token = std::sync::Arc::new(());
        let key = (store.dir.clone(), cli, subject);
        let owned = Self {
            guard: guard.clone(),
            key: key.clone(),
            selection: selection.clone(),
            token: token.clone(),
        };
        let mut result = Err(RenewalDispatch::Superseded);
        // The writer serializes this observation with selection replacement. No
        // queue gate or provider work is held while the local owner book is read.
        store
            .transact_within(&accounts::LOCK_LADDER, now, &mut None, |state| {
                if state.pending_pin.as_ref() != Some(&selection) {
                    return Ok(Vec::new());
                }
                let mut state = guard
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if state
                    .manual_selections
                    .get(&key)
                    .is_some_and(|owner| owner.selection == selection)
                {
                    result = Err(RenewalDispatch::Held);
                    return Ok(Vec::new());
                }
                let queued = QueuedOwner {
                    selection: selection.clone(),
                    token: token.clone(),
                    position: None,
                    identity: row.map(LoginIdentity::of),
                    observed_at: now,
                };
                state.manual_selections.insert(key.clone(), queued);
                result = Ok(());
                Ok(Vec::new())
            })
            .map_err(|why| RenewalDispatch::store_refusal(&why))?;
        result?;
        Ok(owned)
    }

    fn observation_mut<'a>(&self, state: &'a mut AutoRenewGuard) -> Option<&'a mut QueuedOwner> {
        let owner = state.manual_selections.get_mut(&self.key)?;
        std::sync::Arc::ptr_eq(&owner.token, &self.token).then_some(owner)
    }

    fn position(&self, position: Option<accounts::SigninPosition>) {
        let mut state = self
            .guard
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let changed = self.observation_mut(&mut state).is_some_and(|owner| {
            let changed = owner.position != position;
            owner.position = position;
            changed
        });
        drop(state);
        if changed {
            login_activity_moved();
        }
    }

    fn owns(&self) -> bool {
        self.observation_mut(
            &mut self
                .guard
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
        .is_some()
    }

    pub(in crate::harness) fn selection(&self) -> &accounts::PendingSelection {
        &self.selection
    }
}

impl Drop for ManualSelectionOwner {
    fn drop(&mut self) {
        let mut state = self
            .guard
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.observation_mut(&mut state).is_none() {
            return;
        }
        state.manual_selections.remove(&self.key);
        drop(state);
        if !std::thread::panicking() {
            login_activity_moved();
        }
    }
}

struct LoginActivityGuard {
    guard: std::sync::Arc<Watched<AutoRenewGuard>>,
    token: std::sync::Arc<()>,
    owner_token: Option<std::sync::Arc<()>>,
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
    guard: &std::sync::Arc<Watched<AutoRenewGuard>>,
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
        state.unrecorded_checks.remove(key);
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

/// Withdraw superseded retries and expire current person holds, preserving active and newer claims.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountWorkflowRuntime",
        action = "Read",
        project = "aterm_agent::harness::accounts_sync::workflow_runtime_tests::runtime_transition"
    )
)]
fn reconcile_workflow_retries(
    guard: &std::sync::Arc<Watched<AutoRenewGuard>>,
    store: &accounts::Store,
    cli: Cli,
    roster_path: &Path,
    captured: (&accounts::Row, &accounts::Row, &accounts::Logins),
    now: i64,
) {
    reconcile_workflow_retries_with_read(guard, store, cli, roster_path, captured, now, || {});
}

fn reconcile_workflow_retries_with_read(
    guard: &std::sync::Arc<Watched<AutoRenewGuard>>,
    store: &accounts::Store,
    cli: Cli,
    roster_path: &Path,
    (raw, row, logins): (&accounts::Row, &accounts::Row, &accounts::Logins),
    now: i64,
    after_read: impl FnOnce(),
) {
    let claims = LoginClaims::capture(guard);
    let Ok(records) = store.read_account_records(Some(roster_path), now) else {
        return;
    };
    after_read();
    if records.raw_roster.row(&raw.label) != Some(raw)
        || records.roster.row(&raw.label) != Some(row)
        || accounts::current_workflow(raw, logins).is_none_or(|captured| {
            !accounts::workflow_token_current(raw, &records.logins, captured)
        })
    {
        return;
    }
    let logins = &records.logins;
    let stale = |owner: &LoginOwner| {
        owner.matches(store, cli)
            && owner.label == raw.label
            && owner.workflow.is_some_and(|token| {
                !accounts::workflow_token_current(raw, logins, token)
                    || owner
                        .identity
                        .as_ref()
                        .is_some_and(|identity| identity.row != *row)
            })
    };
    let mut state = guard
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if state.in_flight {
        return;
    }
    let activity: Vec<_> = state
        .activity_owners
        .iter()
        .filter(|(key, owner)| {
            stale(owner)
                && claims
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
            stale(owner)
                && claims
                    .ladder
                    .get(*key)
                    .is_some_and(|before| std::sync::Arc::ptr_eq(&before.token, &owner.token))
        })
        .map(|(key, _)| key.clone())
        .collect();
    // A bot protection met and not yet recorded is the account's, and outlasts the workflow
    // ([`accounts::CheckStop`]): it stays.
    for key in &activity {
        state.last_attempt.remove(key);
        state.parked.remove(key);
        state.browser_unparked.remove(key);
        state.activity_owners.remove(key);
    }
    for key in &ladder {
        state.ladder.signed_in(key);
        state.ladder_owners.remove(key);
    }
    // Sleep can advance the task's wall clock while leaving Instant's person hold young.
    // Only its still-current, pre-read owner may release that hold; a check and retry clocks
    // remain.
    let expired: Vec<_> = if accounts::current_sign_in_task(raw, logins)
        .is_some_and(|task| now.saturating_sub(task.at) >= PERSON_BACKOFF_SECS)
    {
        state
            .activity_owners
            .iter()
            .filter(|(key, owner)| {
                state.parked.contains_key(*key)
                    && owner.matches(store, cli)
                    && owner.label == raw.label
                    && owner
                        .identity
                        .as_ref()
                        .is_some_and(|identity| identity.row == *row)
                    && owner
                        .workflow
                        .is_some_and(|token| accounts::workflow_token_current(raw, logins, token))
                    && claims
                        .activity
                        .get(*key)
                        .is_some_and(|before| std::sync::Arc::ptr_eq(&before.token, &owner.token))
            })
            .map(|(key, _)| key.clone())
            .collect()
    } else {
        Vec::new()
    };
    for key in &expired {
        state.parked.remove(key);
    }
    drop(state);
    if !activity.is_empty() || !ladder.is_empty() || !expired.is_empty() {
        login_activity_moved();
    }
}

/// Admit an externally edited policy before capturing its new workflow for a retry.
fn prepare_retry_workflow(
    store: &accounts::Store,
    roster_path: &Path,
    raw: &accounts::Row,
    row: &accounts::Row,
    observed: &accounts::Logins,
    now: i64,
) -> Result<Option<accounts::AccountRecords>, String> {
    if accounts::current_workflow(raw, observed).is_some() {
        return Ok(None);
    }
    let parent = accounts::RenewalParent::capture(observed, &raw.label);
    store.transact_pool_logins_within(&[], roster_path, now, &mut None, |current, _, logins| {
        if current.row(&raw.label) != Some(raw)
            || accounts::bound(current, logins).row(&raw.label) != Some(row)
        {
            return Err("sign-in workflow changed before retry admission".into());
        }
        parent.require_current(logins, &raw.label)?;
        accounts::admit_workflow(logins, &raw.label)?;
        Ok(Vec::new())
    })?;
    let records = store
        .read_account_records(Some(roster_path), now)
        .map_err(|why| format!("{why:?}"))?;
    if records.raw_roster.row(&raw.label) != Some(raw)
        || records.roster.row(&raw.label) != Some(row)
        || !parent.matches(&records.logins, &raw.label)
        || accounts::current_workflow(raw, &records.logins).is_none()
    {
        return Err("sign-in workflow changed during retry admission".into());
    }
    Ok(Some(records))
}

/// Replayed and remote-process publications remain observable in the committed
/// records. Clear only claims that began before that successful publication or
/// in-place sign-in receipt; neither marker changes a credential fence here.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountPublicationActivity",
        action = "Observe",
        project = "aterm_agent::harness::accounts_sync::publication_activity_tests::publication_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSignBackReceipt",
        action = "Observe",
        project = "aterm_agent::harness::accounts_sync::signback_receipt_tests::signback_transition"
    )
)]
fn note_observed_publications(
    guard: &std::sync::Arc<Watched<AutoRenewGuard>>,
    mut claims: LoginClaims,
    store: &accounts::Store,
    cli: Cli,
    logins: &accounts::Logins,
) {
    let newer = |_: &String, owner: &mut LoginOwner| {
        owner.matches(store, cli)
            && owner
                .success_generation
                .is_some_and(|(publication, sign_in)| {
                    logins
                        .get(&owner.label)
                        .is_some_and(|binding| binding.last_success_generation > publication)
                        || logins
                            .sign_in_receipts
                            .get(&owner.label)
                            .is_some_and(|receipt| *receipt > sign_in)
                })
    };
    claims.activity.retain(newer);
    claims.ladder.retain(newer);
    let labels: BTreeSet<_> = claims
        .activity
        .values()
        .chain(claims.ladder.values())
        .map(|owner| owner.label.clone())
        .collect();
    for label in labels {
        note_published_login(guard, &claims, store, cli, &label);
    }
}

/// Observe completed publications while automatic sign-in is disabled. This
/// reads only committed bindings; it starts neither a sign-in nor journal recovery.
fn observe_committed_logins(
    store: &accounts::Store,
    cli: Cli,
    guard: &std::sync::Arc<Watched<AutoRenewGuard>>,
    now: impl Into<trace::Now>,
) {
    let now: trace::Now = now.into();
    let mut claims = {
        let state = guard
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.in_flight {
            return;
        }
        LoginClaims::from_state(&state)
    };
    claims.activity.retain(|_, owner| owner.matches(store, cli));
    claims.ladder.retain(|_, owner| owner.matches(store, cli));
    if (!claims.activity.is_empty() || !claims.ladder.is_empty())
        && let Ok((logins, _)) = store.load_logins(now)
    {
        note_observed_publications(guard, claims, store, cli, &logins);
    }
}

/// Holds `label`'s login in `store` active, as a renewal of it under way does, until the value
/// returned is dropped: how a test lays out a renewal a reader of [`login_activity`] waits for.
#[cfg(test)]
#[must_use]
pub(super) fn hold_login_activity_for_test(
    store: &accounts::Store,
    cli: Cli,
    label: &str,
) -> impl Sized + use<> {
    LoginActivityGuard::begin(&AUTO_RENEW_GUARD, store, cli, label, None)
}

/// Holds the window's observation lock, as any reader or writer of its activity does for an
/// instant, until the value returned is dropped: how a test lays out a busy lock.
#[cfg(test)]
#[must_use]
pub(super) fn hold_login_observation_for_test() -> impl Sized {
    AUTO_RENEW_GUARD
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl LoginActivityGuard {
    #[cfg(test)]
    fn begin(
        guard: &std::sync::Arc<Watched<AutoRenewGuard>>,
        store: &accounts::Store,
        cli: Cli,
        label: &str,
        route: Option<&str>,
    ) -> Self {
        Self::begin_with_identity(guard, store, cli, label, None, route)
    }

    fn begin_row(
        guard: &std::sync::Arc<Watched<AutoRenewGuard>>,
        store: &accounts::Store,
        cli: Cli,
        row: &accounts::Row,
        route: Option<&str>,
    ) -> Self {
        Self::begin_with_identity(
            guard,
            store,
            cli,
            &row.label,
            Some(LoginIdentity::of(row)),
            route,
        )
    }

    fn begin_with_identity(
        guard: &std::sync::Arc<Watched<AutoRenewGuard>>,
        store: &accounts::Store,
        cli: Cli,
        label: &str,
        identity: Option<LoginIdentity>,
        route: Option<&str>,
    ) -> Self {
        let token = std::sync::Arc::new(());
        let mut state = guard
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let owner_token = state
            .activity_owners
            .get_mut(label)
            .filter(|owner| owner.matches(store, cli) && owner.identity == identity)
            .map(|owner| {
                owner.diagnostic = None;
                owner.token.clone()
            });
        let mut owner = LoginOwner::new(store, cli, label, crate::supervise::limit::unix_now());
        owner.identity = identity;
        state.active_login = Some(ActiveLogin {
            owner,
            route: route.map(str::to_owned),
            token: token.clone(),
        });
        drop(state);
        login_activity_moved();
        Self {
            guard: guard.clone(),
            token,
            owner_token,
        }
    }

    fn complete(
        &self,
        row: &accounts::Row,
        completed_at: i64,
        (renewal, observation): (super::cli::Renewal, super::cli::RenewalObservation),
    ) {
        let mut state = self
            .guard
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(active) = state.active_login.as_ref().filter(|active| {
            std::sync::Arc::ptr_eq(&active.token, &self.token) && active.owner.label == row.label
        }) else {
            return;
        };
        let Some(owner_token) = &self.owner_token else {
            return;
        };
        let key = active.owner.label.clone();
        let Some(owner) = state
            .activity_owners
            .get_mut(&key)
            .filter(|owner| std::sync::Arc::ptr_eq(&owner.token, owner_token))
        else {
            return;
        };
        owner.diagnostic = Some(LoginDiagnostic {
            completed_at,
            row: row.clone(),
            renewal,
            observation,
        });
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
#[path = "accounts_sync_activity_batch_tests.rs"]
mod login_activity_batch_tests;

#[cfg(test)]
#[path = "accounts_manual_queue_tests.rs"]
mod manual_queue_tests;

#[cfg(test)]
#[path = "accounts_sync_login_activity_tests.rs"]
mod login_activity_tests;

/// How long a row whose background renewal stopped for a person where no bot protection stopped
/// it ([`AutoRenewGuard::parked`]: a page that waits on a person, its time run out; a Claude
/// window a person closed, another account's consent, no desktop) is left alone by the
/// background before it is tried again: a renewal that stops so starts a live device sign-in
/// every time it runs. A bot protection's stop (Cloudflare's check, a block page, a human check)
/// is held on no clock, whichever CLI's row it stopped ([`left_to_a_person`]).
const PERSON_BACKOFF: Duration = Duration::from_secs(6 * 60 * 60);
/// [`PERSON_BACKOFF`] in seconds.
pub(crate) const PERSON_BACKOFF_SECS: i64 = 6 * 60 * 60;

/// The least time between two background attempts admitted for one key whose inputs did not
/// change ([`AutoRenewGuard::last_attempt`], [`AutoRenewPermit::claim_with_context`]).
const PERMIT_COOLDOWN: Duration = Duration::from_secs(15 * 60);

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

#[cfg(test)]
fn browser_window_signed_in_token(
    store: &accounts::Store,
    label: &str,
    since_at: i64,
) -> Option<(i64, usize)> {
    browser_window_token_where(store, since_at, |event| event.account == label)
}

fn browser_window_token_where(
    store: &accounts::Store,
    since_at: i64,
    applies: impl Fn(&accounts::Event) -> bool,
) -> Option<(i64, usize)> {
    let events = store.tail_validated(accounts::LEDGER_KEEP_ROWS).ok()?;
    let mut last_at: Option<i64> = None;
    let mut same_at_count = 0usize;
    let mut satisfied: Option<(i64, usize)> = None;
    for event in events {
        if !applies(&event) {
            continue;
        }
        if event.kind == accounts::Kind::WrongAccount {
            satisfied = None;
            continue;
        }
        if event.kind != accounts::Kind::SignIn {
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
            satisfied = (event.t >= since_at).then_some((event.t, same_at_count));
        } else if why.is_empty() || why.contains(" window ") || is_person_stop_why(why) {
            satisfied = None;
        }
    }
    satisfied
}

fn workflow_browser_window_token(
    store: &accounts::Store,
    raw: &accounts::Row,
    logins: &accounts::Logins,
    since_at: i64,
) -> Option<(i64, usize)> {
    accounts::current_workflow(raw, logins)?;
    browser_window_token_where(store, since_at, |event| {
        (event.account == raw.label
            && (event.kind == accounts::Kind::WrongAccount
                || (event.kind == accounts::Kind::SignIn && event.why.is_empty())))
            || accounts::workflow_event_matches(raw, logins, event)
    })
}

/// Whether a Kimi Code CLI `row` with no login here signs in with nobody at this Mac: its ledger's
/// last word on its browser is a window a person signed in. The profile directory proves nothing,
/// since the headless approver makes it before its first try, and a later attempt that stopped
/// for a person takes the word back. With none on record, measured 2026-10-24, the page stopped
/// for a person both times it ran.
fn kimi_row_signs_in_unattended(store: &accounts::Store, row: &accounts::Row) -> bool {
    browser_window_token_where(store, i64::MIN, |event| event.account == row.label).is_some()
}

/// Whether a `cli` row of `raw_roster` whose route is its page signs in with nobody at this Mac:
/// only where its ledger can say, and not where it is left to a person ([`person_hold`]: a bot
/// protection stopped a sign-in of its account, or of another row of its pool here, which the
/// next try meets too: measured 2026-11-04, every automatic sign-in on record of the Codex rows
/// of two Macs, 8 of 8, ended at Cloudflare's check). Kimi Code CLI's asks more
/// ([`kimi_row_signs_in_unattended`]).
pub(crate) fn page_row_signs_in_unattended(
    cli: Cli,
    store: &accounts::Store,
    raw_roster: &Roster,
    raw: &accounts::Row,
    logins: &accounts::Logins,
) -> bool {
    store.tail(1).is_ok()
        && person_hold(store, raw_roster, raw, logins).is_none()
        && (cli != Cli::Kimi || kimi_row_signs_in_unattended(store, raw))
}

/// Whether the row's trusted browser profile may be tried for a fresh Claude login.
/// A wrong-account renewal rejects it until a later successful browser window is on record.
/// This permits another provider identity check and changes no authentication or login state.
#[must_use]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountBrowserRehabilitation",
        action = "Read",
        project = "aterm_agent::harness::accounts_sync::browser_rehabilitation_tests::rehabilitation_transition"
    )
)]
pub(crate) fn claude_browser_profile_ready(
    store: &accounts::Store,
    logins: &accounts::Logins,
    raw: &accounts::Row,
) -> bool {
    let (label, email) = (raw.label.as_str(), raw.email.as_deref());
    if !super::headless_oauth::has_browser_profile(&store.dir, label, email) {
        return false;
    }
    let Some(wrong_at) = logins.get(label).and_then(|binding| binding.wrong_at) else {
        return true;
    };
    workflow_browser_window_token(store, raw, logins, wrong_at).is_some_and(|(at, _)| at > wrong_at)
}

/// Fixture choices for browser-profile trust and ordered account history.
#[cfg(any(test, debug_assertions))]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountBrowserRehabilitation",
        action = "Case",
        reason = "The native fixture chooses profile trust and ordered browser/account ledger histories."
    )
)]
#[doc(hidden)]
pub fn account_browser_rehabilitation_environment() {}

/// When the step a person owes `label` (its sign-in task, [`accounts::SignInAction`]) was noted,
/// where one stands: a browser profile to sign in that no signed-in `accounts browser` window has
/// answered since, or a sign-in to approve.
pub(crate) fn unaddressed_sign_in_task_at(
    store: &accounts::Store,
    logins: &accounts::Logins,
    raw: &accounts::Row,
) -> Option<i64> {
    let task = accounts::current_sign_in_task(raw, logins)?;
    if task.action == accounts::SignInAction::Browser
        && workflow_browser_window_token(store, raw, logins, task.at).is_some()
    {
        return None;
    }
    Some(task.at)
}

fn reconcile_browser_window_unpark(
    state: &std::sync::Arc<Watched<AutoRenewGuard>>,
    key: &str,
    token: Option<(i64, usize)>,
    raw: &accounts::Row,
    logins: &accounts::Logins,
    (store, cli, roster_path): (&accounts::Store, Cli, &Path),
) {
    reconcile_browser_window_unpark_with_read(
        state,
        key,
        token,
        raw,
        logins,
        (store, cli, roster_path),
        || {},
    );
}

fn reconcile_browser_window_unpark_with_read(
    state: &std::sync::Arc<Watched<AutoRenewGuard>>,
    key: &str,
    token: Option<(i64, usize)>,
    raw: &accounts::Row,
    logins: &accounts::Logins,
    (store, cli, roster_path): (&accounts::Store, Cli, &Path),
    after_read: impl FnOnce(),
) {
    let Some(token) = token else {
        return;
    };
    let Some(workflow) = accounts::current_workflow(raw, logins) else {
        return;
    };
    let claims = LoginClaims::capture(state);
    let Ok(records) =
        store.read_account_records(Some(roster_path), crate::supervise::limit::unix_now())
    else {
        return;
    };
    after_read();
    if records.raw_roster.row(&raw.label) != Some(raw)
        || !accounts::workflow_token_current(raw, &records.logins, workflow)
    {
        return;
    }
    let logins = &records.logins;
    let mut guard = state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let owns = guard.activity_owners.get(key).is_some_and(|owner| {
        owner.matches(store, cli)
            && claims
                .activity
                .get(key)
                .is_some_and(|before| std::sync::Arc::ptr_eq(&before.token, &owner.token))
    });
    let same_claim = match (guard.activity_owners.get(key), claims.activity.get(key)) {
        (Some(owner), Some(before)) => std::sync::Arc::ptr_eq(&before.token, &owner.token),
        (None, None) => true,
        _ => false,
    };
    let foreign = guard
        .activity_owners
        .get(key)
        .into_iter()
        .chain(guard.ladder_owners.get(key))
        .any(|owner| !owner.matches(store, cli));
    let held = guard.last_attempt.contains_key(key)
        || guard.parked.contains_key(key)
        || guard.unrecorded_checks.contains_key(key);
    if guard.in_flight || !same_claim || foreign || (held && !owns) {
        return;
    }
    let used = guard
        .browser_unparked
        .get(key)
        .is_some_and(|(workflow, at, count)| {
            (*at, *count) == token
                && workflow.is_some_and(|held| accounts::workflow_token_current(raw, logins, held))
        });
    if !used {
        guard.browser_unparked.insert(
            key.to_owned(),
            (accounts::current_workflow(raw, logins), token.0, token.1),
        );
        // A signed-in window answers a check this process met and did not record, as it ends
        // one on record ([`left_to_a_person`]).
        guard.parked.remove(key);
        guard.unrecorded_checks.remove(key);
        guard.last_attempt.remove(key);
    }
}

/// Notes how a background renewal of `label` ended: one that stopped for a person where no bot
/// protection stopped it ([`super::cli::Renewal::NeedsPerson`]) parks the row until
/// [`PERSON_BACKOFF`] has passed since `now`; one a bot protection stopped
/// ([`super::cli::Renewal::BotCheck`]) parks nothing on a clock, since the account's check holds
/// it, on record or in this process ([`hold_for_a_person`]), until a person goes on from it; a
/// contended lock leaves any hold as it was; any other end lifts the park.
#[cfg(test)]
fn note_background_end(
    state: &std::sync::Arc<Watched<AutoRenewGuard>>,
    label: &str,
    renewal: super::cli::Renewal,
    now: Instant,
) {
    let mut guard = state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    apply_background_end(&mut guard, label, renewal, now);
}

fn apply_background_end(
    guard: &mut AutoRenewGuard,
    label: &str,
    renewal: super::cli::Renewal,
    now: Instant,
) {
    use super::cli::Renewal;
    match renewal {
        Renewal::NeedsPerson => {
            guard.parked.insert(label.to_owned(), now);
        }
        Renewal::SigninHeld | Renewal::Deferred => {}
        Renewal::BotCheck
        | Renewal::Renewed
        | Renewal::OtherRenewed
        | Renewal::AuthFailed
        | Renewal::WrongAccount
        | Renewal::Failed => {
            guard.parked.remove(label);
        }
    }
}

fn note_vendor_renewal_end(
    guard: &std::sync::Arc<Watched<AutoRenewGuard>>,
    claims: &LoginClaims,
    store: &accounts::Store,
    cli: Cli,
    (key, label): (&str, &str),
    (roster_path, raw, observed): (&Path, &accounts::Row, &accounts::Logins),
    renewal: super::cli::Renewal,
) {
    if renewal == super::cli::Renewal::Renewed {
        note_published_login(guard, claims, store, cli, label);
    } else if let Some(token) = accounts::current_workflow(raw, observed)
        && let Ok(records) =
            store.read_account_records(Some(roster_path), crate::supervise::limit::unix_now())
        && let Some(current) = records.raw_roster.row(label)
        && accounts::workflow_token_current(current, &records.logins, token)
    {
        let mut state = guard
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.activity_owners.get(key).is_some_and(|owner| {
            owner.matches(store, cli)
                && claims
                    .activity
                    .get(key)
                    .is_some_and(|before| std::sync::Arc::ptr_eq(&before.token, &owner.token))
        }) {
            let at = (state.reads.clock)();
            apply_background_end(&mut state, key, renewal, at);
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

static AUTO_RENEW_GUARD: std::sync::LazyLock<std::sync::Arc<Watched<AutoRenewGuard>>> =
    std::sync::LazyLock::new(|| {
        AutoRenewGuard {
            reads: MachineReads::live(),
            ..AutoRenewGuard::default()
        }
        .shared()
    });

impl AutoRenewGuard {
    /// This guard, watched and shared, as the window's is ([`AUTO_RENEW_GUARD`]).
    fn shared(self) -> std::sync::Arc<Watched<Self>> {
        std::sync::Arc::new(Watched::new("accounts_sync/renew_guard", self))
    }
}

struct AutoRenewPermit {
    guard: std::sync::Arc<Watched<AutoRenewGuard>>,
    wake_pending: Option<PathBuf>,
    key: String,
    attempt_at: Instant,
    before_attempt: Option<Instant>,
    scope: Option<super::account_work::Scope>,
    started: bool,
    completed: std::sync::Arc<std::sync::atomic::AtomicU8>,
}

/// Scheduling evidence only: the admitted work recorded a definite result.
#[derive(Clone)]
struct RenewalCompletion(std::sync::Arc<std::sync::atomic::AtomicU8>);

impl RenewalCompletion {
    fn complete(&self) {
        self.0.store(2, std::sync::atomic::Ordering::Release);
    }

    fn defer(&self) {
        // A definite completed result is never downgraded by a later service refusal.
        let _ = self.0.compare_exchange(
            0,
            1,
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Acquire,
        );
    }
}

enum RenewalPreparation<'a> {
    Shared(&'a super::cli::shared_renewal::Grant),
    Native(&'a super::native_keys::PreparationId),
}

impl AutoRenewPermit {
    fn start(&mut self) {
        self.started = true;
    }
    fn follow_scope(&mut self, scope: Option<&super::account_work::Scope>) -> RenewalCompletion {
        self.scope = scope.cloned();
        RenewalCompletion(self.completed.clone())
    }
    #[cfg(test)]
    fn claim(
        state: &std::sync::Arc<Watched<AutoRenewGuard>>,
        label: &str,
        now: Instant,
        ignore_cooldown: bool,
    ) -> Option<Self> {
        Self::claim_with_mailbox(state, label, now, ignore_cooldown, None)
    }

    fn claim_with_mailbox(
        state: &std::sync::Arc<Watched<AutoRenewGuard>>,
        label: &str,
        now: Instant,
        ignore_cooldown: bool,
        mailbox_mtime: Option<std::time::SystemTime>,
    ) -> Option<Self> {
        Self::claim_with_context(state, label, now, ignore_cooldown, mailbox_mtime, None)
    }

    fn claim_preparation(
        state: &std::sync::Arc<Watched<AutoRenewGuard>>,
        label: &str,
        now: Instant,
        grant: &super::cli::shared_renewal::Grant,
    ) -> Option<Self> {
        Self::claim_with_context(
            state,
            label,
            now,
            false,
            None,
            Some(RenewalPreparation::Shared(grant)),
        )
    }

    fn claim_native_preparation(
        state: &std::sync::Arc<Watched<AutoRenewGuard>>,
        now: Instant,
        preparation: &super::native_keys::PreparationId,
    ) -> Option<Self> {
        Self::claim_with_context(
            state,
            &preparation.key,
            now,
            false,
            None,
            Some(RenewalPreparation::Native(preparation)),
        )
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "MailboxRenewalAdmission",
            action = "Busy",
            project = "aterm_agent::harness::accounts_sync::hardening_tests::mailbox_admission_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "MailboxRenewalAdmission",
            action = "Person",
            project = "aterm_agent::harness::accounts_sync::hardening_tests::mailbox_admission_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "MailboxRenewalAdmission",
            action = "Bot",
            project = "aterm_agent::harness::accounts_sync::hardening_tests::mailbox_admission_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "MailboxRenewalAdmission",
            action = "Release",
            project = "aterm_agent::harness::accounts_sync::hardening_tests::mailbox_admission_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "MailboxRenewalAdmission",
            action = "Left",
            project = "aterm_agent::harness::accounts_sync::hardening_tests::mailbox_admission_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "MailboxRenewalAdmission",
            action = "Right",
            project = "aterm_agent::harness::accounts_sync::hardening_tests::mailbox_admission_transition"
        )
    )]
    fn claim_with_context(
        state: &std::sync::Arc<Watched<AutoRenewGuard>>,
        label: &str,
        now: Instant,
        ignore_cooldown: bool,
        mailbox_mtime: Option<std::time::SystemTime>,
        preparation: Option<RenewalPreparation<'_>>,
    ) -> Option<Self> {
        let mut guard = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mailbox_changed =
            mailbox_mtime.is_some_and(|mtime| guard.mailbox_mtimes.get(label) != Some(&mtime));
        let preparation_changed = match &preparation {
            Some(RenewalPreparation::Shared(grant)) => {
                guard.shared_preparations.get(label) != Some(&(grant.source, grant.decision))
            }
            Some(RenewalPreparation::Native(id)) => {
                guard.native_preparations.get(label) != Some(&id.revision)
            }
            None => false,
        };
        // Each hold is judged through `Since`, so a traced claim one holds records when it ends.
        let stands = |since: Option<&Instant>, span: Duration| {
            since.is_some_and(|since| !trace::Since::new(*since).elapsed_at_least(span, now))
        };
        if guard.in_flight
            || (!ignore_cooldown
                && ((!preparation_changed
                    && !mailbox_changed
                    && stands(guard.last_attempt.get(label), PERMIT_COOLDOWN))
                    || stands(guard.parked.get(label), PERSON_BACKOFF)
                    // A bot protection this process met and has not recorded holds on no clock
                    // (docs/DESIGN-claude-sign-in-sitting-2026-11-12.md §3). A person's own
                    // choice is judged by its door, which knows whether they are at this desk
                    // and holds one nobody is at the desk for by the same pending check.
                    || guard.unrecorded_checks.contains_key(label)))
        {
            return None;
        }
        // A changed mailbox wakes this key once, when it is actually admitted.
        // Another key's observation never erases this attempt's history.
        if let Some(mtime) = mailbox_mtime {
            guard.mailbox_mtimes.insert(label.to_owned(), mtime);
        }
        match preparation {
            Some(RenewalPreparation::Shared(grant)) => {
                record_shared_preparation(&mut guard, label, grant)
            }
            Some(RenewalPreparation::Native(id)) => {
                guard
                    .native_preparations
                    .insert(label.to_owned(), id.revision);
            }
            None => {}
        }
        let before_attempt = if preparation_changed || mailbox_changed {
            None
        } else {
            guard.last_attempt.get(label).copied()
        };
        guard.in_flight = true;
        guard.last_attempt.insert(label.to_owned(), now);
        // The retry belongs to this new permit immediately. Until attribute()
        // supplies its owner, an older publication must see it as unattributed.
        guard.activity_owners.remove(label);
        Some(Self {
            guard: std::sync::Arc::clone(state),
            wake_pending: None,
            key: label.to_owned(),
            attempt_at: now,
            before_attempt,
            scope: None,
            started: false,
            completed: std::sync::Arc::default(),
        })
    }

    #[cfg(test)]
    fn attribute(
        &self,
        store: &accounts::Store,
        cli: Cli,
        label: &str,
        now: i64,
        logins: &accounts::Logins,
    ) {
        self.attribute_owner(LoginOwner::new(store, cli, label, now).observing(logins));
    }

    fn attribute_row(
        &self,
        store: &accounts::Store,
        cli: Cli,
        row: &accounts::Row,
        raw: &accounts::Row,
        now: i64,
        logins: &accounts::Logins,
    ) {
        self.attribute_owner(
            LoginOwner::new(store, cli, &row.label, now)
                .observing(logins)
                .for_row(row)
                .for_workflow(raw, logins),
        );
    }

    fn attribute_owner(&self, owner: LoginOwner) {
        self.guard
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .activity_owners
            .insert(self.key.clone(), owner);
    }
}

impl Drop for AutoRenewPermit {
    fn drop(&mut self) {
        {
            let mut guard = self
                .guard
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let result = self.completed.load(std::sync::atomic::Ordering::Acquire);
            let interrupted = result == 1
                || self
                    .scope
                    .as_ref()
                    .is_some_and(|scope| !self.started || (scope.cancelled() && result != 2));
            // This permit still owns the sole in-flight slot. Restore only its unchanged
            // scheduling claim before allowing another admission; credential journals stand.
            if interrupted && guard.last_attempt.get(&self.key) == Some(&self.attempt_at) {
                match self.before_attempt {
                    Some(before) => {
                        guard.last_attempt.insert(self.key.clone(), before);
                    }
                    None => {
                        guard.last_attempt.remove(&self.key);
                    }
                }
            }
            guard.in_flight = false;
        }
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

/// Queued work drops its captured resources before the admission permit, even
/// when the worker never starts.
struct AutoRenewWork<W> {
    work: W,
    permit: AutoRenewPermit,
}

impl<W: FnOnce()> AutoRenewWork<W> {
    fn run(self) {
        let Self { work, mut permit } = self;
        permit.start();
        work();
    }
}

fn start_auto_renew(
    permit: AutoRenewPermit,
    work: impl FnOnce() + Send + 'static,
    spawn: impl FnOnce(Box<dyn FnOnce() + Send>) -> io::Result<()>,
) -> io::Result<()> {
    let owned = AutoRenewWork { work, permit };
    // Consume the whole owner so precise closure capture cannot split its fields.
    spawn(Box::new(move || owned.run()))
}

pub(super) fn resolve_claude_program(home: &Path) -> PathBuf {
    if let Some(twin) = super::upgrade_drive::managed_agent_twin("claude") {
        return twin;
    }
    // Claude Code's own installer lays it in `~/.local/bin`: `claude`, and on Windows
    // `claude.exe`.
    let local_bin = home
        .join(".local")
        .join("bin")
        .join(format!("claude{}", std::env::consts::EXE_SUFFIX));
    if trace::fs::is_file(&local_bin) {
        return local_bin;
    }
    PathBuf::from("claude")
}

fn resolve_opener_link(
    home: &Path,
    store: &accounts::Store,
) -> Result<PathBuf, super::cli::OpenerError> {
    let custom_opener = home.join(".local/bin").join(super::cli::opener_file_name());
    // An explicitly installed helper that is broken or inaccessible is a setup error,
    // not permission to replace it with this process's executable.
    if !matches!(trace::fs::symlink_metadata(&custom_opener), Err(error) if error.kind() == io::ErrorKind::NotFound)
    {
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
    super::cli::opener_link(store, &super::cli::LiveAccounts::default())
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
        if trace::fs::is_dir(&path) || trace::fs::is_file(&path) {
            return Some(path);
        }
    }
    let store_mailbox = store.dir.join(super::open_url::DEFAULT_MAILBOX_DIR);
    if trace::fs::is_dir(&store_mailbox) {
        return Some(store_mailbox);
    }
    if let Some(home) = home {
        let config_mailbox = home
            .join(".config/aterm")
            .join(super::open_url::DEFAULT_MAILBOX_DIR);
        if trace::fs::is_dir(&config_mailbox) {
            return Some(config_mailbox);
        }
    }
    None
}

/// Whether a mailbox answers the sign-in of the row `label` (`target_email`): a mailbox folder
/// ([`resolve_mailbox_dir_with`]), or a mailbox configuration, in its files or the environment's
/// mailbox variables ([`super::headless_oauth::discover_mailbox_config_with_env`]), that receives
/// the email. The environment is read through `var` alone.
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

/// Whether aterm's sign-in window reads the login code of the row `label` (`target_email`) by
/// itself: the answer the window's own desk gives
/// ([`super::headless_oauth::MailboxConfig::window_reads`]), over the same configuration, read
/// on this Mac alone. A mailbox only a headless browser reads (webmail) and the fleet's mail
/// relay are none, though [`has_configured_mailbox_for_row_with`] counts both: a sign-in
/// nobody is watching opens the window only where this holds. The environment is read through
/// `var` alone.
pub(super) fn window_reads_mailbox_for_row_with(
    var: &dyn Fn(&str) -> Option<std::ffi::OsString>,
    home: Option<&Path>,
    store: &accounts::Store,
    label: Option<&str>,
    target_email: Option<&str>,
) -> bool {
    super::headless_oauth::discover_mailbox_config_with_env(home, Some(&store.dir), var)
        .is_some_and(|mut cfg| {
            cfg.read_on_this_mac_alone();
            cfg.alias_label_to_email(label, target_email);
            cfg.window_reads(target_email)
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

/// The newest modification time among what a sign-in reads (the configuration and roster files,
/// the mailbox configurations, the messages in the mail folders, the environment `var` reads
/// naming one, and the time this process saw the vault's mailbox entries change,
/// [`crate::vault_entry::changed_at`]): a change
/// here ends an automatic sign-in's cooldown. Neither the vault folder's own files nor
/// the vault's `state.toml`, which its subscriber rewrites about once a minute only to say when
/// it last heard the vault, is among them, and nor is a mail folder's own time, which the mark a
/// sign-in leaves in the folder for a message it used (`.clicked`) moves: a sign-in's own reading
/// never ends its cooldown. A future timestamp is not a new input: it neither ends a retry hold
/// nor hides an earlier message that has arrived by the time of this read.
fn latest_mailbox_mtime_with(
    var: &dyn Fn(&str) -> Option<std::ffi::OsString>,
    home: Option<&Path>,
    store: &accounts::Store,
) -> Option<std::time::SystemTime> {
    let at = aterm_time::since_epoch().map_or(std::time::UNIX_EPOCH, |d| std::time::UNIX_EPOCH + d);
    latest_mailbox_mtime_at(var, home, store, at)
}

/// Read mailbox inputs as of `at`, excluding future timestamps before selecting the newest.
fn latest_mailbox_mtime_at(
    var: &dyn Fn(&str) -> Option<std::ffi::OsString>,
    home: Option<&Path>,
    store: &accounts::Store,
    at: std::time::SystemTime,
) -> Option<std::time::SystemTime> {
    let mut paths = vec![store.dir.join(super::headless_oauth::MAILBOX_TOML_FILE)];
    let mut vault_changed = None;
    if let Some(h) = home {
        let entries = super::headless_oauth::vault_mailbox_entries(h);
        let names = entries.iter().map(String::as_str).collect::<Vec<_>>();
        // A change this read is the first to see is stamped as it reads, after `at`: it is a
        // change as of `at`, never a future timestamp.
        vault_changed = crate::vault_entry::changed_at(
            "mailbox",
            h,
            &names,
            super::headless_oauth::MAILBOX_TOML_MAX_BYTES,
        )
        .map(|seen| seen.min(at));
        paths.push(h.join(".config/aterm/aterm.toml"));
        paths.push(h.join(".config/aterm/accounts.toml"));
        paths.push(h.join(".config/aterm").join(FLEET_ROSTER_FILE));
        paths.push(
            h.join(".config/aterm")
                .join(super::headless_oauth::MAILBOX_TOML_FILE),
        );
    }
    let push_maildir_paths = |paths: &mut Vec<PathBuf>, root: &Path| {
        // A mailbox that is one file (an mbox) counts by its own time.
        if trace::fs::is_file(root) {
            paths.push(root.to_path_buf());
            return;
        }
        for dir in [root.to_path_buf(), root.join("new"), root.join("cur")] {
            if let Ok(entries) = trace::fs::read_dir(&dir) {
                for entry in entries {
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
        .filter_map(|p| trace::fs::metadata(p).ok()?.modified().ok())
        .chain(vault_changed)
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
    auto_renew_candidates_tiered(
        Cli::Claude,
        None,
        file_roster,
        state,
        logins,
        &accounts::OtherMacs::NONE,
        now,
    )
    .into_iter()
    .map(|(_, row)| row)
    .collect()
}

/// A scheduling observation, not permission to authenticate. Workers still admit
/// its raw policy and login generation again before starting an attempt, and each start takes
/// a [`SignIn`] minted from the records it reads then ([`upkeep_sign_in`]).
struct RenewalCandidates {
    records: accounts::AccountRecords,
    /// The rows with no login here whose account `records`' source gives to other Macs alone or
    /// whose source this Mac cannot compare ([`rows_of_other_macs`]), read with them: neither the
    /// ranking nor a door admits one.
    others: accounts::OtherMacs,
    tiered: Vec<(u8, accounts::Row)>,
}

/// Keep candidate ranking and the worker's captured policy in the same completed
/// account publication. Credential presence and sign-in task history remain
/// independent observations, as in the existing ranking policy. A Claude Code row whose own
/// login the credential-free files show live after its recorded sign-out is ranked nowhere
/// (`login_adoption::retain_not_live`, asked of the pass's witnesses `live`); evidence that is
/// not newer hides no row, and neither does evidence of a row an adoption step could not lift.
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
    (home, state_dir): (&Path, &Path),
    roster_path: &Path,
    now: impl Into<trace::Now>,
    live: &super::login_adoption::Pass,
) -> Result<RenewalCandidates, accounts::RosterReadError> {
    let now: trace::Now = now.into();
    let records = store.read_account_records(Some(roster_path), now)?;
    let others = rows_of_other_macs_at(
        cli,
        (home, state_dir),
        roster_path,
        (&records.raw_roster, &records.logins),
        now,
    );
    let mut tiered = auto_renew_candidates_tiered(
        cli,
        Some(store),
        &records.raw_roster,
        &records.state,
        &records.logins,
        &others,
        now,
    );
    // Every route that could start a sign-in asks first (plan package FL-174): a row whose own
    // login served or was signed in after its recorded sign-out is no candidate, whoever has
    // or has not written that down yet. The upkeep's step lifts the record
    // (`login_adoption::adopt_live_logins`); this read writes nothing.
    retain_not_live(
        cli,
        &mut tiered,
        (home, state_dir),
        store,
        &records,
        now,
        live,
    );
    Ok(RenewalCandidates {
        records,
        others,
        tiered,
    })
}

/// `login_adoption::retain_not_live` over the ranking `tiered` of `records`, for Claude Code's
/// rows alone: the one filter [`read_renewal_candidates`] applies and every ranking made after
/// it from the same records applies again ([`auto_renew_next_account_in`]'s ranking with a
/// shared authority installed), so no re-rank puts back a row the files show live. A ranking
/// with no row signed out on record asks no witness and compares nothing.
fn retain_not_live(
    cli: Cli,
    tiered: &mut Vec<(u8, accounts::Row)>,
    paths: (&Path, &Path),
    store: &accounts::Store,
    records: &accounts::AccountRecords,
    now: trace::Now,
    live: &super::login_adoption::Pass,
) {
    if cli == Cli::Claude {
        super::login_adoption::retain_not_live(
            tiered,
            paths,
            store,
            (&records.roster, &records.state),
            now,
            live,
        );
    }
}

/// [`rows_of_other_macs`] for a reader that holds the paths an [`Env`] is made from, at `now`:
/// the [`Env`] its source is read under ([`traced_env`]) is made only where a row has no login
/// here, and the rows on their vault token are judged through `now`
/// ([`super::accounts_tokens::rows_on_token`]). A read whose every row has its login here and
/// no row on a token compares nothing.
pub(crate) fn rows_of_other_macs_at(
    cli: Cli,
    (home, state_dir): (&Path, &Path),
    roster_path: &Path,
    (file_roster, logins): (&Roster, &accounts::Logins),
    now: impl Into<trace::Now>,
) -> accounts::OtherMacs {
    let now: trace::Now = now.into();
    accounts::OtherMacs {
        on_token: rows_on_token(
            cli,
            (Some(home), state_dir),
            &accounts::bound(file_roster, logins),
            now,
        ),
        ..rows_of_other_macs_where(cli, Some(home), (file_roster, logins), || {
            std::borrow::Cow::Owned(traced_env(
                home,
                state_dir,
                &config_beside(roster_path),
                now,
            ))
        })
    }
}

/// ANOTHER MAC'S ACCOUNT IS NOT THIS MAC'S TO SIGN IN UNASKED
/// (`DESIGN-alab-one-mesh-2026-11-05` §4.15 rule 3): the rows of `file_roster` with no login on
/// this machine whose account `cli`'s source gives to other machines alone, each with the first
/// it names ([`accounts::OtherMacs::named`]), or, where this Mac cannot compare that source with
/// its own names, every row with no login here ([`accounts::OtherMacs::unread`]). The upkeep
/// signs in neither by itself ([`accounts::renewal_due`]), and a report says why. `aterm ai
/// login` reads the named ones so as not to name them, since that command also runs with nobody
/// here and a row it names counts as a person's ask. A row like this can stand for as long as its
/// source does: another CLI's sync never drops a row, and Claude Code's rebuilds a roster only
/// when what it is made from changes (`ATERM-KI-1105-9`).
///
/// It is read from the source each time rather than kept as [`accounts::Row::held_by`], which
/// would also refuse the person who names the row (`accounts renew <account>`) to borrow it, and
/// could outlive the sheet's next edit. A login already here stays whatever the source says, and
/// a row the source does not know is this Mac's. Nothing limits the rows where no source names
/// machines: no source at all, the lab, or a roster that is its own list. A source only the
/// network could read (reading who an account belongs to fetches nothing), one that does not
/// parse, a lab that cannot be read, a Mac with no name and an unreadable pass roster each leave
/// the rows unread: their account may be another Mac's.
///
/// With them, the rows that run on their vault token here ([`accounts::OtherMacs::on_token`]),
/// whose stored login no door renews either (`DESIGN-claude-setup-token-2026-11-05` §5.8).
pub(super) fn rows_of_other_macs(
    cli: Cli,
    env: &Env,
    file_roster: &Roster,
    logins: &accounts::Logins,
) -> accounts::OtherMacs {
    accounts::OtherMacs {
        on_token: rows_on_token(
            cli,
            (env.home.as_deref(), &env.state),
            &accounts::bound(file_roster, logins),
            env.now,
        ),
        ..rows_of_other_macs_where(cli, env.home.as_deref(), (file_roster, logins), || {
            std::borrow::Cow::Borrowed(env)
        })
    }
}

/// The rows of `roster` that run on their vault token on this Mac at `now`
/// ([`super::accounts_tokens::rows_on_token`]): Claude Code's only, read from the vault under
/// `home`, judged against its accounts store in `state_dir` ([`Env::accounts_store_for`]).
fn rows_on_token(
    cli: Cli,
    (home, state_dir): (Option<&Path>, &Path),
    roster: &Roster,
    now: impl Into<trace::Now>,
) -> BTreeMap<String, u64> {
    #[cfg(unix)]
    if cli == Cli::Claude
        && let Some(home) = home
    {
        return super::accounts_tokens::rows_on_token(
            &aterm_secrets::client::Paths::for_home(home),
            &accounts::Store::at_for(state_dir, cli, home),
            roster,
            now,
        );
    }
    let _ = (cli, home, state_dir, roster, now);
    BTreeMap::new()
}

/// THE UPKEEP KEEPS WHICH ROWS RUN ON THEIR VAULT TOKEN HERE (`accounts::TokensHere`): one
/// value-free read of the vault under `home` ([`super::accounts_tokens::rows_on_token_read`])
/// over Claude Code's roster at `roster_path` as `logins.json` binds it, kept in its store under
/// `accounts.lock` ([`accounts::Store::note_tokens_here`]), so rotation can pick a row whose
/// stored login is signed out while its token serves it ([`accounts::LoginHere::OnToken`]). The
/// file is written only where the reading changed, or where no window's holder vouches for it
/// ([`accounts::TokensHereHolder`]), so a pass that reads the vault as before writes nothing while
/// the window keeps it. A vault that could not be read keeps the reading before it, records a
/// retry in the armed trace scope and stands the holder's vouch down, so the reading ages out
/// within the hour. Every pass that renews makes it, whether or not the ladder signs rows in,
/// since rotation reads it either way; `read` is the read ([`UpkeepEffects::tokens_here`]).
/// A reading that could not be kept (`accounts.lock` held past its ladder, the file not
/// written) records a retry in the armed trace scope. Returns whether the store noted the
/// reading confirmed for its holder's next turn: the vault was read and the reading kept, as a
/// pass that skips this read notes again ([`accounts::Store::confirm_tokens_here`]).
fn note_tokens_here(
    home: &Path,
    state_dir: &Path,
    roster_path: &Path,
    now: impl Into<trace::Now>,
    read: TokensRead,
) -> bool {
    let now: trace::Now = now.into();
    let store = accounts::Store::at_for(state_dir, Cli::Claude, home);
    let Ok(raw) = store.read_roster(Some(roster_path), now) else {
        return false;
    };
    let Ok((logins, _)) = store.load_logins(now) else {
        return false;
    };
    let read = read(home, &store, &accounts::bound(&raw, &logins), now);
    match store.note_tokens_here(read.as_ref(), now) {
        Ok(_) => read.is_some(),
        Err(why) => {
            trace::retry(format!(
                "the rows on their vault token here were not kept: {why}"
            ));
            false
        }
    }
}

/// [`rows_of_other_macs`] less the rows on their vault token, of the logins made under `home`
/// (the roster's own record of them where none is known), each row with no login here compared
/// with the source under the [`Env`] that `env` gives, which is asked for only where there is
/// such a row.
fn rows_of_other_macs_where<'e>(
    cli: Cli,
    home: Option<&Path>,
    (file_roster, logins): (&Roster, &accounts::Logins),
    env: impl FnOnce() -> std::borrow::Cow<'e, Env>,
) -> accounts::OtherMacs {
    let roster = accounts::bound(file_roster, logins);
    let enrolled = home.map_or_else(
        || accounts::enrolled_for(cli, &roster),
        |home| accounts::enrolled_in(cli, &roster, home),
    );
    let mut unsigned = roster
        .rows
        .iter()
        .filter(|row| !enrolled.contains(&row.label))
        .peekable();
    if unsigned.peek().is_none() {
        return accounts::OtherMacs::NONE;
    }
    let env = env();
    let env = env.as_ref();
    match assigning_source(cli, env) {
        Assigning::Free => accounts::OtherMacs::NONE,
        Assigning::Unread(why) => accounts::OtherMacs {
            unread: unsigned.map(|row| row.label.clone()).collect(),
            unread_why: Some(why),
            ..accounts::OtherMacs::NONE
        },
        Assigning::Names(fleet, here) => accounts::OtherMacs {
            named: unsigned
                .filter_map(|row| {
                    let holder = only_another_machine(cli, &fleet, &here, row)?;
                    Some((row.label.clone(), holder.to_owned()))
                })
                .collect(),
            ..accounts::OtherMacs::NONE
        },
    }
}

/// What [`assigning_source`] read of who runs each account.
enum Assigning {
    /// Nothing limits who runs an account: no source names machines.
    Free,
    /// A source names machines, and this Mac cannot compare it with its own names, for this
    /// reason.
    Unread(accounts::UnreadWhy),
    /// The source, and every name this Mac goes by in it.
    Names(Box<FleetRoster>, Vec<String>),
}

/// The source `cli`'s sync reads, as far as this machine alone can read it, and every name this
/// machine goes by in it ([`Assigning`]): the name a sync of `cli` reads this Mac as, a pool's
/// identity for it (none where the pool refuses this Mac) or [`machine_for`] for any other
/// source. Under the lab nothing limits who runs an account: its fleet names this Mac for every
/// account it lists. A lab not read yet may name this Mac or not, so it is unread, as a sync then
/// reads no source and goes on with the roster as it stands. With no home the vault's sources
/// cannot be read, and they outrank a roster of its own and the file beside it, so that too is
/// unread.
fn assigning_source(cli: Cli, env: &Env) -> Assigning {
    use accounts::UnreadWhy;
    // Whether the source asked of it is only on the network, which nothing here fetches.
    let fetched = std::sync::atomic::AtomicBool::new(false);
    let local_only = |env: &Env, source: &str| {
        if matches!(fetch_plan(env, source), Ok(FetchPlan::Http(_))) {
            fetched.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        fetch_source_text_with(env, source, &|_| {
            Err("reading who an account belongs to fetches nothing".to_owned())
        })
    };
    let read = if cli == Cli::Claude {
        let Some(roster) = env.accounts_roster() else {
            return Assigning::Unread(UnreadWhy::Source);
        };
        match super::alab_roster::chosen(cli, Some(&roster), None, env.config.as_deref()) {
            Ok(Chosen::Unnamed) => {}
            Ok(Chosen::Lab(_)) => return Assigning::Free,
            Ok(Chosen::Pending(_)) | Err(_) => return Assigning::Unread(UnreadWhy::Source),
        }
        let Ok(selected) = source_selection::select_for(
            &env.accounts_store(),
            env.home.as_deref(),
            env.config.as_deref(),
            Some(&roster),
            SourceScope::Init,
        ) else {
            return Assigning::Unread(UnreadWhy::Source);
        };
        let source = match &selected {
            SelectedSource::Absent | SelectedSource::Beside { .. } if env.home.is_none() => {
                return Assigning::Unread(UnreadWhy::Source);
            }
            SelectedSource::Absent => return Assigning::Free,
            SelectedSource::Key(_) => None,
            _ => selected.source(),
        };
        load_fleet_source_for_machine(env, source.as_deref(), None, &local_only).map(
            |(fleet, _, pool)| {
                let named = match pool {
                    Some(pool) => Some(pool.identity.primary),
                    None => machine_for(env.config.as_deref(), None),
                };
                Some((fleet, named))
            },
        )
    } else {
        let Some(home) = env.home.as_deref() else {
            return Assigning::Unread(UnreadWhy::Source);
        };
        match load_cli_fleet_source(cli, env, home, None, None, &local_only) {
            // The lab's fleet names this Mac for every account it lists.
            Ok(Some((fleet, _, _))) if fleet.roster.alab.is_some() => return Assigning::Free,
            loaded => loaded.map(|loaded| loaded.map(|(fleet, _, named)| (fleet.roster, named))),
        }
    };
    let no_name = || machine_for(env.config.as_deref(), None).is_none();
    let (fleet, named) = match read {
        Ok(Some(read)) => read,
        Ok(None) => return Assigning::Free,
        Err(_) if fetched.load(std::sync::atomic::Ordering::Relaxed) => {
            return Assigning::Unread(UnreadWhy::Network);
        }
        Err(_) if no_name() => return Assigning::Unread(UnreadWhy::NoName),
        Err(_) => return Assigning::Unread(UnreadWhy::Source),
    };
    let Some(machine) = named else {
        return Assigning::Unread(UnreadWhy::NoName);
    };
    match names_of_this_mac(env, &machine) {
        Some(here) => Assigning::Names(Box::new(fleet), here),
        None => Assigning::Unread(UnreadWhy::Source),
    }
}

/// `machine` and the other names its swarm roster row gives it, whichever of them `machine` is: a
/// source may still name this Mac by one it had. `None` where that roster cannot be read, since
/// this Mac's own old name would then read as another Mac's. The name is passed in, so this runs
/// no hostname read.
fn names_of_this_mac(env: &Env, machine: &str) -> Option<Vec<String>> {
    let mut names = vec![machine_spelling(machine)];
    let identity = super::ai_pool::resolve_local_pool_identity(
        env.home.as_deref(),
        env.config.as_deref(),
        Some(machine),
    )
    .ok()?;
    if let Some(identity) = identity {
        names.extend(identity.aliases);
    }
    Some(names)
}

fn machine_spelling(machine: &str) -> String {
    normalize_machine_id(machine).unwrap_or_else(|| machine.to_ascii_lowercase())
}

/// The machine `fleet` gives `row`'s account to, where it names other machines alone: `None`
/// where it names one of `here`, names no machine for it or does not know the account. Sheet rows
/// that conflict over the address count too: where none of them names this Mac the sheet moved
/// the account off it, as [`carry_conflicting`] reads them.
fn only_another_machine<'a>(
    cli: Cli,
    fleet: &'a FleetRoster,
    here: &[String],
    row: &Row,
) -> Option<&'a str> {
    let named_here = |machines: &[String]| {
        machines
            .iter()
            .any(|machine| here.contains(&machine_spelling(machine)))
    };
    let mut holder = None;
    for account in fleet
        .accounts
        .iter()
        .filter(|account| fleet_matches_identity(cli, row, account))
    {
        if named_here(&account.machines) {
            return None;
        }
        holder = holder.or(account.machines.first());
    }
    for (address, conflict) in &fleet.conflicting {
        let the_address = row
            .email
            .as_deref()
            .is_some_and(|email| accounts::email_matches(address, email));
        if the_address {
            if named_here(&conflict.machines) {
                return None;
            }
            holder = holder.or(conflict.machines.first());
        }
    }
    holder.map(String::as_str)
}

/// Claude Code's [`needed_renew_candidates_for`], ordered without a store.
pub(super) fn needed_renew_candidates(
    file_roster: &Roster,
    state: &accounts::State,
    (logins, others): (&accounts::Logins, &accounts::OtherMacs),
    now: i64,
) -> Vec<accounts::Row> {
    needed_renew_candidates_for(Cli::Claude, None, file_roster, state, (logins, others), now)
}

/// The tier of a row in shared custody with no login here ([`auto_renew_candidates_tiered`]):
/// the root's decision, not this Mac's use, admits its sign-in, so a person's `accounts renew
/// --due`, the fact menu's rotation and a pooled CLI's upkeep never start it.
const CUSTODY_TIER: u8 = 2;

/// The rows of `cli`'s pool this Mac may sign in now because it uses them, best first
/// ([`auto_renew_candidates_tiered`] below [`CUSTODY_TIER`]): what a person's `accounts renew
/// --due`, the fact menu's rotation and the upkeeps start.
pub(super) fn needed_renew_candidates_for(
    cli: Cli,
    store: Option<&accounts::Store>,
    file_roster: &Roster,
    state: &accounts::State,
    (logins, others): (&accounts::Logins, &accounts::OtherMacs),
    now: i64,
) -> Vec<accounts::Row> {
    auto_renew_candidates_tiered(cli, store, file_roster, state, logins, others, now)
        .into_iter()
        .filter(|(tier, _)| *tier < CUSTODY_TIER)
        .map(|(_, row)| row)
        .collect()
}

/// THE ROWS AN UPKEEP SIGNS IN for `cli`'s pool, best first, each with its tier: only rows this
/// Mac uses (`DESIGN-alab-one-mesh-2026-11-05.md` §4.15 rule 4), each a door can mint a
/// [`SignIn`] for ([`upkeep_sign_in`]). Tier 0, every row a session here runs on whose login is
/// signed out here ([`super::sign_in_ladder::session_signed_out`]), whatever else is ready: that session's lost
/// login follows only its own row (`accounts::Point::LoginEnded`), and its supervisor waits on
/// this sign-in rather than typing `/login` in its tab. Tier 1, the logins due for renewal or
/// ended that this Mac ran on within a week and holds the claim of, a login whose directory has
/// gone since included ([`accounts::renew_due`]): a walled session's successor this Mac used is
/// one. Tier [`CUSTODY_TIER`], the rows a root put in this Mac's shared custody that have no
/// login here, which only the root's own current decision admits
/// ([`shared_renewal::custody_sign_in`]). Any other row nobody here uses is never one: rows no
/// session here has run on for a week wait for a person (`accounts login`, `accounts renew
/// <label>`, or a selection in the window's account menu), and a launch meanwhile picks a row
/// signed in here. Within a tier, roster order (rows this Mac prefers
/// first in tier 0), a row with an unaddressed sign-in task after every row without one and the
/// older task first (read through `store` where one is given, else from `logins` alone), a row
/// whose last renewal answered another account last. A row that runs on its vault token here
/// ([`accounts::OtherMacs::on_token`]) is in no tier: renewal is off for it.
fn auto_renew_candidates_tiered(
    cli: Cli,
    store: Option<&accounts::Store>,
    file_roster: &Roster,
    state: &accounts::State,
    logins: &accounts::Logins,
    others: &accounts::OtherMacs,
    now: impl Into<trace::Now>,
) -> Vec<(u8, accounts::Row)> {
    let mut tiered = rank_renewal_candidates(cli, store, file_roster, state, logins, others, now);
    // Ordinary renewal never substitutes for fresh root authorization, and never runs for a row
    // on its vault token (`DESIGN-claude-setup-token-2026-11-05` §5.8), whatever its tier.
    tiered.retain(|(_, row)| {
        matches!(logins.shared.required(row), Ok(None)) && !others.on_token.contains_key(&row.label)
    });
    tiered
}

fn rank_renewal_candidates(
    cli: Cli,
    store: Option<&accounts::Store>,
    file_roster: &Roster,
    state: &accounts::State,
    logins: &accounts::Logins,
    others: &accounts::OtherMacs,
    now: impl Into<trace::Now>,
) -> Vec<(u8, accounts::Row)> {
    let now: trace::Now = now.into();
    let roster = accounts::bound(file_roster, logins);
    let enrolled = accounts::enrolled_for(cli, &roster);
    let is_limited = |label: &str| {
        state
            .accounts
            .get(label)
            .is_some_and(|a| a.limited_until(now).is_some())
    };
    // By the stored login's reading: a row on its vault token is kept out of every tier only by
    // the live read (`others.on_token`, [`auto_renew_candidates_tiered`]), never by a reading kept
    // across a vault this pass could not read, which no launch here would start on.
    let wants_sign_in = |row: &accounts::Row| {
        let is_enr = row.dir.is_none() || enrolled.contains(&row.label);
        row.held_by.is_none()
            && (row.email.is_some() || cli == Cli::Kimi)
            && accounts::stored_login_wants_sign_in(row, state, is_enr, now)
    };
    let mut tiered: Vec<(u8, accounts::Row)> = Vec::new();
    let mut seen = BTreeSet::new();
    for row in roster
        .rows
        .iter()
        .filter(|r| r.class == Class::Preferred)
        .chain(roster.rows.iter().filter(|r| r.class == Class::Pool))
        .filter(|row| {
            wants_sign_in(row)
                && !is_limited(&row.label)
                && super::sign_in_ladder::session_signed_out(row, state)
        })
    {
        if seen.insert(row.label.clone()) {
            tiered.push((0, row.clone()));
        }
    }
    for label in accounts::renew_due(&roster, logins, state, others, now) {
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
    for row in roster.rows.iter().filter(|row| {
        matches!(logins.shared.required(row), Ok(Some(_)))
            && wants_sign_in(row)
            && !is_limited(&row.label)
    }) {
        if seen.insert(row.label.clone()) {
            tiered.push((CUSTODY_TIER, row.clone()));
        }
    }
    tiered.sort_by_key(|(tier, row)| {
        let task_at = match store {
            Some(store) => file_roster
                .row(&row.label)
                .and_then(|raw| unaddressed_sign_in_task_at(store, logins, raw)),
            None => file_roster
                .row(&row.label)
                .and_then(|raw| accounts::current_sign_in_task(raw, logins).map(|task| task.at)),
        };
        (
            *tier,
            task_at,
            logins.get(&row.label).and_then(|b| b.wrong_at),
        )
    });
    tiered
}

/// THE SELECTION'S SIGN-IN DOOR (§4.15 rule 4): a person's selection of one account from the
/// window's account menu, saved as `state`'s pending request ([`accounts::Store::select_account_for`])
/// and served by the menu's own thread or a later upkeep. `request` must be the one `state` holds.
fn selection_sign_in(
    cli: Cli,
    request: &accounts::PinRequest,
    state: &accounts::State,
) -> Option<SignIn> {
    SignIn::selected_sign_in(cli, request, state)
}

/// THE UPKEEP'S SIGN-IN DOOR (§4.15 rule 4): the [`SignIn`] for `row` that `records` show this
/// Mac may start, where one: a row a session here runs on whose login is signed out here, or one
/// due login this Mac uses. Read from the records the start reads, never from a ranking taken
/// before, so a row that stopped needing a sign-in mints nothing.
fn upkeep_sign_in(
    cli: Cli,
    (records, others): (&accounts::AccountRecords, &accounts::OtherMacs),
    row: &accounts::Row,
    now: i64,
) -> Option<SignIn> {
    if others.on_token.contains_key(&row.label) {
        return None;
    }
    let roster = accounts::bound(&records.raw_roster, &records.logins);
    let bound = roster.row(&row.label)?;
    SignIn::session_sign_in(cli, bound, &records.state).or_else(|| {
        SignIn::renewal_sign_in(cli, bound, (&records.logins, &records.state, others), now)
    })
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
    (guard, cli): (&std::sync::Arc<Watched<AutoRenewGuard>>, Cli),
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

/// WHETHER THE UPKEEP'S SIGN-IN OF A ROW OPENS ATERM'S OWN WINDOW: only this Mac's own
/// upkeep, at its own desk (`reads`: macOS, no SSH connection, no mark of another machine's
/// request and no session another Mac's drive typed into,
/// [`super::cli::opens_sign_in_window_by`]), for a row in no shared custody
/// (`shared_custody`: its sign-in is another holder's to give), and whose login code the
/// window reads by itself (`window_reads_mailbox`,
/// [`window_reads_mailbox_for_row_with`]): nobody is there to type one. The mailbox is read
/// last, and only where everything else allows the window.
fn upkeep_opens_window(
    reads: MachineReads,
    shared_custody: bool,
    window_reads_mailbox: &dyn Fn() -> bool,
) -> bool {
    !shared_custody
        && super::cli::opens_sign_in_window_by(reads.desk, &reads.var, &reads.driven)
        && window_reads_mailbox()
}

/// What opens the page of a sign-in the upkeep starts for a row with a mailbox or a proven
/// headless profile: aterm's own window where this run may open it (`window`,
/// [`upkeep_opens_window`]), else nothing a person sees, the opener signing in headlessly or
/// not at all. The window is told that nobody started it (`unattended`: the upkeep's own
/// ladder, or a saved choice or Next selection with nobody at this desk), so it ends at the read
/// that finds any check (`docs/DESIGN-claude-sign-in-sitting-2026-11-12.md` §3); only a click
/// whose person is at this desk now opens one that waits for them on a check.
fn upkeep_browser(window: bool, unattended: bool, opener: PathBuf) -> super::cli::RenewBrowser {
    if window {
        super::cli::RenewBrowser::Window { opener, unattended }
    } else {
        super::cli::RenewBrowser::Remote {
            opener: Some(opener),
        }
    }
}

/// The log's line for the token step after a row's sign-in, `signed_in` in aterm's window or
/// headlessly: the row as a log line names it (`logged`, its account's key with its slug,
/// [`super::account_names::logged_row`]), and that the token was made, and where, or what the
/// token verb `refused`, on one line and with each address in it written `<address>` (the verb
/// names the account to a person by its address; the log names it by its key). Only the token
/// step after a sign-in writes it, and that step, like [`super::accounts_tokens::SignedIn`], is
/// Unix's.
#[cfg(unix)]
fn token_step_line(
    logged: &str,
    signed_in: super::accounts_tokens::SignedIn,
    refused: &str,
) -> String {
    use super::accounts_tokens::SignedIn;
    let refused = refused.trim();
    format!(
        "{logged}: token: {}",
        if refused.is_empty() {
            match signed_in {
                SignedIn::Window => "made in the sign-in window",
                SignedIn::Headless => "made on its headless browser profile",
                SignedIn::Elsewhere => "made",
            }
            .to_owned()
        } else {
            super::open_url::without_addresses(&super::one_line(refused, 300))
        }
    )
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
    renew_guard: &std::sync::Arc<Watched<AutoRenewGuard>>,
    now: i64,
) -> bool {
    let fixture = aterm_tempfile::TempDir::new().unwrap();
    let path = fixture.path().join(accounts::ACCOUNTS_FILE);
    fs::write(&path, format_roster_toml(file_roster).unwrap()).unwrap();
    recover_interrupted_staging_renewals_for(
        Cli::Claude,
        store,
        &path,
        renew_guard,
        now,
        shared_renewal::CheckpointAuthority::Unavailable,
    )
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
    renew_guard: &std::sync::Arc<Watched<AutoRenewGuard>>,
    now: impl Into<trace::Now>,
    authority: shared_renewal::CheckpointAuthority<'_>,
) -> bool {
    recover_interrupted_staging_renewals_checked(
        cli,
        store,
        roster_path,
        renew_guard,
        now,
        (&accounts::LOCK_LADDER, &authority, None),
        || {},
    )
}

#[cfg(test)]
fn recover_interrupted_staging_renewals_with(
    cli: Cli,
    store: &accounts::Store,
    roster_path: &Path,
    renew_guard: &std::sync::Arc<Watched<AutoRenewGuard>>,
    now: i64,
    ladder: &[Duration],
    on_locked: impl FnMut(),
) -> bool {
    recover_interrupted_staging_renewals_checked(
        cli,
        store,
        roster_path,
        renew_guard,
        now,
        (
            ladder,
            &shared_renewal::CheckpointAuthority::Unavailable,
            None,
        ),
        on_locked,
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
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedAdoptionRecovery",
        action = "CodexCheckpoint",
        project = "aterm_agent::harness::cli::tests::shared_adoption_tests::recovery::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedAdoptionRecovery",
        action = "KimiCheckpoint",
        project = "aterm_agent::harness::cli::tests::shared_adoption_tests::recovery::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedAdoptionRecovery",
        action = "StagingAlias",
        project = "aterm_agent::harness::cli::tests::shared_adoption_tests::recovery::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedAdoptionRecovery",
        action = "PrincipalAlias",
        project = "aterm_agent::harness::cli::tests::shared_adoption_tests::recovery::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedCheckpointAuthority",
        action = "Revoke",
        project = "aterm_agent::harness::accounts_sync::recovery_lock_tests::checkpoint_authority::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedCheckpointAuthority",
        action = "Restore",
        project = "aterm_agent::harness::accounts_sync::recovery_lock_tests::checkpoint_authority::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedCheckpointAuthority",
        action = "Repeat",
        project = "aterm_agent::harness::accounts_sync::recovery_lock_tests::checkpoint_authority::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedCheckpointAuthority",
        action = "Decision",
        project = "aterm_agent::harness::accounts_sync::recovery_lock_tests::checkpoint_authority::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedCheckpointAuthority",
        action = "Configuration",
        project = "aterm_agent::harness::accounts_sync::recovery_lock_tests::checkpoint_authority::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedCheckpointAuthority",
        action = "Generation",
        project = "aterm_agent::harness::accounts_sync::recovery_lock_tests::checkpoint_authority::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedCheckpointAuthority",
        action = "Identity",
        project = "aterm_agent::harness::accounts_sync::recovery_lock_tests::checkpoint_authority::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedCheckpointAuthority",
        action = "Source",
        project = "aterm_agent::harness::accounts_sync::recovery_lock_tests::checkpoint_authority::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedCheckpointAuthority",
        action = "Principal",
        project = "aterm_agent::harness::accounts_sync::recovery_lock_tests::checkpoint_authority::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedCheckpointAuthority",
        action = "ChangedDuringCheck",
        project = "aterm_agent::harness::accounts_sync::recovery_lock_tests::checkpoint_authority::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedCheckpointAuthority",
        action = "Disabled",
        project = "aterm_agent::harness::accounts_sync::recovery_lock_tests::checkpoint_authority::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedCheckpointAuthority",
        action = "Unavailable",
        project = "aterm_agent::harness::accounts_sync::recovery_lock_tests::checkpoint_authority::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedCheckpointAuthority",
        action = "Offline",
        project = "aterm_agent::harness::accounts_sync::recovery_lock_tests::checkpoint_authority::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedCheckpointAuthority",
        action = "StagingAlias",
        project = "aterm_agent::harness::accounts_sync::recovery_lock_tests::checkpoint_authority::transition"
    )
)]
fn recover_interrupted_staging_renewals_checked(
    cli: Cli,
    store: &accounts::Store,
    roster_path: &Path,
    renew_guard: &std::sync::Arc<Watched<AutoRenewGuard>>,
    now: impl Into<trace::Now>,
    (ladder, authority, admitted): (
        &[Duration],
        &shared_renewal::CheckpointAuthority<'_>,
        Option<&accounts::SigninTurn>,
    ),
    mut on_locked: impl FnMut(),
) -> bool {
    let now: trace::Now = now.into();
    if admitted.is_some_and(|turn| turn.check_store(store).is_err()) {
        return false;
    }
    // Snapshot while observing idle, before scanning or waiting. A worker that
    // starts after this observation must retain its newer retry claims.
    let claims = {
        let state = renew_guard
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.in_flight {
            return false;
        }
        LoginClaims::from_state(&state)
    };
    let mut any_recovered = match store.recover_sync_within(ladder, roster_path) {
        Ok(recovered) => recovered,
        Err(_) => return false,
    };
    if (!claims.activity.is_empty() || !claims.ladder.is_empty())
        && let Ok((logins, _)) = store.load_logins(now)
    {
        note_observed_publications(renew_guard, claims.clone(), store, cli, &logins);
    }
    let Ok(scan) = local_renewal_checkpoints(cli, store, now) else {
        return any_recovered;
    };
    let mut candidates = scan.checkpoints;
    if candidates.is_empty() {
        return any_recovered;
    }
    // A checkpoint to recover: what follows writes and stamps, so it takes the reading raw.
    let now = now.raw();
    // Recovery publishes a sign-in too. Keep the machine-wide guard until every
    // candidate transaction ends, including when the lock file did not exist.
    // The account writer is acquired below, in the same order as fresh sign-ins.
    let _signin = match admitted {
        Some(turn) => {
            if turn.check_store(store).is_err() {
                return any_recovered;
            }
            None
        }
        None => match store.signin_now() {
            Ok(Some(held)) => Some(held),
            _ => return any_recovered,
        },
    };
    on_locked();
    // The unlocked scan is only a hint. Refresh proofs after admission so a
    // removed or changed checkpoint cannot be published from that earlier read.
    candidates.retain_mut(|candidate| {
        if trace::fs::canonicalize(&candidate.dir).ok().as_ref() != Some(&candidate.actual_dir) {
            return false;
        }
        let Some(proof) =
            accounts::read_renewal_checkpoint_for(cli, &candidate.dir, &candidate.actual_dir, now)
        else {
            return false;
        };
        if proof.enrollment.is_some() {
            return false;
        }
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
    for LocalRenewalCheckpoint {
        dir,
        actual_dir,
        proof,
        ..
    } in candidates
    {
        if admitted.is_some_and(|turn| turn.check_store(store).is_err()) {
            break;
        }
        if recovered_labels.contains(&proof.label) {
            continue;
        }
        // Managed grants require the native owner's authenticated encrypted
        // recovery path. A surviving stage file grants no ownership authority.
        if proof.native_grant.is_some() {
            continue;
        }
        // All candidates use the same writer. Do not multiply its contention
        // wait by the number of checkpoints while holding every provider's sign-in.
        let Ok(Some(writer)) = store.acquire_within(ladder) else {
            on_locked();
            break;
        };
        let mut note = None;
        let recovered = store
            .transact_roster_logins_held(
                &writer,
                roster_path,
                now,
                &mut note,
                |current, state, logins| {
                    // A prior candidate's transaction or this writer wait may outlive
                    // the refreshed proof. Keep the selected path and proof together.
                    if admitted.is_some_and(|turn| turn.check_store(store).is_err())
                        || proof.enrollment.is_some()
                        || logins.enrollment.owns_staging_path(&dir)
                        || logins.enrollment.owns_staging_path(&actual_dir)
                        || trace::fs::canonicalize(&dir).ok().as_ref() != Some(&actual_dir)
                        || !staged_login_present(cli, &actual_dir)
                        || accounts::read_renewal_checkpoint_for(cli, &dir, &actual_dir, now)
                            .as_ref()
                            != Some(&proof)
                    {
                        return Ok(Vec::new());
                    }
                    // A checkpoint can outlive the configuration read by upkeep.
                    // Every provider resolves its identity policy under the writer lock.
                    let file_roster = current;
                    if !accounts::checkpoint_unreferenced(file_roster, logins, &dir, &actual_dir) {
                        return Ok(Vec::new());
                    }
                    // Another process may have renewed or limited this account
                    // since the staging check. Decide under its lock.
                    let bound = accounts::bound(file_roster, logins);
                    let enrolled = accounts::enrolled_for(cli, &bound);
                    let Some(row) = bound
                        .row(&proof.label)
                        .filter(|row| accounts::checkpoint_identity_matches(cli, row, &proof))
                    else {
                        return Ok(Vec::new());
                    };
                    // Legacy shared adoption checkpoints cannot authorize a
                    // non-Claude identity or seal its shared custody. Leave
                    // them for the current root-authorized provider adapter.
                    if cli != Cli::Claude && !logins.shared.is_empty() {
                        let principal = (cli == Cli::Codex)
                            .then(|| {
                                super::codex_app_login::shared::local_principal(&dir, &proof.email)
                            })
                            .flatten();
                        if logins.shared.required(row)?.is_some()
                            || accounts::shared_staging_retained(logins, &proof.dir)?
                            || principal.is_some_and(|value| {
                                logins.shared.principal_source(value).is_some()
                            })
                        {
                            return Ok(Vec::new());
                        }
                    }
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
                    let needs_auth_recovery =
                        state.accounts.get(&row.label).is_some_and(|account| {
                            let auth = account.current_auth(row);
                            auth == accounts::Auth::SignedOut
                                || (account.auth != accounts::Auth::Unseen
                                    && auth == accounts::Auth::Unseen)
                        });
                    let pending_shared =
                        cli == Cli::Claude && logins.shared.pending_for(row, principal);
                    let needs_recovery =
                        !enrolled.contains(&row.label) || needs_auth_recovery || pending_shared;
                    if state
                        .accounts
                        .get(&row.label)
                        .is_some_and(|a| a.limited_until(now).is_some())
                        || !accounts::checkpoint_current(
                            row,
                            state,
                            logins,
                            &proof,
                            store.dir.parent(),
                        )
                        || !needs_recovery
                    {
                        return Ok(Vec::new());
                    }
                    if cli == Cli::Claude && !logins.shared.is_empty() {
                        if let Some(source) = logins.shared.required(row)?.or_else(|| {
                            principal
                                .and_then(|principal| logins.shared.principal_source(principal))
                        }) {
                            authority.check(source, row, logins, principal)?;
                        } else if accounts::shared_staging_retained(logins, &proof.dir)? {
                            return Err(
                                "shared staging cannot be recovered as an unrelated local account"
                                    .into(),
                            );
                        }
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
                    if let Some(request) = &proof.enrollment {
                        logins.enrollment.publishing(request)?;
                    }
                    if !events.is_empty()
                        && let Some(proved_by) = proof.proved_by
                    {
                        accounts::note_identity(logins, &row.label, proved_by, Some(&proof.email));
                        if let Some(principal) = proof.google_principal {
                            accounts::note_google_identity(
                                logins,
                                &row.label,
                                &proof.email,
                                principal,
                            );
                        }
                    }
                    Ok(events)
                },
            )
            .is_ok_and(|events| !events.is_empty());
        drop(writer);
        if recovered {
            note_published_login(renew_guard, &claims, store, cli, &proof.label);
            recovered_labels.insert(proof.label);
            any_recovered = true;
        }
        on_locked();
    }
    any_recovered
}

/// [`auto_renew_next_account_in`] as a pass of its own, with no adoption step before it (the
/// ladder pass a sync that found rows to sign in ends with): it reads its own witnesses, and
/// knows the rows the upkeep's last step could not lift ([`live_logins_known`]).
fn auto_renew_next_account(
    home: &Path,
    state_dir: &Path,
    roster_path: &Path,
    now: impl Into<trace::Now>,
    fx: &UpkeepEffects<'_>,
) -> bool {
    auto_renew_next_account_in(
        home,
        state_dir,
        roster_path,
        now,
        fx,
        &live_logins_known(fx.renew_guard),
    )
}

/// One upkeep pass's sign-in ladder for Claude Code's rows: starts at most one row's sign-in
/// ([`spawn_row_renewal`]); whether one started or an interrupted one was recovered. `live`
/// is the pass's witnesses, which the adoption step before it read
/// ([`adopt_live_logins_in_upkeep`]): each ranking here and the last ask under the sign-in
/// lock ask those, and open no file again.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedSelection",
        action = "Progress",
        project = "aterm_agent::harness::accounts_sync::shared_selection_tests::selection_transition"
    )
)]
fn auto_renew_next_account_in(
    home: &Path,
    state_dir: &Path,
    roster_path: &Path,
    now: impl Into<trace::Now>,
    fx: &UpkeepEffects<'_>,
    live: &super::login_adoption::Pass,
) -> bool {
    let now: trace::Now = now.into();
    if fx
        .scope
        .as_ref()
        .is_some_and(super::account_work::Scope::cancelled)
    {
        return false;
    }
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
        shared_renewal::CheckpointAuthority::background(
            Some(home),
            fx.shared.checkpoint_authority,
            roster_path,
        ),
    );
    // The pass's ranking: what the read ranked, or, with a shared authority installed (as the
    // app installs one), every row that authority may renew too. Either way no row the files
    // show live after its recorded sign-out is in it: the re-rank is filtered as the read's
    // own ranking was, by the same witnesses.
    let ranked = |candidates: &mut RenewalCandidates| {
        if fx.shared.authority.is_some() {
            candidates.tiered = rank_renewal_candidates(
                Cli::Claude,
                Some(&store),
                &candidates.records.raw_roster,
                &candidates.records.state,
                &candidates.records.logins,
                &candidates.others,
                now,
            );
            retain_not_live(
                Cli::Claude,
                &mut candidates.tiered,
                (home, state_dir),
                &store,
                &candidates.records,
                now,
                live,
            );
        }
    };
    let Ok(mut candidates) = read_renewal_candidates(
        Cli::Claude,
        &store,
        (home, state_dir),
        roster_path,
        now,
        live,
    ) else {
        return recovered;
    };
    ranked(&mut candidates);
    let ladder = LadderCtx::of(fx, roster_path).asking(live);
    for (_, planned_row) in candidates.tiered {
        // A previous row's route observation can create a binding. Start each
        // independent background attempt from current eligibility and custody.
        let Ok(mut current) = read_renewal_candidates(
            Cli::Claude,
            &store,
            (home, state_dir),
            roster_path,
            now,
            live,
        ) else {
            return recovered;
        };
        if current.records.raw_roster.row(&planned_row.label)
            != candidates.records.raw_roster.row(&planned_row.label)
        {
            continue;
        }
        ranked(&mut current);
        let Some((_, next_row)) = current
            .tiered
            .into_iter()
            .find(|(_, row)| row.label == planned_row.label)
        else {
            continue;
        };
        let Some(sign_in) = upkeep_sign_in(
            Cli::Claude,
            (&current.records, &current.others),
            &next_row,
            now.raw(),
        )
        .or_else(|| {
            shared_renewal::custody_sign_in(
                home,
                roster_path,
                &current.records,
                &next_row,
                fx.shared.authority,
            )
        }) else {
            continue;
        };
        if spawn_row_renewal(
            accounts::Cli::Claude,
            home,
            state_dir,
            roster_path,
            current.records.raw_roster,
            &current.records.logins,
            (next_row, sign_in),
            (None, false),
            now.raw(),
            fx.renew_guard,
            fx.spawn_renewal,
            Some(&ladder),
            fx.scope.as_ref(),
            &mut None,
        )
        .progressed()
        {
            return true;
        }
    }
    // A pass that started no sign-in makes, at most, one account's missing token.
    #[cfg(unix)]
    upkeep_tokens(home, state_dir, roster_path, now, fx);
    recovered
}

/// How long, in seconds, the token upkeep waits from finding the account `key`'s token missing
/// to making it on the machine whose platform id is `machine` ([`upkeep_tokens`]): under an hour,
/// fixed by the pair, so the Macs signed in to one account do not all start at the same pass
/// after the vault changes.
#[cfg(unix)]
fn token_delay_s(key: &str, machine: &str) -> i64 {
    let mut hash = aterm_digest::Sha256::new();
    hash.update(b"aterm-token-upkeep-delay-v1");
    for field in [key.as_bytes(), machine.as_bytes()] {
        hash.update((field.len() as u64).to_be_bytes());
        hash.update(field);
    }
    let digest = hash.finalize();
    let mut head = [0u8; 8];
    head.copy_from_slice(&digest[..8]);
    i64::try_from(u64::from_be_bytes(head) % 3600).unwrap_or(0)
}

/// THE HOME MAC'S TURN, IN SECONDS: how much longer than its own spread ([`token_delay_s`]) a Mac
/// that is not the account's home Mac waits on a gap any signed-in Mac may find
/// ([`super::accounts_tokens::Gap::any_mac`]), so the home Mac, which finds it too, has made the
/// token and its write has reached the servers before this Mac's fresh read: the home Mac's whole
/// spread (an hour), its own wait for a vault refresh after it plus the skew between the two Macs'
/// refreshes that found the gap (two of the ladder's refresh periods,
/// [`super::sign_in_ladder::BACKOFF_FIRST`]), another account's sign-in spacing it may wait behind
/// ([`super::sign_in_ladder::SPACING_S`]), and its capture's whole bound in aterm's window
/// ([`super::open_url::WINDOW_ROUTE_WITHIN`]): about an hour and 50 minutes. Two Macs that both
/// make one account's token leave a register conflict every Mac reads `mismatched (draft)` until
/// the vault settles it, and a second year-long token nobody uses.
#[cfg(unix)]
const HOME_TURN_S: i64 = 3_600
    + 2 * super::sign_in_ladder::BACKOFF_FIRST.as_secs() as i64
    + super::sign_in_ladder::SPACING_S
    + super::open_url::WINDOW_ROUTE_WITHIN.as_secs() as i64;

/// How long, in seconds, the token upkeep waits from finding `finding`'s gap to making its token
/// on the machine whose platform id is `machine` ([`upkeep_tokens`]): its spread
/// ([`token_delay_s`]), and before it the home Mac's turn ([`HOME_TURN_S`]) where the account has
/// a home Mac that is not this one (`away`) and the gap is one any signed-in Mac may find. A Mac
/// that is the account's home, or one where no pool names a home for it, waits its spread alone.
#[cfg(unix)]
fn token_wait_s(finding: &super::accounts_tokens::Finding, away: bool, machine: &str) -> i64 {
    let turn = if away && finding.gap.any_mac() {
        HOME_TURN_S
    } else {
        0
    };
    turn + token_delay_s(&finding.key, machine)
}

/// THE UPKEEP MAKES THE MISSING TOKEN OF EACH ROW SIGNED IN HERE, WITH NOBODY THERE (plan package
/// FL-138): one account per pass, and only on a pass that started no sign-in. A row is looked at
/// where it is Claude Code's, names its email, is held by no other Mac, and its login is made
/// and signed in here ([`accounts::LoginHere::In`]); its token page needs a route no person
/// finishes ([`super::accounts_tokens::NobodyRoutes`]: aterm's window, whose store holds the
/// account's session ([`super::signin_window::store_holds_session`]) or which reads its mailbox
/// ([`window_reads_mailbox_for_row_with`]), or the headless profile a sign-in proved), and this
/// process runs at this Mac's own desk ([`super::cli::opens_sign_in_window_by`]) in a desktop
/// session ([`MachineReads::graphic`]) with no sign-in running. What is missing, and whether it is
/// this Mac's to make, is [`super::accounts_tokens::backfill_due`]'s; a Mac not seated in the
/// vault's log makes none, and says so once in the log (`vault-not-seated`) while it stands.
///
/// A gap found waits [`token_wait_s`] from when it was found (on a Mac that is not the account's
/// home Mac, a gap any signed-in Mac finds waits the home Mac's turn first), then for a vault
/// refresh that started after that wait (the ladder's own, on its clock: [`Ladder::vault_turn`])
/// and read the whole vault ([`super::sign_in_ladder::Vault::read_whole`]), and is judged again on
/// what that refresh applied: another Mac may have made the token meanwhile. Its capture starts
/// under the sign-in spacing ([`super::sign_in_ladder::space_token_capture`]), so a second
/// account's waits [`super::sign_in_ladder::SPACING_S`] after the first's, off this thread, with
/// the sign-in lock let go ([`super::accounts_tokens::backfill_run`]), and each gap runs once on
/// record ([`super::token_steps::Tried`]), past a restart, until something that may let it pass
/// changes. A page only a person can pass ends the run and is recorded where the account's step
/// is held ([`super::token_steps`]); nothing presses, solves or retries it, and nobody is asked.
/// The log gets one line per capture, with no address in it. Whether a capture started.
///
/// [`Ladder::vault_turn`]: super::sign_in_ladder::Ladder::vault_turn
#[cfg(unix)]
fn upkeep_tokens(
    home: &Path,
    state_dir: &Path,
    roster_path: &Path,
    now: trace::Now,
    fx: &UpkeepEffects<'_>,
) -> bool {
    use super::accounts_tokens::{Backfill, NobodyRoutes};
    use super::sign_in_ladder::{TokenSpacing, VaultTurn};
    fn lock(
        guard: &std::sync::Arc<Watched<AutoRenewGuard>>,
    ) -> trace::WatchedGuard<'_, AutoRenewGuard> {
        guard
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    let reads = guard_reads(fx.renew_guard);
    let Some(token_host) = reads.tokens else {
        return false;
    };
    if fx
        .scope
        .as_ref()
        .is_some_and(super::account_work::Scope::cancelled)
        || !super::cli::opens_sign_in_window_by(reads.desk, &reads.var, &reads.driven)
        || !(reads.graphic)()
        || lock(fx.renew_guard).token_tier.running
    {
        return false;
    }
    let store = accounts::Store::at(state_dir);
    if store.signing_in().is_some() {
        return false;
    }
    // Past here the gaps, their waits and the vault's standing are judged on the reading as a
    // number (`backfill_due`, `token_wait_s`, the capture's spacing), so a run that gets here
    // takes it raw and is never kept: the keeper makes it again at its rerun
    // (`trace::RERUN_CAP`). A run that stopped above compared no clock.
    let now = now.raw();
    let Ok(records) = store.read_account_records(Some(roster_path), now) else {
        return false;
    };
    let enrolled = accounts::enrolled(&records.roster);
    let config = config_beside(roster_path);
    let env = quiet_env(home, state_dir, &config, now);
    let host = token_host(fx.scope.clone());
    let machine = (reads.machine)().unwrap_or_default();
    // First every row's gap, and which gaps' waits are over; then, at most, one capture.
    let mut found_here = BTreeSet::new();
    let mut ripe = Vec::new();
    for row in &records.roster.rows {
        let Some(email) = row
            .email
            .as_deref()
            .filter(|email| !email.trim().is_empty())
        else {
            continue;
        };
        if row.held_by.is_some()
            || accounts::login_here(row, &records.state, enrolled.contains(&row.label), now)
                != accounts::LoginHere::In
        {
            continue;
        }
        #[cfg(target_os = "macos")]
        let holds_session = super::signin_window::store_holds_session(&store.dir, email);
        #[cfg(not(target_os = "macos"))]
        let holds_session = false;
        let routes = NobodyRoutes {
            window: holds_session
                || window_reads_mailbox_for_row_with(
                    &reads.var,
                    Some(home),
                    &store,
                    Some(&row.label),
                    Some(email),
                ),
            headless: super::headless_oauth::has_browser_profile(
                &store.dir,
                &row.label,
                Some(email),
            ),
        };
        let home_mac = records.state.home_accounts.contains(&row.label);
        // A pool names this Mac (`swarm_label`), and the row, held by no other Mac, is warm
        // here: its home Mac is the pool's `warm[0]`, here or on another Mac. With no pool, no
        // Mac is its home.
        let away = !home_mac && records.state.swarm_label.is_some();
        let finding = match super::accounts_tokens::backfill_due(
            &env,
            host.as_ref(),
            &row.label,
            home_mac,
            routes,
        ) {
            Backfill::Due(finding) => finding,
            Backfill::NotSeated => {
                let mut guard = lock(fx.renew_guard);
                if !guard.token_tier.not_seated {
                    guard.token_tier.not_seated = true;
                    drop(guard);
                    (fx.log)(
                        "token upkeep: vault-not-seated: this Mac is not seated in the vault's \
                         log, so it makes no account's token (`aterm secrets status` says vault \
                         writes:)",
                    );
                }
                return false;
            }
            Backfill::Nothing => continue,
        };
        found_here.insert(row.label.clone());
        let mut guard = lock(fx.renew_guard);
        let tier = &mut guard.token_tier;
        tier.not_seated = false;
        let gap = tier
            .found
            .entry(row.label.clone())
            .and_modify(|gap| {
                if gap.finding != finding {
                    *gap = TokenGap {
                        finding: finding.clone(),
                        at: now,
                        ripe: None,
                    };
                }
            })
            .or_insert_with(|| TokenGap {
                finding: finding.clone(),
                at: now,
                ripe: None,
            });
        if now
            >= gap
                .at
                .saturating_add(token_wait_s(&finding, away, &machine))
        {
            let since = *gap.ripe.get_or_insert_with(reads.clock);
            ripe.push((row.label.clone(), finding, home_mac, routes, since));
        }
    }
    lock(fx.renew_guard)
        .token_tier
        .found
        .retain(|label, _| found_here.contains(label));
    let Some((label, finding, home_mac, routes, since)) = ripe.into_iter().next() else {
        return false;
    };
    // A fresh read of the vault: the ladder's refresh, on its own clock, started once the wait
    // was over. Until one has ended, nothing runs.
    let VaultInput::Read(key) = vault_input(home) else {
        return false;
    };
    let at = (reads.clock)();
    let turn = lock(fx.renew_guard).ladder.vault_turn(Some(key), at);
    match turn {
        VaultTurn::Running | VaultTurn::Unavailable => return false,
        VaultTurn::Due => {
            let _ = start_vault_refresh(
                home,
                key,
                at,
                fx.renew_guard,
                &LadderCtx::of(fx, roster_path),
            );
        }
        VaultTurn::Seen(_) => {}
    }
    let fresh = lock(fx.renew_guard).ladder.vault_seen_since(since);
    if !fresh.is_some_and(|seen| seen.read_whole()) {
        return false;
    }
    // What that refresh applied may hold the token another Mac made meanwhile.
    match super::accounts_tokens::backfill_due(&env, host.as_ref(), &label, home_mac, routes) {
        Backfill::Due(again) if again == finding => {}
        Backfill::Due(_) | Backfill::NotSeated | Backfill::Nothing => return false,
    }
    match super::sign_in_ladder::space_token_capture(&store, &label, now) {
        TokenSpacing::Started => {}
        TokenSpacing::Until(_) | TokenSpacing::Busy => return false,
    }
    {
        let mut guard = lock(fx.renew_guard);
        guard.token_tier.found.remove(&label);
        guard.token_tier.running = true;
    }
    let guard = std::sync::Arc::clone(fx.renew_guard);
    let log = std::sync::Arc::clone(&fx.log);
    let scope = fx.scope.clone();
    let (home_owned, state_owned) = (home.to_path_buf(), state_dir.to_path_buf());
    let work = Box::new(move || {
        let env = quiet_env(&home_owned, &state_owned, &config, now);
        let host = token_host(scope);
        let (mut said, mut refused) = (Vec::new(), Vec::new());
        // A check the token's page carried that could not be recorded on the account is held
        // in this process until it is ([`hold_for_a_person`]).
        let mut unrecorded = |check: PendingCheck| {
            lock(&guard).unrecorded_checks.insert(label.clone(), check);
        };
        if let Some(code) = super::accounts_tokens::backfill_run(
            &env,
            host.as_ref(),
            &label,
            &finding,
            home_mac,
            routes,
            (&mut said, &mut refused),
            &mut unrecorded,
        ) {
            log(&backfill_line(
                &label,
                &finding,
                code,
                &String::from_utf8_lossy(&refused),
            ));
        }
        lock(&guard).token_tier.running = false;
    });
    if (fx.spawn_renewal)(work).is_err() {
        lock(fx.renew_guard).token_tier.running = false;
        return false;
    }
    true
}

/// The log's line for a token capture the upkeep ran with nobody there ([`upkeep_tokens`]), by
/// the verb's exit `code`: where it succeeded, that the token was made, and where, followed by
/// any warning the verb wrote (`said`: a table or record it could not write); otherwise what the
/// verb refused. On one line, with each address in it written `<address>`.
#[cfg(unix)]
fn backfill_line(
    label: &str,
    finding: &super::accounts_tokens::Finding,
    code: ExitCode,
    said: &str,
) -> String {
    use super::accounts_tokens::Via;
    let said = said.trim();
    let said =
        (!said.is_empty()).then(|| super::open_url::without_addresses(&super::one_line(said, 300)));
    let what = if code == ExitCode::SUCCESS {
        let made = match finding.via {
            Via::Unattended => "made in the sign-in window with nobody there",
            Via::Headless => "made on its headless browser profile",
            Via::Desk => "made",
        };
        said.map_or_else(|| made.to_owned(), |said| format!("{made}; {said}"))
    } else {
        said.unwrap_or_else(|| "not made".to_owned())
    };
    format!("{label}: token ({}): {what}", finding.gap.word())
}

pub(super) fn recover_staging_renewals_for(
    cli: Cli,
    store: &accounts::Store,
    roster_path: &Path,
    now: i64,
    authority: shared_renewal::CheckpointAuthority<'_>,
) -> bool {
    recover_interrupted_staging_renewals_for(
        cli,
        store,
        roster_path,
        &AUTO_RENEW_GUARD,
        now,
        authority,
    )
}

pub(super) fn recover_staging_renewals_held(
    cli: Cli,
    store: &accounts::Store,
    roster_path: &Path,
    now: i64,
    authority: shared_renewal::CheckpointAuthority<'_>,
    turn: &accounts::SigninTurn,
) -> bool {
    recover_interrupted_staging_renewals_checked(
        cli,
        store,
        roster_path,
        &AUTO_RENEW_GUARD,
        now,
        (&accounts::LOCK_LADDER, &authority, Some(turn)),
        || {},
    )
}

/// Start an interactive non-blocking staging login for the next row this Mac may sign in
/// ([`upkeep_sign_in`]): a row a session here runs on that lost its login, or a due login it uses.
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
    let lifetime = super::account_work::Lifetime::default();
    let scope = lifetime.enter().expect("new account lifetime admits work");
    auto_renew_next_account_interactive_for_scoped(scope, cli, home, state_dir, roster_path, now)
}

/// Starts interactive App-owned work only under the caller's admitted scope.
pub fn auto_renew_next_account_interactive_for_scoped(
    scope: super::account_work::Scope,
    cli: Cli,
    home: &Path,
    state_dir: &Path,
    roster_path: &Path,
    now: i64,
) -> bool {
    if scope.cancelled() {
        return false;
    }
    with_live_scope(scope, |fx| {
        auto_renew_next_account_interactive_with(cli, home, state_dir, roster_path, now, fx)
    })
}

/// [`auto_renew_next_account_interactive_for`] making `fx`'s effects. The config beside the
/// roster holds it as it holds the window's upkeep ([`upkeep`]): `[harness] accounts = "off"`
/// starts nothing, and while `[harness] logins = "off"`, `enabled = false` or `cli`'s own table's
/// `enabled = false` turns `cli`'s automatic sign-ins off ([`sign_ins_on`]) only a saved explicit
/// choice completes.
fn auto_renew_next_account_interactive_with(
    cli: Cli,
    home: &Path,
    state_dir: &Path,
    roster_path: &Path,
    now: i64,
    fx: &UpkeepEffects<'_>,
) -> bool {
    if fx
        .scope
        .as_ref()
        .is_some_and(super::account_work::Scope::cancelled)
    {
        return false;
    }
    let config = config_beside(roster_path);
    if accounts_sync_off(Some(&config)) {
        return false;
    }
    if let Some(pending) = resume_pending_pin_for(cli, home, state_dir, roster_path, now, fx) {
        return pending.progressed();
    }
    if !sign_ins_on(&config, cli) {
        return false;
    }
    let store = accounts::Store::at_for(state_dir, cli, home);
    let recovered = recover_interrupted_staging_renewals_for(
        cli,
        &store,
        roster_path,
        fx.renew_guard,
        now,
        shared_renewal::CheckpointAuthority::background(
            Some(home),
            fx.shared.checkpoint_authority,
            roster_path,
        ),
    );
    let Ok(candidates) = read_renewal_candidates(
        cli,
        &store,
        (home, state_dir),
        roster_path,
        now,
        &live_logins_known(fx.renew_guard),
    ) else {
        return recovered;
    };
    // A person's click that names no row starts what the upkeep's door admits (a row a session
    // here runs on that lost its login, or a due login this Mac uses), in a browser where no
    // headless route is ready, never a row nobody here uses.
    for (_, next_row) in candidates.tiered {
        let Some(sign_in) = upkeep_sign_in(
            cli,
            (&candidates.records, &candidates.others),
            &next_row,
            now,
        ) else {
            continue;
        };
        if spawn_row_renewal(
            cli,
            home,
            state_dir,
            roster_path,
            candidates.records.raw_roster.clone(),
            &candidates.records.logins,
            (next_row, sign_in),
            (None, true),
            now,
            fx.renew_guard,
            fx.spawn_renewal,
            None,
            fx.scope.as_ref(),
            &mut None,
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
    /// A counted worker owns the request; provider admission remains separate.
    Started,
    /// The saved selection completed without a new provider invocation.
    Recovered,
    /// This exact refused selection was durably retired; no authentication succeeded.
    Retired,
    /// Another renewal or account writer owns admission; keep the request for a later upkeep.
    Held,
    /// Definite startup, authentication or policy failure; this attempt cannot proceed.
    Failed,
    /// Account records are unreadable or recovery is fenced; retain the saved selection
    /// and start no authentication until the store is healthy.
    Fenced,
    /// A newer selection or an explicit cancellation replaced this request.
    Superseded,
    /// The platform has no admitted private manual FIFO implementation, so it can never
    /// run this selection: that exact selection, where still pending, was retired, and
    /// nothing authenticated.
    Unavailable,
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
fn renewal_signin_admission(
    store: &accounts::Store,
) -> Result<accounts::SigninTurn, RenewalDispatch> {
    match store.signin_now() {
        Ok(Some(lock)) => Ok(lock),
        Ok(None) => Err(RenewalDispatch::Held),
        Err(_) => Err(RenewalDispatch::Fenced),
    }
}

/// Whether `sign_in` may start now as `store`'s spacing clock reads
/// ([`super::sign_in_ladder::spaced_until`]): another account's sign-in started here within ten
/// minutes holds an automatic one ([`RenewalDispatch::Held`], tried again at a later upkeep), and
/// an unreadable clock fences it.
fn spacing_admits(
    sign_in: &SignIn,
    store: &accounts::Store,
    now: i64,
) -> Result<(), RenewalDispatch> {
    match sign_in.spaced(store, now) {
        Ok(None) => Ok(()),
        Ok(Some(until)) => Err(spacing_refusal(&super::sign_in_ladder::Refused::Spaced {
            until,
        })),
        Err(refused) => Err(spacing_refusal(&refused)),
    }
}

/// Whether the fleet's files hold `sign_in` ([`SignIn::fleet_holds`]): a block of its account's
/// sign-in that a Mac of the fleet recorded within a day holds an automatic one
/// ([`RenewalDispatch::Held`], tried again at a later upkeep), and a store that does not tell
/// which account it is fences it. A door makes this check before it claims its permit or takes
/// a person's queued admission, as it checks the spacing ([`spacing_admits`]), so a held row
/// costs nothing and a Next selection goes on to its next row; [`SignIn::begin`] checks it
/// again, where it decides. A Claude Code row's ladder holds it itself, after its vault route
/// ([`ladder_route_in_context`]), so the door checks it only past the ladder.
fn fleet_admits(
    sign_in: &SignIn,
    store: &accounts::Store,
    now: i64,
) -> Result<(), RenewalDispatch> {
    sign_in
        .fleet_holds(store, now)
        .map_err(|refused| spacing_refusal(&refused))
}

/// How a background start answers [`SignIn::begin`]'s refusal.
fn spacing_refusal(refused: &super::sign_in_ladder::Refused) -> RenewalDispatch {
    match refused {
        super::sign_in_ladder::Refused::Spaced { .. }
        | super::sign_in_ladder::Refused::Fleet { .. } => RenewalDispatch::Held,
        super::sign_in_ladder::Refused::Record(_) => RenewalDispatch::Fenced,
        super::sign_in_ladder::Refused::Another => RenewalDispatch::Failed,
    }
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedProgress",
        action = "Reissue",
        project = "aterm_agent::harness::accounts_sync::automatic_shared_tests::shared_progress_transition"
    )
)]
fn record_shared_preparation(
    guard: &mut AutoRenewGuard,
    label: &str,
    grant: &super::cli::shared_renewal::Grant,
) {
    let current = (grant.source, grant.decision);
    if guard.shared_preparations.get(label) != Some(&current) {
        guard.shared_preparations.insert(label.to_owned(), current);
        guard.last_attempt.remove(label);
        guard.ladder.preparation_changed(label);
        guard.ladder_owners.remove(label);
    }
}

fn queued_pin_admitted(
    store: &accounts::Store,
    policy: accounts::RenewalPolicy<'_>,
    request: Option<&accounts::PinRequest>,
    selection: Option<&accounts::PendingSelection>,
    now: i64,
) -> bool {
    if let Some(selection) = selection {
        return store
            .admit_manual_renewal_attempt(policy, selection, now)
            .unwrap_or(false);
    }
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
    now: impl Into<trace::Now>,
    fx: &UpkeepEffects<'_>,
) -> Option<RenewalDispatch> {
    resume_pending_pin_for(Cli::Claude, home, state_dir, roster_path, now, fx)
}

fn resume_pending_pin_for(
    cli: Cli,
    home: &Path,
    state_dir: &Path,
    roster_path: &Path,
    now: impl Into<trace::Now>,
    fx: &UpkeepEffects<'_>,
) -> Option<RenewalDispatch> {
    let now: trace::Now = now.into();
    if fx
        .scope
        .as_ref()
        .is_some_and(super::account_work::Scope::cancelled)
    {
        return Some(RenewalDispatch::Held);
    }
    let store = accounts::Store::at_for(state_dir, cli, home);
    if let Err(why) = store.recover_sync_within(&accounts::LOCK_LADDER, roster_path) {
        return Some(RenewalDispatch::store_refusal(&why.to_string()));
    }
    let request = match store.load(now) {
        Ok((state, _)) => state.pending_pin?,
        Err(why) => return Some(RenewalDispatch::store_refusal(&why)),
    };
    let result = reattach_manual_selection(
        cli,
        home,
        state_dir,
        roster_path,
        request.clone(),
        now.raw(),
        fx,
    );
    if result == RenewalDispatch::Retired {
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
    let lifetime = super::account_work::Lifetime::default();
    let scope = lifetime.enter().expect("new account lifetime admits work");
    auto_renew_account_by_label_for_scoped(scope, cli, home, state_dir, roster_path, request, now)
}

/// Renews a saved App selection under the caller's admitted lifetime.
pub fn auto_renew_account_by_label_for_scoped(
    scope: super::account_work::Scope,
    cli: Cli,
    home: &Path,
    state_dir: &Path,
    roster_path: &Path,
    request: accounts::PinRequest,
    now: i64,
) -> RenewalDispatch {
    if scope.cancelled() {
        return RenewalDispatch::Held;
    }
    with_live_scope(scope, |fx| {
        renew_requested_pin_at(
            cli,
            (home, state_dir, roster_path),
            request,
            now,
            (fx.renew_guard, fx.spawn_renewal, fx.scope.as_ref()),
            None,
            false,
        )
    })
}

/// Queue an explicit saved menu choice on its existing App-owned worker.
/// Waiting starts no provider/preparation work and never holds the account writer.
pub fn manual_renew_account_for_scoped(
    scope: super::account_work::Scope,
    cli: Cli,
    home: &Path,
    state_dir: &Path,
    roster_path: &Path,
    request: accounts::PinRequest,
    now: i64,
) -> RenewalDispatch {
    with_live_scope(scope, |fx| {
        manual_selection_with(cli, home, state_dir, roster_path, request.into(), now, fx)
    })
}

/// Queue the exact saved next-account intent. Its row is chosen only after the turn.
pub fn manual_renew_next_for_scoped(
    scope: super::account_work::Scope,
    cli: Cli,
    home: &Path,
    state_dir: &Path,
    roster_path: &Path,
    request: accounts::NextRequest,
    now: i64,
) -> RenewalDispatch {
    with_live_scope(scope, |fx| {
        manual_selection_with(
            cli,
            home,
            state_dir,
            roster_path,
            accounts::PendingSelection::Next(request),
            now,
            fx,
        )
    })
}

fn manual_owner_for(
    cli: Cli,
    store: &accounts::Store,
    roster_path: &Path,
    selection: accounts::PendingSelection,
    now: i64,
    fx: &UpkeepEffects<'_>,
) -> Result<ManualSelectionOwner, RenewalDispatch> {
    if fx
        .scope
        .as_ref()
        .is_none_or(super::account_work::Scope::cancelled)
    {
        return Err(RenewalDispatch::Held);
    }
    let row = match selection.account() {
        Some(request) => {
            let records = store
                .read_account_records(Some(roster_path), now)
                .map_err(|_| RenewalDispatch::Fenced)?;
            records.roster.row(request.label()).cloned()
        }
        None => None,
    };
    ManualSelectionOwner::claim(fx.renew_guard, store, cli, selection, row.as_ref(), now)
}

fn manual_selection_with(
    cli: Cli,
    home: &Path,
    state_dir: &Path,
    roster_path: &Path,
    selection: accounts::PendingSelection,
    now: i64,
    fx: &UpkeepEffects<'_>,
) -> RenewalDispatch {
    let store = accounts::Store::at_for(state_dir, cli, home);
    let owner = match manual_owner_for(cli, &store, roster_path, selection, now, fx) {
        Ok(owner) => owner,
        Err(disposition) => return disposition,
    };
    manual_selection_owned(
        cli,
        home,
        state_dir,
        roster_path,
        (owner, Gesture::Clicked),
        now,
        fx,
    )
}

/// Who a manual selection is served for when its sign-in turn comes: the person whose click
/// is waiting on it now, or nobody (a saved selection the upkeep resumed). Only an uncontended
/// click may open a window ([`spawn_row_renewal`]'s `at_desk`): a selection that waited in the
/// queue, or that a tick resumed after a restart, is served by routes that open none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Gesture {
    /// A person's click, on the thread it started.
    Clicked,
    /// A saved selection, resumed with nobody waiting on it.
    Resumed,
}

fn manual_selection_owned(
    cli: Cli,
    home: &Path,
    state_dir: &Path,
    roster_path: &Path,
    (owner, gesture): (ManualSelectionOwner, Gesture),
    now: i64,
    fx: &UpkeepEffects<'_>,
) -> RenewalDispatch {
    let store = accounts::Store::at_for(state_dir, cli, home);
    let refusal = std::cell::Cell::new(RenewalDispatch::Held);
    // A click that queued behind another sign-in is served when its person may have left: it
    // opens no window, exactly as a resumed selection does.
    let waited = std::cell::Cell::new(false);
    let current = || {
        if fx
            .scope
            .as_ref()
            .is_none_or(super::account_work::Scope::cancelled)
        {
            return false;
        }
        if !owner.owns() {
            refusal.set(RenewalDispatch::Superseded);
            return false;
        }
        match store.load(now) {
            Ok((state, _)) if state.pending_pin.as_ref() == Some(owner.selection()) => true,
            Ok(_) => {
                refusal.set(RenewalDispatch::Superseded);
                false
            }
            Err(_) => {
                refusal.set(RenewalDispatch::Fenced);
                false
            }
        }
    };
    let acquired = store.manual_signin_while(&current, |position| {
        waited.set(true);
        owner.position(Some(position));
    });
    let turn = match acquired {
        Ok(Some((turn, _))) => turn,
        Ok(None) => return refusal.get(),
        Err(error) => {
            let why = error.to_string();
            let refused = manual_queue_refused(&store, owner.selection(), error, now);
            if refused == RenewalDispatch::Unavailable {
                (fx.log)(&retired_selection_line(owner.selection(), &why));
            }
            return refused;
        }
    };
    owner.position(None);
    // A prior worker may be retiring its permit after releasing the signer. Keep
    // the actual turn while it finishes; no account mutex is held during this wait.
    while fx
        .renew_guard
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .in_flight
    {
        if !current() {
            return refusal.get();
        }
        waited.set(true);
        std::thread::sleep(accounts::LOCK_LADDER[0]);
    }
    if !current() {
        return refusal.get();
    }
    let at_desk = gesture == Gesture::Clicked && !waited.get();
    let now = crate::supervise::limit::unix_now().max(now);
    let selection = owner.selection().clone();
    match selection {
        accounts::PendingSelection::Account(request) => renew_requested_pin_at(
            cli,
            (home, state_dir, roster_path),
            request,
            now,
            (fx.renew_guard, fx.spawn_renewal, fx.scope.as_ref()),
            Some((turn, owner)),
            at_desk,
        ),
        accounts::PendingSelection::Next(request) => manual_next_after_turn(
            cli,
            (home, state_dir, roster_path),
            request,
            now,
            (turn, owner, at_desk),
            fx,
        ),
    }
}

fn manual_queue_refused(
    store: &accounts::Store,
    selection: &accounts::PendingSelection,
    error: io::Error,
    now: i64,
) -> RenewalDispatch {
    if error.kind() != io::ErrorKind::Unsupported {
        return RenewalDispatch::Fenced;
    }
    // This platform can never execute the intent, and no provider was admitted.
    // Upkeep resumes a pending selection before any other sign-in, so one kept
    // here would be retried every pass and hold every other account back. Retire
    // only that exact selection, of either kind: a newer selection and the
    // durable preferred pin stay as they are.
    let cancelled = match selection {
        accounts::PendingSelection::Account(request) => store.cancel_pin_request(request, now),
        accounts::PendingSelection::Next(request) => store.cancel_next_request(request, now),
    };
    if cancelled.is_err() {
        return RenewalDispatch::Fenced;
    }
    RenewalDispatch::Unavailable
}

/// The sign-in log's line for a selection [`manual_queue_refused`] retired, and why.
fn retired_selection_line(selection: &accounts::PendingSelection, why: &str) -> String {
    match selection {
        accounts::PendingSelection::Account(request) => {
            format!(
                "account {}: its selection was retired: {why}",
                request.label()
            )
        }
        accounts::PendingSelection::Next(_) => {
            format!("next account: its selection was retired: {why}")
        }
    }
}

fn manual_next_after_turn(
    cli: Cli,
    (home, state_dir, roster_path): (&Path, &Path, &Path),
    request: accounts::NextRequest,
    now: i64,
    (turn, owner, at_desk): (accounts::SigninTurn, ManualSelectionOwner, bool),
    fx: &UpkeepEffects<'_>,
) -> RenewalDispatch {
    let store = accounts::Store::at_for(state_dir, cli, home);
    let config = config_beside(roster_path);
    if turn.check_store(&store).is_err() {
        return RenewalDispatch::Fenced;
    }
    if accounts_sync_off(Some(&config)) || !sign_ins_on(&config, cli) {
        return RenewalDispatch::Held;
    }
    if store.check_signin_state(now).is_err() {
        return RenewalDispatch::Fenced;
    }
    recover_interrupted_staging_renewals_checked(
        cli,
        &store,
        roster_path,
        fx.renew_guard,
        now,
        (
            &accounts::LOCK_LADDER,
            &shared_renewal::CheckpointAuthority::live(Some(home), roster_path),
            Some(&turn),
        ),
        || {},
    );
    if turn.check_store(&store).is_err() {
        return RenewalDispatch::Fenced;
    }
    if fx
        .scope
        .as_ref()
        .is_none_or(super::account_work::Scope::cancelled)
    {
        return RenewalDispatch::Held;
    }
    let candidates = match read_renewal_candidates(
        cli,
        &store,
        (home, state_dir),
        roster_path,
        now,
        &live_logins_known(fx.renew_guard),
    ) {
        Ok(candidates) => candidates,
        Err(_) => return RenewalDispatch::Fenced,
    };
    if candidates
        .records
        .state
        .pending_pin
        .as_ref()
        .and_then(accounts::PendingSelection::next)
        != Some(&request)
    {
        return RenewalDispatch::Superseded;
    }
    let mut admission = Some((turn, owner));
    let mut held = false;
    for (_, row) in candidates.tiered {
        if fx
            .scope
            .as_ref()
            .is_none_or(super::account_work::Scope::cancelled)
        {
            return RenewalDispatch::Held;
        }
        let Some(sign_in) =
            upkeep_sign_in(cli, (&candidates.records, &candidates.others), &row, now)
        else {
            continue;
        };
        let result = spawn_row_renewal(
            cli,
            home,
            state_dir,
            roster_path,
            candidates.records.raw_roster.clone(),
            &candidates.records.logins,
            (row, sign_in),
            (None, at_desk),
            now,
            fx.renew_guard,
            fx.spawn_renewal,
            None,
            fx.scope.as_ref(),
            &mut admission,
        );
        // Once an actual attempt owns admission it alone completes/retains the
        // intent. A preflight-only refusal leaves the SAME ticket for the next row.
        if admission.is_none() || result.progressed() {
            return if result == RenewalDispatch::Failed {
                RenewalDispatch::Held
            } else {
                result
            };
        }
        if result == RenewalDispatch::Fenced {
            return result;
        }
        held |= result == RenewalDispatch::Held;
    }
    if held {
        return RenewalDispatch::Held;
    }
    let Some((turn, _)) = admission.as_ref() else {
        return RenewalDispatch::Held;
    };
    let Some(scope) = fx.scope.as_ref() else {
        return RenewalDispatch::Held;
    };
    retire_manual_selection(
        &store,
        &accounts::PendingSelection::Next(request),
        turn,
        scope,
        now,
        #[cfg(test)]
        None,
    )
}

fn reattach_manual_selection(
    cli: Cli,
    home: &Path,
    state_dir: &Path,
    roster_path: &Path,
    selection: accounts::PendingSelection,
    now: i64,
    fx: &UpkeepEffects<'_>,
) -> RenewalDispatch {
    let Some(scope) = fx.scope.clone().filter(|scope| !scope.cancelled()) else {
        return RenewalDispatch::Held;
    };
    let store = accounts::Store::at_for(state_dir, cli, home);
    let owner = match manual_owner_for(cli, &store, roster_path, selection, now, fx) {
        Ok(owner) => owner,
        Err(disposition) => return disposition,
    };
    let (home, state_dir, roster_path) = (
        home.to_path_buf(),
        state_dir.to_path_buf(),
        roster_path.to_path_buf(),
    );
    let guard = fx.renew_guard.clone();
    let work_scope = scope.clone();
    let settlement_scope = scope.clone();
    let work = scope.wrap(move || {
        let mut inner = live_effects();
        inner.renew_guard = &guard;
        let result = with_scope_effects(work_scope, inner, |fx| {
            manual_selection_owned(
                cli,
                &home,
                &state_dir,
                &roster_path,
                (owner, Gesture::Resumed),
                now,
                fx,
            )
        });
        // A non-started result has retired the original owner and signer. A
        // started provider retains them in its permit and publishes its own wake.
        // A selection this platform cannot run was retired before any signer.
        if !settlement_scope.cancelled()
            && matches!(
                result,
                RenewalDispatch::Retired
                    | RenewalDispatch::Recovered
                    | RenewalDispatch::Unavailable
            )
        {
            let _ = store.touch_wake();
            if cli != Cli::Claude {
                let _ = accounts::Store::at(&state_dir).touch_wake();
            }
        }
    });
    match (fx.spawn_renewal)(Box::new(work)) {
        Ok(()) => RenewalDispatch::Started,
        Err(_) => RenewalDispatch::Held,
    }
}

fn retire_manual_selection(
    store: &accounts::Store,
    selection: &accounts::PendingSelection,
    turn: &accounts::SigninTurn,
    scope: &super::account_work::Scope,
    now: i64,
    #[cfg(test)] observe_writer: Option<&dyn Fn(bool)>,
) -> RenewalDispatch {
    if scope.cancelled() {
        return RenewalDispatch::Held;
    }
    if turn.check_store(store).is_err() {
        return RenewalDispatch::Fenced;
    }
    #[cfg(test)]
    if let Some(observe) = observe_writer {
        observe(false);
    }
    let mut disposition = RenewalDispatch::Superseded;
    let published = store.transact_within(&accounts::LOCK_LADDER, now, &mut None, |state| {
        #[cfg(test)]
        if let Some(observe) = observe_writer {
            observe(true);
        }
        if scope.cancelled() {
            disposition = RenewalDispatch::Held;
        } else if turn.check_store(store).is_err() {
            disposition = RenewalDispatch::Fenced;
        } else if state.pending_pin.as_ref() == Some(selection) {
            state.pending_pin = None;
            disposition = RenewalDispatch::Retired;
        }
        Ok(Vec::new())
    });
    if published.is_ok() {
        disposition
    } else {
        RenewalDispatch::Fenced
    }
}

#[cfg(test)]
fn renew_requested_pin(
    home: &Path,
    state_dir: &Path,
    roster_path: &Path,
    request: accounts::PinRequest,
    now: i64,
    renew_guard: &std::sync::Arc<Watched<AutoRenewGuard>>,
    spawn_renewal: &SpawnRenewal<'_>,
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
        None,
        None,
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "requested-pin renewal binds CLI, paths, saved request, clock, admission guard, and spawn seam"
)]
/// The requested-pin renewal as tests drive it: for a click at this desk.
#[cfg(test)]
fn renew_requested_pin_for(
    cli: Cli,
    home: &Path,
    state_dir: &Path,
    roster_path: &Path,
    request: accounts::PinRequest,
    now: i64,
    renew_guard: &std::sync::Arc<Watched<AutoRenewGuard>>,
    spawn_renewal: &SpawnRenewal<'_>,
    scope: Option<&super::account_work::Scope>,
    admission: Option<(accounts::SigninTurn, ManualSelectionOwner)>,
) -> RenewalDispatch {
    renew_requested_pin_at(
        cli,
        (home, state_dir, roster_path),
        request,
        now,
        (renew_guard, spawn_renewal, scope),
        admission,
        true,
    )
}

/// Serve a saved account choice, saying whether the person whose click asked for it is at
/// this desk now (`at_desk`): only then may its sign-in open a window ([`spawn_row_renewal`]).
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedSelection",
        action = "Resolve",
        project = "aterm_agent::harness::accounts_sync::shared_selection_tests::selection_transition"
    )
)]
fn renew_requested_pin_at(
    cli: Cli,
    (home, state_dir, roster_path): (&Path, &Path, &Path),
    request: accounts::PinRequest,
    now: i64,
    (renew_guard, spawn_renewal, scope): (
        &std::sync::Arc<Watched<AutoRenewGuard>>,
        &SpawnRenewal<'_>,
        Option<&super::account_work::Scope>,
    ),
    mut admission: Option<(accounts::SigninTurn, ManualSelectionOwner)>,
    at_desk: bool,
) -> RenewalDispatch {
    if scope.is_some_and(super::account_work::Scope::cancelled) {
        return RenewalDispatch::Held;
    }
    let store = accounts::Store::at_for(state_dir, cli, home);
    if admission
        .as_ref()
        .is_some_and(|(turn, _)| turn.check_store(&store).is_err())
    {
        return RenewalDispatch::Fenced;
    }
    if let Err(why) = store.recover_sync_within(&accounts::LOCK_LADDER, roster_path) {
        return RenewalDispatch::store_refusal(&why.to_string());
    }
    match store.load(now) {
        Ok((state, _))
            if state
                .pending_pin
                .as_ref()
                .and_then(accounts::PendingSelection::account)
                == Some(&request) => {}
        Ok(_) => return RenewalDispatch::Superseded,
        Err(why) => return RenewalDispatch::store_refusal(&why),
    }
    // A manual continuation already owns the machine signer. Recovery must
    // retain that same turn through publication rather than contend with itself.
    recover_interrupted_staging_renewals_checked(
        cli,
        &store,
        roster_path,
        renew_guard,
        now,
        (
            &accounts::LOCK_LADDER,
            &shared_renewal::CheckpointAuthority::live(Some(home), roster_path),
            admission.as_ref().map(|(turn, _)| turn),
        ),
        || {},
    );
    if admission
        .as_ref()
        .is_some_and(|(turn, _)| turn.check_store(&store).is_err())
    {
        return RenewalDispatch::Fenced;
    }
    let mut disposition = RenewalDispatch::Superseded;
    let mut target = None;
    if let Err(why) = store.transact_pool_logins_within(
        &accounts::LOCK_LADDER,
        roster_path,
        now,
        &mut None,
        |raw, st, logins| {
            if st
                .pending_pin
                .as_ref()
                .and_then(accounts::PendingSelection::account)
                != Some(&request)
            {
                return Ok(Vec::new());
            }
            let roster = accounts::bound(raw, logins);
            // A default row is here only while its CLI's default home holds the login, as
            // `Store::complete_requested_pin_at` reads it; with none, the row is signed in.
            let enrolled = accounts::enrolled_in(cli, &roster, home);
            let Some(next_row) = roster.row(request.label()).cloned() else {
                if scope.is_some_and(super::account_work::Scope::cancelled) {
                    disposition = RenewalDispatch::Held;
                    return Ok(Vec::new());
                }
                st.pending_pin = None;
                disposition = RenewalDispatch::Retired;
                return Ok(Vec::new());
            };
            let mut probe = st.clone();
            if accounts::pin(&mut probe, &roster, &enrolled, request.label(), now).is_err() {
                if logins.shared.required(&next_row)?.is_some() {
                    // This exact request predates shared custody (or an older
                    // writer queued it). An ordinary worker cannot service it.
                    if scope.is_some_and(super::account_work::Scope::cancelled) {
                        disposition = RenewalDispatch::Held;
                        return Ok(Vec::new());
                    }
                    st.pending_pin = None;
                    disposition = RenewalDispatch::Retired;
                    return Ok(Vec::new());
                }
                let Some(sign_in) = selection_sign_in(cli, &request, st) else {
                    return Ok(Vec::new());
                };
                target = Some((raw.clone(), next_row, logins.clone(), sign_in));
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
    let Some((file_roster, next_row, logins, sign_in)) = target else {
        return disposition;
    };
    let manual = admission.is_some();
    let result = spawn_row_renewal(
        cli,
        home,
        state_dir,
        roster_path,
        file_roster,
        &logins,
        (next_row, sign_in),
        (Some(request.clone()), at_desk),
        now,
        renew_guard,
        spawn_renewal,
        None,
        scope,
        &mut admission,
    );
    if manual && result == RenewalDispatch::Failed {
        match (admission.as_ref(), scope) {
            (Some((turn, _)), Some(scope)) => retire_manual_selection(
                &store,
                &request.into(),
                turn,
                scope,
                now,
                #[cfg(test)]
                None,
            ),
            _ => RenewalDispatch::Held,
        }
    } else {
        result
    }
}

/// What the automatic ladder of [`spawn_row_renewal`] reaches with ([`UpkeepEffects`]' seams).
struct LadderCtx<'a> {
    scope: Option<super::account_work::Scope>,
    /// The roster whose current row must still admit an observation.
    roster_path: PathBuf,
    find_browser: &'a (dyn Fn(&Path) -> bool + Sync),
    vault_sync: fn(&Path, Option<&super::account_work::Scope>) -> super::sign_in_ladder::Vault,
    spawn_vault: &'a SpawnRenewal<'a>,
    mailbox: &'a LadderMailbox,
    log: SignInLog,
    run_attempt: Option<RunAttempt>,
    shared: shared_renewal::Effects<'a>,
    /// How many `[harness] hosts` the config beside the roster names.
    hosts: usize,
    /// The vault route's read ([`UpkeepEffects::tokens_here`]).
    tokens_here: TokensRead,
    /// The pass's witnesses of which logins are live, where the pass read some
    /// ([`Self::asking`]): the last ask under the sign-in lock reads those
    /// ([`login_live_again`]), and its own where there are none.
    live: Option<&'a super::login_adoption::Pass>,
}

impl<'a> LadderCtx<'a> {
    fn of(fx: &UpkeepEffects<'a>, roster_path: &Path) -> Self {
        let config = config_beside(roster_path);
        Self {
            scope: fx.scope.clone(),
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
            tokens_here: fx.tokens_here,
            live: None,
        }
    }

    /// This, asking the pass's witnesses `live`.
    fn asking(self, live: &'a super::login_adoption::Pass) -> Self {
        Self {
            live: Some(live),
            ..self
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
    /// The vault route found the row's year token in use here ([`ladder_vault_token`]):
    /// recorded, logged, and no route runs.
    OnToken,
}

/// A read-only cache context, never authority to use a vault or its credentials.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VaultInput {
    Missing,
    Unreadable,
    Read(super::sign_in_ladder::VaultContext),
}

/// Each `key = value` line of the vault's `state.toml` as `(table, key, line)`, the table `""`
/// above the first `[table]` header: the client writes its receipts in tables
/// (`[network_refresh]`, `[network_materialization]`, …) whose keys repeat top-level names.
fn vault_state_lines(text: &str) -> impl Iterator<Item = (&str, &str, &str)> {
    let mut table = "";
    text.lines().map(str::trim).filter_map(move |line| {
        if let Some(name) = line
            .strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
        {
            table = name.trim();
            return None;
        }
        let (key, _) = line.split_once('=')?;
        Some((table, key.trim(), line))
    })
}

/// The top-level fields of the vault's `state.toml` that say where this Mac stands with the
/// vault: its membership (none, pending, in, revoked or plain) and which vault it is. A vault
/// refresh's cached result ([`vault_input`]) and a row's backoff ([`ladder_inputs`]) are kept by
/// them. Not the version applied here (`seq`, `digest`): it moves only after this Mac's
/// subscriber or a sync has applied that version, about once a minute or two on the lab's vault,
/// most of them writing nothing here, so a refresh it started could only find that version
/// applied. What a version does bring a route is in the mailbox entries it reads, which
/// [`ladder_inputs`] digests by what they hold.
fn vault_membership_fields(text: &str) -> impl Iterator<Item = &str> {
    vault_state_lines(text).filter_map(|(table, key, line)| {
        (table.is_empty() && matches!(key, "membership" | "source_identity")).then_some(line)
    })
}

/// How the subscriber's last network refresh of a URL vault ended (`[network_refresh]`'s
/// `outcome`, succeeded or failed): rewritten at each pass, it changes only when the vault stops
/// or starts answering this Mac, so a row's refresh that failed while the vault did not answer is
/// asked again once it does. A path vault keeps no such record.
fn vault_reach_field(text: &str) -> impl Iterator<Item = &str> {
    vault_state_lines(text).filter_map(|(table, key, line)| {
        (table == "network_refresh" && key == "outcome").then_some(line)
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

/// Bind cached refresh observations to bounded source contents, this Mac's standing with the
/// vault ([`vault_membership_fields`]) and whether the vault answers it ([`vault_reach_field`]).
/// Neither heartbeats nor a newly applied version change this digest. Permissions do, so
/// repairing a refused source ends its cached failure. Separate file reads are observations, not
/// a filesystem transaction.
fn vault_input(home: &Path) -> VaultInput {
    fn field(hash: &mut aterm_digest::Sha256, bytes: &[u8]) {
        hash.update((bytes.len() as u64).to_be_bytes());
        hash.update(bytes);
    }
    fn mode(hash: &mut aterm_digest::Sha256, path: &Path) -> Option<()> {
        let metadata = trace::fs::symlink_metadata(path).ok()?;
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
            for line in vault_membership_fields(&state).chain(vault_reach_field(&state)) {
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
/// ([`super::sign_in_ladder::Ladder::held`]): the names the vault holds here and its mailbox
/// entries (an in-process digest of what they hold, [`crate::vault_entry::digest`]); the vault's
/// membership and identity ([`vault_membership_fields`], never
/// the version it applied, nor that file's own time, which the vault's subscriber rewrites about
/// once a minute only to say when it last heard the vault); the vault's `source.toml`; the
/// mailbox configurations; and the store's browser profiles. Mail arriving in a mailbox folder is
/// not among them, since a route's own run brings one, and neither are the roster's own files,
/// which a sync rewrites.
fn ladder_inputs(home: &Path, store: &accounts::Store) -> String {
    let stamp = |path: &Path| {
        trace::fs::symlink_metadata(path)
            .ok()
            .map(|meta| (meta.len(), meta.modified().ok()))
    };
    let mut line = String::new();
    // Every name the vault holds here (value-free), then what its mailbox entries hold.
    for name in crate::vault_entry::names(home) {
        line.push_str(&name);
        line.push(';');
    }
    let entries = super::headless_oauth::vault_mailbox_entries(home);
    let names = entries.iter().map(String::as_str).collect::<Vec<_>>();
    line.push_str(&aterm_codec::hex::encode_lower(
        &crate::vault_entry::digest(home, &names, super::headless_oauth::MAILBOX_TOML_MAX_BYTES),
    ));
    line.push(';');
    let vault = home.join(".config/aterm/secrets");
    if let Ok(text) = read_vault_state_text(&vault.join("state.toml")) {
        for kept in vault_membership_fields(&text) {
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
    // The lab registry's mailboxes, which a sign-in reads before the files (step A9).
    line.push_str(&aterm_codec::hex::encode_lower(
        &super::alab_roster::mail_digest(),
    ));
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
    raw: &accounts::Row,
    row: &accounts::Row,
    ctx: &LadderCtx<'_>,
) -> super::sign_in_ladder::Read {
    use super::sign_in_ladder::{Profile, Read};
    let email = row.email.as_deref();
    let profile = if !super::headless_oauth::has_browser_profile(&store.dir, &row.label, email) {
        Profile::None
    } else if claude_browser_profile_ready(store, logins, raw) {
        Profile::Ready
    } else {
        Profile::WrongAccount
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
/// its cheap probes. Its waits are judged on the guard's clock ([`MachineReads::clock`]).
fn ladder_route(
    home: &Path,
    store: &accounts::Store,
    row: &accounts::Row,
    now: i64,
    renew_guard: &std::sync::Arc<Watched<AutoRenewGuard>>,
    ctx: &LadderCtx<'_>,
) -> LadderStep {
    let clock = guard_reads(renew_guard).clock;
    ladder_route_with_clock(home, store, row, now, renew_guard, ctx, &clock)
}

/// [`ladder_route`] with an injected monotonic clock for expiry checks.
fn ladder_route_with_clock(
    home: &Path,
    store: &accounts::Store,
    row: &accounts::Row,
    now: i64,
    renew_guard: &std::sync::Arc<Watched<AutoRenewGuard>>,
    ctx: &LadderCtx<'_>,
    clock: &dyn Fn() -> Instant,
) -> LadderStep {
    ladder_route_in_context(home, store, (row, None), now, renew_guard, ctx, clock)
}

/// The stops that stand on `raw`'s record ([`page_holds`]).
#[derive(Clone, Copy)]
struct Stops<'a> {
    /// When a bot protection last stopped a sign-in of the row's account
    /// ([`accounts::check_stopped_at`]: its [`accounts::CheckStop`], whatever its workflow, or a
    /// current attempt that waits for a person), with no signed-in `accounts browser` window on
    /// record since.
    check: Option<i64>,
    /// Its current workflow's last sign-in, the upkeep's or one run by hand, where it ended
    /// `blocked` at a stop no bot protection made (a window a person closed, another account's
    /// consent, no desktop), with no signed-in `accounts browser` window on record since.
    page: Option<&'a accounts::SignInAttempt>,
}

/// What stands on `raw`'s record against its sign-in ([`Stops`]). A login made for the row clears
/// both records, and with them the stops.
fn standing_stops<'a>(
    store: &accounts::Store,
    raw: &accounts::Row,
    logins: &'a accounts::Logins,
) -> Stops<'a> {
    let no_window_since = |at: i64| workflow_browser_window_token(store, raw, logins, at).is_none();
    Stops {
        check: accounts::check_stopped_at(raw, logins)
            .max(ledger_check_at(store, raw, logins))
            .filter(|at| no_window_since(*at)),
        page: accounts::current_sign_in_attempt(raw, logins)
            .filter(|attempt| {
                attempt.outcome == accounts::AttemptOutcome::Blocked
                    && !attempt.waits_for_a_person()
            })
            .filter(|attempt| no_window_since(attempt.tried_at)),
    }
}

/// What stands against `raw`'s sign-in by the ladder's route ([`standing_stops`]), `prepared`
/// for a root decision's preparation: a request, not a retry, which passes a stop no bot
/// protection made. A bot protection's stop holds a preparation as it holds every route: no
/// grant, generation or configuration of the issuer's sends the row back to the page that
/// carried one (`docs/DESIGN-claude-sign-in-sitting-2026-11-12.md` §3), only what ends the
/// check does (a login made for the row, a signed-in `accounts browser` window).
fn route_stops<'a>(
    store: &accounts::Store,
    raw: &accounts::Row,
    logins: &'a accounts::Logins,
    prepared: bool,
) -> Stops<'a> {
    let stops = standing_stops(store, raw, logins);
    if prepared {
        Stops {
            check: stops.check,
            page: None,
        }
    } else {
        stops
    }
}

/// Whether `stops`, what stands on `raw`'s record ([`standing_stops`]), still hold its background
/// sign-in. A bot protection's stop holds on no clock: nothing tries a sign-in again into a check,
/// a block page or a human check with nobody there
/// (`docs/DESIGN-claude-sign-in-sitting-2026-11-12.md` §3), and only a person's own renewal, a
/// signed-in `accounts browser` window or a bound login ends it; the row's token in use here
/// ([`accounts::on_token_here`]) lifts it for the vault route alone, while it serves. Any other
/// stop holds for [`PERSON_BACKOFF`] after the pass that met it. A real change of address could
/// change a page's answer sooner, but nothing here can tell one from the rest of what counts as a
/// move ([`super::sign_in_ladder::Ladder::moved`]). This holds across what the ladder's own wait
/// does not: a restart forgets that wait, and a vault entry changing ends it as a new input,
/// though a check judges neither.
fn page_holds(stops: Stops<'_>, raw: &accounts::Row, state: &accounts::State, now: i64) -> bool {
    (stops.check.is_some() && accounts::on_token_here(raw, state, now).is_none())
        || stops
            .page
            .is_some_and(|attempt| now.saturating_sub(attempt.tried_at) < PERSON_BACKOFF_SECS)
}

/// Whether `raw`'s next sign-in is a person's at this Mac, whichever CLI's pool `store` keeps,
/// so that no door with nobody there starts one (the upkeep, a saved pin request or Next
/// selection a tick resumed, an agent's or another Mac's `accounts renew` or `accounts token`,
/// a worker's launch): a bot protection stopped a sign-in of its account ([`check_standing_at`],
/// [`page_holds`]' hold with no clock), or its renewal ended where only a person goes on and
/// recorded that person's renewal as its task ([`accounts::SignInAction::Renew`]), with no
/// signed-in `accounts browser` window since. The row's token in use here lifts neither: from a
/// check only the ladder's vault route goes on, which loads no page, and every route these doors
/// run loads one. A login made for the row ends both.
pub(crate) fn left_to_a_person(
    store: &accounts::Store,
    raw: &accounts::Row,
    logins: &accounts::Logins,
) -> bool {
    check_standing_at(store, raw, logins).is_some()
        || accounts::current_sign_in_task(raw, logins).is_some_and(|task| {
            task.action == accounts::SignInAction::Renew
                && workflow_browser_window_token(store, raw, logins, task.at).is_none()
        })
}

/// When a bot protection last stopped a sign-in of `raw`'s account on this Mac, of any CLI's pool,
/// where that stands: its account's [`accounts::CheckStop`] (or a current attempt that waits for
/// a person, [`accounts::check_stopped_at`]), or another CLI's row's ledger line of that stop
/// ([`ledger_check_at`]), with no signed-in `accounts browser` window on record since. Nothing
/// with nobody at this Mac signs the row in while it stands, on any clock ([`left_to_a_person`]);
/// only a login made for the row or such a window ends it.
pub(crate) fn check_standing_at(
    store: &accounts::Store,
    raw: &accounts::Row,
    logins: &accounts::Logins,
) -> Option<i64> {
    standing_stops(store, raw, logins).check
}

/// When this Mac's ledger keeps a renewal of another CLI's row `raw` that a bot protection
/// stopped (a `SignIn` line of the row's workflow whose word is `bot-check`, as
/// [`super::login_drive::Drove::why`] writes it) with no login made for the row since (a
/// `Renewed` or `SignedIn` line of its label or workflow): the second the newest such line
/// ended. The line is the record of that stop as the account's [`accounts::CheckStop`] is, and
/// holds as it does, on no clock: an earlier build wrote only it and a `login` task for a
/// vendor's bot check, and this build writes it beside the check. A later stop no bot protection
/// made and another account signed in leave it standing; [`standing_stops`] ends it at a
/// signed-in `accounts browser` window since. `None` for a Claude row, whose sign-ins record
/// their stops on the account ([`accounts::note_sign_in_stop`]), and where the ledger cannot be
/// read.
fn ledger_check_at(
    store: &accounts::Store,
    raw: &accounts::Row,
    logins: &accounts::Logins,
) -> Option<i64> {
    if store.cli() == Cli::Claude {
        return None;
    }
    let events = store.tail(accounts::LEDGER_KEEP_ROWS).ok()?;
    let mut checked = None;
    for event in &events {
        let workflow = accounts::workflow_event_matches(raw, logins, event);
        match event.kind {
            accounts::Kind::Renewed | accounts::Kind::SignedIn
                if workflow || event.account == raw.label =>
            {
                checked = None;
            }
            accounts::Kind::SignIn if workflow && event.why.ends_with(" bot-check") => {
                checked = Some(event.t);
            }
            _ => {}
        }
    }
    checked
}

/// What holds another CLI's row for a person at this Mac ([`person_hold`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PersonHold {
    /// Its own record ([`left_to_a_person`]).
    Row,
    /// Another row of its pool and realm whose sign-in a bot protection stopped on this Mac
    /// ([`check_standing_at`]), by its label.
    Pool(String),
}

/// What holds `raw`, a row of `raw_roster` (the pool `store` keeps), for a person at this Mac,
/// so that no door with nobody there loads its sign-in page: its own record
/// ([`left_to_a_person`]), or, for another CLI's row, a bot protection's stop of another row of
/// the same pool and realm that stands ([`check_standing_at`]). A vendor's bot check answers this
/// Mac's headless browser on the vendor's sign-in host, not one account (measured 2026-11-04:
/// every automatic sign-in on record of the Codex rows of two Macs, 8 of 8, met Cloudflare's
/// check), so the next row's page meets it too; the pool waits until a person answers each
/// stop that stands, by a login made for its row or a signed-in `accounts browser` window
/// (`docs/DESIGN-claude-sign-in-sitting-2026-11-12.md` §3, and the owner's ruling of 2026-11-04
/// against signing every account in from one machine). Google's stop pages on agy's native route
/// and Kimi Code's Google federation ("unusual traffic", "Verify it's you", "This browser or app
/// may not be secure") hold the pool alike: each answers this Mac's sign-in from its browser, and
/// the next account's sign-in from the same browser goes back to the page that stopped this one.
/// A Claude row's stop holds that row's account alone, as its ladder's routes are its account's
/// ([`page_holds`]).
pub(crate) fn person_hold(
    store: &accounts::Store,
    raw_roster: &Roster,
    raw: &accounts::Row,
    logins: &accounts::Logins,
) -> Option<PersonHold> {
    if left_to_a_person(store, raw, logins) {
        return Some(PersonHold::Row);
    }
    if store.cli() == Cli::Claude {
        return None;
    }
    raw_roster
        .rows
        .iter()
        .filter(|other| other.label != raw.label && other.vendor.region == raw.vendor.region)
        .find(|other| check_standing_at(store, other, logins).is_some())
        .map(|other| PersonHold::Pool(other.label.clone()))
}

fn ladder_route_in_context(
    home: &Path,
    store: &accounts::Store,
    (row, preparation): (&accounts::Row, Option<&super::cli::shared_renewal::Grant>),
    now: i64,
    renew_guard: &std::sync::Arc<Watched<AutoRenewGuard>>,
    ctx: &LadderCtx<'_>,
    clock: &dyn Fn() -> Instant,
) -> LadderStep {
    if ctx
        .scope
        .as_ref()
        .is_some_and(super::account_work::Scope::cancelled)
    {
        return LadderStep::Held;
    }
    use super::sign_in_ladder::{Vault, VaultTurn};
    fn lock(
        guard: &std::sync::Arc<Watched<AutoRenewGuard>>,
    ) -> trace::WatchedGuard<'_, AutoRenewGuard> {
        guard
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    // Another row's route observation can insert a binding and advance the global
    // counter. Capture this probe separately; its publication still checks the parent.
    let Ok(mut records) = store.read_account_records(Some(&ctx.roster_path), now) else {
        return LadderStep::Held;
    };
    let Some(raw) = records.raw_roster.row(&row.label).cloned() else {
        return LadderStep::Held;
    };
    if records.roster.row(&row.label) != Some(row) {
        return LadderStep::Held;
    }
    match prepare_retry_workflow(store, &ctx.roster_path, &raw, row, &records.logins, now) {
        Ok(Some(current)) => records = current,
        Ok(None) => {}
        Err(_) => return LadderStep::Held,
    }
    let logins = &records.logins;
    reconcile_workflow_retries(
        renew_guard,
        store,
        Cli::Claude,
        &ctx.roster_path,
        (&raw, row, logins),
        now,
    );
    let mut read = ladder_read(home, store, logins, &raw, row, ctx);
    let observed_vault = if read.route().is_none() {
        vault_input(home)
    } else {
        VaultInput::Missing
    };
    // A sign-in of the row's account a Mac of the fleet recorded `blocked` within a day holds
    // every route that opens a browser, after the vault route ([`accounts::fleet_blocked`]). It
    // is an input, so its lapse ends the wait it set.
    let fleet = accounts::fleet_blocked(&records.state, &row.label, now);
    let inputs_of = |read: &super::sign_in_ladder::Read, observed: VaultInput| {
        let mut inputs = row_retry_inputs(
            row,
            &format!(
                "{}|{observed:?}|{}",
                read.words(),
                ladder_inputs(home, store)
            ),
        );
        // A new root decision wakes the route probe without changing the
        // previously admitted preparation or its attempt timestamp.
        if let Some(grant) = preparation {
            inputs.push_str(&format!("|preparation={grant:?}"));
        }
        if let Some(hold) = fleet {
            inputs.push_str(&format!("|fleet={}", hold.until));
        }
        inputs
    };
    let mut inputs = inputs_of(&read, observed_vault);
    let at = clock();
    let stops = route_stops(store, &raw, logins, preparation.is_some());
    // A bot protection's stop that the row's token in use here lifted ([`page_holds`]) lets the
    // vault route record that token and nothing else: no route that loads a page goes on from it.
    let only_the_vault =
        stops.check.is_some() && accounts::on_token_here(&raw, &records.state, now).is_some();
    // A bot protection this process met and could not record yet holds the row until it is on
    // record, which this pass tries again ([`hold_for_a_person`]), for every route, a
    // preparation's included.
    let pending = lock(renew_guard).unrecorded_checks.get(&row.label).cloned();
    if let Some(pending) = pending {
        if write_check_stop(store, &pending) != CheckWrite::Busy {
            lock(renew_guard).unrecorded_checks.remove(&row.label);
        }
        return LadderStep::Held;
    }
    {
        let mut guard = lock(renew_guard);
        if page_holds(stops, &raw, &records.state, now) {
            return LadderStep::Held;
        }
        if preparation.is_none() {
            guard.ladder.page_ended(&row.label);
        }
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
                        let Some(seen) = start_vault_refresh(home, key, at, renew_guard, ctx)
                        else {
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
            read = ladder_read(home, store, logins, &raw, row, ctx);
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
    if read.holds_mailbox()
        && let Some(line) = mailbox_need(home, &row.label, true)
    {
        (ctx.log)(&format!(
            "{}: its mailbox need: {line}",
            super::account_names::logged_row(
                Cli::Claude,
                row,
                super::alab_roster::current().as_deref()
            )
        ));
    }
    // The vault route comes before any route that opens a browser or a window, and before a
    // row with none is recorded as such. A shared row's admitted preparation is its authority's
    // to run or retire, so it never stops here.
    if preparation.is_none()
        && let Some(step) = ladder_vault_token(
            home,
            store,
            (&records.roster, &raw, row, logins),
            now,
            renew_guard,
            ctx,
            (&read, at),
        )
    {
        return step;
    }
    if only_the_vault {
        return LadderStep::Held;
    }
    if let Some(route) = read.route()
        && fleet.is_none()
    {
        return LadderStep::Go {
            route,
            read,
            inputs,
        };
    }
    // A fleet's hold is recorded as no route, never `blocked`, so a hold never sets off another
    // across the fleet. Its record names the hold's end and no time relative to the clock, so a
    // pass the same hold holds again records nothing new; the log line says how long ago and
    // how long more. Where this Mac's own last attempt is the block (a stop no bot protection
    // made, once its own six hours are over) and no signed-in `accounts browser` window answered
    // it since ([`standing_stops`]' rule), that block is this Mac's `ai/logins` row's, the
    // evidence every Mac's hold reads, this one's included: it is kept until the hold lapses,
    // and nothing new is recorded over it.
    let routes = read.words();
    let (words, said) = match &fleet {
        Some(hold) => (
            format!("{}; {routes}", super::sign_in_ladder::fleet_words(hold)),
            format!("{}; {routes}", super::sign_in_ladder::fleet_said(hold, now)),
        ),
        None => (routes.clone(), routes),
    };
    let keeps_its_block = fleet.is_some()
        && logins
            .get(&raw.label)
            .and_then(|binding| binding.sign_in_attempt.as_ref())
            .is_some_and(|attempt| {
                attempt.outcome == accounts::AttemptOutcome::Blocked
                    && workflow_browser_window_token(store, &raw, logins, attempt.tried_at)
                        .is_none()
            });
    if !keeps_its_block
        && !note_attempt(
            store,
            (&ctx.roster_path, row, logins),
            (accounts::AttemptOutcome::NoRoute, None),
            &words,
            now,
        )
    {
        return LadderStep::Held;
    }
    let logged = super::account_names::logged_row(
        Cli::Claude,
        row,
        super::alab_roster::current().as_deref(),
    );
    if fleet.is_none()
        && read.wants_mailbox()
        && let Some(line) = mailbox_need(home, &row.label, false)
    {
        (ctx.log)(&format!(
            "{logged}: the lab is asked for its mailbox password: {line}"
        ));
    }
    let wait = {
        let mut guard = lock(renew_guard);
        let wait = guard.ladder.failed(&row.label, &inputs, at);
        guard.ladder_owners.insert(
            row.label.clone(),
            LoginOwner::new(store, Cli::Claude, &row.label, now)
                .observing(logins)
                .for_row(row)
                .for_workflow(&raw, logins),
        );
        wait
    };
    (ctx.log)(&format!(
        "{} (tried again in {} or when one of them changes)",
        super::sign_in_ladder::log_line(&logged, accounts::AttemptOutcome::NoRoute.as_str(), &said),
        super::upgrade::span(wait.as_secs())
    ));
    LadderStep::NoRoute
}

/// Starts the vault refresh [`super::sign_in_ladder::Ladder::vault_turn`] said is due for the
/// vault context `key` at `at`: one bounded `aterm secrets sync` for `home`
/// ([`LadderCtx::vault_sync`]) off this thread ([`LadderCtx::spawn_vault`]), whose end the ladder
/// records ([`super::sign_in_ladder::Ladder::vault_done`]). What it found where it has ended
/// already; `None` while it runs.
fn start_vault_refresh(
    home: &Path,
    key: super::sign_in_ladder::VaultContext,
    at: Instant,
    renew_guard: &std::sync::Arc<Watched<AutoRenewGuard>>,
    ctx: &LadderCtx<'_>,
) -> Option<super::sign_in_ladder::Vault> {
    let guard = std::sync::Arc::clone(renew_guard);
    let mut claim = VaultWorkClaim {
        guard: guard.clone(),
        context: key,
        completed: false,
    };
    let (sync, home_owned) = (ctx.vault_sync, home.to_path_buf());
    let scope = ctx.scope.clone();
    let _ = (ctx.spawn_vault)(Box::new(move || {
        if scope
            .as_ref()
            .is_some_and(super::account_work::Scope::cancelled)
        {
            return;
        }
        let seen = sync(&home_owned, scope.as_ref());
        if scope
            .as_ref()
            .is_some_and(super::account_work::Scope::cancelled)
        {
            return;
        }
        guard
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .ladder
            .vault_done(seen);
        claim.complete();
    }));
    renew_guard
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .ladder
        .vault_result(key, at)
}

/// THE VAULT ROUTE, BEFORE ANY BROWSER: where the lab keys a token entry for `row` (`raw` as the
/// roster file has it, of the bound `roster`), before a ready route opens a browser or a window for
/// it and before a row with none is recorded as such, read which rows run on their vault token here
/// ([`LadderCtx::tokens_here`]: value-free, this Mac's applied vault only). Where `row` is not one
/// of them and its sign-in workflow has not had one yet, refresh the vault first (the ladder's own
/// bounded `aterm secrets sync`, on its own clock and off this thread: [`Ladder::vault_turn`]),
/// since another Mac may have minted the row's token in aterm's window, or verified one, since this
/// one last applied the vault, and read again: once per workflow (a refresh that did not read the
/// whole vault, [`super::sign_in_ladder::Vault::read_whole`], does not count, so the vault's clock
/// lets the next pass try again), never once per pass, and never in no vault or one this Mac cannot
/// read. Off macOS no launch starts on a token, so this reads nothing and refreshes nothing. Where
/// the token is in use here, the reading is kept for rotation
/// ([`accounts::Store::note_tokens_here`], waking the sessions that wait on a move), the attempt is
/// recorded [`accounts::AttemptOutcome::OnToken`] by the `vault` route
/// ([`super::sign_in_ladder::Route::Vault`]), one `aterm.log` line says so
/// ([`super::sign_in_ladder::on_token_words`]), and nothing opens ([`LadderStep::OnToken`]).
/// [`LadderStep::Held`] while the refresh runs. `None`: the ladder goes on as before, as it does
/// for a token another Mac minted at a person's terminal (or by a meta that names no route) and
/// nobody verified, which serves only that Mac, and for a row whose login is not made here where
/// this Mac's claims do not cover its account ([`super::claims::covers`]), which no reading names
/// whatever its token ([`super::accounts_tokens::rows_on_token_of`]): no launch starts on such a
/// row here until its sign-in makes its login. Where they cover it, its fleet token runs it here
/// (FL-135, [`accounts::LoginHere::OnToken`]). Nothing here asks another Mac anything: the refresh
/// reads the shared vault.
///
/// [`Ladder::vault_turn`]: super::sign_in_ladder::Ladder::vault_turn
fn ladder_vault_token(
    home: &Path,
    store: &accounts::Store,
    (roster, raw, row, logins): (&Roster, &accounts::Row, &accounts::Row, &accounts::Logins),
    now: i64,
    renew_guard: &std::sync::Arc<Watched<AutoRenewGuard>>,
    ctx: &LadderCtx<'_>,
    (read, at): (&super::sign_in_ladder::Read, Instant),
) -> Option<LadderStep> {
    use super::sign_in_ladder::{Route, VaultTurn};
    fn lock(
        guard: &std::sync::Arc<Watched<AutoRenewGuard>>,
    ) -> trace::WatchedGuard<'_, AutoRenewGuard> {
        guard
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    // No launch here starts on a token off macOS, so no refresh could find one serving.
    if !super::token_route::LAUNCHES_ON_TOKEN {
        return None;
    }
    accounts::TokenEntry::of_row(row)?;
    let serving = |reading: Option<&BTreeMap<String, u64>>| {
        reading.and_then(|rows| rows.get(&row.label).copied())
    };
    let mut reading = (ctx.tokens_here)(home, store, roster, now.into());
    let mut read = read.clone();
    if serving(reading.as_ref()).is_none()
        && let Some(workflow) = accounts::current_workflow(raw, logins)
        && !lock(renew_guard)
            .ladder
            .token_refreshed(&row.label, workflow)
    {
        let VaultInput::Read(key) = vault_input(home) else {
            return None;
        };
        let turn = lock(renew_guard).ladder.vault_turn(Some(key), at);
        let seen = match turn {
            VaultTurn::Seen(seen) => seen,
            VaultTurn::Running => return Some(LadderStep::Held),
            VaultTurn::Unavailable => return None,
            VaultTurn::Due => match start_vault_refresh(home, key, at, renew_guard, ctx) {
                Some(seen) => seen,
                None => return Some(LadderStep::Held),
            },
        };
        // Only a refresh that read the whole vault uses the workflow's one: a failed one (off the
        // LAN, say) leaves it for the next pass the vault's own clock lets refresh.
        if seen.read_whole() {
            lock(renew_guard)
                .ladder
                .note_token_refreshed(&row.label, workflow);
        }
        reading = (ctx.tokens_here)(home, store, roster, now.into());
        read.vault.get_or_insert(seen);
    }
    let until = serving(reading.as_ref())?;
    if store
        .note_tokens_here(reading.as_ref(), now)
        .unwrap_or(false)
    {
        let _ = store.touch_wake();
    }
    if !note_attempt(
        store,
        (&ctx.roster_path, row, logins),
        (accounts::AttemptOutcome::OnToken, None),
        Route::Vault.as_str(),
        now,
    ) {
        return Some(LadderStep::Held);
    }
    (ctx.log)(&super::sign_in_ladder::log_line(
        &super::account_names::logged_row(
            Cli::Claude,
            row,
            super::alab_roster::current().as_deref(),
        ),
        accounts::AttemptOutcome::OnToken.as_str(),
        &super::sign_in_ladder::on_token_words(until, &read),
    ));
    Some(LadderStep::OnToken)
}

/// Ask the lab for `label`'s mailbox password, or record that the row's mailbox is now there
/// (`met`), through this Mac's need ledger (docs/DESIGN-credential-requests-2026-10-27.md
/// §12.3): the ledger posts the request once this Mac is a Commons Writer and asks a person only
/// when no Mac the lab reaches holds it. The need's line, for the log; `None` when nothing
/// moved (`met` with no need on file) or the ledger cannot be used here, as on every system but
/// unix, where no vault runs and so no ledger is kept.
pub(super) fn mailbox_need(home: &Path, label: &str, met: bool) -> Option<String> {
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

/// THE LAB'S ASK FOR A ROW'S MAILBOX, ENDED ONCE THE MAILBOX HOLDS, whatever signed the row in:
/// each Claude Code roster row with a `mailbox:<label>` need of the sign-in's that has not ended
/// has its mailbox read through the ladder's own reader ([`UpkeepEffects::mailbox`]), and where
/// that mailbox holds ([`super::sign_in_ladder::Mailbox::holds`]) the need is met
/// ([`mailbox_need`]) and its line logged, as the ladder does. The ladder reads a row only while
/// it renews it, so without this a row signed in again another way would keep a person asked
/// for a password nothing lacks. Claude Code's ladder raises these needs under its rows' labels
/// (a person may file one by hand under the sign-in's purpose too), so only its roster is read.
/// A pass with no such need reads the need ledger once and nothing else; a row's mailbox is read
/// only while its need is open, and a row whose mailbox does not hold keeps its need for its
/// next renewal. Off unix, where no ledger is kept, it does nothing.
fn settle_mailbox_needs(
    home: &Path,
    state_dir: &Path,
    roster_path: &Path,
    now: impl Into<trace::Now>,
    fx: &UpkeepEffects<'_>,
) {
    let now: trace::Now = now.into();
    #[cfg(unix)]
    {
        use aterm_secrets::need::{self, Purpose, Want};
        let Ok(needs) = need::peek(&aterm_secrets::client::Paths::for_home(home)) else {
            return;
        };
        let open: BTreeSet<String> = needs
            .into_iter()
            .filter(|need| need.purpose == Purpose::HarnessSignIn && !need.state.ended())
            .filter_map(|need| match need.want {
                Want::Mailbox(label) => Some(label),
                _ => None,
            })
            .collect();
        if open.is_empty() {
            return;
        }
        let store = accounts::Store::at(state_dir);
        let Ok(roster) = store.read_roster(Some(roster_path), now) else {
            return;
        };
        for row in roster.rows.iter().filter(|row| open.contains(&row.label)) {
            if fx
                .scope
                .as_ref()
                .is_some_and(super::account_work::Scope::cancelled)
            {
                return;
            }
            if (fx.mailbox)(home, &store, row).0.holds()
                && let Some(line) = mailbox_need(home, &row.label, true)
            {
                (fx.log)(&format!("{}: its mailbox need: {line}", row.label));
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (home, state_dir, roster_path, now, fx);
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
    let workflow = accounts::captured_workflow(observed, &row.label)
        .ok_or("automatic sign-in has no admitted workflow custody")?;
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
            if !raw
                .row(&row.label)
                .is_some_and(|raw| accounts::workflow_token_current(raw, current, workflow))
            {
                return Err("automatic sign-in workflow changed; nothing recorded".into());
            }
            accounts::admit_workflow(current, &row.label)?;
            Ok(edit(current))
        },
    )
}

/// Records an attempt only while its row and captured parent still match. A duplicate
/// outcome is admitted without another history entry, so its retry clock can still advance.
/// A [`accounts::AttemptOutcome::Blocked`] attempt keeps the page that stopped it (`stop`,
/// [`accounts::note_sign_in_stop`]; `by_hand` where a person at this desk started the run), a
/// bot protection's with the account's [`accounts::CheckStop`], and where only a person goes on
/// from a bot protection ([`super::open_url::Met::takes_a_person`]) the same write records that
/// person's renewal as the row's task ([`accounts::SignInAction::Renew`]), as a renewal run by hand
/// does. A stop no bot protection made (no desktop, a window a person closed) records no task:
/// it holds for [`PERSON_BACKOFF`] and is tried again after. Any other outcome short of a login
/// keeps the check that stands ([`accounts::keep_check`]).
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
    (outcome, stop): (
        accounts::AttemptOutcome,
        Option<(&super::open_url::Met, bool)>,
    ),
    routes: &str,
    now: i64,
) -> bool {
    let row = captured.1;
    with_current_attempt(store, captured, now, |current| match stop {
        Some((met, by_hand)) if outcome == accounts::AttemptOutcome::Blocked => {
            let events = accounts::note_sign_in_stop(current, row, by_hand, met.word, routes, now);
            if met.takes_a_person() && met.word.is_bot_protection() {
                accounts::note_sign_in_task(
                    current,
                    &row.label,
                    accounts::SignInAction::Renew,
                    now,
                );
            }
            events
        }
        _ => {
            if outcome != accounts::AttemptOutcome::SignedIn {
                accounts::keep_check(current, row);
            }
            accounts::note_sign_in_attempt(current, &row.label, outcome, routes, now)
        }
    })
    .is_ok()
}

/// A bot protection's stop met and not recorded yet ([`write_check_stop`], and in this process
/// [`AutoRenewGuard::unrecorded_checks`]).
#[derive(Clone, Debug, Hash)]
pub(super) struct PendingCheck {
    /// The roster the run's row came from.
    pub(super) roster_path: PathBuf,
    /// The row the run signed in, which names the account the check stopped.
    pub(super) row: accounts::Row,
    /// The row's renewal generation when the run started: a login made since ends the check.
    pub(super) generation: Option<u64>,
    /// When the check stopped it (Unix seconds).
    pub(super) at: i64,
}

/// What [`write_check_stop`] came to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CheckWrite {
    /// The account's [`accounts::CheckStop`] is on record.
    Recorded,
    /// It is no longer the row's to hold: the row left the roster, names another account, or
    /// a login was made for it since the run started.
    Moot,
    /// The records could not be written (`accounts.lock` stayed held, a file could not be read).
    Busy,
}

/// Records `check`'s bot protection as its account's [`accounts::CheckStop`] under no fence but
/// the account's own: the row under its label still names that account in the same realm, or
/// one of them names no address ([`accounts::checked_account`]), and no login was made for it
/// since its run started. A run's own record (the upkeep's [`note_attempt`], a vendor renewal's
/// in `cli::renew_one_for`) is fenced to the run's row, parent and workflow, and refuses a run
/// another write overtook; a check met is a fact of this Mac and that account whatever else
/// moved meanwhile, so every door that met one writes it alone where that record did not land.
pub(super) fn write_check_stop(store: &accounts::Store, check: &PendingCheck) -> CheckWrite {
    let moot = std::cell::Cell::new(false);
    let written = store.transact_pool_logins_within(
        &accounts::LOCK_LADDER,
        &check.roster_path,
        check.at,
        &mut None,
        |raw, _, logins| {
            let Some(row) = raw.row(&check.row.label).filter(|row| {
                accounts::checked_account(&accounts::check_of(&check.row, check.at), row)
            }) else {
                moot.set(true);
                return Err("the row names another account".into());
            };
            // Only a login made for the row since its run started ends the check: its success
            // is of a generation after the binding's then (the generation clock only rises), or
            // it was bound after the check. Any other write since (the run's own admission that
            // made the binding, a sign-out, a usage note) moves the generation and leaves the
            // check standing.
            let since = check.generation.unwrap_or(0);
            if logins.get(&row.label).is_some_and(|binding| {
                binding.last_success_generation > since
                    || binding.renewed_at.is_some_and(|renewed| renewed > check.at)
            }) {
                moot.set(true);
                return Err("a login was made for the row since".into());
            }
            accounts::note_check_stop(logins, row, check.at);
            Ok(Vec::new())
        },
    );
    match written {
        Ok(_) => CheckWrite::Recorded,
        Err(_) if moot.get() => CheckWrite::Moot,
        Err(_) => CheckWrite::Busy,
    }
}

/// Ends the account's [`accounts::CheckStop`] a token step's check wrote
/// (`accounts_tokens::CheckedRow::hold_after`), at `(at, now)`: the check's second and this
/// write's, once the token was made over that step's record by a person's own run at a terminal
/// on this Mac (`accounts_tokens::CheckedRow::end_after`), so the check's two records end
/// together. Only a check of the account `row` names whose second is the record's ends: a later
/// check of the account (a sign-in's met since) keeps the stop standing, as does one of another
/// account. Whether it ended; nothing is written where it did not.
// Read only by the unix-only `accounts_tokens` (`CheckedRow::end_after`).
#[cfg(unix)]
pub(super) fn end_token_check(
    store: &accounts::Store,
    roster_path: &Path,
    row: &accounts::Row,
    (at, now): (i64, i64),
) -> bool {
    store
        .transact_pool_logins_within(
            &accounts::LOCK_LADDER,
            roster_path,
            now,
            &mut None,
            |raw, _, logins| {
                let current = raw
                    .row(&row.label)
                    .ok_or_else(|| "the row left the roster".to_owned())?;
                let binding = logins
                    .labels
                    .get_mut(&row.label)
                    .filter(|binding| {
                        binding.check_stop.as_ref().is_some_and(|check| {
                            check.at == at && accounts::checked_account(check, current)
                        })
                    })
                    .ok_or_else(|| "no check of the token step stands".to_owned())?;
                binding.check_stop = None;
                Ok(Vec::new())
            },
        )
        .is_ok()
}

/// Holds `check`'s row for a person where a bot protection stopped its run, of any CLI's pool,
/// and the run's own record may not have landed: the check alone is recorded
/// ([`write_check_stop`]), or, where the records cannot be written now, this process keeps it
/// under the renewal's `key` ([`AutoRenewGuard::unrecorded_checks`]) and holds the row until a
/// later pass has written it.
fn hold_for_a_person(
    guard: &std::sync::Arc<Watched<AutoRenewGuard>>,
    store: &accounts::Store,
    key: String,
    check: PendingCheck,
) {
    if write_check_stop(store, &check) == CheckWrite::Busy {
        guard
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .unrecorded_checks
            .insert(key, check);
    }
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
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedRecovery",
        action = "BusySigner",
        project = "aterm_agent::harness::accounts_sync::automatic_shared_tests::recovery_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedRecovery",
        action = "AfterSigner",
        project = "aterm_agent::harness::accounts_sync::automatic_shared_tests::recovery_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "SharedRenewalAdmission",
        action = "Blocked",
        project = "aterm_agent::harness::accounts_sync::automatic_shared_tests::renewal_admission_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "SharedRenewalAdmission",
        action = "Unlock",
        project = "aterm_agent::harness::accounts_sync::automatic_shared_tests::renewal_admission_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "SharedRenewalAdmission",
        action = "Start",
        project = "aterm_agent::harness::accounts_sync::automatic_shared_tests::renewal_admission_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "SharedRenewalAdmission",
        action = "Used",
        project = "aterm_agent::harness::accounts_sync::automatic_shared_tests::renewal_admission_transition"
    )
)]
fn spawn_row_renewal(
    cli: accounts::Cli,
    home: &Path,
    state_dir: &Path,
    roster_path: &Path,
    file_roster: Roster,
    logins: &accounts::Logins,
    (next_row, mut sign_in): (accounts::Row, SignIn),
    (pin_request, at_desk): (Option<accounts::PinRequest>, bool),
    now: i64,
    renew_guard: &std::sync::Arc<Watched<AutoRenewGuard>>,
    spawn_renewal: &SpawnRenewal<'_>,
    ladder: Option<&LadderCtx<'_>>,
    scope: Option<&super::account_work::Scope>,
    admission: &mut Option<(accounts::SigninTurn, ManualSelectionOwner)>,
) -> RenewalDispatch {
    if scope.is_some_and(super::account_work::Scope::cancelled) {
        return RenewalDispatch::Held;
    }
    if !sign_in.names(cli, &next_row.label) {
        return RenewalDispatch::Failed;
    }
    if cli != accounts::Cli::Claude {
        return spawn_vendor_renewal_with(
            cli,
            home,
            state_dir,
            roster_path,
            file_roster,
            logins,
            (next_row, sign_in),
            pin_request,
            now,
            renew_guard,
            spawn_renewal,
            scope,
            admission,
        );
    }
    let store = accounts::Store::at(state_dir);
    if let Err(why) = store.check_signin_state(now) {
        return RenewalDispatch::store_refusal(&why);
    }
    // A sign-in another account's start spaces waits before its ladder reads anything; the
    // check is repeated under the sign-in lock below, where it decides.
    if let Err(disposition) = spacing_admits(&sign_in, &store, now) {
        return disposition;
    }
    let Some(raw_workflow) = file_roster.row(&next_row.label).cloned() else {
        return RenewalDispatch::Failed;
    };
    let refreshed =
        match prepare_retry_workflow(&store, roster_path, &raw_workflow, &next_row, logins, now) {
            Ok(records) => records,
            Err(why) => return RenewalDispatch::store_refusal(&why),
        };
    let logins = refreshed.as_ref().map_or(logins, |records| &records.logins);
    reconcile_workflow_retries(
        renew_guard,
        &store,
        cli,
        roster_path,
        (&raw_workflow, &next_row, logins),
        now,
    );
    if next_row.email.is_none() && cli != Cli::Kimi {
        return RenewalDispatch::Failed;
    }
    let mut shared_preparation = None;
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
            let authority = shared_renewal::automatic(authority, config_beside(roster_path));
            let Ok(grant) = authority.prepare(&next_row) else {
                return RenewalDispatch::Failed;
            };
            if grant.source != source {
                return RenewalDispatch::Failed;
            }
            if logins
                .shared
                .check_preparation(source, grant.generation, grant.expected_principal)
                .is_err()
            {
                // A pending attempt may still be running in another worker.
                // Do not ask the issuer to supersede its authority mid-sign-in.
                let _signin = match renewal_signin_admission(&store) {
                    Ok(signin) => signin,
                    Err(disposition) => return disposition,
                };
                if let Err(why) = super::cli::shared_renewal::recover_consumed(
                    authority.as_ref(),
                    &grant,
                    &store,
                    accounts::RenewalPolicy {
                        roster_path,
                        roster: &file_roster,
                        row: &next_row,
                        logins,
                    },
                    &_signin,
                    now,
                ) && let Some(ctx) = ladder
                {
                    (ctx.log)(&format!("shared renewal recovery deferred: {why}"));
                }
                return RenewalDispatch::Failed;
            }
            if renew_guard
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .in_flight
            {
                return RenewalDispatch::Held;
            }
            shared_preparation = Some(grant);
            Some(authority)
        }
        Err(_) => return RenewalDispatch::Failed,
    };
    let reads = guard_reads(renew_guard);
    let mut mailbox_dir = resolve_mailbox_dir_with(&reads.var, Some(home), &store);
    // The automatic ladder ([`ladder_route`]) decides an upkeep's renewal and records how it
    // ends; a person's own choice (a pin request, a click) takes the routes it always took, and
    // only a click whose person is at this desk now (`at_desk`) may take one that opens a
    // window: a saved choice a tick resumed, or one that waited in the sign-in queue, is served
    // by a windowless route or fails, which retires it.
    let (has_mailbox, has_headless_profile, unattended) = match ladder
        .filter(|_| pin_request.is_none())
    {
        Some(ctx) => match if let Some(preparation) = shared_preparation.as_ref() {
            ladder_route_in_context(
                home,
                &store,
                (&next_row, Some(preparation)),
                now,
                renew_guard,
                ctx,
                &reads.clock,
            )
        } else {
            ladder_route(home, &store, &next_row, now, renew_guard, ctx)
        } {
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
            // A row on its token needs no sign-in now; nothing is started, and the next
            // candidate is the upkeep's to try.
            LadderStep::Held | LadderStep::OnToken => return RenewalDispatch::Held,
            LadderStep::NoRoute => return RenewalDispatch::Failed,
        },
        None => {
            // A saved choice a tick resumed, or a Next selection, with nobody at this desk: a
            // row a bot protection stopped, or whose renewal is a person's, is not signed in
            // by one (docs/DESIGN-claude-sign-in-sitting-2026-11-12.md §3), its token in use here
            // or not, since every route here loads a page; failing retires the choice, and a Next
            // selection goes on to its next row.
            if !at_desk {
                // A bot protection this process met and could not record yet holds it too, as
                // it holds the ladder's every route ([`hold_for_a_person`]): this pass tries to
                // record it, and the choice fails either way unless a login made since ended it.
                let pending = renew_guard
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .unrecorded_checks
                    .get(&next_row.label)
                    .cloned();
                if let Some(pending) = pending {
                    let written = write_check_stop(&store, &pending);
                    if written != CheckWrite::Busy {
                        renew_guard
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .unrecorded_checks
                            .remove(&next_row.label);
                    }
                    if written != CheckWrite::Moot {
                        return RenewalDispatch::Failed;
                    }
                }
                if left_to_a_person(&store, &raw_workflow, logins) {
                    return RenewalDispatch::Failed;
                }
            }
            let has_mailbox = mailbox_dir.is_some()
                || has_configured_mailbox_for_row_with(
                    &reads.var,
                    Some(home),
                    &store,
                    Some(&next_row.label),
                    next_row.email.as_deref(),
                );
            let has_headless_profile = claude_browser_profile_ready(&store, logins, &raw_workflow)
                && reads.has_browser(home);
            if !at_desk && !has_mailbox && !has_headless_profile {
                return RenewalDispatch::Failed;
            }
            (has_mailbox, has_headless_profile, None)
        }
    };
    let next_label = next_row.label.clone();
    // The row as the log names it ([`super::account_names::logged_row`]), read here: the attempt
    // runs on a thread of its own, which reads no lab a caller's thread holds.
    let next_logged =
        super::account_names::logged_row(cli, &next_row, super::alab_roster::current().as_deref());
    let next_email = match next_row.email.clone() {
        Some(email) => email,
        None if cli == Cli::Kimi && shared_preparation.is_none() => String::new(),
        None => return RenewalDispatch::Failed,
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
        if !mailbox_updated_since && !claude_browser_profile_ready(&store, logins, &raw_workflow) {
            return RenewalDispatch::Held;
        }
    }
    // This Mac's own upkeep signs a row in in aterm's own window where the window reads the
    // row's login code by itself; a run for another Mac, one over SSH, and a row whose
    // mailbox only a headless browser or the fleet's relay reads open none, and keep the
    // headless route.
    let opens_window = has_mailbox
        && upkeep_opens_window(reads, shared_authority.is_some(), &|| {
            window_reads_mailbox_for_row_with(
                &reads.var,
                Some(home),
                &store,
                Some(&next_label),
                Some(&next_email),
            )
        });
    // A row whose last sign-in ended where only a person can go on (the consent page's human
    // check, Cloudflare's) is left to that person: the upkeep opens no window for it again,
    // however long ago that was. A person's own renewal, or their pin request, opens it.
    if opens_window
        && pin_request.is_none()
        && accounts::current_sign_in_task(&raw_workflow, logins)
            .is_some_and(|task| task.action == accounts::SignInAction::Renew)
    {
        return RenewalDispatch::Held;
    }
    // The machine's one sign-in at a time, taken before anything is admitted: while another
    // sign-in (a vendor's renewal, `accounts browser`'s window, another process's) holds it this
    // renewal is Held, so a pin request stays pending for a later upkeep, with no cooldown
    // started and nothing woken, and the lock taken here is held by the work it starts.
    let background_signin = if !sign_in.has_turn() && admission.is_none() {
        match renewal_signin_admission(&store) {
            Ok(signin) => Some(signin),
            Err(disposition) => return disposition,
        }
    } else {
        None
    };
    // Under the sign-in lock, the last ask (plan package FL-174): the ladder starts no sign-in
    // for a login that was signed in or served after its recorded sign-out and whose record
    // is, or is about to be, lifted. A person's own request is theirs to make and is not asked.
    let own_witnesses;
    let live = match ladder.and_then(|ctx| ctx.live) {
        Some(live) => live,
        None => {
            own_witnesses = live_logins_known(renew_guard);
            &own_witnesses
        }
    };
    if let Some((_, _, _, log)) = unattended.as_ref()
        && login_live_again(
            (home, state_dir, roster_path),
            &next_label,
            now,
            renew_guard,
            live,
            &**log,
        )
    {
        return RenewalDispatch::Held;
    }
    if let Err(disposition) = spacing_admits(&sign_in, &store, now) {
        return disposition;
    }
    if let Err(disposition) = fleet_admits(&sign_in, &store, now) {
        return disposition;
    }
    reconcile_browser_window_unpark(
        renew_guard,
        &next_label,
        workflow_browser_window_token(&store, &raw_workflow, logins, 0),
        &raw_workflow,
        logins,
        (&store, cli, roster_path),
    );
    let Some(mut permit) = AutoRenewPermit::claim_with_context(
        renew_guard,
        &next_label,
        (reads.clock)(),
        pin_request.is_some(),
        mailbox_mtime,
        shared_preparation.as_ref().map(RenewalPreparation::Shared),
    ) else {
        return RenewalDispatch::Held;
    };
    let completion = permit.follow_scope(scope);
    // The start, under the lock this work holds until it ends: the spacing clock records it.
    if scope.is_some_and(super::account_work::Scope::cancelled) {
        return RenewalDispatch::Held;
    }
    if let Some((turn, owner)) = admission.take() {
        sign_in = match sign_in.admitted(turn, owner) {
            Ok(sign_in) => sign_in,
            Err(_) => return RenewalDispatch::Fenced,
        };
    }
    let Some(signin) = sign_in.take_turn().or(background_signin) else {
        return RenewalDispatch::Fenced;
    };
    let signin = match sign_in.begin(cli, &next_label, signin, &store, now) {
        Ok(begun) => begun,
        Err(refused) => return spacing_refusal(&refused),
    };
    permit.wake_pending = Some(state_dir.to_path_buf());
    permit.attribute_row(&store, cli, &next_row, &raw_workflow, now, logins);
    // The ladder's own run is marked for as long as its work lives, so a lift of its account
    // ends it; a person's is not (`RunMark::of`). A marked run works under its own operation,
    // a child of this step's, so ending it ends nothing else the step started.
    let run_mark = RunMark::of(renew_guard, &next_label, unattended.is_some(), scope);
    let home_owned = home.to_path_buf();
    let state_owned = state_dir.to_path_buf();
    let roster_owned = roster_path.to_path_buf();
    let ladder_guard = std::sync::Arc::clone(renew_guard);
    let started_by = unattended.as_ref().map(|(route, _, _, log)| {
        (
            route.as_str(),
            next_logged.clone(),
            std::sync::Arc::clone(log),
        )
    });
    let run_seam = ladder.and_then(|ctx| ctx.run_attempt.clone());
    let shared_host = ladder.and_then(|ctx| ctx.shared.host.clone());
    let captured_logins = logins.clone();
    let worker_scope = run_mark
        .as_ref()
        .and_then(|mark| mark.scope.clone())
        .or_else(|| scope.cloned());
    let scope = worker_scope.clone();
    let started = start_auto_renew(
        permit,
        move || {
            use super::cli::Renewal;

            let store = accounts::Store::at(&state_owned);
            let _signin = signin;
            if scope
                .as_ref()
                .is_some_and(super::account_work::Scope::cancelled)
            {
                // Superseded before its work began: it loaded nothing the provider counts, so
                // its spacing goes back and no cooldown is left on its row.
                if run_mark.as_ref().is_some_and(RunMark::superseded) {
                    _signin.refund_spacing(&store);
                    ladder_guard
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .last_attempt
                        .remove(&next_label);
                }
                return;
            }
            if !queued_pin_admitted(
                &store,
                accounts::RenewalPolicy {
                    roster_path: &roster_owned,
                    roster: &file_roster,
                    row: &next_row,
                    logins: &captured_logins,
                },
                pin_request.as_ref(),
                _signin.selection(),
                now,
            ) {
                return;
            }
            let deferred = std::cell::Cell::new(false);
            // Whether this work started its provider's step (a run of `claude auth login`, or
            // a shared custody's): from there on its sign-in page may have loaded, which the
            // provider counts, so a run that got that far keeps its spacing however it ends.
            let reached = std::cell::Cell::new(false);
            // What the last run's opener's headless sign-in met (its outcome file), which the
            // backoff below reads.
            let met: std::cell::RefCell<Option<super::open_url::Met>> =
                std::cell::RefCell::new(None);
            // How the last run ended (its exit, what its opener met, why it did not finish), kept
            // as a shared-custody run keeps its own, so both say a run's end from one input
            // (`cli::RenewalObservation::ended`, the one wording of a run's end).
            let ended: std::cell::RefCell<Option<super::cli::RenewalObservation>> =
                std::cell::RefCell::new(None);
            let attempt_snapshot = std::cell::RefCell::new(None);
            // Diagnostic facts only. Shared custody must not enter the ordinary attempt
            // snapshot or the met cell used by retry/blocked policy below.
            let shared_observation = std::cell::RefCell::new(None);
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
                let ran = shared_renewal::sign_in_with_inputs_scoped(
                    &home_owned,
                    &staging_text,
                    &next_email,
                    browser,
                    (force_headless, mailbox_dir.as_deref(), None),
                    scope.as_ref(),
                );
                let (ran, seen) = match ran {
                    Ok(got) => got,
                    Err(failure) => {
                        if failure.is_deferred() {
                            completion.defer();
                            deferred.set(true);
                            let _ = fs::remove_dir_all(&staging);
                            return Renewal::Deferred;
                        }
                        if failure.is_cancelled() {
                            deferred.set(true);
                            let _ = fs::remove_dir_all(&staging);
                            return Renewal::Deferred;
                        }
                        met.replace(failure.met.clone());
                        ended.replace(Some(super::cli::RenewalObservation::from_outcome(&Err(
                            failure,
                        ))));
                        let _ = fs::remove_dir_all(&staging);
                        return Renewal::Failed;
                    }
                };
                met.replace(seen.clone());
                let code = ran.code;
                ended.replace(Some(super::cli::RenewalObservation::from_outcome(
                    &super::cli::RenewalExit::from_capture((ran, seen)),
                )));
                if code != Some(0) || !accounts::login_made(&staging) {
                    let _ = fs::remove_dir_all(&staging);
                    return Renewal::authentication_failed(code);
                }
                if scope
                    .as_ref()
                    .is_some_and(super::account_work::Scope::cancelled)
                {
                    deferred.set(true);
                    let _ = fs::remove_dir_all(&staging);
                    return Renewal::Deferred;
                }
                let status_got = match scope.as_ref() {
                    Some(scope) => super::cli::status_in_home_within_scoped(
                        claude_prog.as_os_str(),
                        &staging_text,
                        &env_var,
                        super::cli::AUTH_CHECK_BUDGET,
                        scope,
                    ),
                    None => super::cli::status_in_home_within(
                        claude_prog.as_os_str(),
                        &staging_text,
                        &env_var,
                        super::cli::AUTH_CHECK_BUDGET,
                    ),
                };
                if scope
                    .as_ref()
                    .is_some_and(super::account_work::Scope::cancelled)
                {
                    deferred.set(true);
                    let _ = fs::remove_dir_all(&staging);
                    return Renewal::Deferred;
                }
                let Ok(status_got) = status_got else {
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
                if scope
                    .as_ref()
                    .is_some_and(super::account_work::Scope::cancelled)
                {
                    deferred.set(true);
                    return Renewal::Deferred;
                }
                shared_observation.replace(None);
                let snapshot = &captured_logins;
                let route = if force_headless && has_headless_profile {
                    "profile"
                } else if has_mailbox {
                    "mailbox"
                } else {
                    "browser"
                };
                if let Some(authority) = shared_authority.as_deref() {
                    let env = quiet_env(
                        &home_owned,
                        &state_owned,
                        &config_beside(&roster_owned),
                        now,
                    );
                    let host = shared_renewal::BackgroundHost {
                        scope: scope.as_ref(),
                        home: &home_owned,
                        mailbox: mailbox_dir.as_deref(),
                        headless: force_headless,
                    };
                    let activity = LoginActivityGuard::begin_row(
                        &ladder_guard,
                        &store,
                        cli,
                        &next_row,
                        Some(route),
                    );
                    reached.set(true);
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
                    if result.renewal.published() {
                        completion.complete();
                    }
                    if result
                        .observation
                        .as_ref()
                        .is_some_and(|observation| observation.deferred())
                    {
                        completion.defer();
                        deferred.set(true);
                        return Renewal::Deferred;
                    }
                    if scope
                        .as_ref()
                        .is_some_and(super::account_work::Scope::cancelled)
                    {
                        deferred.set(true);
                        return Renewal::Deferred;
                    }
                    if result.renewal != Renewal::Renewed
                        && let Some(observation) = result.observation.as_ref()
                    {
                        activity.complete(
                            &next_row,
                            shared_host
                                .as_deref()
                                .map_or_else(crate::supervise::limit::unix_now, |host| host.now())
                                .max(now),
                            (result.renewal, observation.clone()),
                        );
                    }
                    shared_observation.replace(result.observation);
                    if matches!(result.renewal, Renewal::SigninHeld | Renewal::Deferred) {
                        deferred.set(true);
                    }
                    return result.renewal;
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
                met.replace(None);
                ended.replace(None);
                let result = {
                    let _activity = LoginActivityGuard::begin_row(
                        &ladder_guard,
                        &store,
                        cli,
                        &next_row,
                        Some(route),
                    );
                    reached.set(true);
                    match &run_seam {
                        Some(run) => run(browser, force_headless),
                        None => live_attempt(browser, force_headless, snapshot),
                    }
                };
                if result.published() {
                    completion.complete();
                }
                if scope
                    .as_ref()
                    .is_some_and(super::account_work::Scope::cancelled)
                {
                    deferred.set(true);
                    return Renewal::Deferred;
                }
                if matches!(result, Renewal::SigninHeld | Renewal::Deferred) {
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
                let (shared, own) = (shared_observation.borrow(), ended.borrow());
                match shared.as_ref().or(own.as_ref()) {
                    Some(observation) if renewal != Renewal::Renewed => observation.ended(renewal),
                    _ => super::sign_in_ladder::how_it_ended(renewal, None, None, None),
                }
            };
            // Where this run's sign-in that stands was made: in aterm's own window, whose store
            // is then the account's, or by aterm's opener on a headless profile of its own; each
            // makes its token there.
            #[cfg(unix)]
            let signed_in = std::cell::Cell::new(None);
            let sleep_watch = super::wake_deadline::SleepWatch::start();
            let ran = (|| -> Result<Renewal, String> {
                store
                    .check_signin_state(now)
                    .inspect_err(|_| deferred.set(true))?;
                let opener = resolve_opener_link(&home_owned, &store);
                // Unattended, or with nobody at the desk for the click that asked, nothing that
                // opens a window is ever a fallback: neither the row's desk browser profile nor
                // the system's default browser.
                let non_headless_fallback = if unattended.is_some() || !at_desk {
                    None
                } else {
                    select_auto_renew_browser(
                        next_row.browser.as_deref(),
                        mailbox_dir.as_deref(),
                        opener.as_ref().ok().cloned(),
                    )
                    .or_else(|| {
                        (at_desk && next_row.browser.is_none())
                            .then_some(super::cli::RenewBrowser::Default)
                    })
                };
                let browser = if has_mailbox || has_headless_profile {
                    // The window where this run opens it ([`upkeep_opens_window`]), decided
                    // before the run started; a scripted run is handed the same choice.
                    upkeep_browser(
                        opens_window,
                        unattended.is_some() || !at_desk,
                        opener.map_err(|error| error.to_string())?,
                    )
                } else {
                    let Some(b) = non_headless_fallback.clone() else {
                        return Err(opener.err().map_or_else(
                            || "no unattended sign-in route is available".to_owned(),
                            |error| error.to_string(),
                        ));
                    };
                    b
                };
                #[cfg(unix)]
                signed_in.set(Some(browser.signed_in()));
                let result = run_attempt(&browser, !has_mailbox && has_headless_profile);
                if result.published() {
                    return Ok(result);
                }
                if result.can_retry_authentication()
                    && !has_mailbox
                    && has_headless_profile
                    && let Some(fallback) = non_headless_fallback
                {
                    #[cfg(unix)]
                    signed_in.set(Some(fallback.signed_in()));
                    let again = run_attempt(&fallback, false);
                    return if again.published() {
                        Ok(again)
                    } else {
                        Err(how(again))
                    };
                }
                Err(how(result))
            })();
            // A run a lift of its account superseded, and an unattended one the Mac slept
            // across, made no login and say nothing of the sign-in (plan package FL-174):
            // neither is counted failed or blocked and neither starts a backoff. A run that
            // made its login stands whatever happened beside it. A row's unattended runs are
            // waived for a sleep only so many times in a row
            // (`sign_in_ladder::MOST_INTERRUPTED_RUNS`): the next one a sleep spans is counted
            // as it ended, below, and its backoff holds the one after.
            let superseded = ran.is_err() && run_mark.as_ref().is_some_and(RunMark::superseded);
            let withdrawn = deferred.get()
                || scope
                    .as_ref()
                    .is_some_and(super::account_work::Scope::cancelled);
            let slept = sleep_watch.slept();
            let by_sleep = !superseded
                && !withdrawn
                && super::sign_in_ladder::interrupted_by_sleep(
                    slept,
                    ran.is_ok(),
                    met.borrow().as_ref().is_some_and(|met| met.stopped()),
                );
            // Only the ladder's own run is ever waived for a sleep, and only its runs are
            // counted: the next pass starts one again by itself, and the count bounds that. A
            // run a person started (a click, a saved choice, a Next selection) is counted as
            // it ended, sleep or no sleep, and its request is retired below: left pending, a
            // saved choice is resumed by every pass with no cooldown and no spacing against
            // its own account, so waiving it would load one sign-in page per wake of a Mac
            // asleep with its lid shut.
            let interrupted = unattended.is_some()
                && ladder_guard
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .ladder
                    .run_ended(
                        &next_label,
                        super::sign_in_ladder::slept_across(slept),
                        by_sleep,
                    );
            // Only a run that never started its provider's step gives its spacing back: one
            // that did may have loaded a sign-in page the provider counts.
            if (superseded || interrupted) && !reached.get() {
                _signin.refund_spacing(&store);
            }
            if interrupted && let Some((_, _, _, log)) = unattended.as_ref() {
                log(&format!(
                    "account {next_label}: automatic sign-in interrupted: {}; it is tried again and \
                     counted against nothing",
                    super::sign_in_ladder::Unfinished::Interrupted.words()
                ));
            }
            if withdrawn || superseded || interrupted {
                ladder_guard
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .last_attempt
                    .remove(&next_label);
                return;
            }
            drop(run_mark);
            let renewed = matches!(&ran, Ok(Renewal::Renewed));
            let published = ran.is_ok();
            let now_done = crate::supervise::limit::unix_now().max(now);
            let terminal = ran.as_ref().copied().unwrap_or(Renewal::Failed);
            if super::cli::finish_manual_next_attempt(&store, &_signin, now_done, terminal)
                == Renewal::Deferred
                && terminal != Renewal::Deferred
            {
                return;
            }
            // What stopped the run, where something did ([`super::open_url::Met::stopped`]), and,
            // where a bot protection did, the check to hold the row's account on
            // ([`hold_for_a_person`]): whoever started the run, the ladder, a person's click, a
            // saved choice or a Next selection, the row waits for a person from here on
            // (docs/DESIGN-claude-sign-in-sitting-2026-11-12.md §3).
            let stopped_by = met
                .borrow()
                .as_ref()
                .filter(|met| !published && met.stopped())
                .cloned();
            let check = stopped_by
                .as_ref()
                .filter(|met| met.word.is_bot_protection())
                .and_then(|_| {
                    attempt_snapshot
                        .borrow()
                        .as_ref()
                        .map(|observed| PendingCheck {
                            roster_path: roster_owned.clone(),
                            row: next_row.clone(),
                            generation: observed
                                .get(&next_label)
                                .map(|binding| binding.renewal_generation),
                            at: now_done,
                        })
                });
            if let Some((route, read, inputs, log)) = &unattended {
                let failed = if stopped_by.is_some() {
                    accounts::AttemptOutcome::Blocked
                } else {
                    accounts::AttemptOutcome::Failed
                };
                let (outcome, routes) = match &ran {
                    Ok(Renewal::Renewed) => (
                        accounts::AttemptOutcome::SignedIn,
                        route.as_str().to_owned(),
                    ),
                    Ok(other) => (
                        failed,
                        super::sign_in_ladder::failed_words(*route, &how(*other), read),
                    ),
                    Err(how) => (
                        failed,
                        super::sign_in_ladder::failed_words(*route, how, read),
                    ),
                };
                // A published login records both the matched success and any requested
                // mismatch atomically. Unpublished failures and a scripted requested success
                // still name their original parent. A bot protection's stop is held whether or
                // not its run's record lands.
                if (!published || (run_seam.is_some() && renewed))
                    && let Some(observed) = attempt_snapshot.borrow().as_ref()
                    && !note_attempt(
                        &store,
                        (&roster_owned, &next_row, observed),
                        (outcome, stopped_by.as_ref().map(|met| (met, false))),
                        &routes,
                        now_done,
                    )
                {
                    let Some(check) = check.clone() else {
                        return;
                    };
                    hold_for_a_person(&ladder_guard, &store, next_label.clone(), check);
                }
                let mut guard = ladder_guard
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let line = super::sign_in_ladder::log_line(&next_logged, outcome.as_str(), &routes);
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
                    let at = (guard.reads.clock)();
                    let mut wait = Some(guard.ladder.failed(&next_label, inputs, at));
                    let mut sooner = "when its inputs change";
                    if let Some(met) = &stopped_by {
                        // A bot protection's page is never tried again on a clock: the row waits
                        // for a person (docs/DESIGN-claude-sign-in-sitting-2026-11-12.md §3).
                        wait = (!met.word.is_bot_protection()).then_some(PERSON_BACKOFF);
                        guard
                            .ladder
                            .page_stopped(&next_label, wait.map(|wait| at + wait));
                        sooner = "when a person signs it in at this Mac";
                    }
                    guard.ladder_owners.insert(
                        next_label.clone(),
                        LoginOwner::new(&store, cli, &next_label, now_done)
                            .for_row(&next_row)
                            .for_workflow(&raw_workflow, &captured_logins)
                            .observing(
                                attempt_snapshot
                                    .borrow()
                                    .as_ref()
                                    .unwrap_or(&captured_logins),
                            ),
                    );
                    drop(guard);
                    login_activity_moved();
                    log(&match wait {
                        Some(wait) => format!(
                            "{line} (tried again in {} or {sooner})",
                            super::upgrade::span(wait.as_secs())
                        ),
                        None => format!(
                            "{line} (not tried again until a person signs it in at this Mac)"
                        ),
                    });
                }
            } else if let Some(met) = &stopped_by
                && let Some(observed) = attempt_snapshot.borrow().as_ref()
            {
                // A sign-in the ladder did not start (a person's click, a saved choice a tick
                // resumed, a Next selection) records where a page stopped it as the ladder's own
                // does, by hand where a person at this desk asked for it, so the upkeep holds
                // the row as it would have held its own.
                let routes = how(terminal);
                let routes = if at_desk {
                    format!("by hand: {routes}")
                } else {
                    routes
                };
                if !note_attempt(
                    &store,
                    (&roster_owned, &next_row, observed),
                    (accounts::AttemptOutcome::Blocked, Some((met, at_desk))),
                    &routes,
                    now_done,
                ) && let Some(check) = check.clone()
                {
                    hold_for_a_person(&ladder_guard, &store, next_label.clone(), check);
                }
                login_activity_moved();
            }
            completion.complete();
            if let Some(request) = &pin_request {
                if !renewed {
                    let _ = store.cancel_pin_request(request, now_done);
                    return;
                }
                let _ =
                    store.complete_requested_pin_at(&roster_owned, &file_roster, request, now_done);
            }
            // A row signed in in aterm's window gets its year-long token in the same store, by
            // itself (the token design's §4.6), and one signed in headlessly on the same
            // headless profile (FL-137), last of all: the renewal is complete, its requested
            // pin is answered and its sign-in lock is released first, so nothing that waits on
            // this sign-in waits on a token's page too. What the token verb refuses is logged
            // with no address in it, and the sign-in stands either way. A scripted run signed
            // nothing in, so no store or profile holds a session to make a token in, and a
            // sign-in in a person's browser is offered the step on the row's own surfaces.
            #[cfg(unix)]
            if renewed
                && run_seam.is_none()
                && let Some(
                    signed_in @ (super::accounts_tokens::SignedIn::Window
                    | super::accounts_tokens::SignedIn::Headless),
                ) = signed_in.get()
            {
                drop(_signin);
                let env = quiet_env(
                    &home_owned,
                    &state_owned,
                    &config_beside(&roster_owned),
                    now_done,
                );
                let host = super::cli::LiveAccounts::background(scope.clone());
                let (mut said, mut refused) = (Vec::new(), Vec::new());
                // A check its page carried that could not be recorded on the account is held
                // in this process until it is ([`hold_for_a_person`]).
                let mut unrecorded = |check: PendingCheck| {
                    ladder_guard
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .unrecorded_checks
                        .insert(next_label.clone(), check);
                };
                if super::accounts_tokens::after_sign_in(
                    &env,
                    &host,
                    &next_label,
                    signed_in,
                    (&mut said, &mut refused),
                    &mut unrecorded,
                )
                .is_some()
                    && let Some((_, _, _, log)) = &unattended
                {
                    log(&token_step_line(
                        &next_logged,
                        signed_in,
                        &String::from_utf8_lossy(&refused),
                    ));
                }
            }
        },
        spawn_renewal,
    );
    if started.is_ok()
        && let Some((route, logged, log)) = started_by
    {
        log(&format!(
            "account {logged}: automatic sign-in started by its {route} route"
        ));
    }
    if worker_scope
        .as_ref()
        .is_some_and(super::account_work::Scope::cancelled)
    {
        return RenewalDispatch::Held;
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
    renew_guard: &std::sync::Arc<Watched<AutoRenewGuard>>,
    spawn_renewal: &SpawnRenewal<'_>,
) -> RenewalDispatch {
    spawn_vendor_renewal_with(
        cli,
        home,
        state_dir,
        roster_path,
        file_roster.clone(),
        logins,
        (
            next_row.clone(),
            crate::harness::sign_in_ladder::asked_fixture(cli, &next_row.label),
        ),
        None,
        now,
        renew_guard,
        spawn_renewal,
        None,
        &mut None,
    )
}

/// [`spawn_row_renewal`] for another CLI's row: its renewal ([`super::cli::renew_one_for`]: the
/// vendor's own sign-in under the machine's sign-in lock, its page approved headlessly) started
/// in the background, where a headless browser can approve the CLI's page
/// ([`Cli::renews_headlessly`]), one is installed, the row has an email and the CLI has a
/// sign-in (one that opens the desk's browser runs under [`super::login_drive::NO_DESK`] there,
/// [`crate::ai::connector::SignIn::no_desk`]). A row whose last one stopped for a person where no
/// bot protection stopped it waits [`PERSON_BACKOFF`] ([`note_background_end`], and its sign-in
/// task on record across a restart). A row a bot protection stopped (Cloudflare's check, a
/// block page), or one of its pool and realm here, is left to a person on no clock
/// ([`person_hold`], or the check this process met and could not record yet,
/// [`hold_for_a_person`]): no door here starts it, the upkeep, a saved choice, a Next selection
/// or a person's click at the desk, since each would run the same headless page; a person's
/// `accounts login` at a terminal does. A Grok row's never starts,
/// from the upkeep, a saved selection or a click alike: each would request a live xAI device
/// code and then load accounts.x.ai's block page, which aterm never works around (design §5
/// (g)). Its records checked as every CLI's are (unreadable ones fence, keeping a saved
/// selection), it is [`RenewalDispatch::Failed`] before any lock, browser or sign-in is
/// reached, and the row is signed in by the one step every surface names, a person's `accounts
/// login <account> --cli grok`.
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
    (next_row, mut sign_in): (accounts::Row, SignIn),
    pin_request: Option<accounts::PinRequest>,
    now: i64,
    renew_guard: &std::sync::Arc<Watched<AutoRenewGuard>>,
    spawn_renewal: &SpawnRenewal<'_>,
    scope: Option<&super::account_work::Scope>,
    admission: &mut Option<(accounts::SigninTurn, ManualSelectionOwner)>,
) -> RenewalDispatch {
    if scope.is_some_and(super::account_work::Scope::cancelled) {
        return RenewalDispatch::Held;
    }
    let store = accounts::Store::at_for(state_dir, cli, home);
    if let Err(why) = store.check_signin_state(now) {
        return RenewalDispatch::store_refusal(&why);
    }
    if let Err(disposition) = spacing_admits(&sign_in, &store, now) {
        return disposition;
    }
    if let Err(disposition) = fleet_admits(&sign_in, &store, now) {
        return disposition;
    }
    let Some(raw_workflow) = file_roster.row(&next_row.label).cloned() else {
        return RenewalDispatch::Failed;
    };
    let refreshed =
        match prepare_retry_workflow(&store, roster_path, &raw_workflow, &next_row, logins, now) {
            Ok(records) => records,
            Err(why) => return RenewalDispatch::store_refusal(&why),
        };
    let logins = refreshed.as_ref().map_or(logins, |records| &records.logins);
    reconcile_workflow_retries(
        renew_guard,
        &store,
        cli,
        roster_path,
        (&raw_workflow, &next_row, logins),
        now,
    );
    if !cli.renews_headlessly() {
        return RenewalDispatch::Failed;
    }
    let reads = guard_reads(renew_guard);
    // Existing native grants use first-party refresh, without entering the
    // browser route that an earlier attempt may have stopped on. The worker
    // still checks current policy, custody and canonical identity before use.
    #[cfg(unix)]
    let native_profile = cli == Cli::Kimi
        && next_row.expected_identity_email().is_some()
        && super::cli::pool_kimi::profile(&store, &next_row, logins)
            .is_ok_and(|profile| profile.is_some());
    #[cfg(not(unix))]
    let native_profile = false;
    let key = format!("{}:{}", cli.id(), next_row.label);
    // A bot protection this process met and could not record holds the row until it is on
    // record, which this pass tries again ([`hold_for_a_person`]), and holds as one on record
    // does; the records read here predate that write. One met on another row of the pool and
    // realm holds this row too ([`person_hold`]).
    let (pending, pool_pending) = {
        let guard = renew_guard
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let prefix = format!("{}:", cli.id());
        (
            guard.unrecorded_checks.get(&key).cloned(),
            guard.unrecorded_checks.iter().any(|(other, check)| {
                *other != key
                    && other.starts_with(&prefix)
                    && check.row.vendor.region == raw_workflow.vendor.region
            }),
        )
    };
    let pending_holds = pending.is_some_and(|pending| {
        let written = write_check_stop(&store, &pending);
        if written != CheckWrite::Busy {
            renew_guard
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .unrecorded_checks
                .remove(&key);
        }
        written != CheckWrite::Moot
    });
    // A row a bot protection stopped, or one of its pool and realm here, is signed in by no door
    // of this upkeep, on any clock (docs/DESIGN-claude-sign-in-sitting-2026-11-12.md §3): not
    // the upkeep, a saved choice or a Next selection a tick resumes, and not a person's click at
    // this desk either, since the renewal it starts loads the same headless page the check
    // answered and nobody can go on in it. Failing retires such a choice, and a Next selection
    // goes on to its next row; the row's person's step is a terminal's `accounts login`
    // ([`super::cli::left_step`]), which every surface names from its task. A Kimi Code CLI row's
    // native refresh loads no page and goes on; its fall back to the page is held where it is
    // chosen ([`super::cli::pool_kimi`]).
    if !native_profile
        && (pending_holds
            || pool_pending
            || person_hold(&store, &file_roster, &raw_workflow, logins).is_some())
    {
        return RenewalDispatch::Failed;
    }
    if next_row.email.is_none() && cli != Cli::Kimi
        || cli.accounts().signin.as_ref().is_none()
        || (!native_profile && !reads.has_browser(home))
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
    let current_task = if native_profile {
        None
    } else {
        accounts::current_sign_in_task(&raw_workflow, logins)
    };
    let browser_token = match current_task {
        Some(task) => {
            let token = (task.action == accounts::SignInAction::Browser)
                .then(|| workflow_browser_window_token(&store, &raw_workflow, logins, task.at))
                .flatten();
            if pin_request.is_none()
                && token.is_none()
                && now.saturating_sub(task.at) < PERSON_BACKOFF_SECS
            {
                return RenewalDispatch::Held;
            }
            token
        }
        None => workflow_browser_window_token(&store, &raw_workflow, logins, 0),
    };
    if scope.is_some_and(super::account_work::Scope::cancelled) {
        return RenewalDispatch::Held;
    }
    // The spacing clock is read under the lock, which the worker takes again to start.
    if !sign_in.has_turn() && admission.is_none() {
        match renewal_signin_admission(&store) {
            Ok(probe) => {
                let spaced = spacing_admits(&sign_in, &store, now);
                drop(probe);
                if let Err(disposition) = spaced {
                    return disposition;
                }
            }
            Err(disposition) => return disposition,
        }
    }
    reconcile_browser_window_unpark(
        renew_guard,
        &key,
        browser_token,
        &raw_workflow,
        logins,
        (&store, cli, roster_path),
    );
    let Some(mut permit) = AutoRenewPermit::claim_with_mailbox(
        renew_guard,
        &key,
        (reads.clock)(),
        pin_request.is_some(),
        mailbox_mtime,
    ) else {
        return RenewalDispatch::Held;
    };
    let completion = permit.follow_scope(scope);
    if let Some((turn, owner)) = admission.take() {
        sign_in = match sign_in.admitted(turn, owner) {
            Ok(sign_in) => sign_in,
            Err(_) => return RenewalDispatch::Fenced,
        };
    }
    permit.wake_pending = Some(state_dir.to_path_buf());
    permit.attribute_row(&store, cli, &next_row, &raw_workflow, now, logins);
    let own_dir = file_roster.row(&next_row.label).and_then(|r| r.dir.clone());
    let roster_path = roster_path.to_path_buf();
    let parker = std::sync::Arc::clone(renew_guard);
    let captured_logins = logins.clone();
    let worker_scope = scope.cloned();
    let scope = worker_scope.clone();
    let started = start_auto_renew(
        permit,
        move || {
            if scope
                .as_ref()
                .is_some_and(super::account_work::Scope::cancelled)
            {
                return;
            }
            if !queued_pin_admitted(
                &store,
                accounts::RenewalPolicy {
                    roster_path: &roster_path,
                    roster: &file_roster,
                    row: &next_row,
                    logins: &captured_logins,
                },
                pin_request.as_ref(),
                sign_in.selection(),
                now,
            ) {
                return;
            }
            let publication_claims = LoginClaims::capture(&parker);
            let renewal = {
                let _activity =
                    LoginActivityGuard::begin_row(&parker, &store, cli, &next_row, None);
                let host = super::cli::LiveAccounts::background(scope.clone());
                super::cli::renew_one_for(
                    cli,
                    now,
                    &host,
                    &store,
                    (&next_row, &roster_path, &captured_logins),
                    (own_dir.as_deref(), sign_in),
                    &mut io::sink(),
                    &mut io::sink(),
                )
            };
            if renewal == super::cli::Renewal::SigninHeld {
                // This typed result is produced before Begin/provider work. The
                // bounded background loser must not stamp a new retry cooldown.
                completion.defer();
                return;
            }
            if renewal.published() {
                completion.complete();
            }
            if scope
                .as_ref()
                .is_some_and(super::account_work::Scope::cancelled)
            {
                return;
            }
            // A bot protection stopped it: the account's check is held on no clock, whether or
            // not the run's own record landed ([`hold_for_a_person`]).
            if renewal == super::cli::Renewal::BotCheck {
                hold_for_a_person(
                    &parker,
                    &store,
                    key.clone(),
                    PendingCheck {
                        roster_path: roster_path.clone(),
                        row: raw_workflow.clone(),
                        generation: captured_logins
                            .get(&next_row.label)
                            .map(|binding| binding.renewal_generation),
                        at: crate::supervise::limit::unix_now().max(now),
                    },
                );
            }
            note_vendor_renewal_end(
                &parker,
                &publication_claims,
                &store,
                cli,
                (&key, &next_row.label),
                (&roster_path, &raw_workflow, &captured_logins),
                renewal,
            );
            completion.complete();
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
                    super::cli::Renewal::SigninHeld | super::cli::Renewal::Deferred => {}
                    _ => {
                        let _ = store.cancel_pin_request(request, now_done);
                    }
                }
            }
        },
        spawn_renewal,
    );
    if worker_scope
        .as_ref()
        .is_some_and(super::account_work::Scope::cancelled)
    {
        return RenewalDispatch::Held;
    }
    started.map_or(RenewalDispatch::Failed, |()| RenewalDispatch::Started)
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

    for accepted in std::iter::repeat_with(|| {
        aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Forever,
            aterm_uds::AcceptedMode::Blocking,
        )
    }) {
        let Ok((mut stream, _)) = accepted else {
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
                    &super::cli::LiveAccounts::default(),
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
                    None,
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
                let Some(cmd) = use_action(&params) else {
                    let _ = write_http(
                        &mut stream,
                        "400 Bad Request",
                        "text/plain; charset=utf-8",
                        b"missing account or auto=1",
                    );
                    continue;
                };
                let used = super::cli::run_accounts(
                    &cmd,
                    &fresh_env,
                    &super::cli::LiveAccounts::default(),
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
    let bound_unspecified = bound_host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<std::net::IpAddr>()
        .is_ok_and(|ip| ip.is_unspecified());
    let bound = format!("{bound_host}:{}", local.port());
    let localhost = format!("localhost:{}", local.port());
    if host_key != authority(&local.to_string())
        && (bound_unspecified || host_key != authority(&bound))
        && !(local.ip().is_loopback() && host_key == authority(&localhost))
    {
        return None;
    }
    if method == "POST" {
        if fetch_site.is_some_and(|site| {
            !site.eq_ignore_ascii_case("same-origin") && !site.eq_ignore_ascii_case("none")
        }) {
            return None;
        }
        match origin {
            Some(value)
                if value
                    .get(..7)
                    .is_some_and(|p| p.eq_ignore_ascii_case("http://"))
                    && authority(&value[7..]) == host_key => {}
            None if action == Some("1") => {}
            _ => return None,
        }
    }
    Some((method, target))
}

/// What a `POST /use` asks of `accounts use`, from its query: `auto=1` ends the pin, and
/// `account=<word>` pins the account the word names (the dashboard's Use form posts its key,
/// `ac-…`, which needs no escaping); a roster label names nothing. `None`: neither.
fn use_action(params: &BTreeMap<String, String>) -> Option<super::cli::AccountsCmd> {
    if params.get("auto").is_some_and(|v| v == "1") {
        return Some(super::cli::AccountsCmd::Auto);
    }
    params
        .get("account")
        .cloned()
        .map(|account| super::cli::AccountsCmd::Use { account })
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
        ..
    }) = report
    else {
        return "<tr><td colspan=\"6\">Pool state is unknown; see the report below.</td></tr>"
            .into();
    };
    let mut rows_html = String::new();
    // The sign-in of each row names it by the word the verb resolves (its slug, else its key; a
    // row with no address, which has no account, by its label), among the report's roster, which
    // holds the roster file's labels and addresses (`accounts::bound` changes only login folders).
    let lab = super::alab_roster::current();
    for row in &roster.rows {
        let here = row.dir.is_none() || enrolled.contains(&row.label);
        let cond = match accounts::condition(row, state, here, now) {
            accounts::Condition::Limited { until } => {
                format!("limited until {}", super::usage::rfc3339_utc(until))
            }
            other => other.as_str().to_owned(),
        } + &accounts::model_walls_clause(state.accounts.get(&row.label), now);
        let active_badge = if state.active.as_deref() == Some(row.label.as_str()) {
            if state.pinned { "pinned" } else { "active" }
        } else if state.active.is_none() && row.dir.is_none() {
            "default (in use)"
        } else {
            "-"
        };
        let login_cmd = Step::sign_in(
            Cli::Claude,
            super::account_names::sign_in_word(Cli::Claude, row, &roster.rows, lab.as_deref()),
            row.email.is_some(),
        )
        .to_string();
        // The pin posts the account's key, which `accounts use` takes; a row with no address has
        // no key, so no word pins it.
        let pin = super::account_names::names_of_row(Cli::Claude, row, None).map_or_else(
            || "no address".to_owned(),
            |(pk, _)| {
                format!(
                    "<form method=\"POST\" action=\"/use?account={}\" style=\"display:inline\">\
                     <button type=\"submit\">Use</button></form>",
                    html_escape(&pk)
                )
            },
        );
        rows_html.push_str(&format!(
            "<tr><td><code>{}</code></td><td>{}</td><td>{}</td><td>{}</td>\
             <td><code>{}</code></td><td>{pin}</td></tr>\n",
            html_escape(&row.label),
            html_escape(row.class.as_str()),
            html_escape(&cond),
            html_escape(active_badge),
            html_escape(&login_cmd),
        ));
    }

    rows_html
}

/// The dashboard's card for each other CLI's pool this machine keeps ([`cli_pools`]): its rows,
/// each with its vendor keys, condition and `--cli` sign-in ([`Cli::sign_in_verb`]); its pins and
/// sign-ins are its commands' (`aterm harness accounts use <account> --cli <id>`, `aterm harness
/// accounts renew|login <account> --cli <id>`, the row named by the word the verb takes,
/// [`super::account_names::sign_in_word`]), never a button here.
/// Nothing where this machine keeps none.
fn pools_html(pools: &[CliPool], now: i64) -> String {
    let lab = super::alab_roster::current();
    let mut out = String::new();
    for pool in pools {
        let product = pool.cli.connector().product;
        let id = pool.cli.id();
        let roster = html_escape(&pool.roster.display().to_string());
        let rows = match &pool.rows {
            Err(why) => format!("<p>The pool is refused: {}</p>", html_escape(why)),
            Ok(rows) if rows.rows.is_empty() => format!(
                "<p>No accounts listed: <code>aterm harness accounts add &lt;account&gt; --cli \
                 {id}</code></p>"
            ),
            Ok(rows) => {
                let mut body = String::new();
                for row in &rows.rows {
                    let here = row.dir.is_none() || pool.enrolled.contains(&row.label);
                    let cond =
                        match accounts::condition(row, &pool.state, here, now) {
                            accounts::Condition::Limited { until } => {
                                format!("limited until {}", super::usage::rfc3339_utc(until))
                            }
                            other => other.as_str().to_owned(),
                        } + &accounts::model_walls_clause(pool.state.accounts.get(&row.label), now);
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
                        html_escape(
                            super::account_names::sign_in_word(
                                pool.cli,
                                row,
                                &rows.rows,
                                lab.as_deref()
                            )
                            .as_str()
                        ),
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
             <code>aterm harness accounts use &lt;account&gt; --cli {id}</code></p>{rows}</div>"
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
#[path = "accounts_workflow_runtime_tests.rs"]
mod workflow_runtime_tests;

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountWorkflowRuntime",
        action = "AnotherCase",
        reason = "The native fixture selects fresh and retained guards, completion custody and replacement."
    )
)]
#[doc(hidden)]
pub fn account_workflow_runtime_environment() {}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountPublicationActivity",
        action = "Provider",
        reason = "The bounded native fixture selects one of the four account providers."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountPublicationActivity",
        action = "Case",
        reason = "The native fixture selects publication, replay, baseline and scope histories."
    )
)]
#[doc(hidden)]
pub fn account_publication_activity_environment() {}

#[cfg(test)]
#[path = "accounts_publication_activity_tests.rs"]
mod publication_activity_tests;

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountSignBackReceipt",
        action = "Operation",
        reason = "The fixture selects bound, unbound, or default in-place sign-in."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountSignBackReceipt",
        action = "Case",
        reason = "The fixture selects success, replay, invalidation, or claim history."
    )
)]
#[doc(hidden)]
pub fn account_signback_receipt_environment() {}

#[cfg(test)]
#[path = "accounts_signback_receipt_tests.rs"]
mod signback_receipt_tests;

#[cfg(test)]
#[path = "accounts_review_observation_tests.rs"]
mod review_observation_tests;

#[cfg(test)]
#[path = "accounts_recovery_lock_tests.rs"]
mod recovery_lock_tests;

// For shared adoption's recovery model, which runs only where aterm links the headless opener a
// first adoption needs (Unix).
#[cfg(all(test, unix))]
pub(crate) use recovery_lock_tests::{
    shared_checkpoint_alias_observation, shared_checkpoint_observation,
};

#[cfg(test)]
#[path = "accounts_completion_identity_tests.rs"]
mod completion_identity_tests;

#[cfg(test)]
#[path = "accounts_sync_rename_tests.rs"]
mod rename_tests;

#[cfg(test)]
#[path = "accounts_sync_trace_tests.rs"]
mod trace_tests;

#[cfg(test)]
#[path = "accounts_no_sweep_tests.rs"]
mod no_sweep_tests;

#[cfg(test)]
#[path = "accounts_sync_renewal_bounds_tests.rs"]
mod renewal_bounds_tests;

#[cfg(test)]
#[path = "accounts_alias_relabel_tests.rs"]
mod alias_relabel_tests;

#[cfg(test)]
#[path = "accounts_sync_report_tests.rs"]
mod sync_report_tests;

#[cfg(test)]
#[path = "accounts_sync_default_login_tests.rs"]
mod default_login_tests;

#[cfg(test)]
#[path = "accounts_dashboard_tests.rs"]
mod dashboard_tests;

#[cfg(test)]
#[path = "accounts_renewal_read_tests.rs"]
mod renewal_read_tests;

/// A login made in a tab nobody supervises is adopted by the upkeep (plan package FL-174).
#[cfg(test)]
#[path = "accounts_sync_login_adopted_tests.rs"]
mod login_adopted_tests;

#[cfg(test)]
#[path = "accounts_shared_selection_tests.rs"]
mod shared_selection_tests;

#[cfg(test)]
#[path = "accounts_auto_shared_tests.rs"]
mod automatic_shared_tests;

/// Keep the advisory recovery transition reachable in source/spec closure checks.
#[doc(hidden)]
pub fn account_shared_recovery_environment() {
    std::hint::black_box(super::cli::shared_renewal::recover_consumed as *const ());
    std::hint::black_box(super::cli::shared_adoption::adopt as *const ());
    std::hint::black_box(spawn_row_renewal as *const ());
    std::hint::black_box(shared_renewal::adopt_first as *const ());
}

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
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountRenewalRead",
        action = "LoginEvidence",
        reason = "Native fixture selection of usage-tap evidence newer or not newer than the sign-out."
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
#[path = "accounts_sync_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "accounts_sync_cli_tests.rs"]
mod cli_pool_tests;

#[cfg(test)]
#[path = "accounts_sync_fleet_publication_tests.rs"]
mod fleet_publication_tests;

#[cfg(test)]
#[path = "accounts_sync_codex_identity_tests.rs"]
mod codex_default_identity_tests;

#[cfg(test)]
#[path = "accounts_sync_alab_tests.rs"]
mod alab_tests;

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
#[path = "accounts_sync_retry_tests.rs"]
mod retry_tests;

#[cfg(test)]
#[path = "accounts_source_path_tests.rs"]
mod source_path_tests;

#[cfg(test)]
#[path = "accounts_sync_ladder_tests.rs"]
pub(in crate::harness) mod ladder_tests;

#[cfg(test)]
#[path = "accounts_browser_rehabilitation_tests.rs"]
mod browser_rehabilitation_tests;

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
#[path = "accounts_sync_label_swap_tests.rs"]
mod label_swap_tests;
#[cfg(all(test, unix))]
#[path = "accounts_sync_vault_sheet_tests.rs"]
mod vault_sheet_tests;

#[cfg(all(test, windows))]
#[path = "accounts_sync_host_win_tests.rs"]
mod host_win_tests;

#[cfg(test)]
#[path = "accounts_sync_lab_cell_tests.rs"]
mod lab_cell_tests;

#[cfg(all(test, unix))]
#[path = "accounts_hardening_tests.rs"]
mod hardening_tests;

#[cfg(all(test, unix))]
#[path = "accounts_sync_config_tests.rs"]
mod config_admission_tests;

#[cfg(all(test, unix))]
#[path = "accounts_capture_owner_tests.rs"]
mod capture_owner_tests;

#[cfg(all(test, unix))]
#[path = "accounts_source_preservation_tests.rs"]
mod source_preservation_tests;

#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "AccountSourcePreservation",
    action = "Source",
    reason = "The environment supplies a new discovery, external fleet, legacy copy or sheet."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "AccountSourcePreservation",
    action = "Cut",
    reason = "The environment refuses preparation or interrupts a durable receipt before replay."
)]
pub fn account_source_preservation_environment() {}
