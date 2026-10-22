// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The host's watchers of a terminal's frame, attached to the terminal.
//!
//! A host whose readers are woken by the write that changes what they read has
//! a place for output: its reader wakes them once a batch is processed. A
//! write the host makes itself under the terminal's lock (a selection, a
//! palette, a resize) has no such place, except the end of the lock hold that
//! made it, and the host's lock wrapper reaches the terminal and nothing else.
//! So the watchers ride on the terminal, and the wrapper asks them. The engine
//! never calls them.

use std::sync::Arc;

/// Who watches a terminal's frame (see the module doc).
pub trait FrameWatchers: Send + Sync {
    /// Whether anyone watches the frame now. A hold is compared only while
    /// someone does.
    fn watched(&self) -> bool;

    /// A lock hold that just ended moved the frame. Never blocks.
    fn frame_moved(&self);
}

impl super::Terminal {
    /// Attach the host's watchers of this terminal's frame, replacing any.
    #[inline]
    pub fn set_frame_watchers(&mut self, watchers: Arc<dyn FrameWatchers>) {
        self.frame_watchers = Some(watchers);
    }

    /// The host's watchers of this terminal's frame, if any are attached.
    #[inline]
    #[must_use]
    pub fn frame_watchers(&self) -> Option<&Arc<dyn FrameWatchers>> {
        self.frame_watchers.as_ref()
    }
}
