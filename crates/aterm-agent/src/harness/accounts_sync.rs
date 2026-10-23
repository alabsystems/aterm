// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Fleet and machine account synchronization (`aterm harness accounts sync`
//! and `aterm harness accounts serve`):
//!
//! * NOTHING HAS A DEFAULT SOURCE. A roster is read only from the source this
//!   machine's person named: the one a command names (`aterm harness accounts
//!   sync <path-or-url>`), else this machine's own `fleet-accounts.toml` where
//!   such a sync wrote it (it says so: [`OPT_IN_KEY`] `= "named"`), else
//!   `[fleet] accounts_source` in aterm.toml ([`configured_source`]). With
//!   none, nothing is fetched, read, synced or renewed ([`NO_SOURCE`]): no
//!   spreadsheet link, no rclone remote, no file in a folder macOS guards (a
//!   read of `~/Downloads` can put a privacy dialog on the person's screen).
//!   0.112.0 to 0.115.0 shipped a hard-coded sheet link, an rclone remote and
//!   a window upkeep that fetched them on every machine with no
//!   `accounts.toml`, and wrote `fleet-accounts.toml` from what came back; all
//!   three are gone, and a fleet file with no [`OPT_IN_KEY`] (every one those
//!   versions wrote) opts nothing in.
//! * Parses an accounting spreadsheet's CSV (a local file or an `http(s)://`
//!   export URL) or a LAN `fleet-accounts.toml` URL/path. An rclone remote
//!   (`rclone:<remote>:<path>`) is refused by name: aterm runs no rclone
//!   (init design §6.1 row 29).
//! * Writes `fleet-accounts.toml` beside `accounts.toml` so any machine on
//!   the LAN can sync from one shared config file, and writes this machine's
//!   `accounts.toml` (`m8` -> `m8` and `m8-2`), adopting whichever account is
//!   already signed into the default Claude Code home (`dir = None`) and
//!   creating isolated login directories (`<state>/accounts/logins/<label>`)
//!   for the remaining accounts.
//! * Scans the default Claude Code home's transcripts for any standing quota
//!   wall on the adopted default login and records it in
//!   `<state>/accounts/state.json` (touching `<state>/accounts/wake`) so a
//!   running `aterm` window immediately sees the wall and rotates once a
//!   second account is signed in.
//! * Serves a lightweight local/LAN HTTP dashboard and config endpoint
//!   (`aterm harness accounts serve`) for LAN sharing, status inspection, and
//!   account rotation control.
//! * The window's upkeep ([`upkeep_live`]): where a source is configured, the
//!   roster is synced where it is missing and a walled login's successor is
//!   signed in in the background; where none is, it does nothing at all
//!   ([`Upkept::Unconfigured`]).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, BufRead, Read, Seek, SeekFrom, Write};
use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use aterm_json::{Map, Value};

use super::accounts::{self, Class, Roster, Row};
use super::cli::{ACCOUNTS_OWN_LABEL, Env};
use super::upgrade;

/// The aterm.toml table whose [`SOURCE_KEY`] names this machine's fleet
/// accounts source.
pub const SOURCE_TABLE: &str = "fleet";

/// `[fleet] accounts_source = "<path | http(s) URL>"`:
/// where a machine with no `fleet-accounts.toml` yet syncs its roster from
/// ([`configured_source`]).
pub const SOURCE_KEY: &str = "accounts_source";

/// The prefix that names an rclone remote (`rclone:<remote>:<path>`), a source
/// refused by name ([`fetch_plan`]): aterm runs no rclone, so the sources are
/// a sheet's CSV export URL, a LAN `fleet-accounts.toml` and a local file
/// (init design §6.1 row 29).
pub const RCLONE_PREFIX: &str = "rclone:";

/// What a sync says when it was handed no source and none is configured:
/// nothing was fetched or read.
pub const NO_SOURCE: &str = "no accounts source is configured, so nothing was fetched or read: \
     name one (`aterm harness accounts sync <path-or-url>`) or set `[fleet] accounts_source` \
     in aterm.toml";

/// The development seam (`aterm_types::dev_seam!`) that gives an instance
/// which is not its account's own — a scratch HOME, a private state root —
/// the live window upkeep all the same: the jailed instance test's arm. A
/// shipped build reads no such variable, and a test binary ignores it.
pub const UPKEEP_SEAM: &str = "ATERM_DEBUG_ACCOUNTS_UPKEEP";

/// Shared fleet config file written beside `accounts.toml`.
pub const FLEET_ROSTER_FILE: &str = "fleet-accounts.toml";

/// The top-level key of this machine's own `fleet-accounts.toml` that says why
/// a sync wrote it ([`OptIn`]). Only `"named"` opts the machine in
/// ([`configured_source`]); a file without the key — every one 0.112.0 to
/// 0.115.0 wrote from their built-in source, unasked — opts nothing in. The
/// file the dashboard serves carries none ([`FleetRoster::to_toml`]).
pub const OPT_IN_KEY: &str = "opt_in";

/// Why a sync wrote this machine's `fleet-accounts.toml` ([`OPT_IN_KEY`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptIn {
    /// The person handed the sync its source (`aterm harness accounts sync
    /// <source>`), or it read a fleet file that said so: the file itself opts
    /// this machine in, until it is deleted.
    Named,
    /// The sync read `[fleet] accounts_source`: the key opts in, the file
    /// does not, so taking the key out opts the machine out.
    Key,
}

impl OptIn {
    /// The word written for it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Named => "named",
            Self::Key => "key",
        }
    }
}

/// Default bind address for `aterm harness accounts serve`.
pub const DEFAULT_SERVE_BIND: &str = "127.0.0.1:8741";

/// Tail bytes read from a transcript when checking for a standing quota wall.
const TRANSCRIPT_TAIL_BYTES: u64 = 256 * 1024;

/// One active Claude Code account in the fleet roster.
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
}

/// Complete fleet account table parsed from CSV or `fleet-accounts.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FleetRoster {
    /// Human-readable source description (path or URL; never an email).
    pub source: String,
    /// Active Claude accounts in deterministic order.
    pub accounts: Vec<FleetAccount>,
}

impl FleetRoster {
    /// Filter accounts belonging to `machine` (e.g. `"m8"`), returning rows in
    /// slot order (`m8` before `m8-2`, dedicated before shared).
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

    /// Serialize the fleet roster as `fleet-accounts.toml` for LAN sharing:
    /// no [`OPT_IN_KEY`], which only this machine's own copy carries
    /// ([`Self::to_toml_for`]).
    #[must_use]
    pub fn to_toml(&self) -> String {
        self.to_toml_for(None)
    }

