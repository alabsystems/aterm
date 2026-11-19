// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The format-2 log's own pass (tracker FL-148F): a full log still checkpoints, a superseded
//! body is shed with no vote, a prune waits for more than half of the seats its log holds, two
//! Macs that prune at one cut hold the same bytes, and a Mac that missed the cut takes the root
//! a pruned one serves.

use super::*;
use crate::crossing::FORMAT_TWO;
use crate::membership_log::budgets::{Author, Authoring};
use crate::membership_log::checkpoint::tests::{A, B, C, Mac, epoch, found, hold, one};
use crate::membership_log::checkpoint::{DAY_MS, propose};
use crate::membership_log::rooted::tests::{entry, evaluated, store_of};
use crate::membership_log::served::tests::holding;
use crate::membership_log::store::{REPLY_CAP, read_reply};
use crate::membership_log::{
    Kind, MAX_HELD_RECORDS, Reading, RecordBudgets, VERSION_TWO, decode_reply,
};
use crate::text::Audience;
use zeroize::Zeroizing;

/// The clock every share of these tests is signed at: twenty days in, so the cut is day five
/// and every write of the tests (day one) lies before it.
const NOW: u64 = 20 * DAY_MS;

/// A log at format 2 whose seats are `macs`, an `[everyone]` epoch A minted wrapped to each.
struct Vault {
    log: Log,
    epoch: (RecordId, Zeroizing<[u8; 32]>),
    clock: u64,
}

impl Vault {
    fn new(macs: &[Mac]) -> Self {
        let mut log = found(macs);
        let epoch = epoch(&mut log, A, DAY_MS, EpochAudience::Everyone, 1, macs);
        log.stored_at = Reading::Format(FORMAT_TWO);
        Self {
            log,
            epoch,
            clock: DAY_MS + 1,
        }
    }

    /// `by`'s version-2 `Put` of `value` to `name`, as a writer of format 2 signs it, unheld.
    fn put(&mut self, by: Mac, name: &str, value: &[u8]) -> Record {
        self.clock += 1;
        self.put_at(by, self.clock, name, value)
    }

    /// [`Vault::put`], authored at `at`.
    fn put_at(&self, by: Mac, at: u64, name: &str, value: &[u8]) -> Record {
        let mut log = self.log.clone();
        log.write_two(&by.identity());
        let draft = log
            .draft_put(
                &by.id(),
                at,
                self.epoch.0,
                &self.epoch.1,
                &entry(&EpochAudience::Everyone, name, value),
            )
            .unwrap();
        let record = one(draft, by);
        assert_eq!(record.version(), VERSION_TWO);
        record
    }

    /// `by`'s version-1 `Put` of `value` to `name`, as a build before format 2 signed it,
    /// unheld.
    fn put_one(&mut self, by: Mac, name: &str, value: &[u8]) -> Record {
        self.clock += 1;
        let mut log = self.log.clone();
        log.stored_at = Reading::ONE;
        let draft = log
            .draft_put(
                &by.id(),
                self.clock,
                self.epoch.0,
                &self.epoch.1,
                &entry(&EpochAudience::Everyone, name, value),
            )
            .unwrap();
        let record = one(draft, by);
        assert_eq!(record.version(), crate::membership_log::VERSION);
        record
    }

    /// Whether a person's batch of `record` is asked through.
    fn person_asks(&self, record: &Record, by: Mac) -> Result<()> {
        let batch = Authoring::all(std::slice::from_ref(record), Class::Person);
        self.log.author_ask(&batch, Author::Person, &by.id(), 0)
    }

    /// `by` shares what its log proposes at [`NOW`], as a seat's pass does: the share is asked
    /// as membership and held. `None` where it shares nothing.
    fn share(&mut self, by: Mac) -> Option<Record> {
        self.share_at(by, NOW)
    }

    /// [`Vault::share`] at `now`.
    fn share_at(&mut self, by: Mac, now: u64) -> Option<Record> {
        let draft = self.log.share_draft(&by.id(), now).unwrap()?;
        let share = one(draft, by);
        let batch = Authoring::all(std::slice::from_ref(&share), Class::Membership);
        self.log
            .author_ask(&batch, Author::Person, &by.id(), now)
            .expect("a share is asked through");
        Some(hold(&mut self.log, share))
    }

    /// This vault's keys over `log`.
    fn over(&self, log: Log) -> Self {
        Self {
            log,
            epoch: self.epoch.clone(),
            clock: self.clock,
        }
    }

