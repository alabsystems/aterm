// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Multi-server vault ledger publishing, parent-digest compare-and-swap, deterministic diverged
//! head merge, grow-only revocations, and read-back verification before retiring queued admissions
//! (`docs/DESIGN-alab-swarm-2026-10-27.md` §3.4, §8.3, §12.3 C7).

use super::*;
use crate::swarm_roster::{
    Newcomer, POLICY_ENTRY, SheetHints, SwarmLease, SwarmPolicy, SwarmRoster, assign_newcomer,
};

const SERVER_HEAD_DEADLINE: Duration = Duration::from_secs(5);
const READ_BACK_DEADLINE: Duration = Duration::from_secs(10);

/// Build a bounded, deduplicated parent-digest spine (`[immediate_heads..., ancestors...]`, capped
/// at [`envelope::MAX_PARENTS`]) so stateless single-envelope servers can verify ancestry across
/// both direct CAS writes, multi-head merges, and subscriber catch-up `PutBack`s while rejecting
/// any sibling fork or split-brain island (`HeadDescendsFromPrior`).
#[must_use]
pub fn extend_parent_spine(
    immediate_heads: &[[u8; 32]],
    ancestor_lists: &[&[[u8; 32]]],
) -> Vec<[u8; 32]> {
    let mut out = Vec::new();
    for head in immediate_heads {
        if out.len() >= envelope::MAX_PARENTS {
            return out;
        }
        if !out.contains(head) {
            out.push(*head);
        }
    }
    for list in ancestor_lists {
        for ancestor in *list {
            if out.len() >= envelope::MAX_PARENTS {
                return out;
            }
            if !out.contains(ancestor) {
                out.push(*ancestor);
            }
        }
    }
    out
}

/// Verified vault head read from one server (`read_server_heads`).
pub struct ObservedHead {
    /// Server URL (or file path) that served this head.
    pub url: String,
    /// Parsed and signature-verified envelope.
    pub envelope: Envelope,
    /// Raw signed envelope bytes.
    pub raw: Vec<u8>,
    /// Decrypted admin document.
    pub doc: Document,
    admissions: super::admission::Ledger,
    pub(crate) seed: [u8; 32],
    pub(crate) commons_seed: [u8; 32],
    pub(crate) commons_bound: bool,
}

impl Drop for ObservedHead {
    fn drop(&mut self) {
        zeroize::Zeroize::zeroize(&mut self.seed);
        zeroize::Zeroize::zeroize(&mut self.commons_seed);
    }
}

impl std::fmt::Debug for ObservedHead {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ObservedHead")
            .field("url", &self.url)
            .field("seq", &self.envelope.seq)
            .finish_non_exhaustive()
    }
}

/// Deterministic merge output across one or more verified [`ObservedHead`]s (`merge_diverged_heads`).
pub struct MergedHeads {
    /// Merged sequence (`max_seq + 1`).
    pub seq: u64,
    /// Parent digests naming every distinct observed head followed by bounded ancestors.
    pub parents: Vec<[u8; 32]>,
    /// Merged admin document with grow-only revocations, unioned admissions, and highest lease epoch.
    pub doc: Document,
    /// Handoff chain from the primary head.
    pub handoff: Vec<([u8; 32], [u8; 64])>,
    pub(super) admissions: super::admission::Ledger,
    pub(crate) seed: [u8; 32],
    pub(crate) commons_seed: [u8; 32],
    pub(crate) commons_bound: bool,
    /// True when more than one distinct head digest was present across `heads`.
    pub diverged: bool,
}

impl Drop for MergedHeads {
    fn drop(&mut self) {
        zeroize::Zeroize::zeroize(&mut self.seed);
        zeroize::Zeroize::zeroize(&mut self.commons_seed);
    }
}

impl std::fmt::Debug for MergedHeads {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MergedHeads")
            .field("seq", &self.seq)
            .field("parents_len", &self.parents.len())
            .field("diverged", &self.diverged)
            .finish_non_exhaustive()
    }
}

pub(super) fn push_policy_server_urls(
    policy: &SwarmPolicy,
    vault_name: &str,
    mut push: impl FnMut(&str),
) {
    for srv in &policy.servers {
        if srv.starts_with("http://") || srv.starts_with("https://") {
            if srv.contains("/v1/") {
                push(srv);
            } else {
                push(&format!("{}/v1/{vault_name}", srv.trim_end_matches('/')));
            }
        } else {
            push(&format!(
                "http://{srv}.local:{}/v1/{vault_name}",
                crate::DEFAULT_PORT
            ));
        }
    }
}

