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
        let mut out = String::from(if opt_in == Some(OptIn::Discovered) {
            "# Found on this Mac by aterm: the accounts its own logins, profiles and transcripts\n\
             # show. No person named a source for them, and aterm adds each account it finds\n\
             # later. `aterm harness accounts sync <path-or-url>` replaces it with a fleet you name.\n\n"
        } else {
            "# Managed by `aterm harness accounts sync` — shared LAN fleet account roster.\n\
             # Run `aterm harness accounts sync <path-or-url>` on any machine to materialize\n\
             # its local `accounts.toml`.\n\n"
        });
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
    /// or any row is missing required fields or carries a refused label or email.
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
            let machines: Vec<String> = match row.get("machines") {
                Some(aterm_toml::Value::Array(arr)) => arr
                    .iter()
                    .filter_map(aterm_toml::Value::as_str)
                    .filter_map(normalize_machine_id)
                    .collect(),
                Some(aterm_toml::Value::String(s)) => normalize_machine_id(s).into_iter().collect(),
                _ => Vec::new(),
            };
            if machines.is_empty() {
                return Err(format!(
                    "fleet roster row {} ({label}): missing `machines`",
                    idx + 1
                ));
            }
            let class = row
                .get("class")
                .and_then(aterm_toml::Value::as_str)
                .and_then(Class::parse)
                .unwrap_or(if machines.len() > 1 {
                    Class::Shared
                } else {
                    Class::Dedicated
                });
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

fn csv_records(mut text: &str) -> impl Iterator<Item = Result<&str, &'static str>> {
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

