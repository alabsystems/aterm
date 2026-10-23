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

use super::accounts::{self, Class, Roster, Row};
use super::cli::{ACCOUNTS_OWN_LABEL, Env};
use super::upgrade;

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

    /// [`Self::to_toml`], with this machine's [`OPT_IN_KEY`] where `opt_in` names one.
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
    /// # Errors
    /// Returns a human-readable error if the TOML is invalid, `account` is not an array of tables,
    /// or any row is missing required fields or carries a refused label or email.
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

/// Build the machine's [`Roster`] from `fleet` for `machine`.
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
        .filter(|fleet| fleet_file_opt_in(fleet) == Some(OptIn::Named))
    {
        return Some(FleetSource::File(fleet));
    }
    config.and_then(source_key).map(FleetSource::Key)
}

/// Whether a fleet roster file stands beside `roster` without opting in ([`configured_source`]).
#[must_use]
pub fn fleet_file_ignored(roster: &Path) -> bool {
    fleet_file_beside(roster)
        .is_some_and(|fleet| fleet.is_file() && fleet_file_opt_in(&fleet) != Some(OptIn::Named))
}

fn fleet_file_beside(roster: &Path) -> Option<PathBuf> {
    roster.parent().map(|dir| dir.join(FLEET_ROSTER_FILE))
}

