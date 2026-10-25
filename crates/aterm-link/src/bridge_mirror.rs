// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE BUS MIRROR, BRIDGE HALF, its wiring (the pure parts are [`crate::mirror`]).

use std::collections::BTreeSet;
use std::io;
use std::time::{Duration, Instant};

use super::{
    past_the_budget, read_back_rows, refused_for_good, Bridge, Opens, Then, DEADLINE_RETRY,
    LINK_MIRROR_WAIT_LANE, PIPELINE_WINDOW,
};
use crate::ctl::Reply;
use crate::mirror::{self, Bucket, LandedWord, LogMode, Pin, PinSlot};
use crate::state::StateDir;
use crate::subject;

/// How long a batch none of whose slots could land (every session in it refused by the subject budget) waits before the next take offers them again: the budget is asked again then, never every round.
pub(super) const REFUSED_WAIT: Duration = Duration::from_secs(2);

/// How long a take that could take nothing NOW waits before the next one asks: the rate bucket empty, or the endpoint's answer empty with `more=1` (a session's timeline lock was held at the take, which never waits on the UI's lock).
pub(super) const BUSY_WAIT: Duration = Duration::from_millis(100);

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
    /// change (`link mirror unretained`).
    unretained: bool,
    /// Why it stopped for this bridge's life, if it did.
    stopped: Option<String>,
    /// Work is owed: a take, a send or a landing.
    owed: bool,
    /// A busy answer (`fenced`, `adopting`, an empty bucket) waits until then.
    retry_at: Option<Instant>,
    /// The batch in flight and the node's total.
    pin: Pin,
    /// Per slot: whether THIS process sent it (a never-sent slot's deduped
    /// answer is another writer's).
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
    /// The window budget holds the take: said once to the endpoint (`link
    /// mirror budget`) until the mirror goes on (`link mirror on`).
    waiting: bool,
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
    /// No record at that offset on the face: retention may have taken the
    /// record since the dedup answer (the broker drops its dedup entry with
    /// it), or the record there is on a subject this face does not hold.
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

    /// AFTER EVERY ATTACH, an `auto` mirror follows the broker's retention.
    pub(super) fn mirror_attached(&mut self) {
        let Some(m) = self.mirror.as_deref() else {
            return;
        };
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
                let _ = self.ctl_request("link mirror on");
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
            let _ = self.ctl_request("link mirror unretained");
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
    /// mirror runs (`link mirror on`) or stopped for it (`reason=pin`).
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
                "link mirror stopped reason=pin"
            }
            None => "link mirror on",
        };
        let _ = self.ctl_request(report);
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

    /// Something may have been appended to a session log: a take is owed.
    pub(super) fn mirror_wake(&mut self) {
        if let Some(m) = self.mirror.as_deref_mut() {
            m.owed = true;
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

    /// When the loop owes the mirror a pass: `Some(now)` for work it can do now (the loop's wait is then zero), the retry's instant while a busy answer waits, `None` for nothing.
    pub(super) fn mirror_due(&self, now: Instant) -> Option<Instant> {
        if !self.mirror_engaged() || self.conn.is_none() {
            return None;
        }
        let m = self.mirror.as_deref()?;
        if !m.owed {
            return None;
        }
        if m.outcome.contains(&Outcome::Sending) && m.sent_inc == self.inc {
            return None;
        }
        Some(m.retry_at.map_or(now, |t| t.max(now)))
    }

    /// Stop the mirror for this bridge's life: said on stderr and to the
    /// endpoint (`fabric status mirror=stopped:<reason>` or `capped`), the
    /// pin kept for a later bridge.
    fn mirror_stop(&mut self, why: &str, report: &str) {
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
        let _ = self.ctl_request(report);
    }

    /// THE PASS: re-pin after a collision, take when nothing is pinned, send what is owed, and settle a batch whose every slot concluded.
    pub(super) fn mirror_step(&mut self) {
        let now = self.fx.now();
        if self.mirror_due(now).is_none_or(|at| at > now) {
            return;
        }
        let inc = self.inc;
        if let Some(m) = self.mirror.as_deref_mut() {
            m.retry_at = None;
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
                let _ = self.ctl_request("link mirror on");
            }
            if total >= mirror::TOTAL_MAX_BYTES {
                self.mirror_stop(
                    &format!(
                        "the node mirrored its total of {} bytes with no retention in the broker",
                        mirror::TOTAL_MAX_BYTES
                    ),
                    "link mirror capped reason=total",
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
                        "link mirror stopped reason=endpoint",
                    );
                }
                return;
            }
        };
        let (slots, more) = match mirror::parse_take(&header, &body) {
            Ok(taken) => taken,
            Err(e) => {
                self.mirror_stop(&e, "link mirror stopped reason=endpoint");
                return;
            }
        };
        if slots.is_empty() {
            if let Some(m) = self.mirror.as_deref_mut() {
                m.owed = more;
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
        }
    }

    /// THE WINDOW BUDGET, on a
    /// retaining broker: whether this node's landed bytes in the broker's
    /// window are under [`mirror::NODE_WINDOW_BYTES`].
    fn mirror_window_open(&mut self) -> bool {
        let over = |m: &MirrorRun| m.pin.window_bytes(m.oldest) >= mirror::NODE_WINDOW_BYTES;
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
            if let (Some(m), Some(oldest)) = (self.mirror.as_deref_mut(), oldest) {
                m.oldest = m.oldest.max(oldest);
                m.pin.prune_window(m.oldest);
            }
        }
        let Some(m) = self.mirror.as_deref_mut() else {
            return false;
        };
        let window = m.pin.window_bytes(m.oldest);
        let open = window < mirror::NODE_WINDOW_BYTES;
        if open == !m.waiting {
            return open;
        }
        m.waiting = !open;
        let report = if open {
            stderr_line!(
                "aterm-link: mirror: the broker's window has room again; the mirror goes on"
            );
            "link mirror on"
        } else {
            stderr_line!(
                "aterm-link: mirror: this node's {window} bytes in the broker's window (from \
                 @{}) reach its budget of {}; the mirror waits for the broker's retention to \
                 give the oldest of them up",
                m.oldest,
                mirror::NODE_WINDOW_BYTES
            );
            if self.endpoint_lane < LINK_MIRROR_WAIT_LANE {
                return open;
            }
            "link mirror budget"
        };
        let _ = self.ctl_request(report);
        open
    }

    fn mirror_retry_in(&mut self, wait: Duration) {
        let now = self.fx.now();
        if let Some(m) = self.mirror.as_deref_mut() {
            m.retry_at = Some(now + wait);
        }
    }

    /// whether `logkey` holds the key this mirror names, written durably now if it does not yet — before the first take that names the key, and before any slot of a recovered batch an older build's pin aliased with it is sent, so a power cut that tears or rolls back the pin can never leave the bus holding aliases of a key the next bridge does not know.
    pub(super) fn mirror_key_on_the_media(&mut self) -> bool {
        let Some(m) = self.mirror.as_deref() else {
            return false;
        };
        if let Err(e) = self.state.ensure_logpin_durable(|| m.pin.render()) {
            stderr_line!(
                "aterm-link: mirror: the pin's version fence could not be made durable \
                 ({e}); nothing taken or sent, tried again"
            );
            self.mirror_retry_in(DEADLINE_RETRY);
            return false;
        }
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
                         {floor}, this open's first: 2 full flushes in {} us",
                        self.fx.now().saturating_duration_since(began).as_micros()
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
    pub(super) fn mirror_send(&mut self) {
        let k = self.mirror.as_deref().map_or(0, |m| m.pin.slots.len());
        if k > 0 && !(self.mirror_key_on_the_media() && self.mirror_floor_covers()) {
            return;
        }
        let mut judged: std::collections::BTreeMap<String, bool> =
            std::collections::BTreeMap::new();
        for i in 0..k {
            if self.conn.is_none() || self.mirror.as_deref().is_some_and(|m| m.repin) {
                return;
            }
            let Some((pseq, sid, body, first)) = self.mirror.as_deref().and_then(|m| {
                (m.outcome[i] == Outcome::Owed).then(|| {
                    let s = &m.pin.slots[i];
                    (s.pseq, s.sid.clone(), s.body.clone(), !m.sent[i])
                })
            }) else {
                continue;
            };
            let face = subject::session_face(&self.cfg.fleet, &self.node, &sid, "ev");
            let allowed = match judged.get(&face) {
                Some(allowed) => *allowed,
                None => {
                    let allowed = self.budget_allows(&face, Opens::Mirror);
                    judged.insert(face.clone(), allowed);
                    allowed
                }
            };
            if !allowed {
                if let Some(m) = self.mirror.as_deref_mut() {
                    m.outcome[i] = Outcome::Refused;
                }
                continue;
            }
            let waiting = self.fx.now();
            while self
                .conn
                .as_ref()
                .is_some_and(|link| link.unacked() >= PIPELINE_WINDOW)
                && self.settle_one(waiting)
            {}
            let producer = self.producer_id;
            let started = self.fx.now();
            let Some(link) = self.conn.as_mut() else {
                return;
            };
            match link.send(
                producer,
                pseq,
                first,
                &face,
                body.as_bytes(),
                Then::Mirror { slot: i },
                started,
            ) {
                Ok(()) => {
                    let inc = self.inc;
                    if let Some(m) = self.mirror.as_deref_mut() {
                        m.sent[i] = true;
                        m.outcome[i] = Outcome::Sending;
                        m.sent_inc = inc;
                    }
                }
                Err((e, _)) => {
                    self.judge(started, Err(&e), None);
                    return;
                }
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
                    "link mirror stopped reason=refused",
                );
                Outcome::Owed
            }
            Err(e) if astream_broker::broker_refusal(&e).is_some() => {
                stderr_line!(
                    "aterm-link: mirror: the broker refused a slot for now ({e}); sent again"
                );
                self.mirror_retry_in(DEADLINE_RETRY);
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
            let fx = self.fx.clone();
            let Some(conn) = self.conn_now() else {
                return;
            };
            let started = fx.now();
            let answer = read_back_rows(conn.fetch(off, &face, 1), off);
            let read = match self.observe(started, answer, Some("read")) {
                Ok(rows) => match rows.iter().find(|(at, f, _)| *at == off && *f == face) {
                    Some((_, _, bytes)) if bytes == body.as_bytes() => ReadBack::Ours,
                    Some(_) => ReadBack::Other,
                    None => ReadBack::Unread,
                },
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
                self.mirror_stop(&e, "link mirror stopped reason=endpoint");
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

    /// The bus log was replaced: the pinned batch names the old one.
    pub(super) fn mirror_forget_log(&mut self) {
        let Some(m) = self.mirror.as_deref_mut() else {
            return;
        };
        m.oldest = 0;
        if m.pin.slots.is_empty() && m.pin.win.is_empty() {
            return;
        }
        m.pin.win.clear();
        m.clear_batch();
        m.repin = false;
        m.owed = true;
        if let Err(e) = self.state.set_logpin(&m.pin.render()) {
            stderr_line!("aterm-link: mirror: the dropped pin could not be written ({e})");
        }
    }
}
