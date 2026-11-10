// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Swarm census (`aterm fleet census`, `FleetOp::Census`, and `/monitor`'s read)
//! (`docs/DESIGN-alab-swarm-2026-10-27.md` §1 G3/G4/G7/G8, §7.2, §12.3 C24, §12.6 P2–P5).
//!
//! Computed every 5 minutes (`CENSUS_REFRESH_S`) on every Mac from its own book
//! (`docs/DESIGN-fluid-swarm-2026-10-28.md` §4.1, F17), persisted at
//! `<state_dir>/swarm-census.json`, and served over `FleetOp::Census` to an older Mac that asks
//! for it: `/monitor` and `aterm fleet census` dial no Mac for it.
//!
//! Joins five sources by canonical `m<N>` label:
//! 1. Bonjour (`_ssh._tcp`, `_rfb._tcp`, `_alab._tcp`)
//! 2. Vault members (`aterm-swarm-roster`) + server last-heard records (`server_heard`)
//! 3. Fleet book (`fleet-peers.toml`, `fleet-issuer-book-*.toml`) + C16 heartbeats (`swarm-facts/`)
//! 4. Commons membership count / member bindings
//! 5. Account pool (`ai-pool.toml`) + every Mac's account claims (`accounts/claims/1`)

use std::collections::{BTreeMap, BTreeSet};
#[cfg(unix)]
use std::path::Path;

use crate::harness::ai_pool::{self, LocalPoolIdentity, Pool};
use crate::harness::claims;
use crate::survey_facts::{ServerHeardFact, SwarmSurveyFacts};

/// Cached census filename under `<state_dir>/`.
pub const CENSUS_CACHE_FILE: &str = "swarm-census.json";

/// Refresh cadence for each Mac's cached census (`DESIGN-alab-swarm` §7.2: 5 minutes).
pub const CENSUS_REFRESH_S: u64 = 5 * 60;

/// Seven-day horizon for P2 distinct-Macs-heard ratio (`DESIGN-alab-swarm` §12.6 P2).
pub const HEARD_7D_S: u64 = 7 * 24 * 60 * 60;

/// Twenty-four-hour horizon for release version & converge checks (`DESIGN-alab-swarm` §12.6 P2, P5).
pub const HEARD_24H_S: u64 = 24 * 60 * 60;

/// Window-observation horizon for reported warm-slot eligibility (`DESIGN-alab-swarm` §6.3).
pub const WARM_WINDOW_S: u64 = 24 * 60 * 60;

/// Nine-minute horizon within which a member is considered live/awake (`DESIGN-alab-swarm` §7.2).
pub const LIVE_HEARD_S: u64 = 9 * 60;

/// Exact status string rendered in `/monitor` for Bonjour LAN hosts outside the swarm (§7.2).
pub const STRAGGLER_MONITOR_STATUS: &str = "on the LAN, not in the swarm";

/// Deterministic classification for a LAN host heard on Bonjour that is not an active swarm member
/// (`DESIGN-alab-swarm` §12.6 P2: `{no aterm, Linux, revoked}`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum StragglerReason {
    /// Host is on the swarm roster's `deny_labels` or `deny_nodes`.
    Revoked,
    /// Host's Bonjour / SSH metadata identifies a Linux machine rather than macOS.
    Linux,
    /// macOS / LAN host with no swarm or fleet membership (`no aterm`).
    NoAterm,
}

impl StragglerReason {
    /// Canonical P2 reason string (`"no aterm"`, `"Linux"`, or `"revoked"`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Revoked => "revoked",
            Self::Linux => "Linux",
            Self::NoAterm => "no aterm",
        }
    }

    /// Parse a canonical P2 reason string.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim_ascii() {
            "revoked" => Some(Self::Revoked),
            "Linux" => Some(Self::Linux),
            "no aterm" => Some(Self::NoAterm),
            _ => None,
        }
    }
}

/// Accounts summary for one swarm member (`DESIGN-alab-swarm` §7.2).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemberAccountSummary {
    /// Number of pool accounts whose `warm` list includes this Mac (or its aliases).
    pub warm: usize,
    /// Number of pool accounts this Mac holds a lease on: its senior live claims of kind lease.
    pub leased: usize,
    /// Number of warm accounts for this Mac currently reported walled.
    pub walled: usize,
}

/// One swarm member row in [`SwarmCensus`] (`DESIGN-alab-swarm` §7.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CensusMember {
    /// Canonical `m<N>` label (e.g. `"m22"`).
    pub label: String,
    /// Primary fleet name authenticated by this node, independent of its pool label.
    pub fleet_name: Option<String>,
    /// Legacy `m<N>` aliases carried on the roster row.
    pub aliases: Vec<String>,
    /// Fleet node id (`32` lowercase hex chars) when bound or seen in the fleet book.
    pub node: Option<String>,
    /// Dial host (e.g. `"m22.local"` or IP) when known from fleet book or Bonjour.
    pub host: Option<String>,
    /// Last reported `aterm` version string (`None` when never heard via survey facts).
    pub version: Option<String>,
    /// Receipt time of the survey reporting version, window and session facts.
    pub facts_at: Option<u64>,
    /// True when the member's newest heartbeat reported an open GUI window.
    pub window: bool,
    /// Unix seconds when an open GUI window was last observed on this member.
    pub window_last: Option<u64>,
    /// Active terminal / agent session count from newest heartbeat.
    pub sessions: usize,
    /// Warm / leased / walled account counts for this member.
    pub accounts: MemberAccountSummary,
    /// Newest Unix-seconds timestamp heard across vault `server_heard`, heartbeats, or fleet book.
    pub last_seen: Option<u64>,
    /// Count of failed `aterm init --converge --check` rows in the last 24 h (`0` when clean).
    pub converge_failures: usize,
    /// True when fleet membership, convergence and recent window evidence qualify for this report.
    /// This is not a credential or connection check.
    pub eligible: bool,
    /// True when present in the fleet book or heartbeat set.
    pub in_fleet: bool,
    /// True when present in Commons membership.
    pub in_commons: bool,
}

impl CensusMember {
    /// Human-facing `/monitor` status word derived from `last_seen` at `now_s` (§7.2).
    #[must_use]
    pub fn monitor_status(&self, now_s: u64) -> String {
        match self.last_seen.filter(|seen| *seen <= now_s) {
            Some(seen) if heard_within(now_s, seen, LIVE_HEARD_S) => "online (swarm)".to_owned(),
            Some(seen) if self.window => format!("asleep since {seen}"),
            Some(seen) => format!("away since {seen}"),
            None => "admitted (unheard)".to_owned(),
        }
    }
}

/// One LAN straggler row (`"on the LAN, not in the swarm"`, §7.2 & §12.6 P2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CensusStraggler {
    /// Host or Bonjour instance name (canonicalized when `m<N>`-prefixed, else raw name).
    pub name: String,
    /// `.local` target or IP address.
    pub host: String,
    /// Deterministic P2 reason (`no aterm`, `Linux`, or `revoked`).
    pub reason: StragglerReason,
}

/// Aggregate P2, P3, and P5 swarm health metrics (`DESIGN-alab-swarm` §12.6).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CensusSummary {
    /// Active swarm member count (`members.len()`).
    pub members_total: usize,
    /// Distinct Macs heard on any channel within 7 days (`>= members` heard + stragglers).
    pub distinct_heard_7d: usize,
    /// P2 ratio in basis points (`10_000 == 1.00`, target `>= 9_000` for `0.90`).
    pub p2_ratio_bps: u32,
    /// Members registered in the fleet book / heartbeats.
    pub fleet_members: usize,
    /// Members enrolled in Commons (`P2`: Commons members equal fleet members once C22 lands).
    pub commons_members: usize,
    /// Members heard within 24 h that report `converge_failures > 0` (`P5` target: `0`).
    pub converge_failed_24h: usize,
    /// Fleet or vault rows with no canonical `m<N>` label (`P5` target: `0`).
    pub unlabeled: usize,
    /// Distinct nodes/recipients claiming the same canonical `m<N>` label (`P5` target: `0`).
    pub label_conflicts: usize,
    /// P3's `"short by N accounts"`: how many of [`Self::shortfall`] there are.
    pub shortfall_accounts: usize,
    /// Each CLI of a Mac with sessions, heard within a day, whose every signed-in account is at a
    /// wall (`"m2 claude"`): one more account there is what would let it move. Read from what the
    /// Macs report, so it needs no pool file, and a Mac that says nothing counts for nothing. It
    /// is the wall now, not P3's session-hours at a wall, which nothing measures yet
    /// (`ATERM-KI-1104-14`). A session's CLI is not on the wire, so whether one wants that CLI now
    /// is not read: a wall says it was used up within its window.
    pub shortfall: Vec<String>,
    /// Whether rotation can happen and has ([`RotationSummary`]); `None` from a census written
    /// before it was reported.
    pub rotation: Option<RotationSummary>,
}

/// Whether rotation can happen across the fleet, and has, from what the Macs with sessions report:
/// the plain answer to "has this worked?", which a roster or a pool of accounts cannot give.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RotationSummary {
    /// Macs with sessions heard within a day.
    pub macs: usize,
    /// Those that could not read a pool, whose rows of it are unknown, never none.
    pub unread: usize,
    /// Those holding two or more rows of one CLI that do not read signed out: the condition of
    /// rotating at all. A row's login is not proved to work until a session runs on it, so only
    /// [`Self::moved_7d`] and [`Self::last_move`] say rotation worked.
    pub two_logins: Vec<String>,
    /// Those reporting moves, which have the build for it and could read their ledgers.
    pub reporting: usize,
    /// Those on which a session proved a row a rotation chose, in the last seven days.
    pub moved_7d: usize,
    /// Those that tried more rotations than a session proved, in the last seven days: the move
    /// failed or nothing has run on its row yet.
    pub unproven_7d: usize,
    /// The newest proved rotation any of them keeps, Unix seconds.
    pub last_move: Option<i64>,
}

/// How many Macs a census line names before it says how many more there are.
const NAMED_MAX: usize = 12;

fn named(items: &[String]) -> String {
    let shown = items
        .iter()
        .take(NAMED_MAX)
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    match items.len().saturating_sub(NAMED_MAX) {
        0 => shown,
        more => format!("{shown} and {more} more"),
    }
}

