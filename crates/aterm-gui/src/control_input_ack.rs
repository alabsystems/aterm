// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Control input receipt marks.

use crate::SessionCtx;
use crate::input::EgressReceipt;
use crate::session_log::InputVerb;
use crate::session_timeline::SessionTimeline;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

#[derive(Clone)]
pub(crate) struct InputMark {
    timeline: Weak<Mutex<SessionTimeline>>,
    session: u64,
    epoch: Option<u32>,
    actor: aterm_slog::Actor,
    verb: InputVerb,
    cause: Option<aterm_slog::ShortId>,
    turn: bool,
    /// What the input says it means for the box it dismisses.
    act: Option<aterm_slog::ActWord>,
    admission: Option<aterm_slog::Admission>,
    accepted: Arc<AtomicBool>,
    /// Informational credit captured at admission.
    typist: Option<(
        crate::typed_by::TypedStamp,
        aterm_session::SessionId,
        Arc<crate::typed_by::Caller>,
    )>,
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
    /// A mark at the session timeline's current epoch.
    pub(crate) fn at_epoch(
        ctx: &SessionCtx,
        session: u64,
        actor: aterm_slog::Actor,
        verb: InputVerb,
        cause: Option<aterm_slog::ShortId>,
        turn: bool,
    ) -> Self {
        let (epoch, admission) = {
            let timeline = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            let slog = timeline.slog();
            (slog.log().ep(), slog.admit_input(slog.now()))
        };
        Self {
            timeline: Arc::downgrade(&ctx.timeline),
            session,
            epoch: Some(epoch),
            actor,
            verb,
            cause,
            turn,
            act: None,
            admission,
            accepted: Arc::new(AtomicBool::new(false)),
            typist: None,
        }
    }

    /// The input's `act=`.
    pub(crate) const fn acting(mut self, act: Option<aterm_slog::ActWord>) -> Self {
        self.act = act;
        self
    }

    /// The request read this before validating its cause.
    pub(crate) const fn within_epoch(mut self, epoch: Option<u32>) -> Self {
        self.epoch = epoch;
        self
    }

    /// The already admitted connection's caller, carried with this exact event.
    pub(crate) fn typed_by(
        mut self,
        ctx: &SessionCtx,
        caller: Option<Arc<crate::typed_by::Caller>>,
    ) -> Self {
        self.typist = caller.map(|by| (ctx.typed.clone(), ctx.self_id.clone(), by));
        self
    }

    pub(crate) const fn targets(&self, session: u64) -> bool {
        self.session == session
    }

    pub(crate) fn completion(&self) -> InputCompletion {
        InputCompletion { mark: self.clone() }
    }
}

/// Sole custody of one event's observation.
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
    pub(crate) fn finish(self, receipt: EgressReceipt) {
        if !receipt.fully_accepted_nonempty() {
            return;
        }
        let Some(timeline) = self.mark.timeline.upgrade() else {
            return;
        };
        {
            let mut timeline = timeline.lock().unwrap_or_else(|p| p.into_inner());
            if self.mark.epoch != Some(timeline.slog().log().ep()) {
                return;
            }
            if self.mark.accepted.swap(true, Ordering::AcqRel) {
                return;
            }
            let at = timeline.slog().now();
            let bound = timeline.slog_mut().ctl_accepted(
                self.mark.actor,
                self.mark.act,
                self.mark.admission,
                at,
            );
            let acted = crate::session_log::InputAct::of_acceptance(self.mark.act, bound);
            if self.mark.turn {
                timeline.slog_mut().ctl_input_mark(
                    self.mark.actor,
                    self.mark.verb,
                    self.mark.cause,
                    acted,
                    at,
                );
            } else {
                timeline.slog_mut().ctl_input(
                    self.mark.actor,
                    self.mark.verb,
                    self.mark.cause,
                    acted,
                    at,
                );
            }
            timeline.slog_take_wake();
        }
        if let Some((stamp, session, caller)) = self.mark.typist {
            stamp.record(&session, caller);
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
