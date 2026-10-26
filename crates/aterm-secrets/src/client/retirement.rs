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
        let node = crate::commons::roster::rotate_retiring_recipients(
            &self.commons_seed,
            &self.commons_eligibility,
            journal,
            grants,
            &[],
            &targets.iter().copied().collect::<Vec<_>>(),
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
pub fn admin_revoke(
    paths: &Paths,
    admin: &Admin,
    host: &str,
    drop_sections: bool,
) -> crate::Result<Revocation> {
    let host = machine_named(&admin.published_doc, host)?;
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
    admin_keys::validate_commons_transition(&admin.published_doc, &doc)?;
    let sequence = envelope::next_seq(admin.seq)?;
    let identity = read_identity(paths)?.ok_or_else(|| Error::new("admin identity is missing"))?;
    publication_rights(admin, &doc, &identity)?;
    admin_keys::validate_commons(&doc, &admin.commons_seed)?;
    admin.admissions.retained(&doc)?.validate(&doc, sequence)?;
    // A refused locker rewrite comes before the permanent retirement, not after it.
    custody_readable(paths, admin)?;

    let retirement = retire_cached(paths, admin, &doc, &[recipient])?;
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
            let host = machine_named(&admin.published_doc, subject)?;
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
    let journal = Journal::open_existing(bootstrap.journal_dir(), bootstrap.trust())?;
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

#[cfg(test)]
mod tests;