impl RotationSummary {
    /// The census's line.
    #[must_use]
    pub fn line(&self) -> String {
        let can = if self.two_logins.is_empty() {
            String::new()
        } else {
            format!(" ({})", named(&self.two_logins))
        };
        let unread = match self.unread {
            0 => String::new(),
            n => format!(", {n} of them could not read a pool"),
        };
        let moved = if self.reporting == 0 {
            "whether any has moved is unknown: none of them reports it".to_owned()
        } else {
            let unproven = match self.unproven_7d {
                0 => String::new(),
                n => format!(" ({n} tried with no proof it worked)"),
            };
            format!(
                "{} of the {} that report moves moved in 7 days{unproven}, last {}",
                self.moved_7d,
                self.reporting,
                self.last_move
                    .map_or_else(|| "never".to_owned(), crate::harness::usage::rfc3339_utc),
            )
        };
        format!(
            "rotation: {} of {} Macs with sessions hold two signed-in accounts of one CLI{can}\
             {unread}; {moved}",
            self.two_logins.len(),
            self.macs,
        )
    }
}

impl CensusSummary {
    /// Canonical P3 shortfall phrase (`"short by N accounts"`), naming who.
    #[must_use]
    pub fn shortfall_line(&self) -> String {
        let n = self.shortfall_accounts;
        let plural = if n == 1 { "" } else { "s" };
        if self.shortfall.is_empty() {
            format!("short by {n} account{plural}")
        } else {
            format!(
                "short by {n} account{plural} (every signed-in account of the CLI at a wall: {})",
                named(&self.shortfall)
            )
        }
    }
}

/// Complete cached swarm census (`<state_dir>/swarm-census.json` & `aterm fleet census --json`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SwarmCensus {
    /// Unix seconds when this census was computed.
    pub generated_at: u64,
    /// Canonical `m<N>` label of the Mac that computed it, when known.
    pub steward: Option<String>,
    /// Sorted swarm member rows (`m1`, `m2`, …, `m100`).
    pub members: Vec<CensusMember>,
    /// Sorted LAN stragglers (`"on the LAN, not in the swarm"`).
    pub stragglers: Vec<CensusStraggler>,
    /// P2 / P3 / P5 summary metrics.
    pub summary: CensusSummary,
}

/// One Bonjour-discovered host fed into [`CensusInputs`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BonjourHostInput {
    /// Advertised instance or host name (e.g. `"m15"`, `"ubuntu-box"`).
    pub name: String,
    /// `.local` host target (e.g. `"m15.local"`).
    pub dot_local: String,
    /// True if TXT / SSH metadata identifies Linux rather than Darwin/macOS.
    pub is_linux: bool,
    /// Unix seconds when observed on Bonjour (`now_s` for live browse).
    pub seen_at: u64,
}

/// One fleet book entry fed into [`CensusInputs`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetBookPeerInput {
    /// 32-hex fleet node identifier, when bound.
    pub node: Option<String>,
    /// Peer dial host (e.g. `"m14.local"`).
    pub host: String,
    /// Unix seconds when registered or last checked, when known.
    pub seen_at: Option<u64>,
}

/// One admitted cached C16 heartbeat snapshot fed into [`CensusInputs`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeartbeatInput {
    /// Proved 32-hex fleet node identifier.
    pub node: String,
    /// Parsed C16 survey facts.
    pub facts: SwarmSurveyFacts,
    /// Unix seconds when the heartbeat was generated or received.
    pub received_at: Option<u64>,
    /// Count of failed `aterm init --converge --check` rows reported for this host.
    pub converge_failures: usize,
}

/// Pure, deterministic inputs for [`SwarmCensus::compute`].
///
/// Any two stewards given equal [`CensusInputs`] produce byte-for-byte identical JSON
/// (`DESIGN-alab-swarm` §7.2 & §12.6 P2).
#[derive(Debug, Clone, Default)]
pub struct CensusInputs {
    /// Evaluation timestamp in Unix seconds.
    pub now_s: u64,
    /// Computing steward's canonical `m<N>` label, if known.
    pub steward: Option<String>,
    /// Parsed `aterm-swarm-roster` (`members`, `deny_labels`, `deny_nodes`).
    pub roster: aterm_secrets::swarm_roster::SwarmRoster,
    /// Vault server subscriber last-heard records across stewards (`(vault, subject, at_s)`).
    pub server_heard: Vec<ServerHeardFact>,
    /// Fleet book peers (`fleet-peers.toml` + `fleet-issuer-book-*.toml`).
    pub fleet_book: Vec<FleetBookPeerInput>,
    /// Authenticated primary fleet names indexed by immutable node id.
    pub fleet_names: BTreeMap<String, String>,
    /// Admitted pushed C16 survey-facts heartbeats, from current records or legacy text.
    pub heartbeats: Vec<HeartbeatInput>,
    /// Canonical `m<N>` labels (or recipient/node ids) enrolled in Commons, when known.
    pub commons_labels: BTreeSet<String>,
    /// Explicit Commons member count override when per-label bindings are opaque.
    pub commons_member_count: Option<usize>,
    /// Parsed `ai-pool.toml`, when materialized.
    pub pool: Option<Pool>,
    /// Every Mac's account claims row (`accounts/claims/1`).
    pub claims: claims::Book,
    /// How the census host reads `claims`: its machine and what its Commons hears, so a lease
    /// whose Mac it has not heard counts for nobody, as on every Mac's claims path.
    pub claims_view: claims::View,
    /// Bonjour LAN hosts (`_ssh._tcp`, `_rfb._tcp`, `_alab._tcp`).
    pub bonjour_hosts: Vec<BonjourHostInput>,
}

fn label_sort_key(label: &str) -> (u8, u32, String) {
    if let Some((n, canon)) = ai_pool::leading_m_label(label) {
        (0, n, canon)
    } else {
        (1, u32::MAX, label.trim_ascii().to_ascii_lowercase())
    }
}

fn heard_within(now: u64, at: u64, within: u64) -> bool {
    now.checked_sub(at).is_some_and(|age| age <= within)
}

/// The strings of a JSON array, none where it is absent.
fn strings_of(value: Option<&aterm_json::Value>) -> Vec<String> {
    value
        .and_then(aterm_json::Value::as_array)
        .map_or_else(Vec::new, |items| {
            items
                .iter()
                .filter_map(aterm_json::Value::as_str)
                .map(str::to_owned)
                .collect()
        })
}

/// P3's shortfall and [`RotationSummary`], from the Macs with sessions heard within a day: each
/// counts a row as signed in where its Mac reports a login and no sign-out.
fn rotation_of(
    heartbeats: &BTreeMap<String, &HeartbeatInput>,
    now_s: u64,
    now_i64: i64,
) -> (Vec<String>, RotationSummary) {
    let mut labels: Vec<&String> = heartbeats.keys().collect();
    labels.sort_by_key(|label| label_sort_key(label));
    let mut shortfall = Vec::new();
    let mut rotation = RotationSummary::default();
    for label in labels {
        let hb = heartbeats[label];
        if hb.facts.sessions == 0
            || !hb
                .received_at
                .is_some_and(|at| heard_within(now_s, at, HEARD_24H_S))
        {
            continue;
        }
        rotation.macs += 1;
        rotation.unread += usize::from(!hb.facts.accounts_unread.is_empty());
        let mut pools: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
        for acc in &hb.facts.accounts {
            if acc.enrolled && acc.condition != "signed-out" {
                let (signed_in, walled) = pools.entry(acc.pool.as_str()).or_default();
                *signed_in += 1;
                *walled += usize::from(account_is_walled(acc, now_i64));
            }
        }
        if pools.values().any(|(signed_in, _)| *signed_in >= 2) {
            rotation.two_logins.push(label.clone());
        }
        shortfall.extend(
            pools
                .iter()
                .filter(|(_, (signed_in, walled))| signed_in == walled)
                .map(|(pool, _)| format!("{label} {pool}")),
        );
        if let Some(moves) = hb.facts.moves {
            rotation.reporting += 1;
            rotation.moved_7d += usize::from(moves.proven_7d > 0);
            rotation.unproven_7d += usize::from(moves.tried_7d > moves.proven_7d);
            rotation.last_move = rotation.last_move.max(moves.last);
        }
    }
    (shortfall, rotation)
}

fn account_is_walled(acc: &crate::survey_facts::SurveyAccountFact, now_i64: i64) -> bool {
    if !acc.enrolled {
        return false;
    }
    match acc.walled_until {
        Some(until) => until > now_i64,
        None => matches!(acc.condition.as_str(), "limited" | "rate_limited"),
    }
}

/// Normalize a raw hostname, `.local` target, or `m<N>`-prefixed instance name to its canonical
/// lowercase `m<N>` label when present, else its lowercase `.local`-stripped stem.
#[must_use]
pub fn canonical_host_stem(raw: &str) -> String {
    let trimmed = raw.trim_ascii();
    let stem = trimmed
        .strip_suffix(".local.")
        .or_else(|| trimmed.strip_suffix(".local"))
        .unwrap_or(trimmed);
    if let Some((_, canon)) = ai_pool::leading_m_label(stem) {
        canon
    } else {
        stem.to_ascii_lowercase()
    }
}

