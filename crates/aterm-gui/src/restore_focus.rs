// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Focus chosen during one restore, independent of person/security attribution.

use std::sync::Arc;

use crate::WindowId;

#[derive(Clone, Debug)]
pub(crate) struct Revision {
    owner: Arc<()>,
    era: Arc<()>,
    sequence: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Choice {
    Saved,
    Window(WindowId),
    Stale,
}

#[derive(Debug)]
pub(crate) struct Intent {
    owner: Arc<()>,
    era: Arc<()>,
    sequence: u64,
    beginning: Option<Revision>,
    window: Option<WindowId>,
}

impl Default for Intent {
    fn default() -> Self {
        Self {
            owner: Arc::new(()),
            era: Arc::new(()),
            sequence: 0,
            beginning: None,
            window: None,
        }
    }
}

impl Intent {
    pub(crate) fn begin(&mut self) -> Revision {
        self.beginning
            .get_or_insert_with(|| Revision {
                owner: self.owner.clone(),
                era: self.era.clone(),
                sequence: self.sequence,
            })
            .clone()
    }

    pub(crate) fn note(&mut self, window: WindowId) {
        if self.beginning.is_none() {
            self.begin();
        }
        self.sequence = match self.sequence.checked_add(1) {
            Some(next) => next,
            None => {
                self.era = Arc::new(());
                0
            }
        };
        self.window = Some(window);
    }

    pub(crate) fn choice(&self, since: &Revision) -> Choice {
        if !Arc::ptr_eq(&self.owner, &since.owner) || self.beginning.is_none() {
            return Choice::Stale;
        }
        if Arc::ptr_eq(&self.era, &since.era) && self.sequence == since.sequence {
            Choice::Saved
        } else {
            self.window.map_or(Choice::Stale, Choice::Window)
        }
    }

    pub(crate) fn finish(&mut self) {
        if self.beginning.is_some() {
            *self = Self::default();
        }
    }

    pub(crate) fn current_choice(&self) -> Choice {
        self.beginning
            .as_ref()
            .map_or(Choice::Stale, |revision| self.choice(revision))
    }
}

impl crate::App {
    pub(crate) fn prepare_restore_focus(&mut self) {
        if self.pending_restore.is_some()
            && self.restore_drain.is_none()
            && self.carried_window_show.is_some()
        {
            self.carried_window_show = None;
            self.restore_focus.finish();
        }
        self.restore_focus.begin();
    }

    pub(crate) fn note_restore_focus_intent(&mut self, window: WindowId) {
        let pending = self.restore_pass_queued()
            || self.carried_window_show.as_ref().is_some_and(|show| {
                !show.stacked || !show.committed || !show.plan.fullscreen.is_empty()
            });
        if pending && self.windows.contains_key(&window) {
            self.prepare_restore_focus();
            self.restore_focus.note(window);
        }
    }

    pub(crate) fn note_restore_focus_event(
        &mut self,
        window: aterm_window::window::WindowId,
        event: &aterm_window::event::WindowEvent,
    ) {
        use aterm_window::event::{ElementState, Ime, WindowEvent};
        let chosen = match event {
            WindowEvent::KeyboardInput {
                event,
                is_synthetic: false,
                ..
            } => event.state == ElementState::Pressed,
            WindowEvent::Ime(Ime::Commit(text) | Ime::Preedit(text, _)) => !text.is_empty(),
            WindowEvent::MouseInput { state, .. } => *state == ElementState::Pressed,
            // Background scrolling does not choose the keyboard's window.
            _ => false,
        };
        if chosen && let Some(window) = self.winit_to_window.get(&window).copied() {
            self.note_restore_focus_intent(window);
        }
    }
}

#[cfg(test)]
#[path = "restore_focus_tests.rs"]
mod tests;
