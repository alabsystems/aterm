// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Approved enrollment is a local journal transaction. Its signed result may
//! be sent only after ingestion; a lost reply retries the retained binding.

use super::*;
use crate::commons::{
    ENTRY,
    binding::{Binding, Possession},
    bootstrap::Bootstrap,
    constraints::Fact,
    hearing::{Gate, Ledger, RECENT},
    journal::Journal,
    node::Node,
    roster::{self, Access, Grant, MAX_MEMBERS, Role},
};
use astream_wire::sync::Actor;

fn now_ms() -> crate::Result<u64> {
    aterm_time::unix_millis()
        .ok_or_else(|| Error::new("system clock cannot represent a Commons contact time"))
}

// A local plan is never retained across peer I/O or exposed as a grant.
enum Candidate {
    Retained(Node),
    New { host: String, grants: Vec<Grant> },
}

pub(super) fn include_approved(
    doc: &Document,
    approved: &[(String, AdmissionMethod)],
) -> crate::Result<Option<Document>> {
    if admin_keys::control_descriptor(doc)?.is_none() {
        return Ok(None);
    }
    let Some(Audience::Hosts(old)) = doc.commons_audience() else {
        return Ok(None);
    };
    let mut hosts = old.clone();
    // Every approval, an open one included (owner, 2026-10-26: "always approve by
    // default"), shares the root with the machine it lets in.
    for (recipient, _) in approved {
        let machine = doc
            .machines()
            .into_iter()
            .find(|m| &m.recipient == recipient)
            .ok_or_else(|| Error::new("approved Commons recipient is not in the vault"))?;
        if !machine_denied_in_doc(doc, &machine.name, &machine.recipient)
            && !hosts.contains(&machine.name)
        {
            hosts.push(machine.name);
        }
    }
    if &hosts == old {
        return Ok(None);
    }
    let value = doc
        .entries()
        .into_iter()
        .find(|e| e.name == ENTRY)
        .ok_or_else(|| Error::new("Commons control entry disappeared"))?
        .value;
    let mut shared = doc.clone();
    shared
        .set_entry(&Audience::Hosts(hosts), ENTRY, value.as_slice())
        .map_err(|e| Error::new(e.to_string()))?;
    Ok(Some(shared))
}

impl Admin {
    /// Check current local eligibility before issuing a possession challenge.
    /// This signs nothing and grants no authority. Reopen the admin snapshot and
    /// journal after the peer replies; enrollment repeats every policy check.
    pub fn check_commons_enrollment(
        &self,
        bootstrap: &Bootstrap,
        journal: &Journal,
        binding: &Binding,
    ) -> crate::Result<()> {
        self.enrollment_candidate(bootstrap, journal, binding)?;
        Ok(())
    }

    fn enrollment_candidate(
        &self,
        bootstrap: &Bootstrap,
        journal: &Journal,
        binding: &Binding,
    ) -> crate::Result<Candidate> {
        if bootstrap.trust() != journal.trust() {
            return Err(Error::new("Commons enrollment has another local root"));
        }
        if !self
            .admissions
            .permits(journal.trust(), &binding.recipient())
        {
            return Err(Error::new(
                "Commons enrollment needs authenticated admission evidence",
            ));
        }
        let machines = self.published_doc.machines();
        let host = machines
            .iter()
            .find(|m| Recipient::parse(&m.recipient).ok() == Some(binding.recipient()))
            .ok_or_else(|| Error::new("Commons enrollment recipient is not admitted"))?
            .name
            .clone();
        if self.is_denied_in_published_roster(&host, &binding.recipient())
            || self.is_binding_node_denied_in_published_roster(binding)
        {
            return Err(Error::new("Commons enrollment recipient is not admitted"));
        }
        crate::commons::membership::check_candidate(
            &self.commons_seed,
            journal,
            &self.commons_eligibility,
            &host,
            binding,
        )?;
        let access = bootstrap
            .access_in(journal)?
            .ok_or_else(|| Error::new("Commons enrollment needs a current local roster"))?;
        if let Some(member) = access
            .members()
            .iter()
            .find(|member| member.binding().recipient() == binding.recipient())
        {
            if member.binding() != binding {
                return Err(Error::new(
                    "Commons enrollment cannot replace an existing device",
                ));
            }
            return std::iter::once(access.basis())
                .chain(journal.inventory())
                .filter_map(|id| journal.node(id))
                .find(|node| {
                    node.facts()
                        .iter()
                        .any(|fact| matches!(fact, Fact::Bind { binding: b } if b == binding))
                })
                .cloned()
                .map(Candidate::Retained)
                .ok_or_else(|| Error::new("Commons enrolled device has no retained witness"));
        }
        access.check_addition(binding, Role::Writer)?;
        let grants = self.kept(&access, |_| false)?;
        Ok(Candidate::New { host, grants })
    }

