// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::commons::binding::Request;
use crate::membership_log::{Entrant, Log, Record};
use crate::swarm_roster::{SwarmMember, SwarmRoster};

/// A gap mark with more clocks than [`MAX_GAP_MARK`] holds (40 epochs times 60 Macs, every
/// field at its widest) keeps the first that fit, in order, and reads back with its digest
/// and due, so those gaps' ranks keep counting.
///
/// NEGATIVE CONTROL: the same mark written whole is past the bound, and the reader takes it
/// for none, which forgets every clock.
#[test]
fn a_gap_mark_past_its_bound_keeps_the_clocks_that_fit() {
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(home.path());
    fsx::ensure_private_dir(&paths.dir).unwrap();
    let mut seen = BTreeMap::new();
    for e in 0u8..40 {
        for m in 0u8..60 {
            let recipient = Identity::from_secret_bytes([m.wrapping_add(1); 32])
                .recipient()
                .to_bech32();
            seen.insert(([e; 32], recipient), (u64::MAX, u64::MAX));
        }
    }
    let digest = [7; 32];
    write_gap_mark(&paths, &digest, u64::MAX, &seen).unwrap();
    let len = std::fs::metadata(gap_mark_file(&paths)).unwrap().len();
    assert!(len <= u64::try_from(MAX_GAP_MARK).unwrap(), "{len} bytes");
    let mark = read_gap_mark(&paths).expect("the mark reads back");
    assert_eq!(mark.digest, digest);
    assert_eq!(mark.due, u64::MAX);
    assert!(!mark.seen.is_empty() && mark.seen.len() < seen.len());
    let kept: Vec<_> = seen.iter().take(mark.seen.len()).collect();
    assert_eq!(
        mark.seen.iter().collect::<Vec<_>>(),
        kept,
        "the first in order"
    );

    let mut whole = format!("{} {}\n", aterm_codec::hex::encode_lower(&digest), u64::MAX);
    for ((epoch, recipient), (first, start)) in &seen {
        whole.push_str(&format!(
            "{} {recipient} {first} {start}\n",
            aterm_codec::hex::encode_lower(epoch)
        ));
    }
    assert!(
        whole.len() > MAX_GAP_MARK,
        "CONTROL: the whole mark is past the bound"
    );
    fsx::write_atomic(&gap_mark_file(&paths), whole.as_bytes(), 0o600).unwrap();
    assert!(
        read_gap_mark(&paths).is_none(),
        "CONTROL: the whole mark reads as none"
    );
}

#[test]
fn public_roster_and_stamp_distinguish_envelopes_readers_and_parse_failures() {
    let identity = Identity::from_secret_bytes([0x61; 32]);
    let outsider = Identity::from_secret_bytes([0x62; 32]);
    let seal = |value: Vec<u8>| {
        let head = aterm_digest::Sha256::digest(&value);
        let plain = text::render_everyone_locker(&[Entry {
            name: crate::swarm_roster::ROSTER_ENTRY.into(),
            value: SecretBytes::new(value),
            audience: Audience::Everyone,
            if_absent: false,
            anchor: None,
        }]);
        let plain = stamped_text(plain.as_slice(), &stamp_line(&[head]).unwrap());
        let cipher = age::encrypt(&[identity.recipient()], plain.as_slice()).unwrap();
        envelope::parse(
            &envelope::sign(
                &envelope::Unsigned {
                    name: "alab".into(),
                    seq: 1,
                    time: 1_000,
                    handoff: Vec::new(),
                    parents: Vec::new(),
                    lockers: vec![envelope::Locker {
                        label: "1".into(),
                        armored: age::armor(&cipher),
                    }],
                },
                &[0x63; 32],
            )
            .unwrap(),
        )
        .unwrap()
    };
    let first = SwarmRoster::default();
    let original = seal(first.to_toml().into_bytes());
    let first_stamp = Stamp::Heads(vec![aterm_digest::Sha256::digest(
        first.to_toml().as_bytes(),
    )]);
    assert_eq!(envelope_stamp(&original, &identity).unwrap(), first_stamp);
    assert_eq!(
        envelope_stamp(&original, &outsider).unwrap(),
        Stamp::NoLocker
    );
    assert_eq!(
        roster_with_log(None, &original, &identity)
            .unwrap()
            .0
            .into_roster()
            .unwrap(),
        Some(first.clone())
    );
    assert!(
        roster_with_log(None, &original, &outsider)
            .unwrap()
            .0
            .into_roster()
            .unwrap()
            .is_none()
    );
    let changed = SwarmRoster {
        deny_labels: vec!["m8".into()],
        ..SwarmRoster::default()
    };
    let next = seal(changed.to_toml().into_bytes());
    let next_stamp = Stamp::Heads(vec![aterm_digest::Sha256::digest(
        changed.to_toml().as_bytes(),
    )]);
    assert_eq!(envelope_stamp(&next, &identity).unwrap(), next_stamp);
    assert_eq!(envelope_stamp(&original, &identity).unwrap(), first_stamp);
    assert_eq!(
        roster_with_log(None, &next, &identity)
            .unwrap()
            .0
            .into_roster()
            .unwrap(),
        Some(changed)
    );
    assert_eq!(
        roster_with_log(None, &original, &identity)
            .unwrap()
            .0
            .into_roster()
            .unwrap(),
        Some(first)
    );
    let malformed = seal(vec![0xff]);
    for _ in 0..2 {
        assert!(
            roster_with_log(None, &malformed, &identity)
                .unwrap()
                .0
                .into_roster()
                .is_err()
        );
    }
}

