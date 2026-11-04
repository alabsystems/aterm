// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::commons::binding::Request;
use crate::membership_log::{Entrant, Inserted};

const TARGET: &str = "aterm-swarm-roster";

struct Fixture {
    log: Log,
    identity: Identity,
    actor: MemberId,
    seed: [u8; 32],
    incarnation: [u8; 16],
}

impl Fixture {
    fn new() -> Self {
        let seed = [17; 32];
        let incarnation = [18; 16];
        let identity = Identity::from_secret_bytes([19; 32]);
        let request =
            Request::create([20; 16], [21; 32], [22; 16], incarnation, &identity, &seed).unwrap();
        let actor = request.binding().actor();
        let genesis = Record::found(
            &[23; 32],
            vec![Entrant {
                request,
                node_key: [24; 32],
                name: "reader".into(),
                label: "m1".into(),
            }],
            1,
        )
        .unwrap();
        Self {
            log: Log::new(genesis).unwrap(),
            identity,
            actor,
            seed,
            incarnation,
        }
    }

    fn keep(&mut self, draft: Draft) -> RecordId {
        let record = draft.sign(&self.seed, self.incarnation).unwrap();
        let id = *record.id();
        assert_eq!(self.log.insert(record).unwrap(), Inserted::Held);
        id
    }

    fn epoch(&mut self, audience: EpochAudience) -> (RecordId, Zeroizing<[u8; 32]>) {
        let (draft, key) = self.log.draft_epoch(&self.actor, 2, audience, 1).unwrap();
        let epoch = self.keep(draft);
        let wrap = self
            .log
            .draft_wrap(&self.actor, 3, epoch, &key, self.identity.recipient())
            .unwrap();
        self.keep(wrap);
        (epoch, key)
    }

    fn put(&mut self, epoch: RecordId, key: &[u8; 32], entry: &Entry) -> RecordId {
        let draft = self
            .log
            .draft_put(&self.actor, 4, epoch, key, entry)
            .unwrap();
        self.keep(draft)
    }

    fn named(&self) -> Option<Vec<Entry>> {
        let selected = self
            .log
            .open_entries_named(&self.identity, &[TARGET])
            .unwrap();
        let full = self
            .log
            .open_entries(&self.identity)
            .unwrap()
            .map(|entries| {
                entries
                    .into_iter()
                    .filter(|entry| entry.name == TARGET)
                    .collect()
            });
        assert_eq!(selected, full);
        selected
    }

    fn in_audience(&self, audience: &Audience) -> Option<Vec<Entry>> {
        let selected = self
            .log
            .open_audience_entries_named(&self.identity, audience, &[TARGET])
            .unwrap();
        let full = self
            .log
            .open_audience_entries(&self.identity)
            .unwrap()
            .map(|mut opened| {
                opened
                    .by_audience
                    .remove(audience)
                    .into_iter()
                    .flatten()
                    .filter_map(|(name, entry)| (name == TARGET).then_some(entry))
                    .collect()
            });
        assert_eq!(selected, full);
        selected
    }
}

fn entry(name: &str, value: &[u8], audience: Audience) -> Entry {
    Entry {
        name: name.into(),
        value: SecretBytes::from(value),
        audience,
        if_absent: false,
        anchor: None,
    }
}

#[test]
fn named_selection_keeps_reader_unavailability_distinct_from_absence() {
    let mut fixture = Fixture::new();
    assert!(fixture.named().is_none());
    let (epoch, key) = fixture.epoch(EpochAudience::Everyone);
    fixture.put(epoch, &key, &entry("other", b"value", Audience::Everyone));
    assert_eq!(fixture.named(), Some(Vec::new()));
    let wanted = entry(TARGET, b"roster", Audience::Everyone);
    fixture.put(epoch, &key, &wanted);
    assert_eq!(fixture.named(), Some(vec![wanted]));
}

#[test]
fn audience_selection_keeps_the_full_reader_without_current_keys() {
    let mut fixture = Fixture::new();
    assert_eq!(fixture.in_audience(&Audience::Everyone), Some(Vec::new()));
    assert!(fixture.named().is_none());
    let (epoch, key) = fixture.epoch(EpochAudience::Everyone);
    let expected = entry(TARGET, b"everyone", Audience::Everyone);
    fixture.put(epoch, &key, &expected);
    assert_eq!(
        fixture.in_audience(&Audience::Everyone),
        Some(vec![expected])
    );
}

