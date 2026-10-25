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
    fn take_new_completion(&mut self, session: u64, cmd_done: Option<(u64, i32)>) -> Option<bool> {
        let key = (session, cmd_done.map_or(0, |(seq, _)| seq));
        if self.last_cmd == Some(key) {
            return None;
        }
        let same_session = self.last_cmd.is_some_and(|(sid, _)| sid == session);
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
mod tests {
    use super::RainLatches;

    #[test]
    fn the_execute_edge_is_a_same_session_rising_edge() {
        let mut l = RainLatches::default();
        assert!(!l.note_shell_executing(1, true), "first sight baselines");
        assert!(!l.note_shell_executing(1, true), "held is not an edge");
        assert!(!l.note_shell_executing(1, false));
        assert!(l.note_shell_executing(1, true), "the rising edge fires");
        assert!(
            !l.note_shell_executing(2, true),
            "a session switch baselines"
        );
    }

    #[test]
    fn a_completion_is_news_once_in_its_own_session() {
        let mut l = RainLatches::default();
        assert_eq!(l.take_new_completion(1, None), None, "watching, none seen");
        assert_eq!(l.take_new_completion(1, Some((1, 2))), Some(true));
        assert_eq!(l.take_new_completion(1, Some((1, 2))), None, "once");
        assert_eq!(
            l.take_new_completion(2, Some((9, 0))),
            None,
            "a switch baselines"
        );
        l.baseline_completion(2, Some(10));
        assert_eq!(
            l.take_new_completion(2, Some((10, 0))),
            None,
            "a suspension-era completion was absorbed"
        );
        assert_eq!(l.take_new_completion(2, Some((11, 0))), Some(false));
    }

    #[test]
    fn a_surface_without_an_engine_keeps_both_latches_observing() {
        let mut l = RainLatches::default();
        l.observe_without_engine(1, false, None);
        l.observe_without_engine(1, true, Some(4));
        assert!(
            !l.note_shell_executing(1, true),
            "the running command is not news"
        );
        assert_eq!(l.take_new_completion(1, Some((4, 1))), None, "nor its end");
        assert_eq!(l.take_new_completion(1, Some((5, 1))), Some(true));

        let mut execute_only = RainLatches::default();
        execute_only.observe_without_engine(1, false, None);
        let _ = execute_only.note_shell_executing(1, true);
        assert_eq!(
            execute_only.take_new_completion(1, Some((4, 1))),
            Some(true),
            "control: watching Execute alone replays the off-era completion"
        );
    }
}
