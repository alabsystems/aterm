// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Authenticated ALab fleet-code distribution. The code is never materialized
//! as a secret file, logged, or included in an error.

use super::*;

/// Reserved vault entry holding the default ALab fleet's join code.
pub const FLEET_CODE_ENTRY: &str = "aterm-fleet-code";

/// Reserved `[everyone]` vault entry holding the swarm roster and its grow-only `deny_nodes`.
pub const SWARM_ROSTER_ENTRY: &str = "aterm-swarm-roster";

/// The signed ALab default fleet read from the locally applied vault copy: its
/// join code (never printed) and whether this machine holds that copy's admin
/// locker. Fleet sync grants an admin the default drive grant as it grants every
/// member (`docs/DESIGN-fluid-swarm-2026-10-28.md` F2).
pub struct AlabFleet {
    /// Join code from [`FLEET_CODE_ENTRY`].
    pub code: SecretBytes,
    /// `true` when this machine's vault identity opens the envelope's `admin`
    /// locker.
    pub admin: bool,
}

impl std::fmt::Debug for AlabFleet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AlabFleet")
            .field("admin", &self.admin)
            .finish_non_exhaustive()
    }
}

/// Read the current signed ALab fleet code and whether this machine holds the
/// vault's admin locker after the local apply completed. Open-admitted ALab
/// members qualify under the owner's 2026-10-26 default full-membership policy.
/// Other vaults never supply the default fleet. `None`: the applied copy
/// carries no code for this machine (none is published, or the machine is not
/// yet a member it reaches).
///
/// # Errors
/// Local state, the signed cache or its recipient or admin lockers cannot be
/// read safely, or the apply has not finished (a pass applying now, or the last
/// one leaving an entry failed or pending), so the code is read once it has.
pub fn alab_fleet(paths: &Paths) -> crate::Result<Option<AlabFleet>> {
    let expected =
        config(paths)?.ok_or_else(|| Error::new("no vault configured; run aterm secrets join"))?;
    if !alab_vault(&expected) {
        return Ok(None);
    }
    reserved_fleet(paths, &expected)
}

/// Read an applied fixture's default fleet without requiring the compiled ALab key.
/// Production always uses [`alab_fleet`]; this seam is absent from release builds.
#[cfg(any(test, debug_assertions))]
pub fn test_alab_fleet(paths: &Paths) -> crate::Result<Option<AlabFleet>> {
    let expected = config(paths)?.ok_or_else(|| Error::new("no fixture vault configured"))?;
    reserved_fleet(paths, &expected)
}

/// Read the current signed ALab fleet code after the local apply completed.
/// Open-admitted ALab members qualify under the owner's 2026-10-26 default
/// full-membership policy. Other vaults never supply the default fleet.
/// `None`: the applied copy carries no code for this machine (none is
/// published, or the machine is not yet a member it reaches).
///
/// # Errors
/// Local state, the signed cache or its recipient or admin lockers cannot be
/// read safely, or the apply has not finished (a pass applying now, or the last
/// one leaving an entry failed or pending), so the code is read once it has.
pub fn alab_fleet_code(paths: &Paths) -> crate::Result<Option<SecretBytes>> {
    Ok(alab_fleet(paths)?.map(|fleet| fleet.code))
}

/// [`alab_fleet`]'s read of `expected`'s applied copy, any vault's: an apply
/// that has not finished is an error naming it, never read as no code.
pub(super) fn reserved_fleet(paths: &Paths, expected: &Config) -> crate::Result<Option<AlabFleet>> {
    if steward::local_recipient_denied_in_roster(paths) {
        return Ok(None);
    }
    if let Some((entry, admin)) =
        entry::read_reserved_entry_and_admin(paths, expected, FLEET_CODE_ENTRY)?
    {
        return Ok(Some(AlabFleet {
            code: entry.into_parts().0,
            admin,
        }));
    }
    let busy = StateLock::observe(paths)?.is_none()
        && fsx::kind(&paths.lock_file())? != fsx::Kind::Missing;
    if busy {
        return Err(Error::new(
            "a vault pass is applying on this machine now: the default fleet code is read once \
             it ends",
        ));
    }
    let state = load_state(paths)?;
    if state.membership == Stored::In && (state.failed || !state.pending.is_empty()) {
        return Err(Error::new(
            "this machine's last vault apply did not finish (`aterm secrets sync` names what \
             failed): the default fleet code is read once it does",
        ));
    }
    Ok(None)
}

