// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Swarm identity, policy, seeding, and newcomer label assignment
//! (`docs/DESIGN-alab-swarm-2026-10-27.md` §3.2, §4.1, §12.3 C11).
//!
//! One canonical `m<N>` label names each Mac across `--host`, `/monitor`, the
//! account pool, fleet registration, and census. The roster lives in the
//! reserved `[everyone]` vault entry [`ROSTER_ENTRY`] (`aterm-swarm-roster`),
//! while steward policy and reserved labels live in [`POLICY_ENTRY`]
//! (`aterm-swarm-policy`).

use crate::text::{Audience, Document};
use crate::{Error, Result};
use aterm_toml::{Table, Value};
use std::collections::{BTreeMap, BTreeSet};

/// Reserved `[everyone]` vault entry carrying member labels, aliases, hints, and deny-lists.
pub const ROSTER_ENTRY: &str = "aterm-swarm-roster";

/// Reserved admin vault entry carrying steward policy, network id, and reserved labels.
pub const POLICY_ENTRY: &str = "aterm-swarm-policy";

/// Reserved admin vault entry carrying the active steward lease (`holder`, `epoch`, `until`).
pub const LEASE_ENTRY: &str = "aterm-swarm-lease";

/// Hold window (24 h) before an unclaimed sheet-named `m<N>` label promotes to a newcomer.
pub const SHEET_CLAIM_HOLD_MS: u64 = 24 * 60 * 60 * 1_000;

/// Legacy sheet rename offset (`N` paired with `N - 68`, `DESIGN-alab-swarm` §2.1 F6).
pub const RENAME_PAIR_OFFSET: u32 = 68;

/// Default release Mac label that policy always reserves (`DESIGN-alab-swarm` §3.2).
pub const DEFAULT_RESERVED_RELEASE_LABEL: &str = "m3";

/// Extract a canonical `(N, "m<N>")` label ONLY when `name` starts with `m<N>` (`N >= 1`).
///
/// Mid-string tokens are never accepted: `"Mac-mini-M4"` and `"macbook-m3"` return `None`,
/// as do alphanumeric suffixes like `"m3pro"` or zero-numbered `"m0"`. Host suffixes such
/// as `"M22.local"`, `"m100-studio"`, or `"m4 (2)"` return `Some((N, "m<N>"))`.
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

/// One Mac's row in [`SwarmRoster`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwarmMember {
    /// Age X25519 recipient (`age1…`).
    pub recipient: String,
    /// Machine name recorded in `[machines]`.
    pub vault_name: String,
    /// Active canonical `m<N>` label.
    pub label: String,
    /// Sorted, deduplicated legacy `m<N>` aliases (excluding `label`).
    pub aliases: Vec<String>,
    /// Optional signer-salted hardware hint (`SHA-256(signer_fingerprint || IOPlatformUUID)`).
    pub hint: Option<String>,
    /// Admission timestamp in Unix milliseconds (`0` when unrecorded).
    pub admitted_ms: u64,
    /// Sheet-named `m<N>` currently under a 24 h provisional hold, if any.
    pub claimed_label: Option<String>,
    /// Unix milliseconds when [`Self::claimed_label`] began its 24 h hold.
    pub claimed_at_ms: Option<u64>,
    /// Optional fleet node identifier (`node_id`).
    pub node_id: Option<String>,
    /// Optional bound fleet node public key (`0.147+`).
    pub node_key: Option<String>,
}

impl SwarmMember {
    /// Normalize `aliases` so they are canonical `m<N>` strings, sorted, deduplicated,
    /// and never duplicate `self.label`.
    pub fn normalize_aliases(&mut self) {
        let mut clean = BTreeSet::new();
        for alias in &self.aliases {
            if let Some((_, canonical)) = leading_m_label(alias)
                && canonical != self.label
            {
                clean.insert(canonical);
            }
        }
        self.aliases = clean.into_iter().collect();
    }

    /// True if `candidate` matches `self.label` or any of `self.aliases` (case-insensitively).
    #[must_use]
    pub fn matches_label_or_alias(&self, candidate: &str) -> bool {
        let Some((_, canonical)) = leading_m_label(candidate) else {
            return false;
        };
        if self.label == canonical {
            return true;
        }
        self.aliases.iter().any(|alias| alias == &canonical)
    }
}

/// Parsed `aterm-swarm-roster` document.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SwarmRoster {
    /// Admitted swarm members, ordered by canonical label number then recipient.
    pub members: Vec<SwarmMember>,
    /// Grow-only deny-list of permanently revoked `m<N>` labels (e.g. stolen Macs).
    pub deny_labels: Vec<String>,
    /// Grow-only deny-list of revoked fleet node identifiers.
    pub deny_nodes: Vec<String>,
}

impl SwarmRoster {
    /// Parse `aterm-swarm-roster` TOML text.
    pub fn from_toml(text: &str) -> Result<Self> {
        let table: Table = aterm_toml::from_str(text)
            .map_err(|e| Error::new(format!("invalid {ROSTER_ENTRY}: {e}")))?;
        let deny_labels = normalize_label_set(string_array(&table, "deny_labels")?);
        let deny_nodes = normalize_word_set(string_array(&table, "deny_nodes")?);
        let mut members = Vec::new();
        if let Some(raw_members) = table.get("member") {
            let rows = raw_members
                .as_array()
                .ok_or_else(|| Error::new(format!("{ROSTER_ENTRY}: `member` must be an array")))?;
            let mut seen_recipients = BTreeSet::new();
            for (idx, item) in rows.iter().enumerate() {
                let row = item.as_table().ok_or_else(|| {
                    Error::new(format!("{ROSTER_ENTRY} member {}: expected table", idx + 1))
                })?;
                let recipient = required_str(row, "recipient", idx)?;
                if !seen_recipients.insert(recipient.clone()) {
                    return Err(Error::new(format!(
                        "{ROSTER_ENTRY} member {}: duplicate recipient",
                        idx + 1
                    )));
                }
                let vault_name = required_str(row, "vault_name", idx)?;
                let raw_label = required_str(row, "label", idx)?;
                let (_, label) = leading_m_label(&raw_label).ok_or_else(|| {
                    Error::new(format!(
                        "{ROSTER_ENTRY} member {}: invalid m<N> label `{raw_label}`",
                        idx + 1
                    ))
                })?;
                let aliases = string_array(row, "aliases")?;
                let hint = optional_str(row, "hint");
                let admitted_ms = optional_u64(row, "admitted_ms").unwrap_or(0);
                let claimed_label = match optional_str(row, "claimed_label") {
                    Some(raw) => Some(
                        leading_m_label(&raw)
                            .ok_or_else(|| {
                                Error::new(format!(
                                    "{ROSTER_ENTRY} member {}: invalid claimed_label `{raw}`",
                                    idx + 1
                                ))
                            })?
                            .1,
                    ),
                    None => None,
                };
                let claimed_at_ms = optional_u64(row, "claimed_at_ms");
                let node_id = optional_str(row, "node_id");
                let node_key = optional_str(row, "node_key");
                let mut member = SwarmMember {
                    recipient,
                    vault_name,
                    label,
                    aliases,
                    hint,
                    admitted_ms,
                    claimed_label,
                    claimed_at_ms,
                    node_id,
                    node_key,
                };
                member.normalize_aliases();
                members.push(member);
            }
        }
        let mut roster = Self {
            members,
            deny_labels,
            deny_nodes,
        };
        roster.canonicalize();
        Ok(roster)
    }

