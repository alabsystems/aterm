// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The writer of format 2 (tracker FL-148F): what its `Put`s and `Delete`s state of their
//! register, read back by the resolve and the checkpoint's keep set.

use super::*;
use crate::membership_log::checkpoint::tests::{A, B, C, Mac, epoch, found, hold, one};
use crate::membership_log::checkpoint::{self, DAY_MS};
use crate::membership_log::rooted::tests::entry;
use crate::membership_log::{Draft, EpochAudience, Record, RecordId, VERSION, VERSION_TWO};
use crate::text::Audience;
use zeroize::Zeroizing;

/// The log the tests write: A, B and C seated, an `[everyone]` epoch wrapped to each.
struct Vault {
    log: Log,
    epoch: (RecordId, Zeroizing<[u8; 32]>),
    clock: u64,
}

impl Vault {
    fn new() -> Self {
        let mut log = found(&[A, B, C]);
        let epoch = epoch(&mut log, A, DAY_MS, EpochAudience::Everyone, 1, &[A, B, C]);
        Self {
            log,
            epoch,
            clock: DAY_MS + 1,
        }
    }

    /// `by`'s draft of `value` to `name` (a `Delete` for `None`) from `log`.
    fn draft(&mut self, log: &Log, by: Mac, name: &str, value: Option<&[u8]>) -> Draft {
        self.clock += 1;
        let (id, key) = (&self.epoch.0, &self.epoch.1);
        match value {
            Some(value) => log.draft_put(
                &by.id(),
                self.clock,
                *id,
                key,
                &entry(&EpochAudience::Everyone, name, value),
            ),
            None => log.draft_delete(&by.id(), self.clock, *id, key, name),
        }
        .unwrap()
    }

    /// `by` writes as a build before format 2 does: record version 1.
    fn one(&mut self, by: Mac, name: &str, value: Option<&[u8]>) -> Record {
        let log = self.log.clone();
        let record = one(self.draft(&log, by, name, value), by);
        hold(&mut self.log, record)
    }

    /// `by` writes as a writer of format 2 does ([`Log::write_two`]).
    fn two(&mut self, by: Mac, name: &str, value: Option<&[u8]>) -> Record {
        let mut log = self.log.clone();
        log.write_two(&by.identity());
        let record = one(self.draft(&log, by, name, value), by);
        hold(&mut self.log, record)
    }

    /// The op the resolve reads as `name`'s value, as A opens it.
    fn rep(&self, name: &str) -> Option<RecordId> {
        self.log
            .open_audience_entries(&A.identity())
            .unwrap()
            .unwrap()
            .winning_ops
            .get(&(Audience::Everyone, name.to_owned()))
            .copied()
    }
}

fn sorted(records: &[&Record]) -> Vec<RecordId> {
    let mut ids: Vec<RecordId> = records.iter().map(|record| *record.id()).collect();
    ids.sort_unstable();
    ids
}