    /// The `[everyone]` entries `who` reads, by name, with the op that wins each.
    fn read(&self, who: Mac) -> Vec<(String, Vec<u8>, Option<RecordId>)> {
        let opened = self
            .log
            .open_audience_entries(&who.identity())
            .unwrap()
            .unwrap();
        opened
            .by_audience
            .get(&Audience::Everyone)
            .into_iter()
            .flatten()
            .map(|(name, entry)| {
                (
                    name.clone(),
                    entry.value.as_slice().to_vec(),
                    opened
                        .winning_ops
                        .get(&(Audience::Everyone, name.clone()))
                        .copied(),
                )
            })
            .collect()
    }
}

/// A log filled with version-2 `Put`s past a person's byte stop and to its keyed-count stop
/// still checkpoints: a person's write is refused, each seat's share is still asked through
/// and held (the keyed stop's room is kept for it, and membership runs to the byte bound), A's
/// share makes the checkpoint final (A is the one seat that wrote in the cut, which drops only
/// version-2 records), B joins it, the prune drops every `Put` A's own later `Put` replaced,
/// and the person's write then lands.
/// NEGATIVE CONTROL (observed): with a share counted against the keyed stop (the
/// `record.kind() != Kind::Checkpoint` test cut from `Authoring::classed` in `budgets.rs`), A's
/// share is refused and `share` panics with "a share is asked through".
#[test]
fn vault_checkpoint_a_full_log_still_checkpoints() {
    let budgets = RecordBudgets {
        weighed: 16,
        keyed: 16,
        held: 32,
    };
    budgets.within(|| {
        let mut vault = Vault::new(&[A, B, C]);
        // The epoch and its three wraps, then A's Puts of new values up to the room the shares
        // leave.
        let mut written = Vec::new();
        loop {
            let value = vec![u8::try_from(written.len()).unwrap(); 2000];
            let record = vault.put(A, "k", &value);
            if vault.person_asks(&record, A).is_err() {
                break;
            }
            written.push(*hold(&mut vault.log, record).id());
        }
        assert_eq!(vault.log.counts.keyed, 16 - 3, "at the keyed-count stop");
        // A byte bound the log is past a person's stop of, with room left for membership.
        let held = vault.log.total_bytes();
        let bound = held * 17 / 16;
        RecordBudgets::within_bytes(bound, || {
            assert!(
                Class::Person.stop_within(bound) < held,
                "past a person's stop"
            );
            let early = vault.put(A, "k", b"late");
            let refused = vault
                .person_asks(&early, A)
                .expect_err("a person's write is refused");
            assert_eq!(refused.kind(), crate::ErrorKind::MembershipCapacity);
            assert!(vault.log.wants_room());
            let first = vault.share(A).expect("A shares");
            assert_eq!(first.version(), VERSION_TWO);
            assert!(vault.share(A).is_none(), "A shares a body once");
            vault.share(B).expect("B joins A's body");
            let body = vault
                .log
                .to_prune()
                .expect("final by the cut's one recent seat");
            assert_eq!(checkpoint_id(&body), checkpoint_id(first.body()));
            let pruned = vault.log.adopt(&body).expect("the prune");
            for id in &written[..written.len() - 1] {
                assert!(pruned.is_dropped(id), "a replaced Put is dropped");
            }
            assert!(pruned.get(written.last().unwrap()).is_some(), "the winner");
            vault.log = pruned;
            let late = vault.put(A, "k", b"late");
            vault
                .person_asks(&late, A)
                .expect("the person's write fits after the prune");
            hold(&mut vault.log, late.clone());
            assert_eq!(
                vault.read(A),
                vec![("k".to_owned(), b"late".to_vec(), Some(*late.id()))]
            );
        });
    });
}

