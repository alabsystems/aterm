// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Generated signed logs: three Macs that write, delete, reseal, remove and let each other back
//! in on diverging copies and take in each other's, for the differentials that hold a resolver
//! to the one it replaced. Every opening of a copy's entries checks each register and its keyed
//! epochs ([`entries`'s register oracle]), and every epoch verdict the probes ask checks itself
//! ([`epochs`'s oracle]).
//!
//! [`entries`'s register oracle]: super::super::entries
//! [`epochs`'s oracle]: super::super::epochs

use super::super::reach::tests::Rng;
use crate::SecretBytes;
use crate::age::Identity;
use crate::commons::binding::Request;
use crate::membership_log::{
    Body, Draft, Entrant, EpochAudience, Evidence, Founder, Log, MemberId, Record, RecordId, Root,
};
use crate::text::{Audience, Entry};
use zeroize::Zeroizing;

/// One Mac of a generated log: its keys and seat.
pub(in crate::membership_log) struct Mac {
    seed: [u8; 32],
    incarnation: [u8; 16],
    pub(in crate::membership_log) identity: Identity,
    pub(in crate::membership_log) member: MemberId,
}

impl Mac {
    /// The Mac `tag` names, and the entrant that seats it.
    pub(in crate::membership_log) fn new(tag: u8) -> (Self, Entrant) {
        let seed = [tag; 32];
        let incarnation = [tag; 16];
        let identity = Identity::from_secret_bytes([tag.wrapping_add(0x40); 32]);
        let request = Request::create(
            [0x31; 16],
            [0x42; 32],
            [tag.wrapping_add(0x20); 16],
            incarnation,
            &identity,
            &seed,
        )
        .expect("request");
        let member = request.binding().actor();
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
                member,
            },
            entrant,
        )
    }

    /// Sign `draft` and hold it in `log`; its id.
    pub(in crate::membership_log) fn sign(&self, log: &mut Log, draft: Draft) -> RecordId {
        let record = draft.sign(&self.seed, self.incarnation).expect("sign");
        let id = *record.id();
        log.insert(record).expect("insert");
        id
    }
}

/// The newest `[everyone]` epoch `log` holds for `mac`, and its key.
fn newest(log: &Log, mac: &Mac) -> Option<(RecordId, Zeroizing<[u8; 32]>)> {
    let opened = log.open_audience_entries(&mac.identity).ok()??;
    let epoch = *opened
        .maximal_epochs
        .get(&EpochAudience::Everyone)?
        .first()?;
    let key = opened.unwrapped_keys.get(&epoch)?.clone();
    Some((epoch, key))
}

/// The audiences a generated log seals to.
fn audiences() -> [EpochAudience; 3] {
    [
        EpochAudience::Everyone,
        EpochAudience::Commons,
        EpochAudience::Hosts(vec!["mac-1".to_owned(), "mac-2".to_owned()]),
    ]
}

/// Ask `log` every epoch verdict on every one of `macs` and every audience: each checks itself
/// against the code it replaced.
pub(in crate::membership_log) fn probe(log: &Log, macs: &[Mac]) {
    for audience in &audiences() {
        for mac in macs {
            let recipient = mac.identity.recipient();
            let _ = log.audience_left_out_recipient(audience, &recipient);
            let _ = log.audience_left_out(audience, &mac.member);
            let _ = log.wraps_maximal_audience_to(audience, &recipient);
        }
    }
}

/// The three Macs of every generated log, the founder first.
pub(in crate::membership_log) fn three_macs() -> (Vec<Mac>, Vec<Entrant>) {
    (1..=3).map(Mac::new).unzip()
}

