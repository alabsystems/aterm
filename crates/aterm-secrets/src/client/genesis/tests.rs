// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::commons::binding::{Pending, Request};
use crate::commons::journal::Journal;
use crate::membership_log::{Inserted, Log, MemberId};

/// A scratch lab on a vault file: the admin `m22`, whose own key stands in for the ALab vault's.
struct Lab {
    root: aterm_tempfile::TempDir,
    paths: Paths,
    pinned: String,
    lineage: [u8; 32],
}

impl Lab {
    fn new() -> Self {
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(&root.path().join("m22"));
        let source = root.path().join("alab.vault");
        let created =
            test_create_unbound(&paths, source.to_str().unwrap(), None, Some("m22")).unwrap();
        // As a vault from before Commons custody: `admin_commons_init` founds its Commons.
        admin_keys::legacy::publish_legacy(&paths, &admin_open(&paths).unwrap());
        Self {
            pinned: format!("{}#{}", source.display(), created.signer),
            lineage: decode_key32(&created.signer).unwrap(),
            root,
            paths,
        }
    }

    /// `host` joins with its own identity once the admin puts it in `[machines]`.
    fn join(&self, host: &str) -> Paths {
        let machine = Paths::for_home(&self.root.path().join(host));
        let recipient = machine_identity(&machine).unwrap().recipient().to_bech32();
        let admin = admin_open(&self.paths).unwrap();
        let mut doc = admin.doc.clone();
        doc.add_machine(host, &recipient).unwrap();
        admin_publish(&self.paths, &admin, &doc).unwrap();
        join(
            &machine,
            &JoinOptions {
                source: self.pinned.clone(),
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

    /// The admin enrolls `machine` in Commons by the possession challenge: Commons then holds a
    /// binding for it.
    fn enroll(&self, machine: &Paths) {
        sync(machine, None).unwrap();
        let owner = Bootstrap::required(&self.paths).unwrap();
        let peer = Bootstrap::required(machine).unwrap();
        let request = peer.request().unwrap();
        let (mut pending, challenge) =
            Pending::issue(&request, owner.trust().network, owner.trust().authority).unwrap();
        let answer = peer.device().answer(&request, &challenge).unwrap();
        let mut journal = Journal::open_existing(owner.journal_dir(), owner.trust()).unwrap();
        admin_cached(&self.paths)
            .unwrap()
            .enroll_commons_member(&owner, &mut journal, pending.verify(&answer).unwrap())
            .unwrap();
    }

    fn recipients(&self) -> BTreeSet<Recipient> {
        admin_open(&self.paths)
            .unwrap()
            .doc
            .machines()
            .iter()
            .map(|machine| Recipient::parse(&machine.recipient).unwrap())
            .collect()
    }
}

#[test]
fn every_current_member_is_in_after_genesis_v2() {
    let lab = Lab::new();
    // m7 is in the vault when Commons is founded and enrolls; air joins later and holds no
    // binding.
    let m7 = lab.join("m7");
    admin_commons_init(&lab.paths).unwrap();
    lab.enroll(&m7);
    let air = lab.join("air");
    let out = lab.root.path().join("alab.genesis");

    // Only a released aterm signs it, and a refusal writes nothing.
    assert!(sign_genesis(&lab.paths, &out, &lab.lineage, false).is_err());
    assert!(!out.exists());
    // Only the ALab vault's key signs it.
    assert!(sign_genesis(&lab.paths, &out, &[1; 32], true).is_err());
    assert!(!out.exists());

    let signed = sign_genesis(&lab.paths, &out, &lab.lineage, true).unwrap();
    let record = Record::decode(&std::fs::read(&out).unwrap()).unwrap();
    assert_eq!(*record.id(), signed.digest);
    assert_eq!(signed.members, 3);
    // Once: a second run refuses, and the file holds the first record.
    assert!(sign_genesis(&lab.paths, &out, &lab.lineage, true).is_err());
    assert_eq!(std::fs::read(&out).unwrap(), record.bytes());

    // Every [machines] recipient is a founder: m22 and m7 with their bindings, air with none.
    let crate::membership_log::Body::Genesis { founders, .. } = record.body() else {
        panic!("not a genesis");
    };
    let listed: BTreeSet<Recipient> = founders.iter().map(|f| f.recipient).collect();
    assert_eq!(listed, lab.recipients());
    let air_recipient = machine_identity(&air).unwrap().recipient();
    for founder in founders {
        assert_eq!(
            founder.binding.is_some(),
            founder.recipient != air_recipient,
            "{}",
            founder.name
        );
    }

    // air proves its device key to m7, which lets it in: every current member is then in.
    let mut log = Log::new(record).unwrap();
    let member = Bootstrap::required(&m7).unwrap();
    let by: MemberId = member.request().unwrap().binding().actor();
    let trust = member.trust();
    let seed = [44; 32];
    let request = Request::create(
        trust.network,
        trust.authority,
        [44; 16],
        [45; 16],
        &machine_identity(&air).unwrap(),
        &seed,
    )
    .unwrap();
    let (proof, challenge) = log.challenge(&request, [46; 32], "air".into()).unwrap();
    let answer = request
        .answer(&machine_identity(&air).unwrap(), &seed, &challenge)
        .unwrap();
    let draft = proof.admit(&log, &answer, &by, 2).unwrap();
    let incarnation = *member.request().unwrap().binding().incarnation();
    let admit = member
        .device()
        .with_seed(|seed| draft.sign(seed, incarnation))
        .unwrap();
    assert_eq!(log.insert(admit).unwrap(), Inserted::Held);
    let membership = log.membership();
    let held: BTreeSet<Recipient> = membership
        .members()
        .values()
        .map(|m| m.binding.recipient())
        .collect();
    assert_eq!(held, lab.recipients());
    assert!(membership.unproved().is_empty());
}
