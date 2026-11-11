// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Per-frame rain driver sequencing around the engine.

use aterm_core::grid::LineSize;
use aterm_core::grid::extra::ImageRef;
use aterm_core::terminal::RenderCell;
use aterm_render::RainHalo;
use aterm_time::Instant;

use super::{EffectGeom, MatrixRain, RainCell, RainSignal, RainTickInput, RainVisibility};

/// Per-surface rain latches keyed by session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RainLatches {
    /// `(session, completed_command_seq)` of the last completion observed.
    pub last_cmd: Option<(u64, u64)>,
    /// `(session, executing)` for the OSC 133/633 C rising edge.
    pub shell_executing: Option<(u64, bool)>,
}

impl Default for RainLatches {
    #[inline]
    fn default() -> Self {
        Self::EMPTY
    }
}

impl RainLatches {
    /// Unbaselined rain latches before any session frame is observed.
    pub const EMPTY: Self = Self {
        last_cmd: None,
        shell_executing: None,
    };
    /// Record the shell's Execute state for `session` and return whether this is its rising edge.
    #[inline]
    pub const fn note_shell_executing(&mut self, session: u64, executing: bool) -> bool {
        let rising =
            matches!(self.shell_executing, Some((sid, false)) if sid == session) && executing;
        self.shell_executing = Some((session, executing));
        rising
    }

    /// Keep the completion latch baselined on a frame that draws no rain.
    #[inline]
    pub const fn baseline_completion(&mut self, session: u64, cmd_seq: Option<u64>) {
        self.last_cmd = Some((
            session,
            match cmd_seq {
                Some(seq) => seq,
                None => 0,
            },
        ));
    }

    /// Observe both latches on a frame where the surface runs no rain engine.
    #[inline]
    pub const fn observe_without_engine(
        &mut self,
        session: u64,
        executing: bool,
        cmd_seq: Option<u64>,
    ) {
        let _ = self.note_shell_executing(session, executing);
        self.baseline_completion(session, cmd_seq);
    }

    /// Exit status → weather (OSC 133/633): `Some(failed)` once per new
    /// completion in the same session; a tab switch re-baselines silently.
    #[inline]
    const fn take_new_completion(
        &mut self,
        session: u64,
        cmd_done: Option<(u64, i32)>,
    ) -> Option<bool> {
        let seq = match cmd_done {
            Some((s, _)) => s,
            None => 0,
        };
        let key = (session, seq);
        if matches!(self.last_cmd, Some(k) if k.0 == key.0 && k.1 == key.1) {
            return None;
        }
        let same_session = matches!(self.last_cmd, Some((sid, _)) if sid == session);
        self.last_cmd = Some(key);
        match cmd_done {
            Some((_, code)) if same_session => Some(code != 0),
            _ => None,
        }
    }
}

/// Coherent grid snapshot a frame may scan at `epoch`.
#[derive(Clone, Copy)]
pub struct RainScan<'a> {
    pub cells: &'a [Vec<RenderCell>],
    pub line_sizes: &'a [LineSize],
    pub images: &'a [Vec<(usize, ImageRef)>],
    pub rows: usize,
    pub cols: usize,
    pub default_bg: u32,
    pub epoch: u64,
}

/// Whose clock the frame runs on.
#[derive(Clone, Copy, Debug)]
pub enum RainClock {
    /// The native frame instant ([`MatrixRain::tick`]).
    At(Instant),
    /// Milliseconds already fed through [`MatrixRain::advance_ms`] and consumed by [`MatrixRain::emit`].
    Accumulated,
}

/// Everything one rain frame reads from its host.
#[derive(Clone, Copy)]
pub struct RainFrame<'a> {
    pub clock: RainClock,
    /// Draw nothing this frame while advancing suspended weather state.
    pub suspended: bool,
    /// Whether the motion policy allows rain to animate.
    pub animate: bool,
    pub visibility: RainVisibility,
    /// The session this surface shows (keys both latches).
    pub session: u64,
    /// The agent-output clock (`Terminal::content_seq`): only a change counts.
    pub content_seq: u64,
    /// This frame's Execute rising edge ([`RainLatches::note_shell_executing`]).
    pub execute_edge: bool,
    /// The most recent completed command: `(completed_command_seq, exit)`.
    pub cmd_done: Option<(u64, i32)>,
    pub scan: Option<RainScan<'a>>,
    /// Visible cursor cell excluded by the material sampler.
    pub cursor: Option<(u16, u16)>,
    pub geom: EffectGeom,
    pub tick: RainTickInput<'a>,
}

impl MatrixRain {
    /// Run one frame and return its fingerprint (`0` when nothing was drawn).
    #[inline]
    pub fn host_frame(
        &mut self,
        f: &RainFrame<'_>,
        latches: &mut RainLatches,
        quads: &mut Vec<RainCell>,
        add: &mut Vec<RainHalo>,
    ) -> u64 {
        if f.suspended {
            match f.clock {
                RainClock::At(now) => self.tick_suspended(now),
                RainClock::Accumulated => self.step_suspended(),
            }
            latches.baseline_completion(f.session, f.cmd_done.map(|(seq, _)| seq));
            quads.clear();
            add.clear();
            return 0;
        }
        self.set_reduced_motion(!f.animate);
        self.set_visibility(f.visibility);
        self.note_activity(f.content_seq);
        if f.execute_edge {
            self.note_signal(RainSignal::Execute as u32, 4);
        }
        if let Some(failed) = latches.take_new_completion(f.session, f.cmd_done) {
            self.note_exit_status(failed);
        }
        if let Some(scan) = f.scan
            && self.can_emit()
        {
            let needs_grid_rescan = self.needs_rescan(scan.epoch);
            let needs_material_sample =
                self.needs_material_sample() || (needs_grid_rescan && self.cfg.output_material);
            if needs_grid_rescan {
                self.rescan_from_cells(
                    scan.cells,
                    scan.line_sizes,
                    scan.images,
                    scan.rows,
                    scan.cols,
                    scan.default_bg,
                    scan.epoch,
                );
            }
            if needs_material_sample {
                self.sample_material(scan.cells, scan.rows, f.cursor, f.tick.hidden_band);
            }
        }
        match f.clock {
            RainClock::At(now) => self.tick(now, f.geom, &f.tick, quads, add),
            RainClock::Accumulated => self.emit(f.geom, &f.tick, quads, add),
        }
    }
}

#[cfg(test)]
#[path = "step_tests.rs"]
mod tests;