#[test]
fn roster_observation_follows_exact_encrypted_history_and_reader() {
    let writer = [0x71; 32];
    let incarnation = [0x72; 16];
    let identity = Identity::from_secret_bytes([0x73; 32]);
    let outsider = Identity::from_secret_bytes([0x74; 32]);
    let request = Request::create(
        [0x75; 16],
        [0x76; 32],
        [0x77; 16],
        incarnation,
        &identity,
        &writer,
    )
    .unwrap();
    let actor = request.binding().actor();
    let founder = Entrant {
        request,
        node_key: [0x78; 32],
        name: "m1.local".into(),
        label: "m1".into(),
    };
    let mut log = Log::new(Record::found(&[0x79; 32], vec![founder], 1_000).unwrap()).unwrap();
    let (draft, epoch_key) = log
        .draft_epoch(&actor, 1_001, EpochAudience::Everyone, 1)
        .unwrap();
    let epoch = draft.sign(&writer, incarnation).unwrap();
    let epoch_id = *epoch.id();
    log.insert(epoch).unwrap();
    let wrap = log
        .draft_wrap(&actor, 1_002, epoch_id, &epoch_key, identity.recipient())
        .unwrap()
        .sign(&writer, incarnation)
        .unwrap();
    log.insert(wrap).unwrap();
    let put = |log: &mut Log, roster: &SwarmRoster, time| {
        let entry = Entry {
            name: crate::swarm_roster::ROSTER_ENTRY.into(),
            value: SecretBytes::new(roster.to_toml().into_bytes()),
            audience: Audience::Everyone,
            if_absent: false,
            anchor: None,
        };
        let record = log
            .draft_put(&actor, time, epoch_id, &epoch_key, &entry)
            .unwrap()
            .sign(&writer, incarnation)
            .unwrap();
        log.insert(record).unwrap();
    };
    let first = SwarmRoster::default();
    put(&mut log, &first, 1_003);
    let original = log.clone();
    let empty_envelope = envelope::parse(
        &envelope::sign(
            &envelope::Unsigned {
                name: "alab".into(),
                seq: 1,
                time: 1_000,
                handoff: Vec::new(),
                parents: Vec::new(),
                lockers: Vec::new(),
            },
            &[0x79; 32],
        )
        .unwrap(),
    )
    .unwrap();
    let read = |log: &Log, who: &Identity| {
        roster_with_log(Some((log, records_heads_digest(log))), &empty_envelope, who).unwrap()
    };
    assert_eq!(
        read(&log, &identity).0.into_roster().unwrap(),
        Some(first.clone())
    );
    assert_eq!(read(&log, &identity).1, Some(records_heads_digest(&log)));
    assert_eq!(
        roster_from_log(&log, &identity)
            .unwrap()
            .into_roster()
            .unwrap(),
        Some(first.clone())
    );
    assert!(read(&log, &outsider).0.into_roster().unwrap().is_none());
    assert!(
        roster_from_log(&log, &outsider)
            .unwrap()
            .into_roster()
            .unwrap()
            .is_none()
    );

    let changed = SwarmRoster {
        deny_labels: vec!["m9".into()],
        ..SwarmRoster::default()
    };
    put(&mut log, &changed, 1_004);
    assert_eq!(
        read(&log, &identity).0.into_roster().unwrap(),
        Some(changed.clone())
    );
    assert_eq!(
        roster_from_log(&log, &identity)
            .unwrap()
            .into_roster()
            .unwrap(),
        Some(changed)
    );
    assert_eq!(
        read(&original, &identity).0.into_roster().unwrap(),
        Some(first.clone())
    );

    let (draft, _) = log
        .draft_epoch(&actor, 1_005, EpochAudience::Everyone, 2)
        .unwrap();
    log.insert(draft.sign(&writer, incarnation).unwrap())
        .unwrap();
    let revoked = read(&log, &identity);
    assert!(!revoked.0.visible);
    assert!(revoked.0.into_roster().unwrap().is_none());
    assert_eq!(revoked.1, Some(records_heads_digest(&log)));
    assert_eq!(
        read(&original, &identity).0.into_roster().unwrap(),
        Some(first)
    );
}

