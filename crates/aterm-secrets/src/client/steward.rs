// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The ALab steward (owner rulings 2026-10-27): from the converge pass, the Mac that holds the
//! ALab vault's admin key finishes a revoke cascade left half done (`sync.steward.revoke`) and
//! runs the swarm lease's step (`sync.steward.lease`). Only the ALab vault ([`alab_vault`]) and
//! only an admin: a personal vault is never stewarded, and a member's pass reads that it is no
//! steward and writes nothing. The vault server step (`sync.steward.server.standby`) is any
//! active ALab member's whose vault is at a URL. ALab Commons is founded by `sync.shared-data`
//! ([`commons_foundable`]), on an open vault's only admin. A fleet's first secret is no
//! steward's: a person's `aterm fleet init` and `aterm fleet publish` mint and publish it
//! (`sync.steward.fleet` retired, `docs/DESIGN-fluid-swarm-2026-10-28.md` §4.1, F17).
//!
//! The read opens the admin locker, which is age-encrypted to this Mac's identity file, so it
//! reaches no keychain and raises no dialog.

use super::*;

/// Whether this Mac is the ALab steward ([`stewardship`]), read from the vault copy it applied
/// last.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stewardship {
    /// It is not: why (no vault, a vault not ALab's, this Mac's vault sync turned off by the
    /// person, no admin key here, no copy applied yet).
    No(String),
    /// A vault pass is applying on this Mac now, so its copy is not read: why. The next pass
    /// reads it once that one ends.
    Busy(String),
    /// It holds the ALab vault's admin key.
    Steward {
        /// The vault publishes the default fleet's code ([`fleet::FLEET_CODE_ENTRY`]).
        fleet_code: bool,
    },
}

/// Read, writing nothing and fetching nothing, whether this Mac is the ALab steward and what the
/// vault it applied last carries: the admin text of the local signed copy, decrypted with this
/// Mac's identity file where its admin locker is addressed to it. A Mac whose person turned its
/// vault sync off (`aterm secrets agent off`) is no steward while that record stands.
///
/// # Errors
/// A vault pass is applying on this Mac now, or the local state, the signed copy or the admin
/// locker cannot be read.
pub fn stewardship(paths: &Paths) -> crate::Result<Stewardship> {
    let no = |why: &str| Ok(Stewardship::No(why.to_owned()));
    let Some(config) = config(paths)? else {
        return no("no vault is configured");
    };
    if !alab_vault(&config) {
        return no("the configured vault is not ALab's, and no other vault is stewarded");
    }
    // A steward's publish ends in a sync of this Mac's copy, which the person stopped: the
    // founding by default stands aside for the same record ([`commons_foundable`]).
    if crate::service::subscriber_turned_off(&paths.home) {
        return no(
            "sync is off on this Mac by your choice (`aterm secrets agent off`), so no pass \
             publishes to the vault from it: `aterm secrets agent on` lets it",
        );
    }
    if read_identity(paths)?.is_none() || fsx::kind(&paths.cache_file())? == fsx::Kind::Missing {
        return no("this Mac has applied no copy of the vault yet");
    }
    let lock = StateLock::observe(paths)?;
    if lock.is_none() && fsx::kind(&paths.lock_file())? != fsx::Kind::Missing {
        return Ok(Stewardship::Busy(
            "a vault pass is applying on this machine now: the steward reads the vault once it \
             ends"
                .to_owned(),
        ));
    }
    let state = load_state(paths)?;
    let Some(text) = admin_text_here(paths, &config, &state)? else {
        return no("this Mac holds no admin key of the ALab vault");
    };
    Ok(Stewardship::Steward {
        fleet_code: text
            .doc
            .entry(&Audience::Everyone, fleet::FLEET_CODE_ENTRY)
            .is_some(),
    })
}

/// What the ALab steward's revoke-repair step (`sync.steward.revoke`) finds or did
/// (`DESIGN-alab-swarm` §7.6, C25).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StewardRevoke {
    /// Not applicable on this Mac (not an ALab steward, or vault sync is off).
    Idle(String),
    /// No half-done revoke cascade remains in the vault.
    Current(String),
    /// A half-done revoke cascade was found and can be finished in one publish.
    Due {
        /// What the read found unfinished.
        found: String,
        /// What the repair publish will finish.
        makes: String,
    },
    /// Finished a half-done revoke cascade and published it.
    Done(String),
}

