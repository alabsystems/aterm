// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Client and approver bindings for `aterm-swarm-roster` and `aterm-swarm-policy`
//! (`docs/DESIGN-alab-swarm-2026-10-27.md` §3.2, §4.1, §4.6, §12.3 C11).

use super::*;
use crate::swarm_roster::{
    Newcomer, POLICY_ENTRY, ROSTER_ENTRY, SeedAdmission, SheetHints, SwarmPolicy, SwarmRoster,
    assign_newcomer, leading_m_label, seed_roster,
};

/// Read the verified `aterm-swarm-roster` from the locally applied vault copy, if present.
///
/// # Errors
/// Local state or the verified cache cannot be read safely, or `aterm-swarm-roster` is malformed.
pub fn swarm_roster(paths: &Paths) -> crate::Result<Option<SwarmRoster>> {
    let Some(expected) = config(paths)? else {
        return Ok(None);
    };
    if let Some(entry) = entry::read_reserved_entry(paths, &expected, ROSTER_ENTRY)? {
        let (bytes, _) = entry.into_parts();
        let text = std::str::from_utf8(bytes.as_slice())
            .map_err(|_| Error::new(format!("{ROSTER_ENTRY} is not UTF-8")))?;
        let mut roster = SwarmRoster::from_toml(text)?;
        if let Some(log) = log_entries::trusted_local_log(paths, &expected)? {
            overlay_log_membership(&log, &mut roster);
        }
        return Ok(Some(roster));
    }
    let state = load_state(paths)?;
    let log_only_bound = state.digest.is_none()
        && fsx::kind(&paths.cache_file())? == fsx::Kind::Missing
        && state.source_identity == Some(source_identity(&expected));
    let envelope_bound =
        source_bound(&expected, &state) && held_copy(paths, &expected, &state).is_some();
    if state.membership == Stored::In
        && !state.failed
        && state.pending.is_empty()
        && pin_admitted(&expected, None)
        && (log_only_bound || envelope_bound)
        && let Some(identity) = read_identity(paths)?
        && let Some(log) = log_entries::trusted_local_log(paths, &expected)?
        && !log.recipient_is_removed(&identity.recipient())
        && !log.audience_left_out_recipient(
            &crate::membership_log::EpochAudience::Everyone,
            &identity.recipient(),
        )
        && log.wraps_to(&identity.recipient())
    {
        let mut roster = SwarmRoster::default();
        overlay_log_membership(&log, &mut roster);
        if !roster.members.is_empty()
            || !roster.deny_labels.is_empty()
            || !roster.deny_nodes.is_empty()
        {
            return Ok(Some(roster));
        }
    }
    Ok(None)
}