#[test]
fn audience_selection_does_not_replace_everyone_with_the_host_winner() {
    let mut fixture = Fixture::new();
    let (epoch, key) = fixture.epoch(EpochAudience::Everyone);
    let everyone = entry(TARGET, b"everyone", Audience::Everyone);
    fixture.put(epoch, &key, &everyone);
    let host = Audience::Hosts(vec!["reader".into()]);
    let (epoch, key) = fixture.epoch(EpochAudience::from(&host));
    let host_entry = entry(TARGET, b"host", host.clone());
    fixture.put(epoch, &key, &host_entry);
    assert_eq!(fixture.named(), Some(vec![host_entry.clone()]));
    assert_eq!(
        fixture.in_audience(&Audience::Everyone),
        Some(vec![everyone])
    );
    assert_eq!(fixture.in_audience(&host), Some(vec![host_entry]));
}

#[test]
fn audience_selection_still_resolves_anchors_in_other_audiences() {
    let mut fixture = Fixture::new();
    let (epoch, key) = fixture.epoch(EpochAudience::Everyone);
    let mut root = entry("root", b"base", Audience::Everyone);
    root.if_absent = true;
    fixture.put(epoch, &key, &root);
    let mut target = entry(TARGET, b"everyone", Audience::Everyone);
    target.if_absent = true;
    target.anchor = Some("root".into());
    fixture.put(epoch, &key, &target);
    assert_eq!(fixture.in_audience(&Audience::Everyone), Some(vec![target]));
    let host = Audience::Hosts(vec!["reader".into()]);
    let (epoch, key) = fixture.epoch(EpochAudience::from(&host));
    fixture.put(epoch, &key, &entry("root", b"ordinary", host));
    // A host's non-conditional root removes the dependent Everyone entry globally.
    assert_eq!(fixture.in_audience(&Audience::Everyone), Some(Vec::new()));
}

#[test]
fn conditional_host_winner_is_resolved_before_reserved_entry_policy() {
    for anchor_exists in [false, true] {
        let mut fixture = Fixture::new();
        let (everyone, everyone_key) = fixture.epoch(EpochAudience::Everyone);
        let fallback = entry(TARGET, b"everyone", Audience::Everyone);
        fixture.put(everyone, &everyone_key, &fallback);
        let host = Audience::Hosts(vec!["reader".into()]);
        let (epoch, key) = fixture.epoch(EpochAudience::from(&host));
        if anchor_exists {
            let mut anchor = entry("anchor", b"base", host.clone());
            anchor.if_absent = true;
            fixture.put(epoch, &key, &anchor);
        }
        let mut conditional = entry(TARGET, b"host", host);
        conditional.if_absent = true;
        conditional.anchor = Some("anchor".into());
        fixture.put(epoch, &key, &conditional);
        assert_eq!(
            fixture.named(),
            Some(vec![if anchor_exists { conditional } else { fallback }])
        );
    }
    let mut fixture = Fixture::new();
    let (epoch, key) = fixture.epoch(EpochAudience::Everyone);
    let mut dangling = entry(TARGET, b"gone", Audience::Everyone);
    dangling.if_absent = true;
    dangling.anchor = Some("absent".into());
    fixture.put(epoch, &key, &dangling);
    assert_eq!(fixture.named(), Some(Vec::new()));
}