/// Read, writing nothing and fetching nothing, whether the locally applied ALab vault copy holds
/// any half-done revoke cascade (`sync.steward.revoke` probe), or, where the vault holds a log, a
/// member's log removal not yet mirrored into the envelope `0.145` Macs read (F16: any member
/// removes, and an admin's steward carries it into `[machines]`).
///
/// # Errors
/// A vault pass is applying on this Mac now, or local state, the signed copy, the admin locker,
/// `aterm-swarm-roster`, or `ai-pool.toml` cannot be read.
pub fn steward_revoke_check(paths: &Paths) -> crate::Result<StewardRevoke> {
    let idle = |why: &str| Ok(StewardRevoke::Idle(why.to_owned()));
    let Some(config) = config(paths)? else {
        return idle("no vault is configured");
    };
    if !alab_vault(&config) {
        return idle("the configured vault is not ALab's, and no other vault is stewarded");
    }
    if crate::service::subscriber_turned_off(&paths.home) {
        return idle(
            "sync is off on this Mac by your choice (`aterm secrets agent off`), so no pass \
             publishes to the vault from it: `aterm secrets agent on` lets it",
        );
    }
    if read_identity(paths)?.is_none() || fsx::kind(&paths.cache_file())? == fsx::Kind::Missing {
        return idle("this Mac has applied no copy of the vault yet");
    }
    let lock = StateLock::observe(paths)?;
    if lock.is_none() && fsx::kind(&paths.lock_file())? != fsx::Kind::Missing {
        return Err(Error::new(
            "a vault pass is applying on this machine now: the steward reads the vault once it \
             ends",
        ));
    }
    let state = load_state(paths)?;
    let Some(text) = admin_text_here(paths, &config, &state)? else {
        return idle("this Mac holds no admin key of the ALab vault");
    };
    let mut candidate = text.doc.clone();
    let mut log_removed_machines = Vec::new();
    let local_log = super::log_entries::trusted_local_log(paths, &config)?;
    if let Some(log) = &local_log {
        let admins: BTreeSet<String> = candidate.admins().into_iter().collect();
        for machine in candidate.machines() {
            if admins.contains(&machine.name) && admins.len() <= 1 {
                continue;
            }
            if let Ok(recipient) = Recipient::parse(&machine.recipient)
                && (log.recipient_is_removed(&recipient)
                    || log.audience_left_out_recipient(
                        &crate::membership_log::EpochAudience::Everyone,
                        &recipient,
                    )
                    || log.membership().members().values().any(|m| {
                        m.binding.recipient() == recipient
                            && m.node_key.is_some_and(|nk| {
                                nk != [0; 32]
                                    && matches!(
                                        log.membership().node(&nk),
                                        crate::membership_log::NodeStanding::Removed(_)
                                    )
                            })
                    }))
                && candidate.remove_machine(&machine.name, true) == Ok(true)
            {
                log_removed_machines.push(machine.name);
            }
        }
    }
    match (
        log_removed_machines.is_empty(),
        retirement::repair_half_done_revokes_with_log(&mut candidate, local_log.as_ref())?,
    ) {
        (true, Some(makes)) => Ok(StewardRevoke::Due {
            found: "a revoked Mac still holds swarm roster or account-pool entries".to_owned(),
            makes,
        }),
        (false, Some(makes)) => Ok(StewardRevoke::Due {
            found: format!(
                "membership-log removal of {} is not yet mirrored and still holds swarm roster or \
                 account-pool entries",
                log_removed_machines.join(", ")
            ),
            makes,
        }),
        (false, None) => {
            let names = log_removed_machines.join(", ");
            Ok(StewardRevoke::Due {
                found: format!(
                    "membership-log removal of {names} is not yet mirrored into the vault envelope"
                ),
                makes: format!("mirror membership-log removal of {names} into the vault envelope"),
            })
        }
        (true, None) => Ok(StewardRevoke::Current(
            "every revoked Mac's roster and pool cascade is complete".to_owned(),
        )),
    }
}

/// Open the ALab vault fresh on this steward and finish any half-done revoke cascade in one
/// publish (`sync.steward.revoke` apply, `DESIGN-alab-swarm` §7.6).
///
/// # Errors
/// Vault read, roster/pool parse, or publication fails.
pub fn steward_finish_revokes(paths: &Paths) -> crate::Result<StewardRevoke> {
    match stewardship(paths)? {
        Stewardship::No(why) => return Ok(StewardRevoke::Idle(why)),
        Stewardship::Busy(why) => return Err(Error::new(why)),
        Stewardship::Steward { .. } => {}
    }
    let mirrored = super::log_entries::admin_mirror_log(paths, &[])
        .ok()
        .flatten();
    let admin = admin_open(paths)?;
    let local_log = super::log_entries::trusted_local_log(paths, &admin.config)?;
    let mut doc = admin.published_doc.clone();
    let Some(summary) =
        retirement::repair_half_done_revokes_with_log(&mut doc, local_log.as_ref())?
    else {
        if let Some(seq) = mirrored {
            return Ok(StewardRevoke::Done(format!(
                "mirrored membership-log removal into vault envelope (version {seq})"
            )));
        }
        return Ok(StewardRevoke::Current(
            "every revoked Mac's roster and pool cascade is complete".to_owned(),
        ));
    };
    match admin_publish(paths, &admin, &doc) {
        Ok(done) => Ok(StewardRevoke::Done(format!(
            "{summary} (version {})",
            done.seq
        ))),
        Err(err) if err.kind() == crate::ErrorKind::NotStored => {
            let refreshed = admin_open(paths)?;
            let refreshed_log = super::log_entries::trusted_local_log(paths, &refreshed.config)?;
            let mut again = refreshed.published_doc.clone();
            match retirement::repair_half_done_revokes_with_log(&mut again, refreshed_log.as_ref())?
            {
                None => Ok(StewardRevoke::Current(
                    "another steward finished the revoke cascade first".to_owned(),
                )),
                Some(summary2) => {
                    let done = admin_publish(paths, &refreshed, &again)?;
                    Ok(StewardRevoke::Done(format!(
                        "{summary2} (version {})",
                        done.seq
                    )))
                }
            }
        }
        Err(err) => Err(err),
    }
}

