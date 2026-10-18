// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A control input mark follows a nonempty, fully accepted seam receipt.
//! The factory captures admitted identity; a sole completion rides one event.
//! Dropping it (refusal, cancellation, unwind) is inert. Queue admission is not
//! completion, and neither a wire reply nor the current front session is evidence.

use crate::SessionCtx;
use crate::input::EgressReceipt;
use crate::session_log::InputVerb;
use crate::session_timeline::SessionTimeline;
use crate::subscribe::{SubscriberRegistry, Subscribers};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

#[derive(Clone)]
pub(crate) struct InputMark {
    timeline: Weak<Mutex<SessionTimeline>>,
    subscribers: Option<Weak<SubscriberRegistry>>,
    session: u64,
    epoch: Option<u32>,
    actor: aterm_slog::Actor,
    verb: InputVerb,
    cause: Option<aterm_slog::ShortId>,
    turn: bool,
    accepted: Arc<AtomicBool>,
}

impl std::fmt::Debug for InputMark {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InputMark")
            .field("session", &self.session)
            .field("epoch", &self.epoch)
            .field("turn", &self.turn)
            .finish_non_exhaustive()
    }
}

impl InputMark {
    /// A mark at the session timeline's current epoch. Not named `new`: the
    /// lock-order census resolves a call by its name, so a `fn new` that takes
    /// `timeline` would read every `X::new()` under a held lock as taking it.
    #[allow(
        clippy::too_many_arguments,
        reason = "the receipt follows the existing input capability handles"
    )]
    pub(crate) fn at_epoch(
        ctx: &SessionCtx,
        subscribers: Option<&Subscribers>,
        session: u64,
        actor: aterm_slog::Actor,
        verb: InputVerb,
        cause: Option<aterm_slog::ShortId>,
        turn: bool,
    ) -> Self {
        let epoch = ctx
            .timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .slog()
            .log()
            .ep();
        Self {
            timeline: Arc::downgrade(&ctx.timeline),
            subscribers: subscribers.map(Arc::downgrade),
            session,
            epoch: Some(epoch),
            actor,
            verb,
            cause,
            turn,
            accepted: Arc::new(AtomicBool::new(false)),
        }
    }

    /// The request read this before validating its cause. A reset at any later
    /// point makes its eventual diagnostic observation ineligible, not retargeted.
    pub(crate) fn within_epoch(mut self, epoch: Option<u32>) -> Self {
        self.epoch = epoch;
        self
    }

    pub(crate) fn targets(&self, session: u64) -> bool {
        self.session == session
    }

    pub(crate) fn completion(&self) -> InputCompletion {
        InputCompletion { mark: self.clone() }
    }
}

/// Sole custody of one event's observation. It contains no input payload and
/// confers no authority. Its fixed cost is part of the existing Job size budget.
pub(crate) struct InputCompletion {
    mark: InputMark,
}

impl InputCompletion {
    pub(crate) fn matches(&self, session: u64, ctx: &SessionCtx) -> bool {
        self.mark.targets(session)
            && Weak::ptr_eq(&self.mark.timeline, &Arc::downgrade(&ctx.timeline))
    }

    pub(crate) fn into_mark(self) -> InputMark {
        self.mark
    }

    /// Called only after the source-blind seam and echo publication have returned.
    /// Full spill acceptance counts; later program consumption is not asserted.
    pub(crate) fn finish(self, receipt: EgressReceipt) {
        if !receipt.fully_accepted_nonempty() {
            return;
        }
        let Some(timeline) = self.mark.timeline.upgrade() else {
            return;
        };
        let owed = {
            let mut timeline = timeline.lock().unwrap_or_else(|p| p.into_inner());
            if self.mark.epoch != Some(timeline.slog().log().ep()) {
                return;
            }
            if self.mark.accepted.swap(true, Ordering::AcqRel) {
                return;
            }
            let at = timeline.slog().now();
            if self.mark.turn {
                timeline.slog_mut().ctl_input_mark(
                    self.mark.actor,
                    self.mark.verb,
                    self.mark.cause,
                    at,
                );
            } else {
                timeline
                    .slog_mut()
                    .ctl_input(self.mark.actor, self.mark.verb, self.mark.cause, at);
            }
            timeline.slog_take_wake()
        };
        // The timeline is a leaf; subscribers are reached after releasing it.
        if owed
            && let Some(subscribers) = self.mark.subscribers.and_then(|s| s.upgrade())
            && subscribers.any()
        {
            subscribers
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .notify(self.mark.session);
        }
    }
}

#[cfg(all(test, unix))]
pub(crate) mod testing {
    use super::*;
    use std::io::Read;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

    pub(crate) fn owned_handle(local: u64) -> (crate::session_store::SessionHandle, std::fs::File) {
        let mut fds = [-1; 2];
        // SAFETY: pipe writes two fresh descriptors on success.
        let result = unsafe { libc::pipe(fds.as_mut_ptr()) };
        assert_eq!(result, 0, "private pipe");
        // SAFETY: both descriptors were just created, and each is adopted once.
        let reader = unsafe { std::fs::File::from_raw_fd(fds[0]) };
        let writer = unsafe { OwnedFd::from_raw_fd(fds[1]) };
        for fd in [reader.as_raw_fd(), writer.as_raw_fd()] {
            // SAFETY: an owned descriptor remains alive across this call.
            assert_eq!(
                unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) },
                0
            );
        }
        // Readback is fail-fast; no blocking read can outlive a failed write.
        let flags = unsafe { libc::fcntl(reader.as_raw_fd(), libc::F_GETFL) };
        assert!(flags >= 0);
        assert_eq!(
            unsafe { libc::fcntl(reader.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) },
            0
        );
        let mut handle = crate::session_store::test_handle_on(local, writer.as_raw_fd());
        Arc::get_mut(&mut handle.ctx)
            .expect("unpublished context")
            .sink = Arc::new(aterm_session::sink::SinkWriter::new_owned(writer));
        (handle, reader)
    }

    pub(crate) fn paste(
        handle: &crate::session_store::SessionHandle,
        mark: &InputMark,
        text: &str,
    ) -> bool {
        let event =
            crate::input::InputEvent::Paste(text.to_owned(), crate::input::PasteFraming::AtDrain);
        let receipt = crate::app_input::tracked_egress(
            &handle.term,
            &handle.ctx.modes,
            &handle.ctx.sink,
            &handle.ctx.output_echo,
            &event,
            crate::input::EgressMode::Backpressured,
        );
        let accepted = receipt.fully_accepted_nonempty();
        mark.completion().finish(receipt);
        accepted
    }

    pub(crate) fn input_count(handle: &crate::session_store::SessionHandle) -> usize {
        handle
            .ctx
            .timeline
            .lock()
            .unwrap()
            .slog()
            .log()
            .window()
            .filter(|r| r.k == aterm_slog::Kind::Input)
            .count()
    }

    pub(crate) fn byte(reader: &mut std::fs::File) -> u8 {
        let mut one = [0];
        assert_eq!(
            reader
                .read(&mut one)
                .expect("accepted byte remains readable"),
            1
        );
        one[0]
    }
}