/// A writer's first version-2 write of an entry names every op of the entry it read, the
/// version-1 history of every signer included, and its next names only what came since; a
/// version-2 `Delete` after a `Delete` names it and reads as a delete again. Each of A's
/// version-1 ops is then superseded by its own signer's statement, as the keep set reads it,
/// and B's is not (no op of B's names it). NEGATIVE CONTROL (observed): with the writer naming
/// only the ops no version-2 op of the signer names yet left out (every op of the register named
/// again, `!stated.contains(*id)` dropped in `writer.rs`), the second write's statement names
/// all four ops and `second.statement()` fails its `assert_eq!` against `[first]`.
#[test]
fn vault_checkpoint_a_writers_first_v2_write_restates_the_entrys_v1_history() {
    let mut vault = Vault::new();
    let a1 = vault.one(A, "k", Some(b"1"));
    let b2 = vault.one(B, "k", Some(b"2"));
    let a3 = vault.one(A, "k", Some(b"3"));
    let other = vault.one(A, "other", Some(b"x"));
    let first = vault.two(A, "k", Some(b"4"));
    assert_eq!(first.version(), VERSION_TWO);
    let stated = first
        .statement()
        .expect("a version-2 Put states its register");
    assert_eq!(stated.supersedes, sorted(&[&a1, &b2, &a3]));
    assert_eq!(stated.carries, None, "a new value carries nothing");
    let second = vault.two(A, "k", Some(b"5"));
    assert_eq!(
        second.statement().unwrap().supersedes,
        vec![*first.id()],
        "only what came since the first"
    );
    assert_eq!(vault.rep("k"), Some(*second.id()));
    let superseded = checkpoint::superseded(&vault.log);
    for op in [&a1, &a3, &first] {
        assert!(superseded.contains(op.id()), "A's own op is superseded");
    }
    assert!(!superseded.contains(b2.id()), "B's op is B's to name");
    assert!(!superseded.contains(other.id()), "another entry's op");
    assert!(!superseded.contains(second.id()), "the winner stands");
    // A Delete names the value it removes and reads as a write; one after it reads as a carry.
    let gone = vault.two(A, "k", None);
    assert_eq!(gone.statement().unwrap().supersedes, vec![*second.id()]);
    assert_eq!(
        vault.log.frame_reps().get(gone.id()),
        None,
        "a Delete of a value"
    );
    let again = vault.two(A, "k", None);
    assert_eq!(again.statement().unwrap().supersedes, vec![*gone.id()]);
    assert_eq!(
        vault.log.frame_reps().get(again.id()),
        Some(gone.id()),
        "a Delete of a Delete stands for it"
    );
}

/// A version-2 `Put` that writes the value the register holds carries its semantic
/// representative, and so does one after a version-1 reseal of it: the reseal states nothing,
/// and the resolve reads it, and the writer's next carry, back to the first op of the chain.
/// NEGATIVE CONTROL (observed): with `Index::take` recording a carry's own id as the rep
/// (`rep: carries.unwrap_or(id)` replaced by `rep: id`), the third write is taken from the
/// index and carries `p` itself, and `assert_eq!(.., Some(*p0.id()))` fails on it.
#[test]
fn vault_checkpoint_a_v2_puts_carry_survives_a_version_one_reseal() {
    let mut vault = Vault::new();
    let p0 = vault.one(A, "k", Some(b"v"));
    // A's writer, whose log holds what it signs as a batch's snapshot does, carries v: its rep
    // is p0.
    let mut writer = vault.log.clone();
    writer.write_two(&A.identity());
    let draft = vault.draft(&writer, A, "k", Some(b"v"));
    let p = hold(&mut vault.log, one(draft, A));
    hold(&mut writer, p.clone());
    assert_eq!(p.statement().unwrap().carries, Some(*p0.id()));
    assert_eq!(p.statement().unwrap().supersedes, vec![*p0.id()]);
    // The same writer's next carry, read from its index alone, still names p0.
    let draft = vault.draft(&writer, A, "k", Some(b"v"));
    let p2 = hold(&mut vault.log, one(draft, A));
    assert_eq!(p2.statement().unwrap().carries, Some(*p0.id()));
    assert_eq!(p2.statement().unwrap().supersedes, vec![*p.id()]);
    // B, a build before format 2, reseals v as version 1: a carry the frontier reads.
    let q = vault.one(B, "k", Some(b"v"));
    assert_eq!(q.version(), VERSION);
    assert_eq!(vault.rep("k"), Some(*p0.id()), "the reseal stands for p0");
    // A's next carry after the reseal names p0, not the reseal.
    let s = vault.two(A, "k", Some(b"v"));
    assert_eq!(s.statement().unwrap().carries, Some(*p0.id()));
    assert_eq!(s.statement().unwrap().supersedes, sorted(&[&p2, &q]));
    assert_eq!(vault.rep("k"), Some(*p0.id()));
    // A's earlier carries are its own to drop; p0 stays as the rep the kept carry names.
    let superseded = checkpoint::superseded(&vault.log);
    assert!(superseded.contains(p.id()) && superseded.contains(p2.id()));
    assert!(!superseded.contains(q.id()), "B's reseal is B's to name");
    assert!(!superseded.contains(s.id()), "the winner stands");
}