    /// [`Self::to_toml`], with this machine's [`OPT_IN_KEY`] where `opt_in`
    /// names one.
    #[must_use]
    pub fn to_toml_for(&self, opt_in: Option<OptIn>) -> String {
        let mut out = String::from(
            "# Managed by `aterm harness accounts sync` — shared LAN fleet account roster.\n\
             # Run `aterm harness accounts sync <path-or-url>` on any machine to materialize\n\
             # its local `accounts.toml`.\n\n",
        );
        if let Some(opt_in) = opt_in {
            out.push_str(&format!(
                "# Why this machine keeps its fleet accounts: `named`, a sync handed its source\n\
                 # (delete this file to stop); `key`, `[fleet] accounts_source` in aterm.toml.\n\
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
            out.push('\n');
        }
        out
    }

    /// Parse `fleet-accounts.toml` text back into a [`FleetRoster`].
    ///
    /// # Errors
    /// The generated rows do not admit as one complete roster.
    ///
    /// Returns a human-readable error if the TOML is invalid or any row is
    /// missing required fields.
    pub fn from_toml(text: &str, source: &str) -> Result<Self, String> {
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
            accounts.push(FleetAccount {
                label,
                machines,
                class,
                email,
                browser,
            });
        }
        Ok(Self {
            source: source.to_owned(),
            accounts,
        })
    }
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

/// Normalize a machine token such as `"m8"`, `"M109"`, `"m8-Macbook-Pro"`, or
/// `"MacbookAir-m27-M5-32GB-4TB.local"` to its canonical lowercase `"m<digits>"`
/// identifier.
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

/// Parse a `computer` cell from the accounting spreadsheet (e.g. `"m8"`,
/// `"m8.2"`, `"M109"`, `"m38.106.2"`) into `(machines, slot_suffix)`.
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

/// Also check if an email local-part encodes a secondary shared machine
/// (e.g. `foxtrot.m38.106.2@example.test` or `golf.m41.109.2@example.test`).
fn machines_from_email_hint(email: &str) -> Vec<String> {
    let Some((local, _)) = email.split_once('@') else {
        return Vec::new();
    };
    let tokens: Vec<&str> = local.split(['.', '-', '_']).collect();
    let mut out = Vec::new();
    for (idx, tok) in tokens.iter().enumerate() {
        if let Some(m) = normalize_machine_id(tok) {
            if !out.contains(&m) {
                out.push(m);
            }
            if let Some(next) = tokens.get(idx + 1)
                && next.len() >= 2
                && next.bytes().all(|b| b.is_ascii_digit())
            {
                let m2 = format!("m{next}");
                if !out.contains(&m2) {
                    out.push(m2);
                }
            }
        }
    }
    out
}

/// Borrow complete CSV records, keeping line endings inside quoted fields.
/// Each record is scanned once and passed straight to the existing field decoder.
/// Quotes must open a field; after they close only a delimiter or record end is valid.
fn csv_records(mut text: &str) -> impl Iterator<Item = Result<&str, &'static str>> {
    std::iter::from_fn(move || {
        let input = text;
        if input.is_empty() {
            return None;
        }
        // An error ends the iterator; no partial record can become another row.
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

/// Split one admitted CSV record into fields.
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

/// Whether a row's notes mark the account unusable (`banned`, `roll off`,
/// `payment disabled`, `deactivated`, `suspended`).
fn notes_disqualify(notes: &str) -> bool {
    let lower = notes.to_ascii_lowercase();
    lower.contains("banned")
        || lower.contains("roll off")
        || lower.contains("payment disabled")
        || lower.contains("deactivated")
        || lower.contains("suspended")
}

/// Parse the accounting CSV into a [`FleetRoster`].
///
/// # Errors
/// The generated rows do not admit as one complete roster.
///
/// Returns an error if the CSV has no header row, no `email` / `claude`
/// columns, or malformed quoting. LF and CRLF separate records only outside quotes.
pub fn parse_accounting_csv(csv_text: &str, source: &str) -> Result<FleetRoster, String> {
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
    let claude_col = header
        .iter()
        .position(|h| h.trim().eq_ignore_ascii_case("claude"))
        .ok_or_else(|| format!("CSV at {source} has no `claude` column"))?;
    let mut computer_cols: Vec<usize> = header
        .iter()
        .enumerate()
        .filter(|(_, h)| h.trim().eq_ignore_ascii_case("computer"))
        .map(|(i, _)| i)
        .collect();
    // In an accounting sheet's CSV export, the column right after `computer`
    // may hold the paired machine (`m38`, `m42`, …) under an empty header.
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

    for record in records {
        let raw_line = record.map_err(|why| format!("CSV at {source}: {why}"))?;
        if raw_line.trim().is_empty() {
            continue;
        }
        let cols = parse_csv_line(raw_line);
        // Skip rows marked with `*` in column 0 or email starting with `*`.
        if cols.first().is_some_and(|c| c.trim().starts_with('*')) {
            continue;
        }
        let email = cols.get(email_col).map(|s| s.trim()).unwrap_or("");
        if email.is_empty() || email.starts_with('*') || accounts::email_refusal(email).is_some() {
            continue;
        }
        let claude_val = cols.get(claude_col).map(|s| s.trim()).unwrap_or("");
        if claude_val != "1" {
            continue;
        }
        let notes = if notes_start < cols.len() {
            cols[notes_start..].join(" ")
        } else {
            String::new()
        };
        if notes_disqualify(&notes) {
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
        for hint_m in machines_from_email_hint(email) {
            if !machines.contains(&hint_m) {
                machines.push(hint_m);
            }
        }

        let email_lower = email.to_ascii_lowercase();
        if !seen_emails.insert(email_lower) {
            continue;
        }

        let base_label = match (&machines[..], slot) {
            ([only], None | Some(1)) => only.clone(),
            ([only], Some(n)) => format!("{only}-{n}"),
            (many, None | Some(1)) => many.join("-"),
            (many, Some(n)) => format!("{}-{n}", many.join("-")),
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

        accounts.push(FleetAccount {
            label,
            machines,
            class,
            email: email.to_owned(),
            browser: None,
        });
    }

    Ok(FleetRoster {
        source: source.to_owned(),
        accounts,
    })
}

/// Detect this machine's canonical ID (`"m8"`, `"m14"`, …) from an explicit
/// override, `aterm.toml`, or the system hostname (`scutil --get LocalHostName`
/// / `hostname`).
#[must_use]
pub fn detect_machine_id(override_machine: Option<&str>, config: Option<&Path>) -> Option<String> {
    if let Some(m) = override_machine {
        return normalize_machine_id(m);
    }
    if let Some(cfg_path) = config
        && let Ok(text) = fs::read_to_string(cfg_path)
        && let Ok(table) = aterm_toml::from_str::<aterm_toml::Table>(&text)
    {
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

/// Read the `oauthAccount.emailAddress` field from a Claude Code `.claude.json`
/// profile IN MEMORY ONLY for matching against a roster row's `email`
/// ([`accounts::email_matches`]). Never logged, printed, or persisted.
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

/// Find the newest standing quota wall across `<claude_dir>/projects/*/*.jsonl`
/// in the default Claude Code home, so syncing on a machine whose default
/// login is already at a usage wall immediately records that wall in
/// `<state>/accounts/state.json`.
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

/// Match a `"session_context"` transcript line's `"userEmail"` against `roster`
/// IN MEMORY ONLY, returning `Some(Some(label))` when the line names an email
/// in `roster`, `Some(None)` when the line names an email not in `roster`, or
/// `None` when the line is not a `"session_context"` with `"userEmail"`.
fn match_session_context_label(line: &str, roster: &Roster) -> Option<Option<String>> {
    if !line.contains("\"session_context\"") || !line.contains("\"userEmail\"") {
        return None;
    }
    let v: Value = aterm_json::from_str(line).ok()?;
    let attachment = v.get("attachment")?;
    if attachment.get("type").and_then(Value::as_str) != Some("session_context") {
        return None;
    }
    let user_email = attachment.get("context")?.get("userEmail")?.as_str()?;
    let tok = user_email
        .split([' ', '\t', '\n', '\r', '<', '>', '"', '\''])
        .map(|s| s.trim_end_matches(['.', ',', ';', ':']))
        .find(|s| s.contains('@'))?;
    let matched = roster
        .rows
        .iter()
        .find(|r| {
            r.email
                .as_deref()
                .is_some_and(|re| accounts::email_matches(re, tok))
        })
        .map(|r| r.label.clone());
    Some(matched)
}

/// Scan recent `.jsonl` transcripts under `<claude_dir>/projects/*/*.jsonl`
/// (up to 16 newest files by modification time), attributing quota walls and
/// model answers to the active account row tracked via `"session_context"`
/// `"userEmail"` entries (falling back to the roster's default login row when
/// a transcript has no `"session_context"`). Returns each roster label's
/// newest standing quota wall (`until > now`) that was not cleared by a newer
/// model turn under that same label.
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

/// Build the machine's [`Roster`] from `fleet` for `machine`, choosing at most
/// one row as the default login (`dir = None`) and assigning `<state>/accounts/logins/<label>`
/// to the remaining rows.
#[must_use]
pub fn build_machine_roster(
    fleet: &FleetRoster,
    machine: &str,
    store: &accounts::Store,
    existing: Option<&Roster>,
    default_email: Option<&str>,
) -> Roster {
    let fleet_rows = fleet.accounts_for_machine(machine);
    if fleet_rows.is_empty() {
        return existing.cloned().unwrap_or_default();
    }

    // Determine which label gets `dir = None` (the machine's default login):
    // 1. The row whose email matches `~/.claude.json`'s `oauthAccount.emailAddress`.
    // 2. Otherwise, a row that already had `dir = None` in `existing`.
    // 3. Otherwise, the first row for this machine.
    let default_label: String = default_email
        .and_then(|de| {
            fleet_rows
                .iter()
                .find(|r| accounts::email_matches(&r.email, de))
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
            browser,
            was: Vec::new(),
        });
    }

    // Put the default login row (`dir = None`) first if it is currently active
    // or so `accounts_next` naturally treats it as the starting login when
    // `state.active` is `None`.
    rows.sort_by_key(|r| (r.dir.is_some(), r.class, r.label.clone()));

    Roster { rows }
}

/// Format a [`Roster`] into valid `accounts.toml` text.
///
/// # Errors
/// The generated rows do not admit as one complete roster.
pub fn format_roster_toml(roster: &Roster) -> Result<String, String> {
    accounts::append_rows("", &roster.rows)
}

fn toml_quote(s: &str) -> String {
    aterm_toml::Value::String(s.to_owned()).to_string()
}

/// Extract `(machines, slot_suffix)` from an account email local-part such as
/// `"sol-m27@example.test"` -> `(["m27"], None)`, `"sun.m27.2@example.test"` ->
/// `(["m27"], Some(2))`, or `"yellow.m38.106.2@example.test"` ->
/// `(["m38", "m106"], Some(2))`.
fn email_machines_and_slot(email: &str) -> (Vec<String>, Option<u32>) {
    let Some((local, _)) = email.split_once('@') else {
        return (Vec::new(), None);
    };
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
                let is_last = idx + 1 + rem_idx + 1 == tokens.len();
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

/// Where this machine's person said the fleet accounts come from
/// ([`configured_source`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FleetSource {
    /// This machine's own `fleet-accounts.toml` beside `accounts.toml`, as a
    /// sync handed its source wrote it ([`OptIn::Named`]).
    File(PathBuf),
    /// `[fleet] accounts_source` in aterm.toml, as written ([`SOURCE_KEY`]).
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

    /// Which of the two it is, for a log line: never the source itself, whose
    /// link may be a secret of its own.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::File(_) => "fleet-file",
            Self::Key(_) => "key",
        }
    }
}

/// THE ONE OPT-IN of the fleet accounts feature: the source this machine's
/// person configured, or `None`, and then nothing is fetched, read, synced or
/// renewed — not by a sync handed no source ([`load_fleet_source`]), not by
/// the window's upkeep ([`upkeep_live`]). First this machine's own
/// `fleet-accounts.toml` beside `roster` where it says a sync handed its
/// source wrote it ([`OPT_IN_KEY`] `= "named"`), else `[fleet]
/// accounts_source` in the aterm.toml at `config` (a string that is not
/// blank; a file that is not TOML names none). A fleet file without that
/// word — one 0.112.0 to 0.115.0 wrote from their built-in source, unasked, or
/// one a sync from the key wrote — is no opt-in ([`fleet_file_ignored`]). A
/// fresh home — every new install and every test world — has neither. There
/// is NO DEFAULT: no spreadsheet link, no rclone remote, no file in
/// `~/Downloads`.
#[must_use]
pub fn configured_source(config: Option<&Path>, roster: Option<&Path>) -> Option<FleetSource> {
    if let Some(fleet) = roster
        .and_then(fleet_file_beside)
        .filter(|fleet| fleet_file_opt_in(fleet) == Some(OptIn::Named))
    {
        return Some(FleetSource::File(fleet));
    }
    config.and_then(source_key).map(FleetSource::Key)
}

/// Whether a `fleet-accounts.toml` stands beside `roster` that opts nothing
/// in ([`configured_source`]): what the window's log says where no source is
/// configured, so a person whose machine kept the file an older aterm wrote
/// learns it is no longer read.
#[must_use]
pub fn fleet_file_ignored(roster: &Path) -> bool {
    fleet_file_beside(roster)
        .is_some_and(|fleet| fleet.is_file() && fleet_file_opt_in(&fleet) != Some(OptIn::Named))
}

/// This machine's own `fleet-accounts.toml`, beside `roster`.
fn fleet_file_beside(roster: &Path) -> Option<PathBuf> {
    roster.parent().map(|dir| dir.join(FLEET_ROSTER_FILE))
}

/// What the fleet file at `fleet` says of why it was written ([`OPT_IN_KEY`]):
/// `None` where it is missing, is not TOML, or says nothing (as every one
/// 0.112.0 to 0.115.0 wrote).
fn fleet_file_opt_in(fleet: &Path) -> Option<OptIn> {
    let text = read_local_source_text(fleet).ok()?;
    let table: aterm_toml::Table = aterm_toml::from_str(&text).ok()?;
    match table.get(OPT_IN_KEY)?.as_str()? {
        "named" => Some(OptIn::Named),
        "key" => Some(OptIn::Key),
        _ => None,
    }
}

/// `[fleet] accounts_source` in the aterm.toml at `config`, when it is a string
/// that is not blank.
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

/// How a source's text is fetched: [`fetch_source_text`] live, a test's
/// recorder in a test.
type Fetch = dyn Fn(&Env, &str) -> Result<String, String> + Sync;

/// Load a [`FleetRoster`] from `source`, else from the source this machine
/// configured ([`configured_source`]); with neither, nothing is read and the
/// answer is [`NO_SOURCE`].
///
/// # Errors
/// The generated rows do not admit as one complete roster.
///
/// No source, or the source could not be read or parsed.
pub fn load_fleet_source(env: &Env, source: Option<&str>) -> Result<FleetRoster, String> {
    load_fleet_source_with(env, source, &fetch_source_text).map(|(fleet, _)| fleet)
}

/// [`load_fleet_source`] over `fetch`, with why it may be kept
/// ([`OptIn`]): `source` itself, or this machine's own named fleet file, is
/// [`OptIn::Named`]; the key is [`OptIn::Key`].
fn load_fleet_source_with(
    env: &Env,
    source: Option<&str>,
    fetch: &Fetch,
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
    let fleet = if chosen.ends_with(".toml")
        || raw.trim_start().starts_with('#')
        || raw.contains("[[account]]")
    {
        FleetRoster::from_toml(&raw, &chosen)
    } else {
        parse_accounting_csv(&raw, &chosen)
    }?;
    Ok((fleet, opt_in))
}

/// Discover accounts for this machine from local `.claude.json*` profiles,
/// `.claude/backups/`, `.config/muse/auth.json`, and recent
/// `.claude/projects/*/*.jsonl` `"session_context"` `"userEmail"` lines when
/// external CSV/sheet sources are unreachable or incomplete.
fn discover_local_fleet(env: &Env) -> Option<FleetRoster> {
    discover_local_fleet_with_min(env, 2)
}

fn discover_local_fleet_with_min(env: &Env, min_accounts: usize) -> Option<FleetRoster> {
    let fallback_machine = detect_machine_id(None, env.config.as_deref()).or_else(|| {
        read_default_profile_email(env)
            .as_deref()
            .and_then(|de| machines_from_email_hint(de).into_iter().next())
    })?;
    let mut emails = BTreeSet::new();
    let mut record_email = |candidate: &str| {
        let trimmed = candidate.trim();
        if !trimmed.is_empty() && accounts::email_refusal(trimmed).is_none() {
            emails.insert(trimmed.to_owned());
        }
    };
    if let Some(de) = read_default_profile_email(env) {
        record_email(&de);
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
                    record_email(em);
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
                    record_email(em);
                }
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
                    record_email(em);
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
            files_with_mtime.truncate(32);
            for (_, path) in files_with_mtime {
                let Ok(text) = read_local_source_text(&path) else {
                    continue;
                };
                for line in text.lines() {
                    if line.contains("\"session_context\"")
                        && line.contains("\"userEmail\"")
                        && let Ok(v) = aterm_json::from_str::<Value>(line)
                        && let Some(user_email) = v
                            .get("attachment")
                            .filter(|a| {
                                a.get("type").and_then(Value::as_str) == Some("session_context")
                            })
                            .and_then(|a| a.get("context"))
                            .and_then(|c| c.get("userEmail"))
                            .and_then(Value::as_str)
                        && let Some(tok) = user_email
                            .split([' ', '\t', '\n', '\r', '<', '>', '"', '\''])
                            .map(|s| s.trim_end_matches(['.', ',', ';', ':']))
                            .find(|s| s.contains('@'))
                    {
                        record_email(tok);
                    }
                }
            }
        }
    }
    if emails.is_empty() {
        return None;
    }
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
        let base_label = match (&machines[..], slot) {
            ([only], None | Some(1)) => only.clone(),
            ([only], Some(n)) => format!("{only}-{n}"),
            (many, None | Some(1)) => many.join("-"),
            (many, Some(n)) => format!("{}-{n}", many.join("-")),
        };
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
        });
    }
    (accounts.len() >= min_accounts).then_some(FleetRoster {
        source: "local-claude-transcripts".to_owned(),
        accounts,
    })
}

