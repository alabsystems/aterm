// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The pre-app inbox: the lane for a message minted on a thread with no `App`, or before `App`
//! exists, such as startup config warnings, crash evidence, launch load failures, the backend
//! build thread, the accessibility panic hook, GPU-loss recovery, the fabric thread, and the
//! harness host's restored-agent rows ([`queue_restored_row`]). Each is queued with the wall
//! clock at queue time and posted into the message center at the next park
//! (`App::drain_message_inbox`), so it keeps the instant it happened. An exceptions entry raised
//! there (the fabric endpoint's torn state files) has a lane of its own ([`queue_exception`]),
//! handed to `App::except` at the same park, and so has its end ([`queue_exception_clear`],
//! `App::except_clear`).
//!
//! A `Mutex<Vec<_>>` behind an `AtomicBool`, so the drain — which runs on
//! every park — is one atomic load (`Acquire`, pairing with the producer's
//! `Release` store) in the overwhelmingly common empty case and never
//! touches the lock. Bounded ([`INBOX_CAP`]): a pathological loop
//! that queued forever must not grow this vector between two parks; every
//! shipping producer is `Once`-guarded or runs once per launch, so the cap
//! is a backstop, not a policy. Exact duplicates fold on
//! `(tag, title, detail[0])` — the center's own duplicate rule, applied
//! before it can see them. Poison-tolerant: a panic while queuing must never
//! take the app down — the panic hook queues here. The lock nests nothing.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use aterm_messages::{Message, WallStamp};

/// The most messages the inbox holds between two parks.
pub(crate) const INBOX_CAP: usize = 32;

/// One queued message and the wall clock it was queued at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InboxMessage {
    /// The message, normalized at queue time.
    pub(crate) message: Message,
    /// The wall clock at queue time — the message's ingress stamp (D4).
    pub(crate) stamp: WallStamp,
    /// The restored agents' row `message` words as the host said it
    /// ([`queue_restored_row`]): the window words it again from this
    /// (`App::post_restored_row`). `None` for every other message.
    pub(crate) restored: Option<crate::harness_host::RestoredRow>,
}

static INBOX_PENDING: AtomicBool = AtomicBool::new(false);
static WORKSTATION_SETUP_FINISHED: AtomicBool = AtomicBool::new(false);

/// THE EXCEPTIONS LANE: a fact for the owner's later look
/// ([`aterm_messages::exceptions::Entry`], an event family's) raised, or
/// ended, on a thread with no `App` — the fabric endpoint's, when the bridge
/// says it set torn state files aside and when it says its quarantine holds
/// none of them any more — and handed to `App::except` or `App::except_clear`
/// at the next park, with the lane it was queued in. Its own flag, so a
/// drain of the message lane never lowers it. Bounded at [`INBOX_CAP`], and a
/// word already queued under the same key is replaced by the later one, so
/// the last word on a fact is the one the park acts on.
static EXCEPTIONS: Mutex<Vec<(u64, ExceptionWord)>> = Mutex::new(Vec::new());
static EXCEPTIONS_PENDING: AtomicBool = AtomicBool::new(false);

/// One word the exceptions lane carries ([`EXCEPTIONS`]).
#[derive(Clone, Debug)]
#[cfg_attr(
    not(any(unix, test)),
    expect(
        dead_code,
        reason = "its producers, queue_exception and queue_exception_clear, are built where \
                  fabric_bind's reports are (unix and tests)"
    )
)]
pub(crate) enum ExceptionWord {
    /// The fact holds: `App::except` (a later raise restates it).
    Raise(Box<aterm_messages::exceptions::Entry>),
    /// The fact's cause ended, and its reporter says so: `App::except_clear`
    /// of the entry's key.
    Clear(String),
}

#[cfg(any(unix, test))]
impl ExceptionWord {
    /// The key of the fact it is about.
    fn key(&self) -> String {
        match self {
            Self::Raise(entry) => entry.key(),
            Self::Clear(key) => key.clone(),
        }
    }
}

/// Queue `word` for the next park ([`EXCEPTIONS`]): the later word on a key
/// replaces the earlier one in its place. Safe from any thread and from
/// before `App` exists.
#[cfg(any(unix, test))]
fn queue_exception_word(word: ExceptionWord) {
    let lane = current_lane();
    let Ok(mut q) = EXCEPTIONS.lock() else {
        return; // a poisoned lane must never take the app down
    };
    let key = word.key();
    if let Some(slot) = q
        .iter_mut()
        .find(|(at, queued)| *at == lane && queued.key() == key)
    {
        slot.1 = word;
    } else if q.iter().filter(|(at, _)| *at == lane).count() < INBOX_CAP {
        q.push((lane, word));
    }
    EXCEPTIONS_PENDING.store(true, Ordering::Release);
}