#[test]
fn merge_swarm_roster_and_denied_checks_prefer_active_row_over_stale_denied_row() {
    let id1 = Identity::from_secret_bytes([0x11; 32]);
    let id2 = Identity::from_secret_bytes([0x22; 32]);
    let req1 = Request::create([2; 16], [3; 32], [1; 16], [1; 16], &id1, &[1; 32]).expect("req1");
    let req2 = Request::create([2; 16], [3; 32], [2; 16], [2; 16], &id2, &[2; 32]).expect("req2");
    let rec2_str = id2.recipient().to_bech32();
    let ent1 = Entrant {
        request: req1,
        node_key: [1; 32],
        name: "m1.local".to_owned(),
        label: "m1".to_owned(),
    };
    let ent2 = Entrant {
        request: req2,
        node_key: [2; 32],
        name: "m8.local".to_owned(),
        label: "m8".to_owned(),
    };
    let genesis = Record::found(&[99; 32], vec![ent1, ent2], 1_000).expect("genesis");
    let log = Log::new(genesis).expect("log");

    let stale_denied = SwarmMember {
        recipient: rec2_str.clone(),
        vault_name: "m5".to_owned(),
        label: "m5".to_owned(),
        aliases: vec!["m50".to_owned()],
        hint: None,
        hw_hint: None,
        admitted_ms: 100,
        claimed_label: None,
        claimed_at_ms: None,
        node_id: Some("aa".repeat(16)),
        node_key: Some("aa".repeat(32)),
    };
    let active_row = SwarmMember {
        recipient: rec2_str.clone(),
        vault_name: "m8".to_owned(),
        label: "m8".to_owned(),
        aliases: Vec::new(),
        hint: None,
        hw_hint: None,
        admitted_ms: 200,
        claimed_label: None,
        claimed_at_ms: None,
        node_id: Some("bb".repeat(16)),
        node_key: Some("02".repeat(32)),
    };

    // 1. Both rows in one roster (`m5` denied, `m8` active): `member_denied_by_roster` and
    //    `recipient_denied_by_roster_or_log` must NOT treat `id2` as denied.
    let mut both = SwarmRoster {
        members: vec![stale_denied.clone(), active_row.clone()],
        deny_labels: vec!["m5".to_owned()],
        deny_nodes: vec!["aa".repeat(32)],
    };
    let membership = log.membership();
    let m2_seat = membership
        .members()
        .values()
        .find(|m| m.binding.recipient() == id2.recipient())
        .expect("m2 seat");
    assert!(!member_denied_by_roster(&both, m2_seat));
    assert!(!recipient_denied_by_roster_or_log(
        Some(&both),
        &log,
        &id2.recipient()
    ));

    // Negative control: denying `m8` as well makes both functions return true.
    both.deny_labels.push("m8".to_owned());
    assert!(member_denied_by_roster(&both, m2_seat));
    assert!(recipient_denied_by_roster_or_log(
        Some(&both),
        &log,
        &id2.recipient()
    ));

    // 2. `merge_swarm_roster` with stale denied in `acc` and active in `other` replaces the
    //    stale denied row without polluting `m8`'s aliases with `"m5"`.
    let mut acc1 = SwarmRoster {
        members: vec![stale_denied.clone()],
        deny_labels: vec!["m5".to_owned()],
        deny_nodes: vec!["aa".repeat(32)],
    };
    let other1 = SwarmRoster {
        members: vec![active_row.clone()],
        deny_labels: Vec::new(),
        deny_nodes: Vec::new(),
    };
    merge_swarm_roster(&mut acc1, other1);
    let merged1 = acc1.member_by_recipient(&rec2_str).expect("merged1");
    assert_eq!(merged1.label, "m8");
    assert!(!acc1.is_member_denied(merged1));

    // 3. `merge_swarm_roster` in the reverse direction (`active` in `acc`, `stale_denied` in
    //    `other`) also keeps `m8` un-denied.
    let mut acc2 = SwarmRoster {
        members: vec![active_row],
        deny_labels: Vec::new(),
        deny_nodes: Vec::new(),
    };
    let other2 = SwarmRoster {
        members: vec![stale_denied],
        deny_labels: vec!["m5".to_owned()],
        deny_nodes: vec!["aa".repeat(32)],
    };
    merge_swarm_roster(&mut acc2, other2);
    let merged2 = acc2.member_by_recipient(&rec2_str).expect("merged2");
    assert_eq!(merged2.label, "m8");
    assert!(!acc2.is_member_denied(merged2));
}