    /// Read `aterm-swarm-roster` from `[everyone]` in `doc`, if present.
    pub fn from_document(doc: &Document) -> Result<Option<Self>> {
        let Some(entry) = doc.entry(&Audience::Everyone, ROSTER_ENTRY) else {
            return Ok(None);
        };
        let text = std::str::from_utf8(entry.value.as_slice())
            .map_err(|_| Error::new(format!("{ROSTER_ENTRY} is not UTF-8")))?;
        Self::from_toml(text).map(Some)
    }

    /// Canonicalize member ordering, alias sets, and grow-only deny sets.
    pub fn canonicalize(&mut self) {
        for member in &mut self.members {
            member.normalize_aliases();
        }
        self.members.sort_by(|a, b| {
            let na = leading_m_label(&a.label).map_or(u32::MAX, |(n, _)| n);
            let nb = leading_m_label(&b.label).map_or(u32::MAX, |(n, _)| n);
            (na, &a.recipient).cmp(&(nb, &b.recipient))
        });
        self.deny_labels = normalize_label_set(std::mem::take(&mut self.deny_labels));
        self.deny_nodes = normalize_word_set(std::mem::take(&mut self.deny_nodes));
    }

    /// Merge `prior`'s grow-only deny-lists into `self` so neither list ever shrinks.
    pub fn merge_deny_lists(&mut self, prior: &Self) {
        self.deny_labels.extend(prior.deny_labels.iter().cloned());
        self.deny_nodes.extend(prior.deny_nodes.iter().cloned());
        self.deny_labels = normalize_label_set(std::mem::take(&mut self.deny_labels));
        self.deny_nodes = normalize_word_set(std::mem::take(&mut self.deny_nodes));
    }

    /// Render canonical TOML text for `aterm-swarm-roster`.
    #[must_use]
    pub fn to_toml(&self) -> String {
        let mut clone = self.clone();
        clone.canonicalize();
        let mut root = Table::new();
        root.insert(
            "deny_labels".to_owned(),
            Value::Array(clone.deny_labels.into_iter().map(Value::String).collect()),
        );
        root.insert(
            "deny_nodes".to_owned(),
            Value::Array(clone.deny_nodes.into_iter().map(Value::String).collect()),
        );
        let rows: Vec<Value> = clone
            .members
            .into_iter()
            .map(|m| {
                let mut t = Table::new();
                t.insert("recipient".to_owned(), Value::String(m.recipient));
                t.insert("vault_name".to_owned(), Value::String(m.vault_name));
                t.insert("label".to_owned(), Value::String(m.label));
                if !m.aliases.is_empty() {
                    t.insert(
                        "aliases".to_owned(),
                        Value::Array(m.aliases.into_iter().map(Value::String).collect()),
                    );
                }
                if let Some(hint) = m.hint {
                    t.insert("hint".to_owned(), Value::String(hint));
                }
                if m.admitted_ms > 0
                    && let Ok(ms) = i64::try_from(m.admitted_ms)
                {
                    t.insert("admitted_ms".to_owned(), Value::Integer(ms));
                }
                if let Some(claimed) = m.claimed_label {
                    t.insert("claimed_label".to_owned(), Value::String(claimed));
                }
                if let Some(at) = m.claimed_at_ms
                    && let Ok(ms) = i64::try_from(at)
                {
                    t.insert("claimed_at_ms".to_owned(), Value::Integer(ms));
                }
                if let Some(node_id) = m.node_id {
                    t.insert("node_id".to_owned(), Value::String(node_id));
                }
                if let Some(node_key) = m.node_key {
                    t.insert("node_key".to_owned(), Value::String(node_key));
                }
                Value::Table(t)
            })
            .collect();
        if !rows.is_empty() {
            root.insert("member".to_owned(), Value::Array(rows));
        }
        aterm_toml::to_string(&Value::Table(root)).unwrap_or_default()
    }

    /// Find the roster row for `recipient`.
    #[must_use]
    pub fn member_by_recipient(&self, recipient: &str) -> Option<&SwarmMember> {
        self.members.iter().find(|m| m.recipient == recipient)
    }

    /// Find the roster row matching `label` or one of its `aliases`.
    #[must_use]
    pub fn member_by_label_or_alias(&self, label: &str) -> Option<&SwarmMember> {
        self.members
            .iter()
            .find(|m| m.matches_label_or_alias(label))
    }

    /// True when `node_id` appears on the grow-only `deny_nodes` list.
    #[must_use]
    pub fn is_node_denied(&self, node_id: &str) -> bool {
        let needle = node_id.trim_ascii().to_ascii_lowercase();
        !needle.is_empty() && self.deny_nodes.iter().any(|n| n == &needle)
    }

    /// True when `label` appears on the grow-only `deny_labels` list.
    #[must_use]
    pub fn is_label_denied(&self, label: &str) -> bool {
        leading_m_label(label)
            .is_some_and(|(_, canonical)| self.deny_labels.iter().any(|l| l == &canonical))
    }
}

/// One steward's policy record inside [`SwarmPolicy`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StewardPolicy {
    /// Canonical `m<N>` label of this steward.
    pub label: String,
    /// Lease preference rank (lower rank wins when healthy).
    pub rank: u32,
    /// Whether this steward passed the §8.2 always-on check.
    pub always_on: bool,
}

/// Parsed `aterm-swarm-policy` document.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SwarmPolicy {
    /// Recorded ALab network identifier, if configured.
    pub network_id: Option<String>,
    /// Explicit reserved `m<N>` labels (the approver never assigns these to newcomers).
    pub reserved_labels: Vec<String>,
    /// Labels of release hosts (e.g. `m3`, `m22`).
    pub release_hosts: Vec<String>,
    /// Vault server URLs recorded in policy.
    pub servers: Vec<String>,
    /// Steward ranks and `always_on` marks.
    pub stewards: Vec<StewardPolicy>,
}