#[test]
fn unrelated_host_anchor_override_keeps_the_global_fixed_point() {
    let mut fixture = Fixture::new();
    let (everyone, everyone_key) = fixture.epoch(EpochAudience::Everyone);
    let fallback = entry(TARGET, b"everyone", Audience::Everyone);
    fixture.put(everyone, &everyone_key, &fallback);
    let mut root = entry("root", b"base", Audience::Everyone);
    root.if_absent = true;
    fixture.put(everyone, &everyone_key, &root);
    let mut middle = entry("middle", b"mid", Audience::Everyone);
    middle.if_absent = true;
    middle.anchor = Some("root".into());
    fixture.put(everyone, &everyone_key, &middle);
    let host = Audience::Hosts(vec!["reader".into()]);
    let (epoch, key) = fixture.epoch(EpochAudience::from(&host));
    let mut conditional = entry(TARGET, b"host", host.clone());
    conditional.if_absent = true;
    conditional.anchor = Some("middle".into());
    fixture.put(epoch, &key, &conditional);
    assert_eq!(fixture.named(), Some(vec![conditional]));
    fixture.put(epoch, &key, &entry("root", b"not conditional", host));
    assert_eq!(fixture.named(), Some(vec![fallback]));
}

#[test]
fn forced_digest_collision_cannot_turn_a_changed_value_into_a_carry() {
    let mut fixture = Fixture::new();
    let (epoch, key) = fixture.epoch(EpochAudience::Everyone);
    let first = fixture.put(epoch, &key, &entry(TARGET, b"aaaa", Audience::Everyone));
    let second = fixture.put(epoch, &key, &entry(TARGET, b"bbbb", Audience::Everyone));
    let repeated = fixture.put(epoch, &key, &entry(TARGET, b"bbbb", Audience::Everyone));
    let resolved = fixture
        .log
        .resolve_audience_entries(&fixture.identity, |_| [0; 32])
        .unwrap()
        .unwrap();
    let name = (Audience::Everyone, TARGET.to_owned());
    assert_ne!(first, second);
    assert_ne!(second, repeated);
    assert_eq!(resolved.summary.winning_ops[&name], second);
    let expected = fixture.named();
    let forced = fixture
        .log
        .open_entries_named_with_digest(&fixture.identity, &[TARGET], |_| [0; 32])
        .unwrap();
    assert_eq!(forced, expected);
    assert_eq!(forced.unwrap()[0].value.as_slice(), b"bbbb");
}

#[test]
fn unrelated_eligible_payload_errors_still_refuse_named_reads() {
    for kind in 0..3 {
        let mut fixture = Fixture::new();
        let (everyone, key) = fixture.epoch(EpochAudience::Everyone);
        fixture.put(
            everyone,
            &key,
            &entry(TARGET, b"roster", Audience::Everyone),
        );
        let audience = if kind == 2 {
            EpochAudience::Commons
        } else {
            EpochAudience::Everyone
        };
        let (epoch, _) = fixture.epoch(audience.clone());
        let body = if kind == 1 {
            Body::Delete {
                audience,
                epoch,
                salt: [25; 32],
                ciphertext: vec![26; 17].into(),
            }
        } else {
            Body::Put {
                audience,
                epoch,
                salt: [25; 32],
                ciphertext: vec![26; 17].into(),
            }
        };
        let corrupt = fixture.log.draft(&fixture.actor, 5, body);
        fixture.keep(corrupt);
        assert!(fixture.log.open_entries(&fixture.identity).is_err());
        assert!(
            fixture
                .log
                .open_entries_named(&fixture.identity, &[TARGET])
                .is_err()
        );
        assert!(
            fixture
                .log
                .open_audience_entries_named(&fixture.identity, &Audience::Everyone, &[TARGET])
                .is_err()
        );
    }
}

#[test]
fn named_reads_keep_unaddressed_payload_skip_and_addressed_wrap_errors() {
    let mut fixture = Fixture::new();
    let (epoch, key) = fixture.epoch(EpochAudience::Everyone);
    let wanted = entry(TARGET, b"roster", Audience::Everyone);
    fixture.put(epoch, &key, &wanted);
    let audience = EpochAudience::Hosts(vec!["elsewhere".into()]);
    let (draft, _) = fixture
        .log
        .draft_epoch(&fixture.actor, 5, audience.clone(), 1)
        .unwrap();
    let inaccessible = fixture.keep(draft);
    let bad_put = fixture.log.draft(
        &fixture.actor,
        6,
        Body::Put {
            audience,
            epoch: inaccessible,
            salt: [25; 32],
            ciphertext: vec![26; 17].into(),
        },
    );
    fixture.keep(bad_put);
    assert_eq!(fixture.named(), Some(vec![wanted]));
    let bad_wrap = fixture.log.draft(
        &fixture.actor,
        7,
        Body::Wrap {
            epoch: inaccessible,
            recipient: fixture.identity.recipient(),
            capsule: vec![27; 32].into(),
        },
    );
    fixture.keep(bad_wrap);
    assert!(fixture.log.open_entries(&fixture.identity).is_err());
    assert!(
        fixture
            .log
            .open_entries_named(&fixture.identity, &[TARGET])
            .is_err()
    );
}

