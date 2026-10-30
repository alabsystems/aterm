// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Revoke through durable Commons retirement before publishing vault removal, wherever
//! Commons control exists ([`holds_control`]); a vault without it removes a member from the
//! vault alone. A failed publication cannot restore an already retired device's membership.

use super::*;
use crate::commons::{
    Unavailable,
    bootstrap::{Availability, Bootstrap},
    constraints::Fact,
    journal::Journal,
    roster::Grant,
};

/// A permanent retirement retained in the local control journal. Peers learn it
/// through ordinary control repair; it cannot erase their existing copies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Retirement {
    /// Current control head witnessing the retirement and its ancestry.
    pub head: [u8; 32],
    /// Number of retained writer bindings covered by this retirement.
    pub bindings: usize,
    /// Number of distinct age recipients permanently denied membership.
    pub recipients: usize,
    /// Whether this call appended a retirement rather than recognizing a retry.
    pub changed: bool,
}

/// Successful vault removal and its prior Commons retirement, when enrolled.
pub struct Revocation {
    pub published: Published,
    pub retirement: Option<Retirement>,
}

/// Successful `--stolen` revocation (`DESIGN-alab-swarm` §7.6, C25): vault removal, Commons
/// retirement, swarm deny-list + pool slot release, `aterm-fleet-code` rotation, and the
/// single-line `NEEDS YOU:` provider rotation list.
pub struct StolenRevocation {
    /// Vault publication and prior Commons retirement.
    pub revocation: Revocation,
    /// True when `aterm-fleet-code` was present in `[everyone]` and rotated to a fresh secret.
    pub fleet_code_rotated: bool,
    /// Single-line `NEEDS YOU:` provider-credential rotation notice.
    pub needs_you_line: String,
}

/// The refusal of a removal by an admin that is no enrolled Commons member of the vault, `me`
/// this Mac's key: `held` when it holds the control entry and no node binds it as a writer,
/// else outside the entry's audience. It names the other admins in that audience, which may
/// remove the machine, and, from outside, the command that brings this Mac in there (naming it
/// by its code, as a Mac the vault gave another name than it asked for names itself).
fn not_a_member(admin: &Admin, me: Option<Recipient>, held: bool) -> Error {
    let mine = me.map(|me| me.to_bech32());
    let admins = admin.published_doc.admins();
    let machines = admin.published_doc.machines();
    let holders: Vec<&str> = match admin.published_doc.commons_audience() {
        Some(Audience::Hosts(hosts)) => hosts
            .iter()
            .filter(|host| {
                admins.contains(*host)
                    && machines.iter().any(|machine| {
                        machine.name == **host && Some(&machine.recipient) != mine.as_ref()
                    })
            })
            .map(String::as_str)
            .collect(),
        _ => Vec::new(),
    };
    let from = if holders.is_empty() {
        "an admin Mac enrolled in it".to_owned()
    } else {
        holders.join(" or ")
    };
    Error::new(if held {
        format!(
            "this Mac is not yet an enrolled member of the vault's Commons control, so it cannot remove a machine: remove it from {from}"
        )
    } else {
        let this = me.map_or_else(
            || "<this Mac's code, from aterm secrets status>".to_owned(),
            |me| join_code(&me),
        );
        format!(
            "this Mac is outside the vault's Commons control, so it cannot remove a machine: remove it from {from}, or run aterm secrets admins add {this} there to bring this Mac in"
        )
    })
}

/// This Mac's Commons custody for a removal: refused as [`not_a_member`] where the control entry
/// is outside what it can read (or, where the version acted on brings this Mac in, until it
/// applies that version), and with its own reason for any other (a busy or unapplied vault, and
/// the rest of [`crate::commons::Unavailable`]).
fn removal_custody(paths: &Paths, admin: &Admin) -> crate::Result<Bootstrap> {
    match Bootstrap::observe(paths)? {
        Availability::Available(bootstrap) => Ok(*bootstrap),
        Availability::Unavailable(Unavailable::ControlNotVisible) => {
            let me = read_identity(paths)?.map(|identity| identity.recipient());
            let mine = me.map(|me| me.to_bech32());
            let doc = &admin.published_doc;
            // The version acted on may already bring this Mac in, ahead of the one it applied.
            let brought_in = matches!(doc.commons_audience(), Some(Audience::Hosts(hosts))
            if hosts.iter().any(|host| doc.machines().iter().any(|machine| {
                machine.name == *host && Some(&machine.recipient) == mine.as_ref()
            })));
            Err(if brought_in {
                Error::new(
                    "this Mac has not yet applied the vault version that brings it into the vault's Commons control: run aterm secrets sync (or let its subscriber apply it), then remove the machine again",
                )
            } else {
                not_a_member(admin, me, false)
            })
        }
        Availability::Unavailable(reason) => Err(Error::new(format!(
            "removing a machine needs this Mac's Commons control applied ({}): {}",
            reason.code(),
            reason.explanation()
        ))),
    }
}

/// This device's roster in `journal`, for a removal: refused as [`not_a_member`] when no node
/// binds it as a writer (the roster is sealed to its members, so one never enrolled cannot read
/// it), and as a history to recover when one does but no current roster is readable.
fn removal_access(
    admin: &Admin,
    bootstrap: &Bootstrap,
    journal: &Journal,
) -> crate::Result<crate::commons::roster::Access> {
    let request = bootstrap.request()?;
    let bound = journal
        .inventory()
        .filter_map(|id| journal.node(id))
        .any(|node| {
            node.facts()
                .iter()
                .any(|fact| matches!(fact, Fact::Bind { binding } if binding == request.binding()))
        });
    if !bound {
        return Err(not_a_member(
            admin,
            Some(request.binding().recipient()),
            true,
        ));
    }
    bootstrap.access_in(journal)?.ok_or_else(|| {
        Error::new(
            "Commons retirement needs a current local roster: this Mac's Commons control history is incomplete or conflicting, so it cannot remove a machine until that history is recovered",
        )
    })
}

fn non_admin_target(admin: &Admin, recipient: Recipient) -> crate::Result<()> {
    if recipients_of(&admin.published_doc.admins(), &admin.published_doc)?.contains(&recipient) {
        return Err(Error::new(
            "cannot retire a vault admin while it retains Commons authority; an authority transfer is required",
        ));
    }
    Ok(())
}

