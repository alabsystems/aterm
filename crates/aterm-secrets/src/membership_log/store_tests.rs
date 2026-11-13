// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::age::Identity;
use crate::commons::binding::Request;
use crate::membership_log::{Body, Entrant, Evidence, MemberId};

const NETWORK: [u8; 16] = [0x11; 16];
const AUTHORITY: [u8; 32] = [0x22; 32];

fn entrant(tag: u8, label: &str) -> ([u8; 32], [u8; 16], MemberId, Entrant) {
    let writer = [tag; 32];
    let machine = [tag.wrapping_add(0x20); 16];
    let incarnation = [tag; 16];
    let identity = Identity::from_secret_bytes([tag.wrapping_add(0x40); 32]);
    let request = Request::create(NETWORK, AUTHORITY, machine, incarnation, &identity, &writer)
        .expect("request");
    let member = request.binding().actor();
    let entrant = Entrant {
        request,
        node_key: [tag.wrapping_add(0x80); 32],
        name: format!("mac-{tag}"),
        label: label.to_owned(),
    };
    (writer, incarnation, member, entrant)
}

/// A store's copy shares its records' bytes. A server copies its store to try each POST
/// against it (`server::post_records`), which any LAN host may send, and that copy held
/// every record's bytes again, up to `MAX_RECORDS_STREAM` (about 64 MiB), once per POST.
/// NEGATIVE CONTROL: a `Record` that owns its bytes (`wire: Vec<u8>`, main before this fix)
/// copies them with the store, and the test fails "the copy holds the record's bytes again".
#[test]
fn a_stores_copy_shares_every_records_bytes() {
    let (w1, inc1, m1, e1) = entrant(1, "m1");
    let (_, _, _, e2) = entrant(2, "m2");
    let genesis = Record::found(&[9; 32], vec![e1], 1_000).expect("genesis");
    let admit = Log::new(genesis.clone())
        .expect("log")
        .draft(
            &m1,
            1_001,
            Body::Admit {
                entrant: e2,
                evidence: Evidence::Phrase,
            },
        )
        .sign(&w1, inc1)
        .expect("admit");
    let mut store = Store::for_vault("team").expect("store");
    assert!(store.insert(genesis).expect("genesis"));
    assert!(store.insert(admit).expect("admitted"));
    let copy = store.clone();
    assert_eq!(copy.len(), 2);
    for id in store.inventory() {
        let held = store.get_bytes(&id).expect("held");
        let copied = copy.get_bytes(&id).expect("copied");
        assert!(
            std::ptr::eq(held, copied),
            "the copy holds the record's bytes again"
        );
    }
}

/// A store recovered from a records stream holds its records in no more room than they take:
/// the records cache keeps such a store, and its clones share that room. NEGATIVE CONTROL:
/// a recovery that keeps the room its holds grew into holds two records in room for four.
#[test]
fn a_recovered_store_keeps_no_spare_room_for_its_records() {
    let (w1, inc1, m1, e1) = entrant(1, "m1");
    let (_, _, _, e2) = entrant(2, "m2");
    let genesis = Record::found(&[9; 32], vec![e1], 1_000).expect("genesis");
    let admit = Log::new(genesis.clone())
        .expect("log")
        .draft(
            &m1,
            1_001,
            Body::Admit {
                entrant: e2,
                evidence: Evidence::Phrase,
            },
        )
        .sign(&w1, inc1)
        .expect("admit");
    let mut store = Store::for_vault("team").expect("store");
    assert!(store.insert(genesis).expect("genesis"));
    assert!(store.insert(admit).expect("admitted"));
    let recovered = Store::recover("team", &store.encode()).expect("recovered");
    let order = &recovered.log.as_ref().expect("a rooted store").order;
    assert_eq!(order.len(), 2);
    assert_eq!(order.capacity(), order.len(), "spare room for its records");
}

/// `log`'s running byte counters, against the sums they replace.
fn assert_counters(log: &Log, after: &str) {
    let held: usize = log.order.iter().map(|r| r.bytes().len()).sum();
    let waiting: usize = log.waiting.values().map(|r| r.bytes().len()).sum();
    assert_eq!(log.held_bytes(), held, "held bytes after {after}");
    assert_eq!(log.waiting_bytes(), waiting, "waiting bytes after {after}");
}

/// `Log`'s byte counters (`held_bytes`, `waiting_bytes`) match the sums they replaced after
/// each insert (complete and waiting), each landing, and each record a store at its bound
/// lets go (`make_room`). The sums walked every record on each insert, and a store at its
/// bound walked them again for each record it let go (F23, rule 6).
/// NEGATIVE CONTROL: a `drop_last_waiting` that does not lower `waiting_bytes` (so
/// `make_room` leaves the counter high) fails "waiting bytes after make_room".
#[test]
fn log_byte_counters_match_their_sums() {
    RecordBudgets {
        held: MAX_WAITING,
        ..RecordBudgets::PRODUCTION
    }
    .within(byte_counters_at_a_full_held_store);
}

fn byte_counters_at_a_full_held_store() {
    let (w1, inc1, m1, e1) = entrant(1, "m1");
    let (_, _, _, e2) = entrant(2, "m2");
    // A founder's key: only a seated signer's record waits for its parents.
    let (w_junk, inc_junk, _, e_junk) = entrant(9, "m9");
    let genesis = Record::found(&[9; 32], vec![e1, e_junk], 1_000).expect("genesis");
    let waiting = |i: usize| {
        let mut parent = [0x77; 32];
        parent[..8].copy_from_slice(&(i as u64).to_le_bytes());
        super::super::Draft {
            network: *genesis.id(),
            sequence: 1,
            previous: None,
            parents: vec![parent],
            authored_ms: 2_000 + i as u64,
            body: Body::Name {
                name: format!("w{i}"),
            },
        }
        .sign(&w_junk, inc_junk)
        .expect("signed")
    };
    let mut log = Log::new(genesis.clone()).expect("log");
    log.require_seats();
    assert_counters(&log, "the genesis");
    let admit = log
        .draft(
            &m1,
            1_001,
            Body::Admit {
                entrant: e2,
                evidence: Evidence::Phrase,
            },
        )
        .sign(&w1, inc1)
        .expect("admit");
    let mut grown = log.clone();
    grown.insert(admit.clone()).expect("admit");
    let child = grown
        .draft(
            &m1,
            1_002,
            Body::Name {
                name: "child".to_owned(),
            },
        )
        .sign(&w1, inc1)
        .expect("child");
    assert_eq!(
        log.insert(child).expect("child"),
        super::super::Inserted::Waiting
    );
    assert_counters(&log, "a waiting insert");
    assert_eq!(
        log.insert(waiting(0)).expect("waits"),
        super::super::Inserted::Waiting
    );
    assert_counters(&log, "a second waiting insert");
    assert_eq!(
        log.insert(admit).expect("admit"),
        super::super::Inserted::Held
    );
    assert_eq!(log.waiting.len(), 1, "the child landed");
    assert_counters(&log, "a landing");

    let mut store = Store::with_compiled(Some(*genesis.id()));
    store.insert(genesis.clone()).expect("genesis");
    for i in 0..MAX_WAITING - 1 {
        store.insert(waiting(i)).expect("room to wait");
    }
    assert_counters(store.log().expect("log"), "filling the store");
    let admit = store
        .log()
        .expect("log")
        .draft(
            &m1,
            1_001,
            Body::Admit {
                entrant: entrant(3, "m3").3,
                evidence: Evidence::Phrase,
            },
        )
        .sign(&w1, inc1)
        .expect("admit");
    assert!(
        store
            .insert(admit)
            .expect("a complete record takes a waiting one's place")
    );
    let log = store.log().expect("log");
    assert_eq!(
        log.order.len() + log.waiting.len(),
        MAX_WAITING,
        "make_room let one go"
    );
    assert_counters(log, "make_room");
}