#[test]
fn member_node_removed_and_denied_checks_cover_binding_writer_and_incarnation() {
    let id1 = Identity::from_secret_bytes([0x11; 32]);
    let id2 = Identity::from_secret_bytes([0x22; 32]);
    let id3 = Identity::from_secret_bytes([0x33; 32]);
    let req1 = Request::create([2; 16], [3; 32], [1; 16], [1; 16], &id1, &[1; 32]).expect("req1");
    let req2 =
        Request::create([2; 16], [3; 32], [2; 16], [0x2a; 16], &id2, &[2; 32]).expect("req2");
    let req3 =
        Request::create([2; 16], [3; 32], [3; 16], [0x3a; 16], &id3, &[3; 32]).expect("req3");
    let m1_actor = req1.binding().actor();
    let m2_actor = req2.binding().actor();
    let m2_writer = *req2.binding().writer();
    let m2_incarnation = *req2.binding().incarnation();
    let m3_writer = *req3.binding().writer();
    let ent1 = Entrant {
        request: req1,
        node_key: [1; 32],
        name: "m1.local".to_owned(),
        label: "m1".to_owned(),
    };
    // `ent2` has `node_key = m3_writer`, while `ent3` is admitted with `node_key = [0; 32]`.
    // When `m2` is later removed, `membership.node(&m3_writer)` becomes `NodeStanding::Removed(m2_actor)`
    // even though `m3.node_key` is `None`.
    let ent2 = Entrant {
        request: req2,
        node_key: m3_writer,
        name: "m2.local".to_owned(),
        label: "m2".to_owned(),
    };
    let genesis = Record::found(&[99; 32], vec![ent1, ent2], 1_000).expect("genesis");
    let mut log = Log::new(genesis).expect("log");
    let ent3 = Entrant {
        request: req3,
        node_key: [0; 32],
        name: "m3.local".to_owned(),
        label: "m3".to_owned(),
    };
    let admit_m3 = log
        .draft(
            &m1_actor,
            1_001,
            crate::membership_log::Body::Admit {
                entrant: ent3,
                evidence: crate::membership_log::Evidence::Phrase,
            },
        )
        .sign(&[1; 32], [1; 16])
        .expect("sign admit m3");
    log.insert(admit_m3).expect("insert admit m3");

    // 1. Before `m2` is removed, `m2`'s `binding.writer()` or `binding.incarnation()` in
    //    `SwarmRoster::deny_nodes` denies `m2` even though `m2.node_key` (`m3_writer`) is not
    //    in `deny_nodes`.
    let m2_seat = log.membership().get(&m2_actor).expect("m2 seat").clone();
    let roster_writer_denied = SwarmRoster {
        members: Vec::new(),
        deny_labels: Vec::new(),
        deny_nodes: vec![aterm_codec::hex::encode_lower(&m2_writer)],
    };
    assert!(member_denied_by_roster(&roster_writer_denied, &m2_seat));
    assert!(recipient_denied_by_roster_or_log(
        Some(&roster_writer_denied),
        &log,
        &id2.recipient()
    ));
    let roster_inc_denied = SwarmRoster {
        members: Vec::new(),
        deny_labels: Vec::new(),
        deny_nodes: vec![aterm_codec::hex::encode_lower(&m2_incarnation)],
    };
    assert!(member_denied_by_roster(&roster_inc_denied, &m2_seat));
    assert!(recipient_denied_by_roster_or_log(
        Some(&roster_inc_denied),
        &log,
        &id2.recipient()
    ));

    // 2. Remove `m2`: `membership.node(&m3_writer)` becomes `NodeStanding::Removed(m2_actor)`,
    //    which marks `m3` (`binding.writer() == m3_writer`, `node_key == None`) as removed in
    //    `member_node_removed`, `recipient_is_removed`, and `recipient_denied_by_roster_or_log`.
    let rem_m2 = log
        .draft(
            &m1_actor,
            1_002,
            crate::membership_log::Body::Remove {
                member: m2_actor,
                cutoff: *log.network(),
            },
        )
        .sign(&[1; 32], [1; 16])
        .expect("sign remove m2");
    log.insert(rem_m2).expect("insert remove m2");

    let membership = log.membership();
    let m3_seat = membership
        .members()
        .values()
        .find(|m| m.binding.recipient() == id3.recipient())
        .expect("m3 seat");
    assert_eq!(m3_seat.node_key, None);
    assert!(membership.member_node_removed(m3_seat));
    assert!(log.recipient_is_removed(&id3.recipient()));
    assert!(recipient_denied_by_roster_or_log(
        None,
        &log,
        &id3.recipient()
    ));
}

/// One synthetic Mac of [`carry_lab`]: its writer seed, incarnation, identity and actor.
struct Seat {
    seed: [u8; 32],
    incarnation: [u8; 16],
    identity: Identity,
    actor: MemberId,
}

impl Seat {
    fn new(tag: u8) -> (Self, Entrant) {
        let seed = [tag; 32];
        let incarnation = [tag; 16];
        let identity = Identity::from_secret_bytes([tag.wrapping_add(0x40); 32]);
        let request = Request::create([2; 16], [3; 32], [tag; 16], incarnation, &identity, &seed)
            .expect("request");
        let actor = request.binding().actor();
        let entrant = Entrant {
            request,
            node_key: [tag.wrapping_add(0x80); 32],
            name: format!("mac-{tag}"),
            label: format!("m{tag}"),
        };
        (
            Self {
                seed,
                incarnation,
                identity,
                actor,
            },
            entrant,
        )
    }

    fn sign(&self, log: &mut Log, ms: u64, body: crate::membership_log::Body) -> Record {
        let record = log
            .draft(&self.actor, ms, body)
            .sign(&self.seed, self.incarnation)
            .expect("sign");
        log.insert(record.clone()).expect("insert");
        record
    }

    /// Mint an epoch of `audience`, wrapped to each of `to`.
    fn mint(
        &self,
        log: &mut Log,
        ms: u64,
        audience: EpochAudience,
        generation: u64,
        to: &[Recipient],
    ) -> (RecordId, zeroize::Zeroizing<[u8; 32]>) {
        let (draft, key) = log
            .draft_epoch(&self.actor, ms, audience, generation)
            .expect("epoch");
        let epoch = draft.sign(&self.seed, self.incarnation).expect("sign");
        let id = *epoch.id();
        log.insert(epoch).expect("insert epoch");
        for &recipient in to {
            let wrap = log
                .draft_wrap(&self.actor, ms, id, &key, recipient)
                .expect("wrap")
                .sign(&self.seed, self.incarnation)
                .expect("sign");
            log.insert(wrap).expect("insert wrap");
        }
        (id, key)
    }

    fn opened(&self, log: &Log) -> crate::membership_log::entries::OpenedAudienceEntries {
        log.open_audience_entries(&self.identity)
            .expect("open")
            .expect("not removed")
    }

