// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE SESSION'S SUBMISSIONS (`text --json`'s `"submits"`): how many times an
//! Enter reached the session's program over something typed into it since the
//! Enter before, whoever typed it — a person at the window (their keys, a
//! paste, an IME commit, a development build's `as=person`) or a control
//! client (`send`, `key`, `paste`, `feed`, `turn`: the harness's own acts, a
//! lab's prompt, an orchestrator's).
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::input::InputEvent;

/// Session submission count ([`Self::submits`]) and composing state since the last Enter.
#[derive(Debug, Default)]
pub(crate) struct SubmitCount {
    /// A composing input was accepted since the last Enter.
    composed: AtomicBool,
    /// How many Enters submitted something composed.
    submits: AtomicU64,
}

/// What one input write means for the count ([`SubmitInput::of`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum SubmitInput {
    /// Composes nothing and submits nothing.
    #[default]
    Neither,
    /// Composes: a key press that is no plain Enter, text, a paste.
    Composes,
    /// A plain Enter press: submits what was composed since the last.
    Enter,
    /// Raw bytes holding `crs` carriage returns.
    Raw {
        lead: bool,
        crs: u32,
        inner: u32,
        tail: bool,
    },
}

impl SubmitInput {
    /// The class of `ev`.
    pub(crate) fn of(ev: &InputEvent) -> Self {
        use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};
        match ev {
            InputEvent::Key {
                event_type: KeyEventType::Release,
                ..
            } => Self::Neither,
            InputEvent::Key { key, mods, .. } => {
                let folded = ev.keypad_folded();
                let key = match folded.as_ref() {
                    Some(InputEvent::Key { key, .. }) => key,
                    _ => key,
                };
                match key {
                    Key::Named(NamedKey::Enter)
                        if !mods.intersects(
                            Modifiers::SHIFT | Modifiers::CTRL | Modifiers::ALT | Modifiers::SUPER,
                        ) =>
                    {
                        Self::Enter
                    }
                    Key::Named(NamedKey::CapsLock | NamedKey::NumLock | NamedKey::ScrollLock) => {
                        Self::Neither
                    }
                    Key::Named(named) if named.modifier_twin().is_some() => Self::Neither,
                    _ => Self::Composes,
                }
            }
            InputEvent::Text(text) => Self::raw(text.as_bytes()),
            InputEvent::KeySequence(bytes) => Self::raw(bytes),
            InputEvent::Paste(text, _) if !text.is_empty() => Self::Composes,
            _ => Self::Neither,
        }
    }

    /// Classify raw `bytes` written verbatim (`send`, `feed`, chord, or committed text).
    fn raw(bytes: &[u8]) -> Self {
        let mut lead = false;
        let mut crs = 0_u32;
        let mut inner = 0_u32;
        let mut run = false;
        for &b in bytes {
            if b == b'\r' {
                if crs == 0 {
                    lead = run;
                } else if run {
                    inner = inner.saturating_add(1);
                }
                crs = crs.saturating_add(1);
                run = false;
            } else {
                run = true;
            }
        }
        match (crs, run) {
            (0, false) => Self::Neither,
            (0, true) => Self::Composes,
            _ => Self::Raw {
                lead,
                crs,
                inner,
                tail: run,
            },
        }
    }
}

impl SubmitCount {
    /// An input write of class `input` was fully accepted by the session's sink.
    pub(crate) fn accepted(&self, input: SubmitInput) {
        match input {
            SubmitInput::Neither => {}
            SubmitInput::Composes => self.composed.store(true, Ordering::Release),
            SubmitInput::Enter => {
                if self.composed.swap(false, Ordering::AcqRel) {
                    self.submits.fetch_add(1, Ordering::AcqRel);
                }
            }
            SubmitInput::Raw {
                lead,
                crs,
                inner,
                tail,
            } => {
                if crs == 0 {
                    self.composed.fetch_or(tail, Ordering::AcqRel);
                    return;
                }
                let before = self.composed.swap(tail, Ordering::AcqRel);
                let n = u64::from(lead || before) + u64::from(inner);
                if n > 0 {
                    self.submits.fetch_add(n, Ordering::AcqRel);
                }
            }
        }
    }

    /// Submissions written to the session's program in this process (`text --json`'s `"submits"`).
    pub(crate) fn submits(&self) -> u64 {
        self.submits.load(Ordering::Acquire)
    }
}

#[cfg(test)]
#[path = "submit_count_tests.rs"]
mod tests;