impl SwarmCensus {
    /// Deterministically join all five swarm sources by canonical `m<N>` label (§7.2).
    #[must_use]
    pub fn compute(inputs: &CensusInputs) -> Self {
        let now_s = inputs.now_s;
        let now_i64 = i64::try_from(now_s).unwrap_or(i64::MAX);

        let mut deny_labels: BTreeSet<String> = inputs
            .roster
            .deny_labels
            .iter()
            .map(|d| canonical_host_stem(d))
            .collect();
        let mut deny_nodes: BTreeSet<String> = inputs
            .roster
            .deny_nodes
            .iter()
            .map(|n| n.trim_ascii().to_ascii_lowercase())
            .collect();
        let mut active_labels: BTreeSet<String> = BTreeSet::new();
        let mut active_nodes: BTreeSet<String> = BTreeSet::new();
        for member in &inputs.roster.members {
            let member_denied = deny_labels.contains(&canonical_host_stem(&member.label))
                || member
                    .aliases
                    .iter()
                    .any(|a| deny_labels.contains(&canonical_host_stem(a)))
                || member
                    .node_id
                    .as_deref()
                    .is_some_and(|n| deny_nodes.contains(&n.trim_ascii().to_ascii_lowercase()))
                || member
                    .node_key
                    .as_deref()
                    .is_some_and(|k| deny_nodes.contains(&k.trim_ascii().to_ascii_lowercase()));
            if !member_denied {
                if let Some((_, canon)) = ai_pool::leading_m_label(&member.label) {
                    active_labels.insert(canon);
                }
                for alias in &member.aliases {
                    if let Some((_, calias)) = ai_pool::leading_m_label(alias) {
                        active_labels.insert(calias);
                    }
                }
                if let Some(n) = member.node_id.as_deref() {
                    active_nodes.insert(n.trim_ascii().to_ascii_lowercase());
                }
                if let Some(k) = member.node_key.as_deref() {
                    active_nodes.insert(k.trim_ascii().to_ascii_lowercase());
                }
            }
        }
        for member in &inputs.roster.members {
            let member_denied = deny_labels.contains(&canonical_host_stem(&member.label))
                || member
                    .aliases
                    .iter()
                    .any(|a| deny_labels.contains(&canonical_host_stem(a)))
                || member
                    .node_id
                    .as_deref()
                    .is_some_and(|n| deny_nodes.contains(&n.trim_ascii().to_ascii_lowercase()))
                || member
                    .node_key
                    .as_deref()
                    .is_some_and(|k| deny_nodes.contains(&k.trim_ascii().to_ascii_lowercase()));
            if member_denied {
                if let Some(n) = member.node_id.as_deref() {
                    let norm = n.trim_ascii().to_ascii_lowercase();
                    if !active_nodes.contains(&norm) {
                        deny_nodes.insert(norm);
                    }
                }
                if let Some(k) = member.node_key.as_deref() {
                    let norm = k.trim_ascii().to_ascii_lowercase();
                    if !active_nodes.contains(&norm) {
                        deny_nodes.insert(norm);
                    }
                }
                if let Some((_, canon)) = ai_pool::leading_m_label(&member.label)
                    && !active_labels.contains(&canon)
                {
                    deny_labels.insert(canon);
                    for alias in &member.aliases {
                        if let Some((_, calias)) = ai_pool::leading_m_label(alias)
                            && !active_labels.contains(&calias)
                        {
                            deny_labels.insert(calias);
                        }
                    }
                }
            }
        }

        // Map canonical alias/label -> canonical primary `m<N>` label, and detect conflicts/unlabeled.
        let mut alias_to_primary: BTreeMap<String, String> = BTreeMap::new();
        let mut node_to_primary: BTreeMap<String, String> = BTreeMap::new();
        let mut recipient_to_primary: BTreeMap<String, String> = BTreeMap::new();
        let mut vault_name_to_primary: BTreeMap<String, String> = BTreeMap::new();
        let mut primary_owners: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut unlabeled = 0usize;

        let mut roster_by_label: BTreeMap<String, aterm_secrets::swarm_roster::SwarmMember> =
            BTreeMap::new();
        for member in &inputs.roster.members {
            if deny_labels.contains(&canonical_host_stem(&member.label))
                || member
                    .aliases
                    .iter()
                    .any(|a| deny_labels.contains(&canonical_host_stem(a)))
                || member
                    .node_id
                    .as_deref()
                    .is_some_and(|n| deny_nodes.contains(&n.trim_ascii().to_ascii_lowercase()))
                || member
                    .node_key
                    .as_deref()
                    .is_some_and(|k| deny_nodes.contains(&k.trim_ascii().to_ascii_lowercase()))
            {
                continue;
            }
            let Some((_, canon)) = ai_pool::leading_m_label(&member.label) else {
                unlabeled += 1;
                continue;
            };
            primary_owners
                .entry(canon.clone())
                .or_default()
                .insert(format!("recipient:{}", member.recipient));
            if !member.recipient.trim_ascii().is_empty() {
                recipient_to_primary.insert(
                    member.recipient.trim_ascii().to_ascii_lowercase(),
                    canon.clone(),
                );
            }
            alias_to_primary.insert(canon.clone(), canon.clone());
            for alias in &member.aliases {
                if let Some((_, calias)) = ai_pool::leading_m_label(alias) {
                    alias_to_primary.insert(calias, canon.clone());
                }
            }
            if !member.vault_name.trim_ascii().is_empty()
                && !deny_labels.contains(&canonical_host_stem(&member.vault_name))
            {
                vault_name_to_primary.insert(
                    member.vault_name.trim_ascii().to_ascii_lowercase(),
                    canon.clone(),
                );
            }
            if let Some(node) = member.node_id.as_deref() {
                node_to_primary.insert(node.trim_ascii().to_ascii_lowercase(), canon.clone());
            }
            match roster_by_label.get_mut(&canon) {
                None => {
                    roster_by_label.insert(canon, member.clone());
                }
                Some(prev) => {
                    let prev_key = (
                        prev.node_id.is_some() || prev.node_key.is_some(),
                        prev.admitted_ms,
                        prev.recipient.as_str(),
                    );
                    let cur_key = (
                        member.node_id.is_some() || member.node_key.is_some(),
                        member.admitted_ms,
                        member.recipient.as_str(),
                    );
                    let mut merged_aliases: BTreeSet<String> =
                        prev.aliases.iter().cloned().collect();
                    merged_aliases.extend(member.aliases.iter().cloned());
                    if cur_key > prev_key {
                        *prev = member.clone();
                    }
                    prev.aliases = merged_aliases.into_iter().collect();
                }
            }
        }

        // Index fleet book & heartbeats by canonical `m<N>` label (falling back to fleet-only rows
        // when the vault roster hasn't materialized yet).
        let mut fleet_names_by_node: BTreeMap<String, String> = BTreeMap::new();
        for (raw_node, name) in &inputs.fleet_names {
            let trimmed = raw_node.trim_ascii();
            let node_lc = trimmed.to_ascii_lowercase();
            if node_lc.is_empty()
                || deny_nodes.contains(&node_lc)
                || !aterm_types::fleet_name::valid_primary_name(name)
            {
                continue;
            }
            if trimmed == node_lc || !fleet_names_by_node.contains_key(&node_lc) {
                fleet_names_by_node.insert(node_lc, name.clone());
            }
        }
        let mut fleet_host_by_label: BTreeMap<String, (u64, String)> = BTreeMap::new();
        let mut fleet_node_by_label: BTreeMap<String, (u64, String)> = BTreeMap::new();
        let mut fleet_seen_by_label: BTreeMap<String, u64> = BTreeMap::new();
        let mut in_fleet_labels: BTreeSet<String> = BTreeSet::new();

        for peer in &inputs.fleet_book {
            if let Some(node) = peer.node.as_deref()
                && deny_nodes.contains(&node.to_ascii_lowercase())
            {
                continue;
            }
            let resolved = peer
                .node
                .as_deref()
                .and_then(|n| node_to_primary.get(&n.to_ascii_lowercase()).cloned())
                .or_else(|| {
                    let stem = canonical_host_stem(&peer.host);
                    alias_to_primary
                        .get(&stem)
                        .cloned()
                        .or_else(|| ai_pool::leading_m_label(&stem).map(|(_, canon)| canon))
                });
            let Some(canon) = resolved else {
                unlabeled += 1;
                continue;
            };
            if deny_labels.contains(&canon) {
                continue;
            }
            in_fleet_labels.insert(canon.clone());
            let seen = peer.seen_at.filter(|at| *at <= now_s);
            let seen_rank = seen.unwrap_or(0);
            if !peer.host.trim_ascii().is_empty() {
                let candidate = (seen_rank, peer.host.clone());
                match fleet_host_by_label.get_mut(&canon) {
                    None => {
                        fleet_host_by_label.insert(canon.clone(), candidate);
                    }
                    Some(prev) => {
                        if (candidate.0, &candidate.1) > (prev.0, &prev.1) {
                            *prev = candidate;
                        }
                    }
                }
            }
            if let Some(node) = peer.node.as_deref() {
                let node_lc = node.to_ascii_lowercase();
                primary_owners
                    .entry(canon.clone())
                    .or_default()
                    .insert(format!("node:{node_lc}"));
                // If the roster already bound this member's `node_id` to `node_lc`, collapse the two
                // owner tokens so a bound member is never counted as a conflict!
                if roster_by_label
                    .get(&canon)
                    .and_then(|m| m.node_id.as_deref())
                    .is_some_and(|rn| rn.eq_ignore_ascii_case(&node_lc))
                    && let Some(owners) = primary_owners.get_mut(&canon)
                {
                    owners.remove(&format!("node:{node_lc}"));
                }
                let candidate = (seen_rank, node_lc);
                match fleet_node_by_label.get_mut(&canon) {
                    None => {
                        fleet_node_by_label.insert(canon.clone(), candidate);
                    }
                    Some(prev) => {
                        if (candidate.0, &candidate.1) > (prev.0, &prev.1) {
                            *prev = candidate;
                        }
                    }
                }
            }
            if let Some(at) = seen {
                fleet_seen_by_label
                    .entry(canon)
                    .and_modify(|prev| *prev = (*prev).max(at))
                    .or_insert(at);
            }
        }

        let mut hb_by_label: BTreeMap<String, &HeartbeatInput> = BTreeMap::new();
        let mut all_server_heard: Vec<ServerHeardFact> = inputs.server_heard.clone();
        for hb in &inputs.heartbeats {
            let node_lc = hb.node.to_ascii_lowercase();
            if deny_nodes.contains(&node_lc) {
                continue;
            }
            let future_receipt = hb.received_at.is_some_and(|at| at > now_s);
            let resolved = node_to_primary.get(&node_lc).cloned().or_else(|| {
                hb.facts.label.as_deref().and_then(|lbl| {
                    let stem = canonical_host_stem(lbl);
                    alias_to_primary
                        .get(&stem)
                        .cloned()
                        .or_else(|| ai_pool::leading_m_label(&stem).map(|(_, c)| c))
                })
            });
            let Some(canon) = resolved else {
                unlabeled += 1;
                continue;
            };
            if deny_labels.contains(&canon) {
                continue;
            }
            if !future_receipt {
                all_server_heard.extend(hb.facts.server_heard.iter().cloned());
            }
            in_fleet_labels.insert(canon.clone());
            let hb_seen = hb.received_at.filter(|at| *at <= now_s).unwrap_or(0);
            let node_candidate = (hb_seen, node_lc.clone());
            match fleet_node_by_label.get_mut(&canon) {
                None => {
                    fleet_node_by_label.insert(canon.clone(), node_candidate);
                }
                Some(prev) => {
                    if (node_candidate.0, &node_candidate.1) > (prev.0, &prev.1) {
                        *prev = node_candidate;
                    }
                }
            }
            primary_owners
                .entry(canon.clone())
                .or_default()
                .insert(format!("node:{node_lc}"));
            if roster_by_label
                .get(&canon)
                .and_then(|m| m.node_id.as_deref())
                .is_some_and(|rn| rn.eq_ignore_ascii_case(&node_lc))
                && let Some(owners) = primary_owners.get_mut(&canon)
            {
                owners.remove(&format!("node:{node_lc}"));
            }
            // A clock correction does not erase this proved member's identity. Its
            // future receipt supplies no timed facts and displaces no known report.
            if future_receipt {
                continue;
            }
            let replace = match hb_by_label.get(&canon) {
                None => true,
                Some(prev) => {
                    let cur_rank = (
                        hb.received_at.unwrap_or(0),
                        hb.facts.window_last.filter(|at| *at <= now_s).unwrap_or(0),
                        hb.facts.window,
                        hb.facts.sessions,
                        hb.node.as_str(),
                        hb.facts.version.as_str(),
                    );
                    let prev_rank = (
                        prev.received_at.unwrap_or(0),
                        prev.facts
                            .window_last
                            .filter(|at| *at <= now_s)
                            .unwrap_or(0),
                        prev.facts.window,
                        prev.facts.sessions,
                        prev.node.as_str(),
                        prev.facts.version.as_str(),
                    );
                    cur_rank > prev_rank
                }
            };
            if replace {
                hb_by_label.insert(canon, hb);
            }
        }

        // Index vault `server_heard` timestamps by canonical `m<N>` label.
        let mut server_seen_by_label: BTreeMap<String, u64> = BTreeMap::new();
        for sh in &all_server_heard {
            if sh.at > now_s {
                continue;
            }
            let subj_lc = sh.subject.trim_ascii().to_ascii_lowercase();
            let resolved = recipient_to_primary
                .get(&subj_lc)
                .cloned()
                .or_else(|| vault_name_to_primary.get(&subj_lc).cloned())
                .or_else(|| {
                    alias_to_primary
                        .get(&canonical_host_stem(&subj_lc))
                        .cloned()
                })
                .or_else(|| ai_pool::leading_m_label(&subj_lc).map(|(_, c)| c));
            if let Some(canon) = resolved
                && !deny_labels.contains(&canon)
            {
                server_seen_by_label
                    .entry(canon)
                    .and_modify(|prev| *prev = (*prev).max(sh.at))
                    .or_insert(sh.at);
            }
        }

        // Collect all active swarm member labels across roster + fleet.
        let mut all_member_labels: BTreeSet<String> = roster_by_label.keys().cloned().collect();
        for lbl in &in_fleet_labels {
            all_member_labels.insert(lbl.clone());
        }

        // Index Bonjour hosts and split into member-matched vs straggler candidates.
        let mut bonjour_by_label: BTreeMap<String, &BonjourHostInput> = BTreeMap::new();
        let mut stragglers: Vec<CensusStraggler> = Vec::new();
        let mut distinct_7d_keys: BTreeSet<String> = BTreeSet::new();

        for bj in &inputs.bonjour_hosts {
            if bj.seen_at > now_s {
                continue;
            }
            let stem = canonical_host_stem(&bj.name);
            let mapped = alias_to_primary
                .get(&stem)
                .cloned()
                .or_else(|| all_member_labels.contains(&stem).then(|| stem.clone()));
            if let Some(canon) = mapped {
                match bonjour_by_label.get_mut(&canon) {
                    None => {
                        bonjour_by_label.insert(canon.clone(), bj);
                    }
                    Some(prev) => {
                        let cur_key = (bj.seen_at, bj.dot_local.as_str(), bj.name.as_str());
                        let prev_key = (prev.seen_at, prev.dot_local.as_str(), prev.name.as_str());
                        if cur_key > prev_key {
                            *prev = bj;
                        }
                    }
                }
                if heard_within(now_s, bj.seen_at, HEARD_7D_S) {
                    distinct_7d_keys.insert(format!("member:{canon}"));
                }
                continue;
            }
            let reason = if deny_labels.contains(&stem) {
                StragglerReason::Revoked
            } else if bj.is_linux {
                StragglerReason::Linux
            } else {
                StragglerReason::NoAterm
            };
            let display_name = ai_pool::leading_m_label(&bj.name)
                .map(|(_, c)| c)
                .unwrap_or_else(|| bj.name.trim_ascii().to_owned());
            if heard_within(now_s, bj.seen_at, HEARD_7D_S) {
                distinct_7d_keys.insert(format!("straggler:{}", display_name.to_ascii_lowercase()));
            }
            stragglers.push(CensusStraggler {
                name: display_name,
                host: bj.dot_local.trim_ascii().to_owned(),
                reason,
            });
        }
        stragglers.sort_by(|a, b| {
            label_sort_key(&a.name)
                .cmp(&label_sort_key(&b.name))
                .then_with(|| a.host.cmp(&b.host))
        });
        stragglers.dedup_by(|a, b| a.name.eq_ignore_ascii_case(&b.name));

        // Member summaries and P3 read the same selected, un-denied heartbeats.
        let mut walled_pool_emails: BTreeSet<(String, String)> = BTreeSet::new();
        for hb in hb_by_label.values() {
            for acc in &hb.facts.accounts {
                if account_is_walled(acc, now_i64) {
                    walled_pool_emails
                        .insert((acc.pool.to_ascii_lowercase(), acc.id.to_ascii_lowercase()));
                }
            }
        }

        // Build sorted `CensusMember` rows.
        let mut ordered_labels: Vec<String> = all_member_labels.into_iter().collect();
        ordered_labels.sort_by_key(|lbl| label_sort_key(lbl));

        let mut members = Vec::with_capacity(ordered_labels.len());

        let active_claims = claims::Book::of(
            inputs
                .claims
                .rows()
                .filter(|row| {
                    let stem = canonical_host_stem(&row.label);
                    let resolved = alias_to_primary
                        .get(&stem)
                        .cloned()
                        .or_else(|| ai_pool::leading_m_label(&stem).map(|(_, c)| c))
                        .unwrap_or(stem);
                    resolved.is_empty() || !deny_labels.contains(&resolved)
                })
                .cloned(),
        );
        let mut converge_failed_24h = 0usize;
        for canon in ordered_labels {
            let roster_row = roster_by_label.get(&canon);
            let aliases = roster_row.map(|r| r.aliases.clone()).unwrap_or_default();
            let identity = LocalPoolIdentity::new(canon.clone(), aliases.clone(), true);
            let hb = hb_by_label.get(&canon).copied();
            let bj = bonjour_by_label.get(&canon).copied();

            let last_seen = [
                server_seen_by_label.get(&canon).copied(),
                fleet_seen_by_label.get(&canon).copied(),
                hb.and_then(|h| h.received_at),
                hb.and_then(|h| h.facts.window_last),
                bj.map(|b| b.seen_at),
            ]
            .into_iter()
            .flatten()
            .filter(|at| *at <= now_s)
            .max();

            if let Some(seen) = last_seen
                && heard_within(now_s, seen, HEARD_7D_S)
            {
                distinct_7d_keys.insert(format!("member:{canon}"));
            }

            let mut warm = 0usize;
            let mut walled = 0usize;
            if let Some(pool) = &inputs.pool {
                for acc in &pool.accounts {
                    if acc.is_warm_for(&identity) {
                        warm += 1;
                        let provider = acc.provider_str().to_ascii_lowercase();
                        let id_key = acc
                            .label
                            .as_deref()
                            .unwrap_or(&acc.email)
                            .to_ascii_lowercase();
                        let email_key = acc.email.to_ascii_lowercase();
                        if walled_pool_emails.contains(&(provider.clone(), id_key))
                            || walled_pool_emails.contains(&(provider, email_key))
                        {
                            walled += 1;
                        }
                    }
                }
            }
            if warm == 0
                && let Some(h) = hb
            {
                warm = h.facts.accounts.iter().filter(|a| a.enrolled).count();
                walled = h
                    .facts
                    .accounts
                    .iter()
                    .filter(|a| account_is_walled(a, now_i64))
                    .count();
            }

            let leased = active_claims
                .rows()
                .filter(|row| identity.matches(&row.label))
                .flat_map(|row| {
                    row.claims.iter().filter(|claim| {
                        claim.kind == claims::Kind::Lease
                            && active_claims
                                .senior(&claim.key, &inputs.claims_view, now_i64)
                                .is_some_and(|holder| holder.machine == row.machine)
                    })
                })
                .count();

            let window = hb.is_some_and(|h| h.facts.window);
            let window_last = hb
                .and_then(|h| h.facts.window_last)
                .filter(|at| *at <= now_s);
            let sessions = hb.map_or(0, |h| h.facts.sessions);
            let version = hb.map(|h| h.facts.version.clone());
            let facts_at = hb.and_then(|h| h.received_at).filter(|at| *at <= now_s);
            let converge_failures = hb.map_or(0, |h| h.converge_failures);
            if converge_failures > 0
                && last_seen.is_some_and(|seen| heard_within(now_s, seen, HEARD_24H_S))
            {
                converge_failed_24h += 1;
            }

            let in_fleet = in_fleet_labels.contains(&canon);
            let in_commons = inputs.commons_labels.contains(&canon);
            let eligible = in_fleet
                && converge_failures == 0
                && window_last.is_some_and(|wl| heard_within(now_s, wl, WARM_WINDOW_S));

            let host = fleet_host_by_label
                .get(&canon)
                .map(|(_, h)| h.clone())
                .or_else(|| bj.map(|b| b.dot_local.clone()));
            let node = roster_row
                .and_then(|r| r.node_id.clone())
                .or_else(|| fleet_node_by_label.get(&canon).map(|(_, n)| n.clone()));

            members.push(CensusMember {
                label: canon,
                fleet_name: node
                    .as_deref()
                    .and_then(|n| fleet_names_by_node.get(&n.trim_ascii().to_ascii_lowercase()))
                    .cloned(),
                aliases,
                node,
                host,
                version,
                facts_at,
                window,
                window_last,
                sessions,
                accounts: MemberAccountSummary {
                    warm,
                    leased,
                    walled,
                },
                last_seen,
                converge_failures,
                eligible,
                in_fleet,
                in_commons,
            });
        }

        // Count `label_conflicts`: any canonical `m<N>` claimed by >1 distinct unmerged identity.
        // Note: if a roster row has `node_id == None` and 1 fleet node claims its hostname, merge
        // that single unbound fleet node with the 1 recipient; >1 recipients or >1 distinct fleet
        // nodes for the same `m<N>` is a true conflict!
        let mut label_conflicts = 0usize;
        for (canon, owners) in &primary_owners {
            let recipients = owners
                .iter()
                .filter(|o| o.starts_with("recipient:"))
                .count();
            let nodes = owners.iter().filter(|o| o.starts_with("node:")).count();
            let unbound_roster = roster_by_label
                .get(canon)
                .is_some_and(|m| m.node_id.is_none());
            let conflict = if unbound_roster {
                recipients > 1 || nodes > 1
            } else {
                owners.len() > 1
            };
            if conflict {
                label_conflicts += 1;
            }
        }

        let members_total = members.len();
        let distinct_heard_7d = distinct_7d_keys.len();
        let member_heard_7d = distinct_7d_keys
            .iter()
            .filter(|k| k.starts_with("member:"))
            .count();
        let p2_ratio_bps = member_heard_7d
            .saturating_mul(10_000)
            .checked_div(distinct_heard_7d)
            .and_then(|v| u32::try_from(v).ok())
            .unwrap_or(10_000);

        let fleet_members = members.iter().filter(|m| m.in_fleet).count();
        let commons_members = inputs
            .commons_member_count
            .unwrap_or_else(|| members.iter().filter(|m| m.in_commons).count());

        let (shortfall, rotation) = rotation_of(&hb_by_label, now_s, now_i64);

        let steward = inputs
            .steward
            .as_deref()
            .map(canonical_host_stem)
            .filter(|s| !s.is_empty());

        Self {
            generated_at: now_s,
            steward,
            members,
            stragglers,
            summary: CensusSummary {
                members_total,
                distinct_heard_7d,
                p2_ratio_bps,
                fleet_members,
                commons_members,
                converge_failed_24h,
                unlabeled,
                label_conflicts,
                shortfall_accounts: shortfall.len(),
                shortfall,
                rotation: Some(rotation),
            },
        }
    }