const SOURCE_TEXT_MAX_BYTES: usize = 4 * 1024 * 1024;

/// Admit a whole source before its text can become a roster.
fn decode_source_text(bytes: Vec<u8>, source: &str) -> Result<String, String> {
    if bytes.len() > SOURCE_TEXT_MAX_BYTES {
        return Err(format!(
            "account source at {source} exceeds the {SOURCE_TEXT_MAX_BYTES}-byte limit"
        ));
    }
    String::from_utf8(bytes)
        .map_err(|e| format!("account source at {source} is not valid UTF-8: {e}"))
}

/// Admit local discovery text from the opened regular file, never a FIFO or
/// an oversized prefix. On Unix the open also refuses the final symbolic link;
/// other platforms use `astream_sys`'s read-only open.
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

/// How one named source is read ([`fetch_plan`]).
#[derive(Debug, Clone, PartialEq, Eq)]
enum FetchPlan {
    /// `curl` of an `http(s)://` URL (a Sheets `/edit` link as its CSV export).
    Http(String),
    /// A local file (a leading `~/` is the home's).
    File(PathBuf),
}

/// What reading `source` takes: curl only for an `http(s)://` URL, else a
/// file. Nothing is added to it: no fallback source, no default remote.
///
/// # Errors
///
/// An rclone remote ([`RCLONE_PREFIX`]), refused by name with the sources
/// that remain.
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

/// How a fetch's one program is run: [`run_fetcher`] live, a test's witness
/// in a test, so the composition of [`fetch_source_text_with`] is seen
/// without curl running.
type RunFetcher =
    dyn Fn(aterm_uds::spawnfd::Command) -> Result<super::align::Captured, String> + Sync;

/// The live [`RunFetcher`]: the program bounded at 20 s, its output at
/// [`SOURCE_TEXT_MAX_BYTES`] and one byte more (so an oversized answer is
/// refused, not cut).
fn run_fetcher(cmd: aterm_uds::spawnfd::Command) -> Result<super::align::Captured, String> {
    super::align::capture_bounded(cmd, Duration::from_secs(20), SOURCE_TEXT_MAX_BYTES + 1)
}