fn push_unique_endpoint(urls: &mut Vec<String>, raw: &str) {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return;
    }
    let endpoint = transport::describe(trimmed);
    if !urls
        .iter()
        .any(|existing| transport::describe(existing).eq_ignore_ascii_case(&endpoint))
    {
        urls.push(trimmed.to_owned());
    }
}

/// Candidate server URLs to PUT a freshly published envelope onto (`docs/DESIGN-fluid-swarm-2026-10-28.md`
/// F6): `config.source` first, then `aterm-swarm-policy`'s `servers` in both the prior and new
/// documents, `live_alab_peer_urls` (browsed `_alab-vault._tcp` peers and ALab defaults), and any
/// explicit `extra_urls`.
pub(super) fn publish_candidate_urls(
    paths: &Paths,
    config: &Config,
    old_doc: &Document,
    new_doc: &Document,
    extra_urls: &[String],
    found: Option<super::Found<'_>>,
) -> Vec<String> {
    let Source::Url(primary) = &config.source else {
        return Vec::new();
    };
    let mut urls = Vec::new();
    push_unique_endpoint(&mut urls, primary);
    let mut push = |raw: &str| {
        let base = raw.split_once('#').map_or(raw, |(b, _)| b).trim();
        let Ok((Source::Url(clean), None)) = transport::parse_source(base, Path::new("/")) else {
            return;
        };
        if transport::vault_name(&Source::Url(clean.clone()))
            .ok()
            .as_deref()
            != Some(config.name.as_str())
        {
            return;
        }
        push_unique_endpoint(&mut urls, &clean);
    };
    for doc in [old_doc, new_doc] {
        if let Ok(Some(policy)) = SwarmPolicy::from_document(doc) {
            push_policy_server_urls(&policy, &config.name, &mut push);
        }
    }
    for url in super::live_alab_peer_urls(paths, config, found) {
        push(&url);
    }
    for extra in extra_urls {
        push(extra);
    }
    urls
}

pub(super) fn candidate_server_urls(
    paths: &Paths,
    config: &Config,
    extra_urls: &[String],
) -> Vec<String> {
    let mut urls = Vec::new();
    if let Source::Url(url) = &config.source {
        push_unique_endpoint(&mut urls, url);
    }
    if let Ok(Some(entry)) = entry::read_reserved_entry(paths, config, POLICY_ENTRY)
        && let Ok(text) = std::str::from_utf8(entry.value().as_slice())
        && let Ok(policy) = SwarmPolicy::from_toml(text)
    {
        push_policy_server_urls(&policy, &config.name, |u| {
            push_unique_endpoint(&mut urls, u)
        });
    }
    #[cfg(not(test))]
    if super::alab_vault(config) {
        for &default_url in crate::ALAB_DEFAULT_URLS {
            push_unique_endpoint(&mut urls, default_url);
        }
        if let Ok(discovered) = crate::discover::browse_alab(super::PROBE_DEADLINE) {
            for item in discovered {
                push_unique_endpoint(&mut urls, &item.url);
            }
        }
    }
    for extra in extra_urls {
        push_unique_endpoint(&mut urls, extra);
    }
    urls
}

fn decode_verified_head(
    url_label: String,
    bytes: Vec<u8>,
    config: &Config,
    pin: &[u8; 32],
    identity: &Identity,
) -> crate::Result<ObservedHead> {
    let (envelope, _, _) =
        envelope::verify_for_reader(&bytes, &config.name, pin, &config.followed, None)?;
    let locker = envelope
        .admin_locker()
        .ok_or_else(|| Error::new("served envelope has no admin locker"))?;
    let plain = age::decrypt(std::slice::from_ref(identity), locker.armored.as_bytes())
        .map_err(|e| Error::new(format!("decrypting admin locker from {url_label}: {e}")))?;
    let opened = parse_admin(&plain)?;
    opened.admissions.validate(&opened.doc, envelope.seq)?;
    if envelope::signer_public(&opened.seed)? != envelope.signer {
        return Err(Error::new(
            "the admin locker's key does not match the envelope's signer",
        ));
    }
    Ok(ObservedHead {
        url: url_label,
        envelope,
        raw: bytes,
        doc: opened.doc.clone(),
        admissions: opened.admissions.clone(),
        seed: opened.seed,
        commons_seed: opened.commons_seed,
        commons_bound: opened.commons_bound,
    })
}

