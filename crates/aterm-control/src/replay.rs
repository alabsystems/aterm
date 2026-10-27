// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Headless recording replay engine ([`ReplayEngine::fork`]).

use std::sync::Arc;

use aterm_core::config::TerminalConfig;
use aterm_core::grid::{ResizeLaw, ResizePolicy};
use aterm_core::terminal::{ClockReading, ReplayCarry, Terminal, TerminalCheckpoint};

/// A headless engine replaying one session's recorded output and resizes.
pub struct ReplayEngine {
    term: Terminal,
    policy: ResizePolicy,
    law: ResizeLaw,
    host_resyncs: bool,
    config: Option<Arc<TerminalConfig>>,
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

    /// Restores an engine from `cp`, a keyframe this process took of the engine it replays ([`Terminal::replay_from_checkpoint`]), that resizes under `policy` and verifies shell marks with the nonce `cp` carries.
    #[must_use]
    pub fn from_checkpoint(cp: &TerminalCheckpoint, policy: ResizePolicy) -> Self {
        Self::restored(Terminal::replay_from_checkpoint(cp), cp, policy)
    }

    /// [`Self::from_checkpoint`] with the history kept to `store_lines` lines past the ring ([`Terminal::replay_from_checkpoint_bounded`]), so [`Self::fork`] copies it in bounded time.
    #[must_use]
    pub fn from_checkpoint_bounded(
        cp: &TerminalCheckpoint,
        policy: ResizePolicy,
        store_lines: usize,
    ) -> Self {
        Self::restored(
            Terminal::replay_from_checkpoint_bounded(cp, store_lines),
            cp,
            policy,
        )
    }

    fn restored(mut term: Terminal, cp: &TerminalCheckpoint, policy: ResizePolicy) -> Self {
        if let Some(nonce) = cp.shell_integration_nonce {
            term.authorize_shell_integration(nonce.0);
        }
        Self::configured(term, policy, ResizeLaw::default(), false)
    }

    /// Sets `law` as the resize layout law for subsequent resizes.
    #[must_use]
    pub fn with_resize_law(mut self, law: ResizeLaw) -> Self {
        self.term.set_resize_law(law);
        self.law = law;
        self
    }

    /// Replays under the session's `config` (its colour baseline, default cursor, mode seeds, budgets and policies) rather than the defaults, keeping the modes the keyframe restored ([`Terminal::apply_config_keeping_modes`]), and so does every [`Self::fork`]; a report the rebase queued is dropped, as the live engine's was.
    #[must_use]
    pub fn with_config(mut self, config: Arc<TerminalConfig>) -> Self {
        self.term.apply_config_keeping_modes(&config);
        let _ = self.term.take_response();
        self.config = Some(config);
        self
    }

    /// Configures whether the host forwards `?1049l` to exit an orphaned alternate screen.
    #[must_use]
    pub fn with_host_resyncs_alt_screen(mut self, on: bool) -> Self {
        self.term.set_host_resyncs_alt_screen(on);
        self.host_resyncs = on;
        self
    }

    /// Installs the in-process transient state captured with the keyframe ([`Terminal::capture_replay_carry`]), so subsequent slices and [`Self::fork`] inherit the live engine's open mode-2026 frame start, rate-limit buckets, alternate-screen notes, Kitty ids and serials, shell-integration authorization and pending restore lineage.
    #[must_use]
    pub fn with_replay_carry(mut self, carry: &ReplayCarry) -> Self {
        self.term.apply_replay_carry(carry);
        self
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
            config: None,
        }
    }

    /// Feed recorded output through the parser.
    pub fn process(&mut self, bytes: &[u8]) {
        self.term.process(bytes);
    }

    /// Feed recorded output through the parser at the clock reading the live engine parsed it under.
    pub fn process_at(&mut self, bytes: &[u8], clock: ClockReading) {
        self.term.process_at(bytes, clock);
    }

    /// Apply a recorded resize under the session's seam policy.
    pub fn resize(&mut self, rows: u16, cols: u16) {
        self.term.resize_with_policy(rows, cols, self.policy);
    }

    /// Apply a recorded host cell-pixel update; an in-band size report (DEC 2048) it queues is dropped, as the live host drained it on the update.
    pub fn set_cell_pixel_size(&mut self, width: u16, height: u16) {
        self.term.set_cell_pixel_size(width, height);
        let _ = self.term.take_reports();
    }

    /// Apply a recorded host-side mutation ([`aterm_core::terminal::HostOp`]).
    pub fn apply_host_op(&mut self, op: aterm_core::terminal::HostOp, clock: Option<ClockReading>) {
        match clock {
            Some(c) => self.term.apply_host_op_at(op, c),
            None => self.term.apply_host_op(op),
        }
    }

    /// A copy of this engine at this point of the recording.
    #[must_use]
    pub fn fork(&self) -> Option<Self> {
        let mut fork = Self::configured(
            self.term.replay_fork()?,
            self.policy,
            self.law,
            self.host_resyncs,
        );
        fork.config.clone_from(&self.config);
        Some(fork)
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

    /// A replay under the session's config, and a fork of it, answers a colour query and resets its modes as the live engine does; one under the defaults does neither.
    #[test]
    fn a_replay_with_the_sessions_config_answers_as_the_live_engine() {
        let config = Arc::new(TerminalConfig {
            default_background: aterm_core::terminal::Rgb { r: 1, g: 2, b: 3 },
            cursor_style: aterm_core::terminal::CursorStyle::SteadyBar,
            ..TerminalConfig::default()
        });
        let live = || {
            let mut t = Terminal::new(4, 20);
            let _ = t.apply_config(&config);
            t.process(b"\x1b[2 qtext");
            t
        };
        let cp = live().checkpoint();
        for (configured, forked) in [(false, false), (true, false), (false, true), (true, true)] {
            let engine = ReplayEngine::from_checkpoint(&cp, ResizePolicy::Native);
            let engine = if configured {
                engine.with_config(config.clone())
            } else {
                engine
            };
            let mut replay = if forked {
                engine.fork().expect("a ground engine forks")
            } else {
                engine
            };
            let mut probe = live();
            let ask = b"\x1b]11;?\x07\x1b[!p";
            replay.process(ask);
            probe.process(ask);
            let mut term = replay.into_terminal();
            assert_eq!(
                (
                    term.take_response() == probe.take_response(),
                    term.cursor_style() == probe.cursor_style()
                ),
                (configured, configured),
                "configured: {configured}, forked: {forked}"
            );
        }
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
            let bounded = ReplayEngine::from_checkpoint_bounded(&foreign.checkpoint(), policy, 64);
            for (how, mut engine) in [
                ("new", fresh),
                ("from_checkpoint", restored),
                ("from_checkpoint_bounded", bounded),
            ] {
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