fn fetch_http(url: &str, run: &RunFetcher) -> Result<String, String> {
    let mut cmd = aterm_uds::spawnfd::command("/usr/bin/curl");
    // Ignore default curl config and keep the initial request and redirects HTTP(S).
    // `-q` must be first; `--` keeps the URL out of option parsing.
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

/// The live [`Fetch`]: the one source named, read as [`fetch_plan`] says.
fn fetch_source_text(env: &Env, source: &str) -> Result<String, String> {
    fetch_source_text_with(env, source, &run_fetcher)
}

/// [`fetch_source_text`] over `run`: one program at most (curl for a link,
/// none for a file or a refused rclone remote), and its failure is the
/// answer — nothing else is tried after it.
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

/// The files and report state published by one successful sync.
struct SyncedAccounts {
    roster: Roster,
    fleet_path: PathBuf,
    state: accounts::State,
    note: Option<String>,
}

/// The fleet bytes init captured before taking the account lock, or its
/// absence for a first discovery. Other syncs intentionally replace a source.
struct FleetSnapshot(Option<Vec<u8>>);

/// How long a sync from the command line waits for `accounts.lock` before it
/// gives up and says so ([`persist_accounts_sync`]): the host's own bounded
/// wait ([`accounts::LOCK_LADDER`], two seconds), then sixteen half-second
/// tries more, ten seconds in all. The lock is a short one; a holder that keeps
/// it past this (a suspended `aterm harness accounts`) is not waited out.
fn sync_lock_ladder() -> Vec<Duration> {
    accounts::LOCK_LADDER
        .iter()
        .copied()
        .chain(std::iter::repeat_n(Duration::from_millis(500), 16))
        .collect()
}

/// Persist captured sync inputs. Source/profile/transcript discovery happens
/// before this call; the account lock spans the roster's read/build/write and
/// the existing state/login transaction. Init's locally captured fleet also
/// requires unchanged fleet bytes under that lock. Individual replacements
/// are durable writes, not an atomic transaction across all these files.
///
/// THE LOCK IS WAITED FOR WITHIN `ladder` ([`accounts::Store::acquire_within`]):
/// still held after it, the sync fails before it reads or writes anything and
/// says so — [`accounts::LOCK_HELD`], the store, how long it waited, and that
/// nothing was synced.
fn persist_accounts_sync(
    store: &accounts::Store,
    roster_path: &Path,
    (fleet, opt_in): (&FleetRoster, OptIn),
    (machine, default_email): (&str, Option<&str>),
    standing_walls: &BTreeMap<String, upgrade::StandingWall>,
    now: i64,
    (ladder, fleet_snapshot): (&[Duration], Option<&FleetSnapshot>),
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
    if let Some(expected) = fleet_snapshot {
        let current = match read_local_source_text(&fleet_path) {
            Ok(text) => Some(text.into_bytes()),
            Err(why) if why.kind() == io::ErrorKind::NotFound => None,
            Err(why) => {
                return Err(format!(
                    "could not check captured fleet at {}: {why}; nothing was synced",
                    fleet_path.display()
                ));
            }
        };
        if current != expected.0 {
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
    let default_matched = default_email.is_some_and(|de| {
        roster
            .row_for_login(None)
            .and_then(|r| r.email.as_deref())
            .is_some_and(|re| accounts::email_matches(re, de))
    });
    let mut note = None;
    let mut publishing = false;
    // The held API reads and admits logins and state before invoking this edit.
    // Re-acquiring the store here would deadlock the lock already held above.
    store
        .transact_logins_held(&held, now, &mut note, |state, logins| {
            publishing = true;
            for row in &roster.rows {
                if let Some(dir) = &row.dir {
                    aterm_types::fs_restricted::ensure_private_dir(&store.logins_dir())
                        .and_then(|()| {
                            aterm_types::fs_restricted::ensure_private_dir(Path::new(dir))
                        })
                        .map_err(|why| format!("could not create {dir}: {why}"))?;
                }
            }
            super::upgrade_models::write_atomic_mode(&write_to, &roster_toml, keep_mode)
                .map_err(|why| format!("could not write {}: {why}", write_to.display()))?;
            super::upgrade_models::write_atomic_mode(&fleet_write_to, &fleet_toml, fleet_mode)
                .map_err(|why| format!("could not write {}: {why}", fleet_write_to.display()))?;
            let mut events = Vec::new();
            if let Some(label) = &default_row_label {
                if default_matched {
                    accounts::note_served_login(logins, label, now);
                    events.extend(accounts::note_auth(state, label, accounts::Auth::Ok, now));
                }
                // Retain the initial adoption of the default login's walls.
                if let Some(own_acct) = state.accounts.get(ACCOUNTS_OWN_LABEL).cloned() {
                    for (win, wall) in own_acct.walls.into_iter().filter(|(_, wall)| !wall.cleared)
                    {
                        events.extend(accounts::note_wall(
                            state, label, &win, wall.until, wall.at, now,
                        ));
                    }
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
    // A roster change must wake readers even when no state event changed.
    store.touch_wake().map_err(|why| {
        format!(
            "sync files were published but the wake marker could not be written: {why}; \
             the published files are kept"
        )
    })?;
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
        roster,
        fleet_path,
        state,
        note,
    })
}

/// Run `aterm harness accounts sync [<source>] [--machine <mN>] [--json]`:
/// from `source`, else the source this machine configured
/// ([`configured_source`]); with neither, nothing is read ([`NO_SOURCE`],
/// exit 1).
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
        fleet_snapshot: None,
    };
    run_accounts_sync_with(env, (source, machine_override, json), (out, err), &how)
}

/// What one sync reads its source with and how long it waits for the lock:
/// the command line's ([`run_accounts_sync`]), the window's upkeep's
/// ([`upkeep`]) or a test's. Init also binds its captured fleet snapshot.
struct SyncHow<'a> {
    fetch: &'a Fetch,
    ladder: &'a [Duration],
    fleet_snapshot: Option<&'a FleetSnapshot>,
}

/// [`run_accounts_sync`] over `how`.
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
    let machine = detect_machine_id(machine_override, env.config.as_deref()).or_else(|| {
        default_email.as_deref().and_then(|de| {
            fleet
                .accounts
                .iter()
                .find(|a| accounts::email_matches(&a.email, de))
                .and_then(|a| a.machines.first().cloned())
        })
    });
    let Some(machine) = machine else {
        let _ = writeln!(
            err,
            "aterm harness accounts sync: could not determine machine id (`m<N>`); pass `--machine m8`"
        );
        return ExitCode::FAILURE;
    };

    // The potentially slow discovery is outside the short account lock.
    let store = env.accounts_store();
    let prelim_roster =
        build_machine_roster(&fleet, &machine, &store, None, default_email.as_deref());
    let standing_walls = env
        .claude_dir()
        .map(|dir| find_transcript_walls_by_email(&dir, &prelim_roster, env.now))
        .unwrap_or_default();
    let SyncedAccounts {
        roster,
        fleet_path,
        state,
        note,
    } = match persist_accounts_sync(
        &store,
        &roster_path,
        (&fleet, opt_in),
        (&machine, default_email.as_deref()),
        &standing_walls,
        env.now,
        (how.ladder, how.fleet_snapshot),
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
    let enrolled = accounts::enrolled(&roster);
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
            "synced machine `{machine}` ({} accounts on this machine, {} across fleet)",
            roster.rows.len(),
            fleet.accounts.len()
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
                "next step to sign in remaining accounts: aterm harness accounts renew {}",
                needs_renew.join(" ")
            );
        }
    }
    ExitCode::SUCCESS
}

/// The window's sync's cooldown: one try in ten minutes per process
/// ([`sync_if_missing`]).
static AUTO_SYNC_COOLDOWN: std::sync::Mutex<Option<Instant>> = std::sync::Mutex::new(None);

/// Which of the upkeep's two works one call makes ([`upkeep_live`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpkeepStep {
    /// Both, the sync first: the activation wake's upkeep.
    Both,
    /// The sync of a missing roster alone: before a wall is read against the
    /// roster, so a wall is recorded against the rows the sync wrote.
    Sync,
    /// The renewal alone: after a wall is recorded, so the wall just recorded
    /// is the one renewed.
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
    /// No source is configured ([`configured_source`]): nothing was read,
    /// fetched, synced or renewed, and no program was started.
    /// `fleet_file_ignored`: a `fleet-accounts.toml` stands that opts nothing
    /// in ([`fleet_file_ignored`]), one an older aterm wrote unasked, say.
    Unconfigured {
        /// [`fleet_file_ignored`].
        fleet_file_ignored: bool,
    },
    /// A source is configured (`kind`, [`FleetSource::kind`]); the roster
    /// stands (`roster`: it stood, or the sync wrote it) and a renewal's
    /// worker was started or not (`renewal`).
    Configured {
        /// [`FleetSource::kind`].
        kind: &'static str,
        /// `accounts.toml` exists after the upkeep.
        roster: bool,
        /// A background sign-in was started ([`auto_renew_next_account`]).
        renewal: bool,
    },
}

/// What the upkeep may do past reading its own files: fetch a source, start a
/// renewal's worker, and the two process-wide gates (the sync's cooldown and
/// the renewal's admission). The live ones in [`upkeep_live`]; a test's own
/// in a test, so none reaches curl, `claude` or another test's gates.
struct UpkeepEffects<'a> {
    fetch: &'a Fetch,
    spawn_renewal: &'a SpawnRenewal,
    sync_cooldown: &'a std::sync::Mutex<Option<Instant>>,
    renew_guard: &'a std::sync::Arc<std::sync::Mutex<AutoRenewGuard>>,
}

/// How a renewal's worker is started: [`spawn_renewal_thread`] live, a test's
/// recorder in a test.
type SpawnRenewal = dyn Fn(Box<dyn FnOnce() + Send>) -> io::Result<()> + Sync;

/// The live renewal worker: a thread of its own.
fn spawn_renewal_thread(work: Box<dyn FnOnce() + Send>) -> io::Result<()> {
    std::thread::Builder::new()
        .name("aterm-auto-renew".to_owned())
        .spawn(work)
        .map(|_| ())
}

/// THE WINDOW'S FLEET ACCOUNTS' UPKEEP, the one way a window reaches this
/// module: with the source this machine's person configured
/// ([`configured_source`], read again at every upkeep, beside `roster_path`),
/// `accounts.toml` is synced where it is missing ([`sync_if_missing`]) and a
/// walled login's successor is signed in in the background
/// ([`auto_renew_next_account`]); with none — every fresh home, every new
/// install, every test world — it returns [`Upkept::Unconfigured`] having read
/// nothing past aterm.toml and the fleet file beside it, fetched nothing and
/// started nothing. The aterm.toml read is the one beside `roster_path`.
/// `step` says which of the two works this call makes ([`UpkeepStep`]).
pub fn upkeep_live(
    home: &Path,
    state_dir: &Path,
    roster_path: &Path,
    now: i64,
    step: UpkeepStep,
) -> Upkept {
    upkeep(
        home,
        state_dir,
        (roster_path, now, step),
        &UpkeepEffects {
            fetch: &fetch_source_text,
            spawn_renewal: &spawn_renewal_thread,
            sync_cooldown: &AUTO_SYNC_COOLDOWN,
            renew_guard: &AUTO_RENEW_GUARD,
        },
    )
}

/// [`upkeep_live`] over `fx`.
fn upkeep(
    home: &Path,
    state_dir: &Path,
    (roster_path, now, step): (&Path, i64, UpkeepStep),
    fx: &UpkeepEffects<'_>,
) -> Upkept {
    let config = config_beside(roster_path);
    let Some(source) = configured_source(Some(&config), Some(roster_path)) else {
        return Upkept::Unconfigured {
            fleet_file_ignored: fleet_file_ignored(roster_path),
        };
    };
    let roster = if step.syncs() {
        sync_if_missing(home, state_dir, roster_path, &config, now, fx)
    } else {
        roster_path.exists()
    };
    let renewal = step.renews() && auto_renew_next_account(home, state_dir, roster_path, now, fx);
    Upkept::Configured {
        kind: source.kind(),
        roster,
        renewal,
    }
}

/// The aterm.toml `roster` sits beside.
fn config_beside(roster: &Path) -> PathBuf {
    roster
        .parent()
        .map(|p| p.join("aterm.toml"))
        .unwrap_or_else(|| PathBuf::from("aterm.toml"))
}