/// Read and verify the current head from every reachable server in `paths`'s config, policy
/// `servers` list, and `extra_urls`. Forged or unverified answers are ignored (`SeqMonotonic`).
///
/// # Errors
/// Vault configuration or local admin identity is missing or unreadable.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "LedgerPublish",
        action = "MemberSync",
        project = "aterm_secrets::client::ledger_publish::tests::ledger_publish_transition"
    )
)]
pub fn read_server_heads(paths: &Paths, extra_urls: &[String]) -> crate::Result<Vec<ObservedHead>> {
    let config = super::config(paths)?.ok_or_else(|| Error::new("no vault configured"))?;
    let pin = config
        .signer
        .ok_or_else(|| Error::new("a plain text vault has no signed server heads"))?;
    let identity = machine_identity(paths)?;
    let mut out = Vec::new();

    if let Source::Path(path) = &config.source
        && extra_urls.is_empty()
    {
        if let Ok(Fetched::Body(bytes)) =
            transport::fetch(&config.source, None, SERVER_HEAD_DEADLINE)
            && let Ok(head) =
                decode_verified_head(path.display().to_string(), bytes, &config, &pin, &identity)
        {
            out.push(head);
        }
        return Ok(out);
    }

    for url in candidate_server_urls(paths, &config, extra_urls) {
        if let Ok(Fetched::Body(bytes)) =
            transport::fetch_within(&url, None, servers_within(SERVER_HEAD_DEADLINE))
            && let Ok(head) = decode_verified_head(url, bytes, &config, &pin, &identity)
        {
            out.push(head);
        }
    }
    Ok(out)
}

/// Give `doc` the first of `branches`' `aterm-swarm-policy` entries, audience and all, when it
/// has none of its own ([`merge_diverged_heads`]).
fn keep_policy<'a>(
    doc: &mut Document,
    branches: impl IntoIterator<Item = &'a Document>,
) -> crate::Result<()> {
    let policy = |doc: &Document| {
        doc.entries()
            .into_iter()
            .find(|entry| entry.name == POLICY_ENTRY)
    };
    if policy(doc).is_some() {
        return Ok(());
    }
    if let Some(entry) = branches.into_iter().find_map(policy) {
        doc.set_entry(&entry.audience, POLICY_ENTRY, entry.value.as_slice())
            .map_err(|e| Error::new(e.to_string()))?;
    }
    Ok(())
}

fn head_merge_order(heads: &[ObservedHead]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..heads.len()).collect();
    order.sort_by(|&a, &b| {
        let ha = &heads[a].envelope;
        let hb = &heads[b].envelope;
        hb.seq
            .cmp(&ha.seq)
            .then_with(|| hb.digest().cmp(&ha.digest()))
    });
    order
}

/// Whether the merge of diverged heads settles entry `name` by a rule of its own
/// ([`reconcile_merged_metadata`], Commons control's reconciliation): the swarm policy, roster
/// and lease, and Commons control. A conflict on one in the membership log is the rule's to
/// settle, not a person's.
pub(super) fn ruled_by_merge(name: &str) -> bool {
    matches!(
        name,
        POLICY_ENTRY | crate::SWARM_ROSTER_ENTRY | crate::SWARM_LEASE_ENTRY | crate::commons::ENTRY
    )
}

