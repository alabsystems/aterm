// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE PUBLISHER CONNECTION, WITH THE ACKS IT HAS NOT READ YET.

use std::collections::VecDeque;
use std::io;
use std::ops::Deref;
use std::time::Instant;

use astream_broker::Pipelined;

use crate::transport::Conn;

/// One pipelined publish whose ack has not been read.
#[derive(Debug)]
pub(crate) struct Unacked<T> {
    /// The producer sequence it was sent under.
    pub seq: u64,
    /// Whether this was the FIRST send of a sequence just reserved — the one whose deduped answer is another writer's record, not a retry.
    pub first: bool,
    /// When it was written to the connection.
    pub sent: Instant,
    /// What its sender wants done with the answer.
    pub tag: T,
    /// A number hung on this ack AFTER it was sent ([`Link::ride`]), handed back with its answer: the caller learns that what it was waiting on — this publish and every one before it — has been answered on a connection still in step.
    pub rider: Option<u64>,
}

/// One owed publish and the broker's answer to it: `(offset, deduped)`, or the refusal or transport failure ([`Link::recv`]).
pub(crate) type Settled<T> = (Unacked<T>, io::Result<(u64, bool)>);

/// A consumer group's commit that rides a burst of publishes in one write, ahead of them
/// ([`Link::send_burst`]).
pub(crate) struct HeldCommit<'a, T> {
    /// The group.
    pub group: &'a str,
    /// The offset it commits to.
    pub upto: u64,
    /// What its sender wants done with its answer.
    pub tag: T,
}

/// One publish of a burst ([`Link::send_burst`]).
pub(crate) struct Publish<'a, T> {
    /// The producer the record is deduped under.
    pub producer_id: u64,
    /// Its sequence within that producer.
    pub seq: u64,
    /// Whether this is the FIRST send of a sequence just reserved ([`Unacked::first`]).
    pub first: bool,
    /// Where it is published.
    pub subject: &'a str,
    /// What it carries.
    pub body: &'a [u8],
    /// What its sender wants done with its answer.
    pub tag: T,
}

/// A burst the connection did not take ([`Link::send_burst`]): the send's error and every tag
/// it carried, the commit's apart. Nothing is owed for any of them, though each frame ahead
/// of the cut may have landed.
pub(crate) struct Unsent<T> {
    /// The send's error.
    pub error: io::Error,
    /// The commit's tag, when one rode.
    pub commit: Option<T>,
    /// Each publish's tag, in the burst's order.
    pub tags: Vec<T>,
}

/// The publisher connection and the acks owed on it, in send order.
pub(crate) struct Link<T> {
    client: Conn,
    unacked: VecDeque<Unacked<T>>,
    /// A write of this connection was refused since its GAP FENCES were last lifted, on a
    /// broker that keeps them ([`Link::gap_fences`]): until the producer lifts them
    /// ([`Link::lift_gap_fences`]), a write at or under the refused sequence lands, or the
    /// connection closes, every later write above it is refused `gap-fenced=`.
    fenced: bool,
    /// This connection asked its broker to gap-fence its writes ([`Link::ask_gap_fences`]).
    asked: bool,
    /// The most acks this connection has had unread at once: what a test reads
    /// to know a burst went out without waiting on each answer.
    #[cfg(test)]
    peak: usize,
    /// Told each write's frames, in the write's order, as it goes out ([`Link::tap_writes`]):
    /// what a test reads to know which frames shared a write.
    #[cfg(test)]
    tap: Option<WriteTap<T>>,
}

/// What [`Link::tap_writes`] is told: one write's frames in order, each its tag and, for a
/// publish, its subject (`None`: a commit).
#[cfg(test)]
type WriteTap<T> = Box<dyn FnMut(&[(&T, Option<&str>)]) + Send>;

impl<T> From<Conn> for Link<T> {
    fn from(client: Conn) -> Self {
        Link {
            client,
            unacked: VecDeque::new(),
            fenced: false,
            asked: false,
            #[cfg(test)]
            peak: 0,
            #[cfg(test)]
            tap: None,
        }
    }
}

/// Read-only access, for what asks the client nothing over the wire
/// (`seen_log_identity`).
impl<T> Deref for Link<T> {
    type Target = Conn;
    fn deref(&self) -> &Conn {
        &self.client
    }
}

impl<T> Link<T> {
    /// The client, for a request that reads its own reply — ONLY while no pipelined ack is unread, so that reply is the next frame.
    pub fn ready(&mut self) -> Option<&mut Conn> {
        self.unacked.is_empty().then_some(&mut self.client)
    }