/// A log no writer of format 2 drafts from states nothing: its `Put`s and `Delete`s sign as
/// record version 1, byte for byte as a log before the writer existed signs them. NEGATIVE
/// CONTROL (observed): with `statement_for` stating a register on a log no writer drafts for
/// (its `self.writing.as_ref()?` replaced by a writer of A's identity), the draft signs as
/// version 2 and `assert_eq!(record.version(), VERSION)` fails.
#[test]
fn vault_checkpoint_a_log_with_no_writer_signs_version_one() {
    let mut vault = Vault::new();
    let before = vault.one(A, "k", Some(b"1"));
    let record = vault.one(A, "k", Some(b"2"));
    assert_eq!(record.version(), VERSION);
    assert_eq!(record.statement(), None);
    assert_eq!(before.version(), VERSION);
    let gone = vault.one(A, "k", None);
    assert_eq!(gone.version(), VERSION);
    assert!(!vault.log.writes_two());
}

/// A log at format 2 signs no batch that takes its keyed records past its keyed budget less one
/// checkpoint share of each seat (three here): the last write that fits is asked through, the
/// next is refused as `MembershipCapacity` naming the shares, and the same log at format 1 is
/// not asked. NEGATIVE CONTROL (observed): with the stop at the budget itself (`seats` passed
/// as 0 to `keyed_refusal` in `Log::asked`), the write past the room is taken and
/// `expect_err` panics on its `Ok`.
#[test]
fn vault_checkpoint_a_format_two_log_keeps_room_for_each_seats_checkpoint_share() {
    use crate::membership_log::Reading;
    use crate::membership_log::budgets::{Author, Authoring, Class};
    let budgets = crate::membership_log::RecordBudgets {
        weighed: 16,
        keyed: 16,
        held: 32,
    };
    budgets.within(|| {
        let mut vault = Vault::new();
        vault.log.stored_at = Reading::Format(crate::crossing::FORMAT_TWO);
        // The epoch and its three wraps, then Puts up to the room: 16 less 3 shares.
        while vault.log.counts.keyed < 12 {
            vault.two(A, "k", Some(b"v"));
        }
        let ask = |vault: &mut Vault| {
            let mut log = vault.log.clone();
            log.write_two(&A.identity());
            let record = one(vault.draft(&log, A, "k", Some(b"w")), A);
            let batch = Authoring::all(std::slice::from_ref(&record), Class::Person);
            (log.author_ask(&batch, Author::Person, &A.id(), 0), record)
        };
        let (fits, record) = ask(&mut vault);
        fits.expect("the thirteenth keyed record fits");
        hold(&mut vault.log, record);
        let (refused, _) = ask(&mut vault);
        let refused = refused.expect_err("the fourteenth takes a seat's share");
        assert_eq!(refused.kind(), crate::ErrorKind::MembershipCapacity);
        assert!(
            refused.to_string().contains("checkpoint share"),
            "{refused}"
        );
        vault.log.stored_at = Reading::ONE;
        ask(&mut vault).0.expect("a format-1 log is not asked");
    });
}