#[test]
fn stream_codec_and_store_converge_across_orders_and_refuse_corruption() {
    let (w1, inc1, m1, e1) = entrant(1, "m1");
    let (_, _, _, e2) = entrant(2, "m2");
    let (_, _, _, e3) = entrant(3, "m3");
    let (w_other, inc_other, _, e_other) = entrant(9, "m9");
    let genesis = Record::found(&[9; 32], vec![e1], 1_000).expect("genesis");
    let other_genesis = Record::found(&[8; 32], vec![e_other], 1_000).expect("other genesis");

    // A store with a compiled genesis digest accepts only that genesis and refuses another.
    let mut pinned = Store::with_compiled(Some(*genesis.id()));
    assert!(pinned.insert(other_genesis).is_err());
    assert!(pinned.insert(genesis.clone()).expect("matching genesis"));

    let left_log = Log::new(genesis.clone()).expect("log");
    let admit_two = left_log
        .draft(
            &m1,
            1_001,
            Body::Admit {
                entrant: e2,
                evidence: Evidence::Phrase,
            },
        )
        .sign(&w1, inc1)
        .expect("admit 2");
    let right_log = Log::new(genesis.clone()).expect("log");
    let admit_three = right_log
        .draft(
            &m1,
            1_002,
            Body::Admit {
                entrant: e3,
                evidence: Evidence::Phrase,
            },
        )
        .sign(&w1, inc1)
        .expect("admit 3");

    // Pre-genesis bogus network unrooted record cannot squat the store's network, and
    // valid out-of-order records land once genesis arrives.
    let bogus_unrooted = super::super::Draft {
        network: [0xde; 32],
        sequence: 1,
        previous: None,
        parents: vec![[0xde; 32]],
        authored_ms: 999,
        body: Body::Name {
            name: "squatter".to_owned(),
        },
    }
    .sign(&w_other, inc_other)
    .expect("signed");
    let mut out_of_order = Store::default();
    assert!(out_of_order.insert(bogus_unrooted).expect("unrooted"));
    assert!(out_of_order.insert(admit_two.clone()).expect("unrooted"));
    assert!(out_of_order.frontier().is_empty());
    assert!(out_of_order.insert(genesis.clone()).expect("genesis"));
    assert_eq!(out_of_order.frontier(), vec![*admit_two.id()]);
    assert_eq!(
        out_of_order
            .missing(&[*genesis.id()])
            .iter()
            .map(|r| *r.id())
            .collect::<Vec<_>>(),
        vec![*admit_two.id()]
    );

    // Divergent frontier diff converges to the union, and batch corruption leaves store intact.
    let mut right_store = Store::default();
    right_store
        .ingest(&encode_records([&genesis, &admit_three]))
        .expect("ingest");
    let diff = out_of_order.missing(&right_store.frontier());

    let mut bad_batch = encode_records([&admit_two]);
    let flip = bad_batch.len() - 1;
    bad_batch[flip] ^= 0x01;
    let before = right_store.inventory();
    assert!(right_store.ingest(&bad_batch).is_err());
    assert_eq!(right_store.inventory(), before);

    assert_eq!(right_store.ingest(&encode_records(&diff)).expect("pull"), 1);
    assert_eq!(
        right_store.inventory(),
        BTreeSet::from([*genesis.id(), *admit_two.id(), *admit_three.id()])
    );

    // An unseated signer's record is refused even while its parent is missing, and never
    // held when the parent arrives.
    let unseated_waiting = super::super::Draft {
        network: *genesis.id(),
        sequence: 1,
        previous: None,
        parents: vec![*admit_three.id()],
        authored_ms: 1_500,
        body: Body::Name {
            name: "forger".to_owned(),
        },
    }
    .sign(&w_other, inc_other)
    .expect("signed");
    assert!(out_of_order.insert(unseated_waiting.clone()).is_err());
    assert!(
        out_of_order
            .insert(admit_three.clone())
            .expect("parent lands")
    );
    assert!(!out_of_order.contains(unseated_waiting.id()));
}

/// A record signed by a key `tag` names, which no founder or `Admit` seats, under `network`,
/// following a parent nobody holds.
fn unseated(network: RecordId, tag: u8, i: usize) -> Record {
    super::super::Draft {
        network,
        sequence: 1,
        previous: None,
        parents: vec![[0x77; 32]],
        authored_ms: 5_000 + i as u64,
        body: Body::Name {
            name: format!("junk-{i}"),
        },
    }
    .sign(&[tag; 32], [tag; 16])
    .expect("signed")
}

/// A stream read with a bound of its own is refused whole past that bound, wherever the held
/// bound stands.
///
/// NEGATIVE CONTROL (in the test): the same stream within the bound, and with the held bound,
/// decodes.
#[test]
fn a_stream_bound_of_its_own_refuses_past_it() {
    let records: Vec<Record> = (0..5).map(|i| unseated([0x55; 32], 0xEE, i)).collect();
    let stream = encode_records(&records);
    assert_eq!(decode_records(&stream).expect("held bound").len(), 5);
    assert_eq!(
        decode_records_bounded(&stream, 5).expect("its bound").len(),
        5
    );
    assert!(decode_records_bounded(&stream, 4).is_err());
}