    /// Whether this connection's broker GAP-FENCES each producer's writes on a connection that
    /// asks, as its `Hello` said (astream's `Client::supports_gap_fences`,
    /// `OpenOptions::sticky_batch_refusal`), and this one asked ([`Link::ask_gap_fences`]): once
    /// one is refused, no later one above its sequence lands until the producer lifts the
    /// fence. Only then may one sender keep several posts in flight (`Bridge::pipelines_posts`):
    /// against any other broker, and on a connection that did not ask, a refused batch is
    /// rolled back and the next write lands ahead of the refused one's retry
    /// (`a_refused_batch_lands_the_publish_sent_after_it`).
    pub fn gap_fences(&self) -> bool {
        self.asked && self.client.supports_gap_fences()
    }

    /// ASK THE BROKER TO GAP-FENCE this connection's writes (astream's `Client::fence_gaps`),
    /// where its `Hello` said it offers that, before anything is written on it: a broker fences
    /// only a connection that asks, so a link of an older build, which never lifts a fence, is
    /// not held behind a refusal it does not retry next. A broker that announced the fence
    /// and predates the ask refuses it, and fences every connection anyway (astream
    /// `8a15cdb`..`ea8bab7`), so the link is fenced either way once its `Hello` said so. Against
    /// a broker that did not say so nothing is asked.
    /// # Errors
    /// The request's transport failure.
    pub fn ask_gap_fences(&mut self) -> io::Result<()> {
        if !self.client.supports_gap_fences() {
            return Ok(());
        }
        self.client.fence_gaps()?;
        self.asked = true;
        Ok(())
    }

    /// Take the broker's offer as asked for without asking: the bridge that pipelines on the
    /// `Hello` alone (`Mutant::NeverAskForTheFence`).
    #[cfg(test)]
    pub fn assume_asked(&mut self) {
        self.asked = true;
    }

    /// A write of this connection was refused: on a broker that gap-fences, the connection
    /// now holds a fence ([`Link::fenced`]).
    pub fn note_refused(&mut self) {
        self.fenced |= self.gap_fences();
    }

    /// Whether a write was refused since the fences were last lifted ([`Link::note_refused`]).
    pub fn fenced(&self) -> bool {
        self.fenced
    }

    /// Whether an owed publish was sent at a sequence above `seq`. On a broker that
    /// gap-fences, a write at or under a fence lands and lifts it, so a publish sent below
    /// one still owed could let a write sent after it land ahead of an earlier refused one's
    /// retry: the caller reads those answers first (`Bridge::settle_above`).
    pub fn owes_above(&self, seq: u64) -> bool {
        self.unacked.iter().any(|owed| owed.seq > seq)
    }

    /// LIFT THE GAP FENCES this connection's refused writes set (astream's
    /// `Client::clear_gap_fences`): only with no ack owed, every answer read, so the caller
    /// knows each refused write and holds back what must follow it; `None` while one is
    /// owed. The broker answers it after every write sent before it, so it reaches none of
    /// them.
    /// # Errors
    /// The request's: a transport failure, or a refusal from a broker that predates it.
    pub fn lift_gap_fences(&mut self) -> Option<io::Result<()>> {
        if !self.unacked.is_empty() {
            return None;
        }
        let lifted = match self.client.clear_gap_fences() {
            Ok(true) => Ok(()),
            Ok(false) => Err(io::Error::other(
                "the broker said it gap-fences, and refused to lift its fences",
            )),
            Err(e) => Err(e),
        };
        if lifted.is_ok() {
            self.fenced = false;
        }
        Some(lifted)
    }