fn local_observed_network_id(paths: &Paths) -> Option<String> {
    let text = std::fs::read_to_string(paths.dir.join("network-id")).ok()?;
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

pub(super) fn member_is_denied_in_roster(
    roster: &crate::swarm_roster::SwarmRoster,
    member: &crate::swarm_roster::SwarmMember,
) -> bool {
    roster.is_label_denied(&member.label)
        || member.aliases.iter().any(|a| roster.is_label_denied(a))
        || member
            .node_id
            .as_deref()
            .is_some_and(|n| roster.is_node_denied(n))
        || member
            .node_key
            .as_deref()
            .is_some_and(|k| roster.is_node_denied(k))
}

pub fn local_recipient_denied_in_roster(paths: &Paths) -> bool {
    let Some(identity) = read_identity(paths).ok().flatten() else {
        return false;
    };
    let my_rec = identity.recipient();
    let recipient = my_rec.to_bech32();
    let cfg = super::config(paths).ok().flatten();
    let local_log = cfg.as_ref().and_then(|c| {
        super::log_entries::trusted_local_log(paths, c)
            .ok()
            .flatten()
    });
    if let (Some(c), Some(log)) = (&cfg, &local_log) {
        if log.removes(&my_rec) {
            return true;
        }
        let state = load_state(paths).ok();
        let behind = state
            .as_ref()
            .and_then(|s| super::held_copy(paths, c, s))
            .and_then(|bytes| envelope::parse(&bytes).ok())
            .is_some_and(|env| {
                super::log_entries::log_base(Some(log), &env, &identity).ok()
                    == Some(super::log_entries::LogBase::Behind)
            });
        if !behind
            && log.audience_left_out_recipient(
                &crate::membership_log::EpochAudience::Everyone,
                &my_rec,
            )
        {
            return true;
        }
    }
    let Some(r) = roster::swarm_roster(paths).ok().flatten() else {
        return false;
    };
    if let Some(m) = r.member_by_recipient(&recipient)
        && member_is_denied_in_roster(&r, m)
    {
        return true;
    }
    if let Some(cfg) = &cfg {
        if r.is_label_denied(&cfg.host) {
            return true;
        }
        if let Some(log) = &local_log
            && log_entries::recipient_denied_by_roster_or_log(Some(&r), log, &my_rec)
        {
            return true;
        }
    }
    false
}

fn resolve_steward_own_label(paths: &Paths, doc: &Document) -> crate::Result<Option<String>> {
    if local_recipient_denied_in_roster(paths) {
        return Ok(None);
    }
    let Some(identity) = read_identity(paths)? else {
        return Ok(None);
    };
    let recipient = identity.recipient().to_bech32();
    let mut doc_roster = crate::swarm_roster::SwarmRoster::from_document(doc)?;
    if let Some(ref mut r) = doc_roster {
        if let Ok(Some(denied)) = super::swarm_denied_nodes(paths) {
            r.deny_nodes.extend(denied);
            r.canonicalize();
        }
        if let Some(log) = config(paths)
            .ok()
            .flatten()
            .and_then(|c| log_entries::trusted_local_log(paths, &c).ok().flatten())
        {
            roster::overlay_log_membership(&log, r);
        }
    }
    let local_roster = roster::swarm_roster(paths).ok().flatten();
    if let Some(ref r) = doc_roster
        && let Some(member) = r.member_by_recipient(&recipient)
    {
        if member_is_denied_in_roster(r, member) {
            return Ok(None);
        }
        return Ok(Some(member.label.clone()));
    }
    if let Some(ref r) = local_roster
        && let Some(member) = r.member_by_recipient(&recipient)
        && member_is_denied_in_roster(r, member)
    {
        return Ok(None);
    }
    let candidate_allowed = |canonical: &str| {
        doc_roster.as_ref().is_none_or(|r| {
            !r.is_label_denied(canonical)
                && r.member_by_label_or_alias(canonical)
                    .is_none_or(|m| m.recipient == recipient)
        }) && local_roster.as_ref().is_none_or(|r| {
            !r.is_label_denied(canonical)
                && r.member_by_label_or_alias(canonical)
                    .is_none_or(|m| m.recipient == recipient)
        })
    };
    if let Some(label) = roster::resolve_local_swarm_label(paths, &[])?
        && candidate_allowed(&label)
    {
        return Ok(Some(label));
    }
    for m in doc.machines() {
        if m.recipient == recipient
            && let Some((_, canonical)) = crate::swarm_roster::leading_m_label(&m.name)
            && candidate_allowed(&canonical)
        {
            return Ok(Some(canonical));
        }
    }
    Ok(None)
}

fn resolve_member_own_label(paths: &Paths, config: &Config) -> crate::Result<Option<String>> {
    if local_recipient_denied_in_roster(paths) {
        return Ok(None);
    }
    if let Some(label) = roster::resolve_local_swarm_label(paths, &[])? {
        return Ok(Some(label));
    }
    let recipient = read_identity(paths)?.map(|id| id.recipient().to_bech32());
    let local_roster = roster::swarm_roster(paths)?;
    if let (Some(r), Some(rec)) = (&local_roster, recipient.as_deref())
        && let Some(member) = r.member_by_recipient(rec)
        && member_is_denied_in_roster(r, member)
    {
        return Ok(None);
    }
    if let Some((_, canonical)) = crate::swarm_roster::leading_m_label(&config.host) {
        if local_roster.as_ref().is_some_and(|r| {
            r.is_label_denied(&canonical)
                || r.member_by_label_or_alias(&canonical)
                    .is_some_and(|m| Some(m.recipient.as_str()) != recipient.as_deref())
        }) {
            return Ok(None);
        }
        return Ok(Some(canonical));
    }
    if local_roster.is_none()
        && config.host != crate::alab_advertised()
        && crate::text::valid_machine_name(&config.host)
    {
        return Ok(Some(config.host.clone()));
    }
    Ok(None)
}

/// Evaluate ALab member server-role settings for `cli::serve` (`DESIGN-fluid-swarm` §4.1 F6 & F7:
/// every active ALab vault member pins the ALab key and advertises `_alab-vault._tcp` under its
/// own `<label>.local`).
#[must_use]
pub fn steward_alab_policy_role(paths: &Paths) -> Option<(bool, Option<String>)> {
    let config = super::config(paths).ok()??;
    if !alab_vault(&config) {
        return None;
    }
    let state = load_state(paths).ok()?;
    let log_intact = state.seq == 0 && super::log_only_intact(paths, &config, &state);
    if state.membership != Stored::In || (state.seq < 1 && !log_intact) {
        return None;
    }
    if local_recipient_denied_in_roster(paths) {
        return None;
    }
    if let Some(identity) = read_identity(paths).ok().flatten()
        && let Some(log) = super::log_entries::trusted_local_log(paths, &config)
            .ok()
            .flatten()
        && (log.recipient_is_removed(&identity.recipient())
            || log.audience_left_out_recipient(
                &crate::membership_log::EpochAudience::Everyone,
                &identity.recipient(),
            ))
    {
        return None;
    }
    let own_label = resolve_member_own_label(paths, &config).ok().flatten();
    Some((true, own_label))
}

/// Outcome of the ALab steward lease step (`sync.steward.lease`, `DESIGN-alab-swarm` §3.4, §8.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StewardLease {
    /// Not applicable on this Mac (not an `always_on` steward, off the ALab network, or no policy).
    Idle(String),
    /// Existing lease stands (`>= 3 h` remaining for holder, or unexpired for standby).
    Current(String),
    /// A claim, renewal, or takeover publish is due.
    Due {
        /// What the probe found.
        found: String,
        /// What publishing will record.
        makes: String,
    },
    /// Claimed, renewed, or took over the lease in one CAS publish.
    Done(String),
}