#[test]
fn a_keyless_flood_of_waiting_records_leaves_room_for_members() {
    let (w1, inc1, m1, e1) = entrant(1, "m1");
    let (_, _, m2, e2) = entrant(2, "m2");
    let genesis = Record::found(&[9; 32], vec![e1], 1_000).expect("genesis");
    let mut store = Store::for_vault("team").expect("store");
    store.ingest(genesis.bytes()).expect("genesis");

    // Any host that reaches a server can POST records signed by a key it made, under the
    // network's public id, waiting on parents that never arrive.
    let flood: Vec<Record> = (0..MAX_HELD_RECORDS - 1)
        .map(|i| unseated(*genesis.id(), 0xEE, i))
        .collect();
    assert!(store.ingest(&encode_records(&flood)).is_err());
    for record in flood.iter().take(3) {
        assert!(store.insert(record.clone()).is_err());
    }
    assert_eq!(store.len(), 1);

    let admit = store
        .log()
        .expect("log")
        .draft(
            &m1,
            1_001,
            Body::Admit {
                entrant: e2,
                evidence: Evidence::Phrase,
            },
        )
        .sign(&w1, inc1)
        .expect("admit");
    assert!(store.insert(admit).expect("a member's admit lands"));
    assert!(store.log().expect("log").membership().contains(&m2));
}

#[test]
fn a_store_with_no_genesis_yet_always_has_room_for_it() {
    let (w1, inc1, m1, e1) = entrant(1, "m1");
    let (w2, inc2, m2, e2) = entrant(2, "m2");
    let genesis = Record::found(&[9; 32], vec![e1], 1_000).expect("genesis");
    // ALab's store while no digest is compiled: nothing pins its network before the genesis.
    let mut store = Store::with_compiled(None);

    // A member's records that came before the genesis: an `Admit`, and a record of the Mac
    // it lets in whose id sorts first.
    let mut log = Log::new(genesis.clone()).expect("log");
    let admit = log
        .draft(
            &m1,
            1_001,
            Body::Admit {
                entrant: e2,
                evidence: Evidence::Phrase,
            },
        )
        .sign(&w1, inc1)
        .expect("admit");
    log.insert(admit.clone()).expect("admit");
    let name = (0..64)
        .map(|i| {
            log.draft(
                &m2,
                1_002,
                Body::Name {
                    name: format!("two-{i}"),
                },
            )
            .sign(&w2, inc2)
            .expect("name")
        })
        .find(|name| name.id() < admit.id())
        .expect("one of 64 ids sorts first");
    assert!(store.insert(name.clone()).expect("waits for the genesis"));
    assert!(store.insert(admit.clone()).expect("waits for the genesis"));

    // Then a flood under any network id, before the genesis arrives.
    let mut accepted = 2;
    for i in 0..MAX_WAITING {
        let network = if i % 2 == 0 {
            [0xAB; 32]
        } else {
            *genesis.id()
        };
        if store.insert(unseated(network, 0xEE, i)).is_ok() {
            accepted += 1;
        }
    }
    assert_eq!(accepted, MAX_WAITING - 1);

    assert!(store.insert(genesis.clone()).expect("the genesis lands"));
    assert_eq!(
        store.inventory(),
        BTreeSet::from([*genesis.id(), *admit.id(), *name.id()])
    );
    let membership = store.log().expect("log").membership();
    assert_eq!(
        membership.get(&m2).map(|m| m.name.as_str()),
        Some(match name.body() {
            Body::Name { name } => name.as_str(),
            _ => unreachable!("a name"),
        })
    );
}

#[test]
fn a_peer_stream_with_a_record_this_store_refuses_still_brings_the_rest() {
    let (wa, inca, ma, ea) = entrant(1, "m1");
    let (_, _, _, er) = entrant(2, "m2");
    let (_, _, mc, ec) = entrant(3, "m3");
    let (wx, incx, _, _) = entrant(9, "m9");
    let genesis = Record::found(&[9; 32], vec![ea, er], 1_000).expect("genesis");
    let mut here = Store::for_vault("team").expect("store");
    assert!(here.insert(genesis.clone()).expect("genesis"));
    let admit = here
        .log()
        .expect("log")
        .draft(
            &ma,
            1_001,
            Body::Admit {
                entrant: ec,
                evidence: Evidence::Phrase,
            },
        )
        .sign(&wa, inca)
        .expect("admit");

    // A record from a key no record seats, ahead of a member's `Admit`, in one stream.
    let stray = super::super::Draft {
        network: *genesis.id(),
        sequence: 1,
        previous: None,
        parents: vec![*genesis.id()],
        authored_ms: 1_002,
        body: Body::Name {
            name: "stray".to_owned(),
        },
    }
    .sign(&wx, incx)
    .expect("signed");
    let stream = encode_records([&stray, &admit]);
    assert_eq!(here.ingest(&stream).expect("the admit enters"), 1);
    assert!(here.log().expect("log").membership().contains(&mc));
    assert!(!here.contains(stray.id()));
    // A stream the store takes nothing from still fails, as one record a POST sends alone.
    assert!(here.ingest(stray.bytes()).is_err());
}

#[test]
fn a_made_up_genesis_never_binds_alabs_store_while_this_build_compiles_no_digest() {
    // Any host can sign a genesis with a seed it makes up and copy the real Commons root into
    // it, and the first genesis a store takes binds it. With no digest compiled (until F12b)
    // ALab's store takes only the genesis the ALab vault's key signed; after F12b the digest
    // refuses the made-up one instead.
    let (_, _, _, e1) = entrant(1, "m1");
    let (_, _, _, e2) = entrant(2, "m2");
    let made_up = Record::found(&[0x66; 32], vec![e1], 1_000).expect("genesis");
    let mut alab = Store::for_vault(crate::ALAB_VAULT).expect("alab store");
    assert!(
        alab.insert(made_up.clone()).is_err(),
        "a made-up genesis bound ALab's store"
    );
    assert!(alab.ingest(made_up.bytes()).is_err());
    assert!(Store::decode(crate::ALAB_VAULT, made_up.bytes()).is_err());
    assert!(alab.is_empty() && alab.log().is_none());

    // A dormant or held-key store under a vault key takes the genesis that key signed, and
    // only that.
    let ours = Record::found(&[0x55; 32], vec![e2], 1_000).expect("genesis");
    let Body::Genesis { signer, .. } = ours.body() else {
        unreachable!("a genesis");
    };
    let mut dormant = Store::dormant(&[*signer]);
    assert!(dormant.insert(made_up.clone()).is_err());
    assert!(
        dormant
            .insert(ours.clone())
            .expect("the vault key's genesis")
    );
    let mut held_team = Store::for_vault("team")
        .expect("team store")
        .under(std::slice::from_ref(signer));
    assert!(held_team.insert(made_up.clone()).is_err());
    assert!(held_team.insert(ours).expect("held team key's genesis"));

    // An unheld non-ALab vault's store still takes its first genesis: its server takes one
    // only from its own Mac or under a pin.
    let mut team = Store::for_vault("team").expect("team store");
    assert!(team.insert(made_up).expect("team genesis"));
}