    fn is_denied_in_published_roster(&self, host: &str, recipient: &Recipient) -> bool {
        machine_denied_in_doc(&self.published_doc, host, &recipient.to_bech32())
    }

    fn is_binding_node_denied_in_published_roster(&self, binding: &Binding) -> bool {
        let Some(roster) = crate::swarm_roster::SwarmRoster::from_document(&self.published_doc)
            .ok()
            .flatten()
        else {
            return false;
        };
        roster.is_node_denied(&aterm_codec::hex::encode_lower(binding.writer()))
            || roster.is_node_denied(&aterm_codec::hex::encode_lower(binding.incarnation()))
            || roster.is_node_denied(&aterm_codec::hex::encode_lower(binding.machine()))
    }

    /// The current members a roster write keeps, as recorded grants: all but those `leaving`,
    /// who leave the roster but not the vault. This device stays.
    fn kept(&self, access: &Access, leaving: impl Fn(&Actor) -> bool) -> crate::Result<Vec<Grant>> {
        let machines = self.published_doc.machines();
        let local = access.local().binding().actor();
        let mut grants = Vec::new();
        for member in access.members() {
            let actor = member.binding().actor();
            if actor != local && leaving(&actor) {
                continue;
            }
            let name = machines
                .iter()
                .find(|m| Recipient::parse(&m.recipient).ok() == Some(member.binding().recipient()))
                .ok_or_else(|| {
                    Error::new("retire the removed Commons member before enrolling another device")
                })?
                .name
                .clone();
            if !self
                .commons_eligibility
                .includes(&name, member.binding().recipient())
            {
                return Err(Error::new(
                    "current Commons member is outside the control audience",
                ));
            }
            grants.push(Grant::recorded(
                name,
                member.binding().clone(),
                member.role(),
            ));
        }
        Ok(grants)
    }

    /// Bring the roster in line with what this device heard (`ledger`, at `now_ms`): members
    /// silent for 30 days leave it, and recorded devices of eligible vault machines heard again
    /// within 7 days are re-enrolled as writers, in one gated rotation retained in `journal`.
    /// Members never heard are first noted in `ledger`, which starts their 30 days. While the
    /// C26 gate holds, returns stop at 63 members. `None` when nothing changes, or when an admin
    /// device with a smaller writer reported aterm 0.146 within 15 minutes (it refreshes).
    /// Retired devices never return. This performs no network I/O.
    pub fn refresh_commons_roster(
        &self,
        bootstrap: &Bootstrap,
        journal: &mut Journal,
        ledger: &mut Ledger,
        now_ms: u64,
    ) -> crate::Result<Option<Node>> {
        if bootstrap.trust() != journal.trust() {
            return Err(Error::new("Commons refresh has another local root"));
        }
        if bootstrap.is_log_backed()? {
            return Ok(None);
        }
        let Some(access) = bootstrap.access_in(journal)? else {
            return Ok(None);
        };
        ledger.know_local(access.local().binding());
        for member in access.members() {
            ledger.note(member.binding().actor(), now_ms);
        }
        let machines = self.published_doc.machines();
        let admins = recipients_of(&self.published_doc.admins(), &self.published_doc)?;
        let local = access.local().binding().actor();
        if ledger.defers(
            &local,
            access
                .members()
                .iter()
                .filter(|member| admins.contains(&member.binding().recipient()))
                .map(|member| member.binding().actor()),
            now_ms,
        ) {
            return Ok(None);
        }
        let mut grants = self.kept(&access, |actor| ledger.silent(actor, now_ms))?;
        let kept = grants.len();
        let constraints = journal.constraints()?;
        let (mut recipients, mut ids): (
            std::collections::BTreeSet<_>,
            std::collections::BTreeSet<_>,
        ) = access
            .members()
            .iter()
            .map(|m| (m.binding().recipient(), *m.binding().machine()))
            .unzip();
        let mut back = Vec::new();
        for fact in constraints.facts() {
            let Fact::Bind { binding } = fact else {
                continue;
            };
            if recipients.contains(&binding.recipient())
                || ids.contains(binding.machine())
                || !ledger.heard_within(&binding.actor(), now_ms, RECENT)
                || constraints.check_recorded_device(binding).is_err()
            {
                continue;
            }
            let Some(machine) = machines
                .iter()
                .find(|m| Recipient::parse(&m.recipient).ok() == Some(binding.recipient()))
            else {
                continue;
            };
            if !self
                .commons_eligibility
                .includes(&machine.name, binding.recipient())
                || self.is_denied_in_published_roster(&machine.name, &binding.recipient())
                || self.is_binding_node_denied_in_published_roster(binding)
                || grants.len() == MAX_MEMBERS
            {
                continue;
            }
            recipients.insert(binding.recipient());
            ids.insert(*binding.machine());
            grants.push(Grant::recorded(
                machine.name.clone(),
                binding.clone(),
                Role::Writer,
            ));
            back.push(binding);
        }
        // While the gate would refuse a roster past 63, fewer return, so those leaving still
        // leave.
        let room = ledger.room(
            roster::crossed(journal),
            access
                .members()
                .iter()
                .map(|m| m.binding())
                .chain(back.iter().copied()),
            now_ms,
        );
        grants.truncate(room.max(kept));
        if kept == access.members().len() && grants.len() == kept {
            return Ok(None);
        }
        let node = roster::rotate_gated(
            &self.commons_seed,
            &self.commons_eligibility,
            journal,
            grants,
            &[],
            &[],
            Some(&Gate { ledger, now_ms }),
        )?;
        journal.ingest(node.bytes())?;
        bootstrap
            .access_in(journal)?
            .ok_or_else(|| Error::new("Commons refresh did not retain a usable roster"))?;
        Ok(Some(node))
    }