/// A log that reads format 2 drafts no `Put`, `Delete` or Commons control `Put` for no writer
/// of format 2: a version-1 op has no place over version-2 history, whose shed and pruned ops it
/// would read by their values. With a writer each signs version 2. NEGATIVE CONTROL (observed):
/// with `statement_for` answering `None` on a log with no writer whatever it reads (its
/// `stored_at.admits_two()` refusal cut in `writer.rs`), the draft is taken and signs version 1,
/// and `expect_err` panics with "a format-2 log refuses a writerless Put".
#[test]
fn vault_checkpoint_a_format_two_log_drafts_no_version_one_op() {
    let mut vault = Vault::new();
    vault.log.stored_at = crate::membership_log::Reading::Format(crate::crossing::FORMAT_TWO);
    let (id, key) = (vault.epoch.0, vault.epoch.1.clone());
    let put = vault
        .log
        .draft_put(
            &A.id(),
            7,
            id,
            &key,
            &entry(&EpochAudience::Everyone, "k", b"v"),
        )
        .map(|_| ())
        .expect_err("a format-2 log refuses a writerless Put");
    assert!(put.to_string().contains("format 2"), "{put}");
    vault
        .log
        .draft_delete(&A.id(), 7, id, &key, "k")
        .map(|_| ())
        .expect_err("a format-2 log refuses a writerless Delete");
    let record = vault.two(A, "k", Some(b"v"));
    assert_eq!(record.version(), VERSION_TWO);
    let gone = vault.two(A, "k", None);
    assert_eq!(gone.version(), VERSION_TWO);
}

/// A writer whose names do not all fit its record names the newest that do, by record order,
/// and still signs version 2; its next write names the older ones it left out. With room for two
/// names, A's first version-2 write over its three version-1 ops names the last two, and its
/// next names the first and the one before it. NEGATIVE CONTROL (observed): with every name
/// kept whatever the room (`state`'s `drain` cut), the first write names all three and its
/// `assert_eq!(.., sorted(&[&a2, &a3]))` fails with three ids on the left.
#[test]
fn vault_checkpoint_a_writer_names_the_newest_ops_that_fit() {
    let mut vault = Vault::new();
    let a1 = vault.one(A, "k", Some(b"1"));
    let a2 = vault.one(A, "k", Some(b"2"));
    let a3 = vault.one(A, "k", Some(b"3"));
    MOST_NAMES.set(Some(2));
    let first = vault.two(A, "k", Some(b"4"));
    let second = vault.two(A, "k", Some(b"5"));
    MOST_NAMES.set(None);
    assert_eq!(first.version(), VERSION_TWO);
    assert_eq!(first.statement().unwrap().supersedes, sorted(&[&a2, &a3]));
    assert_eq!(
        second.statement().unwrap().supersedes,
        sorted(&[&a1, &first])
    );
    let superseded = checkpoint::superseded(&vault.log);
    for op in [&a1, &a2, &a3, &first] {
        assert!(superseded.contains(op.id()), "each is named once");
    }
    assert_eq!(vault.rep("k"), Some(*second.id()));
}

/// A draft that does not follow every head of its log (more heads than a record names: here
/// sixty-five forks of A's at one sequence) states what its own causal past reads, and signs
/// version 2: it names the ops of its register in its parents' past and none outside it.
/// NEGATIVE CONTROL (observed): with `statement_for` answering `None` for such a draft (its
/// `statement_within` call replaced by `return Ok(None)`), a writer on a log at format 1 signs
/// version 1 and `assert_eq!(put.version(), VERSION_TWO)` fails with `left: 1`.
#[test]
fn vault_checkpoint_a_draft_that_follows_fewer_heads_states_its_own_past() {
    use crate::membership_log::checkpoint::tests::{draft, name};
    let mut vault = Vault::new();
    let b1 = vault.one(B, "k", Some(b"1"));
    let base = vault.log.clone();
    for at in 0..65 {
        let fork = one(draft(&base, A, 10_000 + at, name(&format!("a{at}"))), A);
        hold(&mut vault.log, fork);
    }
    assert!(vault.log.heads().len() > crate::membership_log::MAX_PARENTS);
    let mut writer = vault.log.clone();
    writer.write_two(&C.identity());
    let draft = vault.draft(&writer, C, "k", Some(b"2"));
    let past = vault.log.past_of(&draft.parents);
    assert!(!vault.log.heads().iter().all(|head| past.holds(head)));
    let put = one(draft, C);
    assert_eq!(put.version(), VERSION_TWO);
    assert_eq!(put.statement().unwrap().supersedes, vec![*b1.id()]);
}