/// [`alab_fleet_code`]'s read of `expected`'s applied copy, any vault's: an
/// apply that has not finished is an error naming it, never read as no code.
#[cfg(test)]
pub(super) fn reserved_code(
    paths: &Paths,
    expected: &Config,
) -> crate::Result<Option<SecretBytes>> {
    Ok(reserved_fleet(paths, expected)?.map(|fleet| fleet.code))
}

/// Publish the code of the fleet this Mac minted to every current and future ALab
/// vault member, including open-admitted machines. This changes no fleet or
/// drive setting on the publisher.
///
/// # Errors
/// The caller is not an ALab vault admin, or publication fails.
pub fn admin_publish_alab_fleet(paths: &Paths, code: &SecretBytes) -> crate::Result<Published> {
    let admin = alab_admin(paths)?;
    if code.is_empty() {
        return Err(Error::new("the fleet join code is empty"));
    }
    let doc = with_code(&admin.doc, code)?;
    admin_publish(paths, &admin, &doc)
}

/// The admin of the ALab vault, for a publish of the default fleet's code.
fn alab_admin(paths: &Paths) -> crate::Result<Admin> {
    let admin = admin_open(paths)?;
    if !alab_vault(&admin.config) {
        return Err(Error::new(
            "only an ALab vault admin can publish the default fleet",
        ));
    }
    Ok(admin)
}

/// `doc` with `code` as the default fleet's, for every member.
fn with_code(doc: &Document, code: &SecretBytes) -> crate::Result<Document> {
    let mut doc = doc.clone();
    doc.set_entry(&Audience::Everyone, FLEET_CODE_ENTRY, code.as_slice())
        .map_err(|error| Error::new(error.to_string()))?;
    Ok(doc)
}

/// Parse the top-level `deny_nodes` array from an `aterm-swarm-roster` TOML document,
/// returning sorted, deduplicated, lowercase node identifiers.
///
/// # Errors
/// `text` is not valid TOML, or `deny_nodes` is present and not an array of strings.
pub fn parse_denied_nodes_toml(text: &str) -> crate::Result<Vec<String>> {
    let table: aterm_toml::Table = aterm_toml::from_str(text)
        .map_err(|error| Error::new(format!("invalid {SWARM_ROSTER_ENTRY}: {error}")))?;
    let Some(raw) = table.get("deny_nodes") else {
        return Ok(Vec::new());
    };
    let items = raw.as_array().ok_or_else(|| {
        Error::new(format!(
            "{SWARM_ROSTER_ENTRY}: `deny_nodes` must be an array"
        ))
    })?;
    let mut set = BTreeSet::new();
    for item in items {
        let s = item.as_str().ok_or_else(|| {
            Error::new(format!(
                "{SWARM_ROSTER_ENTRY}: `deny_nodes` must contain only strings"
            ))
        })?;
        let trimmed = s.trim_ascii().to_ascii_lowercase();
        if !trimmed.is_empty() {
            set.insert(trimmed);
        }
    }
    Ok(set.into_iter().collect())
}