fn overlay_log_membership(log: &crate::membership_log::Log, roster: &mut SwarmRoster) {
    let membership = log.membership();
    let mut removed_labels = BTreeSet::new();
    let mut kept = Vec::with_capacity(roster.members.len());
    for m in std::mem::take(&mut roster.members) {
        if Recipient::parse(&m.recipient).is_ok_and(|rec| {
            log.recipient_is_removed(&rec)
                || log.audience_left_out_recipient(
                    &crate::membership_log::EpochAudience::Everyone,
                    &rec,
                )
        }) {
            removed_labels.insert(m.label);
            for alias in m.aliases {
                removed_labels.insert(alias);
            }
            if let Some(claimed) = m.claimed_label {
                removed_labels.insert(claimed);
            }
            if let Some(node_id) = m.node_id {
                roster.deny_nodes.push(node_id);
            }
            if let Some(node_key) = m.node_key {
                roster.deny_nodes.push(node_key);
            }
        } else {
            kept.push(m);
        }
    }
    roster.members = kept;

    for label in membership.removed_labels() {
        if let Some((_, canon)) = leading_m_label(label) {
            removed_labels.insert(canon);
        }
    }
    for (nk, standing) in membership.nodes() {
        if matches!(standing, crate::membership_log::NodeStanding::Removed(_)) && *nk != [0; 32] {
            roster.deny_nodes.push(aterm_codec::hex::encode_lower(nk));
        }
    }

    for (actor, member) in membership.members() {
        if log.audience_left_out(&crate::membership_log::EpochAudience::Everyone, actor) {
            if let Some((_, canon)) =
                leading_m_label(&member.label).or_else(|| leading_m_label(&member.name))
            {
                removed_labels.insert(canon);
            }
            if let Some(nk) = member.node_key.filter(|k| *k != [0; 32]) {
                roster.deny_nodes.push(aterm_codec::hex::encode_lower(&nk));
            }
            continue;
        }
        let rec = member.binding.recipient().to_bech32();
        if let Some(existing) = roster.members.iter_mut().find(|m| m.recipient == rec) {
            if let Some(key) = member.node_key.filter(|k| *k != [0; 32]) {
                existing.node_key = Some(aterm_codec::hex::encode_lower(&key));
            }
            continue;
        }
        let Some((_, canonical)) =
            leading_m_label(&member.label).or_else(|| leading_m_label(&member.name))
        else {
            continue;
        };
        if roster.is_label_denied(&canonical) || roster.members.iter().any(|m| m.label == canonical)
        {
            continue;
        }
        let admitted_ms = log
            .get(&member.entry)
            .map_or(0, crate::membership_log::Record::authored_ms);
        let node_key = member
            .node_key
            .filter(|k| *k != [0; 32])
            .map(|k| aterm_codec::hex::encode_lower(&k));
        roster.members.push(crate::swarm_roster::SwarmMember {
            recipient: rec,
            vault_name: member.name.clone(),
            label: canonical,
            aliases: Vec::new(),
            hint: None,
            admitted_ms,
            claimed_label: None,
            claimed_at_ms: None,
            node_id: None,
            node_key,
        });
    }
    for lbl in removed_labels {
        if !roster.members.iter().any(|m| {
            m.matches_label_or_alias(&lbl) || m.claimed_label.as_deref() == Some(lbl.as_str())
        }) {
            roster.deny_labels.push(lbl);
        }
    }
    roster.canonicalize();
}

/// Read the verified `aterm-swarm-lease` `holder` label from the locally applied vault copy, if present.
///
/// # Errors
/// Local state or the verified cache cannot be read safely, or `aterm-swarm-lease` is malformed.
pub fn swarm_lease_holder(paths: &Paths) -> crate::Result<Option<String>> {
    let Some(expected) = config(paths)? else {
        return Ok(None);
    };
    let Some(entry) = entry::read_reserved_entry(paths, &expected, crate::SWARM_LEASE_ENTRY)?
    else {
        return Ok(None);
    };
    let (bytes, _) = entry.into_parts();
    let text = std::str::from_utf8(bytes.as_slice())
        .map_err(|_| Error::new(format!("{} is not UTF-8", crate::SWARM_LEASE_ENTRY)))?;
    let table: aterm_toml::Table = aterm_toml::from_str(text)
        .map_err(|e| Error::new(format!("invalid {}: {e}", crate::SWARM_LEASE_ENTRY)))?;
    Ok(table
        .get("holder")
        .and_then(aterm_toml::Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned))
}