impl Admin {
    /// Permanently retire every retained binding of these recipients, preserving
    /// other current members and roles in a fresh epoch. Exact retries append
    /// nothing. This performs no network I/O; the returned result is durable.
    pub fn retire_commons_members(
        &self,
        bootstrap: &Bootstrap,
        journal: &mut Journal,
        recipients: &[Recipient],
    ) -> crate::Result<Option<Retirement>> {
        if recipients.is_empty() || recipients.len() > crate::commons::constraints::MAX_FACTS {
            return Err(Error::new(
                "Commons retirement recipient count is out of bounds",
            ));
        }
        let targets: std::collections::BTreeSet<_> = recipients.iter().copied().collect();
        for recipient in &targets {
            non_admin_target(self, *recipient)?;
        }
        let descriptor = admin_keys::control_descriptor(&self.published_doc)?
            .ok_or_else(|| Error::new("Commons retirement needs its authenticated descriptor"))?;
        if descriptor.trust() != journal.trust()
            || bootstrap.trust() != journal.trust()
            || self.commons_authority()? != journal.trust().authority
        {
            return Err(Error::new(
                "Commons retirement has another authority or root",
            ));
        }
        if let Some((log, head)) =
            crate::client::log_entries::trusted_commons_log(bootstrap.paths(), bootstrap.trust())?
            && log.has_commons_epochs()
        {
            return self.retire_commons_members_in_log(bootstrap, &log, head, &targets);
        }
        let access = removal_access(self, bootstrap, journal)?;
        let constraints = journal.constraints()?;
        constraints.check_known_actor(&access.local().binding().actor())?;
        let mut known: std::collections::BTreeSet<_> = constraints
            .facts()
            .filter_map(|fact| match fact {
                Fact::Bind { binding } => Some(binding.recipient()),
                Fact::RetireRecipient { recipient } => Some(*recipient),
                _ => None,
            })
            .collect();
        known.extend(
            self.published_doc
                .machines()
                .iter()
                .filter_map(|machine| Recipient::parse(&machine.recipient).ok()),
        );
        if !targets.is_subset(&known) {
            return Ok(None);
        }
        let actors: std::collections::BTreeSet<_> = constraints
            .facts()
            .filter_map(|fact| match fact {
                Fact::Bind { binding } if targets.contains(&binding.recipient()) => {
                    Some(binding.actor())
                }
                _ => None,
            })
            .collect();
        if targets.contains(&access.local().binding().recipient()) {
            return Err(Error::new(
                "this device cannot retire its own Commons membership",
            ));
        }
        let mut pending = false;
        for recipient in &targets {
            pending |= !constraints.recipient_retired(recipient)?;
        }
        if !pending {
            return Ok(Some(Retirement {
                head: *access.head(),
                bindings: actors.len(),
                recipients: targets.len(),
                changed: false,
            }));
        }
        let machines = self.published_doc.machines();
        let grants = access
            .members()
            .iter()
            .filter(|member| !targets.contains(&member.binding().recipient()))
            .map(|member| {
                let host = machines
                    .iter()
                    .find(|machine| {
                        Recipient::parse(&machine.recipient).ok()
                            == Some(member.binding().recipient())
                    })
                    .ok_or_else(|| Error::new(format!(
                        "current Commons member {} is absent from the vault; include its full recipient in the same retire command",
                        member.binding().recipient().to_bech32()
                    )))?;
                if !self.commons_eligibility.includes(&host.name, member.binding().recipient()) {
                    return Err(Error::new(format!(
                        "current Commons member {} is outside the audience; include its full recipient in the same retire command",
                        member.binding().recipient().to_bech32()
                    )));
                }
                Ok(Grant::recorded(
                    host.name.clone(),
                    member.binding().clone(),
                    member.role(),
                ))
            })
            .collect::<crate::Result<Vec<_>>>()?;
        let ledger = crate::commons::hearing::Ledger::read(bootstrap);
        let node = crate::commons::roster::rotate_gated(
            &self.commons_seed,
            &self.commons_eligibility,
            journal,
            grants,
            &[],
            &targets.iter().copied().collect::<Vec<_>>(),
            Some(&crate::commons::hearing::Gate {
                ledger: &ledger,
                now_ms: aterm_time::unix_millis().ok_or_else(|| {
                    Error::new("system clock cannot represent a Commons contact time")
                })?,
            }),
        )?;
        journal.ingest(node.bytes()).map_err(|error| Error::new(format!(
            "Commons retirement write is unconfirmed; reopen and retry before assuming it failed: {error}"
        )))?;
        let current = bootstrap
            .access_in(journal)?
            .ok_or_else(|| Error::new("Commons retirement did not retain a usable roster"))?;
        let constraints = journal.constraints()?;
        for recipient in &targets {
            if !constraints.recipient_retired(recipient)? {
                return Err(Error::new(
                    "Commons retirement did not retain the recipient restriction",
                ));
            }
        }
        if current
            .members()
            .iter()
            .any(|member| targets.contains(&member.binding().recipient()))
            || actors
                .iter()
                .any(|actor| constraints.check_actor(actor).is_ok())
        {
            return Err(Error::new(
                "Commons retirement did not retain the requested restriction",
            ));
        }
        Ok(Some(Retirement {
            head: node.id(),
            bindings: actors.len(),
            recipients: targets.len(),
            changed: true,
        }))
    }

    fn retire_commons_members_in_log(
        &self,
        bootstrap: &Bootstrap,
        log: &crate::membership_log::Log,
        head: [u8; 32],
        targets: &std::collections::BTreeSet<Recipient>,
    ) -> crate::Result<Option<Retirement>> {
        let request = bootstrap.request()?;
        let local_actor = request.binding().actor();
        let membership = log.membership();
        let bound = membership
            .members()
            .get(&local_actor)
            .is_some_and(|m| &m.binding == request.binding())
            || log.has_historical_seat(request.binding());
        if !bound {
            return Err(not_a_member(
                self,
                Some(request.binding().recipient()),
                true,
            ));
        }
        let access = bootstrap.access()?.ok_or_else(|| {
            Error::new(
                "Commons retirement needs a current local roster: this Mac's Commons control history is incomplete or conflicting, so it cannot remove a machine until that history is recovered",
            )
        })?;
        if targets.contains(&access.local().binding().recipient()) {
            return Err(Error::new(
                "this device cannot retire its own Commons membership",
            ));
        }
        let mut known = std::collections::BTreeSet::new();
        let mut actors = std::collections::BTreeSet::new();
        for rec in log.records() {
            match rec.body() {
                crate::membership_log::Body::Genesis { founders, .. } => {
                    for f in founders {
                        known.insert(f.recipient);
                        if let Some(b) = &f.binding
                            && targets.contains(&b.recipient())
                        {
                            actors.insert(b.actor());
                        }
                    }
                }
                crate::membership_log::Body::Admit { entrant, .. } => {
                    let b = entrant.request.binding();
                    if log.has_historical_seat(b) {
                        known.insert(b.recipient());
                        if targets.contains(&b.recipient()) {
                            actors.insert(b.actor());
                        }
                    }
                }
                _ => {}
            }
        }
        known.extend(
            self.published_doc
                .machines()
                .iter()
                .filter_map(|machine| Recipient::parse(&machine.recipient).ok()),
        );
        if !targets.is_subset(&known) {
            return Ok(None);
        }
        let active_targets: Vec<crate::membership_log::MemberId> = membership
            .members()
            .iter()
            .filter(|(_, m)| targets.contains(&m.binding.recipient()))
            .map(|(actor, _)| *actor)
            .collect();
        let target_still_wrapped = targets.iter().any(|r| {
            log.wraps_maximal_audience_to(&crate::membership_log::EpochAudience::Commons, r)
                || log.wraps_maximal_audience_to(&crate::membership_log::EpochAudience::Everyone, r)
        });
        if active_targets.is_empty() && !target_still_wrapped {
            return Ok(Some(Retirement {
                head,
                bindings: actors.len(),
                recipients: targets.len(),
                changed: false,
            }));
        }
        let _ = crate::client::log_entries::retire_recipients_in_log_local(
            bootstrap.paths(),
            &active_targets,
            targets,
        )?;
        let (updated_log, new_head) =
            crate::client::log_entries::trusted_commons_log(bootstrap.paths(), bootstrap.trust())?
                .ok_or_else(|| {
                    Error::new("Commons retirement did not retain a usable membership log")
                })?;
        if updated_log
            .membership()
            .members()
            .values()
            .any(|m| targets.contains(&m.binding.recipient()))
            || targets.iter().any(|r| {
                updated_log
                    .wraps_maximal_audience_to(&crate::membership_log::EpochAudience::Commons, r)
                    || updated_log.wraps_maximal_audience_to(
                        &crate::membership_log::EpochAudience::Everyone,
                        r,
                    )
            })
        {
            return Err(Error::new(
                "Commons retirement did not retain the requested restriction",
            ));
        }
        Ok(Some(Retirement {
            head: new_head,
            bindings: actors.len(),
            recipients: targets.len(),
            changed: true,
        }))
    }
}

