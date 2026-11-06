// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE BUS MIRROR, BRIDGE HALF, its wiring (the pure parts are [`crate::mirror`]).

use std::collections::BTreeSet;
use std::io;
use std::time::{Duration, Instant};

use super::{
    past_the_budget, refused_for_good, Bridge, Opens, ReadAt, Then, DEADLINE_RETRY, IDLE_ACK_GRACE,
    LINK_MIRROR_WAIT_LANE, PIPELINE_WINDOW,
};
use crate::ctl::Reply;
use crate::mirror::{self, Bucket, LandedWord, LogMode, Pin, PinSlot};
use crate::pipeline::{HeldCommit, Publish};
use crate::state::StateDir;
use crate::subject;
use aterm_types::fabric_grammar::{MirrorReason, MirrorState};

/// How long a batch none of whose slots could land (every session in it refused by the subject budget) waits before the next take offers them again: the budget is asked again then, never every round.
pub(super) const REFUSED_WAIT: Duration = Duration::from_secs(2);

/// How long a take that could take nothing NOW waits before the next one asks: the rate bucket empty, or the endpoint's answer empty with `more=1` (a session's timeline lock was held at the take, which never waits on the UI's lock).
pub(super) const BUSY_WAIT: Duration = Duration::from_millis(100);

/// The room under [`PIPELINE_WINDOW`] a mirror burst waits for: a slot and the inbox commit it may carry ahead of it ([`Bridge::mirror_send`]), so a pass that starts at the window's edge still carries the commit rather than sending its slots one bare write each.
const BURST_ROOM: usize = 2;

/// What became of one pinned slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Outcome {
    /// Not sent, or to send again.
    Owed,
    /// Sent; its ack is owed.
    Sending,
    /// Appended at this offset.
    OnBus(u64),
    /// Deduped at this offset: read back before it counts.
    Deduped(u64),
    /// Deduped and read back: ours.
    Verified(u64),
    /// The node's subject budget refused the session's `ev` face.
    Refused,
    /// Another writer's record holds the pinned sequence: owed its re-pin.
    Collided,
}

impl Outcome {
    /// What came of the slot is final for the batch's landing.
    const fn concluded(self) -> bool {
        !matches!(self, Outcome::Owed | Outcome::Sending | Outcome::Collided)
    }
}

/// How long the first pass of an open after an attach is held at most while the open has yet to
/// make the pin's fence ([`Bridge::mirror_yields`], critic item D06). What an attach or a
/// handoff owes a delivery is on its way by then: a post the loop's first drain published to a
/// session this node hosts comes back on the inbox face once the broker has taken it, an
/// ordering barrier later (a pong's acked -> visible read 0.04 / 0.12 ms at p50 / p99 in ping-pong,
/// `2026-11-04-fabric-504`).
/// The mirror has no deadline, so this is at most how much later it starts.
pub(super) const FIRST_PASS_HOLD: Duration = Duration::from_millis(5);

/// THE FIRST PASS OF AN OPEN AFTER AN ATTACH (critic item D06): whether the loop holds it back
/// for the deliveries the attach owes while this open has yet to make the pin's fence durable
/// ([`Bridge::make_the_pin_fence`]: two full flushes of the bridge's own, or, where the broker's
/// log is on the state dir's device, two barriers and the broker's settle, critic item F07), so
/// the first delivery after an attach or a handoff does not wait behind the fence
/// ([`Bridge::mirror_yields`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FirstPass {
    /// Nothing holds a pass back.
    Free,
    /// An attach came up ([`Bridge::mirror_attached`]) and its first pass has yet to come due;
    /// `drained`: how many posts the loop's drains published since to a session this node hosts
    /// ([`Bridge::mirror_drained_post`]), each a record that comes back on the inbox face.
    Attached { drained: u64 },
    /// The pass is held until `until`, while `awaits` records are still to come back (each
    /// inbox record the loop takes counts one off, [`Bridge::mirror_took_inbox`]) or an item is
    /// queued; the first round that finds neither ends it.
    Held { until: Instant, awaits: u64 },
}

/// The mirror's state in a bridge whose `[fabric] log` is `"turns"` or `"auto"`.
pub(super) struct MirrorRun {
    /// `Turns`, or `Auto` (on while the broker retains).
    mode: LogMode,
    /// The endpoint answered the hello's `log=turns`.
    acked: bool,
    /// The pin is not judged yet: the recovery an unclean stop owes, which
    /// reconciles it, is not complete ([`Bridge::mirror_hello_answered`]).
    awaiting: bool,
    /// An `auto` mirror over a broker that retains nothing, said once a
    /// change ([`MirrorState::Unretained`]).
    unretained: bool,
    /// Why it stopped for this bridge's life, if it did.
    stopped: Option<String>,
    /// Work is owed: a take, a send or a landing.
    owed: bool,
    /// A busy answer (`fenced`, `adopting`, an empty bucket) waits until then.
    retry_at: Option<Instant>,
    /// A snapshot the endpoint held back to its rate (`due=`, a moved mail count) is due then:
    /// a take is owed at that instant though nothing appends to wake one. Until then a take
    /// owed only by wakes of the sessions that hold covers ([`MirrorRun::held`]) waits for an
    /// owed inbox commit's write ([`Bridge::mirror_behind_the_commit`]).
    held_back: Option<Instant>,
    /// The sessions whose snapshot the endpoint's last empty answer held back to its rate
    /// (`held=`, [`mirror::Taken::held`]), each until [`MirrorRun::held_back`] at the soonest.
    /// The hold is per session: any other session's snapshot is in a take as soon as its log
    /// owes one (critic item E04c's review).
    held: BTreeSet<String>,
    /// A wake since that answer that the hold may not cover: an endpoint event of a session
    /// [`MirrorRun::held`] does not name, or a wake of no session (a gap, an attach, the
    /// roster's backstop). The take it owes may carry a snapshot, so it does not wait for the
    /// commit's write ([`Bridge::mirror_woken_by`], critic item E04c's review).
    unheld: bool,
    /// When the loop first held a take: one nothing would carry waits from then for an inbox
    /// commit to fall due, at most [`IDLE_ACK_GRACE`] ([`Bridge::mirror_rides_nothing`], critic
    /// item E04). A take inside the endpoint's rate hold that waited for a commit's write
    /// ([`Bridge::mirror_behind_the_commit`], critic item E04c) is dated from then too, so once
    /// that commit is written it is not held a grace more for another to fall due.
    uncarried: Option<Instant>,
    /// The inbox offset whose commit a take inside the endpoint's rate hold waits to see
    /// written: what [`Bridge::inbox_commit_due`] named when the pass first waited
    /// ([`Bridge::mirror_behind_the_commit`], critic item E04c).
    behind: Option<u64>,
    /// The last take left a backlog at the endpoint: it said `more=1`, or came back with every
    /// slot it asked. The take after its landing drains that backlog, so it is never held for a
    /// commit to carry ([`Bridge::mirror_rides_nothing`], critic item E04's review).
    drains: bool,
    /// The broker's answer to the pin's settle made this open's fence, and no pass has taken
    /// since ([`Bridge::pin_settled`], critic item F07's review). The take it lets go is the
    /// open's first, which E04's hold does not hold ([`Bridge::mirror_rides_nothing`]), as a
    /// pass that made its fence by its own full flushes takes behind them at once.
    fence_settled: bool,
    /// The batch in flight and the node's total.
    pin: Pin,
    /// Per slot: whether THIS process may have sent it (a never-sent slot's
    /// deduped answer is another writer's): every slot of a burst it wrote,
    /// the write taken or not, since a burst cut short lands each frame the
    /// broker received whole ahead of the cut ([`Bridge::mirror_send`]).
    sent: Vec<bool>,
    /// Per slot: the offset a deduped answer named whose record the read-back
    /// could not see on the slot's face ([`Bridge::mirror_settle`]): the slot
    /// is sent again, and a second dedup naming the same offset is another
    /// writer's record.
    unread: Vec<Option<u64>>,
    /// Per slot: what came of it.
    outcome: Vec<Outcome>,
    /// The incarnation the `Sending` slots went out under: a reattach drops
    /// their acks with the connection, and they are sent again.
    sent_inc: u64,
    /// A collision re-pins once the connection is back.
    repin: bool,
    /// The rate bucket and its clock's origin.
    bucket: Bucket,
    clock: Instant,
    /// The alias key, in hex for the take (empty: none, every
    /// edge `-`); the pin carries it ([`Pin::key`]).
    key: String,
    /// Whether `logkey` holds [`Pin::key`] on the media (or there is no key
    /// to hold): no take names the key, and no slot is sent, before it does.
    key_durable: bool,
    /// What `logfloor` holds on the media, as this run knows it:
    /// at or above every body byte the node sent.
    floor: u64,
    /// Whether `floor` covers the batch's bytes, so its slots may be sent.
    floored: bool,
    /// Sessions whose `ev` face the budget refused, said once.
    noticed: BTreeSet<String>,
    /// Recovery counts: slots republished from a pin this process
    /// did not take, and how many of those the broker deduped.
    recovered: u64,
    recovered_deduped: u64,
    /// The most slots a take asks ([`mirror::BATCH_MAX`]; a test's one).
    batch: usize,
    /// The broker's `oldest=` for this node's `ev` faces as last read:
    /// it only rises, so a window it counts under the budget is.
    oldest: u64,
    /// The window budget holds the take: said once to the endpoint
    /// ([`MirrorState::Budget`]) until the mirror goes on ([`MirrorState::On`]).
    waiting: bool,
    /// The window budget, in body bytes: [`mirror::NODE_WINDOW_BYTES`] (a simulation's
    /// scaled one, `Bridge::simulate_window`).
    window: u64,
    /// Whether the first pass after an attach is held back ([`FirstPass`]).
    first_pass: FirstPass,
    /// How long a first pass is held at most: [`FIRST_PASS_HOLD`] (a test's own,
    /// [`MirrorRun::hold_first_pass_for_tests`]).
    hold: Duration,
}

impl MirrorRun {
    /// The state a bridge starts with: `None` with the mirror off.
    pub(super) fn new(
        mode: LogMode,
        state: &StateDir,
        now: Instant,
        mint: impl FnOnce() -> Option<[u8; 16]>,
    ) -> Option<Box<MirrorRun>> {
        (mode != LogMode::Off).then(|| {
            let key = state
                .logpin()
                .ok()
                .flatten()
                .and_then(|t| Pin::parse(&t).ok())
                .and_then(|p| p.key)
                .or_else(mint);
            Box::new(MirrorRun {
                mode,
                acked: false,
                awaiting: false,
                unretained: false,
                stopped: None,
                owed: false,
                retry_at: None,
                held_back: None,
                held: BTreeSet::new(),
                unheld: false,
                uncarried: None,
                behind: None,
                drains: false,
                fence_settled: false,
                pin: Pin {
                    key,
                    ..Pin::default()
                },
                sent: Vec::new(),
                unread: Vec::new(),
                outcome: Vec::new(),
                sent_inc: 0,
                repin: false,
                bucket: Bucket::full(0),
                clock: now,
                key: key.map_or_else(String::new, |k| aterm_codec::hex::encode_lower(&k)),
                key_durable: false,
                floor: 0,
                floored: false,
                noticed: BTreeSet::new(),
                recovered: 0,
                recovered_deduped: 0,
                batch: mirror::BATCH_MAX,
                oldest: 0,
                waiting: false,
                window: mirror::NODE_WINDOW_BYTES,
                first_pass: FirstPass::Free,
                hold: FIRST_PASS_HOLD,
            })
        })
    }