/// Queue `entry` for `App::except` at the next park ([`EXCEPTIONS`]). Safe
/// from any thread and from before `App` exists. Built where its one caller
/// is (`fabric_bind`'s torn-state report, unix and tests).
#[cfg(any(unix, test))]
pub(crate) fn queue_exception(entry: aterm_messages::exceptions::Entry) {
    queue_exception_word(ExceptionWord::Raise(Box::new(entry)));
}

/// Queue the end of the fact under `key` for `App::except_clear` at the next
/// park ([`EXCEPTIONS`]): its cause ended (the owner emptied the fabric's
/// quarantine). Built where its one caller is (`fabric_bind`, unix and
/// tests).
#[cfg(any(unix, test))]
pub(crate) fn queue_exception_clear(key: String) {
    queue_exception_word(ExceptionWord::Clear(key));
}

/// Take every word queued in the calling thread's lane ([`queue_exception`],
/// [`queue_exception_clear`]), oldest first, as [`take_queued`] takes
/// messages.
pub(crate) fn take_queued_exceptions() -> Vec<ExceptionWord> {
    if !EXCEPTIONS_PENDING.load(Ordering::Acquire) {
        return Vec::new();
    }
    let lane = current_lane();
    let Ok(mut q) = EXCEPTIONS.lock() else {
        EXCEPTIONS_PENDING.store(false, Ordering::Relaxed);
        return Vec::new();
    };
    let (mine, others): (Vec<_>, Vec<_>) = std::mem::take(&mut *q)
        .into_iter()
        .partition(|(at, _)| *at == lane);
    *q = others;
    if q.is_empty() {
        EXCEPTIONS_PENDING.store(false, Ordering::Relaxed);
    }
    mine.into_iter().map(|(_, word)| word).collect()
}

/// A setup worker verified completion or restore. Its earlier queued failures
/// must be posted before their stable digest keys are cleared at the next park.
pub(crate) fn note_workstation_setup_finished() {
    WORKSTATION_SETUP_FINISHED.store(true, Ordering::Release);
}

/// Take the completion before draining messages: the acquire also makes every
/// earlier message from the completing worker visible to that drain.
pub(crate) fn take_workstation_setup_finished() -> bool {
    // Setup workers belong to the production lane, which is lane 0 in tests.
    // An unrelated test's private inbox must not consume their completion.
    if current_lane() != 0 {
        return false;
    }
    WORKSTATION_SETUP_FINISHED.swap(false, Ordering::AcqRel)
}

/// Each message with its LANE: always `0` in a build that is not a unit test
/// ([`current_lane`]), where the inbox is one queue. A unit test that owns a
/// lane ([`own_lane_for_test`]) sees only its own, so the tests whose rows
/// come through here run side by side instead of in turn.
static INBOX: Mutex<Vec<(u64, InboxMessage)>> = Mutex::new(Vec::new());

/// The inbox lane of the calling thread: `0` in every build but a unit test,
/// and there `0` too unless the test took one ([`own_lane_for_test`]). A
/// producer on a thread of its own queues into the lane of whoever started it
/// ([`queue_message_in`]; the harness host keeps its creator's).
pub(crate) fn current_lane() -> u64 {
    #[cfg(test)]
    {
        LANE.with(std::cell::Cell::get)
    }
    #[cfg(not(test))]
    {
        0
    }
}

/// Queue one message for the next event-loop park. Safe from any thread and from before `App`
/// exists. Callers also log the words, so a console launch keeps its diagnostic.
pub(crate) fn queue_message(message: Message) {
    queue_message_in(current_lane(), message);
}

/// [`queue_message`] into `lane` ([`current_lane`] of the thread that owns the
/// producer): what a producer on a thread of its own queues through.
pub(crate) fn queue_message_in(lane: u64, message: Message) {
    let message = message.normalized();
    let Ok(mut q) = INBOX.lock() else {
        return; // a poisoned inbox must never take the app down
    };
    let mut held = 0;
    let mut duplicate = false;
    for (at, m) in q.iter() {
        if *at != lane {
            continue;
        }
        held += 1;
        duplicate |= m.message.tag == message.tag
            && m.message.title == message.title
            && m.message.detail.first() == message.detail.first();
    }
    if held < INBOX_CAP && !duplicate {
        q.push((
            lane,
            InboxMessage {
                message,
                stamp: crate::messages_host::wall_stamp_now(),
                restored: None,
            },
        ));
        INBOX_PENDING.store(true, Ordering::Release);
    }
}