impl SwarmPolicy {
    /// Parse `aterm-swarm-policy` TOML text.
    pub fn from_toml(text: &str) -> Result<Self> {
        let table: Table = aterm_toml::from_str(text)
            .map_err(|e| Error::new(format!("invalid {POLICY_ENTRY}: {e}")))?;
        let network_id = optional_str(&table, "network_id");
        let reserved_labels = normalize_label_set(string_array(&table, "reserved_labels")?);
        let release_hosts = normalize_label_set(string_array(&table, "release_hosts")?);
        let servers = string_array(&table, "servers")?;
        let mut stewards = Vec::new();
        if let Some(raw_stewards) = table.get("steward") {
            let rows = raw_stewards
                .as_array()
                .ok_or_else(|| Error::new(format!("{POLICY_ENTRY}: `steward` must be an array")))?;
            for (idx, item) in rows.iter().enumerate() {
                let row = item.as_table().ok_or_else(|| {
                    Error::new(format!(
                        "{POLICY_ENTRY} steward {}: expected table",
                        idx + 1
                    ))
                })?;
                let raw_label = required_str(row, "label", idx)?;
                let (_, label) = leading_m_label(&raw_label).ok_or_else(|| {
                    Error::new(format!(
                        "{POLICY_ENTRY} steward {}: invalid m<N> label `{raw_label}`",
                        idx + 1
                    ))
                })?;
                let rank = optional_u64(row, "rank")
                    .and_then(|v| u32::try_from(v).ok())
                    .unwrap_or(idx as u32 + 1);
                let always_on = row
                    .get("always_on")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                stewards.push(StewardPolicy {
                    label,
                    rank,
                    always_on,
                });
            }
        }
        Ok(Self {
            network_id,
            reserved_labels,
            release_hosts,
            servers,
            stewards,
        })
    }

    /// Read `aterm-swarm-policy` from `doc` (any audience) if present; otherwise return default.
    pub fn from_document(doc: &Document) -> Result<Option<Self>> {
        let Some(entry) = doc
            .entries()
            .into_iter()
            .find(|entry| entry.name == POLICY_ENTRY)
        else {
            return Ok(None);
        };
        let text = std::str::from_utf8(entry.value.as_slice())
            .map_err(|_| Error::new(format!("{POLICY_ENTRY} is not UTF-8")))?;
        Self::from_toml(text).map(Some)
    }

    /// Render canonical TOML text for `aterm-swarm-policy`.
    #[must_use]
    pub fn to_toml(&self) -> String {
        let mut root = Table::new();
        if let Some(net) = &self.network_id {
            root.insert("network_id".to_owned(), Value::String(net.clone()));
        }
        root.insert(
            "reserved_labels".to_owned(),
            Value::Array(
                normalize_label_set(self.reserved_labels.clone())
                    .into_iter()
                    .map(Value::String)
                    .collect(),
            ),
        );
        if !self.release_hosts.is_empty() {
            root.insert(
                "release_hosts".to_owned(),
                Value::Array(
                    normalize_label_set(self.release_hosts.clone())
                        .into_iter()
                        .map(Value::String)
                        .collect(),
                ),
            );
        }
        if !self.servers.is_empty() {
            root.insert(
                "servers".to_owned(),
                Value::Array(self.servers.iter().cloned().map(Value::String).collect()),
            );
        }
        if !self.stewards.is_empty() {
            let rows: Vec<Value> = self
                .stewards
                .iter()
                .map(|s| {
                    let mut t = Table::new();
                    t.insert("label".to_owned(), Value::String(s.label.clone()));
                    t.insert("rank".to_owned(), Value::Integer(i64::from(s.rank)));
                    t.insert("always_on".to_owned(), Value::Boolean(s.always_on));
                    Value::Table(t)
                })
                .collect();
            root.insert("steward".to_owned(), Value::Array(rows));
        }
        aterm_toml::to_string(&Value::Table(root)).unwrap_or_default()
    }

    /// Compute the full set of reserved `m<N>` labels: always `m3` (the release Mac),
    /// every policy `reserved_labels` / `release_hosts` / `stewards` label, and every
    /// vault admin's `m<N>` label (`DESIGN-alab-swarm` §3.2).
    #[must_use]
    pub fn effective_reserved(
        &self,
        doc: &Document,
        existing: Option<&SwarmRoster>,
    ) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        out.insert(DEFAULT_RESERVED_RELEASE_LABEL.to_owned());
        for raw in self
            .reserved_labels
            .iter()
            .chain(self.release_hosts.iter())
            .chain(self.stewards.iter().map(|s| &s.label))
        {
            if let Some((_, canonical)) = leading_m_label(raw) {
                out.insert(canonical);
            }
        }
        let machines = doc.machines();
        for admin_name in doc.admins() {
            if let Some((_, canonical)) = leading_m_label(&admin_name) {
                out.insert(canonical);
            }
            if let Some(roster) = existing
                && let Some(machine) = machines.iter().find(|m| m.name == admin_name)
                && let Some(member) = roster.member_by_recipient(&machine.recipient)
            {
                out.insert(member.label.clone());
            }
        }
        out
    }

    /// True when `label` is marked `always_on = true` in `self.stewards` (§8.1).
    #[must_use]
    pub fn is_always_on_steward(&self, label: &str) -> bool {
        let Some((_, canonical)) = leading_m_label(label) else {
            return false;
        };
        self.stewards
            .iter()
            .any(|s| s.always_on && s.label == canonical)
    }

    /// Ring distance (`0` for the immediate next-ranked `always_on` steward, `1` for the one
    /// after it, …) from `previous_holder` to `candidate` across `always_on` stewards sorted by
    /// `(rank, label)` (`DESIGN-alab-swarm` §3.4).
    #[must_use]
    pub fn takeover_rank_steps(&self, previous_holder: &str, candidate: &str) -> u64 {
        let Some((_, cand)) = leading_m_label(candidate) else {
            return 0;
        };
        let mut ring: Vec<&StewardPolicy> = self.stewards.iter().filter(|s| s.always_on).collect();
        ring.sort_by(|a, b| a.rank.cmp(&b.rank).then_with(|| a.label.cmp(&b.label)));
        ring.dedup_by(|a, b| a.label == b.label);
        if ring.len() <= 1 {
            return 0;
        }
        let Some(cand_idx) = ring.iter().position(|s| s.label == cand) else {
            return 0;
        };
        let prev = leading_m_label(previous_holder).map(|(_, l)| l);
        if let Some(prev_label) = prev
            && let Some(prev_idx) = ring.iter().position(|s| s.label == prev_label)
        {
            let next_idx = (prev_idx + 1) % ring.len();
            let steps = (cand_idx + ring.len() - next_idx) % ring.len();
            return u64::try_from(steps).unwrap_or(0);
        }
        u64::try_from(cand_idx).unwrap_or(0)
    }

    /// True when `observed_network_id` matches the ALab network policy (`Some(expected)` when
    /// `self.network_id` is pinned, or any confirmed ALab vault network route set when unpinned).
    #[must_use]
    pub fn on_alab_network(&self, observed_network_id: Option<&str>) -> bool {
        let observed = observed_network_id.map(str::trim).filter(|s| !s.is_empty());
        match self
            .network_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(expected) => observed == Some(expected),
            None => observed.is_some(),
        }
    }
}

/// Steward lease TTL (6 hours, `DESIGN-alab-swarm` §3.1, §12.3 C7).
pub const LEASE_TTL_SECS: u64 = 6 * 60 * 60;