/// Reconcile `aterm-swarm-policy`, `aterm-swarm-roster`, half-done revocations, and
/// `aterm-swarm-lease` across `doc` and all `heads` (used both by [`merge_diverged_heads`] and
/// after overlaying membership-log entries onto a merged document).
pub(super) fn reconcile_merged_metadata(
    doc: &mut Document,
    heads: &[ObservedHead],
) -> crate::Result<()> {
    if heads.is_empty() {
        return Ok(());
    }
    let order = head_merge_order(heads);
    let primary = &heads[order[0]];
    let primary_recipients: BTreeSet<String> = primary
        .doc
        .machines()
        .into_iter()
        .map(|m| m.recipient)
        .collect();
    let mut revoked_recipients: BTreeSet<String> = doc.code_only().into_iter().collect();
    for &idx in &order {
        for r in heads[idx].doc.code_only() {
            revoked_recipients.insert(r);
        }
    }
    let newly_admitted: Vec<(String, String)> = doc
        .machines()
        .into_iter()
        .filter(|m| {
            !revoked_recipients.contains(&m.recipient) && !primary_recipients.contains(&m.recipient)
        })
        .map(|m| (m.recipient, m.name))
        .collect();

    // The policy the labels below are assigned under.
    keep_policy(doc, order.iter().map(|&idx| &heads[idx].doc))?;

    // Merge `aterm-swarm-roster`: grow-only union of `deny_labels` + `deny_nodes`, prune
    // revoked members, and deterministically assign labels for unioned admissions.
    let mut any_roster = false;
    let mut merged_roster = SwarmRoster::default();
    let cur_doc = doc.clone();
    for branch_doc in std::iter::once(&cur_doc).chain(order.iter().map(|&idx| &heads[idx].doc)) {
        if let Some(r) = SwarmRoster::from_document(branch_doc)? {
            if !any_roster {
                merged_roster = r;
                any_roster = true;
            } else {
                merged_roster.deny_labels.extend(r.deny_labels);
                merged_roster.deny_nodes.extend(r.deny_nodes);
                for member in r.members {
                    if !revoked_recipients.contains(&member.recipient)
                        && !merged_roster.deny_labels.contains(&member.label)
                        && member
                            .node_id
                            .as_ref()
                            .is_none_or(|n| !merged_roster.deny_nodes.contains(n))
                        && member
                            .node_key
                            .as_ref()
                            .is_none_or(|n| !merged_roster.deny_nodes.contains(n))
                    {
                        if let Some(existing) = merged_roster
                            .members
                            .iter_mut()
                            .find(|m| m.recipient == member.recipient)
                        {
                            if existing.node_key.is_none() {
                                existing.node_key = member.node_key;
                            }
                            if existing.node_id.is_none() {
                                existing.node_id = member.node_id;
                            }
                        } else {
                            merged_roster.members.push(member);
                        }
                    }
                }
            }
        }
    }
    if any_roster {
        merged_roster.canonicalize();
        let candidates = std::mem::take(&mut merged_roster.members);
        merged_roster.members = candidates
            .into_iter()
            .filter(|m| {
                !revoked_recipients.contains(&m.recipient)
                    && !merged_roster.is_label_denied(&m.label)
                    && !m.aliases.iter().any(|a| merged_roster.is_label_denied(a))
                    && !merged_roster.is_label_denied(&m.vault_name)
                    && m.node_id
                        .as_deref()
                        .is_none_or(|n| !merged_roster.is_node_denied(n))
                    && m.node_key
                        .as_deref()
                        .is_none_or(|n| !merged_roster.is_node_denied(n))
            })
            .collect();
        // Ensure deterministic unique labels across merged branches (`§3.4 rule 5`: recompute
        // conflicting labels rather than allowing two approvers to hand out one label).
        let policy = SwarmPolicy::from_document(doc)?.unwrap_or_default();
        let sheet = SheetHints::from_document(doc);
        let mut seen_labels = BTreeSet::new();
        let mut deduped_members = Vec::new();
        for member in merged_roster.members.clone() {
            if seen_labels.insert(member.label.clone()) {
                deduped_members.push(member);
            } else {
                let mut temp = SwarmRoster {
                    members: deduped_members.clone(),
                    deny_labels: merged_roster.deny_labels.clone(),
                    deny_nodes: merged_roster.deny_nodes.clone(),
                };
                let new_label = assign_newcomer(
                    &mut temp,
                    doc,
                    &policy,
                    &sheet,
                    &[],
                    Newcomer {
                        recipient: &member.recipient,
                        requested_host: &member.vault_name,
                        vault_name: &member.vault_name,
                        now_ms: member.admitted_ms,
                    },
                );
                if let Some(reassigned) = temp
                    .members
                    .iter_mut()
                    .find(|m| m.recipient == member.recipient)
                {
                    if reassigned.node_id.is_none() {
                        reassigned.node_id = member.node_id;
                    }
                    if reassigned.node_key.is_none() {
                        reassigned.node_key = member.node_key;
                    }
                    if reassigned.hint.is_none() {
                        reassigned.hint = member.hint;
                    }
                }
                seen_labels.insert(new_label);
                deduped_members = temp.members;
            }
        }
        merged_roster.members = deduped_members;
        for (recipient, vault_name) in &newly_admitted {
            if merged_roster.member_by_recipient(recipient).is_none()
                && !merged_roster.is_label_denied(vault_name)
            {
                assign_newcomer(
                    &mut merged_roster,
                    doc,
                    &policy,
                    &sheet,
                    &[],
                    Newcomer {
                        recipient,
                        requested_host: vault_name,
                        vault_name,
                        now_ms: aterm_time::unix_millis().unwrap_or(0),
                    },
                );
            }
        }
        doc.set_entry(
            &Audience::Everyone,
            crate::SWARM_ROSTER_ENTRY,
            merged_roster.to_toml().as_bytes(),
        )
        .map_err(|e| Error::new(e.to_string()))?;
    }

    // Preserve the highest `aterm-swarm-lease` epoch across all branches.
    let cur_for_lease = doc.clone();
    let mut best_lease: Option<SwarmLease> = None;
    for branch_doc in
        std::iter::once(&cur_for_lease).chain(order.iter().map(|&idx| &heads[idx].doc))
    {
        if let Some(lease) = SwarmLease::from_document(branch_doc)? {
            let take = match &best_lease {
                None => true,
                Some(cur) => {
                    (lease.epoch, lease.until, &lease.holder) > (cur.epoch, cur.until, &cur.holder)
                }
            };
            if take {
                best_lease = Some(lease);
            }
        }
    }
    if let Some(lease) = best_lease {
        let admin_hosts = doc.admins();
        if !admin_hosts.is_empty() {
            doc.set_entry(
                &Audience::Hosts(admin_hosts),
                crate::SWARM_LEASE_ENTRY,
                lease.to_toml().as_bytes(),
            )
            .map_err(|e| Error::new(e.to_string()))?;
        }
    }

    // Complete any half-done revoke cascade (e.g. account-pool warm slots or steward lease of a
    // Mac revoked on one branch while reallocated or renewed on another).
    let _ = retirement::repair_half_done_revokes(doc)?;
    Ok(())
}

