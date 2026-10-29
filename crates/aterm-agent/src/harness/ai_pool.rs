// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Reader and canonical TOML representation for `~/.secrets/ai-pool.toml`
//! (`docs/DESIGN-alab-swarm-2026-10-27.md` §4.1, §6.3, §6.5, §12.3 C14).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Duration;

use super::accounts::{self, Class, Cli, Vendor};
use crate::ai::connector::{Flow, Region, RowKeys};

/// Vault-delivered account pool file in `~/.secrets/`.
pub const AI_POOL_FILE: &str = "ai-pool.toml";

/// Maximum warm Macs per account in `ai-pool.toml` (`DESIGN-alab-swarm` §6.3).
pub const MAX_WARM_SLOTS: usize = 3;

/// Holder word recorded in `held_by` when an account has no current warm Mac in `ai-pool.toml`.
pub const POOL_HOLDER_UNASSIGNED: &str = "pool";

/// Reason an account holds a warm slot in `ai-pool.toml` (`DESIGN-alab-swarm` §6.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotWhy {
    /// Pinned by the accounting sheet's `computer` cell.
    Pin,
    /// Assigned by the allocator's normal fill pass.
    Fill,
    /// Freed from a silent or `NotEnrolled` holder.
    Stranded,
    /// Single-slot probation assignment for a newly eligible Mac.
    Probation,
    /// Assigned after a walled Mac borrowed an idle account.
    Borrow,
}

impl SlotWhy {
    /// Canonical `ai-pool.toml` string for this slot reason.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pin => "pin",
            Self::Fill => "fill",
            Self::Stranded => "stranded",
            Self::Probation => "probation",
            Self::Borrow => "borrow",
        }
    }

    /// Parse a [`SlotWhy`] from its `ai-pool.toml` word.
    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        match word {
            "pin" => Some(Self::Pin),
            "fill" => Some(Self::Fill),
            "stranded" => Some(Self::Stranded),
            "probation" => Some(Self::Probation),
            "borrow" => Some(Self::Borrow),
            _ => None,
        }
    }
}

/// Extract a canonical `(N, "m<N>")` label ONLY when `name` starts with `m<N>` (`N >= 1`).
///
/// Mid-string tokens such as `"Mac-mini-M4"` or `"macbook-m3"` return `None`.
#[must_use]
pub fn leading_m_label(name: &str) -> Option<(u32, String)> {
    let trimmed = name.trim_ascii();
    let bytes = trimmed.as_bytes();
    let (&first, rest) = bytes.split_first()?;
    if first != b'm' && first != b'M' {
        return None;
    }
    let (&first_digit, _) = rest.split_first()?;
    if !first_digit.is_ascii_digit() || first_digit == b'0' {
        return None;
    }
    let digit_len = rest.iter().take_while(|b| b.is_ascii_digit()).count();
    if digit_len == 0 || digit_len > 6 {
        return None;
    }
    if let Some(&after) = rest.get(digit_len)
        && after.is_ascii_alphanumeric()
    {
        return None;
    }
    let digits = std::str::from_utf8(&rest[..digit_len]).ok()?;
    let n: u32 = digits.parse().ok()?;
    (n >= 1).then(|| (n, format!("m{n}")))
}

/// This Mac's canonical swarm label and legacy aliases used to match `warm` sets in `ai-pool.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalPoolIdentity {
    /// Active canonical `m<N>` label (e.g. `"m100"`).
    pub primary: String,
    /// Sorted legacy `m<N>` aliases (e.g. `["m32"]`), never duplicating `primary`.
    pub aliases: Vec<String>,
    /// True when the label came from `aterm-swarm-roster` (so hostname mid-string parsing never ran).
    pub from_roster: bool,
}

impl LocalPoolIdentity {
    /// Construct an identity with normalized, deduplicated aliases.
    #[must_use]
    pub fn new(primary: String, aliases: Vec<String>, from_roster: bool) -> Self {
        let mut clean = BTreeSet::new();
        for alias in aliases {
            if let Some((_, canon)) = leading_m_label(&alias)
                && canon != primary
            {
                clean.insert(canon);
            }
        }
        Self {
            primary,
            aliases: clean.into_iter().collect(),
            from_roster,
        }
    }

    /// True if `candidate` matches `self.primary` or any of `self.aliases`.
    #[must_use]
    pub fn matches(&self, candidate: &str) -> bool {
        let Some((_, canon)) = leading_m_label(candidate) else {
            return candidate.eq_ignore_ascii_case(&self.primary);
        };
        canon == self.primary || self.aliases.iter().any(|a| a == &canon)
    }
}

/// Pure resolution of this Mac's [`LocalPoolIdentity`] (`DESIGN-alab-swarm` §6.5).
///
/// Once the swarm roster gives this Mac a label (by age recipient or by matching `config_machine`),
/// warm sets match by that label and its roster aliases, never by `normalize_machine_id` of any
/// hostname. Even without a roster, hostname fallback accepts only names that *start* with `m<N>`
/// (`leading_m_label`), so `"Mac-mini-M4"` is never `"m4"`.
#[must_use]
pub fn resolve_pool_identity_pure(
    roster_members: &[(String, String, Vec<String>)],
    recipient: Option<&str>,
    config_machine: Option<&str>,
    machine_override: Option<&str>,
    hostnames: &[String],
) -> Option<LocalPoolIdentity> {
    let find_by_label = |label: &str| {
        roster_members.iter().find(|(_, primary, aliases)| {
            primary == label || aliases.iter().any(|alias| alias == label)
        })
    };

    if let Some(raw) = machine_override {
        let norm = leading_m_label(raw)
            .map(|(_, l)| l)
            .or_else(|| super::accounts_sync::normalize_machine_id(raw))?;
        if let Some((_, primary, aliases)) = find_by_label(&norm) {
            let mut all_aliases = aliases.clone();
            if *primary != norm {
                all_aliases.push(primary.clone());
            }
            return Some(LocalPoolIdentity::new(norm, all_aliases, true));
        }
        return Some(LocalPoolIdentity::new(norm, Vec::new(), false));
    }

    if let Some(rec) = recipient
        && let Some((_, primary, aliases)) = roster_members.iter().find(|(r, _, _)| r == rec)
    {
        return Some(LocalPoolIdentity::new(
            primary.clone(),
            aliases.clone(),
            true,
        ));
    }

    if let Some(cfg_raw) = config_machine
        && let Some(cfg_norm) = leading_m_label(cfg_raw)
            .map(|(_, l)| l)
            .or_else(|| super::accounts_sync::normalize_machine_id(cfg_raw))
    {
        if let Some((_, primary, aliases)) = find_by_label(&cfg_norm) {
            return Some(LocalPoolIdentity::new(
                primary.clone(),
                aliases.clone(),
                true,
            ));
        }
        return Some(LocalPoolIdentity::new(
            cfg_norm,
            Vec::new(),
            !roster_members.is_empty(),
        ));
    }

    if !roster_members.is_empty() {
        return None;
    }

    for host in hostnames {
        if let Some((_, canon)) = leading_m_label(host) {
            return Some(LocalPoolIdentity::new(canon, Vec::new(), false));
        }
    }
    None
}