/// Read the verified swarm roster's `deny_nodes` from the locally applied vault copy.
///
/// Returns:
/// - `Ok(Some(nodes))` when an authoritative answer is available (`nodes` is empty when no
///   signed vault is configured, this machine has not joined yet, or the applied vault carries
///   no `aterm-swarm-roster` entry).
/// - `Ok(None)` when the local vault state is mid-apply, locked, partial, or revoked, so a
///   listener retains its last authoritative deny-list rather than clearing it transiently.
///
/// # Errors
/// Local vault files cannot be read, or `aterm-swarm-roster` is malformed UTF-8 / TOML.
pub fn swarm_denied_nodes(paths: &Paths) -> crate::Result<Option<Vec<String>>> {
    let Some(expected) = config(paths)? else {
        return Ok(Some(Vec::new()));
    };
    if expected.signer.is_none() {
        return Ok(Some(Vec::new()));
    }
    if let Some(entry) = entry::read_reserved_entry(paths, &expected, SWARM_ROSTER_ENTRY)? {
        let (bytes, _) = entry.into_parts();
        let text = std::str::from_utf8(bytes.as_slice())
            .map_err(|_| Error::new(format!("{SWARM_ROSTER_ENTRY} is not UTF-8")))?;
        let mut denied = parse_denied_nodes_toml(text)?;
        let mut parsed_roster = crate::swarm_roster::SwarmRoster::from_toml(text).ok();
        let local_log = log_entries::trusted_local_log(paths, &expected)
            .ok()
            .flatten();
        // A Mac the log let back in (Allow again, a quorum Undo): the deny rows its removal wrote
        // no longer hold for it (`roster::restored_by_log`).
        if let Some(log) = &local_log {
            let (_, mut nodes) = super::roster::restored_by_log(log);
            if let Some(roster) = parsed_roster.as_mut() {
                let _ = super::roster::lift_restored_from_roster(log, roster);
                let remaining_deny_nodes: BTreeSet<String> = roster
                    .deny_nodes
                    .iter()
                    .map(|n| n.trim().to_ascii_lowercase())
                    .collect();
                denied.retain(|node| {
                    remaining_deny_nodes.contains(&node.trim().to_ascii_lowercase())
                });
            } else {
                denied.retain(|node| !nodes.contains(&node.trim().to_ascii_lowercase()));
            }
            for m in parsed_roster.iter().flat_map(|r| &r.members) {
                if !steward::member_is_denied_in_roster(parsed_roster.as_ref().expect("some"), m) {
                    if let Some(node_id) = &m.node_id {
                        nodes.insert(node_id.trim().to_ascii_lowercase());
                    }
                    if let Some(node_key) = &m.node_key {
                        nodes.insert(node_key.trim().to_ascii_lowercase());
                    }
                }
            }
            denied.retain(|node| !nodes.contains(&node.trim().to_ascii_lowercase()));
        }
        if let Some(roster) = &parsed_roster {
            for member in &roster.members {
                let removed_in_log = local_log.as_ref().is_some_and(|log| {
                    Recipient::parse(&member.recipient).is_ok_and(|rec| {
                        log.recipient_is_removed(&rec)
                            || log.audience_left_out_recipient(
                                &crate::membership_log::EpochAudience::Everyone,
                                &rec,
                            )
                    })
                });
                if removed_in_log || steward::member_is_denied_in_roster(roster, member) {
                    for raw in [&member.node_id, &member.node_key].into_iter().flatten() {
                        let trimmed = raw.trim_ascii().to_ascii_lowercase();
                        if !trimmed.is_empty() {
                            denied.push(trimmed);
                        }
                    }
                }
            }
        }
        if let Some(log) = &local_log {
            extend_denied_nodes_from_log(log, parsed_roster.as_ref(), &mut denied);
        }
        denied.sort();
        denied.dedup();
        return Ok(Some(denied));
    }
    let busy = StateLock::observe(paths)?.is_none()
        && fsx::kind(&paths.lock_file())? != fsx::Kind::Missing;
    if busy {
        return Ok(None);
    }
    let state = load_state(paths)?;
    match state.membership {
        Stored::None | Stored::Pending | Stored::Plain => Ok(Some(Vec::new())),
        Stored::Revoked => Ok(None),
        Stored::In => {
            let Some(identity) = read_identity(paths)? else {
                return Ok(None);
            };
            if state.failed || !state.pending.is_empty() || !pin_admitted(&expected, None) {
                return Ok(None);
            }
            let local_log = log_entries::trusted_local_log(paths, &expected)?;
            if local_log.as_ref().is_some_and(|log| {
                log.recipient_is_removed(&identity.recipient())
                    || log.audience_left_out_recipient(
                        &crate::membership_log::EpochAudience::Everyone,
                        &identity.recipient(),
                    )
            }) {
                return Ok(None);
            }
            if (!source_bound(&expected, &state) || held_copy(paths, &expected, &state).is_none())
                && !log_only_intact(paths, &expected, &state)
            {
                return Ok(None);
            }
            let mut denied = Vec::new();
            if let Some(log) = &local_log {
                extend_denied_nodes_from_log(log, None, &mut denied);
                denied.sort();
                denied.dedup();
            }
            Ok(Some(denied))
        }
    }
}