/// A version-2 `Put` its own signer's later `Put` replaced is shed with no checkpoint and no
/// share: the store keeps its frame under the same id, which still verifies, and holds fewer
/// bytes; every seat reads every entry as a log that holds the record whole does, the heads
/// are the same, and a checkpoint computed over either log is the same body. B's op, which no
/// op of B's replaced, and the winner stay whole. NEGATIVE CONTROL (observed): with
/// `Log::op_key` opening a shed op as any other (its `is_shed` return cut in `entries.rs`), the
/// read of the shed log fails with "decrypting membership log entry failed".
#[test]
fn vault_checkpoint_a_superseded_v2_body_is_shed_without_a_vote() {
    let mut vault = Vault::new(&[A, B, C]);
    // A body worth a rewrite: an eighth of the log's bytes ([`Log::shed_worth`]).
    let a1 = vault.put(A, "k", &[1; 4000]);
    hold(&mut vault.log, a1.clone());
    let b = vault.put(B, "other", b"b");
    hold(&mut vault.log, b.clone());
    let a2 = vault.put(A, "k", b"two");
    hold(&mut vault.log, a2.clone());
    let store = store_of(vault.log.clone()).read_at(Reading::Format(FORMAT_TWO));
    let (shed, ids) = store
        .shed_superseded()
        .unwrap()
        .expect("A's first Put is shed");
    assert_eq!(ids, vec![*a1.id()]);
    let log = shed.log().unwrap();
    let frame = log.get(a1.id()).expect("its frame is held");
    assert!(frame.is_shed() && *frame.id() == *a1.id());
    assert!(
        Record::decode_in(&frame.wire_bytes().unwrap(), Reading::Format(FORMAT_TWO)).is_ok(),
        "the frame verifies"
    );
    for kept in [&b, &a2] {
        assert!(!log.get(kept.id()).unwrap().is_shed());
    }
    assert!(log.held_bytes() < vault.log.held_bytes());
    assert!(
        log.records()
            .all(|record| record.kind() != Kind::Checkpoint),
        "no vote"
    );
    assert_eq!(log.heads_digest(), vault.log.heads_digest());
    let whole = Vault {
        log: log.clone(),
        epoch: vault.epoch.clone(),
        clock: vault.clock,
    };
    for who in [A, B, C] {
        assert_eq!(whole.read(who), vault.read(who));
    }
    assert_eq!(
        propose(log, NOW).and_then(|plan| checkpoint_id(&plan.body)),
        propose(&vault.log, NOW).and_then(|plan| checkpoint_id(&plan.body)),
    );
    assert!(shed.shed_superseded().unwrap().is_none(), "shed once");
}

/// A prune that drops a version-1 record waits for more than half of the seats its log holds,
/// not only of the cut's recent seats: in a vault of four seats of which only A wrote before
/// the cut, its first write version 1, A's and B's shares make the checkpoint final and prune
/// nothing, and C's third share prunes. A store that reads format 1 prunes nothing whatever it
/// holds. NEGATIVE CONTROL (observed): with `to_prune` waiting for the recent seats alone (its
/// `finality::is_final(&seats, ..)` test cut in `pass.rs`), the log prunes on A's and B's
/// shares and `assert!(vault.log.to_prune().is_none(), ..)` fails with "two shares of four
/// seats prune nothing".
#[test]
fn vault_checkpoint_a_prune_waits_for_more_than_half_of_the_seats_its_log_holds() {
    let d = Mac(4);
    let budgets = RecordBudgets {
        weighed: 16,
        keyed: 16,
        held: 32,
    };
    budgets.within(|| {
        let mut vault = Vault::new(&[A, B, C, d]);
        // A's first write is version 1, as a build before format 2 signed it: A's next write,
        // version 2, restates it, and the prune drops it to a stub that does not check itself.
        let a1 = vault.put_one(A, "k", b"1");
        hold(&mut vault.log, a1.clone());
        while !vault.log.wants_room() {
            let record = vault.put(A, "k", b"2");
            hold(&mut vault.log, record);
        }
        vault.share(A).expect("A shares");
        vault.share(B).expect("B joins");
        let body = vault.log.newest_share().expect("a body");
        let judged = checkpoint::judge(&vault.log, &body).unwrap();
        assert!(judged.is_final, "final by the recent seats");
        assert!(
            vault.log.to_prune().is_none(),
            "two shares of four seats prune nothing"
        );
        let format_one = store_of(vault.log.clone()).read_at(Reading::ONE);
        vault.share(C).expect("C joins");
        assert_eq!(
            vault.log.to_prune().and_then(|next| checkpoint_id(&next)),
            checkpoint_id(&body)
        );
        let store = store_of(vault.log.clone()).read_at(Reading::Format(FORMAT_TWO));
        let (pruned, root) = store.adopt_final().unwrap().expect("a prune");
        assert_eq!(Some(root), checkpoint_id(&body));
        assert!(pruned.log().unwrap().is_dropped(a1.id()));
        assert!(format_one.adopt_final().unwrap().is_none());
    });
}

impl Log {
    /// The body of the newest share this log holds.
    fn newest_share(&self) -> Option<Body> {
        self.records()
            .filter(|record| record.kind() == Kind::Checkpoint)
            .last()
            .map(|record| record.body().clone())
    }
}