    /// Enroll a jointly proven, durably approved device as a Commons writer.
    /// Preserve all current members and roles, refusing key replacement or an
    /// unavailable/forked roster; a roster past 63 members passes the C26 gate ([`Gate`]).
    /// The returned authority witness is durable in `journal`; losing the network reply
    /// cannot mint another epoch on retry.
    /// This performs no network I/O and grants no administrative or drive rights.
    pub fn enroll_commons_member(
        &self,
        bootstrap: &Bootstrap,
        journal: &mut Journal,
        possession: Possession,
    ) -> crate::Result<Node> {
        let binding = possession.binding().clone();
        let (host, mut grants) = match self.enrollment_candidate(bootstrap, journal, &binding)? {
            Candidate::Retained(node) => return Ok(node),
            Candidate::New { host, grants } => (host, grants),
        };
        let certificate = self.auto_approve_commons_member(journal, &host, possession)?;
        grants.push(Grant::certified(host, certificate, Role::Writer));
        let ledger = Ledger::read(bootstrap);
        let node = roster::rotate_gated(
            &self.commons_seed,
            &self.commons_eligibility,
            journal,
            grants,
            &[],
            &[],
            Some(&Gate {
                ledger: &ledger,
                now_ms: now_ms()?,
            }),
        )?;
        journal.ingest(node.bytes())?;
        let current = bootstrap
            .access_in(journal)?
            .ok_or_else(|| Error::new("Commons enrollment did not retain a usable roster"))?;
        if !current.members().iter().any(|m| m.binding() == &binding) {
            return Err(Error::new(
                "Commons enrollment did not retain the requested device",
            ));
        }
        Ok(node)
    }
}

pub(super) fn machine_denied_in_doc(doc: &Document, host: &str, recipient_str: &str) -> bool {
    let Some(roster) = crate::swarm_roster::SwarmRoster::from_document(doc)
        .ok()
        .flatten()
    else {
        return false;
    };
    let rec_trimmed = recipient_str.trim();
    if let Some(row) = roster.member_by_recipient(rec_trimmed) {
        return steward::member_is_denied_in_roster(&roster, row);
    }
    roster.is_label_denied(host)
        || roster
            .member_by_label_or_alias(host)
            .is_some_and(|row| steward::member_is_denied_in_roster(&roster, row))
}

/// Open the exact locally applied admin snapshot without contacting a server,
/// repairing materialized files, or creating an identity. LAN enrollment must
/// remain available while the compatibility vault server is offline.
pub fn admin_cached(paths: &Paths) -> crate::Result<Admin> {
    admin_cached_with_lock(paths, StateLock::observe(paths)?)
}

/// Observe already-applied local root authority within the caller's deadline.
/// This never fetches a server, changes setup, or materializes a credential.
pub fn admin_cached_until(paths: &Paths, deadline: Instant) -> crate::Result<Admin> {
    admin_cached_with_lock(paths, StateLock::observe_until(paths, deadline)?)
}