    fn rotate(&self, log: &mut Log) -> (Vec<String>, Vec<Record>) {
        let opened = self.opened(log);
        let mut minted = Vec::new();
        let rotated = rotate_after_removal(
            log,
            &opened,
            &AuthorCtx {
                actor: &self.actor,
                writer_seed: &self.seed,
                incarnation: self.incarnation,
                authored_ms: 9_000,
            },
            &BTreeSet::new(),
            &Rotation::default(),
            &Denies::opened_only(log, &opened),
            &mut minted,
        )
        .expect("rotate");
        (rotated, minted)
    }
}

/// The recipients each `Epoch` of `minted` is wrapped to, by audience.
fn minted_wraps(minted: &[Record]) -> BTreeMap<EpochAudience, BTreeSet<Recipient>> {
    let mut audiences = BTreeMap::new();
    for record in minted {
        if let crate::membership_log::Body::Epoch { audience, .. } = record.body() {
            audiences.insert(*record.id(), audience.clone());
        }
    }
    let mut out: BTreeMap<EpochAudience, BTreeSet<Recipient>> = BTreeMap::new();
    for record in minted {
        if let crate::membership_log::Body::Wrap {
            epoch, recipient, ..
        } = record.body()
            && let Some(audience) = audiences.get(epoch)
        {
            out.entry(audience.clone()).or_default().insert(*recipient);
        }
    }
    out
}

/// Founders m1 and m2; m1 admits m3, m4 and m5. `[everyone]` epoch 1 and Commons epoch 1 are
/// wrapped to all five and to `x`, a key no seat binds (let in through the envelope alone);
/// the roster under epoch 1 denies `m4`; m1 removes m5 with a `Remove`; then m2 re-keys both
/// audiences to epoch 2, wrapped to m1 and m2 only, omitting m3 (the 2026-11-03 shape).
fn carry_lab() -> (Log, Vec<Seat>, Identity) {
    let (seats, entrants): (Vec<Seat>, Vec<Entrant>) = (1..=5).map(Seat::new).unzip();
    let genesis = Record::found(&[99; 32], entrants[..2].to_vec(), 1_000).expect("genesis");
    let mut log = Log::new(genesis).expect("log");
    for (ms, entrant) in (1_001..).zip(&entrants[2..]) {
        seats[0].sign(
            &mut log,
            ms,
            crate::membership_log::Body::Admit {
                entrant: entrant.clone(),
                evidence: crate::membership_log::Evidence::Phrase,
            },
        );
    }
    let x = Identity::from_secret_bytes([0x77; 32]);
    let mut all: Vec<Recipient> = seats.iter().map(|s| s.identity.recipient()).collect();
    all.push(x.recipient());
    let epoch1 = seats[0].mint(&mut log, 1_100, EpochAudience::Everyone, 1, &all);
    seats[0].mint(&mut log, 1_101, EpochAudience::Commons, 1, &all);
    let roster = SwarmRoster {
        members: Vec::new(),
        deny_labels: vec!["m4".to_owned()],
        deny_nodes: Vec::new(),
    };
    let entry = Entry {
        name: crate::swarm_roster::ROSTER_ENTRY.to_owned(),
        value: crate::SecretBytes::from(roster.to_toml().as_bytes()),
        audience: Audience::Everyone,
        if_absent: false,
        anchor: None,
    };
    let put = log
        .draft_put(&seats[0].actor, 1_102, epoch1.0, &epoch1.1, &entry)
        .expect("put")
        .sign(&seats[0].seed, seats[0].incarnation)
        .expect("sign");
    log.insert(put).expect("insert put");
    let m5_entry = log.membership().get(&seats[4].actor).expect("m5").entry;
    seats[0].sign(
        &mut log,
        1_103,
        crate::membership_log::Body::Remove {
            member: seats[4].actor,
            cutoff: m5_entry,
        },
    );
    let two = [seats[0].identity.recipient(), seats[1].identity.recipient()];
    seats[1].mint(&mut log, 1_200, EpochAudience::Everyone, 2, &two);
    seats[1].mint(&mut log, 1_201, EpochAudience::Commons, 2, &two);
    (log, seats, x)
}