    /// Deterministic compact JSON representation (`BTreeMap` key order, identical across stewards).
    #[must_use]
    pub fn to_json(&self) -> String {
        use aterm_json::Value;

        let members_val: Vec<Value> = self
            .members
            .iter()
            .map(|m| {
                let mut acc = BTreeMap::new();
                acc.insert("leased".to_owned(), Value::from(m.accounts.leased as u64));
                acc.insert("walled".to_owned(), Value::from(m.accounts.walled as u64));
                acc.insert("warm".to_owned(), Value::from(m.accounts.warm as u64));

                let mut obj = BTreeMap::new();
                obj.insert("accounts".to_owned(), Value::Object(acc));
                obj.insert(
                    "aliases".to_owned(),
                    Value::Array(m.aliases.iter().cloned().map(Value::String).collect()),
                );
                obj.insert(
                    "converge_failures".to_owned(),
                    Value::from(m.converge_failures as u64),
                );
                obj.insert("eligible".to_owned(), Value::Bool(m.eligible));
                obj.insert(
                    "host".to_owned(),
                    m.host.clone().map_or(Value::Null, Value::String),
                );
                obj.insert("in_commons".to_owned(), Value::Bool(m.in_commons));
                obj.insert("in_fleet".to_owned(), Value::Bool(m.in_fleet));
                obj.insert("label".to_owned(), Value::String(m.label.clone()));
                obj.insert(
                    "fleet_name".to_owned(),
                    m.fleet_name.clone().map_or(Value::Null, Value::String),
                );
                obj.insert(
                    "node_alias".to_owned(),
                    m.node
                        .as_deref()
                        .map(aterm_types::fleet_name::temporary_name)
                        .map_or(Value::Null, Value::String),
                );
                obj.insert(
                    "last_seen".to_owned(),
                    m.last_seen.map_or(Value::Null, Value::from),
                );
                obj.insert(
                    "node".to_owned(),
                    m.node.clone().map_or(Value::Null, Value::String),
                );
                obj.insert("sessions".to_owned(), Value::from(m.sessions as u64));
                obj.insert(
                    "facts_at".to_owned(),
                    m.facts_at.map_or(Value::Null, Value::from),
                );
                obj.insert(
                    "version".to_owned(),
                    m.version.clone().map_or(Value::Null, Value::String),
                );
                obj.insert("window".to_owned(), Value::Bool(m.window));
                obj.insert(
                    "window_last".to_owned(),
                    m.window_last.map_or(Value::Null, Value::from),
                );
                Value::Object(obj)
            })
            .collect();

        let stragglers_val: Vec<Value> = self
            .stragglers
            .iter()
            .map(|s| {
                let mut obj = BTreeMap::new();
                obj.insert("host".to_owned(), Value::String(s.host.clone()));
                obj.insert("name".to_owned(), Value::String(s.name.clone()));
                obj.insert(
                    "reason".to_owned(),
                    Value::String(s.reason.as_str().to_owned()),
                );
                obj.insert(
                    "status".to_owned(),
                    Value::String(STRAGGLER_MONITOR_STATUS.to_owned()),
                );
                Value::Object(obj)
            })
            .collect();

        let s = &self.summary;
        let mut sum = BTreeMap::new();
        sum.insert(
            "commons_members".to_owned(),
            Value::from(s.commons_members as u64),
        );
        sum.insert(
            "converge_failed_24h".to_owned(),
            Value::from(s.converge_failed_24h as u64),
        );
        sum.insert(
            "distinct_heard_7d".to_owned(),
            Value::from(s.distinct_heard_7d as u64),
        );
        sum.insert(
            "fleet_members".to_owned(),
            Value::from(s.fleet_members as u64),
        );
        sum.insert(
            "label_conflicts".to_owned(),
            Value::from(s.label_conflicts as u64),
        );
        sum.insert(
            "members_total".to_owned(),
            Value::from(s.members_total as u64),
        );
        sum.insert(
            "p2_ratio_bps".to_owned(),
            Value::from(u64::from(s.p2_ratio_bps)),
        );
        sum.insert(
            "shortfall_accounts".to_owned(),
            Value::from(s.shortfall_accounts as u64),
        );
        sum.insert(
            "shortfall".to_owned(),
            Value::Array(s.shortfall.iter().cloned().map(Value::String).collect()),
        );
        if let Some(r) = &s.rotation {
            let mut rotation = aterm_json::Map::new();
            rotation.insert("macs".to_owned(), Value::from(r.macs as u64));
            rotation.insert("unread".to_owned(), Value::from(r.unread as u64));
            rotation.insert(
                "two_logins".to_owned(),
                Value::Array(r.two_logins.iter().cloned().map(Value::String).collect()),
            );
            rotation.insert("reporting".to_owned(), Value::from(r.reporting as u64));
            rotation.insert("moved_7d".to_owned(), Value::from(r.moved_7d as u64));
            rotation.insert("unproven_7d".to_owned(), Value::from(r.unproven_7d as u64));
            rotation.insert(
                "last_move".to_owned(),
                r.last_move.map_or(Value::Null, Value::from),
            );
            sum.insert("rotation".to_owned(), Value::Object(rotation));
        }
        sum.insert(
            "shortfall_text".to_owned(),
            Value::String(s.shortfall_line()),
        );
        sum.insert("unlabeled".to_owned(), Value::from(s.unlabeled as u64));

        let mut root = BTreeMap::new();
        root.insert("generated_at".to_owned(), Value::from(self.generated_at));
        root.insert("members".to_owned(), Value::Array(members_val));
        root.insert(
            "steward".to_owned(),
            self.steward.clone().map_or(Value::Null, Value::String),
        );
        root.insert("stragglers".to_owned(), Value::Array(stragglers_val));
        root.insert("summary".to_owned(), Value::Object(sum));

        aterm_json::to_string(&Value::Object(root)).unwrap_or_else(|_| "{}".to_owned())
    }

