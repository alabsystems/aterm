// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::atomic::AtomicU32;
use std::sync::{Mutex, PoisonError};

static WATCHED: Mutex<Vec<(usize, usize)>> = Mutex::new(Vec::new());

fn address(ui_waiting: &AtomicU32) -> usize {
    std::ptr::from_ref(ui_waiting).addr()
}

/// Counts the pauses of the waiters on one `ui_waiting` while it lives.
#[must_use = "the pauses are counted only while the watch is held"]
pub(crate) struct Watch(usize);

impl Watch {
    /// How many pauses the waiters on the watched `ui_waiting` have taken.
    pub(crate) fn pauses(&self) -> usize {
        WATCHED
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .find(|(at, _)| *at == self.0)
            .map_or(0, |(_, pauses)| *pauses)
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        WATCHED
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|(at, _)| *at != self.0);
    }
}

/// Watch `ui_waiting` from now.
pub(crate) fn watch(ui_waiting: &AtomicU32) -> Watch {
    let at = address(ui_waiting);
    WATCHED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push((at, 0));
    Watch(at)
}

pub(super) fn paused(ui_waiting: &AtomicU32) {
    let at = address(ui_waiting);
    for (watched_at, pauses) in WATCHED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter_mut()
    {
        if *watched_at == at {
            *pauses += 1;
        }
    }
}