/// Resolve this Mac's [`LocalPoolIdentity`] from the local vault state, `aterm.toml`, and system
/// hostnames.
#[must_use]
pub fn resolve_local_pool_identity(
    home: Option<&Path>,
    config: Option<&Path>,
    machine_override: Option<&str>,
) -> Option<LocalPoolIdentity> {
    let config_machine = read_config_machine_raw(config);
    #[cfg(unix)]
    let (roster_members, recipient, vault_fallback) = if let Some(home) = home {
        let paths = aterm_secrets::client::Paths::for_home(home);
        let denied_nodes = aterm_secrets::client::swarm_denied_nodes(&paths)
            .ok()
            .flatten()
            .unwrap_or_default();
        let members = aterm_secrets::client::swarm_roster(&paths)
            .ok()
            .flatten()
            .map(|mut r| {
                r.deny_nodes.extend(denied_nodes);
                r.canonicalize();
                let deny_labels = std::mem::take(&mut r.deny_labels);
                let deny_nodes = std::mem::take(&mut r.deny_nodes);
                let is_label_denied = |lbl: &str| {
                    deny_labels
                        .iter()
                        .any(|d| d.eq_ignore_ascii_case(lbl.trim()))
                };
                let is_node_denied =
                    |n: &str| deny_nodes.iter().any(|d| d.eq_ignore_ascii_case(n.trim()));
                r.members
                    .into_iter()
                    .filter(|m| {
                        !is_label_denied(&m.label)
                            && !m.aliases.iter().any(|a| is_label_denied(a))
                            && !m.node_id.as_deref().is_some_and(is_node_denied)
                            && !m.node_key.as_deref().is_some_and(is_node_denied)
                    })
                    .map(|m| (m.recipient, m.label, m.aliases))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let rec = aterm_secrets::client::status(&paths)
            .ok()
            .and_then(|s| s.machine)
            .map(|(rec, _)| rec.to_bech32());
        let fallback = if members.is_empty() {
            aterm_secrets::client::resolve_local_swarm_label(&paths, &[])
                .ok()
                .flatten()
        } else {
            None
        };
        (members, rec, fallback)
    } else {
        (Vec::new(), None, None)
    };
    #[cfg(not(unix))]
    let (roster_members, recipient, vault_fallback): (
        Vec<(String, String, Vec<String>)>,
        Option<String>,
        Option<String>,
    ) = {
        let _ = home;
        (Vec::new(), None, None)
    };

    let effective_config = config_machine.or(vault_fallback);
    if let Some(resolved) = resolve_pool_identity_pure(
        &roster_members,
        recipient.as_deref(),
        effective_config.as_deref(),
        machine_override,
        &[],
    ) {
        return Some(resolved);
    }
    if !roster_members.is_empty() || machine_override.is_some() {
        return None;
    }
    let hosts = system_hostnames();
    resolve_pool_identity_pure(
        &roster_members,
        recipient.as_deref(),
        effective_config.as_deref(),
        machine_override,
        &hosts,
    )
}

fn read_config_machine_raw(config: Option<&Path>) -> Option<String> {
    let text = std::fs::read_to_string(config?).ok()?;
    let table = aterm_toml::from_str::<aterm_toml::Table>(&text).ok()?;
    for (section, key) in [
        ("swarm", "label"),
        ("ai", "machine"),
        ("fleet", "machine"),
        ("harness", "machine"),
    ] {
        if let Some(m) = table
            .get(section)
            .and_then(aterm_toml::Value::as_table)
            .and_then(|t| t.get(key))
            .and_then(aterm_toml::Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return Some(m.to_owned());
        }
    }
    None
}

fn system_hostnames() -> Vec<String> {
    let mut out = Vec::new();
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
        {
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                out.push(trimmed.to_owned());
            }
        }
    }
    out
}

/// One account row in `ai-pool.toml` (`DESIGN-alab-swarm` §6.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolAccount {
    /// Pooled CLI this row belongs to.
    pub provider: Cli,
    /// Vendor keys (`region` for Kimi, `flow` for Codex/Grok).
    pub vendor: Vendor,
    /// Optional explicit roster label; when `None`, derived deterministically.
    pub label: Option<String>,
    /// Account login address (`name@domain`).
    pub email: String,
    /// Optional browser profile hint.
    pub browser: Option<String>,
    /// Up to [`MAX_WARM_SLOTS`] canonical `m<N>` labels in order; the first is the account's home.
    pub warm: Vec<String>,
    /// Per-slot assignment date/timestamp strings (parallel to `warm` when populated).
    pub since: Vec<String>,
    /// Per-slot assignment reasons (parallel to `warm` when populated).
    pub why: Vec<SlotWhy>,
    /// Per-slot last-used UTC day strings (parallel to `warm` when populated).
    pub used: Vec<String>,
}