/// A format-2 vault whose pass both prunes and sheds ([`passing_vault`]).
pub(crate) struct PassingVault {
    /// Its records, the genesis first: A's `Put`s of `k` on day one, its two after the cut, the
    /// other seats' `Put`s, and A's and B's shares of the checkpoint at [`NOW`].
    pub(crate) records: Vec<Record>,
    /// Its store, reading format 2.
    pub(crate) whole: Store,
    /// The store a pass makes of it: pruned at the shares' checkpoint, then shed.
    pub(crate) passed: Store,
    /// The checkpoint the pass prunes at.
    pub(crate) root: RecordId,
    /// Its records but the shares: nothing in it is final.
    pub(crate) unshared: Vec<Record>,
    /// The store a pass makes of `unshared`: shed alone.
    pub(crate) shed_alone: Store,
    /// The clock a pass of it runs at.
    pub(crate) now_ms: u64,
}

/// Seats A, B and C at format 2. A sets `k` three times on day one and twice more on day
/// nineteen, past the cut; B and C each set a name of their own on day one; A and B share the
/// checkpoint their log proposes at [`NOW`], each before it holds the other's share, and A's
/// share alone makes it final (A is the one seat that wrote in the cut, which drops only
/// version-2 records).
/// A pass drops A's first two `Put`s and sheds the third (replaced past the cut) and the
/// fourth.
pub(crate) fn passing_vault() -> PassingVault {
    let mut vault = Vault::new(&[A, B, C]);
    let mut puts = Vec::new();
    // Bodies worth the shed's rewrite ([`Log::shed_worth`]).
    for value in [b'1', b'2', b'3'] {
        let put = vault.put(A, "k", &[value; 3000]);
        puts.push(*hold(&mut vault.log, put).id());
    }
    for by in [B, C] {
        let put = vault.put(by, &format!("of-{}", by.0), b"x");
        hold(&mut vault.log, put);
    }
    for (day, value) in [(19, b'4'), (19, b'5')] {
        vault.clock = day * DAY_MS + puts.len() as u64;
        let put = vault.put_at(A, vault.clock, "k", &[value; 3000]);
        puts.push(*hold(&mut vault.log, put).id());
    }
    let unshared: Vec<Record> = vault.log.records().cloned().collect();
    let (shed_alone, _) = store_of(vault.log.clone())
        .read_at(Reading::Format(FORMAT_TWO))
        .shed_superseded()
        .unwrap()
        .expect("a shed alone");
    // The two shares are concurrent: each follows the log before either.
    let body = propose(&vault.log, NOW).expect("a proposal").body;
    let shares: Vec<Record> = [A, B]
        .into_iter()
        .map(|by| one(vault.log.draft(&by.id(), NOW, body.clone()), by))
        .collect();
    for share in shares {
        hold(&mut vault.log, share);
    }
    let whole = store_of(vault.log.clone()).read_at(Reading::Format(FORMAT_TWO));
    let (pruned, root) = whole.adopt_final().unwrap().expect("a prune");
    let (passed, shed) = pruned.shed_superseded().unwrap().expect("a shed");

    assert!(pruned.log().unwrap().is_dropped(&puts[0]));
    assert!(pruned.log().unwrap().is_dropped(&puts[1]));
    let mut want = vec![puts[2], puts[3]];
    want.sort_unstable();
    assert_eq!(shed, want);
    PassingVault {
        records: vault.log.records().cloned().collect(),
        whole,
        passed,
        root,
        unshared,
        shed_alone,
        now_ms: NOW,
    }
}

/// Each record `store` holds by its bytes, sorted, beside its frame of roots and stubs and its
/// heads: the bytes of its membership whatever order it took the records in.
fn bytes_of(store: &Store) -> (Vec<Vec<u8>>, Option<Vec<u8>>, Option<[u8; 32]>) {
    let mut records: Vec<Vec<u8>> = store
        .log()
        .unwrap()
        .records()
        .map(|record| record.wire_bytes().unwrap().to_vec())
        .collect();
    records.sort();
    (records, store.kept_frame().unwrap(), store.heads_digest())
}

