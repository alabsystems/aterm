// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

/// A serialized writer holding both the exact legacy reservation and complete
/// journal. It creates no live native authority and never restores a callback
/// from history. Dropping it releases complete custody before the legacy lock.
pub struct RecipientWriter {
    opened: Opened,
    reservation: reservation::HeldReservation,
    paths: Paths,
    prepare: Prepare,
    binding: Binding,
    usable: bool,
}

/// Open an already enrolled and bound operation, durably preparing its history
/// if necessary. This never enrolls custody, reserves an ID or creates a binding.
/// The caller must separately validate native authority, image compatibility and
/// live process identity before passing any delivery continuation to this writer.
/// # Errors
/// Refuses invalid recipes, missing or mismatched reservation/binding/custody,
/// competing writers, corrupt history, exhausted capacity or failed barriers.
pub fn open_recipient_writer(prepare: &Prepare, binding: &Binding) -> io::Result<RecipientWriter> {
    open_at(&Paths::locate().map_err(|_| invalid())?, prepare, binding)
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "RecipientDelivery",
        action = "Reopen",
        project = "aterm_agent::harness::native_receipts::recipient_custody::writer::tests::transition"
    )
)]
pub(super) fn open_at(
    paths: &Paths,
    prepare: &Prepare,
    binding: &Binding,
) -> io::Result<RecipientWriter> {
    let held = reservation::hold_at(paths, prepare, binding)?;
    let opened = super::open_at(paths, false)?;
    if prepare.custody.as_deref() != Some(opened.custody.stamp()) {
        return Err(invalid());
    }
    opened
        .view
        .require_binding(&opened.key, prepare, binding, &held)?;
    let mut writer = RecipientWriter {
        opened,
        reservation: held,
        paths: paths.clone(),
        prepare: prepare.clone(),
        binding: binding.clone(),
        usable: true,
    };
    writer.check()?;
    if writer.history()?.is_none() {
        writer.append(recipients::Append::Prepare)?;
    }
    Ok(writer)
}

impl RecipientWriter {
    fn custody_intact(&self) -> io::Result<()> {
        self.reservation.intact(&self.paths)?;
        self.opened.intact(&self.paths)
    }

    fn check(&mut self) -> io::Result<()> {
        if !self.usable {
            return Err(invalid());
        }
        if let Err(error) = self.custody_intact() {
            self.usable = false;
            return Err(error);
        }
        Ok(())
    }

    fn history(&self) -> io::Result<Option<RecipientHistory>> {
        self.opened.view.observe(&self.opened.key, &self.prepare)
    }

    fn append(&mut self, event: recipients::Append) -> io::Result<Observed> {
        self.check()?;
        let observed = Observed {
            sequence: self.opened.view.sequence,
            recorded_ms: aterm_time::unix_millis().ok_or_else(invalid)?,
        };
        let body = recipients::encode_event(
            &self.opened.key,
            observed.sequence,
            self.opened.view.head,
            &self.prepare,
            &self.binding,
            event,
            observed.recorded_ms,
        )?;
        self.opened
            .view
            .ingest_record(&self.opened.key, recipients::SUBJECT, &body)?;
        // Also poison on unwinding inside append. No later callback may depend
        // on an in-memory transition whose durable acknowledgement was lost.
        self.usable = false;
        self.opened.log.append(recipients::SUBJECT, body)?;
        self.custody_intact()?;
        self.usable = true;
        Ok(observed)
    }

    /// Invoke `after` once, only after a fresh recipient record and its checkpoint
    /// have been durably acknowledged. The locks remain held throughout `after`.
    /// An existing recipient phase refuses without calling it, including after
    /// reopening or losing an acknowledgement. A callback error or panic never
    /// removes the recorded possibility of execution.
    ///
    /// The callback is application code, not an execution grant: this writer
    /// cannot prove the caller's native authority or authenticate a live process.
    /// # Errors
    /// Refuses a poisoned writer, invalid recipient, repeated or out-of-order
    /// phase, terminal history, failed encoding or failed durable append.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "RecipientDelivery",
            action = "Send",
            project = "aterm_agent::harness::native_receipts::recipient_custody::writer::tests::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "RecipientDelivery",
            action = "Crash",
            reason = "The environment loses the writer and its in-memory continuation"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "RecipientDelivery",
            action = "LostAck",
            reason = "The environment loses the response after the real append completed but before the callback"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "RecipientDelivery",
            action = "Read",
            reason = "The independently checked complete-history reader returns observations without invoking continuations"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "RecipientDelivery",
            action = "LoseCustody",
            reason = "The environment removes enrollment outside the held journal locks"
        )
    )]
    pub fn record_then<R>(
        &mut self,
        phase: ImagePhase,
        recipient: Recipient,
        after: impl FnOnce(Observed) -> R,
    ) -> io::Result<R> {
        let event = match phase {
            ImagePhase::Forward => recipients::Append::Forward(recipient),
            ImagePhase::Rollback => recipients::Append::Rollback(recipient),
        };
        let observed = self.append(event)?;
        Ok(after(observed))
    }

    /// Close with positive no-recipient evidence, retaining the original result
    /// on an exact retry. This never runs a delivery callback.
    /// # Errors
    /// Refuses possible execution, a different terminal history, a poisoned
    /// writer or a failed append.
    pub fn noexec(&mut self) -> io::Result<Observed> {
        self.check()?;
        if let Some(history) = self.history()?
            && history.state == Recorded::NoExec
        {
            return Ok(history.observed);
        }
        self.append(recipients::Append::NoExec)
    }

    /// Seal the complete history without forgetting any possible recipient.
    /// An existing terminal observation is returned unchanged.
    /// # Errors
    /// Refuses a poisoned writer, invalid history or a failed append.
    pub fn seal(&mut self) -> io::Result<Observed> {
        self.check()?;
        if let Some(history) = self.history()?
            && matches!(history.state, Recorded::NoExec | Recorded::Sealed { .. })
        {
            return Ok(history.observed);
        }
        self.append(recipients::Append::Seal)
    }
}

#[cfg(test)]
#[path = "native_recipient_writer_tests.rs"]
mod tests;