fn expire_denied_lease_holder(
    paths: &Paths,
    doc: &Document,
    current: &mut Option<crate::swarm_roster::SwarmLease>,
) {
    let Some(lease) = current.as_mut() else {
        return;
    };
    let Ok(Some(mut roster)) = crate::swarm_roster::SwarmRoster::from_document(doc) else {
        return;
    };
    if let Ok(Some(denied)) = super::fleet::swarm_denied_nodes(paths) {
        roster.deny_nodes.extend(denied);
        roster.canonicalize();
    }
    if let Some(log) = config(paths)
        .ok()
        .flatten()
        .and_then(|c| log_entries::trusted_local_log(paths, &c).ok().flatten())
    {
        roster::overlay_log_membership(&log, &mut roster);
    }
    let holder_denied = roster.is_label_denied(&lease.holder)
        || roster
            .member_by_label_or_alias(&lease.holder)
            .is_some_and(|member| {
                roster.is_label_denied(&member.label)
                    || member.aliases.iter().any(|a| roster.is_label_denied(a))
                    || member
                        .node_id
                        .as_deref()
                        .is_some_and(|n| roster.is_node_denied(n))
                    || member
                        .node_key
                        .as_deref()
                        .is_some_and(|k| roster.is_node_denied(k))
            });
    if holder_denied {
        lease.until = 0;
    }
}

