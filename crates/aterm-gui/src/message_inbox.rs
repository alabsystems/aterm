// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded, poison-tolerant pre-`App` and background-thread inbox for messages and exceptions.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use aterm_messages::{Message, WallStamp};

/// Maximum messages buffered per lane between event-loop parks. A lane's latest restored-agents
/// row is never dropped for it ([`queue_restored_row`]), so that row can sit one above it.
pub(crate) const INBOX_CAP: usize = 32;

/// Queued message paired with its ingress wall-clock timestamp.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InboxMessage {
    /// The message, normalized at queue time.
    pub(crate) message: Message,
    /// The wall clock at queue time — the message's ingress stamp (D4).
    pub(crate) stamp: WallStamp,
    /// The restored agents' row `message` words as the host said it
    /// ([`queue_restored_row`]): the window words it again from this
    /// (`App::post_restored_row`).
    pub(crate) restored: Option<crate::harness_host::RestoredRow>,
}

static INBOX_PENDING: AtomicBool = AtomicBool::new(false);

/// THE EXCEPTIONS LANE: a fact for the owner's later look
/// ([`aterm_messages::exceptions::Entry`], an event family's) raised, or
/// ended, on a thread with no `App` — the fabric endpoint's, when the bridge
/// says it set torn state files aside and when it says its quarantine holds
/// none of them any more — and handed to `App::except` or `App::except_clear`
/// at the next park, with the lane it was queued in.
static EXCEPTIONS: Mutex<Vec<(u64, ExceptionWord)>> = Mutex::new(Vec::new());
static EXCEPTIONS_PENDING: AtomicBool = AtomicBool::new(false);

/// Queued exception raise or clear operation.
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
/// replaces the earlier one in its place.
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

/// Queue an exception entry to be posted at the next event-loop park.
#[cfg(any(unix, test))]
pub(crate) fn queue_exception(entry: aterm_messages::exceptions::Entry) {
    queue_exception_word(ExceptionWord::Raise(Box::new(entry)));
}

/// Queue an exception clear by `key` for the next event-loop park.
#[cfg(any(unix, test))]
pub(crate) fn queue_exception_clear(key: String) {
    queue_exception_word(ExceptionWord::Clear(key));
}

/// Drain all queued exception operations for the current thread's lane.
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

/// Each message with its LANE: always `0` in a build that is not a unit test
/// ([`current_lane`]), where the inbox is one queue.
static INBOX: Mutex<Vec<(u64, InboxMessage)>> = Mutex::new(Vec::new());

/// Return the inbox lane ID for the calling thread (`0` in production).
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

/// Queue `message` in the calling thread's lane for the next event-loop park.
pub(crate) fn queue_message(message: Message) {
    queue_message_in(current_lane(), message);
}

/// Queue `message` in `lane` for the next event-loop park.
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