/// A rotation of the ALab vault's key asks the next release to name the new key
/// (`ALAB_SIGNER`) and retire the old one (`ALAB_RETIRED`); until F12b compiles a digest,
/// ALab's store is dormant. Keyed to the new key alone, it refused the Genesis v2 the old key
/// signed on every Mac on that release (`Store::recover` in `client::load_records`), rooted
/// nothing, and the next write erased the genesis from the records file, which no dormant
/// build could take back. A dormant ALab store now takes a genesis that any key this build
/// names for the vault signed: the current one or a retired one.
/// NEGATIVE CONTROL: `Store::for_vault` keyed to `ALAB_SIGNER` alone (main before this fix)
/// roots nothing.
#[test]
fn a_release_that_retires_the_key_that_signed_alabs_genesis_keeps_it() {
    use crate::envelope::{Retired, TEST_RETIRED, signer_public};
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            TEST_RETIRED.with(|cell| *cell.borrow_mut() = None);
        }
    }
    let old_seed = [0x57; 32];
    let old = signer_public(&old_seed).expect("public");
    let new = signer_public(&[0x58; 32]).expect("public");
    TEST_RETIRED.with(|cell| {
        *cell.borrow_mut() = Some(Retired {
            keys: vec![old],
            current: Some(new),
        });
    });
    let _reset = Reset;

    let (w1, inc1, m1, e1) = entrant(1, "m1");
    let (_, _, _, e2) = entrant(2, "m2");
    let (_, _, _, e3) = entrant(3, "m3");
    let genesis = Record::found(&old_seed, vec![e1], 1_000).expect("genesis");
    let admit = Log::new(genesis.clone())
        .expect("log")
        .draft(
            &m1,
            1_001,
            Body::Admit {
                entrant: e2,
                evidence: Evidence::Phrase,
            },
        )
        .sign(&w1, inc1)
        .expect("admit");
    let file = encode_records([&genesis, &admit]);
    let held = Store::recover(crate::ALAB_VAULT, &file).expect("recover");
    assert!(
        held.log().is_some(),
        "the release that retired the genesis key refused ALab's genesis"
    );
    assert!(
        Store::decode(crate::ALAB_VAULT, &held.encode())
            .expect("the next write keeps the genesis")
            .contains(genesis.id())
    );

    // A genesis signed by a key this build does not name still binds nothing.
    let made_up = Record::found(&[0x66; 32], vec![e3], 1_000).expect("genesis");
    let mut alab = Store::for_vault(crate::ALAB_VAULT).expect("alab store");
    assert!(alab.insert(made_up).is_err());
}

/// A pull names only the heads its URL has room for (`transport::records_have_url`), so the
/// frontier lists first the heads that name most of the log: the deepest. A key's forks at
/// sequence one, each a head right after its seat, come after an honest chain's head.
/// NEGATIVE CONTROL: heads in id order (`Log::heads`, main before this fix) put the fork
/// first.
#[test]
fn the_frontier_lists_the_deepest_head_first() {
    let (w1, inc1, m1, e1) = entrant(1, "m1");
    let (w2, inc2, _, e2) = entrant(2, "m2");
    let genesis = Record::found(&[9; 32], vec![e1], 1_000).expect("genesis");
    let mut log = Log::new(genesis.clone()).expect("log");
    let admit = log
        .draft(
            &m1,
            1_001,
            Body::Admit {
                entrant: e2,
                evidence: Evidence::Phrase,
            },
        )
        .sign(&w1, inc1)
        .expect("admit");
    log.insert(admit.clone()).expect("admit");
    let mut chain = Vec::new();
    for n in 0..2u64 {
        let next = log
            .draft(
                &m1,
                1_002 + n,
                Body::Name {
                    name: format!("m1-{n}"),
                },
            )
            .sign(&w1, inc1)
            .expect("chain");
        log.insert(next.clone()).expect("chain");
        chain.push(next);
    }
    let deep = chain.last().expect("a chain head").clone();
    let fork = (0..4_096u64)
        .map(|salt| {
            super::super::Draft {
                network: *genesis.id(),
                sequence: 1,
                previous: None,
                parents: vec![*admit.id()],
                authored_ms: 5_000 + salt,
                body: Body::Name {
                    name: "fork".to_owned(),
                },
            }
            .sign(&w2, inc2)
            .expect("fork")
        })
        .find(|fork| fork.id() < deep.id())
        .expect("a fork whose id sorts first");
    let mut store = Store::for_vault("team").expect("store");
    store.insert_many([genesis, admit, fork.clone()].into_iter().chain(chain));
    assert_eq!(store.frontier(), vec![*deep.id(), *fork.id()]);
}

