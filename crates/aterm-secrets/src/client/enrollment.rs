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
        if self.is_denied_in_published_roster(&host, &binding.recipient()) {
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
    roster.is_label_denied(host)
        || roster
            .members
            .iter()
            .filter(|m| {
                m.recipient.trim() == rec_trimmed
                    || m.label.eq_ignore_ascii_case(host)
                    || m.aliases.iter().any(|a| a.eq_ignore_ascii_case(host))
            })
            .any(|m| steward::member_is_denied_in_roster(&roster, m))
}

/// Open the exact locally applied admin snapshot without contacting a server,
/// repairing materialized files, or creating an identity. LAN enrollment must
/// remain available while the compatibility vault server is offline.
pub fn admin_cached(paths: &Paths) -> crate::Result<Admin> {
    admin_cached_with_lock(paths, StateLock::observe(paths)?)
}

pub(super) fn admin_cached_until(paths: &Paths, deadline: Instant) -> crate::Result<Admin> {
    admin_cached_with_lock(paths, StateLock::observe_until(paths, deadline)?)
}

fn admin_cached_with_lock(paths: &Paths, lock: Option<StateLock>) -> crate::Result<Admin> {
    let _lock = lock.ok_or_else(|| Error::new("vault state is changing; retry enrollment"))?;
    admin_cached_unlocked(paths)
}