fn extend_denied_nodes_from_log(
    log: &crate::membership_log::Log,
    roster: Option<&crate::swarm_roster::SwarmRoster>,
    denied: &mut Vec<String>,
) {
    let membership = log.membership();
    for (nk, standing) in membership.nodes() {
        if matches!(standing, crate::membership_log::NodeStanding::Removed(_)) && *nk != [0; 32] {
            denied.push(aterm_codec::hex::encode_lower(nk));
        }
    }
    for (actor, member) in membership.members() {
        let roster_denied = roster.is_some_and(|r| log_entries::member_denied_by_roster(r, member));
        if (log.audience_left_out(&crate::membership_log::EpochAudience::Everyone, actor)
            || roster_denied)
            && let Some(nk) = member.node_key.filter(|k| *k != [0; 32])
        {
            denied.push(aterm_codec::hex::encode_lower(&nk));
        }
    }
    for founder in membership.unproved() {
        let roster_denied = roster
            .is_some_and(|r| r.is_label_denied(&founder.label) || r.is_label_denied(&founder.name));
        if (log.audience_left_out_recipient(
            &crate::membership_log::EpochAudience::Everyone,
            &founder.recipient,
        ) || roster_denied)
            && let Some(nk) = founder.node_key.filter(|k| *k != [0; 32])
        {
            denied.push(aterm_codec::hex::encode_lower(&nk));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_denied_nodes_normalizes_and_rejects_bad_types() {
        assert_eq!(parse_denied_nodes_toml("").unwrap(), Vec::<String>::new());
        assert_eq!(
            parse_denied_nodes_toml(
                "deny_labels = [\"m9\"]\ndeny_nodes = [\" DEADBEEF \", \"0102\", \"deadbeef\", \"\"]\n"
            )
            .unwrap(),
            vec!["0102".to_string(), "deadbeef".to_string()]
        );
        assert!(parse_denied_nodes_toml("deny_nodes = 7\n").is_err());
        assert!(parse_denied_nodes_toml("deny_nodes = [7]\n").is_err());
    }

    #[test]
    fn swarm_denied_nodes_preserves_cache_signal_while_locked() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let fresh = Paths::for_home(&root.path().join("fresh"));
        assert_eq!(swarm_denied_nodes(&fresh).unwrap(), Some(Vec::new()));

        let vault_path = root.path().join("swarm.vault");
        let admin_paths = Paths::for_home(&root.path().join("admin"));
        create(&admin_paths, vault_path.to_str().unwrap(), None, Some("m1")).expect("create");
        assert_eq!(swarm_denied_nodes(&admin_paths).unwrap(), Some(Vec::new()));

        let opened = admin_open(&admin_paths).expect("admin open");
        let mut doc = opened.doc.clone();
        let m9_recipient = Identity::from_secret_bytes([0x99; 32])
            .recipient()
            .to_bech32();
        let roster_toml = format!(
            "deny_labels = [\"m9\"]\ndeny_nodes = [\"aa11bb22cc33dd44\", \"ff00\"]\n\n[[member]]\nrecipient = \"{m9_recipient}\"\nvault_name = \"m9\"\nlabel = \"m9\"\nadmitted_ms = 1\nnode_id = \"1122334455667788\"\n"
        );
        doc.set_entry(
            &Audience::Everyone,
            SWARM_ROSTER_ENTRY,
            roster_toml.as_bytes(),
        )
        .expect("set roster");
        admin_publish(&admin_paths, &opened, &doc).expect("publish");
        sync(&admin_paths, None).expect("sync");
        assert_eq!(
            swarm_denied_nodes(&admin_paths).unwrap(),
            Some(vec![
                "1122334455667788".to_string(),
                "aa11bb22cc33dd44".to_string(),
                "ff00".to_string()
            ])
        );

        let _lock = StateLock::take(&admin_paths).expect("hold state lock");
        assert_eq!(
            swarm_denied_nodes(&admin_paths).unwrap(),
            None,
            "mid-apply lock returns None so callers retain their cached deny-list"
        );
    }
}