    /// Parse a compact or pretty [`SwarmCensus`] JSON document.
    ///
    /// # Errors
    /// Returns a human-readable error when `raw` is not valid census JSON.
    pub fn from_json(raw: &str) -> Result<Self, String> {
        let root = aterm_json::from_str::<aterm_json::Value>(raw)
            .map_err(|e| format!("invalid census JSON: {e}"))?;
        let obj = root
            .as_object()
            .ok_or_else(|| "census JSON root must be an object".to_owned())?;
        let generated_at = obj
            .get("generated_at")
            .and_then(aterm_json::Value::as_u64)
            .ok_or_else(|| "missing `generated_at` in census JSON".to_owned())?;
        let steward = obj
            .get("steward")
            .and_then(aterm_json::Value::as_str)
            .map(str::to_owned);

        let mut members = Vec::new();
        if let Some(arr) = obj.get("members").and_then(aterm_json::Value::as_array) {
            for item in arr {
                let m = item
                    .as_object()
                    .ok_or_else(|| "census member must be an object".to_owned())?;
                let label = m
                    .get("label")
                    .and_then(aterm_json::Value::as_str)
                    .ok_or_else(|| "census member missing `label`".to_owned())?
                    .to_owned();
                let aliases = m
                    .get("aliases")
                    .and_then(aterm_json::Value::as_array)
                    .map(|xs| {
                        xs.iter()
                            .filter_map(|v| v.as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default();
                let to_usize = |n: u64| usize::try_from(n).unwrap_or(usize::MAX);
                let acc_obj = m.get("accounts").and_then(aterm_json::Value::as_object);
                let warm = to_usize(
                    acc_obj
                        .and_then(|a| a.get("warm"))
                        .and_then(aterm_json::Value::as_u64)
                        .unwrap_or(0),
                );
                let leased = to_usize(
                    acc_obj
                        .and_then(|a| a.get("leased"))
                        .and_then(aterm_json::Value::as_u64)
                        .unwrap_or(0),
                );
                let walled = to_usize(
                    acc_obj
                        .and_then(|a| a.get("walled"))
                        .and_then(aterm_json::Value::as_u64)
                        .unwrap_or(0),
                );
                members.push(CensusMember {
                    label,
                    fleet_name: m
                        .get("fleet_name")
                        .and_then(aterm_json::Value::as_str)
                        .filter(|name| aterm_types::fleet_name::valid_primary_name(name))
                        .map(str::to_owned),
                    aliases,
                    node: m
                        .get("node")
                        .and_then(aterm_json::Value::as_str)
                        .map(str::to_owned),
                    host: m
                        .get("host")
                        .and_then(aterm_json::Value::as_str)
                        .map(str::to_owned),
                    version: m
                        .get("version")
                        .and_then(aterm_json::Value::as_str)
                        .map(str::to_owned),
                    facts_at: m.get("facts_at").and_then(aterm_json::Value::as_u64),
                    window: m
                        .get("window")
                        .and_then(aterm_json::Value::as_bool)
                        .unwrap_or(false),
                    window_last: m.get("window_last").and_then(aterm_json::Value::as_u64),
                    sessions: to_usize(
                        m.get("sessions")
                            .and_then(aterm_json::Value::as_u64)
                            .unwrap_or(0),
                    ),
                    accounts: MemberAccountSummary {
                        warm,
                        leased,
                        walled,
                    },
                    last_seen: m.get("last_seen").and_then(aterm_json::Value::as_u64),
                    converge_failures: to_usize(
                        m.get("converge_failures")
                            .and_then(aterm_json::Value::as_u64)
                            .unwrap_or(0),
                    ),
                    eligible: m
                        .get("eligible")
                        .and_then(aterm_json::Value::as_bool)
                        .unwrap_or(false),
                    in_fleet: m
                        .get("in_fleet")
                        .and_then(aterm_json::Value::as_bool)
                        .unwrap_or(false),
                    in_commons: m
                        .get("in_commons")
                        .and_then(aterm_json::Value::as_bool)
                        .unwrap_or(false),
                });
            }
        }

        let mut stragglers = Vec::new();
        if let Some(arr) = obj.get("stragglers").and_then(aterm_json::Value::as_array) {
            for item in arr {
                let s = item
                    .as_object()
                    .ok_or_else(|| "census straggler must be an object".to_owned())?;
                let name = s
                    .get("name")
                    .and_then(aterm_json::Value::as_str)
                    .ok_or_else(|| "straggler missing `name`".to_owned())?
                    .to_owned();
                let host = s
                    .get("host")
                    .and_then(aterm_json::Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let reason = s
                    .get("reason")
                    .and_then(aterm_json::Value::as_str)
                    .and_then(StragglerReason::parse)
                    .unwrap_or(StragglerReason::NoAterm);
                stragglers.push(CensusStraggler { name, host, reason });
            }
        }

        let sum = obj.get("summary").and_then(aterm_json::Value::as_object);
        let u = |k: &str| {
            sum.and_then(|s| s.get(k))
                .and_then(aterm_json::Value::as_u64)
                .unwrap_or(0)
        };
        let to_usize = |n: u64| usize::try_from(n).unwrap_or(usize::MAX);
        let summary = CensusSummary {
            members_total: to_usize(u("members_total")),
            distinct_heard_7d: to_usize(u("distinct_heard_7d")),
            p2_ratio_bps: u32::try_from(u("p2_ratio_bps")).unwrap_or(u32::MAX),
            fleet_members: to_usize(u("fleet_members")),
            commons_members: to_usize(u("commons_members")),
            converge_failed_24h: to_usize(u("converge_failed_24h")),
            unlabeled: to_usize(u("unlabeled")),
            label_conflicts: to_usize(u("label_conflicts")),
            shortfall_accounts: to_usize(u("shortfall_accounts")),
            shortfall: strings_of(sum.and_then(|s| s.get("shortfall"))),
            rotation: sum.and_then(|s| s.get("rotation")).map(|r| {
                let n =
                    |k: &str| to_usize(r.get(k).and_then(aterm_json::Value::as_u64).unwrap_or(0));
                RotationSummary {
                    macs: n("macs"),
                    unread: n("unread"),
                    two_logins: strings_of(r.get("two_logins")),
                    reporting: n("reporting"),
                    moved_7d: n("moved_7d"),
                    unproven_7d: n("unproven_7d"),
                    last_move: r.get("last_move").and_then(aterm_json::Value::as_i64),
                }
            }),
        };

        Ok(Self {
            generated_at,
            steward,
            members,
            stragglers,
            summary,
        })
    }

    /// Render the human-readable `aterm fleet census` table (`DESIGN-alab-swarm` §7.2, §12.6).
    #[must_use]
    pub fn render_human(&self) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();
        let steward = self.steward.as_deref().unwrap_or("-");
        // With nobody heard there is no ratio: `p2_ratio_bps` holds its 10_000 default, which
        // would print a Mac in no fleet as 100% joined.
        let joined = if self.summary.distinct_heard_7d == 0 {
            "none heard".to_owned()
        } else {
            format!(
                "{:.1}% joined",
                f64::from(self.summary.p2_ratio_bps) / 100.0
            )
        };
        let _ = writeln!(
            out,
            "swarm census (steward {steward}, at {}): {} members ({} heard 7d, {joined}), fleet={}, commons={}",
            self.generated_at,
            self.summary.members_total,
            self.summary.distinct_heard_7d,
            self.summary.fleet_members,
            self.summary.commons_members,
        );
        let _ = writeln!(
            out,
            "pool: {} | converge_failed_24h={} unlabeled={} label_conflicts={}",
            self.summary.shortfall_line(),
            self.summary.converge_failed_24h,
            self.summary.unlabeled,
            self.summary.label_conflicts,
        );
        if let Some(rotation) = &self.summary.rotation {
            let _ = writeln!(out, "{}", rotation.line());
        }
        let _ = writeln!(
            out,
            "POOL    VERSION         WINDOW  SESSIONS  WARM/LEASED/WALLED  ELIGIBLE  CONVERGE  LAST_SEEN  FACTS_AT  FLEET_NAME"
        );
        for m in &self.members {
            let ver = m.version.as_deref().unwrap_or("-");
            let win = match (m.window, m.window_last) {
                (true, _) => "yes".to_owned(),
                (false, Some(wl)) => format!("no({wl})"),
                (false, None) => "no".to_owned(),
            };
            let acct = format!(
                "{}/{}/{}",
                m.accounts.warm, m.accounts.leased, m.accounts.walled
            );
            let elig = if m.eligible { "yes" } else { "no" };
            let conv = if m.converge_failures == 0 {
                "ok".to_owned()
            } else {
                format!("fail({})", m.converge_failures)
            };
            let seen = m
                .last_seen
                .map_or_else(|| "-".to_owned(), |s| s.to_string());
            let _ = writeln!(
                out,
                "{:<7} {:<15} {:<7} {:<9} {:<19} {:<9} {:<9} {}  {}  {}",
                m.label,
                ver,
                win,
                m.sessions,
                acct,
                elig,
                conv,
                seen,
                m.facts_at
                    .map_or_else(|| "-".to_owned(), |at| at.to_string()),
                m.fleet_name.as_deref().unwrap_or("-")
            );
        }
        if !self.stragglers.is_empty() {
            let _ = writeln!(out, "stragglers ({STRAGGLER_MONITOR_STATUS}):");
            for s in &self.stragglers {
                let _ = writeln!(
                    out,
                    "  {:<12} {:<24} ({})",
                    s.name,
                    s.host,
                    s.reason.as_str()
                );
            }
        }
        out
    }
}

#[cfg(unix)]
static CENSUS_CACHE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(unix)]
const CENSUS_FILE_MAX_BYTES: usize = 4 << 20;

#[cfg(unix)]
fn read_regular_nofollow_bounded(path: &Path, max_bytes: usize) -> Option<String> {
    use std::io::Read as _;
    let file = astream_sys::open_nonblocking(path, astream_sys::Links::Refuse).ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.len() > max_bytes as u64 {
        return None;
    }
    let mut bytes = Vec::with_capacity(
        usize::try_from(meta.len())
            .unwrap_or(0)
            .min(max_bytes)
            .saturating_add(1),
    );
    file.take((max_bytes as u64).saturating_add(1))
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() <= max_bytes)
        .then_some(bytes)
        .and_then(|b| String::from_utf8(b).ok())
}

/// Read `<state_dir>/swarm-census.json` when present and valid.
#[cfg(unix)]
#[must_use]
pub fn read_cached_census(state_dir: &Path) -> Option<SwarmCensus> {
    let _guard = CENSUS_CACHE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let bytes =
        aterm_uds::private_file::read_private(state_dir, CENSUS_CACHE_FILE, CENSUS_FILE_MAX_BYTES)
            .ok()?;
    let raw = String::from_utf8(bytes).ok()?;
    SwarmCensus::from_json(&raw).ok()
}

/// Persist `census` atomically to `<state_dir>/swarm-census.json` (`0600`).
///
/// # Errors
/// Returns an error if `<state_dir>` cannot be created or written.
#[cfg(unix)]
pub fn write_cached_census(state_dir: &Path, census: &SwarmCensus) -> Result<String, String> {
    let json = census.to_json();
    let _guard = CENSUS_CACHE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    aterm_uds::private_file::ensure_private_dir(state_dir).map_err(|e| e.to_string())?;
    aterm_uds::private_file::write_private(state_dir, CENSUS_CACHE_FILE, json.as_bytes())
        .map_err(|e| e.to_string())?;
    Ok(json)
}

/// This Mac's book as the census lists it ([`listed_book`]).
#[cfg(unix)]
struct ListedBook {
    /// The book's peers, with each node only the copy of its issuer's book names.
    peers: Vec<aterm_net::fleet_member::Peer>,
    /// The node of the issuer the book names, and the copy of its book.
    issuer: Option<String>,
    issuer_copies: Vec<aterm_net::fleet_member::Peer>,
    /// This Mac's own node identity.
    own: Option<aterm_net::fleet::NodeIdentity>,
}

/// This Mac's book (`state_dir`) as the census lists it, judged by the vault's `roster` and the
/// membership log `log`: each roster member's node id is bound by this book where the roster
/// names its key alone, and a peer or member the deny list or the log holds out is added to the
/// roster's deny list, which the census prunes stored survey facts for, and listed no more. A log
/// this Mac cannot read now ([`aterm_net::fleet_member::MemberKeys::unreadable`]) judges no key:
/// it lists no keyed peer but this Mac's own and adds no one to the deny list, so no survey
/// facts go for its sake.
#[cfg(unix)]
fn listed_book(
    state_dir: &Path,
    roster: &mut aterm_secrets::swarm_roster::SwarmRoster,
    log: Option<&aterm_net::fleet_member::MemberKeys>,
) -> ListedBook {
    use aterm_net::fleet_member::{Via, read_issuer_book, read_peers};

    let unread = log.is_some_and(|log| log.unread().is_some());
    let log = log.filter(|_| !unread);
    let mut raw_peers = read_peers(state_dir).unwrap_or_default();
    let issuer_node = raw_peers
        .iter()
        .find(|p| p.via == Via::Issuer)
        .and_then(|p| p.node.clone());
    let mut issuer_copies = issuer_node
        .as_deref()
        .map(|issuer| read_issuer_book(state_dir, issuer))
        .unwrap_or_default();
    for copy in &issuer_copies {
        if !raw_peers
            .iter()
            .any(|p| p.node == copy.node && p.node.is_some())
        {
            raw_peers.push(copy.clone());
        }
    }
    let own_identity = aterm_net::fleet_member::read_node_identity(state_dir)
        .ok()
        .flatten();
    let own_key = own_identity
        .as_ref()
        .map(aterm_net::fleet::NodeIdentity::key);
    for member in &mut roster.members {
        if member.node_id.is_none()
            && let Some(key_hex) = member.node_key.as_deref()
        {
            if let Some(ref own) = own_identity
                && own.key().hex().eq_ignore_ascii_case(key_hex)
            {
                member.node_id = Some(own.node().to_owned());
            } else if let Some(node) = raw_peers
                .iter()
                .find(|p| p.key.is_some_and(|k| k.hex().eq_ignore_ascii_case(key_hex)))
                .and_then(|p| p.node.clone())
            {
                member.node_id = Some(node);
            }
        }
    }
    let mut deny_nodes_changed = false;
    if let Some(log) = log {
        let mut extra_denied = Vec::new();
        for member in &roster.members {
            if let Some(key_hex) = member.node_key.as_deref()
                && let Some(key) = aterm_net::fleet::NodeKey::from_hex(key_hex.trim_ascii())
                && log.holds_out(&key)
            {
                extra_denied.push(key.hex());
                if let Some(node) = member.node_id.as_deref() {
                    extra_denied.push(node.trim_ascii().to_ascii_lowercase());
                }
            }
        }
        if !extra_denied.is_empty() {
            roster.deny_nodes.extend(extra_denied);
            deny_nodes_changed = true;
        }
    }
    if let Some(ref own) = own_identity
        && (aterm_net::fleet::is_denied_key(&own.key(), &roster.deny_nodes)
            || log.is_some_and(|l| l.holds_out(&own.key())))
    {
        roster.deny_nodes.push(own.node().to_ascii_lowercase());
        roster.deny_nodes.push(own.key().hex());
        deny_nodes_changed = true;
    }
    for peers in [&mut raw_peers, &mut issuer_copies] {
        peers.retain(|p| {
            let held_out = p.key.is_some_and(|k| {
                aterm_net::fleet::is_denied_key(&k, &roster.deny_nodes)
                    || log.is_some_and(|l| l.holds_out(&k))
            });
            if held_out {
                if let Some(node) = p.node.as_deref() {
                    roster.deny_nodes.push(node.to_ascii_lowercase());
                    deny_nodes_changed = true;
                }
                if let Some(key) = p.key {
                    roster.deny_nodes.push(key.hex());
                    deny_nodes_changed = true;
                }
            }
            let unjudged = unread && p.key.is_some_and(|k| own_key != Some(k));
            !held_out
                && !unjudged
                && !p
                    .node
                    .as_deref()
                    .is_some_and(|n| aterm_net::fleet::is_denied_node(n, &roster.deny_nodes))
        });
    }
    if deny_nodes_changed {
        roster.canonicalize();
        for peers in [&mut raw_peers, &mut issuer_copies] {
            peers.retain(|p| {
                !p.node
                    .as_deref()
                    .is_some_and(|n| aterm_net::fleet::is_denied_node(n, &roster.deny_nodes))
            });
        }
    }
    ListedBook {
        peers: raw_peers,
        issuer: issuer_node,
        issuer_copies,
        own: own_identity,
    }
}

/// Gather live [`CensusInputs`] on this Mac (`state_dir`, `home_dir`) at `now_s`.
///
/// # Errors
/// The roster or installed claims table is busy or cannot be read; the caller defers
/// publishing this pass.
#[cfg(unix)]
pub fn collect_local_census_inputs(
    state_dir: &Path,
    home_dir: Option<&Path>,
    bonjour_hosts: Vec<BonjourHostInput>,
    now_s: u64,
) -> Result<CensusInputs, String> {
    collect_local_census_inputs_until(
        state_dir,
        home_dir,
        bonjour_hosts,
        now_s,
        std::time::Instant::now() + std::time::Duration::from_millis(250),
    )
}

#[cfg(unix)]
fn collect_local_census_inputs_until(
    state_dir: &Path,
    home_dir: Option<&Path>,
    bonjour_hosts: Vec<BonjourHostInput>,
    now_s: u64,
    deadline: std::time::Instant,
) -> Result<CensusInputs, String> {
    use aterm_net::fleet_member::{Via, read_admitted_pushed_facts};

    let home_paths = home_dir.map(aterm_secrets::client::Paths::for_home);
    let read_roster = home_paths
        .as_ref()
        .map(|paths| {
            aterm_secrets::client::swarm_roster_until(paths, deadline)
                .map_err(|why| format!("swarm roster unread: {why}"))
        })
        .transpose()?
        .flatten();
    let config_path = home_dir.map(|h| h.join(".config/aterm/aterm.toml"));
    let steward = ai_pool::resolve_local_pool_identity(home_dir, config_path.as_deref(), None)?
        .map(|id| id.primary);

    let mut roster = aterm_secrets::swarm_roster::SwarmRoster::default();
    let mut log_keys = None;
    let mut server_heard = Vec::new();
    let mut pool = None;
    let mut commons_labels = BTreeSet::new();
    let mut commons_member_count = None;

    let mut has_roster = false;
    if let (Some(home), Some(paths)) = (home_dir, home_paths.as_ref()) {
        if let Some(r) = read_roster {
            has_roster = !r.members.is_empty();
            roster = r;
        } else if let Ok(Some(denied)) = aterm_secrets::client::swarm_denied_nodes_now(paths) {
            roster.deny_nodes.extend(denied);
            roster.canonicalize();
        }
        log_keys = crate::fleet_log::LocalLog::of(paths).keys();
        for (vault, subject, at) in aterm_secrets::server::read_last_heard(&paths.served()) {
            server_heard.push(ServerHeardFact { vault, subject, at });
        }
        let pool_path = home.join(".secrets").join(ai_pool::AI_POOL_FILE);
        if let Some(raw) = read_regular_nofollow_bounded(&pool_path, CENSUS_FILE_MAX_BYTES)
            && let Ok(parsed) = Pool::from_toml(&raw, &pool_path.display().to_string())
        {
            pool = Some(parsed);
        }
    }

    let ListedBook {
        peers: raw_peers,
        issuer: issuer_node,
        issuer_copies,
        own: own_identity,
    } = listed_book(state_dir, &mut roster, log_keys.as_deref());
    if let Some(ref paths) = home_paths
        && (paths.dir.join("commons").is_dir()
            || paths.records_data_file().is_ok_and(|path| path.is_file()))
        && let Ok(Some(bootstrap)) =
            aterm_secrets::commons::bootstrap::Bootstrap::cached_until(paths, deadline)
        && let Ok(Some(access)) = bootstrap.access()
    {
        let mut count = 0usize;
        let mut counted_recipients = BTreeSet::new();
        for member in access.members() {
            let rec = member.binding().recipient().to_bech32();
            if !counted_recipients.insert(rec.clone()) {
                continue;
            }
            if let Some(rm) = roster.member_by_recipient(&rec) {
                if roster.is_member_denied(rm) {
                    continue;
                }
                commons_labels.insert(rm.label.clone());
            } else if has_roster {
                continue;
            }
            count += 1;
        }
        commons_member_count = Some(count);
    }
    aterm_net::fleet_member::prune_pushed_facts(
        state_dir,
        &raw_peers,
        &roster.deny_nodes,
        log_keys.as_deref(),
    );
    let pushed_facts = read_admitted_pushed_facts(
        state_dir,
        &raw_peers,
        &roster.deny_nodes,
        log_keys.as_deref(),
    );
    let keyring = aterm_net::fleet_member::Keyring::of(raw_peers.iter().chain(&issuer_copies));
    let mut roster_bound_keys: BTreeMap<String, aterm_net::fleet::NodeKey> = BTreeMap::new();
    for member in &roster.members {
        if roster.is_member_denied(member) {
            continue;
        }
        if let (Some(node), Some(key_hex)) = (member.node_id.as_deref(), member.node_key.as_deref())
            && let Some(key) = aterm_net::fleet::NodeKey::from_hex(key_hex.trim_ascii())
        {
            roster_bound_keys
                .entry(node.trim_ascii().to_ascii_lowercase())
                .or_insert(key);
        }
    }
    let mut best_signed_names: BTreeMap<String, (u64, bool, Option<String>)> = BTreeMap::new();
    for peer in raw_peers.iter().chain(&issuer_copies) {
        if !peer.signature_holds() {
            continue;
        }
        let (Some(node), Some(key), Some(signed)) = (&peer.node, peer.key, &peer.signed) else {
            continue;
        };
        if keyring.bound(node) != Some(key) {
            continue;
        }
        if peer.via == Via::IssuerBook
            && issuer_node.as_deref() == Some(node.as_str())
            && !raw_peers.iter().any(|p| {
                p.via != Via::IssuerBook
                    && p.node.as_deref() == Some(node.as_str())
                    && p.key == Some(key)
            })
        {
            continue;
        }
        let node_lc = node.trim_ascii().to_ascii_lowercase();
        if let Some(roster_key) = roster_bound_keys.get(&node_lc)
            && *roster_key != key
        {
            continue;
        }
        let first_hand = peer.via != Via::IssuerBook;
        let rank = (signed.seq, first_hand);
        let candidate_name = peer.primary_name().map(str::to_owned);
        if best_signed_names
            .get(&node_lc)
            .is_none_or(|(prev_seq, prev_first_hand, _)| rank > (*prev_seq, *prev_first_hand))
        {
            best_signed_names.insert(node_lc, (signed.seq, first_hand, candidate_name));
        }
    }
    let mut fleet_names: BTreeMap<String, String> = best_signed_names
        .into_iter()
        .filter_map(|(node, (_, _, name))| Some((node, name?)))
        .collect();
    let own_node = aterm_net::fleet_member::read_node_id(state_dir)
        .ok()
        .flatten();
    let own_label_denied = steward.as_deref().is_some_and(|st| {
        let st_stem = canonical_host_stem(st);
        roster.is_label_denied(&st_stem)
            && !roster
                .members
                .iter()
                .any(|m| !roster.is_member_denied(m) && m.matches_label_or_alias(&st_stem))
    });
    let own_denied = own_node
        .as_deref()
        .is_some_and(|n| roster.is_node_denied(n))
        || own_identity.as_ref().is_some_and(|id| {
            roster.is_node_denied(&id.key().hex())
                || log_keys
                    .as_ref()
                    .is_some_and(|l| l.unread().is_none() && l.holds_out(&id.key()))
        })
        || own_label_denied;
    if let Some(own) = &own_identity {
        // The local name file is the source the node signs on its next heartbeat.
        // Its absence or local denial removes any stale self entry instead of retaining an old name.
        let own_lc = own.node().to_ascii_lowercase();
        fleet_names.remove(&own_lc);
        if !own_denied && let Ok(Some(name)) = aterm_net::fleet_member::read_primary_name(state_dir)
        {
            fleet_names.insert(own_lc, name);
        }
    }
    let mut fleet_book: Vec<FleetBookPeerInput> = raw_peers
        .into_iter()
        .map(|p| FleetBookPeerInput {
            node: p.node,
            host: p.host,
            seen_at: p.seen_ms.map(|ms| ms / 1_000),
        })
        .collect();
    if state_dir.join(aterm_net::fleet::KEY_FILE).is_file()
        && let Some(ref st) = steward
    {
        let st_stem = canonical_host_stem(st);
        let already_in_book = fleet_book.iter().any(|p| {
            (own_node.is_some() && p.node.as_deref() == own_node.as_deref())
                || canonical_host_stem(&p.host) == st_stem
        });
        if !own_denied && !already_in_book {
            fleet_book.push(FleetBookPeerInput {
                node: own_node,
                host: format!("{st_stem}.local"),
                seen_at: Some(now_s),
            });
        }
    }

    let mut heartbeats = Vec::new();
    for record in pushed_facts {
        let node = record.node;
        let payload = record.payload;
        if roster.is_node_denied(&node) {
            continue;
        }
        if let Ok(facts) = SwarmSurveyFacts::from_survey_reply(&payload) {
            if facts.label.as_deref().is_some_and(|lbl| {
                roster.is_label_denied(lbl)
                    && !roster
                        .members
                        .iter()
                        .any(|m| !roster.is_member_denied(m) && m.matches_label_or_alias(lbl))
            }) {
                continue;
            }
            let received_at = record
                .received_at
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs());
            heartbeats.push(HeartbeatInput {
                node,
                facts,
                received_at,
                converge_failures: 0,
            });
        }
    }

