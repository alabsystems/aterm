// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Ordering only. Each transition follows its real owner/physical check; these
//! bits cannot admit a process, image, callback, Source resource or App scene.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Step {
    Held,
    Recorded,
    Granted,
    Command,
    Entered,
    Attached,
    Runtime,
    Ui,
    Observed,
    Promoted,
    Committed,
}
impl Step {
    #[cfg(test)]
    pub(super) const ALL: [Self; 11] = [
        Self::Held,
        Self::Recorded,
        Self::Granted,
        Self::Command,
        Self::Entered,
        Self::Attached,
        Self::Runtime,
        Self::Ui,
        Self::Observed,
        Self::Promoted,
        Self::Committed,
    ];
    pub(super) fn bit(self) -> u16 {
        1 << self as u16
    }
}
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub(super) struct Progress(u16);
impl Progress {
    pub(super) fn advance(
        &mut self,
        step: Step,
    ) -> Result<(), aterm_agent::harness::codex_source_registry::Error> {
        let before = step.bit() - 1;
        if self.0 & before != before {
            return Err(aterm_agent::harness::codex_source_registry::Error::Denied);
        }
        self.0 |= step.bit();
        Ok(())
    }
    pub(super) fn contains(self, step: Step) -> bool {
        self.0 & step.bit() != 0
    }
}

#[cfg(test)]
#[path = "codex_retained_phase_tests.rs"]
mod tests;