/// Remaining-time threshold (< 3 hours) at which the current holder renews its lease.
pub const LEASE_RENEW_REMAINING_SECS: u64 = 3 * 60 * 60;

/// Clock-skew safety margin (5 minutes) past `until` before a standby steward takes over.
pub const LEASE_CLOCK_MARGIN_SECS: u64 = 5 * 60;

/// Additional rank stagger (60 seconds per rank step after the next-ranked `always_on` steward)
/// so the next-ranked steward takes over first while lower-ranked stewards still step in if it is
/// also unreachable (`DESIGN-alab-swarm` §3.4).
pub const LEASE_RANK_STEP_SECS: u64 = 60;

/// Parsed `aterm-swarm-lease` entry (`holder`, `epoch`, `until` in Unix seconds).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwarmLease {
    /// Canonical `m<N>` label of the steward holding this lease epoch.
    pub holder: String,
    /// Monotone lease epoch (`>= 1`).
    pub epoch: u64,
    /// Unix timestamp (seconds) when this lease expires (`0` if unrecorded in legacy fixtures).
    pub until: u64,
}

impl SwarmLease {
    /// Parse `aterm-swarm-lease` TOML text.
    pub fn from_toml(text: &str) -> Result<Self> {
        let table: Table = aterm_toml::from_str(text)
            .map_err(|e| Error::new(format!("invalid {LEASE_ENTRY}: {e}")))?;
        let raw_holder = table
            .get("holder")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| Error::new(format!("{LEASE_ENTRY}: missing `holder`")))?;
        let holder = leading_m_label(raw_holder)
            .map(|(_, canonical)| canonical)
            .unwrap_or_else(|| raw_holder.to_ascii_lowercase());
        let epoch = optional_u64(&table, "epoch")
            .filter(|&e| e >= 1)
            .ok_or_else(|| Error::new(format!("{LEASE_ENTRY}: `epoch` must be >= 1")))?;
        let until = optional_u64(&table, "until").unwrap_or(0);
        Ok(Self {
            holder,
            epoch,
            until,
        })
    }

    /// Read `aterm-swarm-lease` from `doc` (any audience) if present.
    pub fn from_document(doc: &Document) -> Result<Option<Self>> {
        let Some(entry) = doc
            .entries()
            .into_iter()
            .find(|entry| entry.name == LEASE_ENTRY)
        else {
            return Ok(None);
        };
        let text = std::str::from_utf8(entry.value.as_slice())
            .map_err(|_| Error::new(format!("{LEASE_ENTRY} is not UTF-8")))?;
        Self::from_toml(text).map(Some)
    }

    /// Render canonical TOML text for `aterm-swarm-lease`.
    #[must_use]
    pub fn to_toml(&self) -> String {
        let mut root = Table::new();
        root.insert("holder".to_owned(), Value::String(self.holder.clone()));
        root.insert(
            "epoch".to_owned(),
            Value::Integer(i64::try_from(self.epoch).unwrap_or(i64::MAX)),
        );
        root.insert(
            "until".to_owned(),
            Value::Integer(i64::try_from(self.until).unwrap_or(i64::MAX)),
        );
        aterm_toml::to_string(&Value::Table(root)).unwrap_or_default()
    }
}

/// Steward lease evaluation outcome (`sync.steward.lease`, `DESIGN-alab-swarm` §3.1, §8.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaseDecision {
    /// This Mac is not an `always_on` steward or is not on the ALab network.
    NotEligible(String),
    /// Existing lease stands with no publish needed (`>= 3 h` left for holder, or unexpired for standby).
    Current(SwarmLease),
    /// No lease exists yet: claim epoch 1.
    Claim(SwarmLease),
    /// This Mac holds the lease and `< 3 h` remains: renew at the same epoch.
    Renew(SwarmLease),
    /// Another steward's lease expired past `until + 5 min`: take over at `epoch + 1`.
    Takeover(SwarmLease),
}

/// Evaluate whether `own_label` should claim, renew, take over, or leave `current` as-is.
#[must_use]
pub fn decide_lease_action(
    policy: &SwarmPolicy,
    own_label: &str,
    observed_network_id: Option<&str>,
    current: Option<&SwarmLease>,
    now_secs: u64,
) -> LeaseDecision {
    let Some((_, canonical)) = leading_m_label(own_label) else {
        return LeaseDecision::NotEligible(format!(
            "`{own_label}` is not a canonical m<N> steward label"
        ));
    };
    if !policy.is_always_on_steward(&canonical) {
        return LeaseDecision::NotEligible(format!(
            "{canonical} is not marked `always_on = true` in {POLICY_ENTRY}"
        ));
    }
    if !policy.on_alab_network(observed_network_id) {
        return LeaseDecision::NotEligible(format!(
            "{canonical} is not on the recorded ALab network"
        ));
    }
    let next_until = now_secs.saturating_add(LEASE_TTL_SECS);
    let Some(lease) = current else {
        return LeaseDecision::Claim(SwarmLease {
            holder: canonical,
            epoch: 1,
            until: next_until,
        });
    };
    if lease.holder.eq_ignore_ascii_case(&canonical) {
        if now_secs.saturating_add(LEASE_RENEW_REMAINING_SECS) > lease.until {
            return LeaseDecision::Renew(SwarmLease {
                holder: canonical,
                epoch: lease.epoch,
                until: next_until,
            });
        }
        return LeaseDecision::Current(lease.clone());
    }
    let rank_steps = policy.takeover_rank_steps(&lease.holder, &canonical);
    let margin =
        LEASE_CLOCK_MARGIN_SECS.saturating_add(rank_steps.saturating_mul(LEASE_RANK_STEP_SECS));
    if now_secs >= lease.until.saturating_add(margin) {
        return LeaseDecision::Takeover(SwarmLease {
            holder: canonical,
            epoch: lease.epoch.saturating_add(1),
            until: next_until,
        });
    }
    LeaseDecision::Current(lease.clone())
}

/// Sheet `m<N>` observations extracted from `fleet-accounts.toml` (and per-CLI variants).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SheetHints {
    /// Every `m<N>` number referenced anywhere in the sheet.
    pub numbers: BTreeSet<u32>,
    /// Each account row's list of `m<N>` numbers (used to fold F6 `N` / `N - 68` rename pairs).
    pub shared_rows: Vec<Vec<u32>>,
}

impl SheetHints {
    /// Parse `fleet-accounts.toml` text into `SheetHints`.
    #[must_use]
    pub fn from_fleet_toml(text: &str) -> Self {
        let mut out = Self::default();
        out.ingest_fleet_toml(text);
        out
    }

