// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The one engine a recording is replayed through.
//!
//! Every replay of a session (`cast drift`'s folds, the `cast frames`
//! flipbook, `temporal`'s reconstruction) builds its headless engine here, a
//! copy of one mid-recording ([`ReplayEngine::fork`]) included, so each is set
//! up as the live engine was in every way a replay can see:
//!
//! - the console-host posture of the session's resize seam policy
//!   (`Terminal::set_console_host_posture`): a ConPTY session's engine honors
//!   conhost's `?9001h`, which decides how an orphaned alternate screen is
//!   left. It is set from the policy whatever a restored checkpoint carried;
//! - that policy on every resize (`Terminal::resize_with_policy`): a Native
//!   grow reveals history at the top where a ConPTY grow appends blank rows;
//! - the resize law its rows were laid out under (`Terminal::set_resize_law`,
//!   [`ReplayEngine::with_resize_law`]): the current one unless the recording
//!   names an older one;
//! - whether its host leaves a dead app's alternate screen
//!   (`Terminal::set_host_resyncs_alt_screen`,
//!   [`ReplayEngine::with_host_resyncs_alt_screen`]): OpenConsole's sessions
//!   leave it on the `?1049l` the host forwarded, not at the shell's prompt;
//! - the alternate-screen archive off. It keeps only the scrollback text of
//!   committed alt frames, is never part of a checkpoint, never touches the
//!   grid a replay reads, and is most of a fold's time on a recording of a
//!   fullscreen app.

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
    /// A blank engine of `rows` x `cols` (each at least 1), the geometry a
    /// recording starts at, for a session that resizes under `policy`.
    #[must_use]
    pub fn new(rows: u16, cols: u16, policy: ResizePolicy) -> Self {
        Self::configured(
            Terminal::new(rows.max(1), cols.max(1)),
            policy,
            ResizeLaw::default(),
            false,
        )
    }

    /// An engine restored from `cp` (a recorded keyframe) for a session that
    /// resizes under `policy`.
    #[must_use]
    pub fn from_checkpoint(cp: &TerminalCheckpoint, policy: ResizePolicy) -> Self {
        Self::configured(
            Terminal::from_checkpoint(cp),
            policy,
            ResizeLaw::default(),
            false,
        )
    }

    /// This engine laying its rows out under `law` from the next resize on: an
    /// older law for a recording made under it (`cast drift`, from the
    /// recording's `aterm_resize_undo`).
    #[must_use]
    pub fn with_resize_law(self, law: ResizeLaw) -> Self {
        Self::configured(self.term, self.policy, law, self.host_resyncs)
    }

    /// This engine leaving a dead app's alternate screen on the `?1049l` its
    /// host forwards (`on`, an OpenConsole session's posture) rather than at
    /// the shell's next prompt.
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

    /// A copy of this engine at this point of the recording, which folds the
    /// rest of it as this one would (`Terminal::replay_fork`), so replays
    /// sharing a prefix fold it once. `None` when the engine holds state a
    /// fork cannot carry (a parser inside a sequence, Kitty images, an open
    /// resize undo); the caller folds from the start instead.
    #[must_use]
    pub fn fork(&self) -> Option<Self> {
        self.term
            .replay_fork()
            .map(|term| Self::configured(term, self.policy, self.law, self.host_resyncs))
    }

    /// Drop the active grid's resize undo, so the next resize lands as it would
    /// on an engine without the undo (`cast drift`'s undo-less replays).
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

    /// Whether `term`'s alt-screen archive records, as the `archive` verb reads it.
    fn archiving(term: &Terminal) -> bool {
        term.alt_archive_read(AltArchiveQuery::oldest(0, 0)).enabled
    }

    fn rows(term: &Terminal) -> Vec<String> {
        (0..term.rows() as usize)
            .map(|r| visible_row(term, r))
            .collect()
    }

    /// Whether `term` is set up as a replay of a session resizing under
    /// `policy`: the archive off and that policy's console-host posture.
    fn replays(term: &Terminal, policy: ResizePolicy) -> bool {
        let console_host = policy == ResizePolicy::ConPty;
        let modes = term.modes();
        !archiving(term)
            && modes.win32_input_enabled == console_host
            && modes.kitty_keyboard_enabled != console_host
    }

    /// Both constructors, under both policies, give the engine of the
    /// session: archive off, the policy's posture even when the checkpoint
    /// carried the other host's, and a 6 -> 10 grow over seven history lines
    /// laid out as the live engine laid it (Native reveals four of them,
    /// ConPTY appends four blank rows). Negative control: a bare engine has
    /// the archive on and no ConPTY posture, and its resize is Native's.
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