    let (claims_view, claims) = home_dir
        .map(claims::read_book)
        .transpose()?
        .flatten()
        .unwrap_or_default();

    Ok(CensusInputs {
        now_s,
        steward,
        roster,
        server_heard,
        fleet_book,
        fleet_names,
        heartbeats,
        commons_labels,
        commons_member_count,
        pool,
        claims,
        claims_view,
        bonjour_hosts,
    })
}

/// Recompute `<state_dir>/swarm-census.json` on this Mac and return it.
///
/// # Errors
/// The roster or installed claims table cannot be read, or persisting the census fails.
#[cfg(unix)]
pub fn refresh_local_census(
    state_dir: &Path,
    home_dir: Option<&Path>,
    bonjour_hosts: Vec<BonjourHostInput>,
    now_s: u64,
) -> Result<SwarmCensus, String> {
    let inputs = collect_local_census_inputs(state_dir, home_dir, bonjour_hosts, now_s)?;
    let census = SwarmCensus::compute(&inputs);
    write_cached_census(state_dir, &census)?;
    if let Some(home) = home_dir {
        let paths = aterm_secrets::client::Paths::for_home(home);
        // The census never fails over the gate's mark: the mark is sticky and the note
        // idempotent, so one this refresh could not write is written by the next.
        if paths.dir.join("commons").is_dir() {
            let _ = note_fold_gate(
                &paths,
                &census,
                &aterm_secrets::commons::fold::Gate::compiled(),
                now_s,
            );
        }
    }
    Ok(census)
}