    /// Write one publish WITHOUT waiting for its ack, and queue `tag` for the answer: a burst
    /// of one ([`Link::send_burst`]), for the tests that drive the connection directly.
    /// # Errors
    /// The send's, with the tag.
    #[cfg(test)]
    #[allow(
        clippy::too_many_arguments,
        reason = "one publish's fields, its tag and the instant its caller read off its own clock"
    )]
    pub fn send(
        &mut self,
        producer_id: u64,
        seq: u64,
        first: bool,
        subject: &str,
        body: &[u8],
        tag: T,
        now: Instant,
    ) -> Result<(), (io::Error, T)> {
        let publish = Publish {
            producer_id,
            seq,
            first,
            subject,
            body,
            tag,
        };
        self.send_burst(None, vec![publish], now)
            .map_err(|mut unsent| {
                let tag = unsent.tags.pop().expect("the one publish's tag");
                (unsent.error, tag)
            })
    }

    /// Write a consumer group's COMMIT to `upto` WITHOUT waiting for its ack (astream's `Client::send_commit`), and queue `tag` for the answer.
    /// # Errors
    /// The send's, with the tag.
    pub fn send_commit(
        &mut self,
        group: &str,
        upto: u64,
        tag: T,
        now: Instant,
    ) -> Result<(), (io::Error, T)> {
        match self.client.send_commit(group, upto) {
            Ok(()) => {
                #[cfg(test)]
                if let Some(tap) = self.tap.as_mut() {
                    tap(&[(&tag, None)]);
                }
                self.owe(Unacked {
                    seq: 0,
                    first: false,
                    sent: now,
                    tag,
                    rider: None,
                });
                Ok(())
            }
            Err(e) => Err((e, tag)),
        }
    }

    /// Write a consumer group's COMMIT to `commit.upto`, when one rides, AND every publish of
    /// `publishes` in ONE write, the commit first, WITHOUT waiting for any ack (astream's
    /// `Client::send_pipelined`), and queue the commit's tag, then each publish's in order,
    /// for their answers. The broker's reader hands its writer what it finds already
    /// received whole as one unit, so a burst the reader takes in one read lands in one
    /// group-commit batch behind one flush, where a commit written alone a moment before
    /// pays a flush of its own (`broker.pipelining`). A burst the reader does not take whole
    /// in one read (wider than its read step or the socket's buffer) is handed over in
    /// parts that share batches as they allow: the batch is a cost, never a contract, and
    /// what each op did is its own ack's to say.
    /// # Errors
    /// The send's, with every tag ([`Unsent`]): nothing is owed for any of them, though a
    /// write cut short may have landed every frame the broker received whole ahead of the
    /// cut, so a publish of a failed burst may already be on the bus. Only the last frame is
    /// sure not to be.
    pub fn send_burst(
        &mut self,
        commit: Option<HeldCommit<'_, T>>,
        publishes: Vec<Publish<'_, T>>,
        now: Instant,
    ) -> Result<(), Unsent<T>> {
        let mut burst = Vec::with_capacity(publishes.len() + usize::from(commit.is_some()));
        if let Some(c) = &commit {
            burst.push(Pipelined::Commit {
                group: c.group,
                upto: c.upto,
            });
        }
        burst.extend(publishes.iter().map(|p| Pipelined::Publish {
            producer_id: p.producer_id,
            producer_seq: p.seq,
            subject: p.subject,
            body: p.body,
        }));
        if let Err(error) = self.client.send_pipelined(&burst) {
            return Err(Unsent {
                error,
                commit: commit.map(|c| c.tag),
                tags: publishes.into_iter().map(|p| p.tag).collect(),
            });
        }
        #[cfg(test)]
        if let Some(tap) = self.tap.as_mut() {
            let frames: Vec<(&T, Option<&str>)> = commit
                .iter()
                .map(|c| (&c.tag, None))
                .chain(publishes.iter().map(|p| (&p.tag, Some(p.subject))))
                .collect();
            tap(&frames);
        }
        if let Some(c) = commit {
            self.owe(Unacked {
                seq: 0,
                first: false,
                sent: now,
                tag: c.tag,
                rider: None,
            });
        }
        for p in publishes {
            self.owe(Unacked {
                seq: p.seq,
                first: p.first,
                sent: now,
                tag: p.tag,
                rider: None,
            });
        }
        Ok(())
    }

    fn owe(&mut self, owed: Unacked<T>) {
        self.unacked.push_back(owed);
        #[cfg(test)]
        {
            self.peak = self.peak.max(self.unacked.len());
        }
    }

    /// Read the OLDEST owed ack, answering it with the publish it belongs to — `None` when nothing is owed.
    pub fn recv(&mut self) -> Option<Settled<T>> {
        let owed = self.unacked.pop_front()?;
        let answer = self.client.recv_publish_ack();
        Some((owed, answer))
    }

    /// [`Link::recv`] WITHOUT WAITING: the oldest owed ack if the broker has answered it already, and `None` when nothing is owed or its answer has not arrived — astream's `Client::try_recv_publish_ack`, which keeps a frame that only partly arrived for the next read (a try's or a blocking one's), so a `None` leaves the connection in step owing what it owed.
    pub fn try_recv(&mut self) -> Option<Settled<T>> {
        if self.unacked.is_empty() {
            return None;
        }
        let answer = match self.client.try_recv_publish_ack() {
            Ok(None) => return None,
            Ok(Some(ack)) => Ok(ack),
            Err(e) => Err(e),
        };
        let owed = self.unacked.pop_front()?;
        Some((owed, answer))
    }

    /// How many acks are owed.
    pub fn unacked(&self) -> usize {
        self.unacked.len()
    }

    /// The tags of the publishes still owed an ack, oldest first.
    pub fn owed(&self) -> impl Iterator<Item = &T> {
        self.unacked.iter().map(|owed| &owed.tag)
    }

    /// Hang `value` on the NEWEST owed ack whose tag `waits_on` names, to come back with that ack's answer ([`Unacked::rider`]); a rider already there keeps the larger of the two.
    pub fn ride(&mut self, value: u64, waits_on: impl Fn(&T) -> bool) -> bool {
        let Some(owed) = self
            .unacked
            .iter_mut()
            .rev()
            .find(|owed| waits_on(&owed.tag))
        else {
            return false;
        };
        owed.rider = Some(owed.rider.map_or(value, |have| have.max(value)));
        true
    }

    /// Whether any owed ack carries a rider ([`Link::ride`]).
    pub fn riding(&self) -> bool {
        self.unacked.iter().any(|owed| owed.rider.is_some())
    }

    /// The most acks this connection has had unread at once.
    #[cfg(test)]
    pub fn peak(&self) -> usize {
        self.peak
    }

    /// Tell `tap` each later write's frames, in the write's order: a commit and the publishes
    /// of one [`Link::send_burst`] in one call, a [`Link::send_commit`] in one of its own.
    #[cfg(test)]
    pub fn tap_writes(&mut self, tap: impl FnMut(&[(&T, Option<&str>)]) + Send + 'static) {
        self.tap = Some(Box::new(tap));
    }
}