/// One-mesh M1 and M4: the carry is the current seats. The seat a newest epoch omitted (m3)
/// is carried; the seat the roster denies (m4), the seat a `Remove` took out (m5) and the
/// key no seat binds (`x`) are not, for `[everyone]` and Commons alike.
///
/// NEGATIVE CONTROL: the old carry (drop every left-out seat, keep the seatless recipients
/// every newest head wraps) drops m3; restoring its left-out filter fails the first assert.
#[test]
fn the_carry_is_the_current_seats_omitted_kept_seatless_denied_and_removed_dropped() {
    let (log, seats, x) = carry_lab();
    let rec = |i: usize| seats[i].identity.recipient();
    let opened = seats[0].opened(&log);
    assert!(log.left_out(&opened, &EpochAudience::Everyone, &seats[2].actor));
    let flight = Flight::of_batch(&log, &opened, &seats[0].actor);
    let current = log.current_recipients(&EpochAudience::Everyone);
    assert_eq!(current, BTreeSet::from([rec(0), rec(1), rec(2), rec(3)]));
    for audience in [EpochAudience::Everyone, EpochAudience::Commons] {
        let mut active = current.clone();
        active.insert(x.recipient());
        let carried = carried_audience_recipients(
            &log,
            &opened,
            &flight,
            &audience,
            &active,
            &BTreeSet::new(),
            &Denies::opened_only(&log, &opened),
        );
        assert_eq!(
            carried,
            BTreeSet::from([rec(0), rec(1), rec(2)]),
            "{audience:?}: the omitted m3 is carried; denied m4, removed m5 and seatless x drop"
        );
    }

    // The removal's re-key by a holder wraps exactly that set in both audiences.
    let mut rotated_log = log.clone();
    let (rotated, minted) = seats[0].rotate(&mut rotated_log);
    assert_eq!(rotated, vec!["everyone".to_owned(), "commons".to_owned()]);
    let wraps = minted_wraps(&minted);
    for audience in [EpochAudience::Everyone, EpochAudience::Commons] {
        assert_eq!(
            wraps.get(&audience),
            Some(&BTreeSet::from([rec(0), rec(1), rec(2)])),
            "{audience:?}"
        );
    }
}

/// One-mesh M4: a Mac that cannot open an audience's newest epoch never mints for it (gen
/// 66: a remover that could not open Commons minted it anyway, wrapped to the carried 3 and
/// not itself). m3, omitted from both epochs 2, mints nothing; m1, a holder, mints both.
///
/// NEGATIVE CONTROL: drop the `opens_newest` conditions in `rotate_after_removal`: m3 mints
/// an `[everyone]` and a Commons epoch and the first assert fails.
#[test]
fn a_mac_that_cannot_open_the_newest_epoch_mints_nothing() {
    let (log, seats, _) = carry_lab();
    let mut non_holder = log.clone();
    let (rotated, minted) = seats[2].rotate(&mut non_holder);
    assert!(
        rotated.is_empty() && minted.is_empty(),
        "a non-holder minted {rotated:?}"
    );
    let mut holder = log;
    let (rotated, _) = seats[0].rotate(&mut holder);
    assert_eq!(rotated, vec!["everyone".to_owned(), "commons".to_owned()]);
}

/// One-mesh M1 and M5: the gap closer names every current seat a settled newest epoch omits
/// (m3, and the roster-denied m4, which the log alone cannot tell apart), and drops the one a
/// roster denies before it ranks, so a denied seat is never wrapped; the removed m5 is no
/// gap at all.
///
/// NEGATIVE CONTROL: make `gap_denied` return false: m4's gap passes and the second assert
/// fails.
#[test]
fn a_denied_seat_is_never_healed_and_a_removed_one_is_no_gap() {
    let (log, seats, _) = carry_lab();
    let opened = seats[0].opened(&log);
    let audiences = crate::membership_log::admission_audiences(&opened);
    let gaps = log.wrap_gaps(&opened, &audiences);
    let named: BTreeSet<(EpochAudience, MemberId)> = gaps
        .iter()
        .map(|gap| (gap.audience.clone(), gap.member))
        .collect();
    let expected: BTreeSet<(EpochAudience, MemberId)> =
        [EpochAudience::Everyone, EpochAudience::Commons]
            .into_iter()
            .flat_map(|aud| [(aud.clone(), seats[2].actor), (aud, seats[3].actor)])
            .collect();
    assert_eq!(named, expected, "m3 and m4 lack both epochs 2; m5 is out");
    let denies = Denies::opened_only(&log, &opened);
    let membership = log.membership();
    let healed: BTreeSet<MemberId> = gaps
        .iter()
        .filter(|gap| !gap_denied(&log, &membership, gap, &denies))
        .map(|gap| gap.member)
        .collect();
    assert_eq!(healed, BTreeSet::from([seats[2].actor]), "m4 is denied");
}

/// A roster that denies the seat labelled `label`, as this Mac's own.
fn denying(label: &str) -> SwarmRoster {
    SwarmRoster {
        members: Vec::new(),
        deny_labels: vec![label.to_owned()],
        deny_nodes: Vec::new(),
    }
}

/// One-mesh M1, proofs P2's one deny: the heal and the carry read this Mac's own roster joined
/// to the opened one. In [`carry_lab`], a roster only this Mac holds denies m3: the carry
/// drops m3 as it drops the opened roster's m4, and the closer heals neither. Before PF2 the
/// carry read the opened roster alone, so it wrapped m3 into each new epoch while the closer,
/// which read both, refused m3 the heal.
///
/// NEGATIVE CONTROL: `ATERM_TEST_NEGATIVE_CONTROL=opened-roster-deny` reads the opened roster
/// alone and the first assert fails.
#[test]
fn the_carry_and_the_heal_read_one_deny_of_both_rosters() {
    let (log, seats, _) = carry_lab();
    let rec = |i: usize| seats[i].identity.recipient();
    let opened = seats[0].opened(&log);
    let denies = Denies::of(
        Some(denying("m3")),
        Denies::opened_only(&log, &opened).opened,
    );
    let flight = Flight::of_batch(&log, &opened, &seats[0].actor);
    let current = log.current_recipients(&EpochAudience::Everyone);
    for audience in [EpochAudience::Everyone, EpochAudience::Commons] {
        let carried = carried_audience_recipients(
            &log,
            &opened,
            &flight,
            &audience,
            &current,
            &BTreeSet::new(),
            &denies,
        );
        assert_eq!(
            carried,
            BTreeSet::from([rec(0), rec(1)]),
            "{audience:?}: m3, denied by this Mac's roster alone, is not carried"
        );
    }
    let audiences = crate::membership_log::admission_audiences(&opened);
    let membership = log.membership();
    let healed: Vec<MemberId> = log
        .wrap_gaps_denying(&opened, &audiences, |seat| denies.member(seat))
        .iter()
        .filter(|gap| !gap_denied(&log, &membership, gap, &denies))
        .map(|gap| gap.member)
        .collect();
    assert!(healed.is_empty(), "m3 and m4 are denied: {healed:?}");
}

