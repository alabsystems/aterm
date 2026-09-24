// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE PRE-APP INBOX — the one lane a message may take from a thread that has
//! no `App`, or from before `App` exists (docs/DESIGN-unified-messages-2026-09-21.md
//! §3.3, D9). Each is queued here with the wall clock at queue time and
//! posted into the center at the next park (`App::drain_message_inbox`), so
//! a message minted before the center exists still carries the instant it
//! happened. The producers, every one of them (`message_reporters` builds
//! the words):
//!
//! * the startup config warnings, one message per family, and the crash
//!   message — `run()` collects them before `App` is constructed (lib.rs,
//!   the `cfg_warns` chain; R1, R2);
//! * a launch load failure — `app_config::load_config`, also before `App`
//!   (R3);
//! * the CPU-renderer `Once` warnings — `background_opacity` and
//!   `background_material` on the software path, decided wherever the
//!   renderer is first pinned (`app_config`; R5);
//! * the Windows backdrop declines — `run()`'s `hdr_glow` arm and the
//!   backend BUILD THREAD's no-GPU arm (lib.rs), the first-attach
//!   DirectComposition refusal (`app_window`), and the live-reload "not at
//!   launch" arm inside an `AppRt` chrome call handed only a `&Window`
//!   (`platform_win`; R6);
//! * the font family the backend build thread could not admit (lib.rs;
//!   the `config.font-family` row);
//! * the dead accessibility publisher — the PANIC HOOK, on the publisher's
//!   own thread (`a11y_backend::report_failure`; R8);
//! * the GPU-lost row — on the loop thread, but MID-RECOVERY, inside
//!   `App::recover_from_gpu_loss_with` before the CPU renderer is in the
//!   backend slot, so the row lands at the next park rather than re-gridding
//!   every window through a dead GPU (`app_render`; R7).
//!
//! A `Mutex<Vec<_>>` behind an `AtomicBool`, so the drain — which runs on
//! EVERY park — is one atomic load (`Acquire`, pairing with the producer's
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
}

static INBOX_PENDING: AtomicBool = AtomicBool::new(false);
static INBOX: Mutex<Vec<InboxMessage>> = Mutex::new(Vec::new());

/// Queue one message for the next event-loop park. Safe from ANY thread and
/// from before `App` exists. Callers still write the same words to stderr
/// where they always did — a console launch keeps its diagnostic, and this
/// adds the surface a windowed launch has.
pub(crate) fn queue_message(message: Message) {
    let message = message.normalized();
    let Ok(mut q) = INBOX.lock() else {
        return; // a poisoned inbox must never take the app down
    };
    let duplicate = q.iter().any(|m| {
        m.message.tag == message.tag
            && m.message.title == message.title
            && m.message.detail.first() == message.detail.first()
    });
    if q.len() < INBOX_CAP && !duplicate {
        q.push(InboxMessage {
            message,
            stamp: crate::messages_host::wall_stamp_now(),
        });
        INBOX_PENDING.store(true, Ordering::Release);
    }
}

/// Take everything queued (empty when nothing is). The `Acquire` load pairs
/// with [`queue_message`]'s `Release` store, so a message queued on the
/// backend build thread is visible to the event loop that observes the flag.
pub(crate) fn take_queued() -> Vec<InboxMessage> {
    if !INBOX_PENDING.load(Ordering::Acquire) {
        return Vec::new();
    }
    INBOX_PENDING.store(false, Ordering::Relaxed);
    INBOX
        .lock()
        .map(|mut q| std::mem::take(&mut *q))
        .unwrap_or_default()
}

/// SERIALIZE the lane across tests. The inbox is process-global by design (its
/// whole point is being reachable from threads that have no `App`), so two
/// tests exercising it in the same binary would steal each other's messages.
/// Every test that queues or drains takes this first.
#[cfg(test)]
pub(crate) fn lane_test_guard() -> std::sync::MutexGuard<'static, ()> {
    static LANE_TEST: Mutex<()> = Mutex::new(());
    LANE_TEST.lock().unwrap_or_else(|p| p.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_messages::{Severity, tags};

    fn msg(title: &str) -> Message {
        Message::new(tags::CONFIG, Severity::Warn, title).line("a detail")
    }

    /// Bounded, deduplicated, drained in order, the flag one atomic load
    /// once the queue is empty — and POISON-TOLERANT: a producer that panics
    /// while it holds the inbox leaves the lock poisoned, and from then on a
    /// queue is refused silently and a drain answers empty; neither ever
    /// takes the app down with a second panic.
    #[test]
    fn the_pre_app_inbox_is_poison_tolerant_and_bounded() {
        let _lane = lane_test_guard();
        let _ = take_queued();
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
        // Normalized at ingress: hostile text is clipped before it is compared
        // or stored.
        queue_message(Message::new(tags::CONFIG, Severity::Warn, "bad\u{1b}[31m"));
        let taken = take_queued();
        assert_eq!(taken[0].message.title, "bad[31m");

        // THE POISON. A panic while the lock is held (caught here, so the
        // test thread survives it) poisons the inbox for the rest of the
        // process: the next queue returns without pushing, the next drain
        // answers empty — and the flag it cleared stays down, so what was
        // queued before the poison waits for the next queue to raise it.
        // Cleared afterwards, or the lane is dead for every later test in
        // this binary.
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