fn parse_csv_line(line: &str) -> Vec<String> {
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
/// Returns an error if the CSV has no header row, no `email` / `claude` columns, or malformed
/// quoting. LF and CRLF separate records only outside quotes.
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
/// `claude`; `Open AI` (or `openai`, `codex`) for Codex; `xai` (or `grok`) for Grok Build;
/// `kimi` for Kimi Code.
#[must_use]
pub const fn csv_columns(cli: Cli) -> &'static [&'static str] {
    match cli {
        Cli::Claude => &["claude"],
        Cli::Codex => &["open ai", "openai", "codex"],
        Cli::Grok => &["xai", "grok"],
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

/// Whether the sheet's header names `cli`'s column ([`csv_columns`]).
#[must_use]
pub fn csv_names_column(cli: Cli, csv_text: &str) -> bool {
    csv_records(csv_text)
        .next()
        .and_then(Result::ok)
        .is_some_and(|header| member_column(cli, &parse_csv_line(header)).is_some())
}

fn member_column(cli: Cli, header: &[String]) -> Option<usize> {
    header.iter().position(|h| {
        csv_columns(cli)
            .iter()
            .any(|word| h.trim().eq_ignore_ascii_case(word))
    })
}

/// What a row's cell in a CLI's column says of its membership ([`parse_sheet`]).
enum Member {
    /// Not a member.
    No,
    /// A member, with the vendor keys the cell names.
    Yes(accounts::Vendor),
    /// A member the pool cannot take as listed ([`CliFleet::held`]).
    Held,
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
    let email_col = header
        .iter()
        .position(|h| h.trim().eq_ignore_ascii_case("email"))
        .ok_or_else(|| format!("CSV at {source} has no `email` column"))?;
    let member_col = member_column(cli, &header).ok_or_else(|| {
        format!(
            "CSV at {source} has no `{}` column",
            csv_columns(cli).first().copied().unwrap_or("claude")
        )
    })?;
    let claude_col = member_column(Cli::Claude, &header).unwrap_or(member_col);
    let mut computer_cols: Vec<usize> = header
        .iter()
        .enumerate()
        .filter(|(_, h)| h.trim().eq_ignore_ascii_case("computer"))
        .map(|(i, _)| i)
        .collect();
    if let Some(&first_comp) = computer_cols.first()
        && first_comp + 1 < claude_col
        && header
            .get(first_comp + 1)
            .is_some_and(|h| h.trim().is_empty())
        && !computer_cols.contains(&(first_comp + 1))
    {
        computer_cols.push(first_comp + 1);
    }
    let notes_start = header
        .iter()
        .position(|h| h.trim().eq_ignore_ascii_case("notes"))
        .unwrap_or(header.len());

    let mut used_labels = BTreeSet::new();
    let mut seen_emails = BTreeSet::new();
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
        if matches!(member, Member::No) {
            continue;
        }
        let notes = if notes_start < cols.len() {
            cols[notes_start..].join(" ")
        } else {
            String::new()
        };
        let disqualified = match cli {
            Cli::Claude => notes_disqualify(&notes),
            _ => notes_disqualify_for(cli, &notes),
        };
        if disqualified {
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

        let email_lower = email.to_ascii_lowercase();
        if !seen_emails.insert(email_lower) {
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

/// Detect this machine's canonical ID from an override, config file, or system hostname.
#[must_use]
pub fn detect_machine_id(override_machine: Option<&str>, config: Option<&Path>) -> Option<String> {
    if let Some(m) = override_machine {
        return normalize_machine_id(m);
    }
    if let Some(m) = config_machine_id(config) {
        return Some(m);
    }
    for (prog, args) in [
        ("/usr/sbin/scutil", &["--get", "LocalHostName"][..]),
        ("/usr/sbin/scutil", &["--get", "ComputerName"][..]),
        ("/bin/hostname", &[][..]),
    ] {
        let mut cmd = aterm_uds::spawnfd::command(prog);
        cmd.args(args);
        if let Ok(cap) = super::align::capture_bounded(cmd, Duration::from_secs(2), 256)
            && cap.code == Some(0)
            && let Ok(s) = std::str::from_utf8(&cap.stdout)
            && let Some(m) = normalize_machine_id(s)
        {
            return Some(m);
        }
    }
    None
}

fn read_default_profile_email(env: &Env) -> Option<String> {
    let path = env.claude_profile.as_deref().map(|own| {
        super::profile::effective(own, env.claude_dir().as_deref(), env.claude_custom_oauth)
    })?;
    let text = read_local_source_text(&path).ok()?;
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

/// Scan recent transcripts and attribute standing quota walls to active account rows.
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

    let default_label = roster.row_for_login(None).map(|r| r.label.clone());
    let mut newest_wall: BTreeMap<String, upgrade::StandingWall> = BTreeMap::new();
    let mut newest_answer: BTreeMap<String, u64> = BTreeMap::new();

    for (_, path) in files_with_mtime {
        let Ok(f) = fs::File::open(&path) else {
            continue;
        };
        let mut reader = io::BufReader::new(f);
        let mut current_label: Option<String> = default_label.clone();
        let mut line = String::new();
        while reader.read_line(&mut line).ok().is_some_and(|n| n > 0) {
            if line.contains("\"session_context\"") && line.contains("\"userEmail\"") {
                if let Some(matched) = match_session_context_label(&line, roster) {
                    current_label = matched;
                }
            } else if line.contains("<local-command-stdout>Login successful</local-command-stdout>")
            {
                current_label = None;
            } else if line.contains("\"assistant\"")
                && let Some(label) = current_label.as_deref()
            {
                if line.contains("\"quotaLimits\"") && line.contains("\"rejected\"") {
                    if let Some(wall) = upgrade::transcript_standing_wall(&line) {
                        let until = wall.until.and_then(|u| i64::try_from(u).ok());
                        if until.is_some_and(|u| u > now)
                            && newest_wall.get(label).is_none_or(|prev| wall.at > prev.at)
                        {
                            newest_wall.insert(label.to_owned(), wall);
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
                    let entry = newest_answer.entry(label.to_owned()).or_insert(0);
                    *entry = (*entry).max(ans_at);
                }
            }
            line.clear();
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

/// Build the machine's [`Roster`] from `fleet` for `machine`. The row whose email is
/// `default_email` is the default login's (no `dir`); where none of `machine`'s rows has that
/// email, the default login holds an account they do not list, and the roster lists it too, as
/// the default login's row: the fleet's own row for it where another machine's lists it, else
/// the label `existing` already gives that account where the fleet gives it to no account, else
/// a new one ([`local_account`], unique among the fleet's labels and `existing`'s), so no label
/// is kept on an account it is not and an account keeps its label while it can.
#[must_use]
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
    let default_email = default_email.map(str::trim).filter(|de| !de.is_empty());
    let matches_identity = |acct: &FleetAccount, email: &str| {
        let expected = existing
            .and_then(|roster| roster.row(&acct.label))
            .filter(|row| {
                row.email
                    .as_deref()
                    .is_some_and(|login| accounts::email_matches(login, &acct.email))
            })
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
                    let kept = existing
                        .and_then(|ex| {
                            ex.rows.iter().find(|r| {
                                r.expected_identity_email()
                                    .is_some_and(|re| accounts::email_matches(re, de))
                            })
                        })
                        .filter(|row| !used.contains(&row.label));
                    if let Some(kept) = kept {
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
                    local_account(de, machine, &used)
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
            .or_else(|| existing_row.and_then(|r| r.browser.clone()));
        rows.push(Row {
            label: acct.label.clone(),
            class: acct.class,
            dir,
            email: Some(acct.email.clone()),
            identity_email: existing_row
                .filter(|row| {
                    row.email
                        .as_deref()
                        .is_some_and(|email| accounts::email_matches(email, &acct.email))
                })
                .and_then(|row| row.identity_email.clone()),
            browser,
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
/// where `accounts add` lists a row so that the next sync keeps it ([`fleet_origin`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FleetOrigin {
    /// No fleet file and no source: `accounts.toml` is the list, as it was written.
    Roster,
    /// Fleet files on this machine that a sync reads and writes back as they hold: the one
    /// beside the roster, and a copy in `~/.secrets` that a sync from init reads first and that
    /// holds this machine's rows alone ([`fleet_of_this_machine`]). `opt_in` is the word they
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
    /// The vault's sheet or fleet in `~/.secrets` (other machines' rows among them), which this
    /// machine reads and does not write: the vault's subscriber writes it.
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
            Self::Vault(_) => format!("the vault's {source}"),
        }
    }
}

/// [`FleetOrigin`] of the roster at `roster`, for `home` and the config file `config`.
#[must_use]
pub fn fleet_origin(home: Option<&Path>, config: Option<&Path>, roster: &Path) -> FleetOrigin {
    let beside = fleet_file_beside(roster).filter(|fleet| fleet.is_file());
    let beside_opt_in = beside.as_deref().and_then(fleet_file_opt_in);
    let beside_opts_in = beside_opt_in.is_some_and(OptIn::opts_in_by_file);
    if !beside_opts_in && let Some(source) = config.and_then(source_key) {
        return FleetOrigin::Key(source);
    }
    let secrets = home.map(|home| home.join(".secrets"));
    if let Some(vault) = secrets
        .as_ref()
        .map(|secrets| secrets.join(FLEET_ROSTER_FILE))
        .filter(|vault| vault.is_file())
    {
        let read = read_local_source_text(&vault).ok().and_then(|text| {
            let fleet = FleetRoster::from_toml(&text, &vault.display().to_string()).ok()?;
            Some((opt_in_of(&text), fleet))
        });
        return match read {
            Some((opt_in, fleet)) if fleet_of_this_machine(opt_in, &fleet) => {
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
    if let Some(sheet) = secrets
        .map(|secrets| secrets.join(ACCOUNTS_CSV_FILE))
        .filter(|sheet| sheet.is_file())
    {
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

/// Whether a fleet roster file stands beside `roster` without opting in ([`configured_source`]).
#[must_use]
pub fn fleet_file_ignored(roster: &Path) -> bool {
    fleet_file_beside(roster).is_some_and(|fleet| {
        fleet.is_file() && !fleet_file_opt_in(&fleet).is_some_and(OptIn::opts_in_by_file)
    })
}

fn fleet_file_beside(roster: &Path) -> Option<PathBuf> {
    roster.parent().map(|dir| dir.join(FLEET_ROSTER_FILE))
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
/// writes (`accounts add` lists a row there): a fleet discovery made ([`OptIn::Discovered`]), or
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

/// Load a [`FleetRoster`] from `source`, or from [`configured_source`] when `source` is `None`.
/// # Errors
/// No source, or the source could not be read or parsed.
pub fn load_fleet_source(env: &Env, source: Option<&str>) -> Result<FleetRoster, String> {
    load_fleet_source_with(env, source, &fetch_source_text).map(|(fleet, _)| fleet)
}

fn load_fleet_source_with(
    env: &Env,
    source: Option<&str>,
    fetch: &Fetch<'_>,
) -> Result<(FleetRoster, OptIn), String> {
    let (chosen, opt_in) = match source.map(str::trim).filter(|s| !s.is_empty()) {
        Some(named) => (named.to_owned(), OptIn::Named),
        None => configured_source(env.config.as_deref(), env.accounts_roster().as_deref())
            .map(|configured| {
                let opt_in = match configured {
                    FleetSource::File(_) => OptIn::Named,
                    FleetSource::Key(_) => OptIn::Key,
                };
                (configured.source(), opt_in)
            })
            .ok_or_else(|| NO_SOURCE.to_owned())?,
    };
    let raw = fetch(env, &chosen)?;
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
    Ok((fleet, opt_in))
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

/// The fleet discovery makes of what `shown` holds ([`Shown::all`]) for `env`'s machine, where
/// it lists at least `min_accounts`.
fn discovered_fleet(env: &Env, shown: &Shown, min_accounts: usize) -> Option<FleetRoster> {
    let emails = shown.all();
    if emails.is_empty() {
        return None;
    }
    let email_hint_machine = || {
        read_default_profile_email(env)
            .as_deref()
            .and_then(|de| machines_from_email_hint(de).into_iter().next())
            .or_else(|| {
                emails
                    .iter()
                    .find_map(|em| machines_from_email_hint(em).into_iter().next())
            })
    };
    let fallback_machine = config_machine_id(env.config.as_deref())
        .or_else(|| {
            detect_machine_id(None, None).filter(|host_m| {
                emails.iter().any(|em| {
                    let (machines, _) = email_machines_and_slot(em);
                    machines.is_empty() || machines.contains(host_m)
                }) || email_hint_machine().is_none()
            })
        })
        .or_else(email_hint_machine)?;
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
    if bytes.len() > SOURCE_TEXT_MAX_BYTES {
        return Err(format!(
            "account source at {source} exceeds the {SOURCE_TEXT_MAX_BYTES}-byte limit"
        ));
    }
    String::from_utf8(bytes)
        .map_err(|e| format!("account source at {source} is not valid UTF-8: {e}"))
}

fn read_local_source_text(path: &Path) -> io::Result<String> {
    let file = astream_sys::open_nonblocking(path, astream_sys::Links::Refuse)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "local account source is not a regular file",
        ));
    }
    if metadata.len() > SOURCE_TEXT_MAX_BYTES as u64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "local account source exceeds the byte limit",
        ));
    }
    let mut bytes = Vec::new();
    file.take((SOURCE_TEXT_MAX_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    decode_source_text(bytes, &path.to_string_lossy())
        .map_err(|why| io::Error::new(io::ErrorKind::InvalidData, why))
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum FetchPlan {
    Http(String),
    File(PathBuf),
}

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
    Ok(match (source.strip_prefix("~/"), env.home.as_deref()) {
        (Some(rest), Some(home)) => FetchPlan::File(home.join(rest)),
        _ => FetchPlan::File(PathBuf::from(source)),
    })
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
        FetchPlan::File(path) => {
            let mut bytes = Vec::new();
            fs::File::open(&path)
                .and_then(|file| {
                    file.take((SOURCE_TEXT_MAX_BYTES + 1) as u64)
                        .read_to_end(&mut bytes)
                })
                .map_err(|e| format!("could not read {}: {e}", path.display()))?;
            decode_source_text(bytes, source)
        }
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
    state: accounts::State,
    note: Option<String>,
    /// Whether the sync changed anything: a file whose bytes differ from what it held (each is
    /// written only then), account state or logins, or a ledger row.
    changed: bool,
}

struct FleetSnapshot {
    path: PathBuf,
    bytes: Option<Vec<u8>>,
    // Source merges and initial seeds publish under the same lock as admission.
    publish: bool,
}

fn sync_lock_ladder() -> Vec<Duration> {
    accounts::LOCK_LADDER
        .iter()
        .copied()
        .chain(std::iter::repeat_n(Duration::from_millis(500), 16))
        .collect()
}

fn persist_accounts_sync(
    store: &accounts::Store,
    roster_path: &Path,
    (fleet, opt_in): (&FleetRoster, OptIn),
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
    let (write_to, keep_mode) = super::cli::roster_target(roster_path)?;
    let existing = Roster::load(&write_to).map_err(|why| {
        format!(
            "the roster at {} is refused: {why}; the roster is unchanged",
            roster_path.display()
        )
    })?;
    let roster = build_machine_roster(fleet, machine, store, Some(&existing), default_email);
    if roster.rows.is_empty() {
        return Err(format!(
            "no active Claude accounts found for machine `{machine}` in {}",
            fleet.source
        ));
    }
    let roster_toml = format_roster_toml(&roster)
        .map_err(|why| format!("generated roster refused admission: {why}"))?;
    let (fleet_write_to, fleet_mode) = super::cli::roster_target(&fleet_path)?;
    let fleet_toml = fleet.to_toml_for(Some(opt_in));
    let default_row_label = roster.row_for_login(None).map(|r| r.label.clone());
    // The labels the fleet gave another account than the one the roster gave them: what is on
    // record under each is the account it held before, never the one it holds now.
    let relabelled: Vec<String> = roster
        .rows
        .iter()
        .filter(|row| {
            existing
                .row(&row.label)
                .and_then(|was| was.email.as_deref())
                .zip(row.email.as_deref())
                .is_some_and(|(was, now)| !accounts::email_matches(was, now))
        })
        .map(|row| row.label.clone())
        .collect();
    // The default login's account under a new label because the fleet gave its old one to
    // another account: its record moves with it, and nothing was written over.
    let default_moved = existing.row_for_login(None).and_then(|was| {
        let now_row = roster.row_for_login(None)?;
        (relabelled.contains(&was.label)
            && now_row.label != was.label
            && existing.row(&now_row.label).is_none()
            && was
                .email
                .as_deref()
                .zip(now_row.email.as_deref())
                .is_some_and(|(was, now)| accounts::email_matches(was, now)))
        .then(|| (was.label.clone(), now_row.label.clone()))
    });
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
    let mut note = None;
    let mut publishing = false;
    // A sync that finds what is already there writes nothing: each file is replaced only where
    // its bytes differ, so a periodic caller rewrites no file and wakes no watcher.
    let mut changed = false;
    let mut publish_if_changed = |path: &Path, held: Option<&[u8]>, text: &str, mode| {
        if held == Some(text.as_bytes()) {
            return Ok(());
        }
        changed = true;
        super::upgrade_models::write_atomic_mode(path, text, mode)
            .map_err(|why| format!("could not write {}: {why}", path.display()))
    };
    let roster_held = fs::read(&write_to).ok();
    let fleet_held = fs::read(&fleet_write_to).ok();
    let mut state_changed = false;
    let events = store
        .transact_logins_held(&held, now, &mut note, |state, logins| {
            publishing = true;
            let before = (state.clone(), logins.clone());
            for label in &relabelled {
                match &default_moved {
                    Some((from, to)) if from == label => {
                        accounts::move_account(state, logins, from, to);
                    }
                    _ => accounts::forget_account(state, logins, label),
                }
            }
            for row in &roster.rows {
                if let Some(dir) = &row.dir {
                    aterm_types::fs_restricted::ensure_private_dir(&store.logins_dir())
                        .and_then(|()| {
                            aterm_types::fs_restricted::ensure_private_dir(Path::new(dir))
                        })
                        .map_err(|why| format!("could not create {dir}: {why}"))?;
                }
            }
            publish_if_changed(&write_to, roster_held.as_deref(), &roster_toml, keep_mode)?;
            publish_if_changed(
                &fleet_write_to,
                fleet_held.as_deref(),
                &fleet_toml,
                fleet_mode,
            )?;
            for expected in fleet_snapshots.iter().filter(|snapshot| snapshot.publish) {
                publish_if_changed(
                    &expected.path,
                    expected.bytes.as_deref(),
                    &fleet_toml,
                    Some(0o600),
                )?;
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
                if default_matched && unseen_default {
                    accounts::note_served_login(logins, label, now);
                    events.extend(accounts::note_auth(state, label, accounts::Auth::Ok, now));
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
            state_changed = (&*state, &*logins) != (&before.0, &before.1);
            Ok(events)
        })
        .map_err(|why| {
            if publishing {
                format!(
                    "sync publication did not complete: {why}; roster, fleet, login directories \
                     or account state may already have changed; inspect them before syncing again"
                )
            } else {
                format!("sync refused before roster publication: {why}")
            }
        })?;
    let changed = changed || state_changed || !events.is_empty();
    if changed {
        store.touch_wake().map_err(|why| {
            format!(
                "sync files were published but the wake marker could not be written: {why}; \
                 the published files are kept"
            )
        })?;
    }
    let (state, read) = store.load(now).map_err(|why| {
        format!(
            "sync files were published but their state could not be read: {why}; \
             the published files are kept"
        )
    })?;
    if note.is_none() {
        note = read;
    }
    Ok(SyncedAccounts {
        fleet_path,
        state,
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
    };
    run_accounts_sync_with(env, (source, machine_override, json), (out, err), &how)
}

struct SyncHow<'a> {
    fetch: &'a Fetch<'a>,
    ladder: &'a [Duration],
    fleet_snapshots: &'a [FleetSnapshot],
}

fn run_accounts_sync_with(
    env: &Env,
    (source, machine_override, json): (Option<&str>, Option<&str>, bool),
    (out, err): (&mut dyn Write, &mut dyn Write),
    how: &SyncHow<'_>,
) -> ExitCode {
    let Some(roster_path) = env.accounts_roster() else {
        let _ = writeln!(
            err,
            "aterm harness accounts sync: no config directory is known (set HOME or XDG_CONFIG_HOME)"
        );
        return ExitCode::FAILURE;
    };
    let (fleet, opt_in) = match load_fleet_source_with(env, source, how.fetch) {
        Ok(f) => f,
        Err(why) => {
            let _ = writeln!(err, "aterm harness accounts sync: {why}");
            return ExitCode::FAILURE;
        }
    };
    let default_email = read_default_profile_email(env);
    let Some(machine) = sync_machine(env, &fleet, default_email.as_deref(), machine_override)
    else {
        let _ = writeln!(
            err,
            "aterm harness accounts sync: could not determine machine id (`m<N>`); pass `--machine m8`"
        );
        return ExitCode::FAILURE;
    };

    let store = env.accounts_store();
    let prelim_roster =
        build_machine_roster(&fleet, &machine, &store, None, default_email.as_deref());
    let standing_walls = env
        .claude_dir()
        .map(|dir| find_transcript_walls_by_email(&dir, &prelim_roster, env.now))
        .unwrap_or_default();
    let SyncedAccounts {
        fleet_path,
        state,
        note,
        changed,
    } = match persist_accounts_sync(
        &store,
        &roster_path,
        (&fleet, opt_in),
        (&machine, default_email.as_deref()),
        &standing_walls,
        env.now,
        (how.ladder, how.fleet_snapshots),
    ) {
        Ok(saved) => saved,
        Err(why) => {
            let _ = writeln!(err, "aterm harness accounts sync: {why}");
            return ExitCode::FAILURE;
        }
    };
    if let Some(note) = note {
        let _ = writeln!(err, "aterm harness accounts: {note}");
    }
    let recovered = recover_interrupted_staging_renewals_for(
        Cli::Claude,
        &store,
        &roster_path,
        &AUTO_RENEW_GUARD,
        env.now,
    );
    let roster = match Roster::load(&roster_path) {
        Ok(roster) => roster,
        Err(why) => {
            let _ = writeln!(err, "aterm harness accounts sync: {why}");
            return ExitCode::FAILURE;
        }
    };
    let state = if recovered {
        store.load(env.now).map(|(s, _)| s).unwrap_or(state)
    } else {
        state
    };
    let bound = store
        .bound(&roster, env.now)
        .unwrap_or_else(|_| roster.clone());
    let enrolled = accounts::enrolled(&bound);
    let needs_renew: Vec<String> = roster
        .rows
        .iter()
        .filter(|r| {
            r.dir.is_some()
                && !enrolled.contains(&r.label)
                && !state
                    .accounts
                    .get(&r.label)
                    .is_some_and(|a| a.limited_until(env.now).is_some())
        })
        .map(|r| r.label.clone())
        .collect();

    if json {
        let mut o = Map::new();
        o.insert("schema".to_owned(), Value::from(1u64));
        o.insert("kind".to_owned(), Value::from("accounts-sync".to_owned()));
        o.insert("machine".to_owned(), Value::from(machine));
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
            Value::from(u64::try_from(fleet.accounts.len()).unwrap_or(0)),
        );
        o.insert("changed".to_owned(), Value::from(changed));
        o.insert(
            "machine_accounts".to_owned(),
            Value::Array(
                roster
                    .rows
                    .iter()
                    .map(|r| {
                        let here = r.dir.is_none() || enrolled.contains(&r.label);
                        let cond = accounts::condition(r, &state, here, env.now);
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
            Value::Array(needs_renew.into_iter().map(Value::from).collect()),
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
            fleet.accounts.len(),
            if changed {
                ""
            } else {
                "; nothing changed, so no file was written"
            }
        );
        let _ = writeln!(out, "roster: {}", roster_path.display());
        let _ = writeln!(out, "fleet:  {}", fleet_path.display());
        for row in &roster.rows {
            let here = row.dir.is_none() || enrolled.contains(&row.label);
            let cond = match accounts::condition(row, &state, here, env.now) {
                accounts::Condition::Limited { until } => {
                    format!("limited until {}", super::usage::rfc3339_utc(until))
                }
                accounts::Condition::NotEnrolled => match state
                    .accounts
                    .get(&row.label)
                    .and_then(|a| a.limited_until(env.now))
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
    let email_machine = || {
        default_email.and_then(|de| {
            fleet
                .accounts
                .iter()
                .find(|a| accounts::email_matches(&a.email, de))
                .and_then(|a| a.machines.first().cloned())
        })
    };
    machine_override
        .and_then(normalize_machine_id)
        .or_else(|| config_machine_id(env.config.as_deref()))
        .or_else(|| {
            detect_machine_id(None, None)
                .filter(|m| !fleet.accounts_for_machine(m).is_empty() || email_machine().is_none())
                .or_else(email_machine)
        })
}

/// The text each fleet file of `files` ([`FleetOrigin::Local`]) holds with `row` listed as one
/// more account of this machine ([`sync_machine`]), which the next sync then keeps; a file that
/// lists it already is left out. Nothing is written here: `accounts add` writes them, under the
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

/// How often a window's upkeep reads this machine's logins and transcripts again for accounts a
/// fleet discovery made does not list yet ([`init_sync_needed`]).
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
    /// The local file `[fleet] accounts_source` names and its modification time, as last synced
    /// from ([`key_file_changed`]).
    key_seen: Option<(PathBuf, std::time::SystemTime)>,
    /// The addresses a fleet discovery made lists only for a credential, and when they were
    /// read ([`unclaimed_emails`]).
    unclaimed: Option<(Instant, BTreeSet<String>)>,
}

static REDISCOVERY: std::sync::Mutex<Rediscovery> = std::sync::Mutex::new(Rediscovery {
    read_at: None,
    synced_for: None,
    key_seen: None,
    unclaimed: None,
});

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
        .and_then(|row| row.email.as_deref())
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
    sync_cooldown: &'a std::sync::Mutex<Option<Instant>>,
    /// When the upkeep last caught its roster up with this machine's own logins
    /// ([`rediscover_due`], [`default_login_unlisted`]).
    rediscovery: &'a std::sync::Mutex<Rediscovery>,
    renew_guard: &'a std::sync::Arc<std::sync::Mutex<AutoRenewGuard>>,
    /// Whether a headless-capable browser is installed for `home` ([`browser_installed`]).
    find_browser: &'a (dyn Fn(&Path) -> bool + Sync),
    /// One bounded `aterm secrets sync` for `home` ([`run_vault_sync`]).
    vault_sync: fn(&Path) -> super::sign_in_ladder::Vault,
    /// Where that sync runs, off the caller's thread ([`spawn_vault_thread`]).
    spawn_vault: &'a SpawnRenewal,
    /// What the configured mailbox offers a row's sign-in ([`ladder_mailbox_live`]).
    mailbox: &'a LadderMailbox,
    /// Where each automatic sign-in's one line goes ([`log_sign_in`]).
    log: SignInLog,
    /// One run of `claude auth login` for a route, where it is not the live one
    /// ([`spawn_row_renewal`]).
    run_attempt: Option<RunAttempt>,
}

type SpawnRenewal = dyn Fn(Box<dyn FnOnce() + Send>) -> io::Result<()> + Sync;

/// [`UpkeepEffects::mailbox`]: what the configured mailbox offers `row` for `home` and `store`.
type LadderMailbox =
    dyn Fn(&Path, &accounts::Store, &accounts::Row) -> super::sign_in_ladder::Mailbox + Sync;

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
    cmd.args(["secrets", "sync"]).env("HOME", home);
    match super::align::capture_bounded(cmd, VAULT_SYNC_BUDGET, 16 * 1024) {
        Ok(ran) => Vault::of_sync(&String::from_utf8_lossy(&ran.stdout), ran.code),
        Err(why) => Vault::of_run_error(&why, VAULT_SYNC_BUDGET),
    }
}

/// The live [`UpkeepEffects::mailbox`]: the mailbox `row`'s sign-in reads, from the process's
/// environment ([`super::open_url::MAILBOX_ENV`] and its kin) and the mailbox files.
fn ladder_mailbox_live(
    home: &Path,
    store: &accounts::Store,
    row: &accounts::Row,
) -> super::sign_in_ladder::Mailbox {
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
) -> super::sign_in_ladder::Mailbox {
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

/// What a mailbox read offers `row`: a mailbox folder (`dir_found`) or a configuration that
/// reads its inbox over a folder, HTTP or IMAP is ready; one that names only a webmail inbox
/// needs a headless browser; none for the row is none.
fn mailbox_offer(
    dir_found: bool,
    cfg: Option<super::headless_oauth::MailboxConfig>,
    row: &accounts::Row,
) -> super::sign_in_ladder::Mailbox {
    use super::sign_in_ladder::Mailbox;
    if dir_found {
        return Mailbox::Ready;
    }
    let email = row.email.as_deref();
    match cfg {
        Some(mut cfg) => {
            cfg.alias_label_to_email(Some(&row.label), email);
            if !cfg.is_configured_for(email) {
                Mailbox::None
            } else if cfg.dir.is_some()
                || cfg.http_url.is_some()
                || cfg.effective_imap_url(email).is_some()
            {
                Mailbox::Ready
            } else {
                Mailbox::WebmailOnly
            }
        }
        None => Mailbox::None,
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
        sync_cooldown: &AUTO_SYNC_COOLDOWN,
        rediscovery: &REDISCOVERY,
        renew_guard: &AUTO_RENEW_GUARD,
        find_browser: &browser_installed,
        vault_sync: run_vault_sync,
        spawn_vault: &spawn_vault_thread,
        mailbox: &ladder_mailbox_live,
        log: std::sync::Arc::new(log_sign_in),
        run_attempt: None,
    }
}

/// Claude Code's pool's upkeep ([`upkeep_claude`]), then the other CLIs' ([`upkeep_pools`]),
/// which the result does not report.
fn upkeep(
    home: &Path,
    state_dir: &Path,
    (roster_path, now, step): (&Path, i64, UpkeepStep),
    fx: &UpkeepEffects<'_>,
) -> Upkept {
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
    if step.syncs() && sync_called_for(home, state_dir, (roster_path, &config), now, fx.rediscovery)
    {
        let _ = sync_accounts_for_init_with(home, state_dir, roster_path, now, || {}, fx.fetch);
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
    let roster = if step.syncs() {
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
/// (`[harness] enabled`) and its login lifecycle on (`[harness] logins`), as both are unless a
/// person turned one off.
fn sign_ins_on(config: &Path) -> bool {
    let (cfg, _) = crate::supervise::config::SupervisorConfig::from_path(Some(config));
    cfg.enabled && cfg.logins
}

/// THE ONE RULE OF WHEN A PERIODIC CALLER SYNCS a roster that stands (a window's upkeep, the
/// setup pass's [`resync_for_setup`]): when something it is made from changed. That is
/// `~/.secrets` holding a fleet, a sheet or a mailbox where no source is configured yet; a vault
/// fleet or sheet the fleet file does not match, or accounts this machine's own logins show that
/// a fleet discovery made does not list ([`init_sync_needed`], read every [`REDISCOVER_EVERY`]);
/// the default login holding an account the roster's default row is not
/// ([`default_login_unlisted`]); or a `[fleet] accounts_source` that is a local file this process
/// has not synced from as it stands ([`key_file_changed`]). A sync that runs and finds nothing new
/// writes nothing ([`persist_accounts_sync`]).
fn sync_called_for(
    home: &Path,
    state_dir: &Path,
    (roster_path, config): (&Path, &Path),
    now: i64,
    rediscovery: &std::sync::Mutex<Rediscovery>,
) -> bool {
    let secrets = home.join(".secrets");
    let configured = configured_source(Some(config), Some(roster_path));
    let unconfigured_with_secrets = configured.is_none()
        && ([
            FLEET_ROSTER_FILE,
            ACCOUNTS_CSV_FILE,
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
        || matches!(configured, Some(FleetSource::Key(_)))
            && key_file_changed(home, state_dir, config, now, rediscovery)
}

/// Whether `[fleet] accounts_source` names a local file this process has not synced from as it
/// stands now (its modification time is not the one last seen): at the first read of a file that
/// is there, then each time it changes. A URL is read only when the roster is missing or a person
/// syncs.
fn key_file_changed(
    home: &Path,
    state_dir: &Path,
    config: &Path,
    now: i64,
    rediscovery: &std::sync::Mutex<Rediscovery>,
) -> bool {
    let Some(source) = source_key(config) else {
        return false;
    };
    let Ok(FetchPlan::File(path)) = fetch_plan(&quiet_env(home, state_dir, config, now), &source)
    else {
        return false;
    };
    let Ok(modified) = fs::metadata(&path).and_then(|m| m.modified()) else {
        return false;
    };
    let seen = (path, modified);
    let mut held = rediscovery
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if held.key_seen.as_ref() == Some(&seen) {
        return false;
    }
    held.key_seen = Some(seen);
    true
}

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
    };
    let _ = run_accounts_sync_with(&env, (None, None, true), (&mut out, &mut err), &how);
    roster_path.exists()
}

/// Whether a fleet accounts source is configured or discoverable for [`sync_accounts_for_init`].
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
    if [
        FLEET_ROSTER_FILE,
        "ai-accounts.csv",
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
            .is_some_and(|p| p.join(FLEET_ROSTER_FILE).is_file())
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
    let secrets_fleet = secrets.join(FLEET_ROSTER_FILE);
    if secrets_fleet.is_file() && fs::read(&secrets_fleet).ok() != fs::read(&fb).ok() {
        return true;
    }
    let Ok(existing_text) = read_local_source_text(&fb) else {
        return false;
    };
    let Ok(existing_fleet) = FleetRoster::from_toml(&existing_text, "fleet-accounts.toml") else {
        return false;
    };
    let opt_in = opt_in_of(&existing_text);
    let secrets_csv = secrets.join("ai-accounts.csv");
    if !secrets_fleet.is_file()
        && secrets_csv.is_file()
        && let Ok(csv_text) = read_local_source_text(&secrets_csv)
        && let Ok(csv_fleet) = parse_accounting_csv(&csv_text, "ai-accounts.csv")
        && csv_fleet.accounts.iter().any(|acct| {
            !existing_fleet
                .accounts
                .iter()
                .any(|a| accounts::email_matches(&a.email, &acct.email))
        })
    {
        return true;
    }
    // Discovery's accounts are added only to a fleet discovery made: a fleet from a vault, a
    // sheet or a URL stays as its source wrote it, and a person's list as they wrote it.
    if !fleet_of_this_machine(opt_in, &existing_fleet) {
        return false;
    }
    let config_path = config_beside(roster_path);
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
            publish: publish && added,
        },
    ))
}

/// Synchronize the machine and fleet account rosters during `aterm init` or `aterm secrets`:
/// the machine, its rows, those enrolled and not signed out, and the fleet's rows.
/// # Errors
/// No source was found or the sync refused, as [`run_accounts_sync`] says.
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
    let secrets_source = {
        let secrets = home.join(".secrets");
        [FLEET_ROSTER_FILE, "ai-accounts.csv"]
            .into_iter()
            .map(|name| secrets.join(name))
            .find(|p| p.is_file())
            .map(|p| p.to_string_lossy().into_owned())
    };
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
        Some(src)
    } else if configured_source(env.config.as_deref(), Some(roster_path)).is_some() {
        if has_named_fleet {
            let (text, snapshot) = capture_init_fleet(&env, &fleet_path, true, false)?;
            captured_fleet = Some((fleet_path.to_string_lossy().into_owned(), text));
            fleet_snapshots.push(snapshot);
        }
        None
    } else if fleet_path.is_file() {
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
                publish: false,
            });
            source
        })
    };
    let secrets_dir = home.join(".secrets");
    let secrets_fleet = secrets_dir.join(FLEET_ROSTER_FILE);
    if secrets_dir.is_dir() && !secrets_fleet.exists() {
        fleet_snapshots.push(FleetSnapshot {
            path: secrets_fleet,
            bytes: None,
            publish: true,
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
    let mut out = Vec::new();
    let mut err = Vec::new();
    if run_accounts_sync_with(
        &env,
        (init_source.as_deref(), None, true),
        (&mut out, &mut err),
        &SyncHow {
            fetch: &fetch,
            ladder: &sync_lock_ladder(),
            fleet_snapshots: &fleet_snapshots,
        },
    ) != ExitCode::SUCCESS
    {
        let msg = String::from_utf8_lossy(&err).trim().to_owned();
        return Err(if msg.is_empty() {
            "could not sync accounts.toml".to_owned()
        } else {
            msg
        });
    }
    // The sync's own word of what it changed and which rows are still to sign in.
    let said = aterm_json::from_str::<Value>(String::from_utf8_lossy(&out).trim()).ok();
    let changed = said
        .as_ref()
        .and_then(|said| said.get("changed"))
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let to_sign_in: Vec<String> = said
        .as_ref()
        .and_then(|said| said.get("needs_renew"))
        .and_then(Value::as_array)
        .map(|labels| {
            labels
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let store = accounts::Store::at(state_dir);
    let raw = Roster::load(roster_path)?;
    let bound = store.bound(&raw, now)?;
    let enrolled = accounts::enrolled(&bound);
    let state = store.load(now)?.0;
    let enrolled_count = bound
        .rows
        .iter()
        .filter(|r| {
            (r.dir.is_none() || enrolled.contains(&r.label))
                && state
                    .accounts
                    .get(&r.label)
                    .is_none_or(|a| a.auth != accounts::Auth::SignedOut)
        })
        .count();
    let fleet_count = fs::read_to_string(&fleet_path)
        .ok()
        .and_then(|t| FleetRoster::from_toml(&t, "fleet-accounts.toml").ok())
        .map_or(bound.rows.len(), |f| f.accounts.len());
    let machine = config_machine_id(env.config.as_deref())
        .or_else(|| {
            detect_machine_id(None, None).filter(|m| {
                bound
                    .rows
                    .iter()
                    .any(|r| r.label == *m || r.label.starts_with(&format!("{m}-")))
            })
        })
        .or_else(|| {
            bound
                .rows
                .first()
                .map(|r| r.label.split('-').next().unwrap_or(&r.label).to_owned())
        })
        .or_else(|| detect_machine_id(None, None))
        .unwrap_or_else(|| "local".to_owned());
    if changed {
        let _ = store.touch_wake();
    }
    Ok(InitSynced {
        machine,
        rows: bound.rows.len(),
        enrolled: enrolled_count,
        fleet: fleet_count,
        changed,
        to_sign_in,
    })
}

/// What one periodic resync of this machine's accounts did ([`resync_for_setup`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Resync {
    /// Claude Code's sync ran: the roster was missing (at most once per 10 minutes) or something
    /// it is made from changed ([`sync_called_for`]).
    pub synced: bool,
    /// Why that sync failed, where it did (no source found, a refusal); nothing was written.
    pub failed: Option<String>,
    /// It changed a file, the account state or the ledger. A sync that finds what is already
    /// there writes nothing.
    pub changed: bool,
    /// The rows it left with no login made here, for the automatic sign-in ladder.
    pub to_sign_in: Vec<String>,
    /// The ladder started a sign-in, or a saved account choice progressed.
    pub renewal: bool,
    /// Each other CLI whose sync failed, with why.
    pub cli_failed: Vec<(Cli, String)>,
}

/// THE SETUP PASS'S RESYNC (`aterm`'s first-run setup runs it every pass it retries): Claude
/// Code's sync only where the roster is missing or something it is made from changed
/// ([`sync_called_for`], the rule a window's upkeep keeps), each other CLI's (which writes only
/// the rows it adds), and, where the sync leaves rows with no login made here, `sign_in` allows
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

fn resync_with(
    home: &Path,
    state_dir: &Path,
    (roster_path, now, sign_in): (&Path, i64, bool),
    fx: &UpkeepEffects<'_>,
) -> Resync {
    let config = config_beside(roster_path);
    let missing = !roster_path.exists() && {
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
    if missing || sync_called_for(home, state_dir, (roster_path, &config), now, fx.rediscovery) {
        done.synced = true;
        match sync_accounts_for_init_with(home, state_dir, roster_path, now, || {}, fx.fetch) {
            Ok(synced) => {
                done.changed = synced.changed;
                done.to_sign_in = synced.to_sign_in;
            }
            Err(why) => done.failed = Some(why),
        }
    }
    done.cli_failed = sync_cli_accounts_with(home, state_dir, roster_path, now, fx.fetch)
        .into_iter()
        .filter_map(|(cli, synced)| synced.err().map(|why| (cli, why)))
        .collect();
    if sign_in && sign_ins_on(&config) && !done.to_sign_in.is_empty() {
        done.renewal = auto_renew_next_account(home, state_dir, roster_path, now, fx);
    }
    done
}

/// The accounting sheet's name in `~/.secrets` (`DESIGN-secrets-2026-10-20.md` §11).
pub const ACCOUNTS_CSV_FILE: &str = "ai-accounts.csv";

/// The pooled CLIs other than Claude Code, whose pools [`sync_cli_accounts_for_init`] and the
/// window's upkeep ([`upkeep_live`]) keep beside Claude Code's.
pub const OTHER_CLIS: [Cli; 3] = [Cli::Codex, Cli::Grok, Cli::Kimi];

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
    /// Nothing: this machine keeps no roster of the CLI's and no source was named, so nothing
    /// was read (a vault file alone opts no machine in).
    Off,
    /// Opted in, and no fleet of the CLI's is there to read: neither its fleet file
    /// ([`cli_fleet_file`]) nor its column of the sheet ([`ACCOUNTS_CSV_FILE`]).
    NoSource,
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
    /// This machine's fleet rows left out because the roster lists their label with another
    /// email, or their email under another label: the roster's own row stands.
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
    let (chosen, raw) = match source.map(str::trim).filter(|s| !s.is_empty()) {
        Some(named) => (named.to_owned(), fetch(env, named)?),
        None => {
            let secrets = home.join(".secrets");
            let toml = secrets.join(cli_fleet_file(cli));
            let csv = secrets.join(ACCOUNTS_CSV_FILE);
            let read = |path: &Path| {
                read_local_source_text(path).map_err(|why| format!("{}: {why}", path.display()))
            };
            if toml.is_file() {
                (toml.display().to_string(), read(&toml)?)
            } else if csv.is_file() {
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
    Ok(Some(fleet))
}

/// Sync `cli`'s pool (a CLI of [`OTHER_CLIS`]) on this machine from its fleet
/// ([`load_cli_fleet`]): where its roster (`accounts-<cli>.toml`) stands beside `config`, or a
/// `source` is named, this machine's fleet rows the roster does not list are added to it, each
/// with a login directory under `~/.aterm/logins/<cli>/` ([`accounts::Store::at_for`]), and a roster
/// with no rows first gets this machine's own login (`main`, no dir), as `accounts add` gives it.
/// It writes the roster and the CLI's store alone, never `~/.secrets`; a row the roster has is
/// never changed or removed, and a fleet row whose label or email the roster lists otherwise is
/// left out ([`CliSynced::kept`]). Under the CLI's store lock, waited for on `ladder`.
fn sync_cli_with(
    cli: Cli,
    (home, state_dir, config): (&Path, &Path, &Path),
    (source, machine_override): (Option<&str>, Option<&str>),
    now: i64,
    (fetch, ladder): (&Fetch<'_>, &[Duration]),
) -> Result<CliSync, String> {
    let product = cli.connector().product;
    let roster_path = accounts::roster_beside_for(config, cli)
        .ok_or_else(|| format!("{} has no directory", config.display()))?;
    let named = source.is_some_and(|s| !s.trim().is_empty());
    if !named && !roster_path.is_file() {
        return Ok(CliSync::Off);
    }
    let env = quiet_env(home, state_dir, config, now);
    let Some(fleet) = load_cli_fleet(cli, &env, home, source, fetch)? else {
        return Ok(CliSync::NoSource);
    };
    let machine = machine_override
        .and_then(normalize_machine_id)
        .or_else(|| config_machine_id(Some(config)))
        .or_else(|| detect_machine_id(None, None))
        .ok_or_else(|| "could not determine machine id (`m<N>`); pass `--machine m8`".to_owned())?;
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
    let store = accounts::Store::at_for(state_dir, cli, home);
    let lock = store
        .acquire_within(ladder)
        .map_err(|why| format!("{}: {why}", store.dir.display()))?
        .ok_or_else(|| {
            format!(
                "{} at {}: another `aterm harness accounts` or a window holds it; nothing was \
                 synced, so run the sync again",
                accounts::LOCK_HELD,
                store.dir.display()
            )
        })?;
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
    for acct in mine {
        let listed_email = |row: &Row| {
            row.email
                .as_deref()
                .is_some_and(|email| accounts::email_matches(email, &acct.email))
                || (cli == Cli::Codex
                    && row
                        .expected_identity_email()
                        .is_some_and(|email| accounts::email_matches(email, &acct.email)))
        };
        match (
            existing.row(&acct.label),
            existing.rows.iter().find(|row| listed_email(row)),
        ) {
            (Some(row), _) if listed_email(row) => {}
            (None, None) => {
                let dir = store
                    .login_dir(&acct.label)
                    .ok_or_else(|| format!("`{}` names no login directory", acct.label))?;
                let dir = dir
                    .to_str()
                    .map(str::to_owned)
                    .ok_or_else(|| format!("the login directory {} is not UTF-8", dir.display()))?;
                rows.push(Row {
                    label: acct.label.clone(),
                    class: acct.class,
                    dir: Some(dir),
                    email: Some(acct.email.clone()),
                    identity_email: None,
                    browser: acct.browser.clone(),
                    was: Vec::new(),
                    vendor: acct.vendor,
                });
                added.push(acct.label.clone());
            }
            _ => kept.push(acct.label.clone()),
        }
    }
    let mut total = existing.rows.len() + rows.len();
    if !rows.is_empty() {
        if existing.rows.is_empty() {
            rows.insert(
                0,
                Row {
                    label: ACCOUNTS_OWN_LABEL.to_owned(),
                    ..Row::default()
                },
            );
            total += 1;
        }
        let text = accounts::append_rows_for(cli, &existing_text, &rows).map_err(|why| {
            format!("the synced rows do not admit: {why}; the roster is unchanged")
        })?;
        aterm_types::fs_restricted::ensure_private_dir(&store.logins_dir())
            .map_err(|why| format!("{}: {why}", store.logins_dir().display()))?;
        for dir in rows.iter().filter_map(|row| row.dir.as_deref()) {
            aterm_types::fs_restricted::ensure_private_dir(Path::new(dir))
                .map_err(|why| format!("could not create {dir}: {why}"))?;
        }
        super::upgrade_models::write_atomic_mode(&write_to, &text, keep_mode)
            .map_err(|why| format!("could not write {}: {why}", write_to.display()))?;
    }
    drop(lock);
    let recovered =
        recover_interrupted_staging_renewals_for(cli, &store, &write_to, &AUTO_RENEW_GUARD, now);
    // A sync that added nothing wrote nothing, and wakes no watcher.
    if !added.is_empty() || recovered {
        let _ = store.touch_wake();
    }
    Ok(CliSync::Synced(CliSynced {
        cli,
        machine,
        roster: roster_path,
        source: fleet.roster.source.clone(),
        fleet_accounts: fleet.roster.accounts.len(),
        rows: total,
        added,
        held,
        kept,
    }))
}

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
    let (Some(config), Some(home)) = (env.config.as_deref(), env.home.as_deref()) else {
        let _ = writeln!(
            err,
            "aterm harness accounts sync {flag}: no config directory or home is known (set HOME \
             or XDG_CONFIG_HOME)"
        );
        return ExitCode::FAILURE;
    };
    let synced = sync_cli_with(
        cli,
        (home, &env.state, config),
        (source, machine_override),
        env.now,
        (fetch, &sync_lock_ladder()),
    );
    let file = cli.accounts().roster_file;
    let synced = match synced {
        Ok(CliSync::Synced(synced)) => synced,
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
            "left as the roster lists them (another email under the label, or the email under \
             another label): {}",
            synced.kept.join(" ")
        );
    }
    if !synced.added.is_empty() {
        let _ = writeln!(
            out,
            "not signed in yet: {}; aterm signs one in by itself once a session needs it \
             (`aterm harness accounts renew {} {flag}` runs that now)",
            synced.added.join(", "),
            synced.added.join(" ")
        );
    }
    ExitCode::SUCCESS
}

/// [`sync_accounts_for_init`] for the other pooled CLIs ([`OTHER_CLIS`]), each through
/// [`sync_cli_with`] with no source: a CLI whose roster this machine keeps beside `roster_path`
/// syncs from its fleet in `~/.secrets`; one whose roster it does not keep reads nothing
/// ([`CliSync::Off`]), whatever the vault put there.
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

/// The pools of [`OTHER_CLIS`] whose roster stands beside `config` (none where none does): what
/// the doctor's `base.ai.accounts` row and the dashboard show beside Claude Code's.
#[must_use]
pub fn cli_pools(config: &Path, state_dir: &Path, home: &Path, now: i64) -> Vec<CliPool> {
    OTHER_CLIS
        .into_iter()
        .filter_map(|cli| {
            let roster = accounts::roster_beside_for(config, cli)?;
            if !roster.is_file() {
                return None;
            }
            let store = accounts::Store::at_for(state_dir, cli, home);
            let mut state = accounts::State::default();
            let rows = Roster::load_for(cli, &roster).and_then(|raw| {
                let bound = store.bound(&raw, now)?;
                state = store.load(now)?.0;
                Ok(bound)
            });
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
/// `roster_path` syncs from its fleet without waiting on its lock, and, where `[harness] logins`
/// renews, starts one due login's renewal in the background ([`spawn_row_renewal`]); a CLI
/// whose roster this machine does not keep is left alone.
fn upkeep_pools(
    home: &Path,
    state_dir: &Path,
    (roster_path, now, step): (&Path, i64, UpkeepStep),
    fx: &UpkeepEffects<'_>,
) {
    let config = config_beside(roster_path);
    let renews = step.renews()
        && crate::supervise::config::SupervisorConfig::from_path(Some(&config))
            .0
            .logins;
    for cli in OTHER_CLIS {
        let Some(path) = accounts::roster_beside_for(&config, cli) else {
            continue;
        };
        if !path.is_file() {
            continue;
        }
        if step.syncs() {
            let _ = sync_cli_with(
                cli,
                (home, state_dir, &config),
                (None, None),
                now,
                (fx.fetch, &[]),
            );
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

/// Start one of `cli`'s due or wall-recovery logins' renewal ([`needed_renew_candidates_for`],
/// [`spawn_row_renewal`]); whether one started.
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
    let Ok(file_roster) = Roster::load_for(cli, roster_path) else {
        return recovered;
    };
    let (Ok((state, _)), Ok((logins, _))) = (store.load(now), store.load_logins(now)) else {
        return recovered;
    };
    for row in needed_renew_candidates_for(cli, Some(&store), &file_roster, &state, &logins, now) {
        if spawn_row_renewal(
            cli,
            home,
            state_dir,
            roster_path,
            file_roster.clone(),
            &logins,
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

#[derive(Default)]
struct AutoRenewGuard {
    in_flight: bool,
    last_attempt: BTreeMap<String, Instant>,
    mailbox_mtime: Option<std::time::SystemTime>,
    /// The rows whose last background renewal stopped where only a person can go on
    /// ([`super::cli::Renewal::NeedsPerson`]), and since when: none is tried again in the
    /// background before [`PERSON_BACKOFF`] has passed.
    parked: BTreeMap<String, Instant>,
    /// The newest `(at, same_second_count)` signed-in `accounts browser` window event already used
    /// to lift a row's background hold once.
    browser_unparked: BTreeMap<String, (i64, usize)>,
    /// The automatic sign-in ladder's backoffs and vault clock ([`super::sign_in_ladder::Ladder`]).
    ladder: super::sign_in_ladder::Ladder,
}

/// How long a row whose background renewal stopped for a person is left alone by the
/// background before it is tried again: a renewal that stops so (accounts.x.ai's block page,
/// a page that waits on a person) starts a live device sign-in every time it runs.
const PERSON_BACKOFF: Duration = Duration::from_secs(6 * 60 * 60);
const PERSON_BACKOFF_SECS: i64 = 6 * 60 * 60;

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

pub(super) fn unaddressed_sign_in_task_at(
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
        guard.last_attempt.remove(key);
    }
}

/// Notes how a background renewal of `label` ended: one that stopped for a person
/// ([`super::cli::Renewal::NeedsPerson`]) parks the row until [`PERSON_BACKOFF`] has passed
/// since `now`; a contended lock leaves any park as it was; any other end unparks it.
fn note_background_end(
    state: &std::sync::Arc<std::sync::Mutex<AutoRenewGuard>>,
    label: &str,
    renewal: super::cli::Renewal,
    now: Instant,
) {
    let mut guard = state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if renewal == super::cli::Renewal::NeedsPerson {
        guard.parked.insert(label.to_owned(), now);
    } else if renewal != super::cli::Renewal::Busy {
        guard.parked.remove(label);
    }
}

/// Whether Claude Code's store or any pooled CLI's store under `state_dir` holds a saved
/// explicit account selection (`pending_pin`).
#[must_use]
pub fn any_pending_pin(state_dir: &Path, now: i64) -> bool {
    accounts::Cli::ALL.into_iter().any(|cli| {
        accounts::Store::at_for(state_dir, cli, Path::new(""))
            .load(now)
            .is_ok_and(|(state, _)| state.pending_pin.is_some())
    })
}

static AUTO_RENEW_GUARD: std::sync::LazyLock<std::sync::Arc<std::sync::Mutex<AutoRenewGuard>>> =
    std::sync::LazyLock::new(|| {
        std::sync::Arc::new(std::sync::Mutex::new(AutoRenewGuard::default()))
    });

struct AutoRenewPermit {
    guard: std::sync::Arc<std::sync::Mutex<AutoRenewGuard>>,
    wake_pending: Option<PathBuf>,
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
                    .is_some_and(|since| now.saturating_duration_since(*since) < PERSON_BACKOFF)))
        {
            return None;
        }
        guard.in_flight = true;
        guard.last_attempt.insert(label.to_owned(), now);
        Some(Self {
            guard: std::sync::Arc::clone(state),
            wake_pending: None,
        })
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
    if let Some(env_val) = std::env::var_os(super::open_url::MAILBOX_ENV).filter(|v| !v.is_empty())
    {
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
    resolve_mailbox_dir(home, store).is_some()
        || super::headless_oauth::discover_mailbox_config(home, Some(&store.dir)).is_some_and(
            |mut cfg| {
                cfg.alias_label_to_email(label, target_email);
                cfg.is_configured_for(target_email)
            },
        )
}

/// The newest modification time among the files a sign-in reads (each file in `~/.secrets`,
/// the configuration and roster files, the mailbox configurations, the mail folders and their
/// unread mail): a change
/// here ends an automatic sign-in's cooldown. Neither the `~/.secrets` folder's own time nor
/// the vault's `state.toml`, which its subscriber rewrites about once a minute only to say when
/// it last heard the vault, is among them.
fn latest_mailbox_mtime(
    home: Option<&Path>,
    store: &accounts::Store,
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
            paths.push(dir);
        }
    };
    if let Some(mb_dir) = resolve_mailbox_dir(home, store) {
        push_maildir_paths(&mut paths, &mb_dir);
    }
    if let Some(cfg_dir) =
        super::headless_oauth::discover_mailbox_config(home, Some(&store.dir)).and_then(|c| c.dir)
    {
        push_maildir_paths(&mut paths, &cfg_dir);
    }
    paths
        .into_iter()
        .filter_map(|p| std::fs::metadata(p).ok()?.modified().ok())
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
        row.email.is_some()
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

fn complete_staging_renewal_result(
    store: &accounts::Store,
    policy: accounts::RenewalPolicy<'_>,
    staging: &Path,
    staging_text: &str,
    (status_got, success_route): (&super::align::Captured, Option<&str>),
    now_done: i64,
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
            outcome = Renewal::Renewed;
            let mut events = accounts::note_renewed(
                state,
                logins,
                &matched_label,
                own_dir,
                staging_text,
                subscription.as_deref(),
                now_done,
            );
            if let Some(route) = success_route {
                events.extend(accounts::note_sign_in_attempt(
                    logins,
                    requested_label,
                    accounts::AttemptOutcome::SignedIn,
                    route,
                    now_done,
                ));
            }
            Ok(events)
        },
    );
    match result {
        Ok(_) if outcome == Renewal::Renewed => outcome,
        Ok(_) => {
            let _ = fs::remove_dir_all(staging);
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
    ) == super::cli::Renewal::Renewed
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
    if renew_guard
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .in_flight
    {
        return false;
    }
    let Ok(entries) = fs::read_dir(store.logins_dir()) else {
        return false;
    };
    struct Candidate {
        prefix: String,
        sequence: u64,
        dir: PathBuf,
        actual_dir: PathBuf,
        proof: accounts::RenewalCheckpoint,
    }
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
            if cli != Cli::Claude
                && !cli
                    .connector()
                    .stores
                    .iter()
                    .any(|store_file| dir.join(store_file).is_file())
            {
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
        return false;
    }
    if store
        .shared_dir()
        .join(accounts::SIGNIN_LOCK_FILE)
        .is_file()
        && matches!(store.signin_now(), Ok(None))
    {
        return false;
    }
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

    let mut any_recovered = false;
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
        let mut note = None;
        if store
            .transact_logins(now, &mut note, |state, logins| {
                // A checkpoint can outlive the configuration read by upkeep.
                // Every provider resolves its identity policy under the writer lock.
                let current = Roster::load_for(cli, roster_path)?;
                let file_roster = &current;
                let referenced = file_roster
                    .rows
                    .iter()
                    .flat_map(|row| row.dir.iter().chain(row.was.iter().flatten()))
                    .chain(
                        logins
                            .labels
                            .values()
                            .flat_map(|entry| entry.dir.iter().chain(entry.was.iter().flatten())),
                    )
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
                let needs_recovery = !enrolled.contains(&row.label) || is_signed_out;
                if is_limited || newer_auth || newer_binding || !needs_recovery {
                    return Ok(Vec::new());
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
            })
            .is_ok_and(|events| !events.is_empty())
        {
            recovered_labels.insert(proof.label);
            any_recovered = true;
        }
    }
    any_recovered
}

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
    let Ok(file_roster) = Roster::load(roster_path) else {
        return recovered;
    };
    let Ok((state, _)) = store.load(now) else {
        return recovered;
    };
    let Ok((logins, _)) = store.load_logins(now) else {
        return recovered;
    };
    let ladder = LadderCtx::of(fx, roster_path);
    let tiered = auto_renew_candidates_tiered(
        Cli::Claude,
        Some(&store),
        &file_roster,
        &state,
        &logins,
        now,
    );
    // A row no session needs yet is signed in only where it is a Claude Code account by some
    // word: a source's or a person's row, or one Claude Code's own files show, never an address
    // discovery listed only because a credential names it.
    let unclaimed = if tiered.iter().any(|(tier, _)| *tier >= UNNEEDED_TIER) {
        unclaimed_emails(home, state_dir, roster_path, now, fx.rediscovery)
    } else {
        BTreeSet::new()
    };
    for (tier, next_row) in tiered {
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
            file_roster.clone(),
            &logins,
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
    let fx = live_effects();
    if let Some(pending) = resume_pending_pin_for(cli, home, state_dir, roster_path, now, &fx) {
        return pending.progressed();
    }
    let store = accounts::Store::at_for(state_dir, cli, home);
    let recovered =
        recover_interrupted_staging_renewals_for(cli, &store, roster_path, &AUTO_RENEW_GUARD, now);
    let Ok(file_roster) = Roster::load_for(cli, roster_path) else {
        return recovered;
    };
    let Ok((state, _)) = store.load(now) else {
        return recovered;
    };
    let Ok((logins, _)) = store.load_logins(now) else {
        return recovered;
    };
    // A person's click starts only what a session needs now: a row no session needs is the
    // upkeep's to sign in, headless, never in a browser opened for a click.
    for next_row in
        needed_renew_candidates_for(cli, Some(&store), &file_roster, &state, &logins, now)
    {
        if spawn_row_renewal(
            cli,
            home,
            state_dir,
            roster_path,
            file_roster.clone(),
            &logins,
            next_row,
            (None, true),
            now,
            &AUTO_RENEW_GUARD,
            &spawn_renewal_thread,
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
    /// Another renewal owns admission; keep the request for a later upkeep.
    Held,
    /// Startup or store access failed; this attempt cannot proceed.
    Failed,
    /// A newer selection or an explicit cancellation replaced this request.
    Superseded,
}

impl RenewalDispatch {
    fn progressed(self) -> bool {
        matches!(self, Self::Started | Self::Recovered)
    }
}

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
    let request = match store.load(now) {
        Ok((state, _)) => state.pending_pin?,
        Err(_) => return Some(RenewalDispatch::Failed),
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
        let _ = store.cancel_pin_request(&request, now);
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
    match store.load(now) {
        Ok((state, _)) if state.pending_pin.as_ref() == Some(&request) => {}
        Ok(_) => return RenewalDispatch::Superseded,
        Err(_) => return RenewalDispatch::Failed,
    }
    recover_interrupted_staging_renewals_for(cli, &store, roster_path, renew_guard, now);
    let Ok(file_roster) = Roster::load_for(cli, roster_path) else {
        return RenewalDispatch::Failed;
    };
    let mut disposition = RenewalDispatch::Superseded;
    let mut target = None;
    if store
        .transact_logins_within(&accounts::LOCK_LADDER, now, &mut None, |st, logins| {
            if st.pending_pin.as_ref() != Some(&request) {
                return Ok(Vec::new());
            }
            let roster = accounts::bound(&file_roster, logins);
            let enrolled = accounts::enrolled_for(cli, &roster);
            let Some(next_row) = roster.row(request.label()).cloned() else {
                return Err("selected account is no longer in the roster".into());
            };
            let mut probe = st.clone();
            if accounts::pin(&mut probe, &roster, &enrolled, request.label(), now).is_err() {
                target = Some((next_row, logins.clone()));
                return Ok(Vec::new());
            }
            disposition = RenewalDispatch::Recovered;
            Ok(
                accounts::complete_requested_pin(st, &roster, &enrolled, &request, now)
                    .into_iter()
                    .collect(),
            )
        })
        .is_err()
    {
        return RenewalDispatch::Failed;
    }
    let Some((next_row, logins)) = target else {
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
    find_browser: &'a (dyn Fn(&Path) -> bool + Sync),
    vault_sync: fn(&Path) -> super::sign_in_ladder::Vault,
    spawn_vault: &'a SpawnRenewal,
    mailbox: &'a LadderMailbox,
    log: SignInLog,
    run_attempt: Option<RunAttempt>,
    /// How many `[harness] hosts` the config beside the roster names.
    hosts: usize,
}

impl<'a> LadderCtx<'a> {
    fn of(fx: &UpkeepEffects<'a>, roster_path: &Path) -> Self {
        let config = config_beside(roster_path);
        Self {
            find_browser: fx.find_browser,
            vault_sync: fx.vault_sync,
            spawn_vault: fx.spawn_vault,
            mailbox: fx.mailbox,
            log: std::sync::Arc::clone(&fx.log),
            run_attempt: fx.run_attempt.clone(),
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
    if let Ok(text) = read_local_source_text(&vault.join("state.toml")) {
        for kept in text.lines().map(str::trim).filter(|kept| {
            kept.split_once('=').is_some_and(|(key, _)| {
                matches!(
                    key.trim(),
                    "membership" | "seq" | "digest" | "source_identity"
                )
            })
        }) {
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
    Read {
        profile,
        mailbox: (ctx.mailbox)(home, store, row),
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
    let inputs = format!("{}|{}", read.words(), ladder_inputs(home, store));
    let at = Instant::now();
    if lock(renew_guard).ladder.held(&row.label, &inputs, at) {
        return LadderStep::Held;
    }
    if read.route().is_none() {
        let member = home.join(".config/aterm/secrets/source.toml").is_file();
        let vault = if member {
            let turn = lock(renew_guard).ladder.vault_turn(at);
            match turn {
                VaultTurn::Seen(seen) => seen,
                VaultTurn::Running => return LadderStep::Held,
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
                    match lock(renew_guard).ladder.vault_turn(at) {
                        VaultTurn::Seen(seen) => seen,
                        VaultTurn::Running | VaultTurn::Due => return LadderStep::Held,
                    }
                }
            }
        } else {
            Vault::NotMember
        };
        if matches!(vault, Vault::Applied(_)) {
            read = ladder_read(home, store, logins, row, ctx);
        }
        read.vault = Some(vault);
    }
    if let Some(route) = read.route() {
        return LadderStep::Go {
            route,
            read,
            inputs,
        };
    }
    let words = read.words();
    note_attempt(
        store,
        logins,
        &row.label,
        accounts::AttemptOutcome::NoRoute,
        &words,
        now,
    );
    let wait = lock(renew_guard).ladder.failed(&row.label, &inputs, at);
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

/// Records `label`'s attempt ([`accounts::note_sign_in_attempt`]) unless `logins` (as read
/// before) already holds the same one or that captured generation is no longer current.
/// A store that cannot be written now records it on the next attempt.
fn note_attempt(
    store: &accounts::Store,
    logins: &accounts::Logins,
    label: &str,
    outcome: accounts::AttemptOutcome,
    routes: &str,
    now: i64,
) {
    let parent = accounts::RenewalParent::capture(logins, label);
    let same = logins
        .get(label)
        .and_then(|b| b.sign_in_attempt.as_ref())
        .is_some_and(|last| {
            last.outcome == outcome && last.routes == super::one_line(routes, accounts::ROUTES_MAX)
        });
    if same {
        return;
    }
    let _ = store.transact_logins_within(&accounts::LOCK_LADDER, now, &mut None, |_, logins| {
        parent.require_current(logins, label)?;
        Ok(accounts::note_sign_in_attempt(
            logins, label, outcome, routes, now,
        ))
    });
}

#[expect(
    clippy::too_many_arguments,
    reason = "row renewal binds paths, roster, logins, target row, pin/browser flags, clock, admission guard, spawn seam and the automatic ladder"
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
    if store.check_signin_state(now).is_err() {
        return RenewalDispatch::Failed;
    }
    if next_row.email.is_none() {
        return RenewalDispatch::Failed;
    }
    let mailbox_dir = resolve_mailbox_dir(Some(home), &store);
    // The automatic ladder ([`ladder_route`]) decides an upkeep's renewal and records how it
    // ends; a person's own choice (a pin request, a click) takes the routes it always took.
    let (has_mailbox, has_headless_profile, unattended) = match ladder {
        Some(ctx) => match ladder_route(home, &store, &next_row, now, renew_guard, ctx) {
            LadderStep::Go {
                route,
                read,
                inputs,
            } => (
                route == super::sign_in_ladder::Route::Mailbox,
                route == super::sign_in_ladder::Route::Profile,
                Some((route, read, inputs, std::sync::Arc::clone(&ctx.log))),
            ),
            LadderStep::Held => return RenewalDispatch::Held,
            LadderStep::NoRoute => return RenewalDispatch::Failed,
        },
        None => {
            let has_mailbox = mailbox_dir.is_some()
                || has_configured_mailbox_for_row(
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
                && super::headless_oauth::find_headless_browser(Some(home)).is_some();
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
    let mailbox_mtime = latest_mailbox_mtime(Some(home), &store);
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
    let signin = match accounts::Store::at(state_dir).signin_now() {
        Ok(Some(signin)) => signin,
        Ok(None) => return RenewalDispatch::Held,
        Err(_) => return RenewalDispatch::Failed,
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
    let started = start_auto_renew(
        permit,
        move || {
            use super::cli::Renewal;

            let store = accounts::Store::at(&state_owned);
            // How the last run's `claude auth login` exited, for the words of one that made no
            // login (`sign_in_ladder::how_it_ended`).
            let exited: std::cell::Cell<Option<i32>> = std::cell::Cell::new(None);
            let attempt_snapshot = std::cell::RefCell::new(None);
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
                if store
                    .transact_renewal_within(
                        &accounts::LOCK_LADDER,
                        now,
                        &mut None,
                        policy,
                        |_, _| Ok(Vec::new()),
                    )
                    .is_err()
                {
                    return Renewal::Failed;
                }
                let Ok(staging) = store.claim_staging_dir(&next_label) else {
                    return Renewal::Failed;
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
                let Ok(pass_env) = super::cli::auth_check_env(&env_var) else {
                    let _ = fs::remove_dir_all(&staging);
                    return Renewal::Failed;
                };
                let claude_prog = resolve_claude_program(&home_owned);
                let mut cmd = super::cli::renew_command(
                    claude_prog.as_os_str(),
                    &staging_text,
                    &next_email,
                    browser,
                );
                cmd.env("HOME", &home_owned)
                    .env(super::open_url::EMAIL_ENV, &next_email)
                    .envs(pass_env);
                for pass_var in [
                    super::open_url::MAILBOX_ENV,
                    super::headless_oauth::BROWSER_BIN_ENV,
                    super::headless_oauth::HEADLESS_ENV,
                    super::headless_oauth::MAILBOX_URL_ENV,
                    super::headless_oauth::MAILBOX_TOKEN_ENV,
                    super::headless_oauth::IMAP_URL_ENV,
                    super::headless_oauth::IMAP_USER_ENV,
                    super::headless_oauth::IMAP_PASS_ENV,
                    super::headless_oauth::WEBMAIL_URL_ENV,
                    super::headless_oauth::WEBMAIL_PASS_ENV,
                    super::headless_oauth::TOTP_SECRET_ENV,
                    "XDG_CONFIG_HOME",
                    "XDG_CACHE_HOME",
                    "XDG_STATE_HOME",
                    "ATERM_STATE_HOME",
                ] {
                    if let Some(val) = std::env::var_os(pass_var).filter(|v| !v.is_empty()) {
                        cmd.env(pass_var, val);
                    }
                }
                if std::env::var_os(super::open_url::MAILBOX_ENV).is_none_or(|v| v.is_empty())
                    && let Some(mailbox) = &mailbox_dir
                {
                    cmd.env(super::open_url::MAILBOX_ENV, mailbox);
                }
                if force_headless {
                    cmd.env(super::headless_oauth::HEADLESS_ENV, "1");
                }
                let ran = super::align::capture_bounded(cmd, Duration::from_secs(300), 64 * 1024);
                let ran = match ran {
                    Ok(ran) => ran,
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
                complete_staging_renewal_result(
                    &store,
                    policy,
                    &staging,
                    &staging_text,
                    (
                        &status_got,
                        unattended.as_ref().map(|(route, _, _, _)| route.as_str()),
                    ),
                    now_done,
                )
            };
            let run_attempt = |browser: &super::cli::RenewBrowser, force_headless: bool| {
                if store.check_signin_state(now).is_err() {
                    return Renewal::Failed;
                }
                let Ok((snapshot, _)) = store.load_logins(now) else {
                    return Renewal::Failed;
                };
                *attempt_snapshot.borrow_mut() = Some(snapshot.clone());
                exited.set(None);
                let result = match &run_seam {
                    Some(run) => run(browser, force_headless),
                    None => live_attempt(browser, force_headless, &snapshot),
                };
                if result != Renewal::Renewed
                    && store
                        .transact_logins_within(
                            &accounts::LOCK_LADDER,
                            now,
                            &mut None,
                            |_, current| {
                                accounts::RenewalParent::capture(&snapshot, &next_label)
                                    .require_current(current, &next_label)?;
                                Ok(Vec::new())
                            },
                        )
                        .is_err()
                {
                    Renewal::Failed
                } else {
                    result
                }
            };
            let how = |renewal: Renewal| super::sign_in_ladder::how_it_ended(renewal, exited.get());
            let _signin = signin;
            let ran = (|| -> Result<(), String> {
                store.check_signin_state(now)?;
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
                if result == Renewal::Renewed {
                    return Ok(());
                }
                if result.can_retry_authentication()
                    && !has_mailbox
                    && has_headless_profile
                    && let Some(fallback) = non_headless_fallback
                {
                    let again = run_attempt(&fallback, false);
                    return if again == Renewal::Renewed {
                        Ok(())
                    } else {
                        Err(how(again))
                    };
                }
                Err(how(result))
            })();
            let renewed = ran.is_ok();
            let now_done = crate::supervise::limit::unix_now().max(now);
            if let Some((route, read, inputs, log)) = &unattended {
                let (outcome, routes) = match &ran {
                    Ok(()) => (
                        accounts::AttemptOutcome::SignedIn,
                        route.as_str().to_owned(),
                    ),
                    Err(how) => (
                        accounts::AttemptOutcome::Failed,
                        super::sign_in_ladder::failed_words(*route, how, read),
                    ),
                };
                // A live success records its summary in the same transaction as its binding.
                // Failed attempts and the scripted test seam still name their original parent.
                if (!renewed || run_seam.is_some())
                    && let Some(observed) = attempt_snapshot.borrow().as_ref()
                {
                    note_attempt(&store, observed, &next_label, outcome, &routes, now_done);
                }
                let mut guard = ladder_guard
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let line = super::sign_in_ladder::log_line(&next_label, outcome.as_str(), &routes);
                if renewed {
                    guard.ladder.signed_in(&next_label);
                    drop(guard);
                    log(&line);
                } else {
                    let wait = guard.ladder.failed(&next_label, inputs, Instant::now());
                    drop(guard);
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
                let mut note = None;
                let _ = store.transact_logins_within(
                    &accounts::LOCK_LADDER,
                    now_done,
                    &mut note,
                    |st, logins| {
                        let bound = accounts::bound(&file_roster, logins);
                        let enrolled = accounts::enrolled(&bound);
                        Ok(accounts::complete_requested_pin(
                            st, &bound, &enrolled, request, now_done,
                        )
                        .into_iter()
                        .collect())
                    },
                );
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
/// in the background, where a headless browser is installed, the row has an email and the CLI
/// has a sign-in (one that opens the desk's browser runs under
/// [`super::login_drive::NO_DESK`] there, [`crate::ai::connector::SignIn::no_desk`]). A row
/// whose last one stopped for a person waits [`PERSON_BACKOFF`] ([`note_background_end`]).
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
    if store.check_signin_state(now).is_err() {
        return RenewalDispatch::Failed;
    }
    if next_row.email.is_none()
        || cli.accounts().signin.as_ref().is_none()
        || super::headless_oauth::find_headless_browser(Some(home)).is_none()
    {
        return RenewalDispatch::Failed;
    }
    let mailbox_mtime = latest_mailbox_mtime(Some(home), &accounts::Store::at(state_dir));
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
    match store.signin_now() {
        Ok(Some(probe)) => drop(probe),
        Ok(None) => return RenewalDispatch::Held,
        Err(_) => return RenewalDispatch::Failed,
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
    let own_dir = file_roster.row(&next_row.label).and_then(|r| r.dir.clone());
    let roster_path = roster_path.to_path_buf();
    let parker = std::sync::Arc::clone(renew_guard);
    let captured_logins = logins.clone();
    start_auto_renew(
        permit,
        move || {
            let renewal = super::cli::renew_one_for(
                cli,
                now,
                &super::cli::LiveAccounts,
                &store,
                (&next_row, &roster_path, &captured_logins),
                own_dir.as_deref(),
                &mut io::sink(),
                &mut io::sink(),
            );
            note_background_end(&parker, &key, renewal, Instant::now());
            if let Some(request) = &pin_request {
                let now_done = crate::supervise::limit::unix_now().max(now);
                match renewal {
                    super::cli::Renewal::Renewed => {
                        let mut note = None;
                        let _ = store.transact_logins_within(
                            &accounts::LOCK_LADDER,
                            now_done,
                            &mut note,
                            |st, logins| {
                                let bound = accounts::bound(&file_roster, logins);
                                let enrolled = accounts::enrolled_for(cli, &bound);
                                Ok(accounts::complete_requested_pin(
                                    st, &bound, &enrolled, request, now_done,
                                )
                                .into_iter()
                                .collect())
                            },
                        );
                    }
                    super::cli::Renewal::Busy => {}
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
    let machine = detect_machine_id(machine_override, env.config.as_deref())
        .unwrap_or_else(|| "unknown".to_owned());
    let mut report_out = Vec::new();
    let mut report_err = Vec::new();
    let _ = super::cli::run_accounts(
        &super::cli::AccountsCmd::Report { json: false },
        env,
        &super::cli::LiveAccounts,
        &mut report_out,
        &mut report_err,
    );
    let report_text = format!(
        "{}{}",
        String::from_utf8_lossy(&report_out),
        String::from_utf8_lossy(&report_err)
    );
    let store = env.accounts_store();
    let pool = (|| {
        let path = env.accounts_roster().ok_or("no account roster path")?;
        let raw = Roster::load(&path)?;
        let roster = store.bound(&raw, env.now)?;
        let (state, _) = store.load(env.now)?;
        Ok::<_, String>((roster, state))
    })();
    let (roster, state, mut rows_html) = match pool {
        Ok((roster, state)) => (roster, state, String::new()),
        Err(why) => (
            Roster::default(),
            accounts::State::default(),
            format!(
                "<tr><td colspan=\"6\">Pool state is unknown: {}</td></tr>",
                html_escape(&why)
            ),
        ),
    };
    let enrolled = accounts::enrolled(&roster);

    for row in &roster.rows {
        let here = row.dir.is_none() || enrolled.contains(&row.label);
        let cond = match accounts::condition(row, &state, here, env.now) {
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
         <div class=\"card\"><h2>Machine <code>{machine}</code> Accounts</h2>\
           <table><thead><tr><th>Label</th><th>Class</th><th>Condition</th><th>Role</th><th>Email / OAuth Sign-In Command</th><th>Action</th></tr></thead>\
           <tbody>{rows_html}</tbody></table>\
         </div>\
         {pools_html}\
         <div class=\"card\"><h2>Live Harness Report (<code>aterm harness accounts</code>)</h2>\
           <pre>{}</pre>\
         </div></body></html>",
        html_escape(&report_text),
        pools_html = env
            .config
            .as_deref()
            .zip(env.home.as_deref())
            .map(|(config, home)| pools_html(
                &cli_pools(config, &env.state, home, env.now),
                env.now
            ))
            .unwrap_or_default(),
    )
}

/// The dashboard's card for each other CLI's pool this machine keeps ([`cli_pools`]): its rows,
/// each with its vendor keys, condition and `--cli` renewal; its pins and sign-ins are its
/// commands' (`aterm harness accounts use|renew <label> --cli <id>`), never a button here.
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
                         <td>{}</td><td><code>aterm harness accounts renew {} --cli {id}</code>\
                         </td></tr>\n",
                        html_escape(&row.label),
                        html_escape(row.class.as_str()),
                        html_escape(&keys),
                        html_escape(&cond),
                        role,
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

#[cfg(test)]
#[path = "accounts_recovery_policy_tests.rs"]
mod recovery_policy_tests;

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
            Roster::load(&target)
                .unwrap()
                .row("m8")
                .unwrap()
                .browser
                .as_deref(),
            Some("new profile")
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
                &file_roster,
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
                sync_cooldown: &self.cooldown,
                rediscovery: &self.rediscovery,
                renew_guard: &self.guard,
                find_browser: &*self.browser,
                vault_sync: self.vault_sync,
                spawn_vault: &run_inline,
                mailbox: &ladder_mailbox_files,
                log: std::sync::Arc::new(move |line: &str| {
                    logged.lock().unwrap().push(line.to_owned());
                }),
                run_attempt: self.run_attempt.clone(),
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
            accounts::record_wall(&store, &file_roster, "sid-m15", &who, &wall, now - 60, now),
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
            |w: &World, expected: Option<Vec<u8>>, checked_path: &Path, publish: bool| {
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
                    },
                );
                assert_eq!(code, ExitCode::FAILURE);
                assert!(String::from_utf8_lossy(&err).contains("changed after init discovery"));
            };
        let roster_bytes = fs::read(w.roster()).unwrap();
        sync_across_change(&w, Some(captured.clone()), &w.fleet_file(), false);
        assert_eq!(fs::read(w.roster()).unwrap(), roster_bytes);
        assert_eq!(fs::read(w.fleet_file()).unwrap(), replacement.as_bytes());

        let first = World::new();
        first.write_config("[fleet]\nmachine = \"m27\"\n");
        assert!(!first.fleet_file().exists());
        sync_across_change(&first, None, &first.fleet_file(), false);
        assert!(!first.roster().exists());
        assert_eq!(
            fs::read(first.fleet_file()).unwrap(),
            replacement.as_bytes()
        );
        let first_secrets = first.home.join(".secrets");
        fs::create_dir(&first_secrets).unwrap();
        let first_source = first_secrets.join(FLEET_ROSTER_FILE);
        sync_across_change(&first, None, &first_source, true);
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
            sync_cooldown: &cooldown,
            rediscovery: &Mutex::new(Rediscovery::default()),
            renew_guard: &guard,
            find_browser: &|_: &Path| false,
            vault_sync: |_: &Path| super::super::sign_in_ladder::Vault::NotMember,
            spawn_vault: &|_work| Ok(()),
            mailbox: &ladder_mailbox_files,
            log: std::sync::Arc::new(|_: &str| {}),
            run_attempt: None,
        };
        let select = |label| match store.select_account(&roster, label, UPKEEP_NOW).unwrap() {
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
            Some(RenewalDispatch::Failed)
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
        let request = match store.select_account(&roster, "m27", UPKEEP_NOW).unwrap() {
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
                &captured,
                "main",
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
            );
            if changed.is_none() {
                assert_eq!(outcome, super::super::cli::Renewal::Renewed);
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
                    accounts::AttemptOutcome::SignedIn,
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
                store.save(&accounts::State::default()).unwrap();
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
            );
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
                        assert_eq!(
                            binding.get("main").unwrap().dir.as_deref(),
                            staging.to_str()
                        );
                        assert_eq!(
                            store.load(UPKEEP_NOW).unwrap().0.account("main").auth,
                            accounts::Auth::Ok
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
        assert_eq!(code_unconfigured, ExitCode::SUCCESS);
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
        assert_eq!(code_wrong_profile, ExitCode::SUCCESS);
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
            Vault::Applied(52)
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
#[path = "accounts_sync_ladder_tests.rs"]
mod ladder_tests;