/// A log derives what a pull names of it once for its held records ([`Advertised`]), and its
/// clones share that: after each record it holds, asked first through a clone, its frontier
/// and `?have=` ids are those a store given the same records afresh names, and the clone,
/// which held nothing, names what it named. A clone taken where nothing asked since the last
/// record, of a store that then holds another and is asked first, names its own records.
/// NEGATIVE CONTROL: `Log::hold` keeping the derivation names the heads before the record it
/// held at the store's first ask after it; with the loop's records left out, it names the
/// store's records in that clone.
#[test]
fn what_a_pull_names_follows_each_record_the_log_holds() {
    let (w1, inc1, m1, e1) = entrant(1, "m1");
    let genesis = Record::found(&[9; 32], vec![e1], 1_000).expect("genesis");
    let named = |records: &[Record]| {
        let mut store = Store::for_vault("team").expect("store");
        store.insert_many(records.iter().cloned());
        (store.frontier(), store.have())
    };
    let mut records = vec![genesis.clone()];
    let mut store = Store::for_vault("team").expect("store");
    store.insert_many([genesis]);
    for n in 0..6u64 {
        let asked = store.clone();
        let before = (asked.frontier(), asked.have());
        assert_eq!(before, named(&records));
        let next = store
            .log()
            .expect("a log")
            .draft(
                &m1,
                1_001 + n,
                Body::Name {
                    name: format!("m1-{n}"),
                },
            )
            .sign(&w1, inc1)
            .expect("a name");
        assert!(store.insert(next.clone()).expect("held"));
        records.push(next);
        assert_eq!((store.frontier(), store.have()), named(&records));
        assert_eq!((asked.frontier(), asked.have()), before);
    }
    let hold = |store: &mut Store, records: &mut Vec<Record>, n: u64| {
        let next = store
            .log()
            .expect("a log")
            .draft(
                &m1,
                2_000 + n,
                Body::Name {
                    name: format!("m1-later-{n}"),
                },
            )
            .sign(&w1, inc1)
            .expect("a name");
        assert!(store.insert(next.clone()).expect("held"));
        records.push(next);
    };
    // Nothing asks after this record, so the clone shares a derivation nothing made yet.
    hold(&mut store, &mut records, 0);
    let unasked = store.clone();
    let its_own = records.clone();
    hold(&mut store, &mut records, 1);
    assert_eq!((store.frontier(), store.have()), named(&records));
    assert_eq!((unasked.frontier(), unasked.have()), named(&its_own));
}

#[test]
fn records_missing_a_parent_never_shut_out_the_genesis_or_a_record_whose_parents_are_held() {
    RecordBudgets {
        held: MAX_WAITING,
        ..RecordBudgets::PRODUCTION
    }
    .within(missing_parents_at_a_full_held_store);
}

fn missing_parents_at_a_full_held_store() {
    let (w1, inc1, m1, e1) = entrant(1, "m1");
    let (_, _, _, e2) = entrant(2, "m2");
    // A founder's key, so its records wait in a store's log: a Mac removed later keeps its
    // seat, and its key still signs.
    let (w_junk, inc_junk, _, e_junk) = entrant(9, "m9");
    let genesis = Record::found(&[9; 32], vec![e1, e_junk], 1_000).expect("genesis");
    let admit_two = Log::new(genesis.clone())
        .expect("log")
        .draft(
            &m1,
            1_001,
            Body::Admit {
                entrant: e2,
                evidence: Evidence::Phrase,
            },
        )
        .sign(&w1, inc1)
        .expect("admit 2");
    // A seated key's records naming the genesis's network (in every binary once F12b compiles
    // its digest), each after a parent that exists nowhere: more than a store keeps.
    let mut junk: Vec<Record> = (0..=MAX_WAITING)
        .map(|i| {
            let mut parent = [0x77; 32];
            parent[..8].copy_from_slice(&(i as u64).to_le_bytes());
            super::super::Draft {
                network: *genesis.id(),
                sequence: 1,
                previous: None,
                parents: vec![parent],
                authored_ms: 2_000 + i as u64,
                body: Body::Name {
                    name: format!("junk-{i}"),
                },
            }
            .sign(&w_junk, inc_junk)
            .expect("signed")
        })
        .collect();
    let extra = junk.pop().expect("one more");

    // A store with no digest and an F12b store pinned to the genesis both take the junk
    // before the genesis, where nothing can tell it from honest early records.
    for compiled in [None, Some(*genesis.id())] {
        let mut store = Store::with_compiled(compiled);
        for record in &junk {
            let _ = store.insert(record.clone());
        }
        assert_eq!(store.len(), MAX_WAITING - 1);
        assert!(
            store
                .insert(genesis.clone())
                .expect("the genesis finds room")
        );
        assert!(
            store
                .insert(admit_two.clone())
                .expect("a record whose parents are held finds room")
        );
        assert!(store.log().expect("rooted").get(admit_two.id()).is_some());
        assert_eq!(store.len(), MAX_WAITING);
        assert!(
            store.insert(extra.clone()).is_err(),
            "a record missing a parent finds no room in a full store"
        );
        // The full store still reads back whole from its own stream.
        let mut reloaded = Store::with_compiled(compiled);
        reloaded.ingest(&store.encode()).expect("reloads");
        assert_eq!(reloaded.inventory(), store.inventory());
    }
}

#[test]
fn only_causally_complete_records_travel() {
    let (w1, inc1, m1, e1) = entrant(1, "m1");
    let (_, _, _, e2) = entrant(2, "m2");
    // A founder's key: only a seated signer's record waits for its parents.
    let (w_junk, inc_junk, _, e_junk) = entrant(9, "m9");
    let genesis = Record::found(&[9; 32], vec![e1, e_junk], 1_000).expect("genesis");
    let admit_two = Log::new(genesis.clone())
        .expect("log")
        .draft(
            &m1,
            1_001,
            Body::Admit {
                entrant: e2,
                evidence: Evidence::Phrase,
            },
        )
        .sign(&w1, inc1)
        .expect("admit 2");
    let orphan = super::super::Draft {
        network: *genesis.id(),
        sequence: 1,
        previous: None,
        parents: vec![[0x77; 32]],
        authored_ms: 2_000,
        body: Body::Name {
            name: "orphan".to_owned(),
        },
    }
    .sign(&w_junk, inc_junk)
    .expect("signed");

    let mut store = Store::default();
    assert!(store.insert(orphan.clone()).expect("waits for a genesis"));
    assert!(
        store.missing(&[]).is_empty(),
        "nothing travels before a genesis"
    );
    assert!(store.insert(genesis.clone()).expect("genesis"));
    assert!(store.insert(admit_two.clone()).expect("admit 2"));
    assert!(
        store.contains(orphan.id()),
        "the orphan waits for its parent"
    );
    // `have` names complete heads only, so a waiting record in a reply would go out again on
    // every pull: a puller at this store's frontier gets nothing.
    assert!(store.missing(&store.frontier()).is_empty());
    assert_eq!(
        store
            .missing(&[])
            .iter()
            .map(|r| *r.id())
            .collect::<Vec<_>>(),
        vec![*genesis.id(), *admit_two.id()]
    );
}