/// Deterministically merge one or more verified [`ObservedHead`]s into a single child at
/// `max_seq + 1` naming every distinct head digest in `parents` (`DESIGN-alab-swarm` §3.4):
///
/// - **Grow-only revocations (`RevocationsGrowOnly`)**: `code-only` recipients and
///   `aterm-swarm-roster`'s `deny_labels` / `deny_nodes` are merged by set union across ALL
///   branches, and any machine revoked on ANY branch is removed from `[machines]`, the roster, and
///   the account pool even when a higher-seq island still carried it.
/// - **Admissions**: unrevoked machines admitted on any branch are unioned by recipient and
///   assigned deterministic labels over the merged roster.
/// - **Swarm policy**: the primary's `aterm-swarm-policy`, else the first other branch's in merge
///   order, so a policy (its server list among it) set on one island survives whichever digest
///   wins.
/// - **Steward lease (`SingleCurrentLeaseEpoch`)**: the highest `(epoch, until, holder)`
///   `aterm-swarm-lease` across all branches is preserved.
///
/// # Errors
/// `heads` is empty, sequence arithmetic overflows, branch roster, policy, lease or admission
/// metadata cannot be combined, or preserving approved Commons control access fails.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "LedgerPublish",
        action = "MergeDivergedHeads",
        project = "aterm_secrets::client::ledger_publish::tests::ledger_publish_transition"
    )
)]
pub fn merge_diverged_heads(heads: &[ObservedHead]) -> crate::Result<MergedHeads> {
    if heads.is_empty() {
        return Err(Error::new("cannot merge an empty set of server heads"));
    }
    let order = head_merge_order(heads);
    let primary = &heads[order[0]];
    let seq = envelope::next_seq(primary.envelope.seq)?;

    let mut distinct_digests = Vec::new();
    let mut ancestor_slices: Vec<&[[u8; 32]]> = Vec::new();
    for &idx in &order {
        let digest = heads[idx].envelope.digest();
        if !distinct_digests.contains(&digest) {
            distinct_digests.push(digest);
        }
        ancestor_slices.push(heads[idx].envelope.parents.as_slice());
    }
    let diverged = distinct_digests.len() > 1;
    let parents = extend_parent_spine(&distinct_digests, &ancestor_slices);

    let mut doc = primary.doc.clone();

    // 1. Grow-only union of `code-only` revocations across every branch.
    let mut revoked_recipients = BTreeSet::new();
    for &idx in &order {
        for r in heads[idx].doc.code_only() {
            revoked_recipients.insert(r);
        }
    }
    for r in &revoked_recipients {
        doc.add_code_only(r)
            .map_err(|e| Error::new(e.to_string()))?;
    }

    // Remove any non-admin machine whose recipient was revoked on ANY branch.
    let admins: BTreeSet<String> = doc.admins().into_iter().collect();
    let to_remove: Vec<String> = doc
        .machines()
        .into_iter()
        .filter(|m| revoked_recipients.contains(&m.recipient) && !admins.contains(&m.name))
        .map(|m| m.name)
        .collect();
    for name in &to_remove {
        doc.remove_machine(name, true)
            .map_err(|e| Error::new(e.to_string()))?;
    }

    // 2. Union unrevoked machines admitted on other branches.
    for &idx in &order[1..] {
        for machine in heads[idx].doc.machines() {
            if revoked_recipients.contains(&machine.recipient) {
                continue;
            }
            if doc
                .machines()
                .iter()
                .any(|m| m.recipient == machine.recipient)
            {
                continue;
            }
            let assigned_name = super::free_name(&doc, &machine.name);
            doc.add_machine(&assigned_name, &machine.recipient)
                .map_err(|e| Error::new(e.to_string()))?;
        }
    }

    // 2b. Union non-reserved entries added on other branches that are absent on the primary head.
    let recipient_to_merged_name: BTreeMap<String, String> = doc
        .machines()
        .into_iter()
        .map(|m| (m.recipient, m.name))
        .collect();
    for &idx in &order[1..] {
        let branch_name_to_recipient: BTreeMap<String, String> = heads[idx]
            .doc
            .machines()
            .into_iter()
            .map(|m| (m.name, m.recipient))
            .collect();
        for want_anchored in [false, true] {
            for entry in heads[idx].doc.entries() {
                if entry.anchor.is_some() != want_anchored {
                    continue;
                }
                if ruled_by_merge(&entry.name) {
                    continue;
                }
                let mapped_audience = match &entry.audience {
                    Audience::Everyone => Audience::Everyone,
                    Audience::Hosts(hosts) => {
                        let mapped_hosts: Vec<String> = hosts
                            .iter()
                            .filter_map(|h| branch_name_to_recipient.get(h))
                            .filter(|r| !revoked_recipients.contains(*r))
                            .filter_map(|r| recipient_to_merged_name.get(r).cloned())
                            .collect();
                        if mapped_hosts.is_empty() {
                            continue;
                        }
                        Audience::Hosts(mapped_hosts)
                    }
                };
                let already_present = match &mapped_audience {
                    Audience::Everyone => doc.entry(&Audience::Everyone, &entry.name).is_some(),
                    Audience::Hosts(mapped_hosts) => doc.entries().iter().any(|existing| {
                        existing.name == entry.name
                            && matches!(
                                &existing.audience,
                                Audience::Hosts(other)
                                    if other.iter().any(|h| mapped_hosts.contains(h))
                            )
                    }),
                };
                if already_present {
                    continue;
                }
                doc.set_entry_with(
                    &mapped_audience,
                    &entry.name,
                    entry.value.as_slice(),
                    entry.if_absent,
                    entry.anchor.as_deref(),
                )
                .map_err(|e| Error::new(e.to_string()))?;
            }
        }
    }

    // 3–4. Reconcile policy, swarm roster, half-done revocations, and steward lease.
    reconcile_merged_metadata(&mut doc, heads)?;

    if let Some(noted) = super::note_code_only(&primary.doc, primary.envelope.seq, &doc, seq)? {
        doc = noted;
    }

    let admissions =
        super::admission::Ledger::merged(&doc, order.iter().map(|&idx| &heads[idx].admissions))?;
    // A first approval can arrive from another branch even when the primary
    // already knows the machine's row. Recipients approved on the primary keep
    // its current audience, including exclusions. Preserved access requires both
    // evidence and explicit audience inclusion on one authenticated branch.
    let mut eligible = BTreeSet::new();
    for head in heads {
        let eligibility = crate::commons::membership::Eligibility::from_document(&head.doc);
        for machine in head.doc.machines() {
            if let Ok(recipient) = Recipient::parse(&machine.recipient)
                && head.admissions.evidence(&recipient).is_some()
                && eligibility.includes(&machine.name, recipient)
            {
                eligible.insert(machine.recipient);
            }
        }
    }
    let preserved: Vec<_> = doc
        .machines()
        .into_iter()
        .filter(|machine| eligible.contains(&machine.recipient))
        .filter_map(|machine| {
            let recipient = Recipient::parse(&machine.recipient).ok()?;
            if primary.admissions.evidence(&recipient).is_some() {
                return None;
            }
            let evidence = admissions.evidence(&recipient)?;
            Some((machine.recipient, evidence.method))
        })
        .collect();
    let doc = super::enrollment::include_approved(&doc, &preserved)?.unwrap_or(doc);
    Ok(MergedHeads {
        seq,
        parents,
        doc,
        handoff: primary.envelope.handoff.clone(),
        admissions,
        seed: primary.seed,
        commons_seed: primary.commons_seed,
        commons_bound: primary.commons_bound,
        diverged,
    })
}