/// Resolve this Mac's canonical `m<N>` swarm label (`DESIGN-alab-swarm` §3.2):
///
/// 1. When `aterm-swarm-roster` carries a row for this Mac's age recipient, return its `label`.
/// 2. Otherwise, when joined to the ALab vault (or a vault that already publishes
///    `aterm-swarm-roster` / `aterm-swarm-policy`), fall back to the vault-recorded name for this
///    Mac (if it starts with `m<N>`), then `source.toml`'s `host`, then `hostnames`
///    (LocalHostName / ComputerName / hostname) that **start with** `m<N>`.
/// 3. Personal vaults without `aterm-swarm-roster` return `Ok(None)`.
///
/// Never reads `~/.aterm/machine.toml`.
///
/// # Errors
/// Local vault state, identity, or a present `aterm-swarm-roster` cannot be read or parsed.
pub fn resolve_local_swarm_label(
    paths: &Paths,
    hostnames: &[String],
) -> crate::Result<Option<String>> {
    let Some(expected) = config(paths)? else {
        return Ok(None);
    };
    let Some(identity) = read_identity(paths)? else {
        return Ok(None);
    };
    let recipient = identity.recipient().to_bech32();
    let roster_opt = swarm_roster(paths)?;
    if let Some(roster) = &roster_opt
        && let Some(member) = roster.member_by_recipient(&recipient)
    {
        return Ok(Some(member.label.clone()));
    }
    if roster_opt.is_none() && !alab_vault(&expected) {
        return Ok(None);
    }

    let state = load_state(paths)?;
    let local_log = log_entries::trusted_local_log(paths, &expected)?;
    let log_only_bound = state.digest.is_none()
        && fsx::kind(&paths.cache_file())? == fsx::Kind::Missing
        && state.source_identity == Some(source_identity(&expected))
        && local_log.is_some();
    if state.membership != Stored::In
        || state.failed
        || (!source_bound(&expected, &state) && !log_only_bound)
    {
        return Ok(None);
    }

    if let Some(log) = &local_log {
        if log.recipient_is_removed(&identity.recipient())
            || log.audience_left_out_recipient(
                &crate::membership_log::EpochAudience::Everyone,
                &identity.recipient(),
            )
        {
            return Ok(None);
        }
        if let Some(member) = log
            .membership()
            .members()
            .values()
            .find(|m| m.binding.recipient() == identity.recipient())
            && let Some((_, canonical)) =
                leading_m_label(&member.label).or_else(|| leading_m_label(&member.name))
        {
            return Ok(Some(canonical));
        }
    }

    // Fallback 1: name recorded in the vault for this recipient (admin locker or single-host
    // locker, then `source.toml`'s recorded `host`), only when it starts with `m<N>`.
    if let Some(cached) = held_copy(paths, &expected, &state)
        && let Ok(envelope) = envelope::parse(&cached)
        && let Some(recorded) = vault_recorded_name(&envelope, &identity, &recipient)
        && let Some((_, canonical)) = leading_m_label(&recorded)
    {
        return Ok(Some(canonical));
    }
    if let Some((_, canonical)) = leading_m_label(&expected.host) {
        return Ok(Some(canonical));
    }

    // Fallback 2: a hostname that starts with `m<N>` (never mid-string tokens).
    for host in hostnames {
        if let Some((_, canonical)) = leading_m_label(host) {
            return Ok(Some(canonical));
        }
    }
    Ok(None)
}

fn vault_recorded_name(
    envelope: &Envelope,
    identity: &Identity,
    recipient: &str,
) -> Option<String> {
    let identities = std::slice::from_ref(identity);
    if let Some(admin_locker) = envelope.admin_locker()
        && let Ok(plain) = age::decrypt(identities, admin_locker.armored.as_bytes())
        && let Ok(parsed) = parse_admin(&plain)
        && let Some(machine) = parsed
            .doc
            .machines()
            .into_iter()
            .find(|m| m.recipient == recipient)
    {
        return Some(machine.name);
    }
    for locker in envelope
        .lockers
        .iter()
        .filter(|locker| locker.label != "admin")
    {
        if let Ok(plain) = age::decrypt(identities, locker.armored.as_bytes())
            && let Ok(entries) = text::parse_locker(plain.as_slice())
        {
            for entry in entries {
                if let Audience::Hosts(hosts) = entry.audience
                    && hosts.len() == 1
                {
                    return Some(hosts[0].clone());
                }
            }
        }
    }
    None
}