/// Founders m1 and m2; m1 admits m3. `[everyone]` and Commons epoch 1 are wrapped to all
/// three; then m3 re-keys Commons to epoch 2, wrapped to itself alone, so m3 is the last holder
/// of the newest Commons key and m1 cannot open it. Returns the log, the seats and epoch 2.
fn last_holder_lab() -> (Log, Vec<Seat>, (RecordId, zeroize::Zeroizing<[u8; 32]>)) {
    let (seats, entrants): (Vec<Seat>, Vec<Entrant>) = (1..=3).map(Seat::new).unzip();
    let genesis = Record::found(&[99; 32], entrants[..2].to_vec(), 1_000).expect("genesis");
    let mut log = Log::new(genesis).expect("log");
    seats[0].sign(
        &mut log,
        1_001,
        crate::membership_log::Body::Admit {
            entrant: entrants[2].clone(),
            evidence: crate::membership_log::Evidence::Phrase,
        },
    );
    let all: Vec<Recipient> = seats.iter().map(|s| s.identity.recipient()).collect();
    seats[0].mint(&mut log, 1_100, EpochAudience::Everyone, 1, &all);
    seats[0].mint(&mut log, 1_101, EpochAudience::Commons, 1, &all);
    let epoch2 = seats[2].mint(&mut log, 1_200, EpochAudience::Commons, 2, &all[2..]);
    (log, seats, epoch2)
}

/// `log` with `seats[by]`'s `Remove` of `seats[target]` in it, its cutoff the target's last
/// record, as `draft_removal` picks it.
fn removed(log: &Log, seats: &[Seat], by: usize, target: usize) -> Log {
    let mut after = log.clone();
    let member = seats[target].actor;
    let cutoff = after
        .records()
        .filter(|r| r.kind() != crate::membership_log::Kind::Genesis && *r.actor() == member)
        .max_by(|a, b| a.sequence().cmp(&b.sequence()).then(b.id().cmp(a.id())))
        .map_or_else(
            || after.membership().get(&member).expect("a member").entry,
            |r| *r.id(),
        );
    seats[by].sign(
        &mut after,
        1_300,
        crate::membership_log::Body::Remove { member, cutoff },
    );
    after
}

#[test]
fn commons_overlay_refuses_a_failed_removal_of_the_last_control_host() {
    let (log, seats, _) = carry_lab();
    let owner = &seats[0].identity;
    let removed = seats[4].identity.recipient();
    assert!(log.recipient_is_removed(&removed));
    for control_host in ["m5", "m1"] {
        let mut doc = Document::new_vault("m1", &owner.recipient().to_bech32());
        doc.add_machine("m5", &removed.to_bech32()).unwrap();
        doc.set_entry(
            &Audience::Hosts(vec![control_host.to_owned()]),
            crate::commons::ENTRY,
            b"control",
        )
        .unwrap();
        let before = doc.render();
        let result = overlay_log_commons_onto_document(&log, owner, &mut doc, None);
        if control_host == "m5" {
            assert!(result.is_err(), "a failed removal cannot produce a mirror");
            assert_eq!(doc.render().as_slice(), before.as_slice());
        } else {
            assert!(result.is_ok());
            assert!(
                doc.machines()
                    .iter()
                    .all(|machine| machine.recipient != removed.to_bech32())
            );
        }
    }
}