/// Each census member as the fold's adoption gate hears it (docs/DESIGN-alab-one-mesh-2026-11-05.md
/// §4.2 M6): when it was last heard by any source the census joins (the vault's server rows,
/// heartbeats, the fleet book, a window, Bonjour) and the build its newest survey facts reported.
/// A member heard with no build reported is an older aterm's and keeps the gate shut. Every member
/// counts, the fleet's as well as the Commons': one counted that holds no seat can only keep the
/// gate shut.
#[must_use]
pub fn fold_seats(census: &SwarmCensus) -> Vec<aterm_secrets::commons::fold::Heard<'_>> {
    census
        .members
        .iter()
        .map(|member| aterm_secrets::commons::fold::Heard {
            last_heard_ms: member.last_seen.map(|at| at.saturating_mul(1000)),
            build: member.version.as_deref(),
        })
        .collect()
}

/// Feed the fold's adoption gate from a fresh census (§11 B11): the vault at `paths` turns the fold
/// on, stickily, once `gate` holds over [`fold_seats`] at `now_s`. Whether the fold is on.
///
/// # Errors
/// The gate's mark cannot be written.
#[cfg(unix)]
pub fn note_fold_gate(
    paths: &aterm_secrets::client::Paths,
    census: &SwarmCensus,
    gate: &aterm_secrets::commons::fold::Gate,
    now_s: u64,
) -> Result<bool, String> {
    aterm_secrets::commons::fold::note(paths, gate, &fold_seats(census), now_s.saturating_mul(1000))
        .map_err(|err| format!("fold gate: {err}"))
}