/// Seed `aterm-swarm-roster` from the vault's current members in admission-date order and publish
/// it in `[everyone]` (`DESIGN-alab-swarm` §3.2, §12.5 step 1).
///
/// Preserves any existing `deny_labels` and `deny_nodes` if `aterm-swarm-roster` was already present.
///
/// # Errors
/// This Mac is not an admin of a configured vault, an existing policy/roster is malformed, or the
/// publication fails.
pub fn admin_seed_swarm_roster(
    paths: &Paths,
    bonjour_hosts: &[String],
) -> crate::Result<SwarmRoster> {
    let admin = admin_open(paths)?;
    let mut doc = admin.doc.clone();
    let log = log_labels(paths);
    let roster = build_seeded_roster(&admin, &doc, bonjour_hosts, &log)?;
    doc.set_entry(
        &Audience::Everyone,
        ROSTER_ENTRY,
        roster.to_toml().as_bytes(),
    )
    .map_err(|e| Error::new(e.to_string()))?;
    admin_publish(paths, &admin, &doc)?;
    Ok(roster)
}

fn admissions_map(admin: &Admin, doc: &Document) -> BTreeMap<String, SeedAdmission> {
    let mut map = BTreeMap::new();
    for machine in doc.machines() {
        if let Ok(rec) = Recipient::parse(&machine.recipient)
            && let Some(ev) = admin.admissions.evidence(&rec)
        {
            map.insert(
                machine.recipient,
                SeedAdmission {
                    admitted_ms: ev.authored_ms,
                    sequence: ev.sequence,
                },
            );
        }
    }
    map
}

fn build_seeded_roster(
    admin: &Admin,
    doc: &Document,
    bonjour_hosts: &[String],
    log: &(BTreeMap<String, String>, Vec<String>),
) -> crate::Result<SwarmRoster> {
    let policy = SwarmPolicy::from_document(doc)?.unwrap_or_default();
    let sheet = SheetHints::from_document(doc);
    let prior = SwarmRoster::from_document(doc)?;
    let admissions = admissions_map(admin, doc);
    let mut all_hosts = bonjour_hosts.to_vec();
    all_hosts.extend(log.1.iter().cloned());
    let mut roster = seed_roster(doc, &admissions, &policy, &sheet, &all_hosts, &log.0);
    if let Some(existing) = prior {
        roster.merge_deny_lists(&existing);
    }
    Ok(roster)
}

fn swarm_roster_enabled(admin: &Admin, doc: &Document) -> bool {
    alab_vault(&admin.config)
        || doc.entry(&Audience::Everyone, ROSTER_ENTRY).is_some()
        || doc.entries().iter().any(|entry| entry.name == POLICY_ENTRY)
}

pub(super) fn load_or_seed_swarm_roster_with_log(
    admin: &Admin,
    doc: &Document,
    log: &(BTreeMap<String, String>, Vec<String>),
) -> crate::Result<SwarmRoster> {
    if let Some(existing) = SwarmRoster::from_document(doc)? {
        return Ok(existing);
    }
    let policy = SwarmPolicy::from_document(doc)?.unwrap_or_default();
    let sheet = SheetHints::from_document(doc);
    let admissions = admissions_map(admin, &admin.published_doc);
    Ok(seed_roster(
        &admin.published_doc,
        &admissions,
        &policy,
        &sheet,
        &log.1,
        &log.0,
    ))
}

/// The labels the membership log at `paths` holds (F15: the allowing member assigns `m<N>` in
/// its `Admit`): each member's by its age recipient, and every label any founder or `Admit`
/// claims. Empty where this Mac holds no log.
pub(super) fn log_labels(paths: &Paths) -> (BTreeMap<String, String>, Vec<String>) {
    let Ok(store) = load_records(paths) else {
        return (BTreeMap::new(), Vec::new());
    };
    let Some(log) = store.log() else {
        return (BTreeMap::new(), Vec::new());
    };
    let membership = log.membership();
    let by_recipient = membership
        .members()
        .values()
        .map(|member| (member.binding.recipient().to_bech32(), member.label.clone()))
        .collect();
    let all = membership.claimed_labels().iter().cloned().collect();
    (by_recipient, all)
}