impl PoolAccount {
    /// The account's home Mac (`warm[0]`), if any warm Mac is assigned.
    #[must_use]
    pub fn home_mac(&self) -> Option<&str> {
        self.warm.first().map(String::as_str)
    }

    /// Canonical holder word for `held_by` when this Mac is not warm for the account.
    #[must_use]
    pub fn holder_word(&self) -> &str {
        self.home_mac().unwrap_or(POOL_HOLDER_UNASSIGNED)
    }

    /// True if `identity` (primary label or any alias) is in this account's `warm` set.
    #[must_use]
    pub fn is_warm_for(&self, identity: &LocalPoolIdentity) -> bool {
        self.warm.iter().any(|slot| identity.matches(slot))
    }

    /// Contention class implied by the warm set (`Dedicated` for ≤1 warm Mac, `Shared` for >1).
    #[must_use]
    pub fn class(&self) -> Class {
        if self.warm.len() > 1 {
            Class::Shared
        } else {
            Class::Dedicated
        }
    }

    /// Canonical provider string written to `ai-pool.toml`.
    #[must_use]
    pub fn provider_str(&self) -> &'static str {
        match (self.provider, self.vendor.region) {
            (Cli::Claude, _) => "claude",
            (Cli::Codex, _) => "codex",
            (Cli::Grok, _) => "grok",
            (Cli::Kimi, Some(Region::MainlandCn)) => "kimi:mainland-cn",
            (Cli::Kimi, Some(Region::Global) | None) => "kimi:global",
        }
    }
}

/// Complete parsed `ai-pool.toml` document.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Pool {
    /// Human-readable source path or description.
    pub source: String,
    /// Per-label last-heard UTC day (`"m22" -> "2026-10-27"`), carried across lease handoffs.
    pub last_heard: BTreeMap<String, String>,
    /// All pooled accounts across providers.
    pub accounts: Vec<PoolAccount>,
}

impl Pool {
    /// True if `chosen` or `raw` is an `ai-pool.toml` document rather than a legacy
    /// `fleet-accounts.toml` or CSV sheet.
    #[must_use]
    pub fn looks_like_pool(chosen: &str, raw: &str) -> bool {
        if Path::new(chosen)
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|name| name == AI_POOL_FILE)
        {
            return true;
        }
        let Ok(table) = aterm_toml::from_str::<aterm_toml::Table>(raw) else {
            return false;
        };
        if table.get("last_heard").is_some() {
            return true;
        }
        table
            .get("account")
            .and_then(aterm_toml::Value::as_array)
            .is_some_and(|rows| {
                rows.iter().any(|item| {
                    item.as_table()
                        .is_some_and(|t| t.get("warm").is_some() || t.get("provider").is_some())
                })
            })
    }

    /// Parse `ai-pool.toml` text into a validated [`Pool`].
    ///
    /// # Errors
    /// Returns a human-readable error if the TOML is malformed, contains invalid labels/emails,
    /// exceeds [`MAX_WARM_SLOTS`] on any row, or duplicates an email in the same provider realm.
    pub fn from_toml(text: &str, source: &str) -> Result<Self, String> {
        let table: aterm_toml::Table = aterm_toml::from_str(text)
            .map_err(|e| format!("account pool at {source} is not valid TOML: {e}"))?;
        for key in table.keys() {
            if !matches!(
                key.as_str(),
                "version" | "last_heard" | "leases" | "lease" | "account"
            ) {
                return Err(format!(
                    "account pool at {source}: unknown top-level key `{key}`"
                ));
            }
        }
        if let Some(ver) = table.get("version") {
            let Some(v) = ver.as_integer() else {
                return Err(format!(
                    "account pool at {source}: `version` must be integer 1"
                ));
            };
            if v != 1 {
                return Err(format!(
                    "account pool at {source}: unsupported pool version `{v}`"
                ));
            }
        }
        let mut last_heard = BTreeMap::new();
        if let Some(lh_val) = table.get("last_heard") {
            let lh = lh_val
                .as_table()
                .ok_or_else(|| format!("account pool at {source}: `last_heard` must be a table"))?;
            for (raw_label, day_val) in lh {
                let (_, canon) = leading_m_label(raw_label).ok_or_else(|| {
                    format!("account pool at {source}: invalid `last_heard` label `{raw_label}`")
                })?;
                let day = day_val.as_str().ok_or_else(|| {
                    format!(
                        "account pool at {source}: `last_heard.{raw_label}` must be a date string"
                    )
                })?;
                last_heard.insert(canon, day.trim().to_owned());
            }
        }
        let mut accounts = Vec::new();
        let Some(raw_rows) = table.get("account") else {
            return Ok(Self {
                source: source.to_owned(),
                last_heard,
                accounts,
            });
        };
        let rows = raw_rows
            .as_array()
            .ok_or_else(|| format!("account pool at {source}: `account` must be `[[account]]`"))?;
        for (idx, item) in rows.iter().enumerate() {
            let n = idx + 1;
            let row = item
                .as_table()
                .ok_or_else(|| format!("account pool at {source} row {n}: expected table"))?;
            let acct = parse_pool_row(row, n, source)?;
            if accounts.iter().any(|prev: &PoolAccount| {
                prev.provider == acct.provider
                    && accounts::same_provider_realm(
                        acct.provider,
                        prev.vendor.region,
                        acct.vendor.region,
                    )
                    && accounts::email_matches(&prev.email, &acct.email)
            }) {
                return Err(format!(
                    "account pool at {source} row {n}: duplicate email `{}` in provider `{}`",
                    acct.email,
                    acct.provider_str()
                ));
            }
            if let Some(label) = &acct.label
                && accounts.iter().any(|prev: &PoolAccount| {
                    prev.provider == acct.provider && prev.label.as_ref() == Some(label)
                })
            {
                return Err(format!(
                    "account pool at {source} row {n}: duplicate label `{label}` in provider `{}`",
                    acct.provider.id()
                ));
            }
            accounts.push(acct);
        }
        Ok(Self {
            source: source.to_owned(),
            last_heard,
            accounts,
        })
    }

    /// Serialize this pool to canonical `ai-pool.toml` text.
    #[must_use]
    pub fn to_toml(&self) -> String {
        let mut out = String::new();
        if !self.last_heard.is_empty() {
            out.push_str("[last_heard]\n");
            for (label, day) in &self.last_heard {
                out.push_str(&format!("{label} = {}\n", toml_str(day)));
            }
            out.push('\n');
        }
        for acct in &self.accounts {
            out.push_str("[[account]]\n");
            out.push_str(&format!("provider = {}\n", toml_str(acct.provider_str())));
            if let Some(label) = &acct.label {
                out.push_str(&format!("label = {}\n", toml_str(label)));
            }
            out.push_str(&format!("email = {}\n", toml_str(&acct.email)));
            if let Some(browser) = &acct.browser {
                out.push_str(&format!("browser = {}\n", toml_str(browser)));
            }
            if let Some(flow) = acct.vendor.flow {
                out.push_str(&format!("flow = {}\n", toml_str(flow.as_str())));
            }
            out.push_str(&format!("warm = {}\n", toml_str_array(&acct.warm)));
            if !acct.since.is_empty() {
                out.push_str(&format!("since = {}\n", toml_str_array(&acct.since)));
            }
            if !acct.why.is_empty() {
                let whys: Vec<String> = acct.why.iter().map(|w| w.as_str().to_owned()).collect();
                out.push_str(&format!("why = {}\n", toml_str_array(&whys)));
            }
            if !acct.used.is_empty() {
                out.push_str(&format!("used = {}\n", toml_str_array(&acct.used)));
            }
            out.push('\n');
        }
        out
    }

    /// Return all pool rows for `cli`, paired with deterministic, collision-free labels.
    #[must_use]
    pub fn accounts_for_cli(&self, cli: Cli) -> Vec<(&PoolAccount, String)> {
        let rows: Vec<&PoolAccount> = self.accounts.iter().filter(|a| a.provider == cli).collect();
        let mut used = BTreeSet::new();
        for acct in &rows {
            if let Some(explicit) = &acct.label {
                used.insert(explicit.clone());
            }
        }
        let mut out = Vec::with_capacity(rows.len());
        for (idx, acct) in rows.into_iter().enumerate() {
            if let Some(explicit) = &acct.label {
                out.push((acct, explicit.clone()));
                continue;
            }
            let base = default_pool_label_base(cli, acct, idx + 1);
            let mut candidate = base.clone();
            let mut suffix = 2u32;
            while !accounts::admissible_label(&candidate) || used.contains(&candidate) {
                candidate = format!("{base}-{suffix}");
                suffix = suffix.saturating_add(1);
            }
            used.insert(candidate.clone());
            out.push((acct, candidate));
        }
        out
    }
}

