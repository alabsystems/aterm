// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE PUBLISHER CONNECTION, WITH THE ACKS IT HAS NOT READ YET.
//!
//! A blocking `Publish` costs one round trip and, on the broker's `Strict` log,
//! one `fsync` of its own: the broker's group commit folds every write it finds
//! queued into one flush, and a connection that waits for each ack before
//! sending the next never has a second write queued. astream's client can keep
//! many publishes in flight on one connection — `Client::send_publish` now,
//! `Client::recv_publish_ack` later, in the SAME order, because the broker
//! answers a connection's writes in the order it read them — and those share
//! one group-commit flush.
//!
//! The price is a rule the client states and cannot enforce: every other
//! request reads ITS reply as the next frame (`Fetch`, `Last`, `Commit`,
//! `Will`, `Budget`, `Keepalive`, a blocking `Publish`), so it may be sent only
//! once every pipelined ack ahead of it has been read. A request sent with an
//! ack unread would take that ack for its own reply — a `Fetch` that answers
//! "no rows", a commit that answers some publish's offset.
//!
//! [`Link`] is that rule as a type. It owns the client and the queue of unread
//! acks, each carrying what its sender wants done with the answer (`T`), and
//! it hands the client out for a request that reads its own reply only while
//! the queue is empty ([`Link::ready`]). There is no other way to reach the
//! client mutably, so a request cannot be sent past an unread ack by a path
//! that forgot to read it: it finds no connection and fails as an unreachable
//! broker does. And the queue lives and dies with the connection — a dropped
//! connection drops the acks owed on it, so they can never be read off the
//! NEXT connection, which owes them nothing.
//!
//! WHAT IT DOES NOT PROMISE is that a refusal is sticky. The broker answers a
//! connection's writes in order, but a write it refuses for a reason not known
//! to be permanent — an append or `fsync` that failed — is refused with its
//! BATCH, which the broker rolls back before it goes on to the next one. So
//! with two publishes in flight the first can be refused and the second land
//! (`a_refused_batch_lands_the_publish_sent_after_it`, on a real broker), and a
//! caller whose records must land in its own order keeps one of them in flight
//! at a time (`Bridge::settle_sender`, for a sender's posts).

use std::collections::VecDeque;
use std::io;
use std::ops::Deref;
use std::time::Instant;

use crate::transport::Conn;

/// One pipelined publish whose ack has not been read.
#[derive(Debug)]
pub(crate) struct Unacked<T> {
    /// The producer sequence it was sent under.
    pub seq: u64,
    /// Whether this was the FIRST send of a sequence just reserved — the one
    /// whose deduped answer is another writer's record, not a retry.
    pub first: bool,
    /// When it was written to the connection.
    pub sent: Instant,
    /// What its sender wants done with the answer.
    pub tag: T,
    /// A number hung on this ack AFTER it was sent ([`Link::ride`]), handed
    /// back with its answer: the caller learns that what it was waiting on —
    /// this publish and every one before it — has been answered on a
    /// connection still in step.
    pub rider: Option<u64>,
}

/// One owed publish and the broker's answer to it: `(offset, deduped)`, or the
/// refusal or transport failure ([`Link::recv`]).
pub(crate) type Settled<T> = (Unacked<T>, io::Result<(u64, bool)>);

/// The publisher connection and the acks owed on it, in send order.
pub(crate) struct Link<T> {
    client: Conn,
    unacked: VecDeque<Unacked<T>>,
    /// The most acks this connection has had unread at once: what a test reads
    /// to know a burst went out without waiting on each answer. Kept by test
    /// builds alone — nothing in the bridge reads it.
    #[cfg(test)]
    peak: usize,
}

impl<T> From<Conn> for Link<T> {
    fn from(client: Conn) -> Self {
        Link {
            client,
            unacked: VecDeque::new(),
            #[cfg(test)]
            peak: 0,
        }
    }
}

/// Read-only access, for what asks the client nothing over the wire
/// (`seen_log_identity`). Nothing here reaches it MUTABLY: see [`Link::ready`].
impl<T> Deref for Link<T> {
    type Target = Conn;
    fn deref(&self) -> &Conn {
        &self.client
    }
}

impl<T> Link<T> {
    /// The client, for a request that reads its own reply — ONLY while no
    /// pipelined ack is unread, so that reply is the next frame. `None`
    /// otherwise: the caller reads the owed acks first ([`Link::recv`]).
    pub fn ready(&mut self) -> Option<&mut Conn> {
        self.unacked.is_empty().then_some(&mut self.client)
    }

    /// Write one publish WITHOUT waiting for its ack, and queue `tag` for the
    /// answer. A publish that could not be written gives `tag` back with the
    /// error: nothing is owed for it. A write that failed part-way latched the
    /// client out of step (astream's `Client::send`), so every later request on
    /// it fails too; one refused before a byte was written (a frame over the
    /// cap) leaves it in step.
    ///
    /// # Errors
    ///
    /// The send's, with the tag.
    pub fn send(
        &mut self,
        producer_id: u64,
        seq: u64,
        first: bool,
        subject: &str,
        body: &[u8],
        tag: T,
    ) -> Result<(), (io::Error, T)> {
        match self.client.send_publish(producer_id, seq, subject, body) {
            Ok(()) => {
                self.unacked.push_back(Unacked {
                    seq,
                    first,
                    sent: Instant::now(),
                    tag,
                    rider: None,
                });
                #[cfg(test)]
                {
                    self.peak = self.peak.max(self.unacked.len());
                }
                Ok(())
            }
            Err(e) => Err((e, tag)),
        }
    }