/// Remove a vault member after permanently retiring any Commons bindings.
/// Publication failure can leave a durable retirement with the old vault row;
/// retrying this command finishes removal without creating another key epoch.
/// Self-removal and unsupported authority demotion fail before retirement.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbRetirementPublication",
        action = "Revoke",
        project = "aterm_secrets::client::retirement::tests::retirement_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbRetirementPublication",
        action = "PublishFailure",
        project = "aterm_secrets::client::retirement::tests::retirement_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbRetirementPublication",
        action = "Retry",
        project = "aterm_secrets::client::retirement::tests::retirement_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbRetirementPublication",
        action = "NoBinding",
        project = "aterm_secrets::client::retirement::tests::retirement_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbRetirementPublication",
        action = "Legacy",
        project = "aterm_secrets::client::retirement::tests::retirement_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbRetirementPublication",
        action = "SelfRemoval",
        project = "aterm_secrets::client::retirement::tests::retirement_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbRetirementPublication",
        action = "Admin",
        project = "aterm_secrets::client::retirement::tests::retirement_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbRetirementPublication",
        action = "Fork",
        project = "aterm_secrets::client::retirement::tests::retirement_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbRetirementPublication",
        action = "MissingIdentity",
        project = "aterm_secrets::client::retirement::tests::retirement_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbRetirementPublication",
        action = "Unfounded",
        project = "aterm_secrets::client::retirement::tests::retirement_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbRetirementPublication",
        action = "RetainedRoot",
        project = "aterm_secrets::client::retirement::tests::retirement_transition"
    )
)]
pub fn machine_named_in_vault(paths: &Paths, admin: &Admin, word: &str) -> crate::Result<String> {
    let candidate = machine_named(&admin.published_doc, word)?;
    let machines = admin.published_doc.machines();
    if machines.iter().any(|m| m.name == candidate) {
        return Ok(candidate);
    }
    if !transport::valid_code(&word.to_ascii_lowercase())
        && let Some((_, canonical)) = crate::swarm_roster::leading_m_label(word)
        && let Ok(Some(log)) = log_entries::trusted_local_log(paths, &admin.config)
    {
        let membership = log.membership();
        let mut matched = membership
            .members()
            .values()
            .map(|member| {
                (
                    member.label.as_str(),
                    member.name.as_str(),
                    member.binding.recipient(),
                )
            })
            .chain(membership.unproved().iter().map(|founder| {
                (
                    founder.label.as_str(),
                    founder.name.as_str(),
                    founder.recipient,
                )
            }))
            .filter_map(|(label, name, recipient)| {
                let matches_label = crate::swarm_roster::leading_m_label(label)
                    .is_some_and(|(_, c)| c == canonical)
                    || crate::swarm_roster::leading_m_label(name)
                        .is_some_and(|(_, c)| c == canonical);
                if !matches_label {
                    return None;
                }
                let rec_str = recipient.to_bech32();
                machines
                    .iter()
                    .find(|m| m.recipient == rec_str)
                    .map(|m| m.name.clone())
            });
        if let (Some(only), None) = (matched.next(), matched.next()) {
            return Ok(only);
        }
    }
    Ok(candidate)
}

pub fn admin_revoke(
    paths: &Paths,
    admin: &Admin,
    host: &str,
    drop_sections: bool,
) -> crate::Result<Revocation> {
    let host = machine_named_in_vault(paths, admin, host)?;
    let recipient = admin
        .published_doc
        .machines()
        .into_iter()
        .find(|machine| machine.name == host)
        .ok_or_else(|| Error::new(format!("no machine {host} in the vault")))?
        .recipient;
    let recipient = Recipient::parse(&recipient).map_err(|error| Error::new(error.to_string()))?;
    let mut doc = admin.published_doc.clone();
    doc.remove_machine(&host, drop_sections)
        .map_err(|error| Error::new(error.to_string()))?;
    let _ = apply_swarm_revoke_cascade(paths, admin, &mut doc, &host, &recipient, false)?;
    admin_keys::validate_commons_transition(&admin.published_doc, &doc)?;
    let sequence = envelope::next_seq(admin.seq)?;
    let identity = read_identity(paths)?.ok_or_else(|| Error::new("admin identity is missing"))?;
    publication_rights(admin, &doc, &identity)?;
    admin_keys::validate_commons(&doc, &admin.commons_seed)?;
    admin.admissions.retained(&doc)?.validate(&doc, sequence)?;
    // A refused locker rewrite comes before the permanent retirement, not after it.
    custody_readable(paths, admin)?;

    let retirement = retire_cached(paths, admin, &doc, &[recipient])?;
    remove_from_log(paths, admin, &recipient)?;
    let published = admin_publish_checked(paths, admin, &doc).map_err(|error| {
        if retirement.is_some() {
            Error::new(format!("Commons membership is permanently retired; vault removal was not confirmed: {error}. Retry the same revoke command"))
        } else { error }
    })?;
    Ok(Revocation {
        published,
        retirement,
    })
}

/// Take the revoked Mac out of the membership log too ([`log_entries::remove_from_log`]), before
/// the publish that stamps and sends it: a revoke that reached only the envelope left the Mac its
/// log seat, and with it every epoch key it held, so it could still rewrite or delete an entry on
/// every Mac that reads the log, and the admin's mirror carried that into the envelope. A Mac with
/// no seat in the log signs nothing here (F16's `Remove` by any member covers that).
fn remove_from_log(paths: &Paths, admin: &Admin, recipient: &Recipient) -> crate::Result<()> {
    log_entries::remove_from_log(
        paths,
        &admin.config,
        &envelope::signer_public(&admin.seed)?,
        &admin.handoff,
        recipient,
    )?;
    Ok(())
}

/// Permanently retire known Commons recipients together without editing the vault.
/// Full age recipients also address devices removed by an older vault-only
/// workflow. Current admin custody cannot be revoked through this operation.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbRetirementPublication",
        action = "RepairRemoved",
        project = "aterm_secrets::client::retirement::tests::retirement_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbRetirementPublication",
        action = "BatchRemoved",
        project = "aterm_secrets::client::retirement::tests::retirement_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbRetirementPublication",
        action = "UnknownBatch",
        project = "aterm_secrets::client::retirement::tests::retirement_transition"
    )
)]
pub fn admin_retire(
    paths: &Paths,
    admin: &Admin,
    subjects: &[String],
) -> crate::Result<Retirement> {
    if subjects.is_empty() || subjects.len() > crate::commons::constraints::MAX_FACTS {
        return Err(Error::new(
            "Commons retirement recipient count is out of bounds",
        ));
    }
    let recipients = subjects.iter().map(|subject| {
        if subject.starts_with("age1") {
            Recipient::parse(subject).map_err(|error| Error::new(error.to_string()))
        } else {
            let host = machine_named_in_vault(paths, admin, subject)?;
            let machine = admin.published_doc.machines().into_iter().find(|machine| machine.name == host)
                .ok_or_else(|| Error::new("no vault machine with that name; a previously removed device needs its full age recipient"))?;
            Recipient::parse(&machine.recipient).map_err(|error| Error::new(error.to_string()))
        }
    }).collect::<crate::Result<Vec<_>>>()?;
    let identity = read_identity(paths)?.ok_or_else(|| Error::new("admin identity is missing"))?;
    publication_rights(admin, &admin.published_doc, &identity)?;
    if admin_keys::control_descriptor(&admin.published_doc)?.is_none()
        && !(admin.commons_bound && holds_control(paths, admin, &admin.published_doc)?)
    {
        return Err(Error::new(
            "this vault has no Commons control, so it has no Commons membership to retire; aterm secrets revoke removes a machine",
        ));
    }
    retire_cached(paths, admin, &admin.published_doc, &recipients)?.ok_or_else(|| {
        Error::new("a requested recipient has no authenticated vault or Commons identity")
    })
}

/// Whether a member removal of a bound vault must retire through Commons: the published
/// document or the edit `doc` names a control entry, or this Mac keeps Commons control state
/// ([`crate::commons::bootstrap::retains_control`]). A roster can name a machine only there:
/// without a published entry nothing observes, serves or extends a control journal, and a
/// grant needs the host in the published entry's audience. Every caller asks it only of a
/// bound vault (`commons_bound`): an unbound one cannot carry a control entry and removes
/// vault-only. The marker governs the root key's lifetime (rotation keeps it, demotion
/// refuses, a descriptor must carry it); alone it does not refuse a removal.
fn holds_control(paths: &Paths, admin: &Admin, doc: &Document) -> crate::Result<bool> {
    Ok(admin_keys::has_control(&admin.published_doc)
        || admin_keys::has_control(doc)
        || crate::commons::bootstrap::retains_control(paths)?)
}

fn retire_cached(
    paths: &Paths,
    admin: &Admin,
    doc: &Document,
    recipients: &[Recipient],
) -> crate::Result<Option<Retirement>> {
    if let Some(descriptor) = admin_keys::control_descriptor(&admin.published_doc)? {
        for recipient in recipients {
            non_admin_target(admin, *recipient)?;
        }
        let bootstrap = removal_custody(paths, admin)?;
        if bootstrap.trust() != descriptor.trust() {
            return Err(Error::new(
                "cached Commons root differs from the admin snapshot",
            ));
        }
        // Keep local source/configuration updates outside this transaction.
        // The journal lock is released before publishing anything to a server.
        let _state = configured_admin(paths, &admin.config)?;
        if load_state(paths)?.seq > admin.seq {
            return Err(Error::new(
                "a newer local vault exists; reopen before revoking",
            ));
        }
        let mut journal = Journal::open_existing(bootstrap.journal_dir(), bootstrap.trust())?;
        admin.retire_commons_members(&bootstrap, &mut journal, recipients)
    } else if admin.commons_bound && holds_control(paths, admin, doc)? {
        Err(Error::new(
            "bound Commons custody is missing its descriptor; refusing partial revocation (the vault names a Commons control entry, or this Mac keeps Commons control state)",
        ))
    } else {
        Ok(None)
    }
}

