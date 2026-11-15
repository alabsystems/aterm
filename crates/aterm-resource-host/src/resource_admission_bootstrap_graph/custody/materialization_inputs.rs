// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The same actual accepted inputs, never a funded or returned operation.
use super::*;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};

/// Only the existing State seals this actual bundle at the original output
/// attachment. State and a later native operation retain the SAME Arc. There
/// is no constructor/unwrap from DATA, paths, status or completion records.
pub(in crate::actor::owner) struct OriginalMaterializationInputs {
    key: wire::Key,
    active: Arc<AtomicBool>,
    source: Arc<OriginalSource>,
    configurations: Vec<Arc<OriginalConfiguration>>,
    workspace: atpkg::private_input::MaterializationWorkspace,
    deadline: Instant,
    revoked: AtomicBool,
}
impl OriginalMaterializationInputs {
    pub(in crate::actor::owner) fn original_deadline(&self) -> Instant {
        self.deadline
    }
    pub(in crate::actor::owner) fn quarantine(&self) {
        self.revoked.store(true, Ordering::Release);
        self.source.revoked.store(true, Ordering::Release);
        for configuration in &self.configurations {
            configuration.quarantine();
        }
        self.workspace.quarantine();
    }
    fn inputs_current(&self, deadline: Instant) -> io::Result<()> {
        let mut first = time(deadline.min(self.deadline)).err();
        if (self.revoked.load(Ordering::Acquire)
            || !self.active.load(Ordering::Acquire)
            || self.key != self.source.request.key
            || !Arc::ptr_eq(&self.active, &self.source.active)
            || self.configurations.is_empty())
            && first.is_none()
        {
            first = Some(refused(
                "original materialization inputs are revoked or foreign",
            ));
        }
        if let Err(error) = self.source.check_current(deadline.min(self.deadline))
            && first.is_none()
        {
            first = Some(error);
        }
        for configuration in &self.configurations {
            if let Err(error) = configuration.check_current(deadline.min(self.deadline))
                && first.is_none()
            {
                first = Some(error);
            }
        }
        if let Err(error) = time(deadline.min(self.deadline))
            && first.is_none()
        {
            first = Some(error);
        }
        first.map_or(Ok(()), Err)
    }
    pub(in crate::actor::owner) fn check_current(&self, deadline: Instant) -> io::Result<()> {
        self.with_original_directories(deadline, |_, _| Ok(()))
    }
    /// Borrow the SAME retained source and fresh contents descriptions. No
    /// pathname is reopened and neither descriptor creates its original owner.
    /// The native caller keeps this Arc and its independent resource owners
    /// until every actual child/descendant closes, including refusal/unwind.
    pub(in crate::actor::owner) fn with_original_directories<T>(
        &self,
        deadline: Instant,
        actual: impl FnOnce(std::os::fd::BorrowedFd<'_>, std::os::fd::BorrowedFd<'_>) -> io::Result<T>,
    ) -> io::Result<T> {
        self.with_contents_directory(deadline, |contents| {
            let source = self.source.directory(deadline.min(self.deadline))?;
            actual(source, contents)
        })
    }
    /// Borrow only the actual original contents FD. This supplies no grant,
    /// child, SDK, source-issuer, return or seed permission. The funded native
    /// operation must retain this Arc and its separate original resource owners
    /// through every actual child/descendant closure. Bookends cannot stop work.
    pub(in crate::actor::owner) fn with_contents_directory<T>(
        &self,
        deadline: Instant,
        actual: impl FnOnce(std::os::fd::BorrowedFd<'_>) -> io::Result<T>,
    ) -> io::Result<T> {
        if let Err(error) = self.inputs_current(deadline) {
            self.quarantine();
            return Err(error);
        }
        let result = catch_unwind(AssertUnwindSafe(|| {
            self.workspace.with_contents_directory(actual)
        }));
        // Run every actual input bookend even after callback error/unwind.
        let final_check = self.inputs_current(deadline);
        if final_check.is_err() {
            self.quarantine();
        }
        match result {
            Ok(Ok(value)) => final_check.map(|()| value),
            Ok(Err(error)) => {
                self.quarantine();
                Err(error)
            }
            Err(panic) => {
                self.quarantine();
                resume_unwind(panic)
            }
        }
    }
}

impl State {
    /// The protocol's accepted output closes its bounded configuration set.
    /// Only this existing State moves its actual workspace into one bundle.
    pub(super) fn seal_materialization_inputs(
        &mut self,
        key: &wire::Key,
        deadline: Instant,
    ) -> io::Result<()> {
        let attempt = self
            .attempt
            .as_mut()
            .ok_or_else(|| refused("materialization has no original attempt"))?;
        if &attempt.request.key != key {
            return Err(refused("materialization belongs to another original graph"));
        }
        if attempt.materialization_requested.is_none() && attempt.workspace.is_none() {
            return Ok(()); // existing unprepared query/object profile
        }
        let result = (|| {
            let count = attempt.configuration.len();
            if !attempt.active.load(Ordering::Acquire)
                || attempt.materialization_inputs.is_some()
                || attempt
                    .materialization_requested
                    .as_ref()
                    .is_none_or(|value| &value.key != key)
                || attempt.output.is_none()
                || count == 0
                || count > wire::configuration::MAX_FILES
                || attempt.configuration_budget.remaining_files()
                    != wire::configuration::MAX_FILES - count
                || attempt.configuration_requested.as_ref()
                    != attempt.configuration.last().map(|value| value.binding())
            {
                return Err(refused(
                    "materialization inputs are incomplete, pending or repeated",
                ));
            }
            let source = attempt
                .source
                .as_ref()
                .ok_or_else(|| refused("materialization lost its actual original source"))?;
            let workspace = attempt
                .workspace
                .as_ref()
                .ok_or_else(|| refused("materialization lost its actual fresh workspace"))?;
            let mut bytes = 0_u64;
            let mut bound = deadline.min(workspace.deadline()).min(source.deadline);
            for (index, configuration) in attempt.configuration.iter().enumerate() {
                let binding = configuration.binding();
                if &binding.key != key || binding.sequence != index as u64 + 1 {
                    return Err(refused(
                        "materialization configuration set is foreign or unordered",
                    ));
                }
                bytes = bytes
                    .checked_add(binding.bytes)
                    .ok_or_else(|| refused("configuration byte sum overflow"))?;
                bound = bound.min(configuration.deadline());
                configuration.check_current(bound)?;
            }
            if bytes
                != wire::configuration::MAX_BYTES - attempt.configuration_budget.remaining_bytes()
            {
                return Err(refused(
                    "materialization configuration reservation is incomplete",
                ));
            }
            time(bound)?;
            source.check_current(bound)?;
            workspace.check_current()?;
            let original = Arc::new(OriginalMaterializationInputs {
                key: key.clone(),
                active: Arc::clone(&attempt.active),
                source: Arc::clone(source),
                configurations: std::mem::take(&mut attempt.configuration),
                workspace: attempt.workspace.take().expect("same actual workspace"),
                deadline: bound,
                revoked: AtomicBool::new(false),
            });
            // Retain the SAME actual bundle before a postcheck can refuse.
            attempt.materialization_inputs = Some(Arc::clone(&original));
            original.check_current(bound)
        })();
        if result.is_err() {
            if let Some(inputs) = &attempt.materialization_inputs {
                inputs.quarantine();
            } else if let Some(workspace) = &attempt.workspace {
                workspace.quarantine();
            }
        }
        result
    }

    /// Borrow another Arc to the one original bundle; this is not issuance of
    /// an operation, loan, completed result, return or availability permission.
    pub(in crate::actor::owner) fn original_materialization_inputs(
        &self,
        key: &wire::Key,
        deadline: Instant,
    ) -> io::Result<Arc<OriginalMaterializationInputs>> {
        let attempt = self
            .attempt
            .as_ref()
            .ok_or_else(|| refused("original attempt absent"))?;
        if &attempt.request.key != key {
            return Err(refused("materialization belongs to another original graph"));
        }
        let inputs = attempt
            .materialization_inputs
            .as_ref()
            .ok_or_else(|| refused("actual original materialization inputs absent"))?;
        inputs.check_current(deadline)?;
        Ok(Arc::clone(inputs))
    }
}