/// Update `aterm-swarm-roster` in `doc` when the approver admits `newly_approved`
/// (`(recipient, requested_host, vault_name)`), as C11 under F15 has it: the allowing member
/// assigned the `m<N>` in its `Admit`, so the roster follows the membership log (`log`, from
/// [`log_labels`]).
///
/// - A Mac the log labels with a bare `m<N>` takes that label as given, the sheet's 24-hour hold
///   included (the person's Allow settled it), unless another row holds it, the deny-list names
///   it, or the policy reserves it; then it gets the roster's own label.
/// - A Mac the log does not label (F10's fallback, a reinstalled Mac, a code approval) never
///   takes an `m<N>` the log holds for another Mac: asking under one, it gets the next free one,
///   which counts the log's labels.
/// - A Mac whose log label carries a split's suffix (`m40-3fa2`) gets the next free one: the
///   roster holds bare `m<N>` labels only.
///
/// No-op on personal vaults that carry neither `aterm-swarm-roster` nor `aterm-swarm-policy`.
pub(super) fn maintain_swarm_roster_on_approve(
    admin: &Admin,
    doc: &mut Document,
    newly_approved: &[(String, String, String)],
    log: &(BTreeMap<String, String>, Vec<String>),
    now_ms: u64,
) -> crate::Result<()> {
    if newly_approved.is_empty() || !swarm_roster_enabled(admin, doc) {
        return Ok(());
    }
    let policy = SwarmPolicy::from_document(doc)?.unwrap_or_default();
    let sheet = SheetHints::from_document(doc);
    let mut roster = load_or_seed_swarm_roster_with_log(admin, doc, log)?;
    let (log_by_recipient, log_held) = log;
    let log_numbers: BTreeSet<String> = log_held
        .iter()
        .filter_map(|label| leading_m_label(label).map(|(_, canonical)| canonical))
        .collect();
    let held_by_log =
        |host: &str| leading_m_label(host).is_some_and(|(_, label)| log_numbers.contains(&label));
    for (recipient, requested_host, vault_name) in newly_approved {
        let fresh = !roster.members.iter().any(|row| &row.recipient == recipient);
        let logged = log_by_recipient
            .get(recipient)
            .filter(|label| crate::membership_log::is_label(label));
        let requested: &str = match (logged, log_by_recipient.get(recipient)) {
            (Some(label), _) => label,
            (None, Some(_)) => "",
            (None, None) if held_by_log(requested_host) => "",
            (None, None) => requested_host,
        };
        let assigned = assign_newcomer(
            &mut roster,
            doc,
            &policy,
            &sheet,
            log_held,
            Newcomer {
                recipient,
                requested_host: requested,
                vault_name,
                now_ms,
            },
        );
        let Some(label) = logged.filter(|label| fresh && **label != assigned) else {
            continue;
        };
        let held_elsewhere = roster.members.iter().any(|row| {
            &row.recipient != recipient && (row.label == *label || row.aliases.contains(label))
        });
        if held_elsewhere
            || roster.is_label_denied(label)
            || policy
                .effective_reserved(doc, Some(&roster))
                .contains(label)
        {
            continue;
        }
        if let Some(row) = roster
            .members
            .iter_mut()
            .find(|row| &row.recipient == recipient)
        {
            row.label.clone_from(label);
            row.claimed_label = None;
            row.claimed_at_ms = None;
            row.normalize_aliases();
        }
        roster.canonicalize();
    }
    doc.set_entry(
        &Audience::Everyone,
        ROSTER_ENTRY,
        roster.to_toml().as_bytes(),
    )
    .map_err(|e| Error::new(e.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Seeding a vault's `aterm-swarm-roster` never materializes a file in `~/.secrets`,
    /// `resolve_local_swarm_label` reads the member's assigned `m<N>` label (ignoring
    /// `~/.aterm/machine.toml` `id = "m22"`), and approving a newcomer named `m3` or
    /// `Mac-mini-M4` through `approve_request` assigns `next_free` rather than `m3` or `m4`.
    #[test]
    fn swarm_roster_stays_unmaterialized_and_approver_assigns_labels_end_to_end() {
        let root = aterm_tempfile::tempdir().unwrap();
        let server_dir = root.path().join("server");
        let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let running = crate::server::start(
            socket,
            &crate::server::Options {
                listen: "127.0.0.1:0".parse().unwrap(),
                store: server_dir,
                advertise: None,
                pins: Vec::new(),
                hold: Duration::from_millis(200),
                keep_awake: false,
            },
        )
        .unwrap();
        let url = format!("http://{}/v1/test", running.addr);

        let admin_paths = Paths::for_home(&root.path().join("admin"));
        let created = create(&admin_paths, &url, None, Some("m22")).unwrap();
        let pinned_source = format!("{url}#{}", created.signer);

        // Poison `~/.aterm/machine.toml` on the member to prove swarm identity never reads it!
        let member_m4_home = root.path().join("member-m4");
        std::fs::create_dir_all(member_m4_home.join(".aterm")).unwrap();
        std::fs::write(member_m4_home.join(".aterm/machine.toml"), "id = \"m22\"\n").unwrap();

        // Seed `aterm-swarm-roster` on the admin.
        let seeded = admin_seed_swarm_roster(&admin_paths, &[]).unwrap();
        assert_eq!(seeded.members.len(), 1);
        assert_eq!(seeded.members[0].label, "m22");
        assert!(
            !admin_paths
                .home
                .join(".secrets")
                .join(ROSTER_ENTRY)
                .exists(),
            "reserved entry must never materialize into ~/.secrets"
        );

        // Put `aterm-swarm-policy` and `fleet-accounts.toml` in the vault so `m110` is in the sheet.
        {
            let adm = admin_open(&admin_paths).unwrap();
            let mut doc = adm.doc.clone();
            let policy = SwarmPolicy {
                network_id: Some("test-net".to_owned()),
                reserved_labels: vec!["m3".to_owned(), "m22".to_owned()],
                release_hosts: vec!["m3".to_owned(), "m22".to_owned()],
                servers: Vec::new(),
                stewards: Vec::new(),
            };
            let admins = doc.admins();
            doc.set_entry(
                &Audience::Hosts(admins),
                POLICY_ENTRY,
                policy.to_toml().as_bytes(),
            )
            .unwrap();
            doc.set_entry(
                &Audience::Everyone,
                "fleet-accounts.toml",
                br#"
[[account]]
label = "acct-1"
machines = ["m110"]
class = "max"
email = "one@example.org"
"#,
            )
            .unwrap();
            doc.set_entry(
                &Audience::Everyone,
                crate::SWARM_LEASE_ENTRY,
                b"holder = \"m22\"\nepoch = 1\n",
            )
            .unwrap();
            doc.set_open(false).unwrap();
            admin_publish(&admin_paths, &adm, &doc).unwrap();
        }

        // Join two newcomers by code: one named `mac-mini-m4` (from `Mac-mini-M4`) and one named `m3`.
        let member_m4_paths = Paths::for_home(&member_m4_home);
        let m4_id = machine_identity(&member_m4_paths).unwrap();
        let m4_joined = join(
            &member_m4_paths,
            &JoinOptions {
                source: pinned_source.clone(),
                host: Some("mac-mini-m4".to_owned()),
                interactive: false,
                wait: Duration::ZERO,
                confirm: &|_| false,
                password: JoinPassword::Given(None),
                fetched: None,
                requested: &|| {},
                say: &|_| {},
            },
        )
        .unwrap();
        assert!(matches!(m4_joined, JoinOutcome::Pending { .. }));
        approve_request(&admin_paths, &m4_id.recipient().to_bech32()).unwrap();
        sync(&member_m4_paths, None).unwrap();

        let member_m3_paths = Paths::for_home(&root.path().join("member-m3"));
        let m3_id = machine_identity(&member_m3_paths).unwrap();
        let m3_joined = join(
            &member_m3_paths,
            &JoinOptions {
                source: pinned_source,
                host: Some("m3".to_owned()),
                interactive: false,
                wait: Duration::ZERO,
                confirm: &|_| false,
                password: JoinPassword::Given(None),
                fetched: None,
                requested: &|| {},
                say: &|_| {},
            },
        )
        .unwrap();
        assert!(matches!(m3_joined, JoinOutcome::Pending { .. }));
        approve_request(&admin_paths, &m3_id.recipient().to_bech32()).unwrap();
        sync(&member_m3_paths, None).unwrap();

        // Neither member has `aterm-swarm-roster` materialized as a file!
        assert!(
            !member_m4_paths
                .home
                .join(".secrets")
                .join(ROSTER_ENTRY)
                .exists()
        );
        assert!(
            !member_m3_paths
                .home
                .join(".secrets")
                .join(ROSTER_ENTRY)
                .exists()
        );

        // `Mac-mini-M4` (`mac-mini-m4`) got m111 (above sheet's m110), NOT m4 and NOT `~/.aterm/machine.toml`'s m22!
        let resolved_m4 =
            resolve_local_swarm_label(&member_m4_paths, &["Mac-mini-M4".to_owned()]).unwrap();
        assert_eq!(resolved_m4.as_deref(), Some("m111"));

        // Newcomer named `m3` got m112, NOT reserved `m3`!
        let resolved_m3 = resolve_local_swarm_label(&member_m3_paths, &["m3".to_owned()]).unwrap();
        assert_eq!(resolved_m3.as_deref(), Some("m112"));
        assert!(
            !member_m4_paths
                .home
                .join(".secrets")
                .join(crate::SWARM_LEASE_ENTRY)
                .exists()
        );
        assert_eq!(
            swarm_lease_holder(&member_m4_paths).unwrap().as_deref(),
            Some("m22")
        );
        running.stop();
    }

    #[test]
    fn overlay_log_membership_moves_log_removed_members_to_deny_labels_and_deny_nodes() {
        use crate::commons::binding::Request;
        use crate::membership_log::{Body, Entrant, Evidence, Log, Record};

        let mk_entrant = |tag: u8, label: &str| {
            let id = Identity::from_secret_bytes([tag ^ 0x55; 32]);
            let req = Request::create([2; 16], [3; 32], [tag; 16], [tag; 16], &id, &[tag; 32])
                .expect("request");
            (
                id,
                Entrant {
                    request: req,
                    node_key: [tag; 32],
                    name: format!("{label}.local"),
                    label: label.to_owned(),
                },
            )
        };

        let (m1_id, m1_ent) = mk_entrant(1, "m1");
        let (m2_id, m2_ent) = mk_entrant(2, "m2");
        let (_m7_id, m7_ent) = mk_entrant(7, "m7");
        let m1_actor = m1_ent.request.binding().actor();
        let m2_actor = m2_ent.request.binding().actor();
        let m7_actor = m7_ent.request.binding().actor();

        let genesis = Record::found(&[99; 32], vec![m1_ent, m2_ent], 1_000).expect("genesis");
        let genesis_id = *genesis.id();
        let mut log = Log::new(genesis).expect("log");

        let admit_m7 = log
            .draft(
                &m1_actor,
                2_000,
                Body::Admit {
                    entrant: m7_ent,
                    evidence: Evidence::Phrase,
                },
            )
            .sign(&[1; 32], [1; 16])
            .expect("sign admit");
        let admit_m7_id = *admit_m7.id();
        log.insert(admit_m7).expect("insert admit");

        let remove_m2 = log
            .draft(
                &m1_actor,
                3_000,
                Body::Remove {
                    member: m2_actor,
                    cutoff: genesis_id,
                },
            )
            .sign(&[1; 32], [1; 16])
            .expect("sign remove m2");
        log.insert(remove_m2).expect("insert remove m2");

        let remove_m7 = log
            .draft(
                &m1_actor,
                4_000,
                Body::Remove {
                    member: m7_actor,
                    cutoff: admit_m7_id,
                },
            )
            .sign(&[1; 32], [1; 16])
            .expect("sign remove m7");
        log.insert(remove_m7).expect("insert remove m7");

        // Removed m7 attempts to self-Admit (claiming another recipient and label "m41") and
        // rename itself to "m43.local"; neither un-removes m7's recipient nor poisons m41/m43 in
        // `roster.deny_labels`.
        let (_m41_id, mut m41_ent) = mk_entrant(41, "m41");
        let self_forged_req = Request::create(
            [2; 16],
            [3; 32],
            [7; 16],
            [7; 16],
            &Identity::from_secret_bytes([0x99; 32]),
            &[7; 32],
        )
        .expect("forged self request");
        m41_ent.request = self_forged_req;
        let bogus_self_admit = log
            .draft(
                &m7_actor,
                4_500,
                Body::Admit {
                    entrant: m41_ent,
                    evidence: Evidence::Phrase,
                },
            )
            .sign(&[7; 32], [7; 16])
            .expect("sign bogus self admit");
        log.insert(bogus_self_admit)
            .expect("insert bogus self admit");
        let bogus_name = log
            .draft(
                &m7_actor,
                4_600,
                Body::Name {
                    name: "m43.local".to_owned(),
                },
            )
            .sign(&[7; 32], [7; 16])
            .expect("sign bogus name");
        log.insert(bogus_name).expect("insert bogus name");

        let mut roster = SwarmRoster {
            members: vec![
                crate::swarm_roster::SwarmMember {
                    recipient: m1_id.recipient().to_bech32(),
                    vault_name: "m1".to_owned(),
                    label: "m1".to_owned(),
                    aliases: Vec::new(),
                    hint: None,
                    admitted_ms: 1_000,
                    claimed_label: None,
                    claimed_at_ms: None,
                    node_id: Some("11111111111111111111111111111111".to_owned()),
                    node_key: None,
                },
                crate::swarm_roster::SwarmMember {
                    recipient: m2_id.recipient().to_bech32(),
                    vault_name: "m2".to_owned(),
                    label: "m2".to_owned(),
                    aliases: vec!["m9".to_owned()],
                    hint: None,
                    admitted_ms: 1_000,
                    claimed_label: Some("m10".to_owned()),
                    claimed_at_ms: Some(1_000),
                    node_id: Some("22222222222222222222222222222222".to_owned()),
                    node_key: None,
                },
            ],
            deny_labels: Vec::new(),
            deny_nodes: Vec::new(),
        };
        overlay_log_membership(&log, &mut roster);

        assert_eq!(roster.members.len(), 1);
        assert_eq!(roster.members[0].label, "m1");
        assert_eq!(
            roster.deny_labels,
            vec![
                "m10".to_owned(),
                "m2".to_owned(),
                "m7".to_owned(),
                "m9".to_owned()
            ]
        );
        assert_eq!(
            roster.deny_nodes,
            vec![
                "0202020202020202020202020202020202020202020202020202020202020202".to_owned(),
                "0707070707070707070707070707070707070707070707070707070707070707".to_owned(),
                "22222222222222222222222222222222".to_owned(),
            ]
        );
    }
}