#[test]
fn waiting_records_stop_at_max_waiting() {
    let (w1, inc1, m1, e1) = entrant(1, "m1");
    let genesis = Record::found(&[9; 32], vec![e1], 1_000).expect("genesis");
    let mut log = Log::new(genesis.clone()).expect("log");
    let missing_parent = [0x55; 32];
    for i in 0..MAX_WAITING {
        let draft = super::super::Draft {
            network: *genesis.id(),
            sequence: 1,
            previous: None,
            parents: vec![missing_parent],
            authored_ms: 2_000 + i as u64,
            body: Body::Name {
                name: format!("host-{i}"),
            },
        };
        let record = draft.sign(&w1, inc1).expect("signed");
        assert_eq!(
            log.insert(record).expect("within cap"),
            super::super::Inserted::Waiting
        );
    }
    let over = super::super::Draft {
        network: *genesis.id(),
        sequence: 1,
        previous: None,
        parents: vec![missing_parent],
        authored_ms: 99_999,
        body: Body::Name {
            name: "overflow".to_owned(),
        },
    }
    .sign(&w1, inc1)
    .expect("signed");
    let _ = m1;
    let err = log.insert(over).expect_err("past MAX_WAITING");
    assert!(err.to_string().contains("waiting records"), "{err}");
}

#[test]
fn ingest_and_waiting_seat_keep_out_of_order_entrant_records() {
    let (w1, inc1, m1, e1) = entrant(1, "m1");
    let (w2, inc2, m2, e2) = entrant(2, "m2");
    let genesis = Record::found(&[9; 32], vec![e1], 1_000).expect("genesis");
    let mut author = Log::new(genesis.clone()).expect("log");
    let first_by_m1 = author
        .draft(
            &m1,
            1_001,
            Body::Name {
                name: "m1-renamed".to_owned(),
            },
        )
        .sign(&w1, inc1)
        .expect("first by m1");
    author.insert(first_by_m1.clone()).expect("hold first");
    let admit = author
        .draft(
            &m1,
            1_002,
            Body::Admit {
                entrant: e2,
                evidence: Evidence::Phrase,
            },
        )
        .sign(&w1, inc1)
        .expect("admit");
    author.insert(admit.clone()).expect("hold admit");
    let name = (0..64)
        .map(|i| {
            author
                .draft(
                    &m2,
                    1_003 + i,
                    Body::Name {
                        name: format!("m2-renamed-{i}"),
                    },
                )
                .sign(&w2, inc2)
                .expect("name")
        })
        .find(|r| r.id() < admit.id())
        .expect("one of 64 ids sorts before admit");

    // 1. Out-of-order stream `[name, genesis, first_by_m1, admit]` into an empty store.
    let mut from_empty = Store::for_vault("team").expect("store");
    let full_stream = encode_records([&name, &genesis, &first_by_m1, &admit]);
    assert_eq!(from_empty.ingest(&full_stream).expect("ingest all 4"), 4);
    assert!(from_empty.log().expect("log").membership().contains(&m2));

    // 2. Out-of-order stream `[name, admit]` after genesis and `first_by_m1` are held.
    let mut seeded = Store::for_vault("team").expect("store");
    assert!(seeded.insert(genesis.clone()).expect("genesis"));
    assert!(seeded.insert(first_by_m1.clone()).expect("first_by_m1"));
    let child_before_seat = encode_records([&name, &admit]);
    assert_eq!(seeded.ingest(&child_before_seat).expect("ingest 2"), 2);
    assert!(seeded.log().expect("log").membership().contains(&m2));

    // 3. `admit` waits on `first_by_m1` while `name` arrives, then `Log::merge` copies both
    //    waiting records even though `name.id() < admit.id()`.
    let mut waiting = Store::for_vault("team").expect("store");
    assert!(waiting.insert(genesis.clone()).expect("genesis"));
    assert!(waiting.insert(admit.clone()).expect("admit waits"));
    assert!(
        waiting
            .insert(name.clone())
            .expect("name waits under waiting seat")
    );
    // Neither travels while it waits: `have` names complete heads only, so a waiting record
    // would go out again on every pull. Both go once `first_by_m1` lands.
    assert!(waiting.missing(&[*genesis.id()]).is_empty());
    let mut merged = Log::new(genesis).expect("merged log");
    merged.require_seats();
    merged
        .merge(waiting.log().expect("waiting log"))
        .expect("merge waiting records");
    assert_eq!(
        merged.insert(first_by_m1).expect("missing parent lands"),
        super::super::Inserted::Held
    );
    assert_eq!(
        merged.membership().get(&m2).map(|m| m.name.as_str()),
        Some(match name.body() {
            Body::Name { name } => name.as_str(),
            _ => unreachable!("a name"),
        })
    );
}
#[test]
fn complete_inventory_and_complete_bytes_omit_waiting_and_unrooted_records() {
    let (w1, inc1, m1, e1) = entrant(1, "m1");
    let (_, _, _, e2) = entrant(2, "m2");
    let (w_junk, inc_junk, _, e_junk) = entrant(9, "m9");
    let genesis = Record::found(&[9; 32], vec![e1, e_junk], 1_000).expect("genesis");
    let admit_two = Log::new(genesis.clone())
        .expect("log")
        .draft(
            &m1,
            1_001,
            Body::Admit {
                entrant: e2,
                evidence: Evidence::Phrase,
            },
        )
        .sign(&w1, inc1)
        .expect("admit 2");
    let orphan = super::super::Draft {
        network: *genesis.id(),
        sequence: 1,
        previous: None,
        parents: vec![[0x77; 32]],
        authored_ms: 2_000,
        body: Body::Name {
            name: "orphan".to_owned(),
        },
    }
    .sign(&w_junk, inc_junk)
    .expect("signed");

    let mut store = Store::default();
    assert!(store.insert(orphan.clone()).expect("unrooted"));
    assert!(store.complete_inventory().is_empty());
    assert!(store.complete_bytes(orphan.id()).is_none());
    assert!(
        store
            .complete_missing(&BTreeSet::new(), None, 16)
            .is_empty()
    );

    assert!(store.insert(genesis.clone()).expect("genesis"));
    assert!(store.insert(admit_two.clone()).expect("admit 2"));
    assert!(store.contains(orphan.id()));
    assert_eq!(
        store.complete_inventory(),
        BTreeSet::from([*genesis.id(), *admit_two.id()])
    );
    assert_eq!(store.complete_bytes(genesis.id()), Some(genesis.bytes()));
    assert_eq!(
        store.complete_bytes(admit_two.id()),
        Some(admit_two.bytes())
    );
    assert!(store.complete_bytes(orphan.id()).is_none());
    assert_eq!(
        store.complete_missing(&BTreeSet::new(), None, 16),
        vec![*genesis.id(), *admit_two.id()]
    );
    assert_eq!(
        store.complete_missing(&BTreeSet::new(), Some(genesis.id()), 16),
        vec![*admit_two.id(), *genesis.id()]
    );
}

