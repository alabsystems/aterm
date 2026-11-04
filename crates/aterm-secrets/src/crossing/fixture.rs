// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What the crossing tests serve and read: a vault whose log sits at the legacy bound with a
//! removal batch past it, and a 0.148-0.152 member's view of what it reads ([`Legacy`]).
//!
//! The vault's founders are F, A, B and C. Its base log ([`Batch::base`]) is the genesis, an
//! `[everyone]` epoch wrapped to all four, and F's `Name` records up to [`BASE`] records: three
//! short of [`LEGACY_RECORDS`]. Its removal batch ([`Batch::removal`]) is F's chain `Remove C`,
//! a new `[everyone]` epoch, and its wraps to F, B and A (A's last), so the whole log holds
//! [`LEGACY_RECORDS`] + 2 records, and any store at the legacy bound that holds the base takes
//! the batch only up to F's wrap: cut mid-Wraps. The whole log is built at a raised held bound
//! ([`RecordBudgets::within`]), past [`LEGACY_RECORDS`]; each test says what bound it reads it
//! at.

use crate::age::{Identity, Recipient};
use crate::commons::binding::Request;
use crate::membership_log::{
    Body, Entrant, EpochAudience, LEGACY_RECORDS, Log, MemberId, NodeStanding, Record,
    RecordBudgets, Store, decode_records,
};
use std::collections::BTreeSet;
use std::sync::OnceLock;

/// The vault the fixture's records are of.
pub(crate) const VAULT: &str = "team";

/// How many records the base log holds: three short of [`LEGACY_RECORDS`], so a store at the
/// legacy bound that holds it takes three records of the removal batch and no more.
pub(crate) const BASE: usize = LEGACY_RECORDS - 3;

/// A held bound past [`LEGACY_RECORDS`] that holds the whole fixture: the raised bound a test
/// builds a crossed log at.
pub(crate) const RAISED: RecordBudgets = RecordBudgets {
    held: 2 * LEGACY_RECORDS,
    ..RecordBudgets::PRODUCTION
};

/// The bound a 0.146-0.152 build holds its log within.
pub(crate) const LEGACY: RecordBudgets = RecordBudgets {
    held: LEGACY_RECORDS,
    ..RecordBudgets::PRODUCTION
};

/// One founder: its recipient and fleet node key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Founder {
    /// Its age recipient.
    pub(crate) recipient: Recipient,
    /// Its fleet node key.
    pub(crate) node: [u8; 32],
}

/// The vault: its base log, its removal batch, and its founders.
#[derive(Debug)]
pub(crate) struct Batch {
    /// The genesis, the first `[everyone]` epoch and its wraps, and F's names: [`BASE`] records,
    /// each after its parents.
    pub(crate) base: Vec<Record>,
    /// F's chain past the base: `Remove C`, the second `[everyone]` epoch, and its wraps to F, B
    /// and A, in that order.
    pub(crate) removal: Vec<Record>,
    /// F, who removes C.
    pub(crate) f: Founder,
    /// A, whose wrap is the batch's last record.
    pub(crate) a: Founder,
    /// B, wrapped before A.
    pub(crate) b: Founder,
    /// C, whom the batch removes.
    pub(crate) c: Founder,
}

impl Batch {
    /// The vault, built once per test process (its 4,000 records take a while to sign).
    pub(crate) fn get() -> &'static Batch {
        static BATCH: OnceLock<Batch> = OnceLock::new();
        BATCH.get_or_init(|| RAISED.within(build))
    }

    /// The whole log: the base, then the removal batch.
    pub(crate) fn whole(&self) -> Vec<Record> {
        self.base.iter().chain(&self.removal).cloned().collect()
    }

    /// The node keys a member that reads the whole log puts out: C's alone.
    pub(crate) fn removed(&self) -> BTreeSet<[u8; 32]> {
        BTreeSet::from([self.c.node])
    }

    /// The node keys a member that reads the batch cut mid-Wraps puts out: C's, and A's and B's,
    /// whom its newest `[everyone]` epoch then leaves out.
    pub(crate) fn cut_out(&self) -> BTreeSet<[u8; 32]> {
        BTreeSet::from([self.a.node, self.b.node, self.c.node])
    }
}

/// A founder `tag` names: its writer seed, incarnation, member id, recipient and entrant.
fn founder(tag: u8, label: &str) -> ([u8; 32], [u8; 16], MemberId, Founder, Entrant) {
    let writer = [tag; 32];
    let machine = [tag.wrapping_add(0x20); 16];
    let incarnation = [tag; 16];
    let identity = Identity::from_secret_bytes([tag.wrapping_add(0x40); 32]);
    let request = Request::create(
        [0x31; 16],
        [0x42; 32],
        machine,
        incarnation,
        &identity,
        &writer,
    )
    .expect("request");
    let member = request.binding().actor();
    let node = [tag.wrapping_add(0x80); 32];
    let entrant = Entrant {
        request,
        node_key: node,
        name: format!("mac-{tag}"),
        label: label.to_owned(),
    };
    let founder = Founder {
        recipient: identity.recipient(),
        node,
    };
    (writer, incarnation, member, founder, entrant)
}

/// Hold `record` in `log`, and keep it in `into`.
fn hold(log: &mut Log, record: Record, into: &mut Vec<Record>) {
    let _ = log.insert(record.clone()).expect("held");
    into.push(record);
}

