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

    /// Ask the broker to SETTLE its log's device WITHOUT waiting for the answer (astream's
    /// `Client::send_settle`, critic item F07), and queue `tag` for it: a full flush its writer
    /// issues once it took the request, answered in send order with the durable head after it.
    /// # Errors
    /// The send's, with the tag.
    pub fn send_settle(&mut self, tag: T, now: Instant) -> Result<(), (io::Error, T)> {
        match self.client.send_settle() {
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
#[path = "pipeline_tests.rs"]
mod tests;
