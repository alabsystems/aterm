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

    /// Apply a recorded host-side mutation ([`aterm_core::terminal::HostOp`]); a config the host installed is the one this engine and every [`Self::fork`] replay under from then on ([`Self::config`]).
    pub fn apply_host_op(&mut self, op: aterm_core::terminal::HostOp, clock: Option<ClockReading>) {
        if let aterm_core::terminal::HostOp::Config { config, .. } = &op {
            self.config = Some(Arc::clone(&config.0));
        }
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

    /// The config this engine replays under now: the one it was configured with ([`Self::with_config`]), or the last a replayed host install put in force.
    #[must_use]
    #[inline]
    pub const fn config(&self) -> Option<&Arc<TerminalConfig>> {
        self.config.as_ref()
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
#[path = "replay_tests.rs"]
mod tests;