fn fleet_file_opt_in(fleet: &Path) -> Option<OptIn> {
    let text = read_local_source_text(fleet).ok()?;
    let table: aterm_toml::Table = aterm_toml::from_str(&text).ok()?;
    match table.get(OPT_IN_KEY)?.as_str()? {
        "named" => Some(OptIn::Named),
        "key" => Some(OptIn::Key),
        _ => None,
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

type Fetch = dyn Fn(&Env, &str) -> Result<String, String> + Sync;

/// Load a [`FleetRoster`] from `source`, or from [`configured_source`] when `source` is `None`.
/// # Errors
/// No source, or the source could not be read or parsed.
pub fn load_fleet_source(env: &Env, source: Option<&str>) -> Result<FleetRoster, String> {
    load_fleet_source_with(env, source, &fetch_source_text).map(|(fleet, _)| fleet)
}

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

#[cfg(test)]
fn discover_local_fleet(env: &Env) -> Option<FleetRoster> {
    discover_local_fleet_with_min(env, 2)
}

fn discover_local_fleet_with_min(env: &Env, min_accounts: usize) -> Option<FleetRoster> {
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
    if let Some(roster_path) = env.accounts_roster()
        && let Ok(roster) = Roster::load(&roster_path)
    {
        for row in &roster.rows {
            if let Some(em) = row.email.as_deref() {
                record_email(em);
            }
        }
    }
    if let Some(mb) = super::headless_oauth::discover_mailbox_config_files(
        env.home.as_deref(),
        Some(&accounts::Store::at(&env.state).dir),
        env.config.as_deref(),
    ) {
        if let Some(u) = mb.user.as_deref().filter(|u| u.contains('@')) {
            record_email(u);
        }
        for key in mb
            .passwords
            .keys()
            .chain(mb.mailbox_passwords.keys())
            .chain(mb.totp_secrets.keys())
        {
            record_email(key);
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
                record_email(em);
            }
            if let Ok(text) =
                read_local_source_text(&dir_path.join(accounts::RENEWAL_CHECKPOINT_FILE))
                && let Ok(v) = aterm_json::from_str::<Value>(&text)
                && let Some(em) = v.get("email").and_then(Value::as_str)
            {
                record_email(em);
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
    roster: Roster,
    fleet_path: PathBuf,
    state: accounts::State,
    note: Option<String>,
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
    let default_matched = default_email.is_some_and(|de| {
        roster
            .row_for_login(None)
            .and_then(|r| r.email.as_deref())
            .is_some_and(|re| accounts::email_matches(re, de))
    });
    let mut note = None;
    let mut publishing = false;
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
            for expected in fleet_snapshots.iter().filter(|snapshot| snapshot.publish) {
                super::upgrade_models::write_atomic_mode(&expected.path, &fleet_toml, Some(0o600))
                    .map_err(|why| format!("could not write {}: {why}", expected.path.display()))?;
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
    fetch: &'a Fetch,
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
    let email_machine = || {
        default_email.as_deref().and_then(|de| {
            fleet
                .accounts
                .iter()
                .find(|a| accounts::email_matches(&a.email, de))
                .and_then(|a| a.machines.first().cloned())
        })
    };
    let explicit_machine = machine_override
        .and_then(normalize_machine_id)
        .or_else(|| config_machine_id(env.config.as_deref()));
    let machine = explicit_machine.or_else(|| {
        detect_machine_id(None, None)
            .filter(|m| !fleet.accounts_for_machine(m).is_empty() || email_machine().is_none())
            .or_else(email_machine)
    });
    let Some(machine) = machine else {
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
    let (state, bound) =
        if recover_interrupted_staging_renewals(&store, &roster, &AUTO_RENEW_GUARD, env.now) {
            let state = store.load(env.now).map(|(s, _)| s).unwrap_or(state);
            let bound = store
                .bound(&roster, env.now)
                .unwrap_or_else(|_| roster.clone());
            (state, bound)
        } else {
            let bound = store
                .bound(&roster, env.now)
                .unwrap_or_else(|_| roster.clone());
            (state, bound)
        };
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

static AUTO_SYNC_COOLDOWN: std::sync::Mutex<Option<Instant>> = std::sync::Mutex::new(None);

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
    /// No automatic source is configured ([`configured_source`]); a saved explicit selection
    /// may still progress.
    Unconfigured {
        /// [`fleet_file_ignored`].
        fleet_file_ignored: bool,
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
    fetch: &'a Fetch,
    spawn_renewal: &'a SpawnRenewal,
    sync_cooldown: &'a std::sync::Mutex<Option<Instant>>,
    renew_guard: &'a std::sync::Arc<std::sync::Mutex<AutoRenewGuard>>,
}

type SpawnRenewal = dyn Fn(Box<dyn FnOnce() + Send>) -> io::Result<()> + Sync;

fn spawn_renewal_thread(work: Box<dyn FnOnce() + Send>) -> io::Result<()> {
    std::thread::Builder::new()
        .name("aterm-auto-renew".to_owned())
        .spawn(work)
        .map(|_| ())
}

/// THE WINDOW'S FLEET ACCOUNTS' UPKEEP, the one way a window reaches this module.
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

fn upkeep(
    home: &Path,
    state_dir: &Path,
    (roster_path, now, step): (&Path, i64, UpkeepStep),
    fx: &UpkeepEffects<'_>,
) -> Upkept {
    let config = config_beside(roster_path);
    if step.syncs() {
        let secrets = home.join(".secrets");
        let unconfigured_with_secrets = configured_source(Some(&config), Some(roster_path))
            .is_none()
            && ([
                FLEET_ROSTER_FILE,
                "ai-accounts.csv",
                super::headless_oauth::SECRETS_MAILBOX_TOML_FILE,
                super::headless_oauth::MAILBOX_TOML_FILE,
            ]
            .into_iter()
            .any(|name| secrets.join(name).is_file())
                || (secrets.is_dir()
                    && super::headless_oauth::discover_mailbox_config_files(
                        Some(home),
                        None,
                        Some(&config),
                    )
                    .is_some()));
        if unconfigured_with_secrets || needs_init_accounts_sync(home, state_dir, roster_path, now)
        {
            let _ = sync_accounts_for_init(home, state_dir, roster_path, now);
        }
    }
    // A saved explicit choice is independent of automatic fleet-source admission.
    let pending = step
        .renews()
        .then(|| resume_pending_pin(home, state_dir, roster_path, now, fx))
        .flatten();
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
    let renewal = pending.map_or_else(
        || step.renews() && auto_renew_next_account(home, state_dir, roster_path, now, fx),
        RenewalDispatch::progressed,
    );
    Upkept::Configured {
        kind: source.kind(),
        roster,
        renewal,
    }
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

/// Whether `~/.secrets/` holds updated fleet or mailbox accounts not yet converged into `roster_path`.
#[must_use]
pub fn needs_init_accounts_sync(
    home: &Path,
    state_dir: &Path,
    roster_path: &Path,
    now: i64,
) -> bool {
    let secrets = home.join(".secrets");
    let Some(fb) = fleet_file_beside(roster_path) else {
        return false;
    };
    if fleet_file_opt_in(&fb) != Some(OptIn::Named) {
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
    let config_path = config_beside(roster_path);
    if ![
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
        codex_home: None,
        now,
        tz_s: 0,
        sid: String::new(),
        sock: None,
        config: Some(config_path),
        claude_tmp: None,
        claude_running: None,
    };
    discover_local_fleet_with_min(&env, 1).is_some_and(|local| {
        local.accounts.iter().any(|acct| {
            !existing_fleet
                .accounts
                .iter()
                .any(|a| accounts::email_matches(&a.email, &acct.email))
        })
    })
}

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
        if let Some(local) = discover_local_fleet_with_min(env, 1) {
            added = merge_discovered_accounts(&mut fleet, local)?;
            if added {
                answer = fleet.to_toml_for(Some(OptIn::Named));
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

/// Synchronize the machine and fleet account rosters during `aterm init` or `aterm secrets`.
pub fn sync_accounts_for_init(
    home: &Path,
    state_dir: &Path,
    roster_path: &Path,
    now: i64,
) -> Result<(String, usize, usize, usize), String> {
    sync_accounts_for_init_with(home, state_dir, roster_path, now, || {})
}

fn sync_accounts_for_init_with(
    home: &Path,
    state_dir: &Path,
    roster_path: &Path,
    now: i64,
    before_sync: impl FnOnce(),
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
            captured_fleet = Some((source.clone(), local.to_toml_for(Some(OptIn::Named))));
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
    let store = accounts::Store::at(state_dir);
    let raw = Roster::load(roster_path)?;
    let bound = store.bound(&raw, now).unwrap_or(raw);
    let enrolled = accounts::enrolled(&bound);
    let state = store.load(now).map(|(s, _)| s).unwrap_or_default();
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
    let _ = store.touch_wake();
    Ok((machine, bound.rows.len(), enrolled_count, fleet_count))
}

#[derive(Default)]
struct AutoRenewGuard {
    in_flight: bool,
    last_attempt: BTreeMap<String, Instant>,
    mailbox_mtime: Option<std::time::SystemTime>,
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
                && guard.last_attempt.get(label).is_some_and(|last| {
                    now.saturating_duration_since(*last) < Duration::from_secs(900)
                }))
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
            let store = accounts::Store::at(state_dir);
            let now = crate::supervise::limit::unix_now();
            if store
                .load(now)
                .is_ok_and(|(state, _)| state.pending_pin.is_some())
            {
                let _ = store.touch_wake();
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
        paths.push(secrets_dir);
        paths.push(h.join(".config/aterm/secrets/state.toml"));
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

pub(super) fn auto_renew_candidates(
    file_roster: &Roster,
    state: &accounts::State,
    logins: &accounts::Logins,
    now: i64,
) -> Vec<accounts::Row> {
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
        .or_else(|| roster.row_for_login(None).map(|row| row.label.as_str()))
        .or_else(|| roster.rows.first().map(|row| row.label.as_str()));
    let any_walled = me.is_some_and(is_walled_or_low)
        || state
            .sessions
            .values()
            .any(|s| is_walled_or_low(&s.account));
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
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
                .filter(|row| me != Some(row.label.as_str()) && row.email.is_some())
                .filter(|row| {
                    let is_enr = row.dir.is_none() || enrolled.contains(&row.label);
                    matches!(
                        accounts::condition(row, state, is_enr, now),
                        accounts::Condition::NotEnrolled | accounts::Condition::SignedOut
                    ) && !state
                        .accounts
                        .get(&row.label)
                        .is_some_and(|a| a.limited_until(now).is_some())
                })
            {
                if seen.insert(next_row.label.clone()) {
                    out.push(next_row.clone());
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
                out.push(me_row);
            }
        }
    }
    for label in accounts::renew_due(&roster, logins, state, &enrolled, now) {
        if state
            .accounts
            .get(&label)
            .is_some_and(|a| a.limited_until(now).is_some())
        {
            continue;
        }
        if let Some(row) = roster.row(&label)
            && row.email.is_some()
            && seen.insert(row.label.clone())
        {
            out.push(row.clone());
        }
    }
    out.sort_by_key(|row| logins.get(&row.label).and_then(|b| b.wrong_at));
    out
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
    file_roster: &Roster,
    requested_label: &str,
    staging: &Path,
    staging_text: &str,
    status_got: &super::align::Captured,
    now_done: i64,
) -> super::cli::Renewal {
    use super::cli::Renewal;

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
    let result = (|| {
        let held = store.acquire().map_err(|why| why.to_string())?;
        let (_, read) = store.load(now_done)?;
        if let Some(why) = read {
            return Err(why);
        }
        store.transact_logins_held(&held, now_done, &mut note, |state, logins| {
            let roster = accounts::bound(file_roster, logins);
            let enrolled = accounts::enrolled(&roster);
            let Some(next_row) = roster.row(requested_label) else {
                return Ok(Vec::new());
            };
            let wrong_account = next_row
                .email
                .as_deref()
                .and_then(|email| super::cli::status_facts(status_got, email))
                .is_some_and(|facts| facts.signed_in && facts.claude_ai && !facts.email_matches);
            let matched = std::iter::once(next_row)
                .chain(roster.rows.iter().filter(|r| r.label != requested_label))
                .find_map(|row| {
                    let email = row.email.as_deref()?;
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
            accounts::write_renewal_checkpoint(
                &matched_label,
                &matched_email,
                staging_text,
                subscription.as_deref(),
                now_done,
            )
            .map_err(|why| why.to_string())?;
            let own_dir = file_roster
                .row(&matched_label)
                .and_then(|r| r.dir.as_deref());
            outcome = Renewal::Renewed;
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
    })();
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
    complete_staging_renewal_result(
        store,
        file_roster,
        requested_label,
        staging,
        staging_text,
        status_got,
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

fn recover_interrupted_staging_renewals(
    store: &accounts::Store,
    file_roster: &Roster,
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
            let actual_dir = fs::canonicalize(&dir).ok()?;
            // A vendor profile is only a local hint. Without the result of the
            // check immediately after fresh sign-in, preserve this work unadopted.
            let proof = accounts::read_renewal_checkpoint(&dir, &actual_dir, now)?;
            Some(Candidate {
                prefix: prefix.to_owned(),
                sequence,
                dir,
                actual_dir,
                proof,
            })
        })
        .collect();
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
                let enrolled = accounts::enrolled(&bound);
                let Some(row) = bound
                    .row(&proof.label)
                    .filter(|r| r.email.as_deref() == Some(proof.email.as_str()))
                else {
                    return Ok(Vec::new());
                };
                let account = state.accounts.get(&row.label);
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
                let needs_recovery = !enrolled.contains(&row.label)
                    || is_signed_out
                    || !logins.labels.contains_key(&row.label);
                if is_limited || newer_auth || newer_binding || !needs_recovery {
                    return Ok(Vec::new());
                }
                let own_dir = file_roster.row(&row.label).and_then(|r| r.dir.as_deref());
                Ok(accounts::note_renewed(
                    state,
                    logins,
                    &row.label,
                    own_dir,
                    &proof.dir,
                    proof.subscription.as_deref(),
                    proof.checked_at,
                ))
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
    let Ok(file_roster) = Roster::load(roster_path) else {
        return false;
    };
    if file_roster.rows.is_empty() {
        return false;
    }
    let store = accounts::Store::at(state_dir);
    let recovered = recover_interrupted_staging_renewals(&store, &file_roster, fx.renew_guard, now);
    let Ok((state, _)) = store.load(now) else {
        return recovered;
    };
    let Ok((logins, _)) = store.load_logins(now) else {
        return recovered;
    };
    for next_row in auto_renew_candidates(&file_roster, &state, &logins, now) {
        if spawn_row_renewal(
            home,
            state_dir,
            file_roster.clone(),
            &logins,
            next_row,
            (None, false),
            now,
            fx.renew_guard,
            fx.spawn_renewal,
        )
        .progressed()
        {
            return true;
        }
    }
    recovered
}

pub(super) fn recover_staging_renewals(
    store: &accounts::Store,
    file_roster: &Roster,
    now: i64,
) -> bool {
    recover_interrupted_staging_renewals(store, file_roster, &AUTO_RENEW_GUARD, now)
}

/// Start an interactive non-blocking staging login for the next un-enrolled successor at a wall.
pub fn auto_renew_next_account_interactive(
    home: &Path,
    state_dir: &Path,
    roster_path: &Path,
    now: i64,
) -> bool {
    let fx = UpkeepEffects {
        fetch: &fetch_source_text,
        spawn_renewal: &spawn_renewal_thread,
        sync_cooldown: &AUTO_SYNC_COOLDOWN,
        renew_guard: &AUTO_RENEW_GUARD,
    };
    if let Some(pending) = resume_pending_pin(home, state_dir, roster_path, now, &fx) {
        return pending.progressed();
    }
    let Ok(file_roster) = Roster::load(roster_path) else {
        return false;
    };
    if file_roster.rows.is_empty() {
        return false;
    }
    let store = accounts::Store::at(state_dir);
    let recovered =
        recover_interrupted_staging_renewals(&store, &file_roster, &AUTO_RENEW_GUARD, now);
    let Ok((state, _)) = store.load(now) else {
        return recovered;
    };
    let Ok((logins, _)) = store.load_logins(now) else {
        return recovered;
    };
    for next_row in auto_renew_candidates(&file_roster, &state, &logins, now) {
        if spawn_row_renewal(
            home,
            state_dir,
            file_roster.clone(),
            &logins,
            next_row,
            (None, true),
            now,
            &AUTO_RENEW_GUARD,
            &spawn_renewal_thread,
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
    let store = accounts::Store::at(state_dir);
    let request = match store.load(now) {
        Ok((state, _)) => state.pending_pin?,
        Err(_) => return Some(RenewalDispatch::Failed),
    };
    let result = renew_requested_pin(
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
    renew_requested_pin(
        home,
        state_dir,
        roster_path,
        request,
        now,
        &AUTO_RENEW_GUARD,
        &spawn_renewal_thread,
    )
}

fn renew_requested_pin(
    home: &Path,
    state_dir: &Path,
    roster_path: &Path,
    request: accounts::PinRequest,
    now: i64,
    renew_guard: &std::sync::Arc<std::sync::Mutex<AutoRenewGuard>>,
    spawn_renewal: &SpawnRenewal,
) -> RenewalDispatch {
    let Ok(file_roster) = Roster::load(roster_path) else {
        return RenewalDispatch::Failed;
    };
    let store = accounts::Store::at(state_dir);
    match store.load(now) {
        Ok((state, _)) if state.pending_pin.as_ref() == Some(&request) => {}
        Ok(_) => return RenewalDispatch::Superseded,
        Err(_) => return RenewalDispatch::Failed,
    }
    recover_interrupted_staging_renewals(&store, &file_roster, renew_guard, now);
    let mut disposition = RenewalDispatch::Superseded;
    let mut target = None;
    if store
        .transact_logins_within(&accounts::LOCK_LADDER, now, &mut None, |st, logins| {
            if st.pending_pin.as_ref() != Some(&request) {
                return Ok(Vec::new());
            }
            let roster = accounts::bound(&file_roster, logins);
            let enrolled = accounts::enrolled(&roster);
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
        home,
        state_dir,
        file_roster,
        &logins,
        next_row,
        (Some(request), true),
        now,
        renew_guard,
        spawn_renewal,
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "row renewal binds paths, roster, logins, target row, pin/browser flags, clock, admission guard, and spawn seam"
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
) -> RenewalDispatch {
    let store = accounts::Store::at(state_dir);
    let mailbox_dir = resolve_mailbox_dir(Some(home), &store);
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
    if !allow_default_browser && next_row.browser.is_none() && !has_mailbox && !has_headless_profile
    {
        return RenewalDispatch::Failed;
    }
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
    start_auto_renew(
        permit,
        move || {
            use super::cli::Renewal;

            let store = accounts::Store::at(&state_owned);
            let run_attempt = |browser: &super::cli::RenewBrowser,
                               force_headless: bool|
             -> Renewal {
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
                    &file_roster,
                    &next_row.label,
                    &staging,
                    &staging_text,
                    &status_got,
                    now_done,
                )
            };
            let renewed = (|| {
                let opener = resolve_opener_link(&home_owned, &store);
                let non_headless_fallback = select_auto_renew_browser(
                    next_row.browser.as_deref(),
                    mailbox_dir.as_deref(),
                    opener.clone(),
                )
                .or_else(|| allow_default_browser.then_some(super::cli::RenewBrowser::Default));
                let browser = if (has_mailbox || has_headless_profile) && opener.is_some() {
                    super::cli::RenewBrowser::Remote { opener }
                } else {
                    let Some(b) = non_headless_fallback.clone() else {
                        return false;
                    };
                    b
                };
                let result = run_attempt(&browser, !has_mailbox && has_headless_profile);
                if result == Renewal::Renewed {
                    return true;
                }
                if result.can_retry_authentication()
                    && !has_mailbox
                    && has_headless_profile
                    && let Some(fallback) = non_headless_fallback
                {
                    return run_attempt(&fallback, false) == Renewal::Renewed;
                }
                false
            })();
            let now_done = crate::supervise::limit::unix_now().max(now);
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
    let report_text = String::from_utf8_lossy(&report_out);
    let raw_roster = env
        .accounts_roster()
        .and_then(|p| Roster::load(&p).ok())
        .unwrap_or_default();
    let store = env.accounts_store();
    let roster = store.bound(&raw_roster, env.now).unwrap_or(raw_roster);
    let enrolled = accounts::enrolled(&roster);
    let (state, _) = store.load(env.now).unwrap_or_default();

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
            (&[], &[]),
        )
        .expect("preserve the current default without a profile hint");
        assert_eq!(kept.roster.row_for_login(None).unwrap().label, "m8-2");
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
        let reset = persist_accounts_sync(
            &store,
            &roster_path,
            (&fleet, OptIn::Named),
            ("m8", None),
            &empty_walls,
            now,
            (&[], &[]),
        )
        .unwrap();

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
        fetch: Box<Fetch>,
        spawn: Box<SpawnRenewal>,
        cooldown: std::sync::Mutex<Option<Instant>>,
        guard: std::sync::Arc<std::sync::Mutex<AutoRenewGuard>>,
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
                guard: std::sync::Arc::new(std::sync::Mutex::new(AutoRenewGuard::default())),
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

    const LOOPBACK_SOURCE: &str = "http://127.0.0.1:9/fleet.csv";
    const ONE_ACCOUNT_CSV: &str = "email,computer,claude\nm1@example.test,m1,1\n";
    const UPKEEP_NOW: i64 = 1_792_660_000;
    const UNCONFIGURED: Upkept = Upkept::Unconfigured {
        fleet_file_ignored: false,
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
                    fleet_file_ignored: true
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
                fleet_file_ignored: true
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

        fs::create_dir_all(&mailbox_dir).unwrap();
        let fleet_aside = w.config.join("fleet-accounts.aside");
        fs::rename(w.fleet_file(), &fleet_aside).unwrap();

        let upkept = |step| upkeep(&w.home, &w.state, (&w.roster(), now, step), &wit.fx());
        assert_eq!(upkept(UpkeepStep::Both), UNCONFIGURED);
        assert_eq!(wit.spawned(), 0, "no renewal without a configured source");

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
            fs::write(&source, initial.to_toml_for(Some(OptIn::Named))).unwrap();
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
                email: Some("sol-m27@example.test".into()),
                browser: None,
            };
            let roster = Roster { rows: vec![row] };
            let staging = store.claim_staging_dir("m27").unwrap();
            let profile = br#"{"oauthAccount":{"emailAddress":"sol-m27@example.test"}}"#;
            fs::write(staging.join(".claude.json"), profile).unwrap();
            let checked_at = UPKEEP_NOW - 10;
            accounts::write_renewal_checkpoint(
                "m27",
                "sol-m27@example.test",
                staging.to_str().unwrap(),
                Some("max"),
                checked_at,
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
                "schema" | "program" | "label" | "email" | "dir" | "future" | "zero"
                | "subscription" => {
                    let (key, value) = match case {
                        "schema" => ("schema", Value::from(2)),
                        "program" => ("program", Value::from("other")),
                        "label" => ("label", Value::from("m28")),
                        "email" => ("email", Value::from("someone@example.test")),
                        "dir" => ("dir", Value::from(w.home.to_str().unwrap())),
                        "future" => ("checked_at", Value::from(UPKEEP_NOW + 1)),
                        "zero" => ("checked_at", Value::from(0)),
                        _ => ("subscription", Value::from("x".repeat(33))),
                    };
                    proof.as_object_mut().unwrap().insert(key.into(), value);
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
                    dir: None,
                    was: Vec::new(),
                    email: Some(email.into()),
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
    fn recover_interrupted_staging_renewals_adopts_completed_login_and_preserves_other_work() {
        let w = World::new();
        let store = accounts::Store::at(&w.state);
        let mut roster = Roster {
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
                    email: Some("sol-m27@example.test".to_owned()),
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
                    email: Some("sun.m27.2@example.test".to_owned()),
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
        )
        .unwrap();
        roster.rows[1]
            .was
            .push(Some(another.to_string_lossy().into_owned()));
        let alias = completed.join(".").to_string_lossy().into_owned();
        store
            .transact_logins(UPKEEP_NOW + 30, &mut None, |state, logins| {
                logins.labels.get_mut("m27-2").unwrap().dir = Some(alias.clone());
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
            renew_guard: &guard,
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
                fleet_file_ignored: false
            }
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
                &w.home,
                &w.state,
                roster.clone(),
                &accounts::Logins::default(),
                roster.rows[1].clone(),
                (None, true),
                UPKEEP_NOW,
                &guard,
                &spawn
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
                email: Some(email.into()),
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
        let staged_m27 = store.claim_staging_dir("m27").unwrap();
        fs::write(staged_m27.join(".credentials.json"), b"{}").unwrap();
        accounts::write_renewal_checkpoint(
            "m27",
            "sol-m27@example.test",
            staged_m27.to_str().unwrap(),
            Some("max"),
            UPKEEP_NOW - 10,
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
    fn staging_completion_failures_stop_fallback_and_keep_verified_logins() {
        use super::super::cli::Renewal;

        for case in [
            "checkpoint",
            "ledger",
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
            let staging = store.claim_staging_dir("main").unwrap();
            let profile = b"private verified login fixture";
            fs::write(staging.join(".claude.json"), profile).unwrap();
            let checkpoint = staging.join(accounts::RENEWAL_CHECKPOINT_FILE);
            if case == "checkpoint" {
                fs::write(&checkpoint, b"unrecognized private work").unwrap();
            } else if case == "ledger" {
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
                &roster,
                "main",
                &staging,
                staging.to_str().unwrap(),
                &status,
                UPKEEP_NOW,
            );
            match case {
                "checkpoint" | "ledger" => {
                    assert_eq!(outcome, Renewal::Failed);
                    assert!(!outcome.can_retry_authentication());
                    assert_eq!(fs::read(staging.join(".claude.json")).unwrap(), profile);
                    if case == "checkpoint" {
                        assert_eq!(fs::read(&checkpoint).unwrap(), b"unrecognized private work");
                        assert!(!store.logins_file().exists() && !store.state_file().exists());
                        assert!(store.tail(10).unwrap().is_empty());
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
                    email: Some("sol-m27@example.test".to_owned()),
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
                    email: Some("sun.m27.2@example.test".to_owned()),
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
                    email: Some("star.m27.3@example.test".to_owned()),
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

        let wit = Witness::answering("");
        assert!(
            auto_renew_next_account(&w.home, &w.state, &w.roster(), UPKEEP_NOW + 10, &wit.fx()),
            "auto_renew_next_account must spawn renewal for m27-3"
        );
        assert_eq!(wit.spawned.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(
            wit.guard.lock().unwrap().last_attempt.contains_key("m27-3"),
            "m27-3 must be the account claimed by AutoRenewPermit"
        );

        // Even with a fresh AutoRenewGuard (as after a process restart or CLI run),
        // if m27-2 has a browser profile and m27-3 does not, background auto_renew_next_account
        // must respect m27-2's persisted 15-minute wrong_at cooldown before retrying it.
        let mut roster_m27_2_browser = roster.clone();
        roster_m27_2_browser.rows[1].browser = Some("Claude m27-2".to_owned());
        roster_m27_2_browser.rows[2].browser = None;
        let mut roster_m27_2_toml = String::new();
        for r in &roster_m27_2_browser.rows {
            roster_m27_2_toml.push_str(&accounts::row_text(r).unwrap());
            roster_m27_2_toml.push('\n');
        }
        fs::write(w.roster(), &roster_m27_2_toml).unwrap();
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
}