/// Two Macs that prune at the same cut end with the same bytes of membership: A holds B's
/// share first (no recent seat's) and prunes nothing on it, B sheds A's replaced `Put`s before
/// either share comes, and each prunes once both shares are held; their records, byte for byte, their frames
/// of roots and stubs, their heads and what every seat reads of them are the same, and the
/// same as the pass makes of the whole vault. A stub of a version-2 record keeps its shed
/// frame, whichever form the Mac held. NEGATIVE CONTROL (observed): with a stub keeping its
/// record's bytes rather than its shed frame (`Stub::of`'s `shed_frame()` replaced by
/// `wire_bytes()` in `rooted.rs`), a Mac's prune no longer nests its root, and
/// `expect("a prune")` panics.
#[test]
fn vault_checkpoint_two_macs_that_prune_at_the_same_cut_hold_the_same_bytes() {
    let world = passing_vault();
    let genesis = &world.records[0];
    let mut a = Log::new(genesis.clone()).unwrap();
    let mut b = Log::new(genesis.clone()).unwrap();
    a.stored_at = Reading::Format(FORMAT_TWO);
    b.stored_at = Reading::Format(FORMAT_TWO);
    let shares: Vec<&Record> = world
        .records
        .iter()
        .filter(|record| record.kind() == Kind::Checkpoint)
        .collect();
    for record in world.records.iter().skip(1) {
        if record.kind() != Kind::Checkpoint {
            hold(&mut a, record.clone());
            hold(&mut b, record.clone());
        }
    }
    // A holds B's share alone, of no seat that wrote in the cut, and prunes nothing; B sheds
    // before either share comes.
    hold(&mut a, shares[1].clone());
    let a_store = store_of(a.clone()).read_at(Reading::Format(FORMAT_TWO));
    assert!(
        a_store.adopt_final().unwrap().is_none(),
        "no recent seat's share"
    );
    let (b_shed, _) = store_of(b.clone())
        .read_at(Reading::Format(FORMAT_TWO))
        .shed_superseded()
        .unwrap()
        .expect("B sheds first");
    let mut b = b_shed.log().unwrap().clone();
    for share in &shares {
        hold(&mut b, (*share).clone());
    }
    hold(&mut a, shares[0].clone());
    let pass = |log: Log| {
        let store = store_of(log).read_at(Reading::Format(FORMAT_TWO));
        let (pruned, root) = store.adopt_final().unwrap().expect("a prune");
        assert_eq!(root, world.root);
        pruned
            .shed_superseded()
            .unwrap()
            .map_or(pruned, |(shed, _)| shed)
    };
    let (a, b) = (pass(a), pass(b));
    assert_eq!(bytes_of(&a), bytes_of(&b));
    assert_eq!(bytes_of(&a), bytes_of(&world.passed));
    assert_eq!(evaluated(a.log().unwrap()), evaluated(b.log().unwrap()));
}

/// A Mac that missed the cut (it holds the genesis, the epoch and its wraps alone) takes the
/// root a Mac that pruned there serves, through the reply's bytes, and then holds what that
/// Mac holds and reads the same members and entries. Asked again, it is served nothing.
/// NEGATIVE CONTROL (observed): with `Store::missing_served` serving no root (its
/// `named_cuts` test answering yes), the test fails with "the pruned Mac serves its root".
#[test]
fn vault_checkpoint_a_mac_that_missed_the_cut_takes_the_served_root() {
    let world = passing_vault();
    let mut behind = Log::new(world.records[0].clone()).unwrap();
    for record in world
        .records
        .iter()
        .filter(|record| matches!(record.kind(), Kind::Epoch | Kind::Wrap))
    {
        hold(&mut behind, record.clone());
    }
    let mut receiver = store_of(behind).read_at(Reading::Format(FORMAT_TWO));
    let peer = &world.passed;
    let missing = peer.missing_served(&receiver.have(), REPLY_CAP);
    assert!(missing.served.is_some(), "the pruned Mac serves its root");
    let bytes = read_reply(&missing).unwrap();
    let (records, served) = decode_reply(&bytes, MAX_HELD_RECORDS, Reading::Follow).unwrap();
    receiver.ingest_served(records, served.as_ref()).unwrap();
    let log = receiver.log().unwrap();
    assert_eq!(log.newest_root().map(|root| root.id), Some(world.root));
    assert_eq!(holding(log), holding(peer.log().unwrap()));
    assert_eq!(evaluated(log), evaluated(peer.log().unwrap()));
    assert_eq!(evaluated(log), evaluated(world.whole.log().unwrap()));
    let again = peer.missing_served(&receiver.have(), REPLY_CAP);
    assert!(again.records.is_empty() && again.served.is_none());
}