    /// A mirror the endpoint has acknowledged, taking `batch` slots at a time.
    #[cfg(test)]
    pub(super) fn acked_for_tests(state: &StateDir, batch: usize) -> Box<MirrorRun> {
        let mut m = MirrorRun::new(
            LogMode::Turns,
            state,
            Instant::now(),
            mirror::mint_alias_key,
        )
        .unwrap_or_else(|| unreachable!());
        m.acked = true;
        m.owed = true;
        m.batch = batch;
        m
    }

    /// Hold the first pass after an attach for at most `hold` ([`FIRST_PASS_HOLD`] in a bridge).
    #[cfg(test)]
    pub(super) fn hold_first_pass_for_tests(&mut self, hold: Duration) {
        self.hold = hold;
    }

    /// When the hold of a take nothing would carry began ([`Bridge::mirror_rides_nothing`]),
    /// while it holds.
    #[cfg(test)]
    pub(super) fn uncarried(&self) -> Option<Instant> {
        self.uncarried
    }

    /// Whether work is owed: a take, a send or a landing.
    #[cfg(test)]
    pub(super) fn owed(&self) -> bool {
        self.owed
    }

    /// The batch in flight and what came of each slot.
    #[cfg(test)]
    pub(super) fn batch(&self) -> (&[PinSlot], &[Outcome]) {
        (&self.pin.slots, &self.outcome)
    }

    /// Why it stopped, if it did.
    #[cfg(test)]
    pub(super) fn stopped(&self) -> Option<&str> {
        self.stopped.as_deref()
    }

    /// Forget the batch as a buggy bridge would (a negative control's broken call): the pin emptied with nothing landed.
    #[cfg(test)]
    pub(super) fn drop_batch_for_tests(&mut self) {
        self.clear_batch();
    }

    /// The alias key the take names, in hex.
    #[cfg(test)]
    pub(super) fn key(&self) -> &str {
        &self.key
    }

    /// When a busy answer's retry is due, if one waits.
    #[cfg(test)]
    pub(super) fn retry_at(&self) -> Option<Instant> {
        self.retry_at
    }

    /// The node's running total of mirrored bytes.
    #[cfg(test)]
    pub(super) fn total(&self) -> u64 {
        self.pin.total
    }

    /// Set the running total (the cap's test).
    #[cfg(test)]
    pub(super) fn set_total_for_tests(&mut self, total: u64) {
        self.pin.total = total;
    }

    /// The pin's window ledger and the `oldest=` it was last judged by.
    #[cfg(test)]
    pub(super) fn window(&self) -> (&[(u64, u64)], u64) {
        (&self.pin.win, self.oldest)
    }

    /// A landed batch of `bytes` at `hi`, as a settle notes it (the budget's tests).
    #[cfg(test)]
    pub(super) fn note_window_for_tests(&mut self, hi: u64, bytes: u64) {
        self.pin.note_window(hi, bytes);
    }

    /// Whether the window budget holds the take.
    #[cfg(test)]
    pub(super) fn waiting(&self) -> bool {
        self.waiting
    }

    /// Milliseconds since this run began, at `now` (the bridge's door): the
    /// rate bucket's clock.
    fn now_ms(&self, now: Instant) -> u64 {
        u64::try_from(now.saturating_duration_since(self.clock).as_millis()).unwrap_or(u64::MAX)
    }

    fn concluded(&self) -> bool {
        !self.pin.slots.is_empty() && self.outcome.iter().all(|o| o.concluded())
    }

    /// A batch of `k` slots, none concluded: `sent` says which of them this
    /// process (or the one whose pin it recovered) may already have sent.
    fn arm_batch(&mut self, k: usize, sent: bool) {
        self.sent = vec![sent; k];
        self.unread = vec![None; k];
        self.outcome = vec![Outcome::Owed; k];
        self.floored = false;
    }

    /// No batch: the pin's slots and every per-slot record of them.
    fn clear_batch(&mut self) {
        self.pin.slots.clear();
        self.sent.clear();
        self.unread.clear();
        self.outcome.clear();
        self.floored = false;
    }

    /// Set the key in hex and in the pin.
    fn set_key(&mut self, key: Option<[u8; 16]>) {
        self.pin.key = key;
        self.key = key.map_or_else(String::new, |k| aterm_codec::hex::encode_lower(&k));
    }
}

/// Why the hello stopped the mirror on its pin, for what a person does
/// ([`Bridge::mirror_pin_remedy`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PinRemedy {
    /// The pin does not read; which durable copies the dir holds, and
    /// whether it begins as an older build's (`v=1`,
    /// [`Pin::by_an_older_build`]), which the copies need not cover.
    Pin { key: bool, floor: bool, older: bool },
    /// `logkey` and the pin's `key=` name two keys.
    Disagree,
    /// A durable copy (`logkey` or `logfloor`) does not read.
    Copy(&'static str),
}

/// What the read-back of a deduped answer at `off` found on the slot's face.
enum ReadBack {
    /// This slot's bytes, at that offset.
    Ours,
    /// Another record at that offset on the face: another writer's.
    Other,
    /// No record at that offset on the face: retention gave the record up
    /// since the dedup answer (a cut, `ReadAt::Cut`: the broker drops its
    /// dedup entry with it), or the record there is on a subject this face
    /// does not hold. On a broker that does not tell fetches, also a refusal
    /// below the face's oldest (`ReadAt::Untold`), the record perhaps held.
    Unread,
}