/// Read, writing nothing and fetching nothing, whether this steward should claim, renew, or take
/// over `aterm-swarm-lease` (`sync.steward.lease` probe).
///
/// # Errors
/// A vault pass is applying on this Mac now, or local state, the signed copy, the admin locker,
/// `aterm-swarm-policy`, or `aterm-swarm-lease` cannot be read.
pub fn steward_lease_check(
    paths: &Paths,
    observed_network_id: Option<&str>,
    now_secs: u64,
) -> crate::Result<StewardLease> {
    let idle = |why: &str| Ok(StewardLease::Idle(why.to_owned()));
    if let Stewardship::No(why) = stewardship(paths)? {
        return Ok(StewardLease::Idle(why));
    }
    let Some(config) = super::config(paths)? else {
        return idle("no vault is configured");
    };
    let state = load_state(paths)?;
    let Some(text) = admin_text_here(paths, &config, &state)? else {
        return idle("this Mac holds no admin key of the ALab vault");
    };
    let Some(policy) = crate::swarm_roster::SwarmPolicy::from_document(&text.doc)? else {
        return idle("no aterm-swarm-policy is published in the vault yet");
    };
    let Some(own_label) = resolve_steward_own_label(paths, &text.doc)? else {
        return idle("this steward has no canonical m<N> label yet");
    };
    let fallback_net = local_observed_network_id(paths);
    let effective_net = observed_network_id.or(fallback_net.as_deref());
    let mut current = crate::swarm_roster::SwarmLease::from_document(&text.doc)?;
    expire_denied_lease_holder(paths, &text.doc, &mut current);
    match crate::swarm_roster::decide_lease_action(
        &policy,
        &own_label,
        effective_net,
        current.as_ref(),
        now_secs,
    ) {
        crate::swarm_roster::LeaseDecision::NotEligible(why) => Ok(StewardLease::Idle(why)),
        crate::swarm_roster::LeaseDecision::Current(lease) => Ok(StewardLease::Current(format!(
            "lease epoch {} held by {} until {}",
            lease.epoch, lease.holder, lease.until
        ))),
        crate::swarm_roster::LeaseDecision::Claim(next) => Ok(StewardLease::Due {
            found: "no aterm-swarm-lease is published yet".to_owned(),
            makes: format!(
                "claim lease epoch {} as {} until {}",
                next.epoch, next.holder, next.until
            ),
        }),
        crate::swarm_roster::LeaseDecision::Renew(next) => Ok(StewardLease::Due {
            found: format!(
                "lease epoch {} held by {} has under 3 h remaining",
                next.epoch, next.holder
            ),
            makes: format!("renew lease epoch {} until {}", next.epoch, next.until),
        }),
        crate::swarm_roster::LeaseDecision::Takeover(next) => Ok(StewardLease::Due {
            found: format!(
                "prior lease expired past clock margin; {} is eligible to take over",
                own_label
            ),
            makes: format!(
                "take over lease at epoch {} as {} until {}",
                next.epoch, next.holder, next.until
            ),
        }),
    }
}

/// Open the ALab vault fresh on this `always_on` steward and claim, renew, or take over
/// `aterm-swarm-lease` via parent-digest CAS (`sync.steward.lease` apply, `DESIGN-alab-swarm` §3.4).
///
/// If a concurrent steward wins the CAS race (`ErrorKind::NotStored`), reopens and verifies the
/// winner's lease stands cleanly (`SingleCurrentLeaseEpoch`).
///
/// # Errors
/// Vault read, policy/lease parse, or publication fails.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "LedgerPublish",
        action = "ClaimLease",
        project = "aterm_secrets::client::ledger_publish::tests::ledger_publish_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "LedgerPublish",
        action = "RenewLease",
        project = "aterm_secrets::client::ledger_publish::tests::ledger_publish_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "LedgerPublish",
        action = "TakeoverLease",
        project = "aterm_secrets::client::ledger_publish::tests::ledger_publish_transition"
    )
)]
pub fn steward_ensure_lease(
    paths: &Paths,
    observed_network_id: Option<&str>,
    now_secs: u64,
) -> crate::Result<StewardLease> {
    if let Stewardship::No(why) = stewardship(paths)? {
        return Ok(StewardLease::Idle(why));
    }
    let fallback_net = local_observed_network_id(paths);
    let effective_net = observed_network_id.or(fallback_net.as_deref());
    let _ = super::log_entries::admin_mirror_log(paths, &[]);

    for attempt in 0..2 {
        let admin = admin_open(paths)?;
        let Some(policy) = crate::swarm_roster::SwarmPolicy::from_document(&admin.published_doc)?
        else {
            return Ok(StewardLease::Idle(
                "no aterm-swarm-policy is published in the vault yet".to_owned(),
            ));
        };
        let Some(own_label) = resolve_steward_own_label(paths, &admin.published_doc)? else {
            return Ok(StewardLease::Idle(
                "this steward has no canonical m<N> label yet".to_owned(),
            ));
        };
        let mut current = crate::swarm_roster::SwarmLease::from_document(&admin.published_doc)?;
        expire_denied_lease_holder(paths, &admin.published_doc, &mut current);
        let next = match crate::swarm_roster::decide_lease_action(
            &policy,
            &own_label,
            effective_net,
            current.as_ref(),
            now_secs,
        ) {
            crate::swarm_roster::LeaseDecision::NotEligible(why) => {
                return Ok(StewardLease::Idle(why));
            }
            crate::swarm_roster::LeaseDecision::Current(lease) => {
                return Ok(StewardLease::Current(format!(
                    "lease epoch {} held by {} until {}",
                    lease.epoch, lease.holder, lease.until
                )));
            }
            crate::swarm_roster::LeaseDecision::Claim(next)
            | crate::swarm_roster::LeaseDecision::Renew(next)
            | crate::swarm_roster::LeaseDecision::Takeover(next) => next,
        };
        let mut doc = admin.published_doc.clone();
        let admins = doc.admins();
        doc.set_entry(
            &Audience::Hosts(admins),
            crate::SWARM_LEASE_ENTRY,
            next.to_toml().as_bytes(),
        )
        .map_err(|e| Error::new(e.to_string()))?;
        match admin_publish(paths, &admin, &doc) {
            Ok(done) => {
                return Ok(StewardLease::Done(format!(
                    "published lease epoch {} as {} until {} (version {})",
                    next.epoch, next.holder, next.until, done.seq
                )));
            }
            Err(err) if err.kind() == crate::ErrorKind::NotStored && attempt == 0 => continue,
            Err(err) => return Err(err),
        }
    }
    let refreshed = admin_open(paths)?;
    if let Some(winner) = crate::swarm_roster::SwarmLease::from_document(&refreshed.published_doc)?
    {
        return Ok(StewardLease::Current(format!(
            "another steward published lease epoch {} ({}) first",
            winner.epoch, winner.holder
        )));
    }
    Err(Error::of_kind(
        crate::ErrorKind::NotStored,
        "lease CAS publish contended and no winning lease was readable",
    ))
}