/// A version-1 `Put` over version-2 history reads its register by the values in its past, so
/// the shedding pass sheds nothing there and every seat reads every entry of the shed log as of
/// the whole one. No writer signs one (a log at format 2 drafts as version 2 alone); here C
/// signs one as a build that erred would: after A's `p0` writes V, C reseals V as version 1 (a
/// carry of `p0`), and A's `p1`, concurrent with it, writes W and names `p0` replaced. Sixteen
/// vaults, so the ids that order the winner vary. NEGATIVE CONTROL (observed): with
/// `Log::sheddable` shedding in a version-1 op's past (`Mutant::ShedUnderVersionOne`), `p0` is
/// shed, the reseal stands for itself, and in some of the vaults a seat reads V where the whole
/// log reads W: `assert_eq!(shed.read(who), vault.read(who))` fails.
#[test]
fn vault_checkpoint_a_version_one_carry_over_version_two_history_reads_alike_once_shed() {
    for _ in 0..16 {
        let mut vault = Vault::new(&[A, B, C]);
        let p0 = vault.put(A, "k", b"V");
        hold(&mut vault.log, p0.clone());
        let base = vault.log.clone();
        let q = vault.put_one(C, "k", b"V");
        hold(&mut vault.log, q);
        let with_q = std::mem::replace(&mut vault.log, base);
        let p1 = vault.put(A, "k", b"W");
        vault.log = with_q;
        hold(&mut vault.log, p1.clone());
        assert_eq!(p1.statement().unwrap().supersedes, vec![*p0.id()]);
        assert!(checkpoint::superseded(&vault.log).contains(p0.id()));
        let sheddable = vault.log.sheddable();
        let shed = vault.over(vault.log.with_shed(&sheddable).unwrap());
        for who in [A, B, C] {
            assert_eq!(shed.read(who), vault.read(who));
        }
        assert!(
            !sheddable.contains(p0.id()),
            "the reseal's past stays whole"
        );
    }
}

/// A carry's rep keeps its body: A's reseal `t1` of `t0`'s value carries `t0`, which A's own
/// statement names replaced, and the shedding pass leaves `t0` whole, so the `Put` its draft
/// wrote reads as it did (what the Claude token's settle reads of a pair's meta,
/// `Log::everyone_puts_of_drafts`). NEGATIVE CONTROL (observed): with `Log::sheddable` shedding
/// a carried rep (its `!carried.contains` test cut in `pass.rs`), `t0` is shed and the draft
/// reads `Unopened`: `assert!(matches!(.., DraftPut::Wrote(_)))` fails.
#[test]
fn vault_checkpoint_a_carried_rep_keeps_its_body() {
    use crate::membership_log::entries::DraftPut;
    let mut vault = Vault::new(&[A, B, C]);
    let t0 = vault.put(A, "tok", b"T0");
    hold(&mut vault.log, t0.clone());
    let t1 = vault.put(A, "tok", b"T0");
    hold(&mut vault.log, t1.clone());
    assert_eq!(t1.statement().unwrap().carries, Some(*t0.id()));
    assert!(checkpoint::superseded(&vault.log).contains(t0.id()));
    let sheddable = vault.log.sheddable();
    let shed = vault.log.with_shed(&sheddable).unwrap();
    let drafts = [(A.id(), t0.authored_ms(), vault.epoch.0)];
    for log in [&vault.log, &shed] {
        let opened = log.open_audience_entries(&B.identity()).unwrap().unwrap();
        let read = log
            .everyone_puts_of_drafts(&opened, "tok", &drafts)
            .unwrap();
        assert!(matches!(read.as_slice(), [DraftPut::Wrote(value)] if value.as_slice() == b"T0"));
    }
    assert!(!sheddable.contains(t0.id()));
}

/// The Commons audience's `Put`s are not shed: A's control `Put` replaced by A's later one is
/// superseded by its statement, and the shedding pass leaves it whole; every seat reads the same
/// control before and after the shed. NEGATIVE CONTROL (observed): with the Commons test cut
/// from `Log::sheddable` (its `*audience != EpochAudience::Commons` made `true`), the first
/// control `Put` is sheddable and `assert!(!sheddable.contains(..))` fails.
#[test]
fn vault_checkpoint_the_commons_control_is_not_shed() {
    let mut vault = Vault::new(&[A, B, C]);
    let commons = epoch(
        &mut vault.log,
        A,
        DAY_MS + 100,
        EpochAudience::Commons,
        1,
        &[A, B, C],
    );
    let mut control = |descriptor: &[u8], at: u64| {
        let mut log = vault.log.clone();
        log.write_two(&A.identity());
        let draft = log
            .draft_commons_put(&A.id(), at, commons.0, &commons.1, descriptor)
            .unwrap();
        hold(&mut vault.log, one(draft, A))
    };
    let first = control(b"ACD1-one", DAY_MS + 200);
    let second = control(b"ACD1-two", DAY_MS + 300);
    assert_eq!(second.version(), VERSION_TWO);
    assert!(checkpoint::superseded(&vault.log).contains(first.id()));
    let sheddable = vault.log.sheddable();
    assert!(
        !sheddable.contains(first.id()),
        "a Commons Put is kept whole"
    );
    let shed = vault.log.with_shed(&sheddable).unwrap();
    for who in [A, B, C] {
        let read = |log: &Log| log.open_commons_control(&who.identity()).unwrap();
        assert_eq!(read(&shed), read(&vault.log));
        assert_eq!(
            read(&vault.log)
                .as_ref()
                .map(|value| value.as_slice().to_vec()),
            Some(b"ACD1-two".to_vec())
        );
    }
}