pub(super) fn leader_subsumes_heads(heads: &[ObservedHead], merged: &MergedHeads) -> bool {
    if heads.is_empty() {
        return false;
    }
    let order = head_merge_order(heads);
    let leader = &heads[order[0]];
    let leader_digest = leader.envelope.digest();
    for h in heads {
        let d = h.envelope.digest();
        if d != leader_digest {
            if h.envelope.seq >= leader.envelope.seq {
                return false;
            }
            if !leader.envelope.parents.is_empty() && !leader.envelope.parents.contains(&d) {
                return false;
            }
        }
    }
    leader.doc.code_only() == merged.doc.code_only()
        && leader.doc.machines() == merged.doc.machines()
        && leader.doc.entries() == merged.doc.entries()
        && leader.admissions == merged.admissions
}

pub(super) fn publish_to_servers_for_admin(
    paths: &Paths,
    expected: &Config,
    urls: &[String],
    bytes: &[u8],
) -> crate::Result<Vec<(String, crate::Result<()>)>> {
    // Publishes from this Mac PUT one at a time, so every server hears two of them in the order
    // they began (`publish_lock`). The state lock, which every secrets command on this Mac takes,
    // covers only the check that the configuration is still the one `bytes` were sealed for and
    // goes before any PUT: held across them, a server that takes a PUT and never answers held
    // every such command for the PUT's exchange deadline.
    let _publishing = super::publish_lock(paths)?;
    drop(super::configured_admin(paths, expected)?);
    // Every PUT runs at once. Each one has its own exchange deadline, and the list holds up to
    // `discover::MAX_CANDIDATES` browsed servers that nothing has verified. Run one after
    // another, a server that accepts and never answers would hold the publish for one deadline
    // per server. Run at once, it holds it for one deadline, as the single PUT to
    // `config.source` did before F6.
    let put = |url: &String| transport::publish(&Source::Url(url.clone()), bytes);
    let results = std::thread::scope(|scope| {
        let handles: Vec<_> = urls
            .iter()
            .map(|url| {
                std::thread::Builder::new()
                    .name("vault-publish".to_owned())
                    .spawn_scoped(scope, move || put(url))
            })
            .collect();
        urls.iter()
            .zip(handles)
            .map(|(url, handle)| {
                let res = match handle {
                    Ok(handle) => handle
                        .join()
                        .unwrap_or_else(|_| Err(Error::new("a publishing thread panicked"))),
                    // No thread to spare: this one PUTs once the others are under way.
                    Err(_) => put(url),
                };
                (url.clone(), res)
            })
            .collect()
    });
    Ok(results)
}