fn default_pool_label_base(cli: Cli, acct: &PoolAccount, ordinal: usize) -> String {
    let stem = if let Some(home) = acct.home_mac() {
        home.to_owned()
    } else if let Some(local) = acct.email.split('@').next() {
        let cleaned: String = local
            .bytes()
            .map(|b| {
                let lower = b.to_ascii_lowercase();
                if lower.is_ascii_alphanumeric() || matches!(lower, b'.' | b'_' | b'-') {
                    char::from(lower)
                } else {
                    '-'
                }
            })
            .collect();
        let trimmed = cleaned.trim_matches(['.', '_', '-']);
        if trimmed.is_empty() || !trimmed.as_bytes()[0].is_ascii_alphanumeric() {
            format!("pool-{ordinal}")
        } else {
            let capped = super::truncate_bytes(trimmed, 40);
            if capped == accounts::RESERVED_LABEL {
                format!("pool-{ordinal}")
            } else {
                capped.to_owned()
            }
        }
    } else {
        format!("pool-{ordinal}")
    };
    match cli {
        Cli::Claude => stem,
        _ => format!("{}-{stem}", cli.id()),
    }
}

fn parse_pool_row(row: &aterm_toml::Table, n: usize, source: &str) -> Result<PoolAccount, String> {
    for key in row.keys() {
        if !matches!(
            key.as_str(),
            "provider"
                | "label"
                | "email"
                | "browser"
                | "region"
                | "flow"
                | "warm"
                | "since"
                | "why"
                | "used"
        ) {
            return Err(format!(
                "account pool at {source} row {n}: unknown key `{key}`"
            ));
        }
    }
    let provider_raw = row
        .get("provider")
        .and_then(aterm_toml::Value::as_str)
        .unwrap_or("claude");
    let (provider, realm_region) = parse_provider_realm(provider_raw).ok_or_else(|| {
        format!("account pool at {source} row {n}: unknown provider `{provider_raw}`")
    })?;
    let email = row
        .get("email")
        .and_then(aterm_toml::Value::as_str)
        .ok_or_else(|| format!("account pool at {source} row {n}: missing `email`"))?
        .to_owned();
    if let Some(why) = accounts::email_refusal(&email) {
        return Err(format!("account pool at {source} row {n}: email {why}"));
    }
    let label = match row.get("label") {
        None => None,
        Some(aterm_toml::Value::String(s)) => {
            if let Some(why) = accounts::label_refusal(s) {
                return Err(format!(
                    "account pool at {source} row {n}: label `{s}` {why}"
                ));
            }
            Some(s.clone())
        }
        Some(other) => {
            return Err(format!(
                "account pool at {source} row {n}: `label` is a TOML {}, not a string",
                other.type_str()
            ));
        }
    };
    let browser = match row.get("browser") {
        None => None,
        Some(aterm_toml::Value::String(s)) => {
            if let Some(why) = accounts::browser_refusal(s) {
                return Err(format!("account pool at {source} row {n}: browser {why}"));
            }
            Some(s.clone())
        }
        Some(other) => {
            return Err(format!(
                "account pool at {source} row {n}: `browser` is a TOML {}, not a string",
                other.type_str()
            ));
        }
    };
    let explicit_region = match row.get("region") {
        None => None,
        Some(aterm_toml::Value::String(w)) => Some(
            Region::parse(w)
                .ok_or_else(|| format!("account pool at {source} row {n}: invalid region `{w}`"))?,
        ),
        Some(other) => {
            return Err(format!(
                "account pool at {source} row {n}: `region` is a TOML {}, not a string",
                other.type_str()
            ));
        }
    };
    let region = match provider.accounts().keys {
        RowKeys::Region => {
            if let (Some(a), Some(b)) = (realm_region, explicit_region)
                && a != b
            {
                return Err(format!(
                    "account pool at {source} row {n}: provider `{provider_raw}` conflicts with `region`"
                ));
            }
            Some(explicit_region.or(realm_region).ok_or_else(|| {
                format!(
                    "account pool at {source} row {n}: Kimi row requires `kimi:global` or `kimi:mainland-cn` (or `region`)"
                )
            })?)
        }
        _ => {
            if explicit_region.is_some() {
                return Err(format!(
                    "account pool at {source} row {n}: provider `{provider_raw}` takes no `region`"
                ));
            }
            None
        }
    };
    let flow = match row.get("flow") {
        None => None,
        Some(aterm_toml::Value::String(w)) => {
            if !matches!(provider.accounts().keys, RowKeys::Flow(_)) {
                return Err(format!(
                    "account pool at {source} row {n}: provider `{provider_raw}` takes no `flow`"
                ));
            }
            Some(
                Flow::parse(w).ok_or_else(|| {
                    format!("account pool at {source} row {n}: invalid flow `{w}`")
                })?,
            )
        }
        Some(other) => {
            return Err(format!(
                "account pool at {source} row {n}: `flow` is a TOML {}, not a string",
                other.type_str()
            ));
        }
    };

    let raw_warm = string_vec(row, "warm", n, source)?.unwrap_or_default();
    if raw_warm.len() > MAX_WARM_SLOTS {
        return Err(format!(
            "account pool at {source} row {n}: `warm` has {} entries (max {MAX_WARM_SLOTS})",
            raw_warm.len()
        ));
    }
    let mut warm = Vec::with_capacity(raw_warm.len());
    let mut seen_warm = BTreeSet::new();
    for entry in raw_warm {
        let (_, canon) = leading_m_label(&entry).ok_or_else(|| {
            format!("account pool at {source} row {n}: invalid warm label `{entry}`")
        })?;
        if !seen_warm.insert(canon.clone()) {
            return Err(format!(
                "account pool at {source} row {n}: duplicate warm label `{canon}`"
            ));
        }
        warm.push(canon);
    }

    let since = string_vec(row, "since", n, source)?.unwrap_or_default();
    if !since.is_empty() && since.len() != warm.len() {
        return Err(format!(
            "account pool at {source} row {n}: `since` length ({}) does not match `warm` ({})",
            since.len(),
            warm.len()
        ));
    }
    let raw_why = string_vec(row, "why", n, source)?.unwrap_or_default();
    if !raw_why.is_empty() && raw_why.len() != warm.len() {
        return Err(format!(
            "account pool at {source} row {n}: `why` length ({}) does not match `warm` ({})",
            raw_why.len(),
            warm.len()
        ));
    }
    let mut why = Vec::with_capacity(raw_why.len());
    for word in raw_why {
        why.push(SlotWhy::parse(&word).ok_or_else(|| {
            format!("account pool at {source} row {n}: unknown `why` reason `{word}`")
        })?);
    }
    let used = string_vec(row, "used", n, source)?.unwrap_or_default();
    if !used.is_empty() && used.len() != warm.len() {
        return Err(format!(
            "account pool at {source} row {n}: `used` length ({}) does not match `warm` ({})",
            used.len(),
            warm.len()
        ));
    }

    Ok(PoolAccount {
        provider,
        vendor: Vendor { region, flow },
        label,
        email,
        browser,
        warm,
        since,
        why,
        used,
    })
}