/// Minimum release carrying F6 and F7 (`DESIGN-fluid-swarm` §4.1 F6, §4.3 step 1).
pub(crate) const FLUID_SERVER_MIN_RELEASE: locker_skew::Version = (0, 146, 0);

/// Outcome of the member vault server step (`sync.steward.server.standby`, `DESIGN-fluid-swarm`
/// §4.1 F6 & §4.3 step 1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StewardStandbyServer {
    /// Not applicable on this Mac (not an active URL-backed ALab member, or turned off by choice).
    Idle(String),
    /// A vault server job (`aterm-vault-server` LaunchDaemon or `aterm-secrets-server` LaunchAgent)
    /// is already running on a current build on this member.
    Current(String),
    /// This member has no vault server running yet, or holds an unpinned server ready
    /// to move onto the installed release.
    Due {
        /// What the probe found.
        found: String,
        /// What applying will start or move.
        makes: String,
    },
    /// Installed and started (or moved) the member vault server.
    Done(String),
}

pub(super) fn check_standby_eligibility(
    paths: &Paths,
    _observed_network_id: Option<&str>,
) -> crate::Result<Result<String, String>> {
    let Some(config) = super::config(paths)? else {
        return Ok(Err("no vault is configured".to_owned()));
    };
    if !alab_vault(&config) {
        return Ok(Err(
            "the configured vault is not ALab's, and only ALab members auto-serve".to_owned(),
        ));
    }
    if !matches!(config.source, Source::Url(_)) {
        return Ok(Err(
            "the configured vault is a local file rather than a URL vault".to_owned(),
        ));
    }
    if crate::service::subscriber_turned_off(&paths.home) {
        return Ok(Err(
            "sync is off on this Mac by your choice (`aterm secrets agent off`)".to_owned(),
        ));
    }
    if crate::service::server_turned_off(&paths.home) {
        return Ok(Err(
            "the vault server is off on this Mac by your choice (`aterm secrets server off`)"
                .to_owned(),
        ));
    }
    let Some(identity) = read_identity(paths)? else {
        return Ok(Err(
            "this Mac has applied no copy of the vault yet".to_owned()
        ));
    };
    let state = load_state(paths)?;
    let log_intact = state.seq == 0 && super::log_only_intact(paths, &config, &state);
    if !log_intact && fsx::kind(&paths.cache_file())? == fsx::Kind::Missing {
        return Ok(Err(
            "this Mac has applied no copy of the vault yet".to_owned()
        ));
    }
    if state.membership != Stored::In || (state.seq < 1 && !log_intact) {
        return Ok(Err(
            "this Mac is not an active member of the ALab vault".to_owned()
        ));
    }
    if local_recipient_denied_in_roster(paths) {
        return Ok(Err(
            "this Mac is not an active member of the ALab vault".to_owned()
        ));
    }
    if let Some(log) = super::log_entries::trusted_local_log(paths, &config)?
        && (log.recipient_is_removed(&identity.recipient())
            || log.audience_left_out_recipient(
                &crate::membership_log::EpochAudience::Everyone,
                &identity.recipient(),
            ))
    {
        return Ok(Err(
            "this Mac is not an active member of the ALab vault".to_owned()
        ));
    }
    let Some(own_label) = resolve_member_own_label(paths, &config)? else {
        return Ok(Err("this member has no valid DNS host label yet".to_owned()));
    };
    Ok(Ok(own_label))
}

/// How long a held server moved onto this aterm has to serve its store's ALab vault before it
/// is put back ([`serves_alab`]).
pub(crate) const SERVES_WITHIN: Duration = Duration::from_secs(60);