/// A shed rewrites the records file, so the pass sheds only bodies worth it: a log that does
/// not need the room sheds a replaced body that frees less than an eighth of its bytes not at
/// all, sheds once the replaced bodies free that much, and sheds the small one too once the
/// log needs the room. NEGATIVE CONTROL (observed): with `Log::shed_worth` answering `true`
/// whatever the bodies free, the first shed is taken and `assert!(.. .is_none(), "a small body
/// waits")` fails.
#[test]
fn vault_checkpoint_a_shed_waits_for_bodies_worth_a_rewrite() {
    let mut vault = Vault::new(&[A, B, C]);
    for value in [b"1".as_slice(), b"2"] {
        let put = vault.put(A, "small", value);
        hold(&mut vault.log, put);
    }
    let store = |vault: &Vault| store_of(vault.log.clone()).read_at(Reading::Format(FORMAT_TWO));
    assert_eq!(vault.log.sheddable().len(), 1);
    assert!(!vault.log.wants_room());
    assert!(
        store(&vault).shed_superseded().unwrap().is_none(),
        "a small body waits"
    );
    let total = vault.log.total_bytes();
    RecordBudgets::within_bytes(total, || {
        assert!(vault.log.wants_room());
        let (_, ids) = store(&vault)
            .shed_superseded()
            .unwrap()
            .expect("a full log sheds");
        assert_eq!(ids.len(), 1);
    });
    for value in [vec![3; 4000], vec![4]] {
        let put = vault.put(A, "large", &value);
        hold(&mut vault.log, put);
    }
    let (_, ids) = store(&vault)
        .shed_superseded()
        .unwrap()
        .expect("an eighth of the bytes");
    assert_eq!(ids.len(), 2);
}

/// A cut that drops only version-2 records waits for its recent seats alone, not for more than
/// half of every seat: in a vault of five seats of which only A writes, A's share makes the
/// checkpoint final and the log prunes at it with two of five seats' shares, as a Mac behind the
/// cut takes the root of self-checking stubs ([`super::served`]). NEGATIVE CONTROL (observed):
/// with every prune waiting for more than half of the seats (`Mutant::AllSeatsAlways`),
/// `to_prune` answers `None` and `expect("final by its recent seat")` panics.
#[test]
fn vault_checkpoint_a_version_two_cut_prunes_without_its_dormant_seats() {
    let (d, e) = (Mac(4), Mac(5));
    let budgets = RecordBudgets {
        weighed: 16,
        keyed: 16,
        held: 32,
    };
    budgets.within(|| {
        let mut vault = Vault::new(&[A, B, C, d, e]);
        let a1 = vault.put(A, "k", b"1");
        hold(&mut vault.log, a1.clone());
        while !vault.log.wants_room() {
            let record = vault.put(A, "k", b"2");
            hold(&mut vault.log, record);
        }
        vault.share(A).expect("A shares");
        vault.share(B).expect("B joins");
        let body = vault.log.to_prune().expect("final by its recent seat");
        let pruned = vault.log.adopt(&body).unwrap();
        assert!(pruned.is_dropped(a1.id()));
        assert!(pruned.served().is_some(), "it serves its root");
    });
}

/// A seat whose share still waits on a body no root holds proposes no other: it joins a body
/// another seat shared, and proposes again only once [`finality::HEARD_WITHIN_MS`] passed since
/// its share, so a log whose bodies do not become final gains no share of each seat a day.
/// NEGATIVE CONTROL (observed): with a seat proposing whenever its log needs the room
/// (`Mutant::SharesDaily`), A shares the next day's body on its own and
/// `assert!(.. .is_none(), "A's share still waits")` fails.
#[test]
fn vault_checkpoint_a_seat_proposes_once_while_its_share_waits() {
    let budgets = RecordBudgets {
        weighed: 16,
        keyed: 16,
        held: 32,
    };
    budgets.within(|| {
        let mut vault = Vault::new(&[A, B, C]);
        while !vault.log.wants_room() {
            let record = vault.put(A, "k", b"2");
            hold(&mut vault.log, record);
        }
        vault.share(A).expect("A proposes");
        let next = NOW + DAY_MS;
        assert!(vault.share_at(A, next).is_none(), "A's share still waits");
        let b = vault
            .share_at(B, next)
            .expect("B proposes the next day's body");
        let a = vault.share_at(A, next).expect("A joins B's body");
        assert_eq!(checkpoint_id(a.body()), checkpoint_id(b.body()));
        let later = next + DAY_MS;
        assert!(vault.share_at(A, later).is_none(), "A's share still waits");
        let much_later = next + finality::HEARD_WITHIN_MS;
        vault
            .share_at(A, much_later)
            .expect("A proposes again once its share is old");
    });
}