    /// Read the OLDEST owed ack, answering it with the publish it belongs to —
    /// `None` when nothing is owed. A refused publish is an `Err` carrying the
    /// broker's refusal ([`astream_broker::BrokerRefusal`]) and leaves the
    /// connection in step for the next ack; a transport failure leaves it out of
    /// step, and every later `recv` fails at once.
    pub fn recv(&mut self) -> Option<Settled<T>> {
        let owed = self.unacked.pop_front()?;
        let answer = self.client.recv_publish_ack();
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

    /// When the NEWEST owed publish was written — the last frame this
    /// connection sent, because a request that reads its own reply is sent
    /// only while nothing is owed ([`Link::ready`]) and so always before every
    /// publish still owed. `None` when nothing is owed.
    pub fn last_sent(&self) -> Option<Instant> {
        self.unacked.back().map(|owed| owed.sent)
    }

    /// Hang `value` on the NEWEST owed ack whose tag `waits_on` names, to come
    /// back with that ack's answer ([`Unacked::rider`]); a rider already there
    /// keeps the larger of the two. Acks are read oldest first, so the rider
    /// comes back only once every owed ack `waits_on` names up to it has been
    /// answered too — and never, if the connection drops first, because the
    /// queue drops with it.
    ///
    /// `false` when no owed ack is one `waits_on` names: nothing the caller
    /// waits on is still unanswered, and nothing was hung.
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
}

impl<T> Drop for Link<T> {
    /// A connection dropped with acks unread: what they answered is unknown.
    /// Each record is on the broker or not, exactly as a blocking publish whose
    /// connection failed mid-request would leave it, and each caller already
    /// owns that outcome (a presence row is republished by the next attach, a
    /// halt ack stays owed until one lands, an `ev` is a digest line, a post
    /// stays queued under its pinned sequence and in flight, and its retry is
    /// a dedup the broker answers with the record it holds). Said once,
    /// on stderr, so the count is not lost with the queue. A rider hung on one
    /// of them ([`Link::ride`]) is never handed back: what it waited on stays
    /// unproven, which is the point of hanging it there.
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
        // Below the Unix sockaddr bound, whatever the runner's TMPDIR.
        let dir = aterm_tempfile::Builder::new()
            .prefix("al-p")
            .tempdir_in("/tmp")
            .expect("a scratch dir");
        let socket = dir.path().join("b.sock");
        let broker = astream_broker::Broker::open(dir.path().join("b.log")).expect("a broker");
        let server = broker.serve(&socket).expect("serve");
        let connect = || {
            let (conn, _) = crate::transport::connect(
                &crate::transport::Transport::Unix,
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
    /// sees all ten. A refused publish in the middle (a subject the broker
    /// refuses) is ITS answer alone — the one behind it still lands and is
    /// still attributed to its own tag.
    #[test]
    fn a_burst_is_sent_before_any_ack_and_every_answer_is_its_own() {
        let (_dir, _server, mut link, mut reader) = served();
        for i in 0..10u32 {
            let subject = if i == 4 {
                "/f/f//empty-segment".to_string()
            } else {
                format!("/f/f/x/{i}")
            };
            link.send(7, u64::from(i) + 1, true, &subject, b"b", i)
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

    /// **A RIDER COMES BACK WITH THE ACK IT WAS HUNG ON, AND ONLY THEN.** It
    /// hangs on the newest owed ack its predicate names — not on a newer one
    /// it does not — keeps the larger of two values, comes back with that
    /// ack's answer and with no other, and finds nothing to hang on while
    /// only acks it does not name are owed.
    #[test]
    fn a_rider_comes_back_with_the_ack_it_was_hung_on() {
        let (_dir, _server, mut link, _reader) = served();
        let send = |link: &mut Link<u32>, tag: u32| {
            link.send(7, u64::from(tag), true, &format!("/f/f/r/{tag}"), b"b", tag)
                .map_err(|(e, _)| e)
                .expect("written");
        };
        assert!(!link.ride(1, |_| true), "nothing owed: nothing to wait on");
        // Even tags are the ones the rider waits on.
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

    /// **A REFUSED BATCH LANDS THE PUBLISH SENT AFTER IT** — the hazard that
    /// makes a caller whose records must land in its own order keep one of
    /// them in flight at a time (`Bridge::settle_sender`), shown on a REAL
    /// Strict broker rather than a scripted one. Every `fsync` fails (the
    /// broker's own `inject_faults` seam), so A's batch is rolled back; the
    /// writer is then held before it answers that batch (`pause_next_acks`,
    /// armed before A is sent, the log lock released), the fault is lifted,
    /// and only then is B sent — so B can only be in a LATER batch, and that
    /// one commits. A is refused `ERR_REFUSED` — not known to be permanent, so
    /// a sender retries it — B lands on a connection still in step, and A's
    /// retry at its own sequence lands BEHIND B: the order the two were sent
    /// in is not the order on the log. Every step is a rendezvous with the
    /// writer, never a race; the claim is in offsets.
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
            &crate::transport::Transport::Unix,
            &socket.to_string_lossy(),
            std::time::Duration::from_secs(60),
        )
        .expect("dial");
        let mut link: Link<char> = Link::from(conn);
        let (a, b) = ("/f/f/order/a", "/f/f/order/b");

        // Every fsync fails until the fault is lifted; the writer is held
        // after A's batch, with the log lock released, before it answers.
        broker.inject_faults(astream_broker::InjectedFaults {
            sync: true,
            truncate: false,
        });
        let held = broker.pause_next_acks();
        link.send(7, 1, true, a, b"a", 'a')
            .map_err(|(e, _)| e)
            .expect("A written");
        assert!(held.wait(), "A's batch was tried and is held unanswered");
        broker.inject_faults(astream_broker::InjectedFaults::default());
        link.send(7, 2, true, b, b"b", 'b')
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
}