/// A generic vault edit may rename a recipient, but cannot silently remove a
/// still-active Commons binding. Explicit revoke retires first and passes this.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbRetirementPublication",
        action = "GenericRemoval",
        project = "aterm_secrets::client::retirement::tests::retirement_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbRetirementPublication",
        action = "UnfoundedEdit",
        project = "aterm_secrets::client::retirement::tests::retirement_transition"
    )
)]
pub(super) fn validate_removals(paths: &Paths, admin: &Admin, doc: &Document) -> crate::Result<()> {
    let removed = let_go(&admin.published_doc, doc);
    if removed.is_empty() {
        return Ok(());
    }
    let Some(descriptor) = admin_keys::control_descriptor(&admin.published_doc)? else {
        return if admin.commons_bound && holds_control(paths, admin, doc)? {
            Err(Error::new(
                "cannot remove members while bound Commons custody is unavailable",
            ))
        } else {
            Ok(())
        };
    };
    let bootstrap = removal_custody(paths, admin)?;
    if bootstrap.trust() != descriptor.trust()
        || admin.commons_authority()? != descriptor.trust().authority
    {
        return Err(Error::new("member removal has another Commons root"));
    }
    let _state = configured_admin(paths, &admin.config)?;
    if load_state(paths)?.seq > admin.seq {
        return Err(Error::new(
            "a newer local vault exists; reopen before removing a member",
        ));
    }
    if let Some((log, _)) =
        crate::client::log_entries::trusted_commons_log(paths, bootstrap.trust())?
        && log.has_commons_epochs()
    {
        if bootstrap.access()?.is_none() {
            return Err(Error::new(
                "Commons membership is not active on this device",
            ));
        }
        let membership = log.membership();
        for recipient in removed {
            let recipient =
                Recipient::parse(&recipient).map_err(|error| Error::new(error.to_string()))?;
            let still_in_commons = (!log.recipient_is_removed(&recipient)
                && !log.audience_left_out_recipient(
                    &crate::membership_log::EpochAudience::Commons,
                    &recipient,
                )
                && !log.audience_left_out_recipient(
                    &crate::membership_log::EpochAudience::Everyone,
                    &recipient,
                )
                && membership.members().iter().any(|(actor, m)| {
                    m.binding.recipient() == recipient
                        && !log.audience_left_out(
                            &crate::membership_log::EpochAudience::Commons,
                            actor,
                        )
                        && !log.audience_left_out(
                            &crate::membership_log::EpochAudience::Everyone,
                            actor,
                        )
                }))
                || log.wraps_maximal_audience_to(
                    &crate::membership_log::EpochAudience::Commons,
                    &recipient,
                );
            if still_in_commons {
                return Err(Error::new(
                    "retire Commons membership before deleting this recipient; use aterm secrets revoke or aterm secrets retire",
                ));
            }
        }
        return Ok(());
    }
    let journal = Journal::read_existing(bootstrap.journal_dir(), bootstrap.trust())?;
    let access = removal_access(admin, &bootstrap, &journal)?;
    let constraints = journal.constraints()?;
    constraints.check_known_actor(&access.local().binding().actor())?;
    for recipient in removed {
        let recipient =
            Recipient::parse(&recipient).map_err(|error| Error::new(error.to_string()))?;
        if !constraints.recipient_retired(&recipient)? {
            return Err(Error::new(
                "retire Commons membership before deleting this recipient; use aterm secrets revoke or aterm secrets retire",
            ));
        }
    }
    Ok(())
}

/// Vault `[everyone]` entry carrying the swarm AI account pool (`ai-pool.toml`).
const AI_POOL_ENTRY: &str = "ai-pool.toml";

/// Join-code alphabet (`aterm-net`'s `ALPHABET`: Crockford-like 32-symbol lowercase).
const FLEET_CODE_ALPHABET: &[u8; 32] = b"0123456789abcdefghjkmnpqrstvwxyz";
const FLEET_CODE_PREFIX: &str = "atf1-";
const FLEET_SECRET_LEN: usize = 20;
const FLEET_CHECK_SYMBOLS: usize = 4;
const FLEET_GROUP: usize = 4;
const FLEET_GF_POLY: u8 = 0b10_0101;

#[inline]
const fn gf32_mul(mut a: u8, mut b: u8) -> u8 {
    let mut product = 0;
    while b != 0 {
        if b & 1 != 0 {
            product ^= a;
        }
        b >>= 1;
        a <<= 1;
        if a & 0x20 != 0 {
            a ^= FLEET_GF_POLY;
        }
    }
    product
}

#[inline]
const fn gf32_generator() -> [u8; FLEET_CHECK_SYMBOLS + 1] {
    let mut g = [0u8; FLEET_CHECK_SYMBOLS + 1];
    g[0] = 1;
    let mut root = 1u8;
    let mut degree = 0;
    while degree < FLEET_CHECK_SYMBOLS {
        let mut i = degree + 1;
        while i >= 1 {
            g[i] ^= gf32_mul(g[i - 1], root);
            i -= 1;
        }
        root = gf32_mul(root, 2);
        degree += 1;
    }
    g
}

fn gf32_check_symbols(data: &[u8]) -> [u8; FLEET_CHECK_SYMBOLS] {
    let g = gf32_generator();
    let mut rem = [0u8; FLEET_CHECK_SYMBOLS];
    for &symbol in data {
        let factor = symbol ^ rem[0];
        rem.copy_within(1.., 0);
        rem[FLEET_CHECK_SYMBOLS - 1] = 0;
        for (slot, coefficient) in rem.iter_mut().zip(&g[1..]) {
            *slot ^= gf32_mul(*coefficient, factor);
        }
    }
    rem
}

fn encode_fleet_join_code(secret: &[u8; FLEET_SECRET_LEN], place: Option<&str>) -> String {
    let mut symbols = Vec::with_capacity(FLEET_SECRET_LEN * 8 / 5 + FLEET_CHECK_SYMBOLS);
    let (mut acc, mut bits) = (0u32, 0u32);
    for &byte in secret {
        acc = (acc << 8) | u32::from(byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            symbols.push(((acc >> bits) & 31) as u8);
        }
        acc &= (1 << bits) - 1;
    }
    symbols.extend_from_slice(&gf32_check_symbols(&symbols));
    let mut out = String::from(FLEET_CODE_PREFIX);
    for (at, symbol) in symbols.iter().enumerate() {
        if at > 0 && at % FLEET_GROUP == 0 {
            out.push('-');
        }
        out.push(char::from(FLEET_CODE_ALPHABET[usize::from(*symbol & 31)]));
    }
    if let Some(place) = place {
        out.push('@');
        out.push_str(place);
    }
    out
}

/// Mint a rotated `atf1-…` join code replacing `existing`'s 20-byte secret (`DESIGN-alab-swarm`
/// §7.6, C25), and keeping the `@<host>:<port>` it ends with, if any: the code is the secret
/// alone (`docs/DESIGN-fluid-swarm-2026-10-28.md` §4.1, F17), and the place, a member to dial
/// first, is what an aterm before 0.147 needs to read the code at all.
///
/// # Errors
/// `existing` does not start `atf1-`, its `@` names no `<host>:<port>`, or OS randomness fails.
pub fn rotate_fleet_code_text(existing: &str) -> crate::Result<String> {
    let trimmed = existing.trim();
    let body = trimmed
        .get(..FLEET_CODE_PREFIX.len())
        .filter(|head| head.eq_ignore_ascii_case(FLEET_CODE_PREFIX))
        .and_then(|_| trimmed.get(FLEET_CODE_PREFIX.len()..))
        .ok_or_else(|| Error::new("aterm-fleet-code does not start with `atf1-`"))?;
    let place = match body.rsplit_once('@') {
        Some((_, place)) if !place.trim().is_empty() && place.contains(':') => Some(place.trim()),
        Some(_) => {
            return Err(Error::new(
                "aterm-fleet-code's `@` names no `<host>:<port>`",
            ));
        }
        None => None,
    };
    let mut secret = [0u8; FLEET_SECRET_LEN];
    loop {
        aterm_uds::rand::fill(&mut secret)
            .map_err(|error| Error::new(format!("could not mint rotated fleet secret: {error}")))?;
        let candidate = encode_fleet_join_code(&secret, place);
        if !candidate.eq_ignore_ascii_case(trimmed) {
            zeroize::Zeroize::zeroize(&mut secret);
            return Ok(candidate);
        }
    }
}