/// Queue a restored-agent summary row in `lane` for the next event-loop park. At [`INBOX_CAP`] the
/// lane's latest row is still kept: a row with a later `seq` names every tab still missed
/// ([`crate::harness_host::RestoredRow`]), so the lane's earlier rows give way to it, and only a
/// row older than one already queued is dropped.
pub(crate) fn queue_restored_row(lane: u64, row: crate::harness_host::RestoredRow) {
    let message = row.message().normalized();
    let Ok(mut q) = INBOX.lock() else {
        return; // a poisoned inbox must never take the app down
    };
    if q.iter().filter(|(at, _)| *at == lane).count() >= INBOX_CAP {
        let later = q.iter().any(|(at, m)| {
            *at == lane && m.restored.as_ref().is_some_and(|held| held.seq > row.seq)
        });
        if later {
            return;
        }
        q.retain(|(at, m)| *at != lane || m.restored.is_none());
    }
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

/// Drain all queued messages for the calling thread's lane.
pub(crate) fn take_queued() -> Vec<InboxMessage> {
    if !INBOX_PENDING.load(Ordering::Acquire) {
        return Vec::new();
    }
    let lane = current_lane();
    let Ok(mut q) = INBOX.lock() else {
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

/// Hold the raw inbox mutex for concurrency tests.
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

/// Assign the calling test thread a dedicated inbox lane for isolated testing.
#[cfg(test)]
pub(crate) fn own_lane_for_test() -> OwnLane {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let lane = NEXT.fetch_add(1, Ordering::Relaxed);
    LANE.with(|at| at.set(lane));
    OwnLane { lane }
}

/// RAII guard resetting the thread's test inbox lane on drop.
#[cfg(test)]
pub(crate) struct OwnLane {
    lane: u64,
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

/// Empty every lane, for the test that poisons the inbox, which runs alone in a child of the
/// test binary ([`crate::test_alone::alone`]): the inbox and its poison are the whole process's.
#[cfg(test)]
pub(crate) fn whole_inbox_for_test() {
    assert!(
        crate::test_alone::in_child(),
        "the whole inbox is every test's: a test that takes it runs alone"
    );
    let mut q = INBOX
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    q.clear();
    INBOX_PENDING.store(false, Ordering::Relaxed);
    drop(q);
    EXCEPTIONS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clear();
    EXCEPTIONS_PENDING.store(false, Ordering::Relaxed);
}

/// Run the calling test alone in a child of the test binary ([`crate::test_alone::alone`]):
/// `true` in the child, which runs the body on lane `0` with no other test's message in it.
/// Lane `0` is the lane of every thread no test gave one of its own, a test's workers included,
/// so in the lib-test process any test's message can land in it; a test that reads lane `0`
/// starts with `if !lane_zero_alone() { return; }`.
#[cfg(test)]
#[track_caller]
pub(crate) fn lane_zero_alone() -> bool {
    crate::test_alone::alone()
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
        if !crate::test_alone::alone() {
            return;
        }
        whole_inbox_for_test();
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
        queue_message(msg("again"));
        queue_message(msg("again"));
        queue_message(Message::new(tags::CONFIG, Severity::Warn, "again").line("other"));
        let taken = take_queued();
        assert_eq!(taken.len(), 2);
        queue_message(Message::new(tags::CONFIG, Severity::Warn, "bad\u{1b}[31m"));
        let taken = take_queued();
        assert_eq!(taken[0].message.title, "bad[31m");

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

    /// A test that reads lane `0` runs where no other test queued into it: a message another
    /// test of the lib-test process queued on lane `0` meanwhile is not in its drain.
    #[test]
    fn a_lane_zero_test_drains_no_message_another_test_queued() {
        const STRAY: &str = "another test's message on lane 0";
        if !crate::test_alone::in_child() {
            queue_message(msg(STRAY));
        }
        if !lane_zero_alone() {
            return;
        }
        assert!(
            take_queued().iter().all(|m| m.message.title != STRAY),
            "a lane 0 drain took another test's message"
        );
    }

    fn row(seq: u64) -> crate::harness_host::RestoredRow {
        crate::harness_host::RestoredRow {
            seq,
            lines: Vec::new(),
            log: None,
        }
    }

    fn rows_in(taken: &[InboxMessage]) -> Vec<u64> {
        taken
            .iter()
            .filter_map(|m| m.restored.as_ref().map(|r| r.seq))
            .collect()
    }

    /// However many rows are said between two parks, and however full the lane is, the window
    /// gets the latest, the one that names every tab, and no other message gives way to it.
    #[test]
    fn the_latest_restored_row_always_reaches_the_window() {
        let _lane = own_lane_for_test();
        let lane = current_lane();
        let many = u64::try_from(INBOX_CAP * 2).expect("small");
        for seq in 1..=many {
            queue_restored_row(lane, row(seq));
        }
        let taken = take_queued();
        assert_eq!(
            rows_in(&taken).last(),
            Some(&many),
            "more rows than the cap"
        );
        assert!(taken.len() <= INBOX_CAP, "the cap holds: {}", taken.len());

        for i in 0..INBOX_CAP {
            queue_message(msg(&format!("warning {i}")));
        }
        queue_restored_row(lane, row(20));
        queue_restored_row(lane, row(22));
        queue_restored_row(lane, row(21));
        let taken = take_queued();
        assert_eq!(
            rows_in(&taken),
            [22],
            "a lane full of other messages keeps only the latest, whichever was said last"
        );
        assert_eq!(taken.len(), INBOX_CAP + 1, "no message gave way to the row");
    }
}