/// Sync `accounts.toml` from the source configured beside `config` when `roster_path` is
/// missing, bounded by a 10-minute cooldown (`fx`'s) so a machine whose
/// source cannot be reached is not asked again on every upkeep, and by the
/// host's bounded wait for the store's lock ([`accounts::LOCK_LADDER`]).
/// Returns `true` when `roster_path` exists.
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
        fleet_snapshot: None,
    };
    // Handed no source: the sync reads the one configured, as configured, so
    // the fleet file it writes says why ([`OptIn`]) and a key taken out later
    // takes the machine out.
    let _ = run_accounts_sync_with(&env, (None, None, true), (&mut out, &mut err), &how);
    roster_path.exists()
}

/// Whether a fleet accounts source is configured or discoverable for [`sync_accounts_for_init`]
/// (`[fleet] accounts_source`, `fleet-accounts.toml`, `~/.secrets/fleet-accounts.toml` or
/// `ai-accounts.csv`, or local Claude profiles/transcripts for this machine).
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
    if [FLEET_ROSTER_FILE, "ai-accounts.csv"]
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
        codex_home: None,
        now,
        tz_s: 0,
        sid: String::new(),
        sock: None,
        config: Some(config_path),
        claude_tmp: None,
        claude_running: None,
    };
    discover_local_fleet(&env).is_some()
}

