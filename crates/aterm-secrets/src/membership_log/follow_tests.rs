// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What moves a node across the format line (tracker FL-148e): a version-2 record its own log
//! holds complete, signed by a seat, and nothing a source only says.

use super::*;
use crate::crossing::{Faults, inject};
use crate::membership_log::checkpoint::tests::{
    A, B, C, HOUR, T, draft, found, name, one, opaque_put, supersedes, two,
};
use crate::membership_log::rooted::tests::store_of;
use crate::membership_log::{Body, EpochAudience, Log, encode_records};

/// [`follow_records`] of a records stream as a format-2 source answers it, each record read as
/// a follow copy reads it.
fn follow(mark: &FormatMark, store: &Store, reply: &[u8]) -> Result<Option<Followed>> {
    let records = crate::membership_log::store::decode_records_in(
        reply,
        store.budgets().held,
        Reading::Follow,
    )?;
    follow_records(mark, store, records, None)
}

/// A node's format mark in a folder of its own, with what a test does to the node.
struct Node {
    _dir: aterm_tempfile::TempDir,
    mark: FormatMark,
}

impl Node {
    fn new(faults: Faults) -> Self {
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let mark = FormatMark::at(dir.path().join("records.format"));
        inject(mark.path(), faults);
        Self { _dir: dir, mark }
    }

    /// A node whose build reads format 2.
    fn following() -> Self {
        Self::new(Faults {
            follows: true,
            ..Faults::default()
        })
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        inject(self.mark.path(), Faults::default());
    }
}

/// A vault of three seats with one format-1 record past its genesis, and what a format-2
/// source holds past it: `n2`, a format-1 record; and `v2`, a version-2 `Put` A signed that
/// follows `n2`.
struct World {
    base: Log,
    n2: Record,
    v2: Record,
}

fn world() -> World {
    let mut base = found(&[A, B, C]);
    let n1 = one(draft(&base, A, T, name("n1")), A);
    base.insert(n1).unwrap();
    let epoch = one(
        draft(
            &base,
            A,
            T,
            Body::Epoch {
                audience: EpochAudience::Everyone,
                generation: 1,
                commitment: [7; 32],
            },
        ),
        A,
    );
    let epoch_id = *epoch.id();
    base.insert(epoch).unwrap();
    let mut ahead = store_of(base.clone()).read_at(Reading::Format(FORMAT_TWO));
    let n2 = one(draft(ahead.log().unwrap(), B, T + HOUR, name("n2")), B);
    assert_eq!(ahead.ingest_records(vec![n2.clone()]).unwrap(), 1);
    let v2 = two(
        draft(
            ahead.log().unwrap(),
            A,
            T + 2 * HOUR,
            opaque_put(EpochAudience::Everyone, epoch_id, 9),
        ),
        A,
        Some(&supersedes(&[], None)),
    );
    assert_eq!(v2.version(), 2);
    assert!(v2.parents().contains(n2.id()), "v2 follows n2");
    assert_eq!(ahead.ingest_records(vec![v2.clone()]).unwrap(), 1);
    World { base, n2, v2 }
}

/// Only a version-2 record a node's own copy holds complete, signed by a seat, is the evidence
/// its format mark is stored on: format-1 records a format-2 source sent are none, and neither
/// is a version-2 record that waits on a parent the source withheld. On the evidence the copy
/// is handed to the write only after the mark names format 2, and reads format 2 from then on.
/// A node whose build reads no format 2 follows nothing, whatever it is sent.
///
/// NEGATIVE CONTROLS, each run against this test and seen to fail:
/// - `Faults::mark_on_header` (the source's statement is the evidence): ``format-1 records are
///   no evidence``.
/// - `Faults::mark_on_waiting` (a version-2 record that only waits is the evidence): ``a
///   version-2 record that waits on a parent is no evidence``.
/// - [`FormatMark::follows`] answering yes for every node (no gate): ``a build that reads no
///   format 2 follows nothing``.
#[test]
fn vault_checkpoint_only_a_seats_complete_version_two_record_is_evidence() {
    let world = world();
    let store = store_of(world.base.clone());
    let node = Node::following();

    let v1_only = encode_records([&world.n2]);
    assert!(
        follow(&node.mark, &store, &v1_only).unwrap().is_none(),
        "format-1 records are no evidence"
    );
    let waiting = encode_records([&world.v2]);
    assert!(
        follow(&node.mark, &store, &waiting).unwrap().is_none(),
        "a version-2 record that waits on a parent is no evidence"
    );
    assert_eq!(node.mark.stored().unwrap(), FORMAT_ONE);
    assert!(evidence(&store).is_none());

    let both = encode_records([&world.n2, &world.v2]);
    let dark = Node::new(Faults::default());
    assert!(
        follow(&dark.mark, &store, &both).unwrap().is_none(),
        "a build that reads no format 2 follows nothing"
    );
    assert_eq!(reading(&dark.mark).unwrap(), Reading::ONE);
    assert!(take_on_evidence(&dark.mark, &store, || Ok(())).is_err());
    assert_eq!(dark.mark.stored().unwrap(), FORMAT_ONE);

    let followed = follow(&node.mark, &store, &both)
        .unwrap()
        .expect("a seat's complete version-2 record is the evidence");
    assert_eq!(followed.added(), 2);
    assert_eq!(
        node.mark.stored().unwrap(),
        FORMAT_ONE,
        "no mark before take"
    );
    assert!(!store.holds(world.v2.id()), "the node's store is untouched");
    let taken = followed
        .take(&node.mark, |copy| {
            assert_eq!(
                node.mark.stored().unwrap(),
                FORMAT_TWO,
                "the mark is durable before the write sees the copy"
            );
            assert_eq!(copy.reading(), Reading::Format(FORMAT_TWO));
            Ok(copy.clone())
        })
        .unwrap();
    assert_eq!(evidence(&taken), Some(*world.v2.id()));
    assert_eq!(reading(&node.mark).unwrap(), Reading::Format(FORMAT_TWO));
    assert!(
        follow(&node.mark, &store, &both).unwrap().is_none(),
        "a marked node follows nothing again"
    );
}