    /// Ingest another `fleet-accounts*.toml` payload into `self`.
    pub fn ingest_fleet_toml(&mut self, text: &str) {
        let Ok(table) = aterm_toml::from_str::<Table>(text) else {
            return;
        };
        let Some(rows) = table.get("account").and_then(Value::as_array) else {
            return;
        };
        for item in rows {
            let Some(machines) = item
                .as_table()
                .and_then(|row| row.get("machines"))
                .and_then(Value::as_array)
            else {
                continue;
            };
            let mut row_nums = Vec::new();
            for cell in machines {
                if let Some(word) = cell.as_str()
                    && let Some((n, _)) = leading_m_label(word)
                {
                    self.numbers.insert(n);
                    if !row_nums.contains(&n) {
                        row_nums.push(n);
                    }
                }
            }
            if !row_nums.is_empty() {
                self.shared_rows.push(row_nums);
            }
        }
    }

    /// Scan `doc` for `fleet-accounts.toml` and `fleet-accounts-*.toml` entries.
    #[must_use]
    pub fn from_document(doc: &Document) -> Self {
        let mut out = Self::default();
        for entry in doc.entries() {
            let base = entry
                .name
                .strip_prefix("~/.secrets/")
                .unwrap_or(&entry.name);
            let is_fleet_sheet = base == "fleet-accounts.toml"
                || (base.starts_with("fleet-accounts-") && base.ends_with(".toml"));
            if is_fleet_sheet && let Ok(text) = std::str::from_utf8(entry.value.as_slice()) {
                out.ingest_fleet_toml(text);
            }
        }
        out
    }

    /// Return folded F6 legacy alias (`m<N - 68>`) for `n` when the sheet pairs `n` and `n - 68`
    /// and no live member holds `m<N - 68>`.
    #[must_use]
    pub fn paired_legacy_aliases(
        &self,
        n: u32,
        live_numbers: &BTreeSet<u32>,
        reserved: &BTreeSet<String>,
    ) -> Vec<String> {
        if n <= RENAME_PAIR_OFFSET {
            return Vec::new();
        }
        let old = n - RENAME_PAIR_OFFSET;
        let old_label = format!("m{old}");
        if live_numbers.contains(&old) || reserved.contains(&old_label) {
            return Vec::new();
        }
        let paired = if self.shared_rows.is_empty() {
            self.numbers.contains(&n) && self.numbers.contains(&old)
        } else {
            self.shared_rows
                .iter()
                .any(|row| row.contains(&n) && row.contains(&old))
        };
        if paired { vec![old_label] } else { Vec::new() }
    }
}

/// Compute the next free `m<N>` number strictly above every `m<N>` seen in `sheet`,
/// `roster` (labels, aliases, pending claims, deny-list), `reserved`, `doc.machines()`,
/// and `bonjour_hosts`.
#[must_use]
pub fn next_free_number(
    roster: &SwarmRoster,
    doc: &Document,
    reserved: &BTreeSet<String>,
    sheet: &SheetHints,
    bonjour_hosts: &[String],
) -> u32 {
    let mut max_seen = 0_u32;
    let mut observe = |n: u32| {
        if n > max_seen {
            max_seen = n;
        }
    };
    for &n in &sheet.numbers {
        observe(n);
    }
    for member in &roster.members {
        if let Some((n, _)) = leading_m_label(&member.label) {
            observe(n);
        }
        for alias in &member.aliases {
            if let Some((n, _)) = leading_m_label(alias) {
                observe(n);
            }
        }
        if let Some(claimed) = &member.claimed_label
            && let Some((n, _)) = leading_m_label(claimed)
        {
            observe(n);
        }
    }
    for denied in &roster.deny_labels {
        if let Some((n, _)) = leading_m_label(denied) {
            observe(n);
        }
    }
    for res in reserved {
        if let Some((n, _)) = leading_m_label(res) {
            observe(n);
        }
    }
    for machine in doc.machines() {
        if let Some((n, _)) = leading_m_label(&machine.name) {
            observe(n);
        }
    }
    for host in bonjour_hosts {
        if let Some((n, _)) = leading_m_label(host) {
            observe(n);
        }
    }
    max_seen.saturating_add(1).max(1)
}

/// Admission timestamp and vault sequence used to order seeding (`DESIGN-alab-swarm` §3.2).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SeedAdmission {
    /// Author timestamp in Unix ms (`0` if unrecorded).
    pub admitted_ms: u64,
    /// Vault sequence where admitted (`0` if unrecorded).
    pub sequence: u64,
}

/// Seed `aterm-swarm-roster` once from `doc.machines()` in admission-date order (§3.2).
///
/// - Each member keeps the `m<N>` its `host_overrides` (LocalHostName/ComputerName) or
///   `[machines]` name **starts with**; a later admission never takes an earlier member's `m<N>`.
/// - On collision between two members whose names start with the same `m<N>`, the earlier
///   admission keeps `m<N>` and the later one gets `next_free` with the old `m<N>` as an alias.
/// - Non-admins never take a reserved label (`m3` or another steward's label) unless explicitly
///   named in `policy.release_hosts` under that `[machines]` name.
/// - Folds F6 `N` / `N - 68` rename pairs from `sheet` into `aliases`.
#[must_use]
pub fn seed_roster(
    doc: &Document,
    admissions: &BTreeMap<String, SeedAdmission>,
    policy: &SwarmPolicy,
    sheet: &SheetHints,
    bonjour_hosts: &[String],
    host_overrides: &BTreeMap<String, String>,
) -> SwarmRoster {
    let admins: BTreeSet<String> = doc.admins().into_iter().collect();
    let reserved = policy.effective_reserved(doc, None);
    let mut indexed: Vec<(usize, crate::text::Machine, SeedAdmission)> = doc
        .machines()
        .into_iter()
        .enumerate()
        .map(|(idx, m)| {
            let adm = admissions.get(&m.recipient).copied().unwrap_or_default();
            (idx, m, adm)
        })
        .collect();
    indexed.sort_by_key(|(idx, _, adm)| (adm.admitted_ms, adm.sequence, *idx));

    let mut roster = SwarmRoster::default();
    let mut taken_labels = BTreeSet::new();

    for (_, machine, adm) in indexed {
        let source_name = host_overrides
            .get(&machine.recipient)
            .map_or(machine.name.as_str(), String::as_str);
        let is_admin = admins.contains(&machine.name);
        let is_release_host = leading_m_label(&machine.name).is_some_and(|(_, lbl)| {
            machine.name.eq_ignore_ascii_case(&lbl)
                && policy.release_hosts.iter().any(|r| r == &lbl)
        });
        let candidate = leading_m_label(source_name);
        let mut aliases = Vec::new();
        let label = match candidate {
            Some((_, ref lbl))
                if !taken_labels.contains(lbl)
                    && (is_admin || is_release_host || !reserved.contains(lbl)) =>
            {
                lbl.clone()
            }
            Some((_, lbl)) => {
                if !reserved.contains(&lbl) {
                    aliases.push(lbl);
                }
                let next = next_free_number(&roster, doc, &reserved, sheet, bonjour_hosts);
                format!("m{next}")
            }
            None => {
                let next = next_free_number(&roster, doc, &reserved, sheet, bonjour_hosts);
                format!("m{next}")
            }
        };
        taken_labels.insert(label.clone());
        let mut member = SwarmMember {
            hint: None,
            recipient: machine.recipient,
            vault_name: machine.name,
            label,
            aliases,
            admitted_ms: adm.admitted_ms,
            claimed_label: None,
            claimed_at_ms: None,
            node_id: None,
            node_key: None,
        };
        member.normalize_aliases();
        roster.members.push(member);
    }

    let live_numbers: BTreeSet<u32> = roster
        .members
        .iter()
        .filter_map(|m| leading_m_label(&m.label).map(|(n, _)| n))
        .collect();
    let alias_owners: BTreeSet<String> = roster
        .members
        .iter()
        .flat_map(|m| m.aliases.iter().cloned())
        .collect();
    for member in &mut roster.members {
        if let Some((n, _)) = leading_m_label(&member.label) {
            for folded in sheet.paired_legacy_aliases(n, &live_numbers, &reserved) {
                if !alias_owners.contains(&folded) {
                    member.aliases.push(folded);
                }
            }
            member.normalize_aliases();
        }
    }
    roster.canonicalize();
    roster
}