/// A held server job this aterm moves (`sync.steward.server.standby`): its release,
/// the store it serves, which holds the ALab vault this build verifies ([`crate::server::holds_alab`]),
/// so the moved job can be seen serving it, and the URL it answers on this Mac.
#[cfg(unix)]
pub(crate) fn held_server_move_due(
    paths: &Paths,
    own: &locker_skew::Own,
    held: &crate::service::Held,
    new_program: &Path,
    release_of: &dyn Fn(&Path) -> Result<locker_skew::Version, String>,
    starts: &dyn Fn(&Path) -> crate::Result<()>,
) -> Option<(locker_skew::Version, PathBuf, String)> {
    if held.job != crate::service::Job::Server || held.pinned() {
        return None;
    }
    if !own.release || own.version < FLUID_SERVER_MIN_RELEASE {
        return None;
    }
    let held_ver = release_of(&held.program).ok()?;
    if held_ver >= FLUID_SERVER_MIN_RELEASE && !held_alab_release_due(own, held_ver) {
        return None;
    }
    crate::service::require_program_release(new_program, own.version, release_of).ok()?;
    let (store_dir, url) = crate::cli::served_by(paths, &held.args)?;
    if !crate::server::holds_alab(&store_dir) {
        return None;
    }
    starts(new_program).ok()?;
    Some((held_ver, store_dir, url))
}

/// Wait until `url` answers with the ALab vault ([`crate::server::is_alab_vault`]), for at most
/// `within`.
///
/// # Errors
/// Nothing there served it in time.
#[cfg(unix)]
pub(crate) fn serves_alab(url: &str, within: Duration) -> crate::Result<()> {
    let deadline = Instant::now() + within;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if let Ok(Fetched::Body(bytes)) =
            transport::fetch_within(url, None, left.min(Duration::from_secs(1)))
            && crate::server::is_alab_vault(&bytes)
        {
            return Ok(());
        }
        if left.is_zero() {
            return Err(Error::new(format!(
                "the moved server did not serve the ALab vault at {url} within {}s",
                within.as_secs()
            )));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(unix)]
pub(crate) type StandbyInstall<'a> =
    dyn FnMut(&Path, &[String]) -> crate::Result<Option<String>> + 'a;

#[cfg(unix)]
#[expect(
    clippy::too_many_arguments,
    reason = "injectable platform seams keep member-server converge checks unit-testable"
)]
pub(crate) fn steward_standby_server_check_with(
    paths: &Paths,
    observed_network_id: Option<&str>,
    own: &locker_skew::Own,
    new_program: Option<&Path>,
    daemon_state: &dyn Fn() -> crate::Result<crate::service::State>,
    server_state: &dyn Fn() -> crate::Result<crate::service::State>,
    held_server: &dyn Fn(Option<&Path>) -> Option<crate::service::Held>,
    release_of: &dyn Fn(&Path) -> Result<locker_skew::Version, String>,
    starts: &dyn Fn(&Path) -> crate::Result<()>,
) -> crate::Result<StewardStandbyServer> {
    let own_label = match check_standby_eligibility(paths, observed_network_id)? {
        Ok(label) => label,
        Err(why) => return Ok(StewardStandbyServer::Idle(why)),
    };
    if let crate::service::State::Installed { pid, .. } = daemon_state()? {
        return Ok(StewardStandbyServer::Current(match pid {
            Some(pid) => format!(
                "{} LaunchDaemon running (pid {pid})",
                crate::service::DAEMON_LABEL
            ),
            None => format!(
                "{} LaunchDaemon installed (managed by aterm machines)",
                crate::service::DAEMON_LABEL
            ),
        }));
    }
    if let Some(prog) = new_program
        && let Some(held) = held_server(Some(prog))
        && let Some((held_ver, store_dir, _)) =
            held_server_move_due(paths, own, &held, prog, release_of, starts)
    {
        return Ok(StewardStandbyServer::Due {
            found: format!(
                "{} runs held {} snapshot ({})",
                crate::service::Job::Server.label(),
                locker_skew::dotted(held_ver),
                held.program.display()
            ),
            makes: format!(
                "move {} onto {} serving {}",
                crate::service::Job::Server.label(),
                locker_skew::dotted(own.version),
                store_dir.display()
            ),
        });
    }
    if let crate::service::State::Installed { pid: Some(pid), .. } = server_state()? {
        return Ok(StewardStandbyServer::Current(format!(
            "{} running for {own_label} (pid {pid})",
            crate::service::Job::Server.label()
        )));
    }
    Ok(StewardStandbyServer::Due {
        found: format!("no vault server job is running on member {own_label}"),
        makes: format!(
            "start {} advertising {own_label}.local",
            crate::service::Job::Server.label()
        ),
    })
}

/// Probe whether this ALab member has its vault server running on a current build
/// (`sync.steward.server.standby` probe, `DESIGN-fluid-swarm` §4.1 F6 & §4.3 step 1).
///
/// # Errors
/// Local vault state or service state cannot be read.
pub fn steward_standby_server_check(
    paths: &Paths,
    observed_network_id: Option<&str>,
) -> crate::Result<StewardStandbyServer> {
    let own = locker_skew::own()?;
    let program = crate::service::program_for_service().ok();
    steward_standby_server_check_with(
        paths,
        observed_network_id,
        &own,
        program.as_deref(),
        &|| crate::service::daemon_state_for_home(&paths.home),
        &|| crate::service::state(&paths.home, crate::service::Job::Server),
        &|prog| crate::service::held(&paths.home, crate::service::Job::Server, prog),
        &locker_skew::version_of,
        &crate::service::check_program_starts,
    )
}