/// PUT `bytes` to every server URL in `urls` (holding `paths`'s publish lock, and its state lock
/// only to check that the vault configuration is unchanged) and return each server's result.
///
/// # Errors
/// Local vault configuration or admin lock cannot be acquired.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "LedgerPublish",
        action = "RevokeOnS1",
        project = "aterm_secrets::client::ledger_publish::tests::ledger_publish_transition"
    )
)]
pub fn publish_to_servers(
    paths: &Paths,
    urls: &[String],
    bytes: &[u8],
) -> crate::Result<Vec<(String, crate::Result<()>)>> {
    let config = super::config(paths)?.ok_or_else(|| Error::new("no vault configured"))?;
    publish_to_servers_for_admin(paths, &config, urls, bytes)
}

/// Read back the served head from `admin.config.source` after an admission publish and verify that
/// `served.seq >= expected_seq` and every recipient in `approved_recipients` is recorded in the
/// served document (or `code-only` if concurrently revoked), before retiring the queued request
/// (`QueuedAdmissionPublished`, `DESIGN-alab-swarm` §3.4 rule 6).
///
/// # Errors
/// The source returns no verified envelope at `>= expected_seq`, its admin locker cannot be
/// decrypted, or any approved recipient is absent from the served head.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "LedgerPublish",
        action = "ApproveAndReadBack",
        project = "aterm_secrets::client::ledger_publish::tests::ledger_publish_transition"
    )
)]
pub fn verify_approved_in_served_head(
    paths: &Paths,
    admin: &Admin,
    expected_seq: u64,
    approved_recipients: &[String],
) -> crate::Result<()> {
    if approved_recipients.is_empty() {
        return Ok(());
    }
    let pin = admin
        .config
        .signer
        .ok_or_else(|| Error::new("vault has no pinned signer for read-back"))?;
    let served_bytes = match transport::fetch(&admin.config.source, None, READ_BACK_DEADLINE)? {
        Fetched::Body(bytes) => bytes,
        Fetched::Missing | Fetched::NoNewer(_) => {
            return Err(Error::new(
                "read-back after approval found no served envelope",
            ));
        }
    };
    let (served_env, _, _) = envelope::verify_for_reader(
        &served_bytes,
        &admin.config.name,
        &pin,
        &admin.config.followed,
        None,
    )?;
    if served_env.seq < expected_seq {
        return Err(Error::new(format!(
            "read-back after approval saw seq {}, below published seq {expected_seq}",
            served_env.seq
        )));
    }
    let identity = machine_identity(paths)?;
    let locker = served_env
        .admin_locker()
        .ok_or_else(|| Error::new("read-back envelope has no admin locker"))?;
    let plain = age::decrypt(std::slice::from_ref(&identity), locker.armored.as_bytes())
        .map_err(|e| Error::new(format!("read-back admin locker: {e}")))?;
    let opened = parse_admin(&plain)?;
    let machines = opened.doc.machines();
    let revoked: BTreeSet<String> = opened.doc.code_only().into_iter().collect();
    for recipient in approved_recipients {
        let present =
            machines.iter().any(|m| &m.recipient == recipient) || revoked.contains(recipient);
        if !present {
            return Err(Error::new(
                "read-back after approval did not find the approved recipient in the served head",
            ));
        }
    }
    Ok(())
}