impl<T> Drop for Link<T> {
    /// A connection dropped with acks unread: what they answered is unknown.
    fn drop(&mut self) {
        if !self.unacked.is_empty() {
            eprintln!(
                "aterm-link: the broker connection closed with {} pipelined publish(es) \
                 unacknowledged; whether they landed is unknown",
                self.unacked.len()
            );
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// Serve a scratch broker and connect a [`Link`] to it, with a second plain
    /// client for reading what landed.
    fn served() -> (
        aterm_tempfile::TempDir,
        astream_broker::BrokerHandle,
        Link<u32>,
        Conn,
    ) {
        let dir = aterm_tempfile::Builder::new()
            .prefix("al-p")
            .tempdir_in("/tmp")
            .expect("a scratch dir");
        let socket = dir.path().join("b.sock");
        let broker = crate::test_broker(dir.path().join("b.log")).expect("a broker");
        let server = broker.serve(&socket).expect("serve");
        let connect = || {
            let (conn, _) = crate::transport::connect(
                &socket.to_string_lossy(),
                std::time::Duration::from_secs(60),
            )
            .expect("dial");
            conn
        };
        (dir, server, Link::from(connect()), connect())
    }

    /// **A BURST GOES OUT WHOLE, AND EACH ANSWER FINDS ITS OWN RECORD.** Ten
    /// publishes are written before one ack is read; while any is owed the
    /// client is not handed out for a request that reads its own reply; the
    /// acks come back in send order, each naming its own sequence and its own
    /// offset; and once they are read the connection serves a `Fetch` that
    /// sees all ten.
    #[test]
    fn a_burst_is_sent_before_any_ack_and_every_answer_is_its_own() {
        let (_dir, _server, mut link, mut reader) = served();
        for i in 0..10u32 {
            let subject = if i == 4 {
                "/f/f//empty-segment".to_string()
            } else {
                format!("/f/f/x/{i}")
            };
            link.send(7, u64::from(i) + 1, true, &subject, b"b", i, Instant::now())
                .map_err(|(e, _)| e)
                .expect("written");
        }
        assert_eq!(link.unacked(), 10);
        assert_eq!(link.peak(), 10, "all ten were on the wire at once");
        assert!(
            link.ready().is_none(),
            "no request may read a reply past an owed ack"
        );
        let mut answered = Vec::new();
        while let Some((owed, answer)) = link.recv() {
            assert_eq!(owed.seq, u64::from(owed.tag) + 1);
            match answer {
                Ok((off, deduped)) => {
                    assert!(!deduped);
                    answered.push((owed.tag, Some(off)));
                }
                Err(e) => {
                    let refusal = astream_broker::broker_refusal(&e).expect("a refusal");
                    assert_eq!(refusal.code, astream_broker::proto::ERR_BAD_SUBJECT);
                    answered.push((owed.tag, None));
                }
            }
        }
        assert_eq!(
            answered.iter().map(|(tag, _)| *tag).collect::<Vec<_>>(),
            (0..10).collect::<Vec<_>>(),
            "in send order"
        );
        assert_eq!(answered[4].1, None, "the refusal is the bad subject's own");
        let landed: Vec<u64> = answered.iter().filter_map(|(_, off)| *off).collect();
        assert_eq!(landed.len(), 9);
        assert!(
            landed.windows(2).all(|w| w[0] < w[1]),
            "one connection, in order"
        );
        let conn = link
            .ready()
            .expect("nothing owed: the client is handed out");
        let (rows, _) = conn
            .fetch(0, "/f/f/x/*", 64)
            .expect("a fetch after the acks");
        assert_eq!(
            rows.iter().map(|(off, _, _)| *off).collect::<Vec<_>>(),
            landed,
            "the fetch reads records, not a stray ack"
        );
        let (rows, _) = reader
            .fetch(0, "/f/f/x/*", 64)
            .expect("a second client agrees");
        assert_eq!(rows.len(), 9);
    }

    /// **A REQUEST BEHIND AN OWED ACK FAILS LOUDLY, AND SENDS NOTHING** — the
    /// FIRST guard, astream's own, reached the one way nothing
    /// in this crate can: past [`Link::ready`], straight to the client.
    #[test]
    fn a_request_behind_an_owed_ack_fails_loudly_and_sends_nothing() {
        let (_dir, _server, mut link, _reader) = served();
        link.send(7, 1, true, "/f/f/owed/a", b"a", 0, Instant::now())
            .map_err(|(e, _)| e)
            .expect("A written");
        assert!(link.ready().is_none(), "the second guard holds the client");
        let fetched = link
            .client
            .fetch(0, "/f/f/owed/*", 64)
            .expect_err("a fetch behind an owed ack is refused, never answered");
        assert_eq!(
            astream_broker::acks_owed(&fetched),
            Some(&astream_broker::AcksOwed { owed: 1 }),
            "astream's typed refusal, naming the ack owed: {fetched}"
        );
        assert_eq!(fetched.kind(), io::ErrorKind::ResourceBusy);
        let committed = link
            .client
            .commit("/f/f/owed/g", 0)
            .expect_err("a commit behind an owed ack is refused");
        assert!(
            astream_broker::acks_owed(&committed).is_some(),
            "{committed}"
        );

        let (owed, answer) = link.recv().expect("A's ack is still owed");
        let (a_at, deduped) = answer.expect("A's own ack, not a fetch's rows");
        assert_eq!((owed.tag, deduped), (0, false));
        let conn = link.ready().expect("nothing owed");
        let (b_at, _) = conn
            .publish(7, 2, "/f/f/owed/b", b"b")
            .expect("in step: nothing the refusals wrote is on the stream");
        assert_eq!(b_at, a_at + 1);
        let (rows, _) = conn
            .fetch(0, "/f/f/owed/*", 64)
            .expect("NEGATIVE CONTROL: the same fetch, nothing owed, is answered");
        assert_eq!(
            rows.iter().map(|(off, _, _)| *off).collect::<Vec<_>>(),
            [a_at, b_at]
        );
    }

    /// **A PIPELINED COMMIT IS OWED LIKE A PUBLISH, IN ORDER, AND A TRY WAITS
    /// FOR NOTHING** — on a real broker.
    #[test]
    fn a_pipelined_commit_is_owed_in_order_and_a_try_waits_for_nothing() {
        let dir = aterm_tempfile::Builder::new()
            .prefix("al-p")
            .tempdir_in("/tmp")
            .expect("a scratch dir");
        let socket = dir.path().join("b.sock");
        let broker = crate::test_broker(dir.path().join("b.log")).expect("a broker");
        let _server = broker.serve(&socket).expect("serve");
        let dial = || {
            crate::transport::connect(
                &socket.to_string_lossy(),
                std::time::Duration::from_secs(60),
            )
            .expect("dial")
            .0
        };
        let mut link: Link<&str> = Link::from(dial());
        let (group, filter) = ("/f/f/cur/n/node/inbox", "/f/f/c/*");

        link.send(7, 1, true, "/f/f/c/a", b"a", "a", Instant::now())
            .map_err(|(e, _)| e)
            .expect("A written");
        let (owed, answer) = link.recv().expect("A's ack");
        let (a_at, _) = answer.expect("A landed");
        assert_eq!(owed.tag, "a");

        let held = broker.pause_next_acks();
        link.send_commit(group, a_at, "commit", Instant::now())
            .map_err(|(e, _)| e)
            .expect("the commit written");
        assert!(held.wait(), "the commit's batch is held unanswered");
        link.send(7, 2, true, "/f/f/c/b", b"b", "b", Instant::now())
            .map_err(|(e, _)| e)
            .expect("B written behind the commit");
        assert_eq!(link.unacked(), 2);
        assert!(link.ready().is_none(), "nothing reads past an owed commit");
        let blocking = link
            .client
            .commit(group, a_at)
            .expect_err("NEGATIVE CONTROL: the blocking commit behind owed acks");
        assert!(
            astream_broker::acks_owed(&blocking).is_some(),
            "refused, nothing sent: {blocking}"
        );
        assert!(
            link.try_recv().is_none(),
            "the commit's answer is held: a try does not wait for it"
        );
        assert_eq!(link.unacked(), 2, "a try that found nothing pays nothing");
        held.resume();

        let (owed, answer) = link.recv().expect("the commit's ack is owed first");
        assert_eq!(owed.tag, "commit");
        assert_eq!((owed.seq, owed.first), (0, false));
        let (commit_at, deduped) = answer.expect("the commit landed");
        assert!(!deduped);
        let (owed, answer) = link.recv().expect("B's ack after it");
        assert_eq!(owed.tag, "b");
        let (b_at, _) = answer.expect("B landed");
        assert!(
            a_at < commit_at && commit_at < b_at,
            "in send order: A @{a_at}, the commit @{commit_at}, B @{b_at}"
        );
        assert!(link.try_recv().is_none(), "nothing owed: nothing tried");

        let first = |group: &str| {
            let mut sub = dial().subscribe_group(group, filter).expect("subscribe");
            sub.recv().expect("a delivery").expect("not the end").0
        };
        assert_eq!(first(group), b_at, "the commit moved the group past A");
        assert_eq!(
            first("/f/f/cur/m/node/inbox"),
            a_at,
            "NEGATIVE CONTROL: a group nobody committed starts at A"
        );
    }

    /// **A COMMIT AND A BURST OF PUBLISHES GO IN ONE WRITE AND ONE BATCH, THE COMMIT FIRST**
    /// (critic item C13) — on a real broker: their acks come back in the burst's order, the
    /// commit's offset below every publish's, all in one writer batch, and the group resumes
    /// past what the commit named. A burst with no commit owes its publishes alone.
    #[test]
    fn a_commit_and_a_burst_of_publishes_go_in_one_write_and_one_batch() {
        let dir = aterm_tempfile::Builder::new()
            .prefix("al-p")
            .tempdir_in("/tmp")
            .expect("a scratch dir");
        let socket = dir.path().join("b.sock");
        let broker = crate::test_broker(dir.path().join("b.log")).expect("a broker");
        let _server = broker.serve(&socket).expect("serve");
        let dial = || {
            crate::transport::connect(
                &socket.to_string_lossy(),
                std::time::Duration::from_secs(60),
            )
            .expect("dial")
            .0
        };
        let mut link: Link<&str> = Link::from(dial());
        let (group, filter) = ("/f/f/cur/n/node/inbox", "/f/f/c/*");
        link.send(7, 1, true, "/f/f/c/a", b"a", "a", Instant::now())
            .map_err(|(e, _)| e)
            .expect("A written");
        let (_, answer) = link.recv().expect("A's ack");
        let (a_at, _) = answer.expect("A landed");

        let publishes = ["s1", "s2", "s3"]
            .into_iter()
            .zip(2u64..)
            .map(|(tag, seq)| Publish {
                producer_id: 7,
                seq,
                first: true,
                subject: "/f/f/pub/n/s/ev",
                body: tag.as_bytes(),
                tag,
            })
            .collect();
        let batches = broker.stats().writer_batches;
        link.send_burst(
            Some(HeldCommit {
                group,
                upto: a_at,
                tag: "commit",
            }),
            publishes,
            Instant::now(),
        )
        .map_err(|unsent| unsent.error)
        .expect("the burst written");
        assert_eq!(link.unacked(), 4, "the commit and three publishes owed");
        let answered: Vec<(&str, u64)> = std::iter::from_fn(|| link.recv())
            .map(|(owed, answer)| (owed.tag, answer.expect("landed").0))
            .collect();
        assert_eq!(
            answered.iter().map(|(tag, _)| *tag).collect::<Vec<_>>(),
            ["commit", "s1", "s2", "s3"],
            "in the burst's order"
        );
        assert!(
            answered.windows(2).all(|w| w[0].1 < w[1].1),
            "the commit below every publish: {answered:?}"
        );
        assert_eq!(
            broker.stats().writer_batches - batches,
            1,
            "one writer batch for the whole burst"
        );
        let mut sub = dial().subscribe_group(group, filter).expect("subscribe");
        link.send_burst(
            None,
            vec![Publish {
                producer_id: 7,
                seq: 5,
                first: true,
                subject: "/f/f/c/b",
                body: b"b",
                tag: "b",
            }],
            Instant::now(),
        )
        .map_err(|unsent| unsent.error)
        .expect("B written alone");
        let (owed, answer) = link.recv().expect("B's ack, and no other");
        assert_eq!(owed.tag, "b");
        let (b_at, _) = answer.expect("B landed");
        assert_eq!(
            sub.recv().expect("a delivery").expect("not the end").0,
            b_at,
            "the commit moved the group past A"
        );
    }

    /// **A RIDER COMES BACK WITH THE ACK IT WAS HUNG ON, AND ONLY THEN.** It
    /// hangs on the newest owed ack its predicate names — not on a newer one
    /// it does not — keeps the larger of two values, comes back with that
    /// ack's answer and with no other, and finds nothing to hang on while
    /// only acks it does not name are owed.
    #[test]
    fn a_rider_comes_back_with_the_ack_it_was_hung_on() {
        let (_dir, _server, mut link, _reader) = served();
        let send = |link: &mut Link<u32>, tag: u32| {
            link.send(
                7,
                u64::from(tag),
                true,
                &format!("/f/f/r/{tag}"),
                b"b",
                tag,
                Instant::now(),
            )
            .map_err(|(e, _)| e)
            .expect("written");
        };
        assert!(!link.ride(1, |_| true), "nothing owed: nothing to wait on");
        send(&mut link, 2);
        send(&mut link, 4);
        send(&mut link, 5);
        assert!(link.ride(10, |tag| tag % 2 == 0));
        assert!(link.ride(9, |tag| tag % 2 == 0), "the larger value stays");
        assert!(link.riding());
        let riders: Vec<(u32, Option<u64>)> = std::iter::from_fn(|| link.recv())
            .map(|(owed, answer)| {
                answer.expect("landed");
                (owed.tag, owed.rider)
            })
            .collect();
        assert_eq!(
            riders,
            [(2, None), (4, Some(10)), (5, None)],
            "on the newest ack it waits on, not on the newer one it does not"
        );
        assert!(!link.riding());
        send(&mut link, 7);
        assert!(
            !link.ride(11, |tag| tag % 2 == 0),
            "only an ack it does not wait on is owed"
        );
        send(&mut link, 8);
        assert!(link.ride(12, |tag| tag % 2 == 0));
    }

    /// **A REFUSED BATCH LANDS THE PUBLISH SENT AFTER IT** on a broker that does not gap-fence —
    /// the hazard that makes a caller whose records must land in its own order keep one of
    /// them in flight at a time there (`Bridge::settle_sender`), shown on a REAL Strict broker
    /// rather than a scripted one. Its `Hello` does not say it gap-fences.
    #[test]
    fn a_refused_batch_lands_the_publish_sent_after_it() {
        let dir = aterm_tempfile::Builder::new()
            .prefix("al-p")
            .tempdir_in("/tmp")
            .expect("a scratch dir");
        let socket = dir.path().join("b.sock");
        let broker = astream_broker::Broker::open(dir.path().join("b.log")).expect("a broker");
        let _server = broker.serve(&socket).expect("serve");
        let (conn, _) = crate::transport::connect(
            &socket.to_string_lossy(),
            std::time::Duration::from_secs(60),
        )
        .expect("dial");
        let mut link: Link<char> = Link::from(conn);
        link.ready()
            .expect("nothing owed")
            .hello()
            .expect("a Hello");
        assert!(!link.gap_fences(), "its Hello says nothing of a fence");
        let (a, b) = ("/f/f/order/a", "/f/f/order/b");

        broker.inject_faults(astream_broker::InjectedFaults {
            sync: true,
            truncate: false,
        });
        let held = broker.pause_next_acks();
        link.send(7, 1, true, a, b"a", 'a', Instant::now())
            .map_err(|(e, _)| e)
            .expect("A written");
        assert!(held.wait(), "A's batch was tried and is held unanswered");
        broker.inject_faults(astream_broker::InjectedFaults::default());
        link.send(7, 2, true, b, b"b", 'b', Instant::now())
            .map_err(|(e, _)| e)
            .expect("B written while A's answer is held");
        held.resume();

        let (owed, answer) = link.recv().expect("A's ack is owed");
        assert_eq!(owed.tag, 'a');
        let refused = answer.expect_err("A's batch failed its fsync");
        assert_eq!(
            astream_broker::broker_refusal(&refused).map(|r| r.code),
            Some(astream_broker::proto::ERR_REFUSED),
            "refused, and not known to be for good: {refused}"
        );
        let (owed, answer) = link.recv().expect("B's ack is owed");
        assert_eq!(owed.tag, 'b');
        let (b_at, deduped) = answer.expect("B landed after A's refusal");
        assert!(!deduped);

        let conn = link.ready().expect("nothing owed");
        let (a_at, deduped) = conn.publish(7, 1, a, b"a").expect("A's retry lands");
        assert!(!deduped, "A never landed: its retry appends");
        assert!(
            a_at > b_at,
            "A was sent first and is on the log after B (@{a_at} > @{b_at})"
        );
    }

    /// **A GAP-FENCED BROKER REFUSES THE PUBLISH SENT AFTER A REFUSED BATCH**, and its producer
    /// lifts the fence once it has read both answers: then the refused one's retry lands first
    /// and the later one's after it, in send order. The broker offers it in its `Hello` and the
    /// link asks for it ([`Link::gap_fences`]), which is what lets the bridge keep several of a
    /// sender's posts in flight; the control is [`a_refused_batch_lands_the_publish_sent_after_it`]'s broker,
    /// opened without the fence, whose `Hello` says nothing.
    #[test]
    fn a_gap_fenced_broker_holds_the_publish_sent_after_a_refused_batch() {
        let dir = aterm_tempfile::Builder::new()
            .prefix("al-g")
            .tempdir_in("/tmp")
            .expect("a scratch dir");
        let socket = dir.path().join("b.sock");
        let broker = astream_broker::Broker::open_opts(
            dir.path().join("b.log"),
            astream_broker::OpenOptions::new(astream_broker::Durability::Strict)
                .with_sticky_batch_refusal(true),
            || Ok(astream_broker::Prepared::Nothing),
        )
        .expect("a broker");
        let _server = broker.serve(&socket).expect("serve");
        let (conn, _) = crate::transport::connect(
            &socket.to_string_lossy(),
            std::time::Duration::from_secs(60),
        )
        .expect("dial");
        let mut link: Link<char> = Link::from(conn);
        link.ready()
            .expect("nothing owed")
            .hello()
            .expect("a Hello");
        assert!(
            !link.gap_fences(),
            "its Hello offers the fence, and the link has not asked"
        );
        link.ask_gap_fences().expect("the link asks for the fence");
        assert!(link.gap_fences(), "it asked where the Hello offered it");
        let (a, b) = ("/f/f/order/a", "/f/f/order/b");

        broker.inject_faults(astream_broker::InjectedFaults {
            sync: true,
            truncate: false,
        });
        let held = broker.pause_next_acks();
        link.send(7, 1, true, a, b"a", 'a', Instant::now())
            .map_err(|(e, _)| e)
            .expect("A written");
        assert!(held.wait(), "A's batch was tried and is held unanswered");
        broker.inject_faults(astream_broker::InjectedFaults::default());
        link.send(7, 2, true, b, b"b", 'b', Instant::now())
            .map_err(|(e, _)| e)
            .expect("B written while A's answer is held");
        held.resume();

        let (owed, answer) = link.recv().expect("A's ack is owed");
        assert_eq!(owed.tag, 'a');
        answer.expect_err("A's batch failed its fsync");
        link.note_refused();
        assert!(link.fenced());
        let (owed, answer) = link.recv().expect("B's ack is owed");
        assert_eq!(owed.tag, 'b');
        let fenced = answer.expect_err("B is held behind A's refusal");
        assert_eq!(
            astream_broker::broker_refusal(&fenced)
                .and_then(|r| astream_broker::sticky_fence::parse_gap_fenced_seq(&r.msg)),
            Some(1),
            "refused behind A's sequence: {fenced}"
        );

        link.lift_gap_fences()
            .expect("nothing owed")
            .expect("the broker lifts the fence");
        assert!(!link.fenced());
        let conn = link.ready().expect("nothing owed");
        let (a_at, deduped) = conn.publish(7, 1, a, b"a").expect("A's retry lands");
        assert!(!deduped, "A never landed: its retry appends");
        let (b_at, deduped) = conn.publish(7, 2, b, b"b").expect("B's retry lands");
        assert!(!deduped, "B never landed: its retry appends");
        assert!(a_at < b_at, "A is on the log before B (@{a_at} < @{b_at})");
    }
}
