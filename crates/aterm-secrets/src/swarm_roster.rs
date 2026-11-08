// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Historical swarm denial evidence and steward policy.
//!
//! Owner labels live in the registry. The historical roster lives in the
//! reserved `[everyone]` vault entry [`ROSTER_ENTRY`] (`aterm-swarm-roster`),
//! until D6 converts its denials to log removals. It never allocates labels;
//! steward policy and reserved labels live in [`POLICY_ENTRY`]
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

/// Default release Mac label that policy always reserves (`DESIGN-alab-swarm` §3.2).
pub const DEFAULT_RESERVED_RELEASE_LABEL: &str = "m3";

/// Extract a canonical `(N, "m<N>")` label ONLY when `name` starts with `m<N>` (`N >= 1`).
///
/// Mid-string tokens are never accepted: `"Mac-mini-M4"` and `"macbook-m3"` return `None`,
/// as do alphanumeric suffixes like `"m3pro"` or zero-numbered `"m0"`. Host suffixes such
/// as `"M22.local"`, `"m100-studio"`, or `"m4 (2)"` return `Some((N, "m<N>"))`.
#[must_use]
pub fn leading_m_label(name: &str) -> Option<(u32, String)> {
    aterm_types::machine_name::Number::leading(name).map(|n| (n.get(), n.to_string()))
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
    /// Optional keyed hardware hint of the lab model (`aterm_db::typed::keys::hw_hint`), in a field
    /// of its own: [`Self::hint`] is salted by a signer and this by the network, so neither can be
    /// compared with the other.
    pub hw_hint: Option<String>,
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
            .map_err(|e| Error::new(format!("invalid {ROSTER_ENTRY}: {}", e.log_words())))?;
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
                        "{ROSTER_ENTRY} member {}: invalid m<N> label",
                        idx + 1
                    ))
                })?;
                let aliases = string_array(row, "aliases")?;
                let hint = optional_str(row, "hint");
                let hw_hint = optional_str(row, "hw_hint");
                let admitted_ms = optional_u64(row, "admitted_ms").unwrap_or(0);
                let claimed_label = match optional_str(row, "claimed_label") {
                    Some(raw) => Some(
                        leading_m_label(&raw)
                            .ok_or_else(|| {
                                Error::new(format!(
                                    "{ROSTER_ENTRY} member {}: invalid claimed_label",
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
                    hw_hint,
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
        self.deny_labels = normalize_label_set(std::mem::take(&mut self.deny_labels));
        self.deny_nodes = normalize_word_set(std::mem::take(&mut self.deny_nodes));
        for member in &mut self.members {
            member.normalize_aliases();
        }
        if self.members.len() > 1 {
            let mut best_by_recipient: BTreeMap<String, (usize, SwarmMember)> = BTreeMap::new();
            for (idx, m) in std::mem::take(&mut self.members).into_iter().enumerate() {
                let m_key = (
                    !self.is_member_denied(&m),
                    m.node_key.is_some() || m.node_id.is_some(),
                    m.admitted_ms,
                    std::cmp::Reverse(idx),
                );
                match best_by_recipient.get_mut(&m.recipient) {
                    Some((prev_idx, prev)) => {
                        let prev_key = (
                            !self.is_member_denied(prev),
                            prev.node_key.is_some() || prev.node_id.is_some(),
                            prev.admitted_ms,
                            std::cmp::Reverse(*prev_idx),
                        );
                        if m_key > prev_key {
                            *prev_idx = idx;
                            *prev = m;
                        }
                    }
                    None => {
                        best_by_recipient.insert(m.recipient.clone(), (idx, m));
                    }
                }
            }
            self.members = best_by_recipient.into_values().map(|(_, m)| m).collect();
        }
        self.members.sort_by(|a, b| {
            let na = leading_m_label(&a.label).map_or(u32::MAX, |(n, _)| n);
            let nb = leading_m_label(&b.label).map_or(u32::MAX, |(n, _)| n);
            (na, &a.recipient).cmp(&(nb, &b.recipient))
        });
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
                if let Some(hw_hint) = m.hw_hint {
                    t.insert("hw_hint".to_owned(), Value::String(hw_hint));
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

    /// Find the roster row for `recipient`, preferring active (un-denied), bound, and newer
    /// rows when multiple rows share `recipient`.
    #[must_use]
    pub fn member_by_recipient(&self, recipient: &str) -> Option<&SwarmMember> {
        self.members
            .iter()
            .enumerate()
            .filter(|(_, m)| m.recipient == recipient)
            .max_by_key(|(idx, m)| {
                (
                    !self.is_member_denied(m),
                    m.node_key.is_some() || m.node_id.is_some(),
                    m.admitted_ms,
                    std::cmp::Reverse(*idx),
                )
            })
            .map(|(_, m)| m)
    }

    /// Find the roster row matching `label` or one of its `aliases`, preferring active
    /// (un-denied), primary-label, bound, and newer rows when multiple rows match.
    #[must_use]
    pub fn member_by_label_or_alias(&self, label: &str) -> Option<&SwarmMember> {
        let canonical = leading_m_label(label).map(|(_, c)| c);
        self.members
            .iter()
            .enumerate()
            .filter(|(_, m)| m.matches_label_or_alias(label))
            .max_by_key(|(idx, m)| {
                (
                    !self.is_member_denied(m),
                    canonical.as_deref() == Some(m.label.as_str()),
                    m.node_key.is_some() || m.node_id.is_some(),
                    m.admitted_ms,
                    std::cmp::Reverse(*idx),
                )
            })
            .map(|(_, m)| m)
    }

    /// True when `m` is denied by `deny_labels` (primary label or alias) or `deny_nodes`
    /// (`node_id` or `node_key`).
    #[must_use]
    pub fn is_member_denied(&self, m: &SwarmMember) -> bool {
        self.is_label_denied(&m.label)
            || m.aliases.iter().any(|a| self.is_label_denied(a))
            || m.node_id.as_deref().is_some_and(|n| self.is_node_denied(n))
            || m.node_key
                .as_deref()
                .is_some_and(|k| self.is_node_denied(k))
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
            .map_err(|e| Error::new(format!("invalid {POLICY_ENTRY}: {}", e.log_words())))?;
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
                        "{POLICY_ENTRY} steward {}: invalid m<N> label",
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
                && !roster.is_member_denied(member)
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
            .map_err(|e| Error::new(format!("invalid {LEASE_ENTRY}: {}", e.log_words())))?;
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
            for cell in machines {
                if let Some(word) = cell.as_str()
                    && let Some((n, _)) = leading_m_label(word)
                {
                    self.numbers.insert(n);
                }
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
}

/// Admission timestamp and vault sequence used to order seeding (`DESIGN-alab-swarm` §3.2).
#[cfg(any(test, debug_assertions))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SeedAdmission {
    /// Author timestamp in Unix ms (`0` if unrecorded).
    pub admitted_ms: u64,
    /// Vault sequence where admitted (`0` if unrecorded).
    pub sequence: u64,
}

/// Build a historical roster fixture. Shipped code never allocates a roster label.
#[cfg(any(test, debug_assertions))]
#[must_use]
pub fn seed_roster(
    doc: &Document,
    admissions: &BTreeMap<String, SeedAdmission>,
    _policy: &SwarmPolicy,
    _sheet: &SheetHints,
    _bonjour_hosts: &[String],
    host_overrides: &BTreeMap<String, String>,
) -> SwarmRoster {
    let mut roster = SwarmRoster::default();
    for (index, machine) in doc.machines().into_iter().enumerate() {
        let name = host_overrides
            .get(&machine.recipient)
            .unwrap_or(&machine.name);
        let label =
            leading_m_label(name).map_or_else(|| format!("m{}", index + 1), |(_, label)| label);
        roster.members.push(SwarmMember {
            admitted_ms: admissions
                .get(&machine.recipient)
                .map_or(0, |a| a.admitted_ms),
            recipient: machine.recipient,
            vault_name: machine.name,
            label,
            aliases: Vec::new(),
            hint: None,
            hw_hint: None,
            claimed_label: None,
            claimed_at_ms: None,
            node_id: None,
            node_key: None,
        });
    }
    roster.canonicalize();
    roster
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

    #[test]
    fn reserved_entry_errors_do_not_quote_values() {
        let private = "private-vault-value-must-not-escape";
        let malformed = format!("value = [\"{private}\" !]\n");
        let syntax = [
            SwarmRoster::from_toml(&malformed).unwrap_err(),
            SwarmPolicy::from_toml(&malformed).unwrap_err(),
            SwarmLease::from_toml(&malformed).unwrap_err(),
        ];
        for error in syntax {
            assert!(!error.to_string().contains(private));
            assert!(error.to_string().contains("line 1"));
        }
        let member = "[[member]]\nrecipient = 'recipient'\nvault_name = 'vault'\n";
        let semantics = [
            SwarmRoster::from_toml(&format!("{member}label = '{private}'\n")).unwrap_err(),
            SwarmRoster::from_toml(&format!(
                "{member}label = 'm1'\nclaimed_label = '{private}'\n"
            ))
            .unwrap_err(),
            SwarmPolicy::from_toml(&format!("[[steward]]\nlabel = '{private}'\n")).unwrap_err(),
        ];
        for error in semantics {
            assert!(!error.to_string().contains(private));
        }
    }

    /// Negative control: dropping historical aliases when rendering loses the revoked key.
    #[test]
    fn historical_label_denials_survive_without_an_allocator() {
        let old = r#"deny_labels = ["m4"]
deny_nodes = []
[[member]]
recipient = "historical-recipient"
vault_name = "old-name"
label = "m100"
aliases = ["m4"]
node_key = "historical-node-key"
"#;
        let roster = SwarmRoster::from_toml(old).unwrap();
        assert!(roster.is_member_denied(&roster.members[0]));
        let copy = SwarmRoster::from_toml(&roster.to_toml()).unwrap();
        assert_eq!(roster, copy);
        assert!(copy.is_member_denied(&copy.members[0]));
    }

    #[test]
    fn local_hostname_tags_are_leading_only() {
        assert_eq!(leading_m_label("M22.local"), Some((22, "m22".into())));
        for word in ["Mac-mini-M4", "macbook-m3", "m0", "m3pro"] {
            assert_eq!(leading_m_label(word), None);
        }
    }
}