/// Queue the restored agents' row the harness host said (`harness_host::say_restored`)
/// for the next park, in the host's lane ([`queue_message_in`]): its message in
/// the words it was said with, and the row itself, which the window words
/// again (`App::post_restored_row`). Never folded as a duplicate: a later row
/// can carry the same title and first line as an earlier one and name other
/// tabs, and the window keeps the later of the two.
pub(crate) fn queue_restored_row(lane: u64, row: crate::harness_host::RestoredRow) {
    let message = row.message().normalized();
    let Ok(mut q) = INBOX.lock() else {
        return; // a poisoned inbox must never take the app down
    };
    let held = q.iter().filter(|(at, _)| *at == lane).count();
    if held < INBOX_CAP {
        q.push((
            lane,
            InboxMessage {
                message,
                stamp: crate::messages_host::wall_stamp_now(),
                restored: Some(row),
            },
        ));
        INBOX_PENDING.store(true, Ordering::Release);
    }
}

/// Take everything queued in the calling thread's lane (empty when nothing
/// is; the whole inbox in every build but a unit test). The `Acquire` load
/// pairs with [`queue_message_in`]'s `Release` store, so a message queued on
/// the backend build thread is visible to the event loop that observes the
/// flag; the flag falls only once nothing is left in any lane.
pub(crate) fn take_queued() -> Vec<InboxMessage> {
    if !INBOX_PENDING.load(Ordering::Acquire) {
        return Vec::new();
    }
    let lane = current_lane();
    let Ok(mut q) = INBOX.lock() else {
        // A poisoned inbox drains empty with its flag down: what it holds
        // waits for the next queue to raise it.
        INBOX_PENDING.store(false, Ordering::Relaxed);
        return Vec::new();
    };
    let (mine, others): (Vec<_>, Vec<_>) = std::mem::take(&mut *q)
        .into_iter()
        .partition(|(at, _)| *at == lane);
    *q = others;
    if q.is_empty() {
        INBOX_PENDING.store(false, Ordering::Relaxed);
    }
    mine.into_iter().map(|(_, m)| m).collect()
}