/// A log that needs no room and holds no other seat's unnested share answers no share before it
/// computes a proposal: a quiet vault's pass costs no plan. NEGATIVE CONTROL (observed): with
/// the proposal computed first (`Mutant::ProposeFirst`), `PLANS_COMPUTED` counts one and
/// `assert_eq!(.., 0)` fails with `left: 1`.
#[test]
fn vault_checkpoint_a_quiet_log_computes_no_proposal() {
    let mut vault = Vault::new(&[A, B, C]);
    let put = vault.put(A, "k", b"1");
    hold(&mut vault.log, put);
    assert!(!vault.log.wants_room());
    checkpoint::PLANS_COMPUTED.set(0);
    assert!(vault.log.share_draft(&A.id(), NOW).unwrap().is_none());
    assert_eq!(checkpoint::PLANS_COMPUTED.get(), 0);
}

/// What a prune turns on changes with a share held, also one that waited for its parent and is
/// held in the take that brings the parent, and not with a `Put`: a vault server judges what to
/// prune only after a take that changes it. NEGATIVE CONTROL (observed): with a server judging
/// after no take (`Mutant::NeverPrunes`), the released share changes nothing and
/// `assert_ne!(.., "the share is held")` fails.
#[test]
fn vault_checkpoint_a_server_judges_its_prune_after_a_take_that_can_change_it() {
    let world = passing_vault();
    let at = world
        .records
        .iter()
        .position(|record| record.kind() == Kind::Checkpoint)
        .unwrap();
    // Every record before the first share but its last parent, then the share, which waits.
    let parent = world.records[at - 1].clone();
    assert!(world.records[at].parents().contains(parent.id()));
    let mut log = Log::new(world.records[0].clone()).unwrap();
    log.stored_at = Reading::Format(FORMAT_TWO);
    for record in &world.records[1..at - 1] {
        hold(&mut log, record.clone());
    }
    let before = store_of(log.clone()).prune_inputs();
    assert_ne!(
        log.insert(world.records[at].clone()).unwrap(),
        crate::membership_log::Inserted::Held,
        "the share waits"
    );
    assert_eq!(
        store_of(log.clone()).prune_inputs(),
        before,
        "a waiting share"
    );
    assert_eq!(parent.kind(), Kind::Put);
    hold(&mut log, parent);
    assert_ne!(
        store_of(log.clone()).prune_inputs(),
        before,
        "the share is held"
    );
}

/// A pruned log asks of each stub whether it lies in a held record's past by the few held
/// records its descendants lead to, not by a walk of every stub after it: a log pruned to a
/// chain of stubs answers each such question in a step or two, not in the chain's length.
/// NEGATIVE CONTROL (observed): with the walk restored (`Mutant::WalkStubs` in `rooted.rs`),
/// the questions take about half the square of the chain's length, and the test fails with "780
/// steps for 39 stubs".
#[test]
fn vault_checkpoint_a_pruned_log_reads_a_stubs_past_in_a_step() {
    use crate::membership_log::rooted::STUB_STEPS;
    let budgets = RecordBudgets {
        weighed: 16,
        keyed: 64,
        held: 128,
    };
    budgets.within(|| {
        let mut vault = Vault::new(&[A, B, C]);
        let mut puts = Vec::new();
        for value in 0..40u8 {
            let put = vault.put(A, "k", &[value]);
            puts.push(*hold(&mut vault.log, put).id());
        }
        let body = propose(&vault.log, NOW).expect("a proposal").body;
        let share = one(vault.log.draft(&A.id(), NOW, body.clone()), A);
        hold(&mut vault.log, share);
        let pruned = vault.log.adopt(&body).expect("final by A");
        let winner = pruned.position(puts.last().unwrap()).unwrap();
        let stubs: Vec<usize> = puts[..puts.len() - 1]
            .iter()
            .map(|id| {
                assert!(pruned.is_dropped(id));
                pruned.position(id).unwrap()
            })
            .collect();
        STUB_STEPS.set(0);
        for &stub in &stubs {
            assert!(pruned.position_before(stub, winner));
        }
        let steps = STUB_STEPS.get();
        assert!(
            steps <= 2 * stubs.len(),
            "{steps} steps for {} stubs",
            stubs.len()
        );
    });
}