impl Bridge {
    /// The `hello`'s token: ` log=turns` when the mirror is asked for, ` log=auto` from an `auto` mirror before an attach found a broker that retains, nothing off.
    pub(super) fn mirror_hello_token(&self) -> &'static str {
        match self.mirror.as_deref() {
            None => "",
            Some(_) if self.mirror_asks() => " log=turns",
            Some(_) => " log=auto",
        }
    }

    /// Whether the `hello` asks for the mirror: `turns`, or `auto` over a
    /// broker the last attach found retaining.
    fn mirror_asks(&self) -> bool {
        self.mirror
            .as_deref()
            .is_some_and(|m| m.mode == LogMode::Turns || self.mirror_retained())
    }

    /// THE HELLO GATE: whether the last attach found the broker
    /// retaining this node's `ev` faces ([`Bridge::broker_retains`]), which
    /// is what an `auto` mirror runs on.
    fn mirror_retained(&self) -> bool {
        self.broker_retains == Some(true)
    }

    /// AFTER EVERY ATTACH, the first pass is held for the deliveries the attach owes while this
    /// open has yet to make the pin's fence ([`Bridge::mirror_yields`]), and an `auto` mirror
    /// follows the broker's retention. The pass is marked once the attach is up, so the posts a
    /// drain inside the attach listed (one that replayed posts runs it,
    /// [`Bridge::attach_broker_inner`]) count for nothing: they are published ahead of the
    /// attach's refill, which reads their records on a session's lane, or meets a replayed one
    /// whose post is still listed and takes the link down, and the inbox commit lands ahead of
    /// the live subscription, so none comes back on it for the pass to wait on.
    pub(super) fn mirror_attached(&mut self) {
        let Some(m) = self.mirror.as_deref_mut() else {
            return;
        };
        m.first_pass = FirstPass::Attached { drained: 0 };
        if m.mode != LogMode::Auto {
            return;
        }
        let (acked, stopped, unretained) = (m.acked, m.stopped.is_some(), m.unretained);
        if self.mirror_retained() {
            if let Some(m) = self.mirror.as_deref_mut() {
                m.unretained = false;
            }
            if !acked && !stopped {
                self.introduce();
            } else if unretained && !stopped {
                let _ = self.ctl_request(&MirrorState::On.request());
            }
            return;
        }
        if unretained {
            return;
        }
        if let Some(m) = self.mirror.as_deref_mut() {
            m.unretained = true;
        }
        stderr_line!(
            "aterm-link: mirror: the broker retains none of this node's `ev` faces, and [fabric] \
             log is \"auto\": the session logs are not mirrored onto it (\"turns\" mirrors \
             them anyway, up to a total)"
        );
        if acked {
            self.introduce();
        }
        if self.endpoint_lane >= LINK_MIRROR_WAIT_LANE {
            let _ = self.ctl_request(&MirrorState::Unretained.request());
        }
    }

    /// The `hello`'s answer: the mirror runs only when the endpoint acknowledged `log=turns` (a newer endpoint than this bridge's grammar reads it past and answers nothing: the mirror stays off, said once).
    pub(super) fn mirror_hello_answered(&mut self, header: &str) {
        let asked = self.mirror_asks();
        #[cfg(test)]
        let early = self
            .fx
            .mutant(crate::sim::world::Mutant::JudgePinBeforeRecovery);
        #[cfg(not(test))]
        let early = false;
        let wait = self.recovery_owed && !early;
        let Some(m) = self.mirror.as_deref_mut() else {
            return;
        };
        if !asked {
            m.acked = false;
            return;
        }
        m.acked = header.split_whitespace().any(|t| t == "log=turns");
        if !m.acked {
            stderr_line!(
                "aterm-link: [fabric] log = \"{}\", but this aterm did not acknowledge \
                 log=turns: it serves no session-log mirror, which stays off",
                m.mode.word()
            );
            return;
        }
        if wait {
            m.awaiting = true;
            stderr_line!(
                "aterm-link: mirror: waits for the recovery an unclean stop owes, which \
                 reconciles its pin, before it judges the pin"
            );
            return;
        }
        self.mirror_judge_pin();
    }

    /// THE RECOVERY COMPLETED ([`Bridge::recover`]): a mirror that waited for it at the hello judges the pin now ([`Bridge::mirror_hello_answered`]).
    pub(super) fn mirror_after_recovery(&mut self) {
        let Some(m) = self.mirror.as_deref_mut().filter(|m| m.awaiting) else {
            return;
        };
        m.awaiting = false;
        self.mirror_judge_pin();
    }

    /// JUDGE THE PIN an acknowledged mirror starts from, and its durable
    /// copies ([`Bridge::mirror_hello_answered`]'s doc), and say whether the
    /// mirror runs ([`MirrorState::On`]) or stopped for it ([`MirrorReason::Pin`]).
    fn mirror_judge_pin(&mut self) {
        let Some(m) = self.mirror.as_deref_mut() else {
            return;
        };
        let pinned = self.state.logpin().map(|t| t.map(|t| Pin::parse(&t)));
        let absent = matches!(pinned, Ok(None));
        let key_copy = self.state.logkey();
        let floor = self.state.logfloor();
        #[cfg(test)]
        let pinned = if self.fx.mutant(crate::sim::world::Mutant::FreshMirrorTake) {
            Ok(None)
        } else {
            pinned
        };
        #[cfg(test)]
        let key_copy = if self.fx.mutant(crate::sim::world::Mutant::KeyInThePinAlone) {
            Ok(None)
        } else {
            key_copy
        };
        #[cfg(test)]
        let floor = if self
            .fx
            .mutant(crate::sim::world::Mutant::TotalInThePinAlone)
        {
            Ok(None)
        } else {
            floor
        };
        let pinned = pinned
            .map_err(|e| e.to_string())
            .and_then(Option::transpose);
        let mut remedy = None;
        match (key_copy, floor) {
            (Err(e), _) => {
                m.stopped = Some(format!("key-unreadable: {e}"));
                remedy = Some(PinRemedy::Copy(crate::state::LOGKEY));
            }
            (_, Err(e)) => {
                m.stopped = Some(format!("floor-unreadable: {e}"));
                remedy = Some(PinRemedy::Copy(crate::state::LOGFLOOR));
            }
            (Ok(key_copy), Ok(floor)) => {
                m.floor = floor.unwrap_or(0);
                if let Some(key) = key_copy {
                    m.set_key(Some(key));
                    m.key_durable = true;
                }
                match pinned {
                    Ok(None) => {
                        m.pin.total = m.floor;
                        if absent && (key_copy.is_some() || floor.is_some()) {
                            if let Err(e) = self.state.set_logpin(&m.pin.render()) {
                                stderr_line!(
                                    "aterm-link: mirror: no logpin, and one could not be \
                                     written from logkey and logfloor ({e}); the next take \
                                     writes it"
                                );
                            }
                        }
                    }
                    Ok(Some(mut pin)) => match (key_copy, pin.key) {
                        (Some(copy), Some(pinned)) if copy != pinned => {
                            m.stopped = Some(
                                "key-disagrees: logkey and logpin's key= name two \
                                              alias keys"
                                    .to_string(),
                            );
                            remedy = Some(PinRemedy::Disagree);
                        }
                        (copy, pinned) => {
                            let k = pin.slots.len();
                            if k > 0 {
                                stderr_line!(
                                    "aterm-link: mirror: recovering a pinned batch of {k} slots"
                                );
                            }
                            m.arm_batch(k, true);
                            m.recovered = k as u64;
                            pin.key = copy.or(pinned).or(m.pin.key);
                            m.pin = pin;
                            let key = m.pin.key;
                            m.set_key(key);
                        }
                    },
                    Err(e) => {
                        m.stopped = Some(format!("pin-unreadable: {e}"));
                        remedy = Some(PinRemedy::Pin {
                            key: key_copy.is_some(),
                            floor: floor.is_some(),
                            older: self.state.logpin_by_an_older_build(),
                        });
                    }
                }
            }
        }
        m.owed = true;
        if let Some(remedy) = remedy.map(|r| self.mirror_pin_remedy(r)) {
            if let Some(why) = self.mirror.as_deref_mut().and_then(|m| m.stopped.as_mut()) {
                why.push_str("; kept as it is.");
                why.push_str(&remedy);
            }
        }
        let report = match self.mirror.as_deref().and_then(|m| m.stopped.as_deref()) {
            Some(why) => {
                stderr_line!("aterm-link: mirror stopped: {why}");
                MirrorReason::Pin.request()
            }
            None => MirrorState::On.request(),
        };
        // What aterm is told now is that the mirror runs: a wait on the window budget a run
        // before this hello announced is no longer what it shows, so the window's next check
        // says it again while it holds (`Bridge::mirror_window_open` speaks only on a change of
        // what it last said; aterm-link B05 review: aterm showed the mirror on while it waited).
        #[cfg(test)]
        let forget_the_wait = !self
            .fx
            .mutant(crate::sim::world::Mutant::JudgeKeepsAnAnnouncedWait);
        #[cfg(not(test))]
        let forget_the_wait = true;
        if let Some(m) = self.mirror.as_deref_mut().filter(|_| forget_the_wait) {
            m.waiting = false;
        }
        let _ = self.ctl_request(&report);
    }

    /// WHAT A PERSON DOES about a mirror the pin or its copies stopped: one
    /// sentence a reader can act on, naming the file, what each choice costs
    /// — a second alias for every edge on the bus where a fresh key would be
    /// minted, a total below what was mirrored where the copies
    /// cannot say it — and claiming no cause it cannot show
    /// (`mirror stopped: …` on stderr, which aterm keeps in its log).
    fn mirror_pin_remedy(&self, remedy: PinRemedy) -> String {
        let at = |name: &str| self.state.root().join(name).display().to_string();
        let pin = at(crate::state::LOGPIN);
        let logkey = at(crate::state::LOGKEY);
        let logfloor = at(crate::state::LOGFLOOR);
        match remedy {
            PinRemedy::Pin {
                key: true,
                floor: true,
                older: false,
            } => {
                let why = match &self.pin_unreconciled {
                    Some(e) => format!(
                        " This bridge, the first after the last one stopped uncleanly in an \
                         earlier boot, could not set it aside ({e})."
                    ),
                    None => " Its durable copies are on disk, but this bridge cannot show a \
                             power cut tore it: the first bridge after a reboot sets such a \
                             pin aside, and within one boot, or after a clean stop (which \
                             flushed it), damage is a bug's — unless that first bridge could \
                             not move it (aterm's log says why), or no boot here can be told \
                             from another."
                        .to_string(),
                };
                format!(
                    "{why} To resume, move {pin} aside; the next bridge resumes with the same \
                     alias key ({logkey}) and the total at its floor ({logfloor}), and a batch \
                     the pin held is taken again, a copy each reader dedupes."
                )
            }
            PinRemedy::Pin {
                key: false,
                floor: true,
                older: false,
            } => format!(
                " {logkey} holds no alias key, though {logfloor} holds the total: either none \
                 could be minted here (every edge named `-`), or the file was removed or lost. \
                 To resume, move {pin} aside — the next bridge then mirrors under a FRESH key, \
                 its total from the floor, and if a key was named before, every edge takes a \
                 second alias on the bus: put {logkey} back first where a copy of the key \
                 survives (the key= word in a logpin's first line, here or in the quarantine)."
            ),
            PinRemedy::Pin {
                key,
                floor: true,
                older: true,
            } => format!(
                " It begins as an older aterm's pin (v=1), written after this build's by one \
                 that keeps neither copy, so {logfloor} may count less than that build \
                 mirrored. To resume, move {pin} aside — the next bridge then mirrors under {}, \
                 its total at the floor, below what that build counted by what it mirrored — \
                 or put back a whole logpin to keep its total.",
                if key {
                    format!("the same alias key ({logkey})")
                } else {
                    "a FRESH key, so every edge takes a second alias on the bus if one was named"
                        .to_string()
                }
            ),
            PinRemedy::Pin {
                key: true,
                floor: false,
                ..
            } => format!(
                " It holds the node's only copy of its lifetime total: to resume, move {pin} \
                 aside — the next bridge then mirrors under the same alias key ({logkey}), its \
                 total counted from zero — or put back a whole logpin to keep the total."
            ),
            PinRemedy::Pin {
                key: false,
                floor: false,
                ..
            } => format!(
                " It holds the node's only copy of its alias key and its lifetime total (an \
                 older aterm's pin): to resume, move {pin} aside — the next bridge then mirrors \
                 under a FRESH alias key, so every edge takes a second alias on the bus, its \
                 total counted from zero — or put back a whole logpin to keep both."
            ),
            PinRemedy::Disagree => format!(
                " Two keys mean an older build minted a second one, or a file was edited: \
                 decide which stands and remove the other ({logkey} or the key= word in {pin}); \
                 the next bridge mirrors under the one left."
            ),
            PinRemedy::Copy(crate::state::LOGKEY) => format!(
                " {logkey} is written durably, so a bug or a hand damaged it: put its whole \
                 bytes back (32 hex digits and a newline; a logpin that reads names the same key \
                 in its key= word). Removed, the next bridge takes the key= word of {pin} where \
                 that pin reads, and otherwise mirrors under a FRESH key, every edge then \
                 taking a second alias on the bus."
            ),
            PinRemedy::Copy(name) => format!(
                " {} is written durably, so a bug or a hand damaged it: put its whole bytes \
                 back (one number and a newline, at or above the total= word of {pin}). \
                 Removed, the next bridge counts from that pin's total where it reads, which a \
                 power cut may have rolled back below what was mirrored; a torn logpin then \
                 stops the mirror, and with no logpin the total counts from zero.",
                at(name)
            ),
        }
    }

    /// Something may have been appended to some session's log (a gap, an attach, the roster's
    /// backstop): a take is owed, and no rate hold is known to cover it ([`MirrorRun::unheld`]).
    pub(super) fn mirror_wake(&mut self) {
        if let Some(m) = self.mirror.as_deref_mut() {
            m.owed = true;
            m.unheld = true;
        }
    }

    /// An endpoint event of the session `local` (`EVENT <local> …`): something may have been
    /// appended to its log, so a take is owed. The take waits behind an owed commit's write only
    /// while every session woken since the last empty answer is one that answer's rate hold
    /// covers ([`MirrorRun::held`], [`Bridge::mirror_behind_the_commit`]); an event of any other
    /// session, or of one this bridge does not list, may owe its snapshot at once.
    pub(super) fn mirror_woken_by(&mut self, local: Option<&str>) {
        let sid = local
            .and_then(|l| l.parse::<u64>().ok())
            .and_then(|l| self.locals.get(&l));
        if let Some(m) = self.mirror.as_deref_mut() {
            m.owed = true;
            m.unheld |= !sid.is_some_and(|sid| m.held.contains(sid));
        }
    }

    /// Whether the mirror runs on this bridge: on, acknowledged by the endpoint, its pin judged, not stopped, and, `auto`, over a broker that retains it.
    pub(super) fn mirror_engaged(&self) -> bool {
        self.mirror.as_deref().is_some_and(|m| {
            m.acked
                && !m.awaiting
                && m.stopped.is_none()
                && (m.mode == LogMode::Turns || self.mirror_retained())
        })
    }

    /// TEST-ONLY: the node's total of mirrored bytes as an engaged mirror holds it (`None`: off, not acknowledged, or stopped) — the simulation's oracle (`crate::sim::world`).
    #[cfg(test)]
    pub(crate) fn mirror_engaged_total(&self) -> Option<u64> {
        if !self.mirror_engaged() {
            return None;
        }
        self.mirror.as_deref().map(|m| m.pin.total)
    }

    /// TEST-ONLY: what the last attach read of the broker's retention ([`Bridge::broker_retains`]) — the simulation's count of the mirror's retained and unretained paths (`crate::sim::world`).
    #[cfg(test)]
    pub(crate) fn broker_retention_read(&self) -> Option<bool> {
        self.broker_retains
    }

    /// TEST-ONLY: the mirror's window budget at `bytes` instead of [`mirror::NODE_WINDOW_BYTES`],
    /// so a simulation whose rules keep a few records reaches it (sim item B05).
    #[cfg(test)]
    pub(crate) fn simulate_window(&mut self, bytes: u64) {
        if let Some(m) = self.mirror.as_deref_mut() {
            m.window = bytes;
        }
    }

    /// When the loop owes the mirror a pass: `Some(now)` for work it can do now (the loop's wait is then zero), the retry's instant while a busy answer waits, a held-back snapshot's when only it is owed, `None` for nothing, and for nothing while the pin's settle is owed its answer, which every round's pass looks for first and the round that reads it makes the fence of ([`Bridge::read_the_pins_settle`], [`Bridge::make_the_pin_fence`], critic item F07); no earlier than a held first pass's instant ([`FirstPass::Held`]), nor, for a take nothing would carry once its hold began, than [`IDLE_ACK_GRACE`] past that ([`Bridge::mirror_rides_nothing`]), nor, for a take inside the endpoint's rate hold that only wakes of the sessions it holds owe, than the owed commit's write ([`Bridge::mirror_behind_the_commit`]).
    pub(super) fn mirror_due(&self, now: Instant) -> Option<Instant> {
        if !self.mirror_engaged() || self.conn.is_none() || self.pin_settle_owed() {
            return None;
        }
        let m = self.mirror.as_deref()?;
        let due = if !m.owed {
            m.held_back.map(|at| at.max(now))
        } else if m.outcome.contains(&Outcome::Sending) && m.sent_inc == self.inc {
            None
        } else {
            Some(m.retry_at.map_or(now, |t| t.max(now)))
        };
        let due = match (due, m.uncarried) {
            (Some(at), Some(from)) if self.mirror_rides_nothing() => {
                Some(at.max(from + IDLE_ACK_GRACE))
            }
            _ => due,
        };
        let due = match (due, self.mirror_behind_the_commit(now)) {
            (Some(at), Some(written)) => Some(at.max(written)),
            _ => due,
        };
        match m.first_pass {
            FirstPass::Held { until, .. } => due.map(|at| at.max(until)),
            FirstPass::Free | FirstPass::Attached { .. } => due,
        }
    }

    /// A TAKE NOTHING WOULD CARRY WAITS FOR A COMMIT TO CARRY (critic item E04): whether a pass
    /// due now would take a fresh batch with no inbox commit for its burst to carry
    /// ([`Bridge::mirror_has_a_commit_to_carry`]), in an open that holds the pin's fence
    /// ([`StateDir::logpin_fence_held`]). Such a pass is held from when the loop first finds it
    /// due ([`Bridge::mirror_step`]) for at most [`IDLE_ACK_GRACE`]: it goes as soon as an inbox
    /// commit falls due for its burst to carry, or alone once the grace is out, and nothing else
    /// ends the hold. A sender's snapshot of the mail count its own post moved falls due before
    /// the delivery owes a commit; on one node the delivery comes well inside the grace, so the
    /// pass behind it takes the sender's snapshot and the recipient's in one take, and its burst
    /// carries the commit ahead of both: one write and one writer batch where they were two. A
    /// take that comes back empty (a snapshot the endpoint holds back to its rate) carries
    /// nothing, and the round's settle holds the commit from the round's start
    /// ([`Bridge::mirror_pass_then_settle`]), so the take's ctl round trip runs inside the
    /// commit's grace, not ahead of it; once an answer said the endpoint holds a snapshot back,
    /// a take before that hold's end that only wakes of the sessions it holds owe waits for the
    /// commit's write instead ([`Bridge::mirror_behind_the_commit`]). With no commit coming (a
    /// post to another node's session, or to a person) the take goes alone at the grace's end,
    /// so the hold puts no take off by more than that.
    ///
    /// Not held: a pass with a pinned batch (a resend, a re-pin, a recovered batch); an open's
    /// first pass, which makes the fence and sends the commit its deliveries owe alone ahead of
    /// it ([`Bridge::commit_ahead_of_the_fence`], critic item D06), so nothing would ride it, and
    /// where the broker settles the pin, the take its settle's answer lets go
    /// ([`MirrorRun::fence_settled`], critic item F07's review), the open's first all the same;
    /// and the take after a landing whose take left a backlog ([`MirrorRun::drains`]), which
    /// drains it at once as each batch lands. A post does not end the hold: a take pins its batch
    /// before any slot goes, so a post that carried one would wait behind the take, and the
    /// post's own delivery on this node owes the commit the pass then rides.
    pub(super) fn mirror_rides_nothing(&self) -> bool {
        #[cfg(test)]
        if self
            .fx
            .mutant(crate::sim::world::Mutant::MirrorSendsAnUnowedSlotAtOnce)
        {
            return false;
        }
        #[cfg(test)]
        let drains = |m: &MirrorRun| {
            m.drains
                && !self
                    .fx
                    .mutant(crate::sim::world::Mutant::MirrorHoldsADrainingTake)
        };
        #[cfg(not(test))]
        let drains = |m: &MirrorRun| m.drains;
        #[cfg(test)]
        let first = |m: &MirrorRun| {
            m.fence_settled
                && !self
                    .fx
                    .mutant(crate::sim::world::Mutant::SettledFenceTakeWaitsForACommit)
        };
        #[cfg(not(test))]
        let first = |m: &MirrorRun| m.fence_settled;
        self.mirror
            .as_deref()
            .is_some_and(|m| m.pin.slots.is_empty() && !m.repin && !drains(m) && !first(m))
            && !self.mirror_has_a_commit_to_carry()
            && self.state.logpin_fence_held()
    }

    /// A TAKE INSIDE THE ENDPOINT'S RATE HOLD GOES AFTER THE COMMIT'S WRITE (critic item E04c):
    /// the endpoint's last answer to a take came back empty and said when a snapshot it holds
    /// back to its rate falls due ([`MirrorRun::held_back`]) and whose snapshots it holds back
    /// ([`MirrorRun::held`]). The hold is per session: until then those sessions' snapshots stay
    /// back, and a delivery's or a post's mail marks stay local, but any other session's
    /// snapshot is in the next take as soon as its log owes one. So only a take every wake since
    /// that answer owes to a session the hold names ([`Bridge::mirror_woken_by`]) comes back
    /// empty, unless one of them appended a record of the bus class. Such a take, with nothing
    /// pinned and no backlog to drain, waits while an inbox commit is owed for the loop's next
    /// write ([`Bridge::mirror_has_a_commit_to_carry`]) until the commit owed when it first
    /// waited ([`MirrorRun::behind`]) is written: alone at its grace's end, carried by a publish,
    /// or sent ahead of the next item off the bus. Its `log-take` ctl round trip then runs while
    /// the broker flushes the commit, not ahead of the commit's write, which a round trip that
    /// crossed the commit's [`IDLE_ACK_GRACE`] put off by itself, with nothing riding the
    /// commit. A take that any other wake owes ([`MirrorRun::unheld`]) may carry a snapshot, so
    /// it goes as E04's rule has it and its burst carries the commit
    /// ([`Bridge::mirror_rides_nothing`]): waiting would send the commit alone and the snapshot
    /// in a write of its own behind it (critic item E04c's review).
    ///
    /// What bounds it: the wait ends once that one commit is written, whatever later deliveries
    /// owe, so it lasts no longer than that commit is held, which past its grace is only while
    /// the round's settle keeps it back for an inbox record still queued; and it ends at the rate
    /// hold's end, or at a wake the hold does not cover. The take is dated from when it first
    /// waited ([`MirrorRun::uncarried`]), so E04's hold for a commit to fall due holds it no
    /// longer than a grace from then once the commit is written. At the rate hold's end the
    /// snapshot is due, the take comes back with it, and its burst carries the commit as E04's
    /// rule has it. A record of the bus class a held session appended inside the hold, with no
    /// wake the hold does not cover since, goes in that take, behind the commit, rather than
    /// riding it.
    ///
    /// Returns the latest instant the wait ends: the held commit's grace end (a grace from now
    /// for one the round's settle has yet to hold), no later than the rate hold's end. It is
    /// `now` or earlier while the round's settle has yet to send a commit whose grace is out,
    /// which the pass waits for ([`Bridge::mirror_step`]). `None`: the pass waits on no commit.
    pub(super) fn mirror_behind_the_commit(&self, now: Instant) -> Option<Instant> {
        #[cfg(test)]
        if self
            .fx
            .mutant(crate::sim::world::Mutant::RateHeldTakeAheadOfTheCommit)
        {
            return None;
        }
        #[cfg(test)]
        let unheld = |m: &MirrorRun| {
            m.unheld
                && !self
                    .fx
                    .mutant(crate::sim::world::Mutant::RateHoldCoversEverySession)
        };
        #[cfg(not(test))]
        let unheld = |m: &MirrorRun| m.unheld;
        let m = self.mirror.as_deref()?;
        let hold_ends = m.held_back.filter(|&at| now < at)?;
        let waits = m.pin.slots.is_empty()
            && !m.repin
            && !m.drains
            && !unheld(m)
            && self.mirror_has_a_commit_to_carry()
            && m.behind.is_none_or(|off| !self.commit_written_through(off));
        waits.then(|| (self.commit_held.unwrap_or(now) + IDLE_ACK_GRACE).min(hold_ends))
    }

    /// Whether an inbox commit through `off` is written: owed on the link, or landed.
    fn commit_written_through(&self, off: u64) -> bool {
        self.committed_through(off)
            || self
                .conn
                .as_ref()
                .is_some_and(|link| link.owed().any(|then| then.commits_through(off)))
    }

    /// WHETHER THE LOOP HOLDS THE MIRROR'S PASS BACK FOR A ROUND (critic item D06): the first
    /// pass due after an attach, in an open that has yet to make the pin's fence
    /// ([`StateDir::logpin_fence_held`]), is held while the loop owes a delivery the attach
    /// brought: an item queued, or the record of a post its drains published to a session this
    /// node hosts still on its way back ([`FirstPass::Attached`]; each inbox record taken
    /// counts one off). The hold lasts at most the mirror's `hold` ([`FIRST_PASS_HOLD`]) from
    /// when the pass came due, and ends at the first round that finds the loop owing neither
    /// ([`FirstPass::Held`]). Each round under it parks until an item comes or the hold is out
    /// ([`Bridge::mirror_due`]) and takes what comes, so the deliveries an attach or a handoff
    /// owes go ahead of the fence and the pass, and a steady stream of items cannot starve the
    /// pass past the hold. The fence is the pin's two full flushes of the bridge's own, or,
    /// where the broker settles the pin ([`Bridge::broker_settles_the_pin`]), its barriers on
    /// the loop and the settle the take then waits for ([`Bridge::make_the_pin_fence`], critic
    /// item F07). A post whose record never comes back here (one to another node's session, or
    /// to a topic) is not awaited (critic item G04). What the fence orders is unchanged: the
    /// pass that follows makes it durable before any take or send
    /// ([`Bridge::mirror_key_on_the_media`]), and sends the inbox commit its deliveries owe
    /// ahead of it ([`Bridge::commit_ahead_of_the_fence`]).
    pub(super) fn mirror_yields(&mut self) -> bool {
        #[cfg(test)]
        if self
            .fx
            .mutant(crate::sim::world::Mutant::MirrorAheadOfTheFirstDelivery)
        {
            return false;
        }
        let now = self.fx.now();
        let idle = self.mailbox.is_idle();
        let Some((first_pass, hold)) = self.mirror.as_deref().map(|m| (m.first_pass, m.hold))
        else {
            return false;
        };
        let next = match first_pass {
            FirstPass::Free => return false,
            FirstPass::Held { until, awaits } if now < until && (awaits > 0 || !idle) => {
                return true;
            }
            FirstPass::Held { .. } => FirstPass::Free,
            FirstPass::Attached { .. } if self.mirror_due(now).is_none_or(|at| at > now) => {
                return false;
            }
            FirstPass::Attached { .. } if self.state.logpin_fence_held() => FirstPass::Free,
            FirstPass::Attached { drained: 0 } if idle => FirstPass::Free,
            FirstPass::Attached { drained } => FirstPass::Held {
                until: now + hold,
                awaits: drained,
            },
        };
        if let Some(m) = self.mirror.as_deref_mut() {
            m.first_pass = next;
        }
        next != FirstPass::Free
    }

    /// A drain publishes a post onto `subject`: an attach's first pass not yet due waits for its
    /// record to come back, or for its hold to run out ([`Bridge::mirror_yields`]), when that
    /// record comes back here, on this node's inbox face (a lane of a session this node hosts,
    /// [`subject::inbox_filter`]). A post to another node's session, or to a topic, lands
    /// where this node's inbox face never reads it, so it is not awaited (critic item G04):
    /// counted, it held the pass the whole hold. NEGATIVE CONTROLS:
    /// `Mutant::FirstPassForgetsTheDrainedPosts` counts none, and
    /// `Mutant::FirstPassCountsAnotherNodesPost` counts every post, as D06 did.
    pub(super) fn mirror_drained_post(&mut self, subject: &str) {
        #[cfg(test)]
        if self
            .fx
            .mutant(crate::sim::world::Mutant::FirstPassForgetsTheDrainedPosts)
        {
            return;
        }
        #[cfg(test)]
        let every = self
            .fx
            .mutant(crate::sim::world::Mutant::FirstPassCountsAnotherNodesPost);
        #[cfg(not(test))]
        let every = false;
        let here = subject
            .strip_prefix("/f/")
            .and_then(|s| s.strip_prefix(self.cfg.fleet.as_str()))
            .and_then(|s| s.strip_prefix("/in/"))
            .and_then(|s| s.strip_prefix(self.node.as_str()))
            .is_some_and(|s| s.starts_with('/'));
        if !(here || every) {
            return;
        }
        if let Some(m) = self.mirror.as_deref_mut() {
            if let FirstPass::Attached { drained } = &mut m.first_pass {
                *drained = drained.saturating_add(1);
            }
        }
    }

    /// The loop took an inbox record: one fewer a held first pass waits on
    /// ([`Bridge::mirror_yields`]).
    pub(super) fn mirror_took_inbox(&mut self) {
        #[cfg(test)]
        if self
            .fx
            .mutant(crate::sim::world::Mutant::FirstPassAwaitsPastItsRecords)
        {
            return;
        }
        if let Some(m) = self.mirror.as_deref_mut() {
            if let FirstPass::Held { awaits, .. } = &mut m.first_pass {
                *awaits = awaits.saturating_sub(1);
            }
        }
    }

    /// Stop the mirror for this bridge's life: said on stderr and to the
    /// endpoint (`fabric status mirror=stopped:<reason>` or `capped`), the
    /// pin kept for a later bridge.
    fn mirror_stop(&mut self, why: &str, reason: MirrorReason) {
        let Some(m) = self.mirror.as_deref_mut() else {
            return;
        };
        if m.stopped.is_some() {
            return;
        }
        m.stopped = Some(why.to_string());
        stderr_line!(
            "aterm-link: mirror stopped for this bridge's life: {why}; the pinned batch, if \
             any, is kept for the next"
        );
        let _ = self.ctl_request(&reason.request());
    }

    /// THE PASS: re-pin after a collision, take when nothing is pinned, send what is owed, and settle a batch whose every slot concluded. A take nothing would carry is held first, from the round that finds it due ([`Bridge::mirror_rides_nothing`]), and a take inside the endpoint's rate hold that only wakes of the sessions it holds owe waits for an owed commit's write, a commit whose grace is out included, which the round's settle sends after this pass ([`Bridge::mirror_behind_the_commit`]).
    pub(super) fn mirror_step(&mut self) {
        let now = self.fx.now();
        // The wait is noted before the due check, which the wait itself puts off.
        let behind = self
            .mirror_behind_the_commit(now)
            .and_then(|_| self.inbox_commit_due());
        if let (Some(off), Some(m)) = (behind, self.mirror.as_deref_mut()) {
            if m.owed {
                m.behind.get_or_insert(off);
                m.uncarried.get_or_insert(now);
            }
        }
        if behind.is_some() || self.mirror_due(now).is_none_or(|at| at > now) {
            return;
        }
        let rides_nothing = self.mirror_rides_nothing();
        if let Some(m) = self.mirror.as_deref_mut() {
            if rides_nothing && m.uncarried.is_none() {
                m.uncarried = Some(now);
                return;
            }
        }
        let inc = self.inc;
        if let Some(m) = self.mirror.as_deref_mut() {
            m.retry_at = None;
            m.held_back = None;
            m.held.clear();
            m.unheld = false;
            m.uncarried = None;
            m.behind = None;
            m.fence_settled = false;
            if m.sent_inc != inc {
                for o in &mut m.outcome {
                    if *o == Outcome::Sending {
                        *o = Outcome::Owed;
                    }
                }
            }
        }
        let was = std::mem::replace(&mut self.in_round, true);
        if self.mirror.as_deref().is_some_and(|m| m.repin) {
            self.mirror_repin();
        }
        if self
            .mirror
            .as_deref()
            .is_some_and(|m| m.pin.slots.is_empty())
        {
            self.mirror_take();
        }
        self.mirror_send();
        if self.mirror.as_deref().is_some_and(MirrorRun::concluded) {
            self.mirror_settle();
        }
        self.in_round = was;
    }

    pub(super) fn mirror_take(&mut self) {
        let now = self.fx.now();
        let Some((total, n, key)) = self.mirror.as_deref_mut().map(|m| {
            let now_ms = m.now_ms(now);
            let room = m.bucket.room(now_ms);
            let n = usize::try_from(room).unwrap_or(usize::MAX).min(m.batch);
            (m.pin.total, n, m.key.clone())
        }) else {
            return;
        };
        if self.broker_retains == Some(true) {
            if !self.mirror_window_open() {
                self.mirror_retry_in(DEADLINE_RETRY);
                return;
            }
        } else {
            // A broker that no longer retains has no window to wait on.
            if self.mirror.as_deref().is_some_and(|m| m.waiting) {
                if let Some(m) = self.mirror.as_deref_mut() {
                    m.waiting = false;
                }
                let _ = self.ctl_request(&MirrorState::On.request());
            }
            if total >= mirror::TOTAL_MAX_BYTES {
                self.mirror_stop(
                    &format!(
                        "the node mirrored its total of {} bytes with no retention in the broker",
                        mirror::TOTAL_MAX_BYTES
                    ),
                    MirrorReason::Total,
                );
                return;
            }
        }
        if n == 0 {
            self.mirror_retry_in(BUSY_WAIT);
            return;
        }
        if !self.mirror_key_on_the_media() {
            return;
        }
        let tier = if self.cfg.presence == crate::presence::Mode::Meta {
            "meta"
        } else {
            "wire"
        };
        let key = if key.is_empty() {
            String::new()
        } else {
            format!(" key={key}")
        };
        let reply = match self.ctl_request(&format!("log-take n={n} tier={tier}{key}")) {
            Ok(reply) => reply,
            Err(e) => {
                stderr_line!("aterm-link: mirror: log-take failed: {e}");
                self.mirror_retry_in(DEADLINE_RETRY);
                return;
            }
        };
        let (header, body) = match reply {
            Reply::Bytes { header, body } => (header, body),
            other => {
                let header = other.header().to_string();
                if header.starts_with("ERR busy") {
                    self.mirror_retry_in(DEADLINE_RETRY);
                } else {
                    self.mirror_stop(
                        &format!("the endpoint refused log-take ({header})"),
                        MirrorReason::Endpoint,
                    );
                }
                return;
            }
        };
        let mirror::Taken {
            slots,
            more,
            due,
            held,
        } = match mirror::parse_take(&header, &body) {
            Ok(taken) => taken,
            Err(e) => {
                self.mirror_stop(&e, MirrorReason::Endpoint);
                return;
            }
        };
        // Slots that never pin (a failure below) are taken again as owed work; a batch that
        // pins drains a backlog only if its take left one.
        if let Some(m) = self.mirror.as_deref_mut() {
            m.drains = more || !slots.is_empty();
        }
        if slots.is_empty() {
            if let Some(m) = self.mirror.as_deref_mut() {
                m.owed = more;
                m.held_back = due.map(|wait| now + wait);
                // A wake from here on is judged against this answer's hold alone.
                m.held = held;
                m.unheld = false;
            }
            if more {
                self.mirror_retry_in(BUSY_WAIT);
            }
            return;
        }
        let unsent = self.unsent.take();
        let mut pinned = Vec::with_capacity(slots.len());
        for s in slots {
            match self.next_seq() {
                Ok(pseq) => pinned.push(PinSlot {
                    pseq,
                    sid: s.sid,
                    at: s.at,
                    body: s.body,
                }),
                Err(e) => {
                    self.unsent = unsent;
                    stderr_line!("aterm-link: mirror: no sequence for a slot: {e}");
                    self.mirror_retry_in(DEADLINE_RETRY);
                    return;
                }
            }
        }
        self.unsent = unsent;
        let inc = self.inc;
        let now = self.fx.now();
        let Some(m) = self.mirror.as_deref_mut() else {
            return;
        };
        let k = pinned.len();
        m.pin.inc = inc;
        m.pin.slots = pinned;
        m.arm_batch(k, false);
        m.recovered = 0;
        m.recovered_deduped = 0;
        m.owed = true;
        if !self.mirror_floor_covers() {
            if let Some(m) = self.mirror.as_deref_mut() {
                m.clear_batch();
            }
            return;
        }
        let Some(m) = self.mirror.as_deref_mut() else {
            return;
        };
        m.bucket.spend(k as u64);
        if let Err(e) = self.state.set_logpin(&m.pin.render()) {
            stderr_line!("aterm-link: mirror: the pin could not be written ({e}); nothing sent");
            m.clear_batch();
            m.retry_at = Some(now + DEADLINE_RETRY);
        } else {
            m.drains = more || k >= n;
        }
    }

    /// THE WINDOW BUDGET, on a
    /// retaining broker: whether this node's landed bytes in the broker's
    /// window are under [`mirror::NODE_WINDOW_BYTES`] (the run's `window`).
    fn mirror_window_open(&mut self) -> bool {
        let over = |m: &MirrorRun| m.pin.window_bytes(m.oldest) >= m.window;
        if self.mirror.as_deref().is_some_and(over) {
            let faces = format!("/f/{}/pub/{}/*/ev", self.cfg.fleet, self.node);
            let fx = self.fx.clone();
            let Some(conn) = self.conn_now() else {
                return false;
            };
            let started = fx.now();
            let answer = conn.oldest(&faces);
            let Ok((oldest, _)) = self.observe(started, answer, Some("read")) else {
                return false;
            };
            #[cfg(test)]
            let oldest = oldest.filter(|_| {
                !self
                    .fx
                    .mutant(crate::sim::world::Mutant::MirrorIgnoresOldest)
            });
            if let (Some(m), Some(oldest)) = (self.mirror.as_deref_mut(), oldest) {
                m.oldest = m.oldest.max(oldest);
                m.pin.prune_window(m.oldest);
            }
        }
        let Some(m) = self.mirror.as_deref_mut() else {
            return false;
        };
        let window = m.pin.window_bytes(m.oldest);
        let open = window < m.window;
        if open == !m.waiting {
            return open;
        }
        m.waiting = !open;
        let report = if open {
            stderr_line!(
                "aterm-link: mirror: the broker's window has room again; the mirror goes on"
            );
            MirrorState::On
        } else {
            stderr_line!(
                "aterm-link: mirror: this node's {window} bytes in the broker's window (from \
                 @{}) reach its budget of {}; the mirror waits for the broker's retention to \
                 give the oldest of them up",
                m.oldest,
                m.window
            );
            if self.endpoint_lane < LINK_MIRROR_WAIT_LANE {
                return open;
            }
            MirrorState::Budget
        };
        let _ = self.ctl_request(&report.request());
        open
    }

    fn mirror_retry_in(&mut self, wait: Duration) {
        let now = self.fx.now();
        if let Some(m) = self.mirror.as_deref_mut() {
            m.retry_at = Some(now + wait);
        }
    }

    /// whether `logkey` holds the key this mirror names, written durably now if it does not yet — before the first take that names the key, and before any slot of a recovered batch an older build's pin aliased with it is sent, so a power cut that tears or rolls back the pin can never leave the bus holding aliases of a key the next bridge does not know. The pin's fence goes first, the inbox commit a delivery owes ahead of it ([`Bridge::commit_ahead_of_the_fence`]).
    pub(super) fn mirror_key_on_the_media(&mut self) -> bool {
        if self.mirror.is_none() {
            return false;
        }
        self.commit_ahead_of_the_fence();
        match self.make_the_pin_fence() {
            Ok(true) => {}
            Ok(false) => return false,
            Err(e) => {
                stderr_line!(
                    "aterm-link: mirror: the pin's version fence could not be made durable \
                     ({e}); nothing taken or sent, tried again"
                );
                self.mirror_retry_in(DEADLINE_RETRY);
                return false;
            }
        }
        let Some(m) = self.mirror.as_deref() else {
            return false;
        };
        if m.key_durable {
            return true;
        }
        let key = m.pin.key;
        #[cfg(test)]
        let key = if self.fx.mutant(crate::sim::world::Mutant::KeyInThePinAlone) {
            None
        } else {
            key
        };
        let written = key.map_or(Ok(()), |key| self.state.set_logkey(&key));
        match written {
            Ok(()) => {
                if let Some(m) = self.mirror.as_deref_mut() {
                    m.key_durable = true;
                }
                true
            }
            Err(e) => {
                stderr_line!(
                    "aterm-link: mirror: the alias key could not be written to logkey ({e}); \
                     nothing taken or sent, tried again"
                );
                self.mirror_retry_in(DEADLINE_RETRY);
                false
            }
        }
    }

    /// THE PIN'S FENCE (critic items D06 and F07): whether this open holds it, made now where
    /// it can be, ahead of any take or send. Where this link's broker settles the state dir's
    /// device ([`Bridge::broker_settles_the_pin`]), the pin goes by barriers alone
    /// ([`StateDir::order_logpin`]: the temp's, the rename, the directory's) and, once they
    /// returned, a settle goes pipelined ([`Bridge::send_pin_settle`]); the fence is held once
    /// the broker answers it ([`Bridge::pin_settled`]), and until then every take and send waits
    /// (`Ok(false)`) while the loop goes on with its deliveries ([`Bridge::mirror_due`]). The
    /// answer is the proof, not a durable head read off the bus: it comes after a full flush the
    /// broker's writer issued once it took the request, which follows the barriers, so it
    /// drained them whatever a flush before it covered. Anywhere else (a log on another device
    /// or another volume, a broker that names no device or cannot settle) the bridge flushes the
    /// pin in full itself ([`StateDir::ensure_logpin_durable`]).
    ///
    /// # Errors
    /// The pin's write, the barrier's or the full flush's, or the settle's send.
    pub(super) fn make_the_pin_fence(&mut self) -> io::Result<bool> {
        #[cfg(test)]
        if self
            .fx
            .mutant(crate::sim::world::Mutant::TakeAheadOfTheFence)
        {
            return Ok(true);
        }
        if self.state.logpin_fence_held() {
            return Ok(true);
        }
        if self.pin_settle_owed() {
            #[cfg(test)]
            if self
                .fx
                .mutant(crate::sim::world::Mutant::FenceBeforeTheSettle)
            {
                return Ok(true);
            }
            return Ok(false);
        }
        let Some(m) = self.mirror.as_deref() else {
            return Ok(false);
        };
        if !self.broker_settles_the_pin() {
            self.state.ensure_logpin_durable(|| m.pin.render())?;
            return Ok(true);
        }
        #[cfg(test)]
        if self
            .fx
            .mutant(crate::sim::world::Mutant::SettleAheadOfTheBarrier)
        {
            let text = m.pin.render();
            self.send_pin_settle()?;
            self.state.order_logpin(|| text)?;
            return Ok(false);
        }
        if self.state.order_logpin(|| m.pin.render())? {
            return Ok(true);
        }
        self.send_pin_settle()?;
        // Held from the answer, never the send: the broker's writer may still be flushing.
        #[cfg(test)]
        let held = self
            .fx
            .mutant(crate::sim::world::Mutant::FenceBeforeTheSettle);
        #[cfg(not(test))]
        let held = false;
        Ok(held)
    }

    /// WHETHER THIS LINK'S BROKER SETTLES THE PIN (critic item F07): its `Hello` offers the settle
    /// and names the device its log is on, the state dir's own ([`StateDir::device`]), whose
    /// every write it drains with its full flush (astream's `LogDevice::settles`: `F_FULLFSYNC`,
    /// on Apple), and it has not refused one on this link. A device number is a volume's, not a
    /// drive's: a log on another volume of the state dir's drive is judged another device, and
    /// the bridge flushes for itself there.
    pub(super) fn broker_settles_the_pin(&self) -> bool {
        if self.settle_refused {
            return false;
        }
        let Some(link) = self.conn.as_ref() else {
            return false;
        };
        let Some(device) = link.log_device().filter(|_| link.supports_settle()) else {
            return false;
        };
        #[cfg(test)]
        if self
            .fx
            .mutant(crate::sim::world::Mutant::TrustAnotherDevicesFlush)
        {
            return device.flush_drains_device;
        }
        self.state.device().is_some_and(|dev| device.settles(dev))
    }

    /// Whether this link owes the pin's settle its answer ([`Then::Settle`]).
    pub(super) fn pin_settle_owed(&self) -> bool {
        self.conn
            .as_ref()
            .is_some_and(|link| link.owed().any(Then::is_settle))
    }

    /// The settle the pin's fence waits on (critic item F07), sent pipelined behind what the link
    /// owes, past the barriers [`StateDir::order_logpin`] returned from: the broker answers it in
    /// send order once its full flush returned ([`Bridge::settled_now`]).
    ///
    /// # Errors
    /// The send's; a link it could not write is judged down.
    fn send_pin_settle(&mut self) -> io::Result<()> {
        let waiting = self.fx.now();
        while self
            .conn
            .as_ref()
            .is_some_and(|link| link.unacked() >= PIPELINE_WINDOW)
            && self.settle_one(waiting)
        {}
        let started = self.fx.now();
        let Some(link) = self.conn.as_mut() else {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "no broker link to settle the pin's fence on",
            ));
        };
        if let Err((e, _)) = link.send_settle(Then::Settle, started) {
            self.judge(started, Err(&e), Some("read"));
            return Err(e);
        }
        Ok(())
    }

    /// THE BROKER SETTLED THE PIN (critic item F07): the pin ordered for it is this open's fence
    /// ([`StateDir::logpin_settled`]) where the name still holds it, and the mirror's pass, which
    /// waited on the answer ([`Bridge::mirror_due`]), is due. One the name no longer holds is
    /// fenced again by the next pass.
    /// Where the answer makes the fence, the take it lets go is the open's first
    /// ([`MirrorRun::fence_settled`]).
    pub(super) fn pin_settled(&mut self) {
        if self.state.logpin_settled() {
            if let Some(m) = self.mirror.as_deref_mut() {
                m.fence_settled = true;
            }
        }
    }

    /// THE PIN'S SETTLE IS LOOKED FOR AT EVERY ROUND THAT OWES IT (critic item F07's review):
    /// while the settle is owed its answer, the round's pass first reads, without waiting, the
    /// answers already in, up to and through the settle's ([`Bridge::try_settle_acks`]'s read),
    /// so the round that finds the settle answered makes the fence ([`Bridge::pin_settled`])
    /// and its pass takes. Left to an idle moment's read or a park that took nothing, the answer
    /// would sit unread while the loop takes an item every round, and the first pass, which
    /// D06's hold lets go once its [`FIRST_PASS_HOLD`] is out whatever is still queued
    /// ([`Bridge::mirror_yields`]), would wait for the queue to drain.
    pub(super) fn read_the_pins_settle(&mut self) {
        #[cfg(test)]
        if self
            .fx
            .mutant(crate::sim::world::Mutant::SettleReadOnlyWhenIdle)
        {
            return;
        }
        if self.pin_settle_owed() {
            self.grouped(|bridge| while bridge.pin_settle_owed() && bridge.try_settle_one() {});
        }
    }

    /// This link's broker refused the settle (critic item F07): its next fence is the bridge's own
    /// full flush ([`Bridge::broker_settles_the_pin`]), until the next attach asks again.
    pub(super) fn pin_settle_refused(&mut self, e: &io::Error) {
        #[cfg(test)]
        if self
            .fx
            .mutant(crate::sim::world::Mutant::RefusedSettleMakesTheFence)
        {
            self.pin_settled();
            return;
        }
        stderr_line!(
            "aterm-link: mirror: the broker refused to settle the pin's fence ({e}); the \
             bridge flushes the pin itself on this link"
        );
        #[cfg(test)]
        if self
            .fx
            .mutant(crate::sim::world::Mutant::SettleRefusalForgotten)
        {
            return;
        }
        self.settle_refused = true;
    }

    /// whether `logfloor` covers the batch — the node's total plus every slot's body bytes — raised now, durably, [`mirror::FLOOR_LEASE_BYTES`] past it if it does not, before any slot is sent.
    pub(super) fn mirror_floor_covers(&mut self) -> bool {
        let Some(m) = self.mirror.as_deref() else {
            return false;
        };
        if m.floored || m.pin.slots.is_empty() {
            return true;
        }
        let bytes: u64 = m.pin.slots.iter().map(|s| s.body.len() as u64).sum();
        let need = m.pin.total.saturating_add(bytes);
        #[cfg(test)]
        let admitted = self.state.logfloor_admitted()
            || self
                .fx
                .mutant(crate::sim::world::Mutant::TrustTheFloorThatReads);
        #[cfg(not(test))]
        let admitted = self.state.logfloor_admitted();
        let proven = !admitted && self.state.admit_logfloor_by_its_proof();
        if proven {
            stderr_line!(
                "aterm-link: mirror: the floor of the node's total is on the media at {}, this \
                 open's first, as its proof ({}) shows: no flush",
                m.floor,
                crate::state::LOGFLOOR_PROOF
            );
        }
        let admitted = admitted || proven;
        if need <= m.floor && admitted {
            if let Some(m) = self.mirror.as_deref_mut() {
                m.floored = true;
            }
            return true;
        }
        let top = need.saturating_add(mirror::FLOOR_LEASE_BYTES);
        let first = !self.state.logfloor_admitted();
        let began = self.fx.now();
        #[cfg(test)]
        let raised = if self
            .fx
            .mutant(crate::sim::world::Mutant::TrustTheFloorThatReads)
        {
            self.state.raise_logfloor_trusting_what_reads(top)
        } else {
            self.state.raise_logfloor(top)
        };
        #[cfg(not(test))]
        let raised = self.state.raise_logfloor(top);
        match raised {
            Ok(floor) => {
                if first {
                    stderr_line!(
                        "aterm-link: mirror: the floor of the node's total is on the media at \
                         {floor}, this open's first: 2 full flushes in {} us{}",
                        self.fx.now().saturating_duration_since(began).as_micros(),
                        aterm_durable::durable_flushes_note()
                    );
                }
                if let Some(m) = self.mirror.as_deref_mut() {
                    m.floor = floor;
                    m.floored = true;
                }
                true
            }
            Err(e) => {
                stderr_line!(
                    "aterm-link: mirror: the floor of the node's total could not be raised \
                     ({e}); nothing sent, tried again"
                );
                self.mirror_retry_in(DEADLINE_RETRY);
                false
            }
        }
    }

    /// Send every owed slot pipelined at its pinned sequence, the session's `ev` face opened only through the subject budget's mirror reserve.
    ///
    /// Every owed slot's face is judged first, each new face counted against the budget together with the faces the pass already allowed, which the broker's count does not hold until they land, the budget read at most once for the pass and again only for a face the count since says it is short for (one budget scope, [`Bridge::budget_scoped`], critic item G04); the slots it allows go out in bursts, one write each ([`crate::pipeline::Link::send_burst`]), each as many as the room left under [`PIPELINE_WINDOW`] holds, and each waiting for [`BURST_ROOM`] first. The first burst carries the inbox commit a round holds, or, with no inbox record queued, the one the round's settle would hold for the loop's next write ([`Bridge::take_commit_for_the_mirror`]: only one still due, clamped), ahead of its slots, so a snapshot a delivery owed shares the commit's write rather than going in a write of its own ahead of it (critic item C13). The commit is taken just before the write, after every ack the window waited on was read, so it crosses no item the loop takes. A burst the link does not take is judged, the commit concluded failed, and every slot in it left owed and counted sent ([`MirrorRun::sent`]): a write cut short lands each frame the broker received whole ahead of the cut, so a resend's dedup is read back, never taken for another writer's.
    pub(super) fn mirror_send(&mut self) {
        self.budget_scoped(Self::mirror_send_scoped);
    }

    /// [`Bridge::mirror_send`] inside its budget scope.
    fn mirror_send_scoped(&mut self) {
        let k = self.mirror.as_deref().map_or(0, |m| m.pin.slots.len());
        if k > 0 && !(self.mirror_key_on_the_media() && self.mirror_floor_covers()) {
            return;
        }
        let mut judged: std::collections::BTreeMap<String, bool> =
            std::collections::BTreeMap::new();
        let mut opening = 0u64;
        let mut allowed: Vec<(usize, String)> = Vec::new();
        for i in 0..k {
            if self.conn.is_none() || self.mirror.as_deref().is_some_and(|m| m.repin) {
                return;
            }
            let Some(sid) = self
                .mirror
                .as_deref()
                .and_then(|m| (m.outcome[i] == Outcome::Owed).then(|| m.pin.slots[i].sid.clone()))
            else {
                continue;
            };
            let face = subject::session_face(&self.cfg.fleet, &self.node, &sid, "ev");
            let opens = match judged.get(&face) {
                Some(opens) => *opens,
                None => {
                    let opens = self.budget_allows(&face, Opens::Mirror, opening);
                    if opens && !self.indexed_subjects.contains(&face) {
                        opening += 1;
                    }
                    judged.insert(face.clone(), opens);
                    opens
                }
            };
            if opens {
                allowed.push((i, face));
            } else if let Some(m) = self.mirror.as_deref_mut() {
                m.outcome[i] = Outcome::Refused;
            }
        }
        let mut rest = allowed.as_slice();
        while !rest.is_empty() {
            if self.conn.is_none() || self.mirror.as_deref().is_some_and(|m| m.repin) {
                return;
            }
            let waiting = self.fx.now();
            while self
                .conn
                .as_ref()
                .is_some_and(|link| link.unacked() + BURST_ROOM > PIPELINE_WINDOW)
                && self.settle_one(waiting)
            {}
            if self.conn.is_none() || self.mirror.as_deref().is_some_and(|m| m.repin) {
                return;
            }
            // A slot's pinned sequence can lie below a publish still in flight: on a broker
            // that gap-fences, those answers are read first, and a fence a refusal set is
            // lifted (`Bridge::settle_above`, `Bridge::lift_gap_fences`), so no slot lands
            // under a fence that holds back a write sent before it.
            if let Some(lowest) = self.mirror.as_deref().and_then(|m| {
                rest.iter()
                    .filter_map(|(i, _)| m.pin.slots.get(*i).map(|slot| slot.pseq))
                    .min()
            }) {
                self.settle_above(lowest);
            }
            self.lift_gap_fences();
            if self.conn.is_none() || self.mirror.as_deref().is_some_and(|m| m.repin) {
                return;
            }
            let room = self
                .conn
                .as_ref()
                .map_or(0, |link| PIPELINE_WINDOW.saturating_sub(link.unacked()));
            if room < BURST_ROOM {
                return;
            }
            // Behind the broker's rate limit ([`Bridge::rate`]): every slot stays owed, sent once
            // the hold is over.
            if let Some(left) = self.rate_held() {
                self.mirror_retry_in(left);
                return;
            }
            #[cfg(test)]
            let bare = self
                .fx
                .mutant(crate::sim::world::Mutant::MirrorSendsItsSlotsBare);
            #[cfg(not(test))]
            let bare = false;
            let held = if bare {
                None
            } else {
                self.take_commit_for_the_mirror()
                    .map(|upto| (upto, subject::inbox_group(&self.cfg.fleet, &self.node)))
            };
            let (burst, later) = rest.split_at(rest.len().min(room - usize::from(held.is_some())));
            rest = later;
            let (retry, producer_id, inc) = (self.commit_refused, self.producer_id, self.inc);
            for (_, face) in burst {
                self.count_opening(face);
            }
            let started = self.fx.now();
            let (Some(m), Some(link)) = (self.mirror.as_deref(), self.conn.as_mut()) else {
                return;
            };
            let commit = held.as_ref().map(|(upto, group)| HeldCommit {
                group,
                upto: *upto,
                tag: Then::Commit { upto: *upto, retry },
            });
            let publishes = burst
                .iter()
                .map(|(i, face)| Publish {
                    producer_id,
                    seq: m.pin.slots[*i].pseq,
                    first: !m.sent[*i],
                    subject: face,
                    body: m.pin.slots[*i].body.as_bytes(),
                    tag: Then::Mirror { slot: *i },
                })
                .collect();
            let written = link.send_burst(commit, publishes, started);
            if let Some(m) = self.mirror.as_deref_mut() {
                for (i, _) in burst {
                    m.sent[*i] = true;
                }
            }
            if let Err(unsent) = written {
                self.burst_unsent(started, unsent.error, unsent.commit);
                return;
            }
            if let Some(m) = self.mirror.as_deref_mut() {
                for (i, _) in burst {
                    m.outcome[*i] = Outcome::Sending;
                }
                m.sent_inc = inc;
            }
        }
    }

    /// One slot's ack, read off the publisher connection by the round ([`Bridge::settle_one`]): an append, a deduped answer to read back, a first send's deduped answer (another writer's record: the collision path, and the slot and every later one of its session re-pinned), the budget's refusal, a refusal FOR GOOD (the mirror stops, the same record would be refused again), or one that is not — astream's `ERR_REFUSED` (a failed append or fsync, a broker stopping) and a lapsed capability, which land once resent or renewed — and a transport failure: sent again, a refusal after [`super::DEADLINE_RETRY`].
    pub(super) fn conclude_mirror(
        &mut self,
        slot: usize,
        seq: u64,
        first: bool,
        answer: io::Result<(u64, bool)>,
    ) {
        let outcome = match answer {
            Ok((off, deduped)) => {
                self.bus_high = self.bus_high.max(off.saturating_add(1));
                if !deduped {
                    // Appended at the face: it is in the index from now on.
                    if let Some(face) = self.mirror.as_deref().and_then(|m| {
                        m.pin.slots.get(slot).map(|s| {
                            subject::session_face(&self.cfg.fleet, &self.node, &s.sid, "ev")
                        })
                    }) {
                        self.indexed_subjects.insert(face);
                    }
                }
                if deduped && first {
                    let _ = self.collision(seq, off);
                    if let Some(m) = self.mirror.as_deref_mut() {
                        m.repin = true;
                    }
                    Outcome::Collided
                } else if deduped {
                    Outcome::Deduped(off)
                } else {
                    Outcome::OnBus(off)
                }
            }
            Err(e) if past_the_budget(&e) => Outcome::Refused,
            Err(e) if refused_for_good(&e) => {
                self.mirror_stop(
                    &format!("the broker refused a mirrored record for good: {e}"),
                    MirrorReason::Refused,
                );
                Outcome::Owed
            }
            Err(e) if astream_broker::broker_refusal(&e).is_some() => {
                stderr_line!(
                    "aterm-link: mirror: the broker refused a slot for now ({e}); sent again"
                );
                // A rate-limited one once the bucket has refilled, as the broker names it.
                self.mirror_retry_in(crate::rate_hold::wait_of(&e).unwrap_or(DEADLINE_RETRY));
                Outcome::Owed
            }
            Err(_) => Outcome::Owed,
        };
        let Some(m) = self.mirror.as_deref_mut() else {
            return;
        };
        if let Some(o) = m.outcome.get_mut(slot) {
            *o = outcome;
        }
    }

    /// After a collision, on the connection the reattach brought: the collided slot and every later slot of its session at fresh sequences, in order, never sent from this incarnation.
    pub(super) fn mirror_repin(&mut self) -> bool {
        if self.conn.is_none() {
            return false;
        }
        let Some(m) = self.mirror.as_deref_mut() else {
            return false;
        };
        let mut from: Vec<(String, usize)> = Vec::new();
        for (i, (s, o)) in m.pin.slots.iter().zip(&m.outcome).enumerate() {
            if *o == Outcome::Collided && !from.iter().any(|(sid, _)| *sid == s.sid) {
                from.push((s.sid.clone(), i));
            }
        }
        let todo: Vec<usize> = (0..m.pin.slots.len())
            .filter(|&i| {
                from.iter()
                    .any(|(sid, at)| i >= *at && m.pin.slots[i].sid == *sid)
            })
            .collect();
        let unsent = self.unsent.take();
        let mut fresh = Vec::with_capacity(todo.len());
        for _ in &todo {
            match self.next_seq() {
                Ok(seq) => fresh.push(seq),
                Err(e) => {
                    self.unsent = unsent;
                    stderr_line!(
                        "aterm-link: mirror: no sequence to re-pin at ({e}); the batch waits, \
                         nothing of it landed"
                    );
                    self.mirror_retry_in(DEADLINE_RETRY);
                    return false;
                }
            }
        }
        self.unsent = unsent;
        let inc = self.inc;
        let now = self.fx.now();
        let Some(m) = self.mirror.as_deref_mut() else {
            return false;
        };
        let mut pin = m.pin.clone();
        for (&i, &seq) in todo.iter().zip(&fresh) {
            pin.slots[i].pseq = seq;
        }
        pin.inc = inc;
        if let Err(e) = self.state.set_logpin(&pin.render()) {
            stderr_line!(
                "aterm-link: mirror: the re-pin could not be written ({e}); nothing re-sent, the \
                 batch waits"
            );
            m.retry_at = Some(now + DEADLINE_RETRY);
            return false;
        }
        m.pin = pin;
        for i in todo {
            m.sent[i] = false;
            m.unread[i] = None;
            m.outcome[i] = Outcome::Owed;
        }
        m.repin = false;
        true
    }

    /// Every slot concluded: read back each deduped answer, then land.
    pub(super) fn mirror_settle(&mut self) {
        if self.mirror.as_deref().is_none_or(|m| !m.concluded()) {
            return;
        }
        let checks: Vec<(usize, u64, String, String, u64)> = self
            .mirror
            .as_deref()
            .map(|m| {
                m.outcome
                    .iter()
                    .enumerate()
                    .filter_map(|(i, o)| match o {
                        Outcome::Deduped(off) => {
                            let s = &m.pin.slots[i];
                            Some((i, *off, s.sid.clone(), s.body.clone(), s.pseq))
                        }
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut again = false;
        for (i, off, sid, body, pseq) in checks {
            let face = subject::session_face(&self.cfg.fleet, &self.node, &sid, "ev");
            // A cut is no record there yet: the slot is sent again at its pinned sequence, and
            // since astream dropped its key with it, it appends anew. A record retention still
            // holds below the face's oldest is read, never taken for a cut ([`Bridge::read_at`]).
            let read = match self.read_at(off, &face) {
                Ok(ReadAt::Rows(rows)) => {
                    match rows.iter().find(|(at, f, _)| *at == off && *f == face) {
                        Some((_, _, bytes)) if bytes == body.as_bytes() => ReadBack::Ours,
                        Some(_) => ReadBack::Other,
                        None => ReadBack::Unread,
                    }
                }
                Ok(ReadAt::Cut(_)) => ReadBack::Unread,
                // As before told fetches: a broker that does not tell them leaves the refusal
                // undecided, and it is read as no record there.
                Ok(ReadAt::Untold(floor)) => {
                    stderr_line!(
                        "aterm-link: mirror: the read back of sequence {pseq} at @{off} meets \
                         {face}'s oldest @{floor} on a broker that does not tell fetches: read \
                         as no record there, which retention may still hold"
                    );
                    ReadBack::Unread
                }
                // No answer about the record, and none of the link's: the pass waits, the link
                // kept, rather than taking it for no record and sending a second copy.
                Ok(ReadAt::Unasked(e)) => {
                    stderr_line!(
                        "aterm-link: mirror: the read back of sequence {pseq} at @{off} meets \
                         {face}'s oldest, and its told read could not be made ({e}); tried \
                         again later"
                    );
                    self.mirror_retry_in(DEADLINE_RETRY);
                    return;
                }
                Err(_) => return,
            };
            let doubted = self
                .mirror
                .as_deref()
                .and_then(|m| m.unread.get(i).copied().flatten());
            let read = match read {
                ReadBack::Unread if doubted == Some(off) => ReadBack::Other,
                read => read,
            };
            match read {
                ReadBack::Ours => {
                    self.indexed_subjects.insert(face.clone());
                    if let Some(m) = self.mirror.as_deref_mut() {
                        m.outcome[i] = Outcome::Verified(off);
                        m.recovered_deduped += 1;
                    }
                }
                ReadBack::Other => {
                    if let Some(m) = self.mirror.as_deref_mut() {
                        m.outcome[i] = Outcome::Collided;
                        m.repin = true;
                    }
                    let _ = self.collision(pseq, off);
                    return;
                }
                ReadBack::Unread => {
                    stderr_line!(
                        "aterm-link: mirror: sequence {pseq} was deduped at @{off}, where {face} \
                         holds no record: sent again at its pinned sequence, not yet a collision"
                    );
                    if let Some(m) = self.mirror.as_deref_mut() {
                        m.unread[i] = Some(off);
                        m.outcome[i] = Outcome::Owed;
                    }
                    again = true;
                }
            }
        }
        if again {
            return;
        }
        let refused: Vec<String> = self
            .mirror
            .as_deref()
            .map(|m| {
                m.pin
                    .slots
                    .iter()
                    .zip(&m.outcome)
                    .filter(|(s, o)| **o == Outcome::Refused && !m.noticed.contains(&s.sid))
                    .map(|(s, _)| s.sid.clone())
                    .collect::<BTreeSet<String>>()
                    .into_iter()
                    .collect()
            })
            .unwrap_or_default();
        for sid in refused {
            self.publish_ev(&format!("log-refused sid={sid} reason=subject-budget"));
            if let Some(m) = self.mirror.as_deref_mut() {
                m.noticed.insert(sid);
            }
        }
        let (pairs, refused_any) = self
            .mirror
            .as_deref()
            .map(|m| {
                let mut refused: BTreeSet<&str> = BTreeSet::new();
                let mut landable: Vec<PinSlot> = Vec::with_capacity(m.pin.slots.len());
                for (s, o) in m.pin.slots.iter().zip(&m.outcome) {
                    if *o == Outcome::Refused {
                        refused.insert(&s.sid);
                    } else if !refused.contains(s.sid.as_str()) {
                        landable.push(s.clone());
                    }
                }
                (mirror::landed_pairs(&landable), !refused.is_empty())
            })
            .unwrap_or_default();
        if pairs.is_empty() {
            let now = self.fx.now();
            if let Some(m) = self.mirror.as_deref_mut() {
                m.clear_batch();
                m.owed = true;
                if refused_any {
                    m.retry_at = Some(now + REFUSED_WAIT);
                }
                if let Err(e) = self.state.set_logpin(&m.pin.render()) {
                    stderr_line!("aterm-link: mirror: the emptied pin could not be written ({e})");
                }
            }
            return;
        }
        self.service_urgent();
        let reply = match self.ctl_request(&mirror::landed_line(&pairs)) {
            Ok(reply) => reply.header().to_string(),
            Err(e) => {
                stderr_line!("aterm-link: mirror: log-landed failed: {e}");
                self.mirror_retry_in(DEADLINE_RETRY);
                return;
            }
        };
        let words = match mirror::parse_landed(&reply, pairs.len()) {
            Ok(Some(words)) => words,
            Ok(None) => {
                self.mirror_retry_in(DEADLINE_RETRY);
                return;
            }
            Err(e) => {
                self.mirror_stop(&e, MirrorReason::Endpoint);
                return;
            }
        };
        for ((sid, _), w) in pairs.iter().zip(&words) {
            if *w == LandedWord::Done {
                self.mirror_retire_face(sid);
            }
        }
        if !words.iter().all(|w| w.settled()) {
            self.mirror_retry_in(DEADLINE_RETRY);
            return;
        }
        let Some(m) = self.mirror.as_deref_mut() else {
            return;
        };
        if m.recovered > 0 {
            stderr_line!(
                "aterm-link: mirror: the recovered batch landed: {} slots republished, {} \
                 deduped at their pinned sequences",
                m.recovered,
                m.recovered_deduped
            );
        }
        let bytes: u64 = m
            .pin
            .slots
            .iter()
            .zip(&m.outcome)
            .filter(|(_, o)| **o != Outcome::Refused)
            .map(|(s, _)| s.body.len() as u64)
            .sum();
        m.pin.total = m.pin.total.saturating_add(bytes);
        let retains = self.broker_retains == Some(true);
        let hi = m
            .outcome
            .iter()
            .filter_map(|o| match o {
                Outcome::OnBus(off) | Outcome::Verified(off) => Some(*off),
                _ => None,
            })
            .max()
            .filter(|_| retains);
        if let Some(hi) = hi {
            m.pin.note_window(hi, bytes);
        }
        m.clear_batch();
        m.recovered = 0;
        m.recovered_deduped = 0;
        m.owed = true;
        if let Err(e) = self.state.set_logpin(&m.pin.render()) {
            stderr_line!("aterm-link: mirror: the emptied pin could not be written ({e})");
        }
    }

    /// A closed session's tail is all on the bus (`done`): its `ev` face
    /// leaves the broker's last-value index and gives its subject back to
    /// the node's budget (astream's `/a/retire`).
    fn mirror_retire_face(&mut self, sid: &str) {
        let face = subject::session_face(&self.cfg.fleet, &self.node, sid, "ev");
        let producer = self.producer_id;
        let fx = self.fx.clone();
        let Some(conn) = self.conn_now() else {
            return;
        };
        let started = fx.now();
        let answer = conn.retire_subject(producer, &face);
        if let Err(e) = self.observe(started, answer, None) {
            stderr_line!("aterm-link: mirror: retiring {face} failed: {e}");
        } else {
            self.indexed_subjects.remove(&face);
        }
    }

    /// The bus log was replaced: the pinned batch names the old one. Its write refused refuses the
    /// attach (`Bridge::forget_log_offsets` returns it) before the attach moves the node's
    /// witness to the new log (`Bridge::save_log_witness`), so the next attach meets the move
    /// again and drops the batch again (aterm-link B05 review: an attach
    /// that went on past it left the batch on disk, which the hello then judged and recovered
    /// onto the moved log, a slot of a lost batch landed and the batch never mirrored again).
    ///
    /// # Errors
    /// The dropped pin's write.
    pub(super) fn mirror_forget_log(&mut self) -> io::Result<()> {
        let Some(m) = self.mirror.as_deref_mut() else {
            return self.mirror_forget_pinned_batch();
        };
        m.oldest = 0;
        if m.pin.slots.is_empty() && m.pin.win.is_empty() {
            // A pin no hello has judged yet is on disk alone (an `auto` mirror waits for the
            // broker's rules, and then the hello, after the attach saw the move): the mirror
            // that judges it later would recover its batch onto this log (sim item B05). So is
            // one whose drop an earlier attach's write did not put there.
            return self.mirror_forget_pinned_batch();
        }
        m.pin.win.clear();
        m.clear_batch();
        m.repin = false;
        m.owed = true;
        let written = self.state.set_logpin(&m.pin.render());
        self.refused_drop(written)
    }

    /// [`Bridge::mirror_forget_log`] for the pin on disk: its batch and its window ledger go,
    /// its total and key stay. A pin that does not read is the hello's to judge, as before.
    fn mirror_forget_pinned_batch(&mut self) -> io::Result<()> {
        #[cfg(test)]
        if self
            .fx
            .mutant(crate::sim::world::Mutant::ForgetOnlyAJudgedPin)
        {
            return Ok(());
        }
        let Ok(Some(text)) = self.state.logpin() else {
            return Ok(());
        };
        let Ok(pin) = Pin::parse(&text) else {
            return Ok(());
        };
        if pin.slots.is_empty() && pin.win.is_empty() {
            return Ok(());
        }
        let pin = Pin {
            win: Vec::new(),
            slots: Vec::new(),
            ..pin
        };
        let written = self.state.set_logpin(&pin.render());
        self.refused_drop(written)
    }

    /// A dropped pin's write, said where the disk refused it; returned, so the attach does not
    /// go on past it (but with the rule switched off, `attach_past_an_unwritten_drop`, a test's
    /// negative control).
    fn refused_drop(&self, written: io::Result<()>) -> io::Result<()> {
        let Err(e) = written else {
            return Ok(());
        };
        stderr_line!("aterm-link: mirror: the dropped pin could not be written ({e})");
        #[cfg(test)]
        if self.attach_past_an_unwritten_drop {
            return Ok(());
        }
        Err(e)
    }
}