#[test]
fn oversized_waiting_records_stay_within_stream_bound_and_yield_to_complete_records() {
    let (w1, inc1, m1, e1) = entrant(1, "m1");
    let (_, _, _, e2) = entrant(2, "m2");
    let (w_junk, inc_junk, _, e_junk) = entrant(9, "m9");
    let genesis = Record::found(&[9; 32], vec![e1, e_junk], 1_000).expect("genesis");
    let mut log = Log::new(genesis.clone()).expect("log");
    let admit_two = log
        .draft(
            &m1,
            1_001,
            Body::Admit {
                entrant: e2,
                evidence: Evidence::Phrase,
            },
        )
        .sign(&w1, inc1)
        .expect("admit 2");
    log.insert(admit_two.clone()).expect("admit 2");
    let (draft_ep, ep_key) = log
        .draft_epoch(&m1, 1_002, super::super::EpochAudience::Everyone, 1)
        .expect("draft epoch");
    let ep_rec = draft_ep.sign(&w1, inc1).expect("epoch");
    log.insert(ep_rec.clone()).expect("epoch");
    let big_entry = crate::text::Entry {
        name: "BIG_SECRET".to_owned(),
        value: crate::SecretBytes::new(vec![b'x'; crate::text::MAX_VALUE]),
        if_absent: false,
        anchor: None,
        audience: crate::text::Audience::Everyone,
    };
    let complete_put = log
        .draft_put(&m1, 1_003, *ep_rec.id(), &ep_key, &big_entry)
        .expect("draft put")
        .sign(&w1, inc1)
        .expect("complete put");

    // 65 waiting `Put` records (~1 MiB each) from a seated key, each missing its parent:
    // far fewer than `MAX_WAITING` (4096), yet > `MAX_RECORDS_STREAM` (64 MiB) in bytes.
    let ciphertext = vec![0xAAu8; crate::text::MAX_VALUE + 32];
    let mut waiting_puts: Vec<Record> = (0..66)
        .map(|i| {
            let mut parent = [0x77; 32];
            parent[..8].copy_from_slice(&(i as u64).to_le_bytes());
            super::super::Draft {
                network: *genesis.id(),
                sequence: 1,
                previous: None,
                parents: vec![parent],
                authored_ms: 3_000 + i as u64,
                body: Body::Put {
                    audience: super::super::EpochAudience::Everyone,
                    epoch: parent,
                    salt: [i as u8; 32],
                    ciphertext: ciphertext.clone().into(),
                },
            }
            .sign(&w_junk, inc_junk)
            .expect("waiting put")
        })
        .collect();
    let extra = waiting_puts.pop().expect("extra waiting put");

    let mut store = Store::with_compiled(Some(*genesis.id()));
    for record in &waiting_puts {
        let _ = store.insert(record.clone());
    }
    assert!(store.encode().len() <= MAX_RECORDS_STREAM - MAX_RECORD);
    assert!(store.insert(extra.clone()).is_err());

    // The genesis and complete records (including a 1 MiB Put) land and displace waiting
    // records by byte size so `store.encode()` stays readable by `decode_records`.
    assert!(store.insert(genesis).expect("genesis lands"));
    for record in &waiting_puts {
        let _ = store.insert(record.clone());
    }
    assert!(store.insert(extra.clone()).is_err());
    assert!(store.insert(admit_two.clone()).expect("admit lands"));
    assert!(store.insert(ep_rec.clone()).expect("epoch lands"));
    assert!(store.insert(complete_put.clone()).expect("1 MiB Put lands"));
    let encoded = store.encode();
    assert!(
        encoded.len() <= MAX_RECORDS_STREAM,
        "encoded len {} exceeded MAX_RECORDS_STREAM {}",
        encoded.len(),
        MAX_RECORDS_STREAM
    );
    assert!(store.insert(extra).is_err());

    let mut reloaded = Store::with_compiled(store.compiled);
    reloaded
        .ingest(&encoded)
        .expect("stream reloads within MAX_RECORDS_STREAM");
    assert!(
        reloaded
            .log()
            .expect("log")
            .get(complete_put.id())
            .is_some()
    );
    assert_eq!(reloaded.inventory(), store.inventory());
}

/// A Mac admitted again after a record of its own (F16's Allow again, or any seated key with
/// its public request): `causal` put every record after each `Admit` of its signer, and that
/// `Admit` follows the record, so the order looped and fell back to id order. A record of a
/// Mac whose `Admit` sorts later in that tail then waited with no seat and was refused, on
/// every reload (`decode`, `recover`, a fresh store pulling `missing`), and the next write
/// dropped it for good.
/// NEGATIVE CONTROL: a seat edge from every record to each `Admit` of its signer, and the
/// records that never release appended in id order (main before this fix), hold 5 of the 6.
#[test]
fn a_mac_admitted_again_after_its_own_record_keeps_every_record_across_a_reload() {
    fn signed(
        store: &Store,
        actor: &MemberId,
        body: Body,
        key: &[u8; 32],
        inc: [u8; 16],
    ) -> Record {
        store
            .log()
            .expect("log")
            .draft(actor, 1_001, body)
            .sign(key, inc)
            .expect("signed")
    }
    let (w1, inc1, m1, e1) = entrant(1, "m1");
    let (w2, inc2, m2, e2) = entrant(2, "m2");
    let (w3, inc3, m3, e3) = entrant(3, "m3");
    let genesis = Record::found(&[9; 32], vec![e1], 1_000).expect("genesis");
    let mut store = Store::for_vault("team").expect("store");
    assert!(store.insert(genesis).expect("genesis"));
    let admit = |entrant: &Entrant| Body::Admit {
        entrant: entrant.clone(),
        evidence: Evidence::Phrase,
    };
    let admit_two = signed(&store, &m1, admit(&e2), &w1, inc1);
    assert!(store.insert(admit_two).expect("m2 admitted"));
    let name_two = signed(
        &store,
        &m2,
        Body::Name {
            name: "two".to_owned(),
        },
        &w2,
        inc2,
    );
    assert!(store.insert(name_two.clone()).expect("m2 names itself"));
    let again = signed(&store, &m1, admit(&e2), &w1, inc1);
    assert!(
        again.parents().contains(name_two.id()),
        "the Admit follows m2's own record"
    );
    assert!(store.insert(again).expect("m2 admitted again"));
    let admit_three = signed(&store, &m2, admit(&e3), &w2, inc2);
    assert!(store.insert(admit_three.clone()).expect("m3 admitted"));
    let name_three = (0..64)
        .map(|i| {
            signed(
                &store,
                &m3,
                Body::Name {
                    name: format!("three-{i}"),
                },
                &w3,
                inc3,
            )
        })
        .find(|r| r.id() < admit_three.id())
        .expect("one of 64 ids sorts before m3's Admit");
    assert!(store.insert(name_three.clone()).expect("m3 names itself"));
    assert_eq!(store.len(), 6);
    let shown = |s: &Store| {
        s.log()
            .expect("log")
            .membership()
            .get(&m3)
            .map(|m| m.name.clone())
    };
    let Body::Name { name } = name_three.body() else {
        unreachable!("a name");
    };
    assert_eq!(shown(&store).as_deref(), Some(name.as_str()));

    let bytes = store.encode();
    let decoded = Store::decode("team", &bytes).expect("decode");
    let recovered = Store::recover("team", &bytes).expect("recover");
    let mut pulled = Store::for_vault("team").expect("store");
    pulled
        .ingest(&encode_records(&store.missing(&[])))
        .expect("a fresh store pulls everything");
    for (how, reloaded) in [
        ("decode", &decoded),
        ("recover", &recovered),
        ("pull", &pulled),
    ] {
        assert_eq!(reloaded.inventory(), store.inventory(), "{how}");
        assert_eq!(shown(reloaded), shown(&store), "{how}");
    }
}