fn build() -> Batch {
    let (wf, incf, mf, f, ef) = founder(0x61, "m1");
    let (_, _, _, a, ea) = founder(0x62, "m2");
    let (_, _, _, b, eb) = founder(0x63, "m3");
    let (_, _, mc, c, ec) = founder(0x64, "m4");
    let genesis = Record::found(&[7; 32], vec![ef, ea, eb, ec], 1_000).expect("genesis");
    let mut log = Log::new(genesis.clone()).expect("log");
    let mut base = vec![genesis];
    let mut at = 1_001;
    let (draft, key) = log
        .draft_epoch(&mf, at, EpochAudience::Everyone, 1)
        .expect("first epoch");
    let epoch = draft.sign(&wf, incf).expect("signed");
    let first = *epoch.id();
    hold(&mut log, epoch, &mut base);
    for to in [f, a, b, c] {
        let wrap = log
            .draft_wrap(&mf, at, first, &key, to.recipient)
            .expect("wrap")
            .sign(&wf, incf)
            .expect("signed");
        hold(&mut log, wrap, &mut base);
    }
    while base.len() < BASE {
        at += 1;
        let name = log
            .draft(
                &mf,
                at,
                Body::Name {
                    name: format!("filler-{at}"),
                },
            )
            .sign(&wf, incf)
            .expect("signed");
        hold(&mut log, name, &mut base);
    }
    let mut removal = Vec::new();
    let remove = log
        .draft(
            &mf,
            at + 1,
            Body::Remove {
                member: mc,
                cutoff: *log.network(),
            },
        )
        .sign(&wf, incf)
        .expect("signed");
    hold(&mut log, remove, &mut removal);
    let (draft, key) = log
        .draft_epoch(&mf, at + 2, EpochAudience::Everyone, 2)
        .expect("second epoch");
    let epoch = draft.sign(&wf, incf).expect("signed");
    let second = *epoch.id();
    hold(&mut log, epoch, &mut removal);
    for to in [f, b, a] {
        let wrap = log
            .draft_wrap(&mf, at + 3, second, &key, to.recipient)
            .expect("wrap")
            .sign(&wf, incf)
            .expect("signed");
        hold(&mut log, wrap, &mut removal);
    }
    assert_eq!(base.len(), BASE);
    assert_eq!(removal.len(), 5);
    Batch {
        base,
        removal,
        f,
        a,
        b,
        c,
    }
}

/// A 0.148-0.152 member: a store at the legacy bound ([`LEGACY`]), which takes a records stream
/// whole only where it holds at most [`LEGACY_RECORDS`] records, inserting what fits, and reads
/// standing as those builds do (aterm-agent `fleet_log::log_member_keys`, and a Mac's own
/// `Revoked` where its log removes it or its newest `[everyone]` epoch leaves it out).
pub(crate) struct Legacy {
    store: Store,
}

impl Legacy {
    /// A member that holds `records`.
    pub(crate) fn holding(records: &[Record]) -> Self {
        Self::at(LEGACY, records)
    }

    /// A member whose store holds its log within `budgets`, holding `records`: the standing a
    /// member of another bound reads, by the same rules.
    pub(crate) fn at(budgets: RecordBudgets, records: &[Record]) -> Self {
        budgets.within(|| {
            let mut store = Store::for_vault(VAULT).expect("store");
            if !records.is_empty() {
                let _ = store.ingest_records(records.to_vec()).expect("held");
            }
            Self { store }
        })
    }

    /// Read `stream`, as a records answer or POST: whether it took it. A stream past
    /// [`LEGACY_RECORDS`] records is refused whole, and a store at its bound takes the records
    /// that fit, each after its parents.
    pub(crate) fn read(&mut self, stream: &[u8]) -> bool {
        LEGACY.within(|| {
            let Ok(records) = decode_records(stream) else {
                return false;
            };
            if records.len() > LEGACY_RECORDS {
                return false;
            }
            self.store.ingest_records(records).is_ok()
        })
    }

    /// How many records its log holds.
    pub(crate) fn held(&self) -> usize {
        self.store.log().map_or(0, Log::held_count)
    }

    /// Whether the Mac whose recipient is `me` reads itself `Revoked`.
    pub(crate) fn revoked(&self, me: &Recipient) -> bool {
        let log = self.store.log().expect("a rooted log");
        log.recipient_is_removed(me)
            || log.audience_left_out_recipient(&EpochAudience::Everyone, me)
    }

    /// The node keys it puts `Standing::Out`, as `fleet_log::log_member_keys` reads a log with no
    /// roster.
    pub(crate) fn out(&self) -> BTreeSet<[u8; 32]> {
        let log = self.store.log().expect("a rooted log");
        let membership = log.membership();
        let everyone = EpochAudience::Everyone;
        let mut out: BTreeSet<[u8; 32]> = membership
            .nodes()
            .iter()
            .filter(|(_, standing)| match standing {
                NodeStanding::Member(actor) => {
                    log.audience_left_out(&everyone, actor)
                        || membership.get(actor).is_some_and(|m| {
                            let recipient = m.binding.recipient();
                            log.recipient_is_removed(&recipient)
                                || log.audience_left_out_recipient(&everyone, &recipient)
                        })
                }
                NodeStanding::Removed(_) | NodeStanding::Retired(_) | NodeStanding::Contested => {
                    true
                }
                NodeStanding::Unknown => false,
            })
            .map(|(key, _)| *key)
            .collect();
        if let Body::Genesis { founders, .. } = log.genesis().body() {
            for founder in founders {
                if let Some(node) = founder.node_key
                    && (log.recipient_is_removed(&founder.recipient)
                        || log.audience_left_out_recipient(&everyone, &founder.recipient))
                {
                    out.insert(node);
                }
            }
        }
        out
    }
}