fn parse_provider_realm(raw: &str) -> Option<(Cli, Option<Region>)> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "claude" | "anthropic" => Some((Cli::Claude, None)),
        "codex" | "openai" => Some((Cli::Codex, None)),
        "grok" | "x" | "xai" => Some((Cli::Grok, None)),
        "kimi" => Some((Cli::Kimi, None)),
        "kimi:global" | "kimi-global" => Some((Cli::Kimi, Some(Region::Global))),
        "kimi:mainland-cn" | "kimi-mainland-cn" | "kimi:cn" | "kimi-cn" => {
            Some((Cli::Kimi, Some(Region::MainlandCn)))
        }
        _ => None,
    }
}

fn string_vec(
    row: &aterm_toml::Table,
    key: &str,
    n: usize,
    source: &str,
) -> Result<Option<Vec<String>>, String> {
    let Some(val) = row.get(key) else {
        return Ok(None);
    };
    let arr = val.as_array().ok_or_else(|| {
        format!(
            "account pool at {source} row {n}: `{key}` is a TOML {}, not an array",
            val.type_str()
        )
    })?;
    let mut out = Vec::with_capacity(arr.len());
    for (i, item) in arr.iter().enumerate() {
        let s = item.as_str().ok_or_else(|| {
            format!(
                "account pool at {source} row {n}: `{key}[{}]` is a TOML {}, not a string",
                i + 1,
                item.type_str()
            )
        })?;
        out.push(s.to_owned());
    }
    Ok(Some(out))
}

fn toml_str(s: &str) -> String {
    aterm_toml::Value::String(s.to_owned()).to_string()
}