/// Synchronize `accounts.toml` and `fleet-accounts.toml` (and `~/.secrets/fleet-accounts.toml`
/// when `~/.secrets` exists) during `aterm init` or `aterm secrets`.
/// Returns `Ok((machine_id, machine_account_count, enrolled_count, fleet_account_count))`.
pub fn sync_accounts_for_init(
    home: &Path,
    state_dir: &Path,
    roster_path: &Path,
    now: i64,
) -> Result<(String, usize, usize, usize), String> {
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
    let has_named_fleet = fleet_file_opt_in(&fleet_path) == Some(OptIn::Named);
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
    let mut fleet_snapshot = None;
    let init_source = if has_key && !has_named_fleet {
        None
    } else if let Some(src) = secrets_source {
        Some(src)
    } else if configured_source(env.config.as_deref(), Some(roster_path)).is_some() {
        if has_named_fleet
            && let Ok(existing_text) = read_local_source_text(&fleet_path)
            && let Ok(mut existing_fleet) =
                FleetRoster::from_toml(&existing_text, &fleet_path.display().to_string())
            && let Some(local) = discover_local_fleet_with_min(&env, 1)
        {
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
                    let base_label = acct.label.clone();
                    let mut suffix = 2u32;
                    loop {
                        if let Some(why) = accounts::label_refusal(&acct.label) {
                            return Err(format!("discovered account label {} {why}", acct.label));
                        }
                        if used_labels.insert(acct.label.clone()) {
                            break;
                        }
                        acct.label = format!("{base_label}-{suffix}");
                        suffix = suffix.checked_add(1).ok_or_else(|| {
                            "discovered account label suffixes exhausted".to_owned()
                        })?;
                    }
                    existing_fleet.accounts.push(acct);
                    added = true;
                }
            }
            if added {
                captured_fleet = Some((
                    fleet_path.to_string_lossy().into_owned(),
                    existing_fleet.to_toml_for(Some(OptIn::Named)),
                ));
                fleet_snapshot = Some(FleetSnapshot(Some(existing_text.into_bytes())));
            }
        }
        None
    } else if fleet_path.is_file() {
        Some(fleet_path.to_string_lossy().into_owned())
    } else {
        discover_local_fleet(&env).map(|local| {
            let source = fleet_path.to_string_lossy().into_owned();
            captured_fleet = Some((source.clone(), local.to_toml_for(Some(OptIn::Named))));
            fleet_snapshot = Some(FleetSnapshot(None));
            source
        })
    };
    // Discovery stays in memory until the normal sync admits the roster and
    // account state under its lock. Only that publication path writes files.
    let fetch = move |env: &Env, source: &str| {
        if let Some((path, text)) = &captured_fleet
            && source == path
        {
            Ok(text.clone())
        } else {
            fetch_source_text(env, source)
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
            fleet_snapshot: fleet_snapshot.as_ref(),
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
    let store = accounts::Store::at(state_dir);
    let raw = Roster::load(roster_path)?;
    let bound = store.bound(&raw, now).unwrap_or(raw);
    let enrolled = accounts::enrolled(&bound);
    let enrolled_count = bound
        .rows
        .iter()
        .filter(|r| r.dir.is_none() || enrolled.contains(&r.label))
        .count();
    let fleet_count = fs::read_to_string(&fleet_path)
        .ok()
        .and_then(|t| FleetRoster::from_toml(&t, "fleet-accounts.toml").ok())
        .map_or(bound.rows.len(), |f| f.accounts.len());
    let secrets_dir = home.join(".secrets");
    let secrets_fleet = secrets_dir.join(FLEET_ROSTER_FILE);
    if secrets_dir.is_dir()
        && !secrets_fleet.exists()
        && let Ok(fleet_text) = fs::read_to_string(&fleet_path)
    {
        let _ = super::upgrade_models::write_atomic_mode(&secrets_fleet, &fleet_text, Some(0o600));
    }
    let machine = detect_machine_id(None, env.config.as_deref())
        .or_else(|| {
            bound
                .rows
                .first()
                .map(|r| r.label.split('-').next().unwrap_or(&r.label).to_owned())
        })
        .unwrap_or_else(|| "local".to_owned());
    Ok((machine, bound.rows.len(), enrolled_count, fleet_count))
}

struct AutoRenewGuard {
    in_flight: bool,
    last_attempt: BTreeMap<String, Instant>,
}

static AUTO_RENEW_GUARD: std::sync::LazyLock<std::sync::Arc<std::sync::Mutex<AutoRenewGuard>>> =
    std::sync::LazyLock::new(|| {
        std::sync::Arc::new(std::sync::Mutex::new(AutoRenewGuard {
            in_flight: false,
            last_attempt: BTreeMap::new(),
        }))
    });

/// Own busy admission before the closure reaches the thread builder. A
/// refused spawn drops the closure too, releasing the same permit that a
/// finished or unwinding worker releases.
struct AutoRenewPermit(std::sync::Arc<std::sync::Mutex<AutoRenewGuard>>);

impl AutoRenewPermit {
    fn claim(
        state: &std::sync::Arc<std::sync::Mutex<AutoRenewGuard>>,
        label: &str,
        now: Instant,
        ignore_cooldown: bool,
    ) -> Option<Self> {
        let mut guard = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if guard.in_flight
            || (!ignore_cooldown
                && guard.last_attempt.get(label).is_some_and(|last| {
                    now.saturating_duration_since(*last) < Duration::from_secs(900)
                }))
        {
            return None;
        }
        guard.in_flight = true;
        guard.last_attempt.insert(label.to_owned(), now);
        Some(Self(std::sync::Arc::clone(state)))
    }
}

impl Drop for AutoRenewPermit {
    fn drop(&mut self) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .in_flight = false;
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

fn resolve_claude_program(home: &Path) -> PathBuf {
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
        if path.is_dir() {
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

/// When the active or default account (or any recorded session's account) on a
/// live host stands at a quota wall and no enrolled account is ready to take
/// over (`candidates` has no `Ok`), automatically launches a background
/// `claude auth login --claudeai --email <email>` renewal into a staging
/// directory for the next non-limited account (`sign_in_next`), bounded by a
/// single in-flight worker and a 15-minute per-label cooldown. Once verified
/// via `claude auth status --json`, binds the account in `logins.json` +
fn next_auto_renew_target(
    file_roster: &Roster,
    state: &accounts::State,
    logins: &accounts::Logins,
    now: i64,
) -> Option<accounts::Row> {
    let roster = accounts::bound(file_roster, logins);
    let enrolled = accounts::enrolled(&roster);
    let is_walled_or_low = |label: &str| {
        state.accounts.get(label).is_some_and(|a| {
            a.limited_until(now).is_some() || a.low(now) || a.auth == accounts::Auth::SignedOut
        })
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
        .or_else(|| roster.row_for_login(None).map(|row| row.label.as_str()));
    let any_walled = me.is_some_and(is_walled_or_low)
        || state
            .sessions
            .values()
            .any(|s| is_walled_or_low(&s.account));
    if !any_walled {
        return None;
    }
    let wall_point = accounts::Point::Wall {
        window: String::new(),
        at: None,
    };
    let has_ready_candidate = accounts::candidates(&roster, state, &enrolled, me, &wall_point, now)
        .iter()
        .any(|(_, verdict)| verdict.is_ok());
    if has_ready_candidate {
        return None;
    }
    let (next_label, true) = accounts::sign_in_next(&roster, state, &enrolled, me, now)? else {
        return None;
    };
    let next_row = roster.row(&next_label)?.clone();
    next_row.email.as_ref()?;
    Some(next_row)
}

#[expect(
    clippy::too_many_arguments,
    reason = "staging renewal commit binds store, rosters, target row, staging dir, status JSON, and clock"
)]
fn complete_staging_renewal(
    store: &accounts::Store,
    file_roster: &Roster,
    roster: &Roster,
    next_row: &accounts::Row,
    staging: &Path,
    staging_text: &str,
    status_got: &super::align::Captured,
    now_done: i64,
) -> bool {
    let matched = std::iter::once(next_row)
        .chain(roster.rows.iter().filter(|r| r.label != next_row.label))
        .find_map(|row| {
            let email = row.email.as_deref()?;
            let facts = super::cli::status_facts(status_got, email)?;
            if !facts.renews() {
                return None;
            }
            Some((row.label.clone(), facts.subscription))
        });
    let Some((matched_label, subscription)) = matched else {
        let _ = fs::remove_dir_all(staging);
        return false;
    };
    let (fresh_state, _) = store.load(now_done).unwrap_or_default();
    if fresh_state
        .accounts
        .get(&matched_label)
        .is_some_and(|a| a.limited_until(now_done).is_some())
    {
        let _ = fs::remove_dir_all(staging);
        return false;
    }
    let own_dir = file_roster
        .row(&matched_label)
        .and_then(|r| r.dir.as_deref());
    let mut note = None;
    store
        .transact_logins(now_done, &mut note, |state, logins| {
            Ok(accounts::note_renewed(
                state,
                logins,
                &matched_label,
                own_dir,
                staging_text,
                subscription.as_deref(),
                now_done,
            ))
        })
        .is_ok()
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

/// Automatically start a non-blocking staging login (`claude auth login --claudeai --email ...`)
/// for the next un-enrolled account when the active or walled account has no ready candidate
/// (`NotEnrolled` or `SignedOut`), so weekly-limit rollovers complete without manual CLI intervention.
///
/// Runs at most one renewal at a time across the process with a 15-minute cooldown per account
/// label, and once `claude auth login` succeeds in `<state>/accounts/logins/<label>+<seq>`,
/// verifies `claude auth status --json`, binds the staging directory in `logins.json` and
/// `state.json` and touches `wake` so waiting sessions rotate immediately.
///
/// REACHED ONLY THROUGH [`upkeep`], past its gate: a machine whose person
/// configured no fleet accounts source ([`configured_source`]) never gets here,
/// so no `claude auth login`, no opener and no mailbox authorizer
/// (`super::open_url::authorize_with_mailbox`, which only a renewal's
/// [`super::open_url::MAILBOX_ENV`] arms) runs there. `true` when the worker
/// was started (`fx`'s spawn took it).
fn auto_renew_next_account(
    home: &Path,
    state_dir: &Path,
    roster_path: &Path,
    now: i64,
    fx: &UpkeepEffects<'_>,
) -> bool {
    let Ok(file_roster) = Roster::load(roster_path) else {
        return false;
    };
    if file_roster.rows.is_empty() {
        return false;
    }
    let store = accounts::Store::at(state_dir);
    let Ok((state, _)) = store.load(now) else {
        return false;
    };
    let Ok((logins, _)) = store.load_logins(now) else {
        return false;
    };
    let Some(next_row) = next_auto_renew_target(&file_roster, &state, &logins, now) else {
        return false;
    };
    spawn_row_renewal(
        home,
        state_dir,
        file_roster,
        &logins,
        next_row,
        (None, false),
        now,
        fx.renew_guard,
        fx.spawn_renewal,
    )
}

/// Start a non-blocking staging login for the next un-enrolled successor when
/// all enrolled accounts stand at a quota wall, allowing the default browser
/// when no mailbox or dedicated profile is configured (e.g. when the person
/// chooses `Auto-rotate` in the Accounts menu).
pub fn auto_renew_next_account_interactive(
    home: &Path,
    state_dir: &Path,
    roster_path: &Path,
    now: i64,
) -> bool {
    let Ok(file_roster) = Roster::load(roster_path) else {
        return false;
    };
    if file_roster.rows.is_empty() {
        return false;
    }
    let store = accounts::Store::at(state_dir);
    let Ok((state, _)) = store.load(now) else {
        return false;
    };
    let Ok((logins, _)) = store.load_logins(now) else {
        return false;
    };
    let Some(next_row) = next_auto_renew_target(&file_roster, &state, &logins, now) else {
        return false;
    };
    spawn_row_renewal(
        home,
        state_dir,
        file_roster,
        &logins,
        next_row,
        (None, true),
        now,
        &AUTO_RENEW_GUARD,
        &spawn_renewal_thread,
    )
}

/// Starts a staging login for a saved explicit selection.
/// The verified login pins only while this request is still current.
pub fn auto_renew_account_by_label(
    home: &Path,
    state_dir: &Path,
    roster_path: &Path,
    request: accounts::PinRequest,
    now: i64,
) -> bool {
    let Ok(file_roster) = Roster::load(roster_path) else {
        return false;
    };
    let store = accounts::Store::at(state_dir);
    let Ok((logins, _)) = store.load_logins(now) else {
        return false;
    };
    let roster = accounts::bound(&file_roster, &logins);
    let Some(next_row) = roster.row(request.label()).cloned() else {
        return false;
    };
    spawn_row_renewal(
        home,
        state_dir,
        file_roster,
        &logins,
        next_row,
        (Some(request), true),
        now,
        &AUTO_RENEW_GUARD,
        &spawn_renewal_thread,
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "row renewal binds paths, rosters, target row, pin/browser flags, clock, admission guard, and spawn seam"
)]
fn spawn_row_renewal(
    home: &Path,
    state_dir: &Path,
    file_roster: Roster,
    logins: &accounts::Logins,
    next_row: accounts::Row,
    (pin_request, allow_default_browser): (Option<accounts::PinRequest>, bool),
    now: i64,
    renew_guard: &std::sync::Arc<std::sync::Mutex<AutoRenewGuard>>,
    spawn_renewal: &SpawnRenewal,
) -> bool {
    let store = accounts::Store::at(state_dir);
    let mailbox_dir = resolve_mailbox_dir(Some(home), &store);
    if !allow_default_browser && next_row.browser.is_none() && mailbox_dir.is_none() {
        return false;
    }
    let next_label = next_row.label.clone();
    let Some(next_email) = next_row.email.clone() else {
        return false;
    };
    let roster = accounts::bound(&file_roster, logins);
    let Some(permit) = AutoRenewPermit::claim(
        renew_guard,
        &next_label,
        Instant::now(),
        allow_default_browser,
    ) else {
        return false;
    };
    let home_owned = home.to_path_buf();
    let state_owned = state_dir.to_path_buf();
    start_auto_renew(
        permit,
        move || {
            let store = accounts::Store::at(&state_owned);
            let renewed = (|| {
                let opener = resolve_opener_link(&home_owned, &store);
                let browser = match select_auto_renew_browser(
                    next_row.browser.as_deref(),
                    mailbox_dir.as_deref(),
                    opener,
                ) {
                    Some(b) => b,
                    None if allow_default_browser => super::cli::RenewBrowser::Default,
                    None => return false,
                };
                let Ok(staging) = store.claim_staging_dir(&next_label) else {
                    return false;
                };
                let Some(staging_text) = staging.to_str().map(str::to_owned) else {
                    let _ = fs::remove_dir_all(&staging);
                    return false;
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
                    return false;
                };
                let claude_prog = resolve_claude_program(&home_owned);
                let mut cmd = super::cli::renew_command(
                    claude_prog.as_os_str(),
                    &staging_text,
                    &next_email,
                    &browser,
                );
                cmd.env("HOME", &home_owned)
                    .env(super::open_url::EMAIL_ENV, &next_email)
                    .envs(pass_env);
                if let Some(mailbox) = &mailbox_dir {
                    cmd.env(super::open_url::MAILBOX_ENV, mailbox);
                }
                let ran = super::align::capture_bounded(cmd, Duration::from_secs(300), 64 * 1024);
                if !ran.is_ok_and(|cap| cap.code == Some(0)) || !accounts::login_made(&staging) {
                    let _ = fs::remove_dir_all(&staging);
                    return false;
                }
                let Ok(status_got) = super::cli::status_in_home_within(
                    claude_prog.as_os_str(),
                    &staging_text,
                    &env_var,
                    super::cli::AUTH_CHECK_BUDGET,
                ) else {
                    let _ = fs::remove_dir_all(&staging);
                    return false;
                };
                let now_done = crate::supervise::limit::unix_now().max(now);
                complete_staging_renewal(
                    &store,
                    &file_roster,
                    &roster,
                    &next_row,
                    &staging,
                    &staging_text,
                    &status_got,
                    now_done,
                )
            })();
            let now_done = crate::supervise::limit::unix_now().max(now);
            if let Some(request) = &pin_request {
                if !renewed {
                    let _ = store.cancel_pin_request(request, now_done);
                    return;
                }
                let bound = store.bound(&file_roster, now_done).unwrap_or(file_roster);
                let enrolled = accounts::enrolled(&bound);
                let mut note = None;
                let _ = store.transact_within(&accounts::LOCK_LADDER, now_done, &mut note, |st| {
                    Ok(
                        accounts::complete_requested_pin(st, &bound, &enrolled, request, now_done)
                            .into_iter()
                            .collect(),
                    )
                });
            }
        },
        spawn_renewal,
    )
    .is_ok()
}

/// Run `aterm harness accounts serve [--bind <addr:port>] [<source>] [--machine <mN>]`.
///
/// Serves a clean HTTP dashboard and LAN configuration service:
/// * `GET /` — HTML status & control webapp for this machine and the fleet
/// * `GET /fleet-accounts.toml` — shared LAN fleet config file
/// * `GET /accounts.toml?machine=<mN>` — per-machine `accounts.toml`
/// * `GET /status.json` — JSON status of this machine's accounts
/// * `POST /sync` — re-syncs from the named or configured source ([`configured_source`])
/// * `POST /use` — pins an account (`label=<l>`) or resumes auto (`auto=1`)
///
/// Open the dashboard using the bound name or an address of the listening
/// interface. Browser actions require its own Origin; scripts that POST
/// explicitly send `X-Aterm-Accounts-Action: 1`. Sharing on a LAN remains an
/// explicit `--bind` choice, with access controlled by that network.
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

/// Read a complete bounded HTTP head, including when TCP splits a header.
/// No route may run on a prefix that omits a later Origin or Host field.
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

/// The dashboard accepts its bound name/address, never an arbitrary Host
/// supplied by a browser after DNS rebinding. Browser actions must have this
/// same origin. A script can explicitly opt in with the non-simple action
/// header; cross-origin browser fetches cannot send it without a preflight,
/// and this service does not grant CORS access.
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

/// A failed account action must not redirect as though it succeeded.
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
    let report_text = String::from_utf8_lossy(&report_out);
    let roster = env
        .accounts_roster()
        .and_then(|p| Roster::load(&p).ok())
        .unwrap_or_default();
    let enrolled = accounts::enrolled(&roster);
    let (state, _) = env.accounts_store().load(env.now).unwrap_or_default();

    let mut rows_html = String::new();
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
         <div class=\"card\"><h2>Live Harness Report (<code>aterm harness accounts</code>)</h2>\
           <pre>{}</pre>\
         </div></body></html>",
        html_escape(&report_text)
    )
}

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
        // A body in the same read is not part of the header.
        let mut padded = format!(
            "GET / HTTP/1.1\r\nHost: localhost:8741\r\nX-Padding: {}\r\n\r\nbody",
            "a".repeat(8000)
        );
        let mut input = padded.as_bytes();
        let head = read_request_head(|buf| input.read(buf)).unwrap();
        assert!(!head.ends_with("body"));
        assert!(admit_request(&head, DEFAULT_SERVE_BIND, local).is_some());
        // A complete head at the exact cap is valid; one byte over is not.
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
        // The complete admitted boundary is still a real, parseable roster.
        // One additional byte is refused instead of publishing its valid prefix.
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

        // Local discovery uses the same whole-source admission. Its opened
        // descriptor, not an earlier directory entry, must be a bounded file.
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
            // SAFETY: a NUL-terminated path in this test's exclusive directory;
            // mkfifo creates a node and retains no pointer. No writer is opened.
            assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
            assert!(read_local_source_text(&fifo).is_err());
            assert_eq!(
                read_local_source_text(&source).unwrap(),
                "ordinary@example.test"
            );
        }

        // A note's second physical line is still the same account's note.
        // Every existing unusable marker, including suspension, refuses it.
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

        // A legitimate quoted note may contain a comma, doubled quote and a
        // complete account-looking physical line; none becomes another row.
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

        // Refuse malformed quoting whole, even after an ordinary valid row.
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

        // Shared row across m106 and m38, plus foxtrot.m38.106.2 hint.
        let m38 = fleet.accounts_for_machine("m38");
        assert_eq!(m38.len(), 2);
        assert!(m38.iter().all(|a| a.class == Class::Shared));
        let m106 = fleet.accounts_for_machine("m106");
        assert_eq!(m106.len(), 2);

        // Round-trip through fleet-accounts.toml.
        let toml = fleet.to_toml();
        let reparsed = FleetRoster::from_toml(&toml, "fleet-accounts.toml").expect("valid toml");
        assert_eq!(reparsed.accounts, fleet.accounts);

        // Browser profile hints are free text. Keep every control and Unicode
        // character through the actual fleet writer/reader, rather than
        // emitting a roster that the next sync cannot parse.
        let mut fleet = fleet.clone();
        let controls: String = (0u8..=31).map(char::from).collect();
        fleet.accounts[0].browser = Some(format!("{controls}\u{7f}\"\\ profile \u{a0} \u{1f680}"));
        let reparsed = FleetRoster::from_toml(&fleet.to_toml(), "captured.toml").unwrap();
        assert_eq!(reparsed.accounts, fleet.accounts);

        let duplicates = "email,computer,claude\none@example.test,m8,1\ntwo@example.test,m8,1\n";
        let fleet = parse_accounting_csv(duplicates, "captured.csv").unwrap();
        assert_eq!(fleet.accounts[0].label, "m8");
        assert_eq!(fleet.accounts[1].label, "m8-2");

        // Growing a suffix can never repair an already oversized label.
        // Both an oversized base and a valid base whose duplicate would
        // exceed the grammar must return, rather than search forever.
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
    fn builds_machine_roster_adopting_current_default_profile_email() {
        let fleet = parse_accounting_csv(SAMPLE_CSV, "sample.csv").expect("valid csv");
        let tmp = std::env::temp_dir().join(format!("aterm-sync-test-{}", std::process::id()));
        let store = accounts::Store::at(&tmp);

        // When default profile email is bravo.m8@example.test (`m8-2`), `m8-2`
        // gets `dir = None` (adopted default login) and `m8` gets a private login dir.
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

        // A duplicate in a captured fleet must not become an empty successful
        // roster response or replace a good roster with an empty file.
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
            (&[], None),
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
        assert_eq!(saved.roster.row_for_login(None).unwrap().label, "m8-2");
        assert_eq!(
            saved.roster.row("m8").unwrap().dir.as_deref(),
            Some(login.as_str())
        );
        assert_eq!(
            saved.roster.row("m8").unwrap().browser.as_deref(),
            Some("kept profile")
        );
        assert_eq!(Roster::load(&target).unwrap(), saved.roster);
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

        let kept = persist_accounts_sync(
            &store,
            &roster_path,
            (&fleet, OptIn::Named),
            ("m8", None),
            &empty_walls,
            now,
            (&[], None),
        )
        .expect("preserve the current default without a profile hint");
        assert_eq!(kept.roster.row_for_login(None).unwrap().label, "m8-2");
        let before_roster = fs::read(&target).unwrap();
        let before_fleet = fs::read(&saved.fleet_path).unwrap();
        // An existing malformed roster is not an empty starting point.
        // Refuse it without changing either its bytes or the published fleet.
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
                (&[], None)
            )
            .is_err()
        );
        assert_eq!(fs::read(&target).unwrap(), malformed);
        assert_eq!(fs::read(&saved.fleet_path).unwrap(), before_fleet);
        fs::write(&target, &before_roster).unwrap();
        let mut changed = fleet.clone();
        changed.accounts[0].browser = Some("new profile".into());
        // Both identity files admit before the edit callback can publish
        // any roster. A newer schema is not an empty state to overwrite.
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
                    (&[], None),
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
                (&[], None),
            )
            .is_err()
        );
        assert_eq!(fs::read(&target).unwrap(), before_roster);
        assert_eq!(fs::read(&saved.fleet_path).unwrap(), before_fleet);

        // A later fleet write fails deterministically at an owned directory.
        // The earlier roster replacement remains: refusal is not rollback.
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
                (&[], None),
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
        let reset = persist_accounts_sync(
            &store,
            &roster_path,
            (&fleet, OptIn::Named),
            ("m8", None),
            &empty_walls,
            now,
            (&[], None),
        )
        .unwrap();

        // Exercise the actual CLI function's failure return and absence of
        // success output using a local CSV and no vendor/profile location.
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
            codex_home: None,
            now,
            tz_s: 0,
            sid: String::new(),
            sock: None,
            config: Some(config.join("aterm.toml")),
            claude_tmp: None,
            claude_running: None,
        };
        let mut out = Vec::new();
        let mut err = Vec::new();
        assert_eq!(
            run_accounts_sync(&env, source.to_str(), Some("m8"), false, &mut out, &mut err),
            ExitCode::FAILURE
        );
        assert!(out.is_empty());
        assert!(!err.is_empty());
        assert!(store.wake_path().is_dir());
        assert_eq!(Roster::load(&target).unwrap(), reset.roster);
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

    /// The production permit and spawn seam, without a thread, process,
    /// account home or global guard reset. A refused builder must release
    /// busy admission; a queued worker owns it until it actually finishes.
    #[test]
    fn a_refused_renewal_worker_releases_admission_without_running_it() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::{Arc, Mutex};

        let state = Arc::new(Mutex::new(AutoRenewGuard {
            in_flight: false,
            last_attempt: BTreeMap::new(),
        }));
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

        // Record a live supervised session `sid-m15-live` on `m15-2` hitting its weekly wall.
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

        // Also verify `next_auto_renew_target` finds `m15-3` when `state.active` is None
        // and the active account is only recorded in `state.sessions`.
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

        // Claim the staging directory for `m15-3`, deliver a decoy + a Quoted-Printable magic-link
        // email to the mailbox, and run the capability-injected OAuth + magic-link flow.
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
                    Duration::from_secs(1),
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

        // Write the newly authorized staging login profile and commit via `complete_staging_renewal`.
        fs::write(
            staging.join(".claude.json"),
            "{\"oauthAccount\":{\"emailAddress\":\"m15-3@example.test\",\"subscriptionType\":\"max\"}}\n",
        )
        .unwrap();
        let status_got = super::super::align::Captured {
            stdout: b"{\"loggedIn\":true,\"authMethod\":\"claude.ai\",\"email\":\"m15-3@example.test\",\"subscriptionType\":\"max\"}\n".to_vec(),
            code: Some(0),
        };
        let roster_before = accounts::bound(&file_roster, &logins_before);
        assert!(
            complete_staging_renewal(
                &store,
                &file_roster,
                &roster_before,
                &next_row,
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

        // Prove the waiting session on `m15-2` immediately receives `Advice::Move` to `m15-3`
        // and rotates onto `CLAUDE_LOGIN_DIR=<state>/accounts/logins/m15-3+1`.
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

    /// A test's world for the fleet accounts: `home`, the harness `state`
    /// root and the config folder, every one empty and none made but `home`.
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

    /// What the upkeep reached past its own files: every source it fetched,
    /// every renewal worker it started (dropped unrun: no `claude`, no opener,
    /// no mailbox sign-in), behind gates of its own.
    struct Witness {
        fetched: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
        spawned: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        fetch: Box<Fetch>,
        spawn: Box<SpawnRenewal>,
        cooldown: std::sync::Mutex<Option<Instant>>,
        guard: std::sync::Arc<std::sync::Mutex<AutoRenewGuard>>,
    }

    impl Witness {
        /// Every fetch answers `text`.
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
                guard: std::sync::Arc::new(std::sync::Mutex::new(AutoRenewGuard {
                    in_flight: false,
                    last_attempt: BTreeMap::new(),
                })),
            }
        }

        fn fx(&self) -> UpkeepEffects<'_> {
            UpkeepEffects {
                fetch: &*self.fetch,
                spawn_renewal: &*self.spawn,
                sync_cooldown: &self.cooldown,
                renew_guard: &self.guard,
            }
        }

        fn fetched(&self) -> Vec<String> {
            self.fetched.lock().unwrap().clone()
        }

        fn spawned(&self) -> usize {
            self.spawned.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    /// An obviously fake source on a loopback port nothing listens on: the
    /// witness answers it, nothing dials it.
    const LOOPBACK_SOURCE: &str = "http://127.0.0.1:9/fleet.csv";
    const ONE_ACCOUNT_CSV: &str = "email,computer,claude\nm1@example.test,m1,1\n";
    const UPKEEP_NOW: i64 = 1_792_660_000;
    /// Nothing configured, and no fleet file beside the roster.
    const UNCONFIGURED: Upkept = Upkept::Unconfigured {
        fleet_file_ignored: false,
    };

    /// NOTHING CONFIGURED, NOTHING DONE: the window's upkeep in a fresh home —
    /// every new install's and every test world's — and under an aterm.toml
    /// that names the machine but no source, a blank source, a source outside
    /// `[fleet]`, or a file that is not TOML, fetches nothing, starts no
    /// renewal and writes nothing (FAILS with a default source of any kind
    /// restored: the 0.112.0 upkeep fetched a hard-coded spreadsheet link
    /// here, then an rclone remote). NEGATIVE CONTROL: the same upkeep with
    /// `[fleet] accounts_source` naming a loopback source fetches exactly it,
    /// writes the roster it answered and a fleet file that owes itself to the
    /// key.
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

        // NEGATIVE CONTROL: the arm on.
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
        // The fleet file it left owes itself to the key, and opts nothing in.
        assert_eq!(fleet_file_opt_in(&w.fleet_file()), Some(OptIn::Key));
    }

    /// `aterm harness accounts sync` handed no source reads nothing where none
    /// is configured: it says so ([`NO_SOURCE`]) and fails (FAILS with the old
    /// fallback to a default link, remote or `~/Downloads` file). NEGATIVE
    /// CONTROL: with `[fleet] accounts_source`, the same command reads that.
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
                fleet_snapshot: None,
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
        // The fleet file a sync from the key left says so, and opts nothing
        // in of itself: the key is the opt-in, and taking it out opts out.
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

    /// A SOURCE IS READ AS WRITTEN AND NOTHING MORE, where the fetch is
    /// composed ([`fetch_source_text_with`]): an rclone remote (`rclone:`) is
    /// refused by name, naming the sources that remain, and runs nothing
    /// (init design §6.1 row 29); curl only for an `http(s)` link (a Sheets
    /// `/edit` link as its export); no program for a file (`~/` the home's; a
    /// bare `remote:path` is a file name); and a fetcher's failure is the
    /// answer — nothing is tried after it (FAILS with 0.112.0's fallback
    /// restored: a Sheets link whose curl failed went on to an rclone remote).
    /// NEGATIVE CONTROL: a fetcher that answers is the answer.
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

        // NEGATIVE CONTROL: a fetcher that answers.
        let (got, ran) = took(LOOPBACK_SOURCE, &run_as(0, ONE_ACCOUNT_CSV));
        assert_eq!(got.as_deref(), Ok(ONE_ACCOUNT_CSV));
        assert_eq!(ran.len(), 1);
    }

    /// One program a fetch ran: its path and its arguments.
    type Ran = (String, Vec<String>);

    /// A FLEET FILE NO ONE HERE NAMED A SOURCE FOR OPTS NOTHING IN: the one
    /// 0.112.0 to 0.115.0 wrote from their built-in sheet, unasked (no
    /// [`OPT_IN_KEY`]), and the one a sync from `[fleet] accounts_source`
    /// wrote once the key is gone. With either beside an aterm.toml that
    /// names no source, the upkeep fetches nothing, writes no roster and
    /// starts nothing, and says a fleet file is ignored (FAILS with any
    /// existing fleet file taken for the person's opt-in, as the first fix
    /// took it). NEGATIVE CONTROL: the person syncs by hand from a source
    /// they name; the file then says so, and the upkeep is configured by it.
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
                    fleet_file_ignored: true
                },
                "{text}"
            );
        }

        // A valid marker in an oversized file does not opt this machine in.
        // The metadata read uses the same whole-file bound as local discovery.
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
                fleet_file_ignored: true
            }
        );
        assert_eq!(wit.fetched(), Vec::<String>::new(), "nothing fetched");
        assert_eq!(wit.spawned(), 0, "no renewal started");
        assert!(!w.roster().exists(), "no roster written");

        // NEGATIVE CONTROL: a sync by hand from a source the person names.
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let synced = run_accounts_sync_with(
            &w.env(UPKEEP_NOW),
            (Some("~/fleet.csv"), None, false),
            (&mut out, &mut err),
            &SyncHow {
                fetch: &*wit.fetch,
                ladder: &[],
                fleet_snapshot: None,
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

    /// A machine whose `accounts.toml` was synced and later unconfigured, with
    /// one login at its weekly wall, a successor free and a mailbox to sign it
    /// in by: the window's upkeep starts no renewal (no `claude auth login`,
    /// no mailbox authorizer) while no source is configured (FAILS with the
    /// renewal made outside the gate, as 0.112.0 made it). NEGATIVE CONTROL:
    /// with this machine's fleet file back, the same upkeep starts one.
    #[test]
    fn a_walled_login_is_renewed_only_where_a_source_is_configured() {
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
                fleet_snapshot: None,
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

        fs::create_dir_all(&mailbox_dir).unwrap();
        let fleet_aside = w.config.join("fleet-accounts.aside");
        fs::rename(w.fleet_file(), &fleet_aside).unwrap();

        let upkept = |step| upkeep(&w.home, &w.state, (&w.roster(), now, step), &wit.fx());
        assert_eq!(upkept(UpkeepStep::Both), UNCONFIGURED);
        assert_eq!(wit.spawned(), 0, "no renewal without a configured source");

        // NEGATIVE CONTROL: the fleet file back.
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
        assert_eq!(wit.spawned(), 0);
        assert_eq!(
            upkept(UpkeepStep::Renew),
            Upkept::Configured {
                kind: "fleet-file",
                roster: true,
                renewal: true
            }
        );
        assert_eq!(wit.spawned(), 1, "the successor's sign-in started");
    }

    /// A HELD ACCOUNTS LOCK FAILS THE SYNC, CLEARLY AND BOUNDED: with the
    /// store's lock held for good, a sync gives up after its ladder, says the
    /// lock is held and that nothing was synced, and writes no roster (FAILS,
    /// by this test's one-minute hang detector, with the unbounded wait it
    /// had). NEGATIVE CONTROL: the lock let go, the same sync completes.
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
                        fleet_snapshot: None,
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

        // NEGATIVE CONTROL: let go.
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
            "[fleet]\naccounts_source = \"{}\"\nmachine = \"m1\"\n",
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

        // User signs into a third account locally; re-running init merges it into
        // fleet-accounts.toml and accounts.toml without manual TOML editing.
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
        // An address with no machine hint falls back to m27, whose label is
        // already owned by a different email in the named fleet.
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

        // The first local discovery must also wait for admission, rather
        // than leave a new fleet file behind when the state is refused.
        let fresh = World::new();
        fresh.write_config("[fleet]\nmachine = \"m27\"\n");
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
        assert_eq!(fs::read(store.state_file()).unwrap(), b"{\"schema\":999}");
        fs::remove_file(store.state_file()).unwrap();
        let (_, count, _, fleet_count) =
            sync_accounts_for_init(&fresh.home, &fresh.state, &fresh.roster(), UPKEEP_NOW)
                .expect("initial discovery publishes after admission");
        assert_eq!((count, fleet_count), (2, 2));

        // A cooperating sync can replace the fleet after discovery but before
        // init takes the lock. Exercise that gap through the actual fetch and
        // publication funnel, without racing threads or sleeps.
        let captured = fs::read(w.fleet_file()).unwrap();
        let mut concurrent =
            FleetRoster::from_toml(std::str::from_utf8(&captured).unwrap(), "captured").unwrap();
        let mut another = concurrent.accounts[0].clone();
        another.label = "m27-other".to_owned();
        another.email = "concurrent@example.test".to_owned();
        concurrent.accounts.push(another);
        let replacement = concurrent.to_toml_for(Some(OptIn::Named));
        let sync_across_change = |w: &World, expected: Option<Vec<u8>>| {
            let state_dir = w.state.clone();
            let fleet_path = w.fleet_file();
            let answer = String::from_utf8(captured.clone()).unwrap();
            let replacement = replacement.clone();
            let fetch = move |_: &Env, _: &str| {
                let store = accounts::Store::at(&state_dir);
                let _held = store.acquire().unwrap();
                fs::write(&fleet_path, &replacement).unwrap();
                Ok(answer.clone())
            };
            let expected = FleetSnapshot(expected);
            let (mut out, mut err) = (Vec::new(), Vec::new());
            let source = w.fleet_file().to_string_lossy().into_owned();
            let code = run_accounts_sync_with(
                &w.env(UPKEEP_NOW + 40),
                (Some(&source), None, true),
                (&mut out, &mut err),
                &SyncHow {
                    fetch: &fetch,
                    ladder: &[],
                    fleet_snapshot: Some(&expected),
                },
            );
            assert_eq!(code, ExitCode::FAILURE);
            assert!(String::from_utf8_lossy(&err).contains("changed after init discovery"));
        };
        let roster_bytes = fs::read(w.roster()).unwrap();
        sync_across_change(&w, Some(captured.clone()));
        assert_eq!(fs::read(w.roster()).unwrap(), roster_bytes);
        assert_eq!(fs::read(w.fleet_file()).unwrap(), replacement.as_bytes());

        let first = World::new();
        first.write_config("[fleet]\nmachine = \"m27\"\n");
        assert!(!first.fleet_file().exists());
        sync_across_change(&first, None);
        assert!(!first.roster().exists());
        assert_eq!(
            fs::read(first.fleet_file()).unwrap(),
            replacement.as_bytes()
        );
    }
}