#[test]
fn equal_reseals_do_not_decrypt_again_inside_register_pair_loops() {
    const WRITES: usize = 64;
    let mut fixture = Fixture::new();
    let (epoch, key) = fixture.epoch(EpochAudience::Everyone);
    let wanted = entry(TARGET, b"unchanged", Audience::Everyone);
    for _ in 0..WRITES {
        fixture.put(epoch, &key, &wanted);
    }
    let before = ENTRY_AEAD_READS.with(std::cell::Cell::get);
    let actual = fixture
        .log
        .open_entries_named(&fixture.identity, &[TARGET])
        .unwrap();
    let reads = ENTRY_AEAD_READS.with(std::cell::Cell::get) - before;
    assert_eq!(actual, Some(vec![wanted]));
    assert!(reads >= WRITES, "every eligible Put was validated");
    assert!(
        reads <= 2 * WRITES,
        "class construction and one winner use linear decryptions: {reads}"
    );
}

#[cfg(unix)]
#[test]
fn named_selection_preserves_keyed_entries_before_and_after_late_undo() {
    let (before, after, _) = crate::membership_log::tests::history_reader_fixture();
    // Y is the independent writer in the shared signed keyed-history fixture.
    let identity = Identity::from_secret_bytes([8 ^ 0x55; 32]);
    assert_eq!(before.keyed_epochs().len(), 1);
    assert!(after.keyed_epochs().is_empty());
    for log in [before, after] {
        let all = log.open_entries(&identity).unwrap().unwrap();
        let expected: Vec<_> = all.into_iter().filter(|e| e.name == "S").collect();
        assert_eq!(expected.len(), 1);
        assert_eq!(expected[0].value.as_slice(), b"v1");
        assert_eq!(
            log.open_entries_named(&identity, &["S"]).unwrap(),
            Some(expected)
        );
    }
}

#[cfg(unix)]
#[test]
fn no_locker_admission_preserves_a_concurrent_wrap_gap_without_a_readable_key() {
    let mut fixture = Fixture::new();
    let seed = [31; 32];
    let incarnation = [30; 16];
    let reader = Identity::from_secret_bytes([32; 32]);
    let request =
        Request::create([20; 16], [21; 32], [29; 16], incarnation, &reader, &seed).unwrap();
    let admit = fixture.log.draft(
        &fixture.actor,
        2,
        Body::Admit {
            entrant: Entrant {
                request,
                node_key: [33; 32],
                name: "new-reader".into(),
                label: "m2".into(),
            },
            evidence: crate::membership_log::Evidence::Phrase,
        },
    );
    let (epoch, _) = fixture
        .log
        .draft_epoch(&fixture.actor, 2, EpochAudience::Everyone, 1)
        .unwrap();
    fixture.keep(epoch);
    fixture.keep(admit);
    assert!(
        fixture
            .log
            .open_entries_named(&reader, &[])
            .unwrap()
            .is_none()
    );
    assert!(fixture.log.admits_without_locker(&reader).unwrap());
}

#[cfg(unix)]
#[test]
fn no_locker_admission_still_validates_addressed_payloads() {
    let mut fixture = Fixture::new();
    let (epoch, _) = fixture.epoch(EpochAudience::Everyone);
    let corrupt = fixture.log.draft(
        &fixture.actor,
        5,
        Body::Put {
            audience: EpochAudience::Everyone,
            epoch,
            salt: [25; 32],
            ciphertext: vec![26; 17].into(),
        },
    );
    fixture.keep(corrupt);
    assert!(
        fixture
            .log
            .admits_without_locker(&fixture.identity)
            .is_err()
    );
}