fn toml_str_array(items: &[String]) -> String {
    let parts: Vec<String> = items.iter().map(|s| toml_str(s)).collect();
    format!("[{}]", parts.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::accounts;
    #[cfg(unix)]
    use crate::harness::accounts_sync;
    use std::collections::BTreeSet;
    #[cfg(unix)]
    use std::fs;

    const NOW: i64 = 1_762_000_000;

    #[cfg(unix)]
    fn seed_local_swarm_roster(home: &Path, label: &str, aliases: &[&str]) {
        let srv_dir = aterm_tempfile::tempdir().expect("server dir");
        let socket = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let running = aterm_secrets::server::start(
            socket,
            &aterm_secrets::server::Options {
                listen: "127.0.0.1:0".parse().unwrap(),
                store: srv_dir.path().to_path_buf(),
                advertise: None,
                pins: Vec::new(),
                hold: Duration::from_millis(50),
                keep_awake: false,
            },
        )
        .expect("start server");
        let url = format!("http://{}/v1/test", running.addr);
        let paths = aterm_secrets::client::Paths::for_home(home);
        aterm_secrets::client::create(&paths, &url, None, Some(label)).expect("create vault");
        let recipient = aterm_secrets::client::machine_identity(&paths)
            .expect("identity")
            .recipient()
            .to_bech32();
        let adm = aterm_secrets::client::admin_open(&paths).expect("admin open");
        let mut doc = adm.doc.clone();
        let roster = aterm_secrets::swarm_roster::SwarmRoster {
            members: vec![aterm_secrets::swarm_roster::SwarmMember {
                recipient,
                vault_name: label.to_owned(),
                label: label.to_owned(),
                aliases: aliases.iter().map(|s| (*s).to_owned()).collect(),
                hint: None,
                admitted_ms: 1,
                claimed_label: None,
                claimed_at_ms: None,
                node_id: None,
                node_key: None,
            }],
            deny_labels: Vec::new(),
            deny_nodes: Vec::new(),
        };
        doc.set_entry(
            &aterm_secrets::text::Audience::Everyone,
            aterm_secrets::swarm_roster::ROSTER_ENTRY,
            roster.to_toml().as_bytes(),
        )
        .expect("set roster");
        aterm_secrets::client::admin_publish(&paths, &adm, &doc).expect("publish roster");
        aterm_secrets::client::sync(&paths, None).expect("sync local copy");
    }

    #[test]
    fn pool_toml_round_trips_and_refuses_invalid_rows() {
        let raw = r#"
version = 1

[last_heard]
m3 = "2026-10-27"
m14 = "2026-10-27"

[[account]]
provider = "claude"
email = "a01@example.test"
warm = ["m3", "m14"]
since = ["2026-10-20T00:00:00Z", "2026-10-26T04:12:00Z"]
why = ["pin", "fill"]
used = ["2026-10-27", "2026-10-27"]

[[account]]
provider = "claude"
email = "a19@example.test"
warm = []

[[account]]
provider = "codex"
flow = "device"
email = "c01@example.test"
warm = ["m1", "m7", "m12"]

[[account]]
provider = "kimi:global"
email = "k01@example.test"
warm = ["m3"]

[[account]]
provider = "kimi:mainland-cn"
email = "k01@example.test"
warm = ["m3"]
"#;
        let parsed = Pool::from_toml(raw, "ai-pool.toml").expect("valid pool parses");
        assert_eq!(parsed.accounts.len(), 5);
        assert_eq!(parsed.accounts[0].home_mac(), Some("m3"));
        assert_eq!(parsed.accounts[0].class(), Class::Shared);
        assert_eq!(parsed.accounts[1].holder_word(), POOL_HOLDER_UNASSIGNED);
        assert_eq!(parsed.accounts[3].class(), Class::Dedicated);

        let rendered = parsed.to_toml();
        let reparsed = Pool::from_toml(&rendered, "round-trip").expect("rendered pool parses");
        assert_eq!(reparsed.accounts, parsed.accounts);
        assert_eq!(reparsed.last_heard, parsed.last_heard);

        // Negative controls: bad version, >3 warm slots, duplicate warm label, non-m<N> warm
        // label ("Mac-mini-M4" is never "m4"), parallel array mismatch, duplicate realm+email.
        for bad in [
            "version = 2\n",
            "pool = 1\n",
            "version = 1\n[[account]]\nemail = \"a@example.test\"\nwarm = [\"m1\", \"m2\", \"m3\", \"m4\"]\n",
            "version = 1\n[[account]]\nemail = \"a@example.test\"\nwarm = [\"m3\", \"m3\"]\n",
            "version = 1\n[[account]]\nemail = \"a@example.test\"\nwarm = [\"Mac-mini-M4\"]\n",
            "version = 1\n[[account]]\nemail = \"a@example.test\"\nwarm = [\"m3\"]\nsince = [\"t1\", \"t2\"]\n",
            "version = 1\n[[account]]\nprovider = \"claude\"\nemail = \"a@example.test\"\n[[account]]\nprovider = \"claude\"\nemail = \"A@example.test\"\n",
            "version = 1\n[[account]]\nprovider = \"kimi\"\nemail = \"k@example.test\"\n",
        ] {
            assert!(
                Pool::from_toml(bad, "bad.toml").is_err(),
                "expected refusal for:\n{bad}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn local_pool_identity_reads_swarm_roster_aliases_and_rejects_mac_mini_m4() {
        assert_eq!(leading_m_label("Mac-mini-M4"), None);
        assert_eq!(leading_m_label("m4-air.local"), Some((4, "m4".to_owned())));

        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let home = tmp.path();
        seed_local_swarm_roster(home, "m100", &["m32"]);
        let config = home.join(".config/aterm/aterm.toml");
        fs::create_dir_all(config.parent().unwrap()).unwrap();
        fs::write(&config, "[fleet]\nmachine = \"m999\"\n").unwrap();

        let id = resolve_local_pool_identity(Some(home), Some(&config), None)
            .expect("swarm-roster resolves identity");
        assert_eq!(id.primary, "m100");
        assert_eq!(id.aliases, vec!["m32".to_owned()]);
        assert!(id.matches("m100"));
        assert!(id.matches("m32"));
        assert!(!id.matches("m999"));
        assert!(!id.matches("m4"));
    }

    #[test]
    fn an_account_leased_elsewhere_is_never_picked_and_turn_end_yields() {
        let roster = accounts::Roster {
            rows: vec![
                accounts::Row {
                    label: "pool-a".into(),
                    class: Class::Shared,
                    dir: Some("/tmp/pool-a".into()),
                    email: Some("a@example.test".into()),
                    ..accounts::Row::default()
                },
                accounts::Row {
                    label: "pool-b".into(),
                    class: Class::Shared,
                    dir: Some("/tmp/pool-b".into()),
                    email: Some("b@example.test".into()),
                    ..accounts::Row::default()
                },
                accounts::Row {
                    label: "cold-c".into(),
                    class: Class::Shared,
                    dir: Some("/tmp/cold-c".into()),
                    email: Some("c@example.test".into()),
                    held_by: Some("m9".into()),
                    ..accounts::Row::default()
                },
            ],
        };
        let enrolled: BTreeSet<String> = ["pool-a", "pool-b", "cold-c"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        let mut state = accounts::State {
            swarm_label: Some("m100".into()),
            swarm_aliases: vec!["m32".into()],
            home_accounts: ["pool-a".to_owned()].into_iter().collect(),
            ..accounts::State::default()
        };
        state.note_lease("pool-a", "m9", NOW + 600, NOW);

        // Positive check: leased elsewhere (`m9`) and held elsewhere (`cold-c`) are never picked.
        let cands = accounts::candidates(
            &roster,
            &state,
            &enrolled,
            None,
            &accounts::Point::TurnEnd,
            NOW,
        );
        let verdict_a = cands.iter().find(|(l, _)| l == "pool-a").unwrap().1;
        let verdict_b = cands.iter().find(|(l, _)| l == "pool-b").unwrap().1;
        let verdict_c = cands.iter().find(|(l, _)| l == "cold-c").unwrap().1;
        assert_eq!(
            verdict_a,
            Err(accounts::WhyNot::LeasedElsewhere { until: NOW + 600 })
        );
        assert!(verdict_b.is_ok(), "{verdict_b:?}");
        assert_eq!(verdict_c, Err(accounts::WhyNot::HeldElsewhere));

        let picked = accounts::launch_row(&roster, &state, &enrolled, NOW).unwrap();
        assert_eq!(picked.label, "pool-b");

        // Running session on `pool-a` yields at safe boundary `TurnEnd` under `Mode::Early` (and
        // stays when `Mode::Off` disables rotation).
        let me = accounts::Me {
            account: Some("pool-a"),
            left: None,
            default_home: true,
            stored_login: true,
        };
        assert_eq!(
            accounts::advise(
                &roster,
                &state,
                &enrolled,
                &me,
                &accounts::Point::TurnEnd,
                accounts::Mode::Off,
                NOW,
            ),
            accounts::Advice::Stay(accounts::StayWhy::Off)
        );
        assert_eq!(
            accounts::advise(
                &roster,
                &state,
                &enrolled,
                &me,
                &accounts::Point::TurnEnd,
                accounts::Mode::Early,
                NOW,
            ),
            accounts::Advice::Move {
                to: "pool-b".into(),
                ahead: true,
                rotates: true,
                takes_canary: false,
            }
        );

        // State JSON round-trip preserves leases, swarm_label, swarm_aliases, home_accounts,
        // unrecorded.
        state.unrecorded = true;
        let round =
            accounts::State::from_json(&state.to_json(), NOW).expect("state json round-trips");
        assert_eq!(round, state);

        // Negative control ("ignore the lease"): once the lease expires (or belongs to our alias
        // `m32`), `pool-a` becomes selectable again; while claims are unrecorded, `pool-a` (our
        // home account) is selectable while `pool-b` (not home, unclaimed) is refused with
        // `Unclaimed`.
        let after_expiry = NOW + 601;
        let picked_home = accounts::launch_row(&roster, &state, &enrolled, after_expiry).unwrap();
        assert_eq!(picked_home.label, "pool-a");
        let cands_down = accounts::candidates(
            &roster,
            &state,
            &enrolled,
            None,
            &accounts::Point::TurnEnd,
            after_expiry,
        );
        assert!(
            cands_down
                .iter()
                .find(|(l, _)| l == "pool-a")
                .unwrap()
                .1
                .is_ok()
        );
        assert_eq!(
            cands_down.iter().find(|(l, _)| l == "pool-b").unwrap().1,
            Err(accounts::WhyNot::Unclaimed)
        );

        // Alias ownership: a claim held by our alias `m32` is recognized as ours.
        state.unrecorded = false;
        state.note_lease("pool-a", "m32", after_expiry + 600, after_expiry);
        assert_eq!(state.lease_verdict("pool-a", after_expiry), Ok(()));
    }

    #[cfg(unix)]
    #[test]
    fn sync_from_ai_pool_outranks_sheet_tracks_aliases_and_marks_held_by() {
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let state_dir = tmp.path().join("state");
        let config_dir = home.join(".config/aterm");
        let secrets_dir = home.join(".secrets");
        fs::create_dir_all(&config_dir).unwrap();
        seed_local_swarm_roster(&home, "m100", &["m32"]);
        aterm_types::fs_restricted::ensure_private_dir(&secrets_dir).unwrap();
        fs::create_dir_all(&state_dir).unwrap();
        fs::write(config_dir.join("aterm.toml"), "").unwrap();

        let roster_path = config_dir.join("accounts.toml");
        let codex_roster_path = config_dir.join("accounts-codex.toml");

        // Negative control before `ai-pool.toml` exists: 0.144 `ai-accounts.csv` still syncs.
        fs::write(
            secrets_dir.join(accounts_sync::ACCOUNTS_CSV_FILE),
            "email,computer,claude,codex\ncsv-only@example.test,m100,1,1\n",
        )
        .unwrap();
        fs::write(
            config_dir.join("aterm.toml"),
            "[fleet]\nmachine = \"m100\"\n",
        )
        .unwrap();
        let (m_csv, rows_csv, _, _) =
            accounts_sync::sync_accounts_for_init(&home, &state_dir, &roster_path, NOW)
                .expect("csv fallback syncs");
        assert_eq!((m_csv.as_str(), rows_csv), ("m100", 1));
        assert_eq!(
            accounts::Roster::load(&roster_path).unwrap().rows[0]
                .email
                .as_deref(),
            Some("csv-only@example.test")
        );

        // Now deliver `~/.secrets/ai-pool.toml`: it outranks `ai-accounts.csv`, resolves `m100`
        // + alias `m32` from `swarm-roster.toml`, warms both `a01` (home `m100`) and `a02` (via
        // alias `m32`, home `m9`), retains `csv-only` as `held_by = "pool"`, and syncs Codex too.
        fs::write(config_dir.join("aterm.toml"), "").unwrap();
        let pool_v1 = r#"
version = 1

[last_heard]
m100 = "2026-10-27"

[[account]]
provider = "claude"
email = "a01@example.test"
warm = ["m100", "m14"]

[[account]]
provider = "claude"
email = "a02@example.test"
warm = ["m9", "m32"]

[[account]]
provider = "claude"
email = "a03@example.test"
warm = ["m9"]

[[account]]
provider = "codex"
email = "c01@example.test"
warm = ["m32"]

[[account]]
provider = "codex"
email = "c02@example.test"
warm = ["m9"]
"#;
        fs::write(secrets_dir.join(AI_POOL_FILE), pool_v1).unwrap();
        assert!(accounts_sync::needs_init_accounts_sync(
            &home,
            &state_dir,
            &roster_path,
            NOW + 10
        ));
        assert_eq!(
            accounts_sync::cli_pools_due(
                &config_dir.join("aterm.toml"),
                &state_dir,
                &home,
                NOW + 10
            ),
            vec![Cli::Codex]
        );
        assert!(
            accounts_sync::resync_due(&home, &state_dir, &roster_path, NOW + 10)
                .as_deref()
                .is_some_and(|why| why.contains("codex's pool has accounts to add")),
            "resync_due resolves swarm-roster alias m32 -> m100"
        );

        let resync1 =
            accounts_sync::resync_for_setup(&home, &state_dir, &roster_path, NOW + 10, false);
        assert_eq!(resync1.failed, None);
        assert!(resync1.cli_failed.is_empty(), "{:?}", resync1.cli_failed);
        assert!(resync1.synced);
        assert!(resync1.changed);
        // Only warm unenrolled rows (`a01` and `a02`) are queued for sign-in, never `csv-only` or `a03`.
        assert_eq!(resync1.to_sign_in.len(), 2);

        // Immediately converged: `needs_init_accounts_sync` and `resync_due` are now false/None.
        assert!(!accounts_sync::needs_init_accounts_sync(
            &home,
            &state_dir,
            &roster_path,
            NOW + 11
        ));
        assert_eq!(
            accounts_sync::resync_due(&home, &state_dir, &roster_path, NOW + 11),
            None
        );

        let claude_roster = accounts::Roster::load(&roster_path).unwrap();
        let held_csv = claude_roster
            .rows
            .iter()
            .find(|r| r.email.as_deref() == Some("csv-only@example.test"))
            .expect("previously enrolled row retained");
        assert_eq!(held_csv.held_by.as_deref(), Some("pool"));
        assert!(
            claude_roster
                .rows
                .iter()
                .any(|r| r.email.as_deref() == Some("a01@example.test") && r.held_by.is_none())
        );
        assert!(
            claude_roster
                .rows
                .iter()
                .any(|r| r.email.as_deref() == Some("a02@example.test") && r.held_by.is_none())
        );
        assert!(
            !claude_roster
                .rows
                .iter()
                .any(|r| r.email.as_deref() == Some("a03@example.test"))
        );

        let store = accounts::Store::at(&state_dir);
        let (state, _) = store.load(NOW + 10).unwrap();
        assert_eq!(state.swarm_label.as_deref(), Some("m100"));
        assert_eq!(state.swarm_aliases, vec!["m32".to_owned()]);
        // Only `a01` has `warm[0] == m100`; `a02` has `warm[0] == m9`, so `home_accounts` has 1 row.
        assert_eq!(state.home_accounts.len(), 1);

        let codex_roster = accounts::Roster::parse_for(
            Cli::Codex,
            &fs::read_to_string(&codex_roster_path).unwrap(),
        )
        .unwrap();
        assert!(
            codex_roster
                .rows
                .iter()
                .any(|r| r.email.as_deref() == Some("c01@example.test") && r.held_by.is_none())
        );

        // Reassign `a01` and `c01` away to `m9`: both rows stay in their rosters with
        // `held_by = Some("m9")` and are excluded from `to_sign_in`.
        let pool_v2 = r#"
version = 1

[[account]]
provider = "claude"
email = "a01@example.test"
warm = ["m9"]

[[account]]
provider = "claude"
email = "a02@example.test"
warm = ["m32"]

[[account]]
provider = "codex"
email = "c01@example.test"
warm = ["m9"]
"#;
        fs::write(secrets_dir.join(AI_POOL_FILE), pool_v2).unwrap();
        assert!(accounts_sync::needs_init_accounts_sync(
            &home,
            &state_dir,
            &roster_path,
            NOW + 20
        ));
        let resync2 =
            accounts_sync::resync_for_setup(&home, &state_dir, &roster_path, NOW + 20, false);
        assert_eq!(resync2.failed, None);
        assert!(resync2.cli_failed.is_empty(), "{:?}", resync2.cli_failed);
        assert_eq!(resync2.to_sign_in.len(), 1);

        let claude_after = accounts::Roster::load(&roster_path).unwrap();
        let a01_after = claude_after
            .rows
            .iter()
            .find(|r| r.email.as_deref() == Some("a01@example.test"))
            .expect("a01 retained when moved to m9");
        assert_eq!(a01_after.held_by.as_deref(), Some("m9"));

        let codex_after = accounts::Roster::parse_for(
            Cli::Codex,
            &fs::read_to_string(&codex_roster_path).unwrap(),
        )
        .unwrap();
        let c01_after = codex_after
            .rows
            .iter()
            .find(|r| r.email.as_deref() == Some("c01@example.test"))
            .expect("c01 retained when moved to m9");
        assert_eq!(c01_after.held_by.as_deref(), Some("m9"));
    }
}