/// The log every generated log starts from, for [`three_macs`]' `macs` and `entrants`: the
/// genesis binds the first and lists the third with no binding, the first lets the second in by
/// its phrase and the third by a listed `Admit`, and mints an `[everyone]` epoch wrapped to all
/// three. The log, the epoch and its key.
pub(in crate::membership_log) fn founded(
    macs: &[Mac],
    entrants: &[Entrant],
) -> (Log, RecordId, Zeroizing<[u8; 32]>) {
    let listed = Founder {
        recipient: macs[2].identity.recipient(),
        binding: None,
        node_key: None,
        name: entrants[2].name.clone(),
        label: entrants[2].label.clone(),
    };
    let genesis = Record::genesis(
        &[7; 32],
        Root::of(entrants[0].request.binding()),
        vec![Founder::from(entrants[0].clone()), listed],
        1_000,
    )
    .unwrap();
    let mut log = Log::new(genesis).unwrap();
    for (index, entrant) in entrants.iter().enumerate().skip(1) {
        let admit = log.draft(
            &macs[0].member,
            1_001,
            Body::Admit {
                entrant: entrant.clone(),
                evidence: evidence(index),
            },
        );
        macs[0].sign(&mut log, admit);
    }
    let (draft, key) = log
        .draft_epoch(&macs[0].member, 1_002, Audience::Everyone, 1)
        .unwrap();
    let epoch = macs[0].sign(&mut log, draft);
    for to in macs {
        let wrap = log
            .draft_wrap(&macs[0].member, 1_003, epoch, &key, to.identity.recipient())
            .unwrap();
        macs[0].sign(&mut log, wrap);
    }
    (log, epoch, key)
}

/// How the Mac at `index` of [`three_macs`] is let in by an `Admit` in a generated log: the third
/// is a founder the genesis lists with no binding, so its `Admit`s are listed ones.
const fn evidence(index: usize) -> Evidence {
    if index == 2 {
        Evidence::Listed
    } else {
        Evidence::Phrase
    }
}

/// Reseal `log` as the Mac at `m` of `macs`, as a member that departs a Mac does: attest every
/// uncovered keyed `[everyone]` epoch it holds the key of, mint a fresh `[everyone]` epoch of
/// `generation` wrapped to each member but the one at `left_out` (none past the last), and put
/// every entry it reads again under it. Whether it could: it reads its entries.
fn reseal(
    log: &mut Log,
    macs: &[Mac],
    m: usize,
    ms: u64,
    generation: u64,
    left_out: usize,
) -> bool {
    let mac = &macs[m];
    let Ok(Some(entries)) = log.open_entries(&mac.identity) else {
        return false;
    };
    if let Ok(Some(opened)) = log.open_audience_entries(&mac.identity)
        && opened
            .fully_unwrapped_audiences
            .contains(&EpochAudience::Everyone)
    {
        let keyed = opened.uncovered_keyed.get(&EpochAudience::Everyone);
        for epoch in keyed.into_iter().flatten() {
            if let Some(key) = opened.keyed_keys.get(epoch) {
                let wrap = log
                    .draft_wrap(&mac.member, ms, *epoch, key, mac.identity.recipient())
                    .unwrap();
                mac.sign(log, wrap);
            }
        }
    }
    let (draft, key) = log
        .draft_epoch(&mac.member, ms, Audience::Everyone, generation)
        .unwrap();
    let epoch = mac.sign(log, draft);
    let members = log.membership();
    for (to, other) in macs.iter().enumerate() {
        if to != m && (to == left_out || !members.contains(&other.member)) {
            continue;
        }
        let wrap = log
            .draft_wrap(&mac.member, ms, epoch, &key, other.identity.recipient())
            .unwrap();
        mac.sign(log, wrap);
    }
    for entry in entries.iter().filter(|e| e.audience == Audience::Everyone) {
        let draft = log.draft_put(&mac.member, ms, epoch, &key, entry).unwrap();
        mac.sign(log, draft);
    }
    true
}