fn admin_cached_with_lock(paths: &Paths, lock: Option<StateLock>) -> crate::Result<Admin> {
    let lock = lock.ok_or_else(|| Error::new("vault state is changing; retry enrollment"))?;
    admin_cached_held(paths, &lock)
}

pub(super) fn admin_cached_unlocked(paths: &Paths) -> crate::Result<Admin> {
    admin_cached_observed(paths, None)
}

pub(super) fn admin_cached_held(paths: &Paths, lock: &StateLock) -> crate::Result<Admin> {
    admin_cached_observed(paths, Some(lock))
}

fn admin_cached_observed(paths: &Paths, lock: Option<&StateLock>) -> crate::Result<Admin> {
    let config = config(paths)?.ok_or_else(|| Error::new("no cached vault configured"))?;
    let pin = config
        .signer
        .ok_or_else(|| Error::new("a plain vault has no admin authority"))?;
    let state = load_state(paths)?;
    if state.membership != Stored::In
        || state.failed
        || !state.pending.is_empty()
        || !source_bound(&config, &state)
        || !pin_admitted(&config, None)
    {
        return Err(Error::new(
            "cached admin requires a completely applied, admitted vault",
        ));
    }
    let held_vault = super::chain_cache::held(&paths.cache_file())?
        .ok_or_else(|| Error::new("no cached admin vault"))?;
    let held = floor(paths, &state, &config.name, &config);
    let unchained = held.as_ref().map(|floor| Floor {
        chain: None,
        ..floor.clone()
    });
    // The held vault's own parse and signature fact, kept with its exact bytes
    // (`chain_cache`): the name, pin and floor are judged on every call.
    let (envelope, pinned, _) =
        held_vault.verify_for_reader(&config.name, &pin, &config.followed, unchained.as_ref())?;
    if envelope.seq != state.seq || Some(envelope.digest()) != state.digest {
        return Err(Error::new("cached admin vault is not the applied version"));
    }
    let handoff = match held.and_then(|floor| floor.chain) {
        Some((signer, chain)) if !matches!(pinned, Pin::Replaced(_)) => {
            envelope.chain_carried_on(&signer, &chain)?
        }
        _ => envelope.handoff.clone(),
    };
    let identity =
        read_identity(paths)?.ok_or_else(|| Error::new("cached admin identity is missing"))?;
    let locker = envelope
        .admin_locker()
        .ok_or_else(|| Error::new("cached vault has no admin locker"))?;
    let plain = age::decrypt(std::slice::from_ref(&identity), locker.armored.as_bytes())
        .map_err(|_| Error::new("this device cannot open cached admin custody"))?;
    let opened = parse_admin(&plain)?;
    opened.admissions.validate(&opened.doc, envelope.seq)?;
    if envelope::signer_public(&opened.seed)? != envelope.signer {
        return Err(Error::new("cached admin custody differs from its signer"));
    }
    if super::admin_removed_in_log_with_lock(paths, &config, envelope, &identity, &opened.doc, lock)
    {
        return Err(Error::new("this device cannot open cached admin custody"));
    }
    let served_machines = opened
        .doc
        .machines()
        .into_iter()
        .map(|machine| machine.recipient)
        .collect();
    let mut doc = opened.doc.clone();
    let mut admissions = opened.admissions.clone();
    // A cached open lands no pending set: its callers may hold the state lock, or an author's
    // own observation of it (`log_entries::view_waiting`).
    let AdminBase {
        log_base,
        shown,
        version,
    } = admin_base(
        paths,
        &config,
        envelope,
        &handoff,
        &identity,
        &opened,
        &mut doc,
        &mut admissions,
        false,
    )?;
    Ok(Admin {
        commons_eligibility: crate::commons::membership::Eligibility::from_document(&doc),
        admissions,
        published_doc: doc.clone(),
        version,
        published_control: super::inspection::PublishedControl::read(&opened.doc)?,
        served_machines,
        doc,
        config,
        seed: opened.seed,
        commons_seed: opened.commons_seed,
        commons_bound: opened.commons_bound,
        custody: opened.custody,
        seq: envelope.seq,
        handoff,
        opened_pairs: envelope.handoff.len(),
        opened_heads: ledger_publish::extend_parent_spine(
            &[envelope.digest()],
            &[envelope.parents.as_slice()],
        ),
        parented: !envelope.parents.is_empty(),
        log_base,
        resolve: BTreeSet::new(),
        shown,
        overtaken: super::unshown_overtaken(paths),
    })
}

#[cfg(test)]
#[path = "enrollment_tests.rs"]
mod tests;
