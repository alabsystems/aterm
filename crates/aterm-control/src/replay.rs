// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Headless recording replay engine ([`ReplayEngine::fork`]).

use aterm_core::grid::{ResizeLaw, ResizePolicy};
use aterm_core::terminal::{Terminal, TerminalCheckpoint};

/// A headless engine replaying one session's recorded output and resizes.
pub struct ReplayEngine {
    term: Terminal,
    policy: ResizePolicy,
    law: ResizeLaw,
    host_resyncs: bool,
}

impl ReplayEngine {
    /// Creates a blank `rows` by `cols` engine that resizes under `policy`.
    #[must_use]
    pub fn new(rows: u16, cols: u16, policy: ResizePolicy) -> Self {
        Self::configured(
            Terminal::new(rows.max(1), cols.max(1)),
            policy,
            ResizeLaw::default(),
            false,
        )
    }

    /// Restores an engine from keyframe `cp` that resizes under `policy`.
    #[must_use]
    pub fn from_checkpoint(cp: &TerminalCheckpoint, policy: ResizePolicy) -> Self {
        Self::configured(
            Terminal::from_checkpoint(cp),
            policy,
            ResizeLaw::default(),
            false,
        )
    }

    /// Sets `law` as the resize layout law for subsequent resizes.
    #[must_use]
    pub fn with_resize_law(self, law: ResizeLaw) -> Self {
        Self::configured(self.term, self.policy, law, self.host_resyncs)
    }

    /// Configures whether the host forwards `?1049l` to exit an orphaned alternate screen.
    #[must_use]
    pub fn with_host_resyncs_alt_screen(self, on: bool) -> Self {
        Self::configured(self.term, self.policy, self.law, on)
    }

    fn configured(
        mut term: Terminal,
        policy: ResizePolicy,
        law: ResizeLaw,
        host_resyncs: bool,
    ) -> Self {
        term.set_resize_law(law);
        term.set_alt_archive_enabled(false);
        term.set_console_host_posture(policy);
        term.set_host_resyncs_alt_screen(host_resyncs);
        Self {
            term,
            policy,
            law,
            host_resyncs,
        }
    }

    /// Feed recorded output through the parser.
    pub fn process(&mut self, bytes: &[u8]) {
        self.term.process(bytes);
    }

    /// Apply a recorded resize under the session's seam policy.
    pub fn resize(&mut self, rows: u16, cols: u16) {
        self.term.resize_with_policy(rows, cols, self.policy);
    }

    /// A copy of this engine at this point of the recording.
    #[must_use]
    pub fn fork(&self) -> Option<Self> {
        self.term
            .replay_fork()
            .map(|term| Self::configured(term, self.policy, self.law, self.host_resyncs))
    }

    /// Drop the active grid's resize undo.
    #[inline]
    pub fn drop_resize_undo(&mut self) {
        self.term.grid_mut().drop_resize_undo();
    }

    /// The replayed engine, for reading.
    #[must_use]
    #[inline]
    pub const fn terminal(&self) -> &Terminal {
        &self.term
    }

    /// The replayed engine, handed over.
    #[must_use]
    #[inline]
    pub fn into_terminal(self) -> Terminal {
        self.term
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::visible_row;
    use aterm_core::terminal::AltArchiveQuery;

    fn archiving(term: &Terminal) -> bool {
        term.alt_archive_read(AltArchiveQuery::oldest(0, 0)).enabled
    }

    fn rows(term: &Terminal) -> Vec<String> {
        (0..term.rows() as usize)
            .map(|r| visible_row(term, r))
            .collect()
    }

    fn replays(term: &Terminal, policy: ResizePolicy) -> bool {
        let console_host = policy == ResizePolicy::ConPty;
        let modes = term.modes();
        !archiving(term)
            && modes.win32_input_enabled == console_host
            && modes.kitty_keyboard_enabled != console_host
    }

    #[test]
    fn every_replay_sets_archive_posture_and_policy() {
        let lines: Vec<u8> = (0..12)
            .flat_map(|i| format!("L{i:02}\r\n").into_bytes())
            .collect();
        let live_after_grow = |policy: ResizePolicy| {
            let mut live = Terminal::new(6, 20);
            live.set_console_host_posture(policy);
            live.process(&lines);
            live.resize_with_policy(10, 20, policy);
            rows(&live)
        };
        for (policy, other) in [
            (ResizePolicy::ConPty, ResizePolicy::Native),
            (ResizePolicy::Native, ResizePolicy::ConPty),
        ] {
            let mut foreign = Terminal::new(6, 20);
            foreign.set_console_host_posture(other);
            let fresh = ReplayEngine::new(6, 20, policy);
            let restored = ReplayEngine::from_checkpoint(&foreign.checkpoint(), policy);
            for (how, mut engine) in [("new", fresh), ("from_checkpoint", restored)] {
                assert!(replays(engine.terminal(), policy), "{how} {policy:?}");
                engine.process(&lines);
                engine.resize(10, 20);
                assert_eq!(
                    rows(engine.terminal()),
                    live_after_grow(policy),
                    "{how} {policy:?}: the grow lands as the live engine's"
                );
            }
        }
        assert_ne!(
            live_after_grow(ResizePolicy::ConPty),
            live_after_grow(ResizePolicy::Native),
            "the grow must tell the policies apart"
        );

        let mut bare = Terminal::new(6, 20);
        assert!(!replays(&bare, ResizePolicy::ConPty));
        assert!(!replays(&bare, ResizePolicy::Native), "the archive is on");
        bare.process(&lines);
        bare.resize(10, 20);
        assert_ne!(rows(&bare), live_after_grow(ResizePolicy::ConPty));
    }
}
