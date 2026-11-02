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
    /// Same-request custody through the actual ordered writer, not merely GUI queue admission.
    supervisor: Option<Arc<crate::session_timeline::SupervisorInputBatch>>,
    supervisor_work: Option<Arc<crate::session_timeline::SupervisorInputWork>>,
    /// A fixed native continuation, admitted by its constructor rather than a typing scope.
    /// Clones retain the same custody through Wake, actual writes and exact kernel drain.
    native: Option<Arc<crate::native_act::NativeInput>>,
    native_work: Option<Arc<crate::native_act::NativeInputWork>>,
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
            supervisor: None,
            supervisor_work: None,
            native: crate::native_act::carried_custody(ctx),
            native_work: None,
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

    pub(crate) fn supervised(
        mut self,
        batch: Option<Arc<crate::session_timeline::SupervisorInputBatch>>,
    ) -> Self {
        if !self
            .supervisor
            .as_ref()
            .zip(batch.as_ref())
            .is_some_and(|(old, new)| Arc::ptr_eq(old, new))
        {
            self.supervisor_work = None;
        }
        self.supervisor = batch;
        self
    }

    pub(crate) fn pending_supervisor(mut self) -> Self {
        if self.supervisor_work.is_none() {
            self.supervisor_work = self.supervisor.as_ref().and_then(|batch| batch.reserve());
        }
        if self.native_work.is_none() {
            self.native_work = self.native.as_ref().and_then(|native| native.reserve());
        }
        self
    }

    pub(crate) fn supervisor_queue_ready(&self) -> bool {
        (self.supervisor.is_none() || self.supervisor_work.is_some())
            && (self.native.is_none() || self.native_work.is_some())
    }

    pub(crate) fn supervisor_producer(
        &self,
    ) -> Option<crate::session_timeline::SupervisorInputProducer> {
        self.supervisor.as_ref().map(|batch| batch.producer())
    }

    pub(crate) fn seal_supervisor_input(&self) {
        if let Some(batch) = &self.supervisor {
            batch.seal();
        }
        if let Some(native) = &self.native {
            native.seal();
        }
    }

    pub(crate) fn native_producer(&self) -> Option<crate::native_act::NativeInputProducer> {
        self.native.as_ref().map(|native| native.producer())
    }

    pub(crate) const fn targets(&self, session: u64) -> bool {
        self.session == session
    }

    pub(crate) fn completion(&self) -> InputCompletion {
        InputCompletion {
            mark: self.clone().pending_supervisor(),
        }
    }
}

/// Sole custody of one event's observation.
pub(crate) struct InputCompletion {
    mark: InputMark,
}

impl InputCompletion {
    fn start_native(&self, nonparking: bool) -> bool {
        if !self
            .mark
            .native
            .as_ref()
            .is_none_or(|native| native.start(nonparking))
        {
            return false;
        }
        if let Some(work) = &self.mark.native_work {
            work.attempted();
        }
        true
    }
    pub(crate) fn is_supervised(&self) -> bool {
        self.mark.supervisor.is_some() || self.mark.native.is_some()
    }

    pub(crate) fn start_supervised(&self) -> bool {
        self.mark.supervisor_queue_ready()
            && self.start_native(false)
            && self
                .mark
                .supervisor
                .as_ref()
                .is_none_or(|batch| batch.start())
    }

    pub(crate) fn start_supervised_inline(&self) -> bool {
        self.mark.supervisor_queue_ready()
            && self.start_native(true)
            && self
                .mark
                .supervisor
                .as_ref()
                .is_none_or(|batch| batch.start_inline())
    }

    pub(crate) fn matches(&self, session: u64, ctx: &SessionCtx) -> bool {
        self.mark.targets(session)
            && Weak::ptr_eq(&self.mark.timeline, &Arc::downgrade(&ctx.timeline))
    }

    pub(crate) fn into_mark(self) -> InputMark {
        self.mark
    }

    /// Called only after the source-blind seam and echo publication have returned.
    pub(crate) fn finish(self, receipt: EgressReceipt) {
        if let Some(work) = &self.mark.native_work {
            work.finish(receipt.accepted_order().is_some());
        }
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