/// The inbox itself, held (tests only): a thread that queues a message
/// meanwhile waits in [`queue_message`] until the guard drops — how a test
/// holds a row's sayer in the middle of its say. Taken after
/// [`lane_test_guard`] or [`own_lane_for_test`], and dropped before anything
/// that may panic.
#[cfg(test)]
pub(crate) fn hold_for_test() -> std::sync::MutexGuard<'static, Vec<(u64, InboxMessage)>> {
    INBOX
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
thread_local! {
    static LANE: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Every test that uses the inbox holds this shared ([`own_lane_for_test`],
/// [`lane_test_guard`]); the one that poisons the inbox holds it alone
/// ([`whole_inbox_for_test`]).
#[cfg(test)]
static INBOX_TESTS: std::sync::RwLock<()> = std::sync::RwLock::new(());

/// A lane of the test's own: the calling test thread queues and drains lane
/// `n`, a number no other test has, until the guard drops (what is left in it
/// is dropped, and the thread is back in lane `0`). For a test whose rows are
/// queued on its own thread or by a producer that keeps its creator's lane
/// (the harness host); a test whose rows come from a thread that does not (a
/// panic hook, a backend build thread) takes [`lane_test_guard`] instead and
/// works in lane `0`.
#[cfg(test)]
pub(crate) fn own_lane_for_test() -> OwnLane {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let shared = INBOX_TESTS
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let lane = NEXT.fetch_add(1, Ordering::Relaxed);
    LANE.with(|at| at.set(lane));
    OwnLane {
        lane,
        _shared: shared,
    }
}

/// [`own_lane_for_test`]'s guard.
#[cfg(test)]
pub(crate) struct OwnLane {
    lane: u64,
    _shared: std::sync::RwLockReadGuard<'static, ()>,
}

#[cfg(test)]
impl Drop for OwnLane {
    fn drop(&mut self) {
        if let Ok(mut q) = INBOX.lock() {
            q.retain(|(at, _)| *at != self.lane);
            if q.is_empty() {
                INBOX_PENDING.store(false, Ordering::Relaxed);
            }
        }
        if let Ok(mut q) = EXCEPTIONS.lock() {
            q.retain(|(at, _)| *at != self.lane);
            if q.is_empty() {
                EXCEPTIONS_PENDING.store(false, Ordering::Relaxed);
            }
        }
        LANE.with(|at| at.set(0));
    }
}

/// The whole inbox, every lane, for a test that breaks it for everyone (a
/// poisoned lock refuses every lane's queue): no other inbox test runs while
/// it is held, and it starts from an empty inbox.
#[cfg(test)]
pub(crate) fn whole_inbox_for_test() -> std::sync::RwLockWriteGuard<'static, ()> {
    let whole = INBOX_TESTS
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut q = INBOX
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    q.clear();
    INBOX_PENDING.store(false, Ordering::Relaxed);
    WORKSTATION_SETUP_FINISHED.store(false, Ordering::Relaxed);
    drop(q);
    EXCEPTIONS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clear();
    EXCEPTIONS_PENDING.store(false, Ordering::Relaxed);
    whole
}

/// SERIALIZE lane `0` across tests. The inbox is process-global by design (its
/// whole point is being reachable from threads that have no `App`), so two
/// tests exercising lane `0` in the same binary would steal each other's
/// messages. Every test that queues or drains in lane `0` takes this first; a
/// test whose producers keep its lane takes [`own_lane_for_test`] instead.
#[cfg(test)]
pub(crate) fn lane_test_guard() -> LaneZero {
    static LANE_TEST: Mutex<()> = Mutex::new(());
    let shared = INBOX_TESTS
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    LaneZero {
        _alone: LANE_TEST.lock().unwrap_or_else(|p| p.into_inner()),
        _shared: shared,
    }
}

/// [`lane_test_guard`]'s guard: lane `0` to this test alone.
#[cfg(test)]
pub(crate) struct LaneZero {
    _alone: std::sync::MutexGuard<'static, ()>,
    _shared: std::sync::RwLockReadGuard<'static, ()>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_messages::{Severity, tags};

    fn msg(title: &str) -> Message {
        Message::new(tags::CONFIG, Severity::Warn, title).line("a detail")
    }

    /// Bounded, deduplicated, drained in order, and poison-tolerant: once a producer panics
    /// holding the lock, a queue is refused and a drain answers empty, never a second panic.
    #[test]
    fn the_pre_app_inbox_is_poison_tolerant_and_bounded() {
        let _whole = whole_inbox_for_test();
        assert!(!INBOX_PENDING.load(Ordering::Acquire));
        for i in 0..(INBOX_CAP + 8) {
            queue_message(msg(&format!("warning {i}")));
        }
        queue_message(msg("warning 0"));
        assert!(INBOX_PENDING.load(Ordering::Acquire));
        let taken = take_queued();
        assert_eq!(taken.len(), INBOX_CAP, "the cap holds");
        assert_eq!(taken[0].message.title, "warning 0");
        assert_eq!(
            taken[INBOX_CAP - 1].message.title,
            format!("warning {}", INBOX_CAP - 1)
        );
        assert!(taken[0].stamp.unix_ms > 0, "stamped at queue time");
        assert!(take_queued().is_empty(), "drained");
        assert!(!INBOX_PENDING.load(Ordering::Acquire));
        // A duplicate of a queued message folds; a different detail does not.
        queue_message(msg("again"));
        queue_message(msg("again"));
        queue_message(Message::new(tags::CONFIG, Severity::Warn, "again").line("other"));
        let taken = take_queued();
        assert_eq!(taken.len(), 2);
        // Normalized at queue time, before it is compared or stored.
        queue_message(Message::new(tags::CONFIG, Severity::Warn, "bad\u{1b}[31m"));
        let taken = take_queued();
        assert_eq!(taken[0].message.title, "bad[31m");

        // A panic with the lock held poisons it: the next queue is refused and the next drain
        // answers empty, and the flag that drain cleared stays down, so what was queued before
        // the poison waits for the next queue to raise it. Cleared afterwards, or the lane stays
        // dead for every later test in this binary.
        queue_message(msg("before the poison"));
        let poisoned = std::panic::catch_unwind(|| {
            let _held = INBOX.lock().unwrap();
            panic!("a producer panicked with the inbox held");
        });
        assert!(poisoned.is_err());
        assert!(INBOX.is_poisoned());
        queue_message(msg("into the poison"));
        assert!(
            take_queued().is_empty(),
            "a poisoned inbox drains empty rather than panicking"
        );
        assert!(!INBOX_PENDING.load(Ordering::Acquire));
        INBOX.clear_poison();
        assert!(
            take_queued().is_empty(),
            "the flag is down: nothing is read until a queue raises it"
        );
        queue_message(msg("after the poison"));
        let taken = take_queued();
        assert_eq!(
            taken
                .iter()
                .map(|m| m.message.title.as_str())
                .collect::<Vec<_>>(),
            ["before the poison", "after the poison"],
            "what the poison stranded comes out ahead of what followed it"
        );
    }
}