/// A signed log from `seed`: three Macs (the first a founder, the third one the genesis lists
/// with no binding and a listed `Admit` lets in), each on its own copy, write, delete, reseal
/// ([`reseal`]), mint a Commons or `[mac-1 mac-2]` epoch wrapped to the members it keeps, remove
/// another at the record that let it in, let a removed one back in (by a listed `Admit` for the
/// third) and, half the time, wrap it the newest `[everyone]` epoch, and take in another's copy,
/// at random; a reseal or mint leaves a Mac out a quarter of the time. So Macs come back after
/// epochs that left them out, with wraps to them before, and epochs a removed minter sealed past
/// its cutoff stay keyed under the writes others put under them. After each step the acting copy
/// opens its entries and is probed ([`probe`]). The copies are merged, each Mac reseals the
/// merge in turn (covering the keyed epochs it holds), and the merge is probed.
pub(in crate::membership_log) fn generated(seed: u64) -> Log {
    let mut rng = Rng(seed.wrapping_mul(0x2545_f491_4f6c_dd1d) | 1);
    let (macs, entrants) = three_macs();
    let (log, _, _) = founded(&macs, &entrants);
    let names = ["a", "b", "c"];
    let mut copies = vec![log.clone(), log.clone(), log];
    let mut generation = 1;
    for step in 0..24u64 {
        let m = rng.below(3);
        let mac = &macs[m];
        let copy = &mut copies[m];
        let ms = 2_000 + step;
        match rng.below(13) {
            0..=3 => {
                let Some((epoch, key)) = newest(copy, mac) else {
                    continue;
                };
                let entry = Entry {
                    name: names[rng.below(3)].to_owned(),
                    value: SecretBytes::from(
                        &[b'v', b'0' + u8::try_from(rng.below(2)).unwrap()][..],
                    ),
                    audience: Audience::Everyone,
                    if_absent: false,
                    anchor: None,
                };
                let draft = copy
                    .draft_put(&mac.member, ms, epoch, &key, &entry)
                    .unwrap();
                mac.sign(copy, draft);
            }
            4 | 5 => {
                let Some((epoch, key)) = newest(copy, mac) else {
                    continue;
                };
                let draft = copy
                    .draft_delete(&mac.member, ms, epoch, &key, names[rng.below(3)])
                    .unwrap();
                mac.sign(copy, draft);
            }
            6 | 7 => {
                generation += 1;
                let left_out = rng.below(4);
                if !reseal(copy, &macs, m, ms, generation, left_out) {
                    continue;
                }
            }
            8 => {
                generation += 1;
                let audience = audiences()[1 + rng.below(2)].clone();
                let (draft, key) = copy
                    .draft_epoch(&mac.member, ms, audience, generation)
                    .unwrap();
                let epoch = mac.sign(copy, draft);
                let left_out = rng.below(4);
                let members = copy.membership();
                for (to, other) in macs.iter().enumerate() {
                    if to != m && (to == left_out || !members.contains(&other.member)) {
                        continue;
                    }
                    let wrap = copy
                        .draft_wrap(&mac.member, ms, epoch, &key, other.identity.recipient())
                        .unwrap();
                    mac.sign(copy, wrap);
                }
            }
            9 => {
                let target = &macs[(m + 1 + rng.below(2)) % 3];
                let Some(entry) = copy.membership().get(&target.member).map(|seat| seat.entry)
                else {
                    continue;
                };
                let removal = copy.draft(
                    &mac.member,
                    ms,
                    Body::Remove {
                        member: target.member,
                        cutoff: entry,
                    },
                );
                mac.sign(copy, removal);
            }
            10 => {
                let back = (m + 1 + rng.below(2)) % 3;
                if copy.membership().contains(&macs[back].member) {
                    continue;
                }
                let admit = copy.draft(
                    &mac.member,
                    ms,
                    Body::Admit {
                        entrant: entrants[back].clone(),
                        evidence: evidence(back),
                    },
                );
                mac.sign(copy, admit);
                if rng.below(2) == 0
                    && let Some((epoch, key)) = newest(copy, mac)
                {
                    let wrap = copy
                        .draft_wrap(
                            &mac.member,
                            ms,
                            epoch,
                            &key,
                            macs[back].identity.recipient(),
                        )
                        .unwrap();
                    mac.sign(copy, wrap);
                }
            }
            _ => {
                let from = copies[rng.below(3)].clone();
                copies[m].merge(&from).unwrap();
            }
        }
        let _ = copies[m].open_entry_set(&macs[m].identity).unwrap();
        probe(&copies[m], &macs);
    }
    let (first, rest) = copies.split_at_mut(1);
    for other in rest.iter() {
        first[0].merge(other).unwrap();
    }
    let mut merged = copies.swap_remove(0);
    for (m, mac) in macs.iter().enumerate() {
        generation += 1;
        let _ = reseal(&mut merged, &macs, m, 3_000, generation, macs.len());
        let _ = merged.open_entry_set(&mac.identity).unwrap();
    }
    probe(&merged, &macs);
    merged
}
