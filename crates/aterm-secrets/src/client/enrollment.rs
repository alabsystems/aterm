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
    journal::Journal,
    node::Node,
    roster::{Grant, Role},
};

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
    for (recipient, method) in approved {
        if *method == AdmissionMethod::Open {
            continue;
        }
        let machine = doc
            .machines()
            .into_iter()
            .find(|m| &m.recipient == recipient)
            .ok_or_else(|| Error::new("approved Commons recipient is not in the vault"))?;
        if !hosts.contains(&machine.name) {
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
        let mut grants = Vec::new();
        for member in access.members() {
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
        Ok(Candidate::New { host, grants })
    }

    /// Enroll a jointly proven, durably approved device as a Commons writer.
    /// Preserve all current members and roles, refusing key replacement or an
    /// unavailable/forked roster. The returned authority witness is durable in
    /// `journal`; losing the network reply cannot mint another epoch on retry.
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
        let node = self.prepare_commons_rotation(journal, grants)?;
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

/// Open the exact locally applied admin snapshot without contacting a server,
/// repairing materialized files, or creating an identity. LAN enrollment must
/// remain available while the compatibility vault server is offline.
pub fn admin_cached(paths: &Paths) -> crate::Result<Admin> {
    let _lock = StateLock::observe(paths)?
        .ok_or_else(|| Error::new("vault state is changing; retry enrollment"))?;
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
    let bytes = fsx::read_private(&paths.cache_file(), envelope::MAX_ENVELOPE)?
        .ok_or_else(|| Error::new("no cached admin vault"))?;
    let held = floor(paths, &state, &config.name, &config);
    let unchained = held.as_ref().map(|floor| Floor {
        chain: None,
        ..floor.clone()
    });
    let (envelope, pinned, _) = envelope::verify_for_reader(
        bytes.as_slice(),
        &config.name,
        &pin,
        &config.followed,
        unchained.as_ref(),
    )?;
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
    Ok(Admin {
        commons_eligibility: crate::commons::membership::Eligibility::from_document(&opened.doc),
        admissions: opened.admissions.clone(),
        doc: opened.doc.clone(),
        published_doc: opened.doc.clone(),
        config,
        seed: opened.seed,
        commons_seed: opened.commons_seed,
        commons_bound: opened.commons_bound,
        custody: opened.custody,
        seq: envelope.seq,
        handoff,
        opened_pairs: envelope.handoff.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequential_approvals_rehome_one_control_audience() {
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(&root.path().join("m1"));
        create(
            &paths,
            root.path().join("vault").to_str().unwrap(),
            None,
            Some("m1"),
        )
        .unwrap();
        let mut doc = admin_cached(&paths).unwrap().doc.clone();
        let initial = doc.lockers().len();
        for n in 2..=30 {
            let recipient = Identity::from_secret_bytes([n; 32]).recipient().to_bech32();
            doc.add_machine(&format!("m{n}"), &recipient).unwrap();
            doc = include_approved(&doc, &[(recipient, AdmissionMethod::Manual)])
                .unwrap()
                .unwrap();
            assert_eq!(
                doc.lockers().len(),
                initial,
                "approval {n} left an empty old audience"
            );
        }
    }

    #[test]
    fn cached_admin_requires_applied_membership_and_exact_snapshot() {
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(&root.path().join("m1"));
        let source = root.path().join("vault");
        create(&paths, source.to_str().unwrap(), None, Some("m1")).unwrap();
        let original = load_state(&paths).unwrap();
        std::fs::remove_file(source).unwrap();
        assert!(admin_cached(&paths).is_ok(), "no source fetch is necessary");
        for mutation in 0..5 {
            let mut state = original.clone();
            match mutation {
                0 => state.failed = true,
                1 => state.membership = Stored::Revoked,
                2 => state.source_identity = None,
                3 => state.seq += 1,
                4 => state.digest = Some([17; 32]),
                _ => unreachable!(),
            }
            save_state(&paths, &state).unwrap();
            assert!(
                admin_cached(&paths).is_err(),
                "accepted state mutation {mutation}"
            );
        }
    }
}
