// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Allocation peak while real authorization results remain retained.

use aterm_secrets::{
    age::Identity,
    commons::binding::Request,
    membership_log::{
        self as log, Body, Draft, Entrant, Evidence, Inserted, Log, Record, RecordId,
    },
};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
    static ACTIVE: Cell<bool> = const { Cell::new(false) };
    static LIVE: Cell<usize> = const { Cell::new(0) };
    static PEAK: Cell<usize> = const { Cell::new(0) };
}

struct Counting;

// SAFETY: unchanged layouts and pointers are forwarded to System. Constant TLS counters
// allocate nothing. The default realloc counts the old and new blocks during growth.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarding the caller's valid layout unchanged.
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            let _ = ACTIVE.try_with(|active| {
                if active.get() {
                    let _ = LIVE.try_with(|live| {
                        let next = live.get() + layout.size();
                        live.set(next);
                        let _ = PEAK.try_with(|peak| peak.set(peak.get().max(next)));
                    });
                }
            });
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        let _ = ACTIVE.try_with(|active| {
            if active.get() {
                let _ = LIVE.try_with(|live| live.set(live.get() - layout.size()));
            }
        });
        // SAFETY: forwarding the caller's allocated pointer and matching layout unchanged.
        unsafe { System.dealloc(pointer, layout) };
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

struct Measuring;

impl Drop for Measuring {
    fn drop(&mut self) {
        ACTIVE.with(|active| active.set(false));
    }
}

fn peak_in(run: &mut dyn FnMut() -> usize) -> (usize, usize) {
    LIVE.with(|live| live.set(0));
    PEAK.with(|peak| peak.set(0));
    ACTIVE.with(|active| assert!(!active.replace(true)));
    let measuring = Measuring;
    let checksum = run();
    drop(measuring);
    assert_eq!(LIVE.with(Cell::get), 0, "all measured owners were released");
    (checksum, PEAK.with(Cell::get))
}

struct Peer {
    seed: [u8; 32],
    incarnation: [u8; 16],
    entrant: Entrant,
}

impl Peer {
    fn new(tag: u8) -> Self {
        let seed = [tag; 32];
        let incarnation = [tag; 16];
        let request = Request::create(
            [2; 16],
            [3; 32],
            [tag; 16],
            incarnation,
            &Identity::from_secret_bytes([tag ^ 0x55; 32]),
            &seed,
        )
        .unwrap();
        Self {
            seed,
            incarnation,
            entrant: Entrant {
                request,
                node_key: [tag; 32],
                name: format!("mac-{tag}"),
                label: format!("m{tag}"),
            },
        }
    }

    fn append(&self, log: &mut Log, body: Body) -> RecordId {
        let actor = self.entrant.member();
        let previous = log
            .records()
            .filter(|record| record.kind() != log::Kind::Genesis && *record.actor() == actor)
            .max_by(|a, b| a.sequence().cmp(&b.sequence()).then(b.id().cmp(a.id())));
        let sequence = previous.map_or(1, |record| record.sequence() + 1);
        let previous = previous.map(|record| *record.id());
        let mut parents: Vec<_> = previous.into_iter().collect();
        parents.extend(log.heads().into_iter().filter(|id| Some(*id) != previous));
        // Each append follows every current head, which already covers this actor's seat
        // and the body reference. No membership evaluation belongs in fixture construction.
        assert!(parents.len() <= log::MAX_PARENTS);
        let record = Draft {
            network: *log.network(),
            sequence,
            previous,
            parents,
            authored_ms: 7,
            body,
        }
        .sign(&self.seed, self.incarnation)
        .unwrap();
        let id = *record.id();
        assert_eq!(log.insert(record).unwrap(), Inserted::Held);
        id
    }
}

const RECORDS: usize = 1029;
const STATES: usize = 16;

fn distinct_states() -> Log {
    let [a, b, writer, entrant] = [1, 2, 7, 8].map(Peer::new);
    let mut log = Log::new(
        Record::found(
            &[99; 32],
            vec![a.entrant.clone(), b.entrant.clone(), writer.entrant.clone()],
            1,
        )
        .unwrap(),
    )
    .unwrap();
    let mut cutoffs = Vec::with_capacity(STATES);
    let mut entered = None;
    for i in 0..STATES {
        let body = if i == 0 {
            Body::Name {
                name: "writer".into(),
            }
        } else if let Some(cutoff) = entered.take() {
            Body::Remove {
                member: entrant.entrant.member(),
                cutoff,
            }
        } else {
            Body::Admit {
                entrant: entrant.entrant.clone(),
                evidence: Evidence::Phrase,
            }
        };
        let is_admit = matches!(&body, Body::Admit { .. });
        let cutoff = writer.append(&mut log, body);
        if is_admit {
            entered = Some(cutoff);
        }
        cutoffs.push(cutoff);
    }
    while log.held_count() < RECORDS - 2 * STATES {
        writer.append(
            &mut log,
            Body::Name {
                name: "writer".into(),
            },
        );
    }
    for cutoff in cutoffs {
        for signer in [&a, &b] {
            signer.append(
                &mut log,
                Body::Remove {
                    member: writer.entrant.member(),
                    cutoff,
                },
            );
        }
    }
    assert_eq!(log.held_count(), RECORDS);
    assert_eq!(log.waiting(), 0);
    let membership = log.membership();
    assert_eq!(membership.quorums().len(), STATES);
    assert_eq!(membership.members().len(), 2);
    log
}

#[test]
fn valid_authorization_states_keep_one_payload_per_result() {
    let log = distinct_states();
    let mut measured = None;
    log::with_authorization_allocation_probe(&log, STATES, |run| {
        measured = Some(peak_in(run));
    });
    let (checksum, peak) = measured.unwrap();
    assert!(
        checksum >= STATES,
        "each valid state authorizes its genesis"
    );
    // One result payload and key per state, each over the weighed records alone (the genesis,
    // the `Admit`s and the `Remove`s), plus one transient payload during conversion. The
    // per-state slack covers tree nodes, shared-owner metadata and result handles; the fixed
    // slack covers container growth. Payloads over every held record, as the full pass kept
    // (STATES * RECORDS bytes), exceed this budget even without metadata.
    let weighed = log
        .records()
        .filter(|record| {
            matches!(
                record.kind(),
                log::Kind::Genesis | log::Kind::Admit | log::Kind::Remove
            )
        })
        .count();
    assert_eq!(weighed, 1 + STATES - 1 + 2 * STATES);
    let key_bytes = weighed.div_ceil(64) * size_of::<u64>();
    let bound = (STATES + 1) * weighed + STATES * (key_bytes + 512) + 2048;
    assert!(STATES * RECORDS > bound);
    assert!(
        peak <= bound,
        "authorization peak {peak} exceeds {bound} bytes"
    );
    println!(
        "authorization states={STATES} records={RECORDS} weighed={weighed} peak={peak} bound={bound}"
    );
}