/// Host for `LedgerPublish`'s environment and negative-control (`Buggy`) waiver markers so
/// `spec_xref_closure` accounts for every model action explicitly.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "LedgerPublish",
        action = "Partition",
        reason = "Adversarial network split between vault servers; environment step"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "LedgerPublish",
        action = "Heal",
        reason = "Network partition recovery; environment step"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "LedgerPublish",
        action = "IslandAdvanceS2",
        reason = "Isolated island advancement during a network split; environment step"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "LedgerPublish",
        action = "QueueAdmission",
        reason = "External newcomer posting a join request; environment step"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "LedgerPublish",
        action = "ExpireLease",
        reason = "Wall-clock passage past lease `until + margin`; environment step"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "LedgerPublish",
        action = "AcceptLowerSeq",
        reason = "Buggy=1 negative control only; server `takes_over` and client `Floor` reject lower seq (`SeqMonotonic`)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "LedgerPublish",
        action = "ClobberWithoutParentCas",
        reason = "Buggy=2 negative control only; server `takes_over` enforces parent-digest CAS (`HeadDescendsFromPrior`)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "LedgerPublish",
        action = "MergeDropsRevocation",
        reason = "Buggy=3 negative control only; `merge_diverged_heads` unions `code_only` and deny-lists (`RevocationsGrowOnly`)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "LedgerPublish",
        action = "ApproveSkipsReadBack",
        reason = "Buggy=4 negative control only; `verify_approved_in_served_head` gates retiring queued admissions (`QueuedAdmissionPublished`)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "LedgerPublish",
        action = "UncasedLeaseClaim",
        reason = "Buggy=5 negative control only; `steward_ensure_lease` publishes via parent-CAS (`SingleCurrentLeaseEpoch`)"
    )
)]
#[doc(hidden)]
pub fn ledger_publish_environment() {}

#[cfg(test)]
#[path = "ledger_publish_tests.rs"]
pub(crate) mod tests;