pub(super) fn admin_cached_unlocked(paths: &Paths) -> crate::Result<Admin> {
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
    if super::admin_removed_in_log(paths, &config, &envelope, &identity, &opened.doc) {
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
    let log_base = admin_base(
        paths,
        &config,
        &envelope,
        &identity,
        &opened,
        &mut doc,
        &mut admissions,
    );
    Ok(Admin {
        commons_eligibility: crate::commons::membership::Eligibility::from_document(&doc),
        admissions,
        published_doc: doc.clone(),
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
        log_base,
        resolve: BTreeSet::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commons::binding::Pending;

    /// A vault `create` made at a path, with Commons control, and the pin a machine joins by.
    fn founded(root: &Path) -> (Paths, String) {
        let paths = Paths::for_home(&root.join("m1"));
        let source = root.join("vault");
        let created = create(&paths, source.to_str().unwrap(), None, Some("m1")).unwrap();
        (paths, format!("{}#{}", source.display(), created.signer))
    }

    /// `owner` lets `host` in as an open vault's approver does ([`auto_approve_listed`]), and
    /// `host` joins. Returns the new machine's paths.
    fn admitted_open(root: &Path, owner: &Paths, pinned: &str, host: &str) -> Paths {
        let machine = Paths::for_home(&root.join(host));
        let recipient = machine_identity(&machine).unwrap().recipient().to_bech32();
        let admin = admin_cached(owner).unwrap();
        let mut doc = admin.doc.clone();
        doc.add_machine(host, &recipient).unwrap();
        publish_as(
            owner,
            &admin,
            &doc,
            false,
            &[(recipient, AdmissionMethod::Open)],
            false,
        )
        .unwrap();
        join(
            &machine,
            &JoinOptions {
                source: pinned.into(),
                host: Some(host.into()),
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
        machine
    }

    /// `owner` publishes its vault with `hosts` as the control entry's audience.
    fn share(owner: &Paths, hosts: &[&str]) {
        let admin = admin_cached(owner).unwrap();
        let mut doc = admin.doc.clone();
        let audience = doc.commons_audience().unwrap().clone();
        let value = doc
            .entries()
            .into_iter()
            .find(|e| e.name == ENTRY)
            .unwrap()
            .value;
        doc.unset_entry(&audience, ENTRY).unwrap();
        let hosts = hosts.iter().map(|h| (*h).into()).collect();
        doc.set_entry(&Audience::Hosts(hosts), ENTRY, value.as_slice())
            .unwrap();
        admin_publish(owner, &admin, &doc).unwrap();
    }

    fn in_audience(owner: &Paths, host: &str) -> bool {
        matches!(
            admin_cached(owner).unwrap().published_doc.commons_audience(),
            Some(Audience::Hosts(hosts)) if hosts.iter().any(|h| h == host)
        )
    }

    /// A possession proof from `peer` for `owner`'s root, as the LAN handshake makes one.
    fn possession(owner: &Bootstrap, peer: &Bootstrap) -> Possession {
        let request = peer.request().unwrap();
        let trust = owner.trust();
        let (mut pending, challenge) =
            Pending::issue(&request, trust.network, trust.authority).unwrap();
        let answer = peer.device().answer(&request, &challenge).unwrap();
        pending.verify(&answer).unwrap()
    }

    /// `owner`'s cached admin enrolls `proof` into its own control journal.
    fn enroll(owner: &Paths, proof: Possession) -> crate::Result<Node> {
        let bootstrap = Bootstrap::required(owner)?;
        let mut journal = Journal::open_existing(bootstrap.journal_dir(), bootstrap.trust())?;
        admin_cached(owner)?.enroll_commons_member(&bootstrap, &mut journal, proof)
    }

    #[test]
    fn an_open_admission_shares_the_root_and_enrolls_while_in_the_audience() {
        let root = aterm_tempfile::tempdir().unwrap();
        let (owner, pinned) = founded(root.path());
        let m2 = admitted_open(root.path(), &owner, &pinned, "m2");
        let admin = admin_cached(&owner).unwrap();
        assert_eq!(
            admin.commons_admission("m2").unwrap().method,
            AdmissionMethod::Open
        );
        assert!(
            in_audience(&owner, "m2"),
            "an open approval shares the control entry in its own publication"
        );
        let peer = Bootstrap::required(&m2).unwrap();
        let proof = possession(&Bootstrap::required(&owner).unwrap(), &peer);
        // An explicit exclusion between the challenge and the enrollment still refuses.
        share(&owner, &["m1"]);
        let refused = enroll(&owner, proof).unwrap_err().to_string();
        assert!(refused.contains("eligible recipient"), "{refused}");
        // Shared again, the same open decision enrolls it.
        share(&owner, &["m1", "m2"]);
        let proof = possession(&Bootstrap::required(&owner).unwrap(), &peer);
        enroll(&owner, proof).unwrap();
        let owner_root = Bootstrap::required(&owner).unwrap();
        let journal = Journal::open_existing(owner_root.journal_dir(), owner_root.trust()).unwrap();
        let access = owner_root.access_in(&journal).unwrap().unwrap();
        assert!(
            access
                .members()
                .iter()
                .any(|m| m.binding().recipient() == machine_identity(&m2).unwrap().recipient())
        );
    }

    #[test]
    fn a_removed_open_member_put_back_by_a_plain_edit_cannot_enroll() {
        let root = aterm_tempfile::tempdir().unwrap();
        let (owner, pinned) = founded(root.path());
        let m2 = admitted_open(root.path(), &owner, &pinned, "m2");
        let peer = Bootstrap::required(&m2).unwrap();
        let recipient = machine_identity(&m2).unwrap().recipient().to_bech32();
        admin_revoke(&owner, &admin_open(&owner).unwrap(), "m2", false).unwrap();
        // Put back by an edit that records no decision, inside the audience.
        let admin = admin_cached(&owner).unwrap();
        let mut doc = admin.doc.clone();
        doc.add_machine("m2", &recipient).unwrap();
        admin_publish(&owner, &admin, &doc).unwrap();
        share(&owner, &["m1", "m2"]);
        assert!(
            admin_cached(&owner)
                .unwrap()
                .commons_admission("m2")
                .is_none(),
            "removal forgets the open decision"
        );
        let proof = possession(&Bootstrap::required(&owner).unwrap(), &peer);
        let refused = enroll(&owner, proof).unwrap_err().to_string();
        assert!(refused.contains("admission evidence"), "{refused}");
    }

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