/// Promote any 24 h sheet-label claims whose hold has elapsed without a competing claimant.
pub fn promote_due_claims(
    roster: &mut SwarmRoster,
    doc: &Document,
    policy: &SwarmPolicy,
    sheet: &SheetHints,
    now_ms: u64,
) -> bool {
    let reserved = policy.effective_reserved(doc, Some(roster));
    let mut claim_counts: BTreeMap<String, usize> = BTreeMap::new();
    for m in &roster.members {
        if let Some(claim) = &m.claimed_label {
            *claim_counts.entry(claim.clone()).or_insert(0) += 1;
        }
    }
    let mut held_labels: BTreeSet<String> = roster
        .members
        .iter()
        .flat_map(|m| std::iter::once(m.label.clone()).chain(m.aliases.iter().cloned()))
        .collect();
    let deny_labels: BTreeSet<String> = roster.deny_labels.iter().cloned().collect();

    let mut changed = false;
    for member in &mut roster.members {
        let (Some(target), Some(since)) = (member.claimed_label.clone(), member.claimed_at_ms)
        else {
            continue;
        };
        let unique_claimant = claim_counts.get(&target).copied() == Some(1);
        let free = !held_labels.contains(&target)
            && !reserved.contains(&target)
            && !deny_labels.contains(&target);
        if unique_claimant && free && now_ms.saturating_sub(since) >= SHEET_CLAIM_HOLD_MS {
            let provisional = std::mem::replace(&mut member.label, target.clone());
            member.aliases.push(provisional);
            member.claimed_label = None;
            member.claimed_at_ms = None;
            member.normalize_aliases();
            held_labels.insert(target);
            changed = true;
        }
    }
    if changed {
        let live_numbers: BTreeSet<u32> = roster
            .members
            .iter()
            .filter_map(|m| leading_m_label(&m.label).map(|(n, _)| n))
            .collect();
        for member in &mut roster.members {
            if let Some((n, _)) = leading_m_label(&member.label) {
                for folded in sheet.paired_legacy_aliases(n, &live_numbers, &reserved) {
                    member.aliases.push(folded);
                }
                member.normalize_aliases();
            }
        }
        roster.canonicalize();
    }
    changed
}

/// Parameters for assigning a newcomer's `m<N>` label in the approver.
pub struct Newcomer<'a> {
    /// Newcomer's `age1…` recipient.
    pub recipient: &'a str,
    /// Host name the newcomer asked for (`request.host`).
    pub requested_host: &'a str,
    /// Machine name assigned in `[machines]` (`free_name`).
    pub vault_name: &'a str,
    /// Unix time in milliseconds of this approval.
    pub now_ms: u64,
}

/// Assign a canonical `m<N>` label for `newcomer` in `roster` (`DESIGN-alab-swarm` §3.2):
///
/// 1. Promotes any existing 24 h sheet claims that are now due.
/// 2. If `newcomer.recipient` already has a row in `roster`, updates its `vault_name`
///    and returns its existing label.
/// 3. Otherwise inspects `leading_m_label(newcomer.requested_host)`:
///    - Never assigns a label in `policy.effective_reserved` (`m3`, stewards), `deny_labels`,
///      or already held (as label or alias) by a member.
///    - If `m<N>` is named in `sheet` and held by no member, assigns provisional `next_free`
///      and records a 24 h claim on `m<N>`.
///    - If `m<N>` is completely free, keeps `m<N>`.
///    - Otherwise (including non-`m<N>` names like `Mac-mini-M4`), assigns `next_free`
///      strictly above every `m<N>` in sheet, members, reserved policy, and `bonjour_hosts`.
pub fn assign_newcomer(
    roster: &mut SwarmRoster,
    doc: &Document,
    policy: &SwarmPolicy,
    sheet: &SheetHints,
    bonjour_hosts: &[String],
    newcomer: Newcomer<'_>,
) -> String {
    promote_due_claims(roster, doc, policy, sheet, newcomer.now_ms);

    if let Some(existing) = roster
        .members
        .iter_mut()
        .find(|m| m.recipient == newcomer.recipient)
    {
        existing.vault_name = newcomer.vault_name.to_owned();
        return existing.label.clone();
    }

    let reserved = policy.effective_reserved(doc, Some(roster));
    let active_recipients: BTreeSet<String> =
        doc.machines().into_iter().map(|m| m.recipient).collect();

    let held_labels: BTreeSet<String> = roster
        .members
        .iter()
        .filter(|m| active_recipients.contains(&m.recipient) || m.recipient == newcomer.recipient)
        .flat_map(|m| std::iter::once(m.label.clone()).chain(m.aliases.iter().cloned()))
        .collect();

    let mut claimed_label = None;
    let mut claimed_at_ms = None;
    let label = match leading_m_label(newcomer.requested_host) {
        Some((n, wanted))
            if !reserved.contains(&wanted)
                && !roster.is_label_denied(&wanted)
                && !held_labels.contains(&wanted) =>
        {
            if sheet.numbers.contains(&n) {
                let provisional = next_free_number(roster, doc, &reserved, sheet, bonjour_hosts);
                claimed_label = Some(wanted);
                claimed_at_ms = Some(newcomer.now_ms);
                format!("m{provisional}")
            } else {
                wanted
            }
        }
        _ => {
            let next = next_free_number(roster, doc, &reserved, sheet, bonjour_hosts);
            format!("m{next}")
        }
    };

    let live_numbers: BTreeSet<u32> = roster
        .members
        .iter()
        .filter_map(|m| leading_m_label(&m.label).map(|(n, _)| n))
        .chain(leading_m_label(&label).map(|(n, _)| n))
        .collect();
    let mut aliases = Vec::new();
    if claimed_label.is_none()
        && let Some((n, _)) = leading_m_label(&label)
    {
        for folded in sheet.paired_legacy_aliases(n, &live_numbers, &reserved) {
            if !held_labels.contains(&folded) {
                aliases.push(folded);
            }
        }
    }

    let mut member = SwarmMember {
        recipient: newcomer.recipient.to_owned(),
        vault_name: newcomer.vault_name.to_owned(),
        label: label.clone(),
        aliases,
        hint: None,
        admitted_ms: newcomer.now_ms,
        claimed_label,
        claimed_at_ms,
        node_id: None,
        node_key: None,
    };
    member.normalize_aliases();
    roster.members.push(member);
    roster.canonicalize();
    label
}