/// A frame a puller reads is named by its record's id, the one id a version-2 record's whole
/// and shed forms share (tracker FL-148a's carried defect: the SHA-256 of a frame's bytes is a
/// version-1 record's id alone). So a store that holds a version-2 record holds the frame a
/// peer sent of it, in either form, and is not counted as having left it untaken; and the
/// record the puller already holds stands for a frame only where it is the same bytes, so a
/// whole record a peer sends is not replaced by the shed one the puller holds under that id.
///
/// NEGATIVE CONTROLS, each a change to `ReceivedFrames::push` run against this test and seen
/// to fail:
/// - the frame named by the SHA-256 of its bytes (as before): ``a store that holds the record
///   holds its frame``.
/// - the held record taken for the frame whatever its form (as before): ``the whole record
///   the peer sent is what the puller reads``.
#[test]
fn vault_checkpoint_a_pulled_frame_is_named_by_its_records_id() {
    use crate::membership_log::untaken::ReceivedFrames;
    let world = world();
    let whole = world.v2.wire_bytes().unwrap().into_owned();
    let shed = Record::decode_in(&world.v2.shed_frame().unwrap(), Reading::Follow).unwrap();
    assert!(shed.is_shed() && !world.v2.is_shed());
    assert_eq!(shed.id(), world.v2.id());
    assert_ne!(aterm_digest::Sha256::digest(&whole), *world.v2.id());

    let mut store = store_of(world.base.clone());
    store
        .ingest_records(vec![world.n2.clone(), shed.clone()])
        .unwrap();
    let received = ReceivedFrames::read_at(&mut whole.as_slice(), Reading::Follow, false, |id| {
        (id == shed.id()).then(|| shed.clone())
    })
    .unwrap();
    let read = received.whole().expect("the stream reads whole");
    assert_eq!(read.len(), 1);
    assert!(
        !read[0].is_shed(),
        "the whole record the peer sent is what the puller reads"
    );
    assert!(
        !received.into_sent().left_in(&store),
        "a store that holds the record holds its frame"
    );

    // A node of format 1 reads no version-2 frame.
    let at_one =
        ReceivedFrames::read_at(&mut whole.as_slice(), Reading::ONE, false, |_| None).unwrap();
    assert!(at_one.whole().is_none());
}

/// A response's served frame is read only by a reader that asked for one: it is kept beside
/// the records it follows, and counted in the response's bytes. A reader that asked for none
/// refuses the whole response, as it did before any reader took a frame, and a frame followed
/// by another byte refuses it for a reader that asked.
///
/// NEGATIVE CONTROL: `ReceivedFrames::read_at` taking a served frame whatever its caller asked,
/// run against this test and seen to fail with ``a reader that asked for none refuses the
/// frame``.
#[test]
fn vault_checkpoint_a_served_frame_is_read_only_where_asked() {
    use crate::membership_log::store::{REPLY_CAP, read_reply};
    use crate::membership_log::untaken::ReceivedFrames;
    let vault = crate::membership_log::pruned_vault();
    let reply = read_reply(&vault.peer.missing_served(&[], REPLY_CAP)).unwrap();
    let read = |bytes: &[u8], asks: bool| {
        let mut bytes = bytes;
        ReceivedFrames::read_at(&mut bytes, Reading::Follow, asks, |_| None).unwrap()
    };

    let mut asked = read(&reply, true);
    assert_eq!(asked.byte_len(), reply.len());
    assert!(asked.whole().is_some());
    assert_eq!(
        asked.take_served().map(|served| served.roots),
        Some(vec![vault.root])
    );

    let mut unasked = read(&reply, false);
    assert!(
        unasked.take_served().is_none() && unasked.whole().is_none(),
        "a reader that asked for none refuses the frame"
    );

    let mut trailing = reply.clone();
    trailing.push(0);
    let mut refused = read(&trailing, true);
    assert!(refused.take_served().is_none() && refused.whole().is_none());
}