/// A founder's genesis, its `Name` `n0`, and two more `Name`s after `n0` (siblings, each
/// with `n0` as its parent), with the founder's writer seed and incarnation.
#[cfg(unix)]
fn founder_with_siblings() -> (Record, Record, Record, Record, [u8; 32], [u8; 16]) {
    let (writer, incarnation, founder, entrant) = entrant(1, "m1");
    let genesis = Record::found(&[9; 32], vec![entrant], 1_000).expect("genesis");
    let mut log = Log::new(genesis.clone()).expect("log");
    let name = |log: &Log, name: &str| {
        log.draft(
            &founder,
            2_000,
            Body::Name {
                name: name.to_owned(),
            },
        )
        .sign(&writer, incarnation)
        .expect("signed")
    };
    let n0 = name(&log, "n0");
    log.insert(n0.clone()).expect("held");
    let x = name(&log, "x");
    let n1 = name(&log, "n1");
    (genesis, n0, x, n1, writer, incarnation)
}

/// A kind code no build writes yet: a test's stand-in for a newer build's record.
#[cfg(unix)]
const NEWER_KIND: u8 = 0xee;

/// A stream holding a record of a kind this build does not know still splits into its
/// records' ids and parents (`frames`), which a pull names back to the server that sent it
/// (`untaken`), while `decode_records` and `Record::wire_len_prefix` still refuse it: the
/// layout frames every kind alike.
/// NEGATIVE CONTROL: framing through the kind check (`Record::frame_prefix` calling
/// `Kind::from_code`) makes `frames` refuse the stream ("frames a kind this build does not
/// know: Error(\"membership record of an unknown kind\", Other)").
/// NEGATIVE CONTROL: dropping the kind check from `Record::wire_len_prefix` fails "the
/// length prefix still refuses an unknown kind".
#[cfg(unix)]
#[test]
fn a_frame_of_a_kind_this_build_does_not_know_still_frames() {
    let (genesis, n0, x, n1, _, _) = founder_with_siblings();
    let unread = crate::membership_log::with_kind_code(&x, NEWER_KIND);
    let mut stream = n0.bytes().to_vec();
    stream.extend_from_slice(&unread);
    stream.extend_from_slice(n1.bytes());

    let framed = match frames(&stream) {
        Ok(framed) => framed,
        Err(e) => panic!("frames a kind this build does not know: {e:?}"),
    };
    let ids: Vec<RecordId> = framed.iter().map(|frame| frame.id).collect();
    assert_eq!(
        ids,
        vec![*n0.id(), aterm_digest::Sha256::digest(&unread), *n1.id()]
    );
    let parents: Vec<&[RecordId]> = framed
        .iter()
        .map(|frame| frame.parents.as_slice())
        .collect();
    assert_eq!(
        parents,
        vec![
            std::slice::from_ref(genesis.id()),
            std::slice::from_ref(n0.id()),
            std::slice::from_ref(n0.id())
        ]
    );
    assert_eq!(stream.get(framed[1].range.clone()), Some(unread.as_slice()));
    assert!(
        decode_records(&stream).is_err(),
        "the stream is refused whole"
    );
    assert!(
        Record::wire_len_prefix(&unread).is_err(),
        "the length prefix still refuses an unknown kind"
    );
    assert_eq!(
        Record::wire_len_prefix(n0.bytes()).ok(),
        Some(n0.bytes().len())
    );
}

/// What a pull offers its store of a stream that does not decode as a whole: every record
/// this build reads (an ancestor and a sibling of the unreadable one) but none that descends
/// from it, which could only wait on a parent that never lands.
/// NEGATIVE CONTROL: leaving out only the frames that fail `Record::decode` (no descent)
/// keeps the child of the unreadable record ("left: [\"n0\", \"y\", \"n1\"], right:
/// [\"n0\", \"n1\"]").
#[cfg(unix)]
#[test]
fn the_readable_part_of_a_stream_leaves_out_what_descends_from_a_record_this_build_cannot_read() {
    let (genesis, n0, x, n1, writer, incarnation) = founder_with_siblings();
    let unread = crate::membership_log::with_kind_code(&x, NEWER_KIND);
    let unread_id = aterm_digest::Sha256::digest(&unread);
    let y = super::super::Draft {
        network: *genesis.id(),
        sequence: 3,
        previous: Some(unread_id),
        parents: vec![unread_id],
        authored_ms: 3_000,
        body: Body::Name {
            name: "y".to_owned(),
        },
    }
    .sign(&writer, incarnation)
    .expect("signed");
    let mut stream = n0.bytes().to_vec();
    stream.extend_from_slice(&unread);
    stream.extend_from_slice(y.bytes());
    stream.extend_from_slice(n1.bytes());

    let framed = frames(&stream).expect("framed");
    let part = readable_part(&stream, &framed);
    let names = |bytes: &[u8]| -> Vec<String> {
        decode_records(bytes)
            .expect("the readable part decodes")
            .iter()
            .map(|record| match record.body() {
                Body::Name { name } => name.clone(),
                _ => String::new(),
            })
            .collect()
    };
    assert_eq!(names(&part), vec!["n0".to_owned(), "n1".to_owned()]);
    assert_eq!(
        readable_part(n0.bytes(), &frames(n0.bytes()).expect("framed")),
        n0.bytes()
    );
}