fn required_str(row: &Table, key: &str, idx: usize) -> Result<String> {
    row.get(key)
        .and_then(Value::as_str)
        .map(str::trim_ascii)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| {
            Error::new(format!(
                "{ROSTER_ENTRY} row {}: missing or empty `{key}`",
                idx + 1
            ))
        })
}

fn optional_str(row: &Table, key: &str) -> Option<String> {
    row.get(key)
        .and_then(Value::as_str)
        .map(str::trim_ascii)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn optional_u64(row: &Table, key: &str) -> Option<u64> {
    row.get(key)
        .and_then(Value::as_integer)
        .and_then(|v| u64::try_from(v).ok())
}

fn string_array(table: &Table, key: &str) -> Result<Vec<String>> {
    let Some(val) = table.get(key) else {
        return Ok(Vec::new());
    };
    let arr = val
        .as_array()
        .ok_or_else(|| Error::new(format!("`{key}` must be an array of strings")))?;
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let s = item
            .as_str()
            .ok_or_else(|| Error::new(format!("`{key}` must contain only strings")))?;
        let trimmed = s.trim_ascii();
        if !trimmed.is_empty() {
            out.push(trimmed.to_owned());
        }
    }
    Ok(out)
}

fn normalize_label_set(raw: Vec<String>) -> Vec<String> {
    let mut set = BTreeSet::new();
    for item in raw {
        if let Some((_, canonical)) = leading_m_label(&item) {
            set.insert(canonical);
        }
    }
    set.into_iter().collect()
}