#[cfg(unix)]
fn cached_census_admitted(cached: &SwarmCensus, state_dir: &Path, home_dir: Option<&Path>) -> bool {
    let mut roster = aterm_secrets::swarm_roster::SwarmRoster::default();
    let mut log_keys = None;
    let home_paths = home_dir.map(aterm_secrets::client::Paths::for_home);
    if let Some(paths) = home_paths.as_ref() {
        let Ok(read) = crate::harness::ai_pool::read_pass_roster(paths) else {
            return false;
        };
        if let Some(r) = read {
            roster = r;
        } else if let Ok(Some(denied)) = aterm_secrets::client::swarm_denied_nodes_now(paths) {
            roster.deny_nodes.extend(denied);
            roster.canonicalize();
        }
        log_keys = crate::fleet_log::LocalLog::of(paths).keys();
    }

    let ListedBook {
        peers: raw_peers,
        own: own_identity,
        ..
    } = listed_book(state_dir, &mut roster, log_keys.as_deref());

    let admitted = aterm_net::fleet_member::all_pushed_facts_admitted(
        state_dir,
        &raw_peers,
        &roster.deny_nodes,
        log_keys.as_deref(),
    );
    if !admitted {
        aterm_net::fleet_member::prune_pushed_facts(
            state_dir,
            &raw_peers,
            &roster.deny_nodes,
            log_keys.as_deref(),
        );
        return false;
    }

    let local_primary_name = own_identity.as_ref().and_then(|_| {
        aterm_net::fleet_member::read_primary_name(state_dir)
            .ok()
            .flatten()
    });
    !cached.members.iter().any(|cm| {
        let is_own = own_identity.as_ref().is_some_and(|own| {
            cm.node
                .as_deref()
                .is_some_and(|n| n.eq_ignore_ascii_case(own.node()))
        });
        let has_active_match = roster.members.iter().any(|m| {
            !roster.is_member_denied(m)
                && (m.matches_label_or_alias(&cm.label)
                    || (cm.node.is_some() && m.node_id == cm.node))
        });
        (is_own && cm.fleet_name != local_primary_name)
            || cm.node.as_deref().is_some_and(|n| roster.is_node_denied(n))
            || ((roster.is_label_denied(&cm.label)
                || cm.aliases.iter().any(|a| roster.is_label_denied(a)))
                && !has_active_match)
            || (roster.members.iter().any(|m| {
                roster.is_member_denied(m)
                    && (m.matches_label_or_alias(&cm.label)
                        || (cm.node.is_some() && m.node_id == cm.node))
            }) && !has_active_match)
    })
}

/// Serve a `FleetOp::Census` request: returns the cached `<state_dir>/swarm-census.json` when
/// fresh (`<= CENSUS_REFRESH_S`), or recomputes and caches it on the spot. Every Mac computes the
/// census of its own book (C24; `docs/DESIGN-fluid-swarm-2026-10-28.md` §4.1, F17).
///
/// # Errors
/// The roster or installed claims table cannot be read, or caching the census fails.
#[cfg(unix)]
pub fn serve_fleet_census(
    state_dir: &Path,
    home_dir: Option<&Path>,
    now_s: u64,
) -> Result<String, String> {
    if let Some(cached) = read_cached_census(state_dir)
        && now_s
            .checked_sub(cached.generated_at)
            .is_some_and(|age| age < CENSUS_REFRESH_S)
        && cached_census_admitted(&cached, state_dir, home_dir)
    {
        return Ok(cached.to_json());
    }
    let inputs = collect_local_census_inputs(state_dir, home_dir, Vec::new(), now_s)?;
    let census = SwarmCensus::compute(&inputs);
    write_cached_census(state_dir, &census)
}

/// Read [`SwarmCensus`]: the cached one while it is fresher than `CENSUS_REFRESH_S` (unless
/// `force_refresh`), else the census of this Mac's own book, computed and cached here (C24;
/// `docs/DESIGN-fluid-swarm-2026-10-28.md` §4.1, F17: no Mac dials another for it).
///
/// # Errors
/// The roster or installed claims table cannot be read, or caching the census fails.
#[cfg(unix)]
pub fn read_or_compute_census(
    state_dir: &Path,
    home_dir: Option<&Path>,
    force_refresh: bool,
    now_s: u64,
) -> Result<SwarmCensus, String> {
    if !force_refresh
        && let Some(cached) = read_cached_census(state_dir)
        && now_s
            .checked_sub(cached.generated_at)
            .is_some_and(|age| age < CENSUS_REFRESH_S)
        && cached_census_admitted(&cached, state_dir, home_dir)
    {
        return Ok(cached);
    }
    refresh_local_census(state_dir, home_dir, Vec::new(), now_s)
}

#[cfg(test)]
#[path = "fleet_census_tests.rs"]
mod tests;