struct CascadeSummary {
    display_label: String,
    warm_accounts: Vec<String>,
    fleet_code_rotated: bool,
}

fn strip_labels_from_pool_toml(
    raw: &str,
    denied_labels: &std::collections::BTreeSet<String>,
) -> crate::Result<(Option<String>, Vec<String>)> {
    if denied_labels.is_empty() {
        return Ok((None, Vec::new()));
    }
    let mut root: aterm_toml::Table = aterm_toml::from_str(raw)
        .map_err(|e| Error::new(format!("invalid {AI_POOL_ENTRY}: {e}")))?;
    let mut changed = false;
    let mut exposed_accounts = Vec::new();

    if let Some(aterm_toml::Value::Table(mut lh)) = root.remove("last_heard") {
        let drop_keys: Vec<String> = lh
            .keys()
            .filter(|k| {
                crate::swarm_roster::leading_m_label(k)
                    .is_some_and(|(_, canon)| denied_labels.contains(&canon))
            })
            .cloned()
            .collect();
        for key in drop_keys {
            lh.remove(&key);
            changed = true;
        }
        root.insert("last_heard".to_owned(), aterm_toml::Value::Table(lh));
    }

    if let Some(aterm_toml::Value::Table(mut leases)) = root.remove("leases") {
        let mut drop_keys = Vec::new();
        for (k, v) in &leases {
            let holder = v.as_str().or_else(|| {
                v.as_table()
                    .and_then(|t| t.get("holder"))
                    .and_then(|h| h.as_str())
            });
            if holder
                .and_then(crate::swarm_roster::leading_m_label)
                .is_some_and(|(_, canon)| denied_labels.contains(&canon))
            {
                drop_keys.push(k.clone());
                if let Some(acct) = v
                    .as_table()
                    .and_then(|t| t.get("account"))
                    .and_then(aterm_toml::Value::as_str)
                    .or_else(|| k.contains(':').then_some(k.as_str()))
                {
                    let acct = acct.trim();
                    if !acct.is_empty() && !exposed_accounts.iter().any(|existing| existing == acct)
                    {
                        exposed_accounts.push(acct.to_owned());
                    }
                }
            }
        }
        for key in drop_keys {
            leases.remove(&key);
            changed = true;
        }
        if !leases.is_empty() {
            root.insert("leases".to_owned(), aterm_toml::Value::Table(leases));
        }
    }

    if let Some(aterm_toml::Value::Array(mut lease_arr)) = root.remove("lease") {
        let before = lease_arr.len();
        lease_arr.retain(|item| {
            let Some(table) = item.as_table() else {
                return true;
            };
            let Some(holder) = table.get("holder").and_then(aterm_toml::Value::as_str) else {
                return true;
            };
            let denied = crate::swarm_roster::leading_m_label(holder)
                .is_some_and(|(_, canon)| denied_labels.contains(&canon));
            if denied
                && let Some(acct) = table
                    .get("account")
                    .and_then(aterm_toml::Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                && !exposed_accounts.iter().any(|existing| existing == acct)
            {
                exposed_accounts.push(acct.to_owned());
            }
            !denied
        });
        changed |= lease_arr.len() != before;
        if !lease_arr.is_empty() {
            root.insert("lease".to_owned(), aterm_toml::Value::Array(lease_arr));
        }
    }

    if let Some(aterm_toml::Value::Array(mut accounts)) = root.remove("account") {
        for item in &mut accounts {
            let Some(acct) = item.as_table_mut() else {
                continue;
            };
            let Some(warm_arr) = acct.get("warm").and_then(aterm_toml::Value::as_array) else {
                continue;
            };
            let old_len = warm_arr.len();
            let mut keep = Vec::with_capacity(old_len);
            let mut matched_here = false;
            for v in warm_arr {
                let drop_slot = v.as_str().is_some_and(|s| {
                    crate::swarm_roster::leading_m_label(s)
                        .is_some_and(|(_, canon)| denied_labels.contains(&canon))
                });
                if drop_slot {
                    matched_here = true;
                }
                keep.push(!drop_slot);
            }
            if !matched_here {
                continue;
            }
            changed = true;
            let provider = acct
                .get("provider")
                .and_then(aterm_toml::Value::as_str)
                .unwrap_or("ai");
            let email = acct
                .get("email")
                .and_then(aterm_toml::Value::as_str)
                .unwrap_or("unknown");
            let candidate = format!("{provider}:{email}");
            if !exposed_accounts.contains(&candidate) {
                exposed_accounts.push(candidate);
            }

            for parallel_key in ["warm", "since", "why", "used"] {
                if let Some(aterm_toml::Value::Array(mut arr)) = acct.remove(parallel_key) {
                    if arr.len() == old_len {
                        let mut idx = 0usize;
                        arr.retain(|_| {
                            let k = keep[idx];
                            idx += 1;
                            k
                        });
                    }
                    acct.insert(parallel_key.to_owned(), aterm_toml::Value::Array(arr));
                }
            }
        }
        root.insert("account".to_owned(), aterm_toml::Value::Array(accounts));
    }

    let rendered = if changed {
        Some(
            aterm_toml::to_string(&aterm_toml::Value::Table(root))
                .map_err(|e| Error::new(format!("serialize {AI_POOL_ENTRY}: {e}")))?,
        )
    } else {
        None
    };
    Ok((rendered, exposed_accounts))
}

fn apply_swarm_revoke_cascade(
    paths: &Paths,
    admin: &Admin,
    doc: &mut Document,
    host: &str,
    recipient: &Recipient,
    stolen: bool,
) -> crate::Result<CascadeSummary> {
    let recipient_str = recipient.to_bech32();
    let has_roster_entry = doc
        .entry(&Audience::Everyone, crate::swarm_roster::ROSTER_ENTRY)
        .is_some();
    let swarm_enabled = has_roster_entry
        || doc
            .entries()
            .iter()
            .any(|e| e.name == crate::swarm_roster::POLICY_ENTRY)
        || alab_vault(&admin.config);

    let mut revoked_labels = std::collections::BTreeSet::new();
    let mut revoked_nodes = std::collections::BTreeSet::new();
    let mut display_label = crate::swarm_roster::leading_m_label(host)
        .map_or_else(|| host.to_owned(), |(_, canon)| canon);
    if let Some((_, canon)) = crate::swarm_roster::leading_m_label(host) {
        revoked_labels.insert(canon);
    }
    if let Ok(Some(log)) = log_entries::trusted_local_log(paths, &admin.config) {
        let membership = log.membership();
        let mut target_actors = std::collections::BTreeSet::new();
        for (actor, member) in membership
            .members()
            .iter()
            .filter(|(_, m)| m.binding.recipient() == *recipient)
        {
            target_actors.insert(*actor);
            if let Some((_, canon)) = crate::swarm_roster::leading_m_label(&member.label) {
                if crate::swarm_roster::leading_m_label(&display_label).is_none() {
                    display_label = canon.clone();
                }
                revoked_labels.insert(canon);
            }
            if let Some(nk) = member.node_key
                && nk != [0; 32]
            {
                revoked_nodes.insert(aterm_codec::hex::encode_lower(&nk));
            }
        }
        for (nk, standing) in membership.nodes() {
            if *nk != [0; 32]
                && matches!(
                    standing,
                    crate::membership_log::NodeStanding::Member(a)
                        | crate::membership_log::NodeStanding::Retired(a)
                        | crate::membership_log::NodeStanding::Removed(a)
                        if target_actors.contains(a)
                )
            {
                revoked_nodes.insert(aterm_codec::hex::encode_lower(nk));
            }
        }
        for founder in membership
            .unproved()
            .iter()
            .filter(|f| f.recipient == *recipient)
        {
            if let Some((_, canon)) = crate::swarm_roster::leading_m_label(&founder.label) {
                if crate::swarm_roster::leading_m_label(&display_label).is_none() {
                    display_label = canon.clone();
                }
                revoked_labels.insert(canon);
            }
            if let Some(nk) = founder.node_key
                && nk != [0; 32]
            {
                revoked_nodes.insert(aterm_codec::hex::encode_lower(&nk));
            }
        }
    }

    if swarm_enabled {
        let log_lbls = roster::log_labels(paths);
        let mut roster = roster::load_or_seed_swarm_roster_with_log(admin, doc, &log_lbls)?;
        let mut kept = Vec::with_capacity(roster.members.len());
        for member in roster.members {
            if member.recipient == recipient_str || member.vault_name == host {
                display_label = member.label.clone();
                revoked_labels.insert(member.label);
                for alias in member.aliases {
                    revoked_labels.insert(alias);
                }
                if let Some(claimed) = member.claimed_label {
                    revoked_labels.insert(claimed);
                }
                if let Some(node_id) = member.node_id {
                    revoked_nodes.insert(node_id);
                }
                if let Some(node_key) = member.node_key {
                    revoked_nodes.insert(node_key);
                }
            } else {
                kept.push(member);
            }
        }
        roster.members = kept;
        roster.deny_labels.extend(revoked_labels.iter().cloned());
        roster.deny_nodes.extend(revoked_nodes);
        roster.canonicalize();
        doc.set_entry(
            &Audience::Everyone,
            crate::swarm_roster::ROSTER_ENTRY,
            roster.to_toml().as_bytes(),
        )
        .map_err(|e| Error::new(e.to_string()))?;
    }

    let mut warm_accounts = Vec::new();
    if let Some(pool_entry) = doc.entry(&Audience::Everyone, AI_POOL_ENTRY) {
        let text = std::str::from_utf8(pool_entry.value.as_slice())
            .map_err(|_| Error::new(format!("{AI_POOL_ENTRY} is not UTF-8")))?;
        let (updated, exposed) = strip_labels_from_pool_toml(text, &revoked_labels)?;
        warm_accounts = exposed;
        if let Some(rendered) = updated {
            doc.set_entry(&Audience::Everyone, AI_POOL_ENTRY, rendered.as_bytes())
                .map_err(|e| Error::new(e.to_string()))?;
        }
    }

    let _ = expire_denied_swarm_lease(doc, &revoked_labels)?;

    let mut fleet_code_rotated = false;
    if stolen && let Some(code_entry) = doc.entry(&Audience::Everyone, fleet::FLEET_CODE_ENTRY) {
        let existing = std::str::from_utf8(code_entry.value.as_slice())
            .map_err(|_| Error::new(format!("{} is not UTF-8", fleet::FLEET_CODE_ENTRY)))?;
        let rotated = rotate_fleet_code_text(existing)?;
        doc.set_entry(
            &Audience::Everyone,
            fleet::FLEET_CODE_ENTRY,
            rotated.as_bytes(),
        )
        .map_err(|e| Error::new(e.to_string()))?;
        fleet_code_rotated = true;
    }

    Ok(CascadeSummary {
        display_label,
        warm_accounts,
        fleet_code_rotated,
    })
}

/// What a member's removal (F16) did to the `[everyone]` entries it reseals: the AI accounts warm
/// on the removed Mac, and whether it rotated the fleet code.
pub(super) struct MemberCascade {
    /// `provider:email` of each account that held a warm slot, lease or `last_heard` of the Mac.
    pub(super) warm_accounts: Vec<String>,
    /// `aterm-fleet-code` was rotated (`--stolen`).
    pub(super) fleet_code_rotated: bool,
}

/// The revoke cascade of §7.6 (`DESIGN-alab-swarm`), as any member writes it into the `[everyone]`
/// entries its removal reseals (F16): the removed Mac's `aterm-swarm-roster` row is dropped and
/// its `ai-pool.toml` slots are freed. On `--stolen` its labels, aliases, claim and node id also
/// go to the roster's deny-lists and the fleet code rotates. A plain removal denies nothing: an
/// "Allow again" restores it, and the readers that hold the log overlay its removals onto the
/// roster as they read it (`roster::overlay_log_membership`), which a restoring `Admit` lifts,
/// while a deny-list row would keep the restored Mac out of the roster for good. A member seeds
/// no roster that is not there. `labels` are the removed Mac's labels besides its roster row's;
/// only bare `m<N>` ones are denied, since a suffixed log label's bare form (`m40` of
/// `m40-3fa2`) can be another Mac's.
///
/// # Errors
/// The roster, pool or fleet code entry is not what it should be.
pub(super) fn cascade_member_removal(
    entries: &mut BTreeMap<String, Entry>,
    recipient: &Recipient,
    labels: &std::collections::BTreeSet<String>,
    node_keys: &std::collections::BTreeSet<String>,
    stolen: bool,
) -> crate::Result<MemberCascade> {
    let recipient_str = recipient.to_bech32();
    let mut revoked_labels: std::collections::BTreeSet<String> = labels
        .iter()
        .filter(|label| crate::membership_log::is_label(label))
        .cloned()
        .collect();
    if let Some(entry) = entries.get_mut(crate::swarm_roster::ROSTER_ENTRY) {
        let text = std::str::from_utf8(entry.value.as_slice()).map_err(|_| {
            Error::new(format!(
                "{} is not UTF-8",
                crate::swarm_roster::ROSTER_ENTRY
            ))
        })?;
        let mut roster = crate::swarm_roster::SwarmRoster::from_toml(text)?;
        let mut kept = Vec::with_capacity(roster.members.len());
        for member in roster.members {
            if member.recipient == recipient_str {
                revoked_labels.insert(member.label);
                revoked_labels.extend(member.aliases);
                revoked_labels.extend(member.claimed_label);
                if stolen {
                    if let Some(node_id) = member.node_id {
                        roster.deny_nodes.push(node_id);
                    }
                    if let Some(node_key) = member.node_key {
                        roster.deny_nodes.push(node_key);
                    }
                }
            } else {
                kept.push(member);
            }
        }
        roster.members = kept;
        if stolen {
            roster.deny_labels.extend(
                revoked_labels
                    .iter()
                    .filter(|label| !roster.deny_labels.contains(label))
                    .cloned()
                    .collect::<Vec<_>>(),
            );
            roster.deny_nodes.extend(
                node_keys
                    .iter()
                    .filter(|nk| !roster.deny_nodes.contains(nk))
                    .cloned()
                    .collect::<Vec<_>>(),
            );
        }
        roster.canonicalize();
        entry.value = SecretBytes::from(roster.to_toml());
    }
    if let Some(entry) = entries.get_mut(crate::swarm_roster::LEASE_ENTRY) {
        let text = std::str::from_utf8(entry.value.as_slice()).map_err(|_| {
            Error::new(format!("{} is not UTF-8", crate::swarm_roster::LEASE_ENTRY))
        })?;
        let mut lease = crate::swarm_roster::SwarmLease::from_toml(text)?;
        let holder_revoked = lease.until > 0
            && (revoked_labels
                .iter()
                .any(|d| d.eq_ignore_ascii_case(&lease.holder))
                || crate::swarm_roster::leading_m_label(&lease.holder)
                    .is_some_and(|(_, canon)| revoked_labels.contains(&canon)));
        if holder_revoked {
            lease.until = 0;
            entry.value = SecretBytes::from(lease.to_toml());
        }
    }
    let mut warm_accounts = Vec::new();
    if let Some(entry) = entries.get_mut(AI_POOL_ENTRY) {
        let text = std::str::from_utf8(entry.value.as_slice())
            .map_err(|_| Error::new(format!("{AI_POOL_ENTRY} is not UTF-8")))?;
        let (updated, exposed) = strip_labels_from_pool_toml(text, &revoked_labels)?;
        warm_accounts = exposed;
        if let Some(rendered) = updated {
            entry.value = SecretBytes::from(rendered);
        }
    }
    let mut fleet_code_rotated = false;
    if stolen && let Some(entry) = entries.get_mut(fleet::FLEET_CODE_ENTRY) {
        let existing = std::str::from_utf8(entry.value.as_slice())
            .map_err(|_| Error::new(format!("{} is not UTF-8", fleet::FLEET_CODE_ENTRY)))?;
        entry.value = SecretBytes::from(rotate_fleet_code_text(existing)?);
        fleet_code_rotated = true;
    }
    Ok(MemberCascade {
        warm_accounts,
        fleet_code_rotated,
    })
}

fn format_stolen_needs_you(
    before: &Document,
    host: &str,
    display_label: &str,
    warm_accounts: &[String],
) -> String {
    let is_admin = before.admins().iter().any(|name| name == host);
    let names: Vec<String> = before
        .entries()
        .into_iter()
        .filter(|entry| is_admin || entry.audience.includes(host))
        .map(|entry| entry.name)
        .collect();
    stolen_needs_you_line(&names, display_label, warm_accounts)
}

/// The one `NEEDS YOU:` line of a stolen Mac's removal: what the provider must rotate of the
/// entries named `names` (the ones the Mac could read) and the AI accounts warm there.
pub(super) fn stolen_needs_you_line(
    names: &[String],
    display_label: &str,
    warm_accounts: &[String],
) -> String {
    let mut gh_tokens = Vec::new();
    let mut has_release_ring = false;
    let mut has_developer_id = false;
    let mut has_notary = false;
    let mut has_mailbox = false;

    for name in names {
        let lower = name.to_ascii_lowercase();
        if lower.starts_with("gh_access_token")
            || lower.contains("gh-token")
            || lower.contains("github_token")
        {
            gh_tokens.push(name.clone());
        } else if lower == "release-notary" || lower == "notary" || lower.contains("notary") {
            has_notary = true;
        } else if lower.contains("developer-id")
            || lower.contains("developer_id")
            || lower.ends_with(".p12")
            || lower.contains("apple-signing")
        {
            has_developer_id = true;
        } else if lower.contains("release-ring")
            || lower.contains("release-credentials.toml")
            || lower.contains("aterm-machines.toml")
            || lower.ends_with("release.key")
        {
            has_release_ring = true;
        } else if lower == "ai-mailbox.toml"
            || lower == "mailbox.toml"
            || lower.starts_with("ai-mailbox")
        {
            has_mailbox = true;
        }
    }

    let mut items = Vec::new();
    if !gh_tokens.is_empty() {
        items.push(format!("gh tokens ({})", gh_tokens.join(", ")));
    }
    if has_release_ring {
        items.push("release ring".to_owned());
    }
    if has_developer_id {
        items.push("Developer ID".to_owned());
    }
    if has_notary {
        items.push("notary".to_owned());
    }
    if has_mailbox {
        items.push("mailbox".to_owned());
    }
    if !warm_accounts.is_empty() {
        items.push(format!("warm AI accounts ({})", warm_accounts.join(", ")));
    }
    if items.is_empty() {
        format!(
            "NEEDS YOU: rotate at the provider any credentials that left with {display_label} \
             (gh tokens, release ring, Developer ID, notary, mailbox, and any AI accounts signed in there)"
        )
    } else {
        format!(
            "NEEDS YOU: rotate at the provider what left with {display_label}: {}",
            items.join("; ")
        )
    }
}

/// Remove a stolen vault member (`aterm secrets revoke --stolen <label>`), running the full §7.6
/// cascade in a single vault publish, rotating `aterm-fleet-code`, and returning the single-line
/// `NEEDS YOU:` provider rotation list (`DESIGN-alab-swarm` §7.6, C25).
///
/// # Errors
/// Self-removal, unsupported authority demotion, unreadable custody, malformed swarm entries, or
/// failed publication.
pub fn admin_revoke_stolen(
    paths: &Paths,
    admin: &Admin,
    host: &str,
    drop_sections: bool,
) -> crate::Result<StolenRevocation> {
    let host = machine_named_in_vault(paths, admin, host)?;
    let recipient = admin
        .published_doc
        .machines()
        .into_iter()
        .find(|machine| machine.name == host)
        .ok_or_else(|| Error::new(format!("no machine {host} in the vault")))?
        .recipient;
    let recipient = Recipient::parse(&recipient).map_err(|error| Error::new(error.to_string()))?;
    let mut doc = admin.published_doc.clone();
    doc.remove_machine(&host, drop_sections)
        .map_err(|error| Error::new(error.to_string()))?;
    let cascade = apply_swarm_revoke_cascade(paths, admin, &mut doc, &host, &recipient, true)?;
    let needs_you_line = format_stolen_needs_you(
        &admin.published_doc,
        &host,
        &cascade.display_label,
        &cascade.warm_accounts,
    );
    admin_keys::validate_commons_transition(&admin.published_doc, &doc)?;
    let sequence = envelope::next_seq(admin.seq)?;
    let identity = read_identity(paths)?.ok_or_else(|| Error::new("admin identity is missing"))?;
    publication_rights(admin, &doc, &identity)?;
    admin_keys::validate_commons(&doc, &admin.commons_seed)?;
    admin.admissions.retained(&doc)?.validate(&doc, sequence)?;
    custody_readable(paths, admin)?;

    let retirement = retire_cached(paths, admin, &doc, &[recipient])?;
    remove_from_log(paths, admin, &recipient)?;
    let published = admin_publish_checked(paths, admin, &doc).map_err(|error| {
        if retirement.is_some() {
            Error::new(format!(
                "Commons membership is permanently retired; vault removal was not confirmed: {error}. Retry the same revoke command"
            ))
        } else {
            error
        }
    })?;
    Ok(StolenRevocation {
        revocation: Revocation {
            published,
            retirement,
        },
        fleet_code_rotated: cascade.fleet_code_rotated,
        needs_you_line,
    })
}

/// Finish any swarm revoke cascade left half-done in `doc` (`sync.steward.revoke`,
/// `DESIGN-alab-swarm` §7.6):
///
/// 1. Any `SwarmMember` row in `aterm-swarm-roster` whose recipient is no longer in `[machines]`
///    (or is listed on `code-only`) is moved to `deny_labels` + `deny_nodes` and removed.
/// 2. Any `m<N>` label in `deny_labels` that still holds `warm` slots, leases, or `last_heard`
///    entries in `ai-pool.toml` is stripped.
///
/// Returns `Ok(Some(summary))` when `doc` changed, or `Ok(None)` when already clean.
///
/// # Errors
/// `aterm-swarm-roster` or `ai-pool.toml` is present and malformed.
pub fn repair_half_done_revokes(doc: &mut Document) -> crate::Result<Option<String>> {
    repair_half_done_revokes_with_log(doc, None)
}

pub(super) fn repair_half_done_revokes_with_log(
    doc: &mut Document,
    log: Option<&crate::membership_log::Log>,
) -> crate::Result<Option<String>> {
    let active_recipients: std::collections::BTreeSet<String> =
        doc.machines().into_iter().map(|m| m.recipient).collect();
    let code_only: std::collections::BTreeSet<String> = doc.code_only().into_iter().collect();

    let mut denied_labels = std::collections::BTreeSet::new();
    let mut removed_members = Vec::new();
    let mut roster_changed = false;

    let mut log_removed_labels = std::collections::BTreeSet::new();
    let mut log_removed_nodes = std::collections::BTreeSet::new();
    if let Some(log) = log {
        let membership = log.membership();
        for label in membership.removed_labels() {
            if let Some((_, canon)) = crate::swarm_roster::leading_m_label(label) {
                log_removed_labels.insert(canon);
            }
        }
        for (nk, standing) in membership.nodes() {
            if matches!(standing, crate::membership_log::NodeStanding::Removed(_)) && *nk != [0; 32]
            {
                log_removed_nodes.insert(aterm_codec::hex::encode_lower(nk));
            }
        }
        for (actor, member) in membership.members() {
            if log.audience_left_out(&crate::membership_log::EpochAudience::Everyone, actor) {
                if let Some((_, canon)) = crate::swarm_roster::leading_m_label(&member.label)
                    .or_else(|| crate::swarm_roster::leading_m_label(&member.name))
                {
                    log_removed_labels.insert(canon);
                }
                if let Some(nk) = member.node_key.filter(|k| *k != [0; 32]) {
                    log_removed_nodes.insert(aterm_codec::hex::encode_lower(&nk));
                }
            }
        }
        for founder in membership.unproved() {
            if log.audience_left_out_recipient(
                &crate::membership_log::EpochAudience::Everyone,
                &founder.recipient,
            ) {
                if let Some((_, canon)) = crate::swarm_roster::leading_m_label(&founder.label)
                    .or_else(|| crate::swarm_roster::leading_m_label(&founder.name))
                {
                    log_removed_labels.insert(canon);
                }
                if let Some(nk) = founder.node_key.filter(|k| *k != [0; 32]) {
                    log_removed_nodes.insert(aterm_codec::hex::encode_lower(&nk));
                }
            }
        }
    }

    let mut lifted = Vec::new();
    if let Some(mut roster) = crate::swarm_roster::SwarmRoster::from_document(doc)? {
        // A Mac the log let back in (Allow again, a quorum's Undo) loses the deny rows its removal
        // wrote, here in the entry 0.145 Macs read as well as in every reader's overlay.
        if let Some(log) = log {
            let before = (roster.deny_labels.len(), roster.deny_nodes.len());
            lifted = super::roster::lift_restored_from_roster(log, &mut roster);
            if (roster.deny_labels.len(), roster.deny_nodes.len()) != before {
                roster_changed = true;
            }
        }
        for existing_denied in &roster.deny_labels {
            denied_labels.insert(existing_denied.clone());
        }
        let existing_deny_labels = roster.deny_labels.clone();
        let existing_deny_nodes = roster.deny_nodes.clone();
        let is_label_denied = |lbl: &str| {
            existing_deny_labels
                .iter()
                .any(|d| d.eq_ignore_ascii_case(lbl.trim()))
        };
        let is_node_denied = |n: &str| {
            let trimmed = n.trim();
            existing_deny_nodes
                .iter()
                .any(|d| d.eq_ignore_ascii_case(trimmed))
                || log_removed_nodes
                    .iter()
                    .any(|d| d.eq_ignore_ascii_case(trimmed))
        };
        let mut kept = Vec::with_capacity(roster.members.len());
        for member in roster.members {
            let stale = !active_recipients.contains(&member.recipient)
                || code_only.contains(&member.recipient)
                || is_label_denied(&member.label)
                || member.aliases.iter().any(|a| is_label_denied(a))
                || member.node_id.as_deref().is_some_and(&is_node_denied)
                || member.node_key.as_deref().is_some_and(&is_node_denied)
                || log.is_some_and(|l| {
                    Recipient::parse(&member.recipient).is_ok_and(|rec| {
                        l.recipient_is_removed(&rec)
                            || l.audience_left_out_recipient(
                                &crate::membership_log::EpochAudience::Everyone,
                                &rec,
                            )
                            || l.membership()
                                .members()
                                .iter()
                                .filter(|(_, m)| m.binding.recipient() == rec)
                                .max_by_key(|(actor, m)| {
                                    (l.record_index(&m.entry).unwrap_or(0), *actor)
                                })
                                .is_some_and(|(_, m)| {
                                    m.node_key.is_some_and(|nk| {
                                        nk != [0; 32]
                                            && is_node_denied(&aterm_codec::hex::encode_lower(&nk))
                                    })
                                })
                            || l.membership().unproved().iter().any(|f| {
                                f.recipient == rec
                                    && f.node_key.is_some_and(|nk| {
                                        nk != [0; 32]
                                            && is_node_denied(&aterm_codec::hex::encode_lower(&nk))
                                    })
                            })
                    })
                });
            if stale {
                roster_changed = true;
                removed_members.push(member.label.clone());
                denied_labels.insert(member.label.clone());
                roster.deny_labels.push(member.label);
                for alias in member.aliases {
                    denied_labels.insert(alias.clone());
                    roster.deny_labels.push(alias);
                }
                if let Some(claimed) = member.claimed_label {
                    denied_labels.insert(claimed.clone());
                    roster.deny_labels.push(claimed);
                }
                if let Some(node_id) = member.node_id {
                    roster.deny_nodes.push(node_id);
                }
                if let Some(node_key) = member.node_key {
                    roster.deny_nodes.push(node_key);
                }
            } else {
                kept.push(member);
            }
        }
        let surviving_labels: std::collections::BTreeSet<String> = kept
            .iter()
            .flat_map(|m| std::iter::once(m.label.clone()).chain(m.aliases.iter().cloned()))
            .collect();
        for canon in &log_removed_labels {
            if !surviving_labels.contains(canon) {
                if denied_labels.insert(canon.clone()) {
                    removed_members.push(canon.clone());
                }
                if !roster.deny_labels.contains(canon) {
                    roster.deny_labels.push(canon.clone());
                    roster_changed = true;
                }
            }
        }
        for node_hex in &log_removed_nodes {
            if !roster.deny_nodes.contains(node_hex) {
                roster.deny_nodes.push(node_hex.clone());
                roster_changed = true;
            }
        }
        if roster_changed {
            roster.members = kept;
            roster.canonicalize();
            doc.set_entry(
                &Audience::Everyone,
                crate::swarm_roster::ROSTER_ENTRY,
                roster.to_toml().as_bytes(),
            )
            .map_err(|e| Error::new(e.to_string()))?;
        }
    } else {
        denied_labels.extend(log_removed_labels);
    }

    let mut pool_changed = false;
    if let Some(pool_entry) = doc.entry(&Audience::Everyone, AI_POOL_ENTRY) {
        let text = std::str::from_utf8(pool_entry.value.as_slice())
            .map_err(|_| Error::new(format!("{AI_POOL_ENTRY} is not UTF-8")))?;
        let (updated, _) = strip_labels_from_pool_toml(text, &denied_labels)?;
        if let Some(rendered) = updated {
            doc.set_entry(&Audience::Everyone, AI_POOL_ENTRY, rendered.as_bytes())
                .map_err(|e| Error::new(e.to_string()))?;
            pool_changed = true;
        }
    }

    let lease_expired = expire_denied_swarm_lease(doc, &denied_labels)?;

    if !roster_changed && !pool_changed && !lease_expired {
        return Ok(None);
    }
    let mut parts = Vec::new();
    if !lifted.is_empty() {
        parts.push(format!(
            "lifted the deny rows of {}, which the membership log let back in",
            lifted.join(", ")
        ));
    }
    if !removed_members.is_empty() {
        parts.push(format!(
            "retired {} from aterm-swarm-roster onto the deny-list",
            removed_members.join(", ")
        ));
    }
    if pool_changed {
        parts.push("released denied warm slots and leases in ai-pool.toml".to_owned());
    }
    if lease_expired {
        parts.push("expired steward lease held by denied member".to_owned());
    }
    Ok(Some(parts.join("; ")))
}

fn expire_denied_swarm_lease(
    doc: &mut Document,
    denied_labels: &std::collections::BTreeSet<String>,
) -> crate::Result<bool> {
    let Some(mut lease) = crate::swarm_roster::SwarmLease::from_document(doc)? else {
        return Ok(false);
    };
    if lease.until == 0 {
        return Ok(false);
    }
    let roster = crate::swarm_roster::SwarmRoster::from_document(doc)?;
    let holder_denied = denied_labels
        .iter()
        .any(|d| d.eq_ignore_ascii_case(&lease.holder))
        || crate::swarm_roster::leading_m_label(&lease.holder)
            .is_some_and(|(_, canon)| denied_labels.contains(&canon))
        || roster.as_ref().is_some_and(|r| {
            r.is_label_denied(&lease.holder)
                || r.is_node_denied(&lease.holder)
                || r.member_by_label_or_alias(&lease.holder).is_some_and(|m| {
                    r.is_label_denied(&m.label)
                        || m.aliases.iter().any(|a| r.is_label_denied(a))
                        || m.node_id.as_deref().is_some_and(|n| r.is_node_denied(n))
                        || m.node_key.as_deref().is_some_and(|k| r.is_node_denied(k))
                })
        });
    if !holder_denied {
        return Ok(false);
    }
    let Some(audience) = doc
        .entries()
        .into_iter()
        .find(|entry| entry.name == crate::swarm_roster::LEASE_ENTRY)
        .map(|entry| entry.audience)
    else {
        return Ok(false);
    };
    lease.until = 0;
    doc.set_entry(
        &audience,
        crate::swarm_roster::LEASE_ENTRY,
        lease.to_toml().as_bytes(),
    )
    .map_err(|e| Error::new(e.to_string()))?;
    Ok(true)
}

#[cfg(test)]
mod tests;