fn normalize_word_set(raw: Vec<String>) -> Vec<String> {
    let mut set = BTreeSet::new();
    for item in raw {
        let trimmed = item.trim_ascii().to_ascii_lowercase();
        if !trimmed.is_empty() {
            set.insert(trimmed);
        }
    }
    set.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::age::Identity;

    fn sample_recipient(seed_byte: u8) -> String {
        Identity::from_secret_bytes([seed_byte; 32])
            .recipient()
            .to_bech32()
    }

    /// Names that start with `m<N>` parse to canonical `(N, "m<N>")`, while mid-string
    /// `m<N>` tokens (`Mac-mini-M4`) or alphanumeric suffixes (`m3pro`) return `None`.
    /// NEGATIVE CONTROL: a token-anywhere rule like `normalize_machine_id` misreads `Mac-mini-M4` as `m4`.
    #[test]
    fn leading_m_label_requires_m_prefix_and_rejects_mac_mini_m4() {
        assert_eq!(leading_m_label("m22"), Some((22, "m22".to_owned())));
        assert_eq!(
            leading_m_label("M100.local"),
            Some((100, "m100".to_owned()))
        );
        assert_eq!(leading_m_label("m4-mac-mini"), Some((4, "m4".to_owned())));
        assert_eq!(leading_m_label("M9 (2)"), Some((9, "m9".to_owned())));

        // Negative controls: mid-string tokens, chip names, zero.
        assert_eq!(leading_m_label("Mac-mini-M4"), None);
        assert_eq!(leading_m_label("macbook-m3"), None);
        assert_eq!(leading_m_label("andrews-m12"), None);
        assert_eq!(leading_m_label("m3pro"), None);
        assert_eq!(leading_m_label("m0"), None);
        assert_eq!(leading_m_label("m04"), None);
    }

    /// Seeding orders members by admission date: an earlier `m<N>` keeps its label, a later
    /// colliding `m<N>` gets `next_free` above sheet + members + Bonjour with the old `m<N>`
    /// recorded as an alias, `Mac-mini-M4` is not `m4`, and F6 `N`/`N-68` pairs fold `m32`
    /// onto `m100`.
    #[test]
    fn seed_roster_orders_by_admission_date_folds_n_minus_68_and_skips_sheet_and_bonjour() {
        let r_admin = sample_recipient(1);
        let r_early = sample_recipient(2);
        let r_late_dup = sample_recipient(3);
        let r_m100 = sample_recipient(4);
        let r_stock = sample_recipient(5);

        let mut doc = Document::new_vault("m22", &r_admin);
        doc.add_machine("m12-2", &r_late_dup).unwrap();
        doc.add_machine("m12", &r_early).unwrap();
        doc.add_machine("m100", &r_m100).unwrap();
        doc.add_machine("mac-mini-m4", &r_stock).unwrap();

        let mut admissions = BTreeMap::new();
        admissions.insert(
            r_admin.clone(),
            SeedAdmission {
                admitted_ms: 1_000,
                sequence: 1,
            },
        );
        // Even though `m12-2` appeared earlier in `[machines]`, `r_early` has the earlier admission date!
        admissions.insert(
            r_early.clone(),
            SeedAdmission {
                admitted_ms: 2_000,
                sequence: 2,
            },
        );
        admissions.insert(
            r_late_dup.clone(),
            SeedAdmission {
                admitted_ms: 3_000,
                sequence: 3,
            },
        );
        admissions.insert(
            r_m100.clone(),
            SeedAdmission {
                admitted_ms: 4_000,
                sequence: 4,
            },
        );
        admissions.insert(
            r_stock.clone(),
            SeedAdmission {
                admitted_ms: 5_000,
                sequence: 5,
            },
        );

        let sheet = SheetHints::from_fleet_toml(
            r#"
[[account]]
label = "acct-1"
machines = ["m32", "m100"]
class = "max"
email = "one@example.org"

[[account]]
label = "acct-2"
machines = ["m111"]
class = "max"
email = "two@example.org"
"#,
        );
        let bonjour = vec!["m115.local".to_owned()];
        let mut overrides = BTreeMap::new();
        overrides.insert(r_stock.clone(), "Mac-mini-M4".to_owned());

        let roster = seed_roster(
            &doc,
            &admissions,
            &SwarmPolicy::default(),
            &sheet,
            &bonjour,
            &overrides,
        );

        // Earlier admission keeps m12; later m12-2 gets m116 (above m111 in sheet and m115 on Bonjour) with alias m12.
        let early = roster.member_by_recipient(&r_early).unwrap();
        assert_eq!(early.label, "m12");
        assert!(early.aliases.is_empty());

        let late_dup = roster.member_by_recipient(&r_late_dup).unwrap();
        assert_eq!(late_dup.label, "m116");
        assert_eq!(late_dup.aliases, vec!["m12".to_owned()]);

        // m100 folds m32 (100 - 68 = 32) from the shared sheet row.
        let m100 = roster.member_by_recipient(&r_m100).unwrap();
        assert_eq!(m100.label, "m100");
        assert_eq!(m100.aliases, vec!["m32".to_owned()]);
        assert_eq!(
            roster.member_by_label_or_alias("m32").unwrap().recipient,
            r_m100
        );

        // Mac-mini-M4 does NOT become m4; it gets next_free (m117).
        let stock = roster.member_by_recipient(&r_stock).unwrap();
        assert_eq!(stock.label, "m117");
        assert_ne!(stock.label, "m4");
        assert!(roster.member_by_label_or_alias("m4").is_none());

        // Round-trip TOML preserves every field.
        let reparsed = SwarmRoster::from_toml(&roster.to_toml()).unwrap();
        assert_eq!(reparsed, roster);
    }

    /// Reserved labels (`m3` and stewards in `aterm-swarm-policy`) are never assigned to newcomers,
    /// free `m<N>` prefixes are kept, and sheet-named unclaimed labels wait 24 h under a provisional
    /// `next_free` label (promoting only when uncontested).
    #[test]
    fn approver_keeps_free_label_refuses_reserved_m3_and_holds_sheet_labels_24h() {
        let r_admin = sample_recipient(10);
        let r_free = sample_recipient(11);
        let r_wants_m3 = sample_recipient(12);
        let r_wants_steward = sample_recipient(13);
        let r_sheet_claim = sample_recipient(14);
        let r_contested_a = sample_recipient(15);
        let r_contested_b = sample_recipient(16);

        let mut doc = Document::new_vault("m22", &r_admin);
        let policy = SwarmPolicy {
            network_id: Some("alab-lan".to_owned()),
            reserved_labels: vec!["m3".to_owned()],
            release_hosts: vec!["m3".to_owned(), "m22".to_owned()],
            servers: Vec::new(),
            stewards: vec![
                StewardPolicy {
                    label: "m22".to_owned(),
                    rank: 2,
                    always_on: false,
                },
                StewardPolicy {
                    label: "m7".to_owned(),
                    rank: 1,
                    always_on: true,
                },
            ],
        };
        let sheet = SheetHints::from_fleet_toml(
            r#"
[[account]]
label = "acct-1"
machines = ["m20", "m21", "m110"]
class = "max"
email = "one@example.org"
"#,
        );
        let bonjour = vec!["m112.local".to_owned()];

        let mut roster = seed_roster(
            &doc,
            &BTreeMap::new(),
            &policy,
            &sheet,
            &bonjour,
            &BTreeMap::new(),
        );
        assert_eq!(roster.member_by_recipient(&r_admin).unwrap().label, "m22");

        // 1. Free m<N> not in sheet -> kept immediately.
        doc.add_machine("m50", &r_free).unwrap();
        let lbl_free = assign_newcomer(
            &mut roster,
            &doc,
            &policy,
            &sheet,
            &bonjour,
            Newcomer {
                recipient: &r_free,
                requested_host: "m50.local",
                vault_name: "m50",
                now_ms: 10_000,
            },
        );
        assert_eq!(lbl_free, "m50");

        // 2. A newcomer named `m3` does NOT get `m3` (reserved release Mac) -> gets m113 (above m112 Bonjour).
        doc.add_machine("m3", &r_wants_m3).unwrap();
        let lbl_m3 = assign_newcomer(
            &mut roster,
            &doc,
            &policy,
            &sheet,
            &bonjour,
            Newcomer {
                recipient: &r_wants_m3,
                requested_host: "m3",
                vault_name: "m3",
                now_ms: 11_000,
            },
        );
        assert_ne!(lbl_m3, "m3", "a newcomer named m3 must never get m3");
        assert_eq!(lbl_m3, "m113");

        // 3. A newcomer named `m7` (reserved steward in policy) does NOT get `m7` -> gets m114.
        doc.add_machine("m7", &r_wants_steward).unwrap();
        let lbl_steward = assign_newcomer(
            &mut roster,
            &doc,
            &policy,
            &sheet,
            &bonjour,
            Newcomer {
                recipient: &r_wants_steward,
                requested_host: "m7",
                vault_name: "m7",
                now_ms: 12_000,
            },
        );
        assert_ne!(lbl_steward, "m7");
        assert_eq!(lbl_steward, "m114");

        // 4. Sheet-named `m20` held by no member gets provisional `m115` with a 24 h hold.
        let t0 = 100_000_u64;
        doc.add_machine("m20", &r_sheet_claim).unwrap();
        let lbl_provisional = assign_newcomer(
            &mut roster,
            &doc,
            &policy,
            &sheet,
            &bonjour,
            Newcomer {
                recipient: &r_sheet_claim,
                requested_host: "m20",
                vault_name: "m20",
                now_ms: t0,
            },
        );
        assert_eq!(lbl_provisional, "m115");
        assert_eq!(
            roster
                .member_by_recipient(&r_sheet_claim)
                .unwrap()
                .claimed_label
                .as_deref(),
            Some("m20")
        );

        // Before 24 h: stays provisional.
        assert!(!promote_due_claims(
            &mut roster,
            &doc,
            &policy,
            &sheet,
            t0 + SHEET_CLAIM_HOLD_MS - 1
        ));
        assert_eq!(
            roster.member_by_recipient(&r_sheet_claim).unwrap().label,
            "m115"
        );

        // At 24 h with no competing claimant: promotes to m20 and keeps m115 as alias!
        assert!(promote_due_claims(
            &mut roster,
            &doc,
            &policy,
            &sheet,
            t0 + SHEET_CLAIM_HOLD_MS
        ));
        let promoted = roster.member_by_recipient(&r_sheet_claim).unwrap();
        assert_eq!(promoted.label, "m20");
        assert_eq!(promoted.aliases, vec!["m115".to_owned()]);
        assert!(promoted.claimed_label.is_none());

        // Negative control on 24 h hold: two competing claimants for sheet-named `m21` never auto-promote.
        doc.add_machine("m21", &r_contested_a).unwrap();
        doc.add_machine("m21-2", &r_contested_b).unwrap();
        let _ = assign_newcomer(
            &mut roster,
            &doc,
            &policy,
            &sheet,
            &bonjour,
            Newcomer {
                recipient: &r_contested_a,
                requested_host: "m21",
                vault_name: "m21",
                now_ms: t0,
            },
        );
        let _ = assign_newcomer(
            &mut roster,
            &doc,
            &policy,
            &sheet,
            &bonjour,
            Newcomer {
                recipient: &r_contested_b,
                requested_host: "m21",
                vault_name: "m21-2",
                now_ms: t0 + 1_000,
            },
        );
        assert!(!promote_due_claims(
            &mut roster,
            &doc,
            &policy,
            &sheet,
            t0 + SHEET_CLAIM_HOLD_MS + 10_000
        ));
        assert_ne!(
            roster.member_by_recipient(&r_contested_a).unwrap().label,
            "m21"
        );
        assert_ne!(
            roster.member_by_recipient(&r_contested_b).unwrap().label,
            "m21"
        );
    }
}