/// Move a held job onto a program, counting it moved once the closure it is given finds it
/// serving ([`crate::service::move_held_serving`]).
type MoveHeld<'a> = dyn FnMut(
        &crate::service::Held,
        &Path,
        &dyn Fn() -> crate::Result<()>,
    ) -> crate::Result<Option<String>>
    + 'a;

#[cfg(unix)]
#[expect(
    clippy::too_many_arguments,
    reason = "injectable platform seams keep member-server converge installation and held moves unit-testable"
)]
pub(crate) fn steward_ensure_standby_server_with(
    paths: &Paths,
    observed_network_id: Option<&str>,
    own: &locker_skew::Own,
    new_program: &Path,
    daemon_state: &dyn Fn() -> crate::Result<crate::service::State>,
    server_state: &dyn Fn() -> crate::Result<crate::service::State>,
    held_server: &dyn Fn(Option<&Path>) -> Option<crate::service::Held>,
    release_of: &dyn Fn(&Path) -> Result<locker_skew::Version, String>,
    starts: &dyn Fn(&Path) -> crate::Result<()>,
    serves: &dyn Fn(&str) -> crate::Result<()>,
    move_it: &mut MoveHeld<'_>,
    install_it: &mut StandbyInstall<'_>,
) -> crate::Result<StewardStandbyServer> {
    let own_label = match check_standby_eligibility(paths, observed_network_id)? {
        Ok(label) => label,
        Err(why) => return Ok(StewardStandbyServer::Idle(why)),
    };
    if let crate::service::State::Installed { pid, .. } = daemon_state()? {
        return Ok(StewardStandbyServer::Current(match pid {
            Some(pid) => format!(
                "{} LaunchDaemon running (pid {pid})",
                crate::service::DAEMON_LABEL
            ),
            None => format!(
                "{} LaunchDaemon installed (managed by aterm machines)",
                crate::service::DAEMON_LABEL
            ),
        }));
    }
    if let Some(held) = held_server(Some(new_program)) {
        if let Some((_, _, url)) =
            held_server_move_due(paths, own, &held, new_program, release_of, starts)
            && let Some(moved) = move_it(&held, new_program, &|| serves(&url))?
        {
            return Ok(StewardStandbyServer::Done(moved));
        }
        if let crate::service::State::Installed { pid: Some(pid), .. } = server_state()? {
            return Ok(StewardStandbyServer::Current(format!(
                "{} running (pid {pid})",
                crate::service::Job::Server.label()
            )));
        }
        return Ok(install_it(&held.program, &held.args)?.map_or_else(
            || StewardStandbyServer::Idle("server policy changed; kept as it is".to_owned()),
            StewardStandbyServer::Done,
        ));
    }
    if let crate::service::State::Installed { pid: Some(pid), .. } = server_state()? {
        return Ok(StewardStandbyServer::Current(format!(
            "{} running for {own_label} (pid {pid})",
            crate::service::Job::Server.label()
        )));
    }
    crate::service::require_program_release(new_program, own.version, release_of)?;
    let args = vec![
        "secrets".to_owned(),
        "serve".to_owned(),
        "--advertise".to_owned(),
        own_label,
        "--pin".to_owned(),
        format!("{}={}", crate::ALAB_VAULT, crate::ALAB_SIGNER),
        // Every member serves, laptops too: none is kept from idle sleep for it.
        "--no-keep-awake".to_owned(),
    ];
    Ok(install_it(new_program, &args)?.map_or_else(
        || StewardStandbyServer::Idle("server policy changed; kept as it is".to_owned()),
        StewardStandbyServer::Done,
    ))
}

/// Ensure the member vault server job runs on this ALab member and moves any held `< 0.146.0`
/// server snapshot onto the new build once it starts and serves the same store
/// (`sync.steward.server.standby` apply, `DESIGN-fluid-swarm` §4.1 F6 & §4.3 step 1).
///
/// # Errors
/// Service program lookup or launchd/systemd installation fails.
pub fn steward_ensure_standby_server(
    paths: &Paths,
    observed_network_id: Option<&str>,
) -> crate::Result<StewardStandbyServer> {
    if let Err(why) = check_standby_eligibility(paths, observed_network_id)? {
        return Ok(StewardStandbyServer::Idle(why));
    }
    let own = locker_skew::own()?;
    let program = crate::service::program_for_service()?;
    steward_ensure_standby_server_with(
        paths,
        observed_network_id,
        &own,
        &program,
        &|| crate::service::daemon_state_for_home(&paths.home),
        &|| crate::service::state(&paths.home, crate::service::Job::Server),
        &|prog| crate::service::held(&paths.home, crate::service::Job::Server, prog),
        &locker_skew::version_of,
        &crate::service::check_program_starts,
        &|url| serves_alab(url, SERVES_WITHIN),
        &mut |held, prog, serves| {
            crate::service::move_held_serving(&paths.home, held, prog, serves)
        },
        &mut |prog, args| crate::service::ensure_server(&paths.home, prog, args, own.version),
    )
}

#[cfg(test)]
mod tests;
