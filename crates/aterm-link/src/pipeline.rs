// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE PUBLISHER CONNECTION, WITH THE ACKS IT HAS NOT READ YET.

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

/// The publisher connection and the acks owed on it, in send order.
pub(crate) struct Link<T> {
    client: Conn,
    unacked: VecDeque<Unacked<T>>,
    /// The most acks this connection has had unread at once: what a test reads
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

    /// Write one publish WITHOUT waiting for its ack, and queue `tag` for the answer.
    /// # Errors
    /// The send's, with the tag.
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
        match self.client.send_publish(producer_id, seq, subject, body) {
            Ok(()) => {
                self.owe(Unacked {
                    seq,
                    first,
                    sent: now,
                    tag,
                    rider: None,
                });
                Ok(())
            }
            Err(e) => Err((e, tag)),
        }
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
    #[test]
    fn a_burst_is_sent_before_any_ack_and_every_answer_is_its_own() {
        let (_dir, _server, mut link, mut reader) = served();
        for i in 0..10u32 {
            let subject = if i == 4 {
                // empty-segment".to_string()
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

    /// **A RIDER COMES BACK WITH THE ACK IT WAS HUNG ON, AND ONLY THEN.** It
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

    /// **A REFUSED BATCH LANDS THE PUBLISH SENT AFTER IT** — the hazard that
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
}