/// One-mesh proofs P2, `last_holder_strands` and `remove_member`'s guard: m1 removing m3, the
/// last holder of the newest Commons epoch, which m1 cannot open to re-key, strands Commons
/// and nothing else. m1 removing m2 strands nothing (m3 still holds it), nor does m3 removing
/// m2 (m3 re-keys), nor m1 removing m3 once epoch 2 wraps m1 too (m1 re-keys). What the guard
/// refuses: unguarded, m1's batch re-keys `[everyone]` alone, and no current Mac holds the
/// newest Commons key.
///
/// NEGATIVE CONTROL: `ATERM_TEST_NEGATIVE_CONTROL=unguarded-removal` strands nothing and the
/// first assert fails.
#[test]
fn a_removal_that_strands_the_last_holder_is_refused() {
    let (log, seats, (epoch2, key2)) = last_holder_lab();
    let commons = EpochAudience::Commons;
    let opened = seats[0].opened(&log);
    assert!(
        !opens_newest(&opened, &commons),
        "m1 cannot open Commons epoch 2"
    );
    let after = removed(&log, &seats, 0, 2);
    assert_eq!(stranded_audiences(&after, &opened), vec![commons.clone()]);
    assert!(
        stranded_audiences(&removed(&log, &seats, 0, 1), &opened).is_empty(),
        "m3 still holds Commons epoch 2"
    );
    assert!(
        stranded_audiences(&removed(&log, &seats, 2, 1), &seats[2].opened(&log)).is_empty(),
        "m3 re-keys Commons"
    );

    let mut unguarded = after.clone();
    let (rotated, _) = seats[0].rotate(&mut unguarded);
    assert_eq!(rotated, vec!["everyone".to_owned()]);
    let view = seats[0].opened(&unguarded);
    let current = unguarded.current_recipients(&EpochAudience::Everyone);
    let holders: Vec<&Recipient> = view.maximal_epochs[&commons]
        .iter()
        .flat_map(|epoch| view.wrapped_recipients.get(epoch).into_iter().flatten())
        .filter(|r| current.contains(*r))
        .collect();
    assert!(
        holders.is_empty(),
        "the newest Commons key is held by {holders:?}"
    );

    let mut shared = log;
    let wrap = shared
        .draft_wrap(
            &seats[2].actor,
            1_250,
            epoch2,
            &key2,
            seats[0].identity.recipient(),
        )
        .expect("wrap")
        .sign(&seats[2].seed, seats[2].incarnation)
        .expect("sign");
    shared.insert(wrap).expect("insert wrap");
    let opened = seats[0].opened(&shared);
    assert!(opens_newest(&opened, &commons));
    assert!(
        stranded_audiences(&removed(&shared, &seats, 0, 2), &opened).is_empty(),
        "m1 re-keys Commons"
    );
}

/// The held-wrap leak (one-mesh review item 4, L1). Founders A, W, S and H. A mints
/// `[everyone]` epoch E0 wrapped to A, W and H, leaving S out. H holds E0 but not the
/// `Remove` below, so S is a current seat E0 omits and H heals it: H's `Wrap` hands E0's key
/// to S. Concurrently A removes S. W holds the `Remove` and E0's batch, but neither H's heal
/// nor A's re-key. The `Wrap`s W holds of E0 name no removed Mac, so a gate read from them
/// lets W seal under E0, which S reads. P1's causal gate keeps E0 due: the `Remove` that took
/// S is not in E0's causal past, so W waits for (and the ranked tender mints) an epoch that
/// postdates it.
///
/// NEGATIVE CONTROL: `ATERM_TEST_NEGATIVE_CONTROL=held-wrap-gate` judges a newest epoch by
/// the `Wrap`s this Mac holds alone (the rule before PF1): W seals under E0 and the asserts
/// fail.
#[test]
fn a_heal_beside_a_remove_never_lets_a_writer_seal_where_the_removed_mac_reads() {
    let (seats, entrants): (Vec<Seat>, Vec<Entrant>) = (1..=4).map(Seat::new).unzip();
    let (a, w, s, h) = (&seats[0], &seats[1], &seats[2], &seats[3]);
    let recipient = |seat: &Seat| seat.identity.recipient();
    let genesis = Record::found(&[99; 32], entrants, 1_000).expect("genesis");
    let mut base = Log::new(genesis).expect("log");
    let (e0, key) = a.mint(
        &mut base,
        1_100,
        EpochAudience::Everyone,
        1,
        &[recipient(a), recipient(w), recipient(h)],
    );

    let mut h_log = base.clone();
    let gaps = h_log.wrap_gaps(&h.opened(&h_log), &[SeatAudience::Everyone]);
    assert!(
        gaps.iter()
            .any(|gap| gap.epoch == e0 && gap.recipient == recipient(s)),
        "H heals S's omission from the newest epoch"
    );
    let heal = h_log
        .draft_wrap(&h.actor, 1_200, e0, &key, recipient(s))
        .expect("wrap")
        .sign(&h.seed, h.incarnation)
        .expect("sign");
    h_log.insert(heal).expect("insert heal");
    let unwrapped = h_log.unwrap_epochs(&s.identity).expect("unwrap");
    assert_eq!(
        unwrapped.get(&e0).map(|k| **k),
        Some(*key),
        "S reads E0 through H's heal"
    );

    let mut a_log = base.clone();
    let cutoff = *a_log.network();
    let remove = a.sign(
        &mut a_log,
        1_200,
        crate::membership_log::Body::Remove {
            member: s.actor,
            cutoff,
        },
    );
    let mut w_log = base;
    w_log.insert(remove).expect("insert remove");
    assert!(w_log.recipient_is_removed(&recipient(s)));
    let opened = w.opened(&w_log);
    assert!(
        rotation_due(&w_log, &opened, &EpochAudience::Everyone),
        "W waits: the Remove of S is not in E0's past"
    );
    assert!(
        w_log
            .rekey_dues()
            .iter()
            .any(|due| due.audience == EpochAudience::Everyone && due.rotation),
        "E0 is due a rotation on W"
    );
    let flight = Flight::of_reader(&w_log, &recipient(w));
    assert_eq!(
        sealing_epoch(
            &w_log,
            &opened,
            &[e0],
            &w.identity,
            &w_log.current_recipients(&EpochAudience::Everyone),
            flight.undecided(&EpochAudience::Everyone),
        ),
        None,
        "W seals nothing under E0"
    );
}
