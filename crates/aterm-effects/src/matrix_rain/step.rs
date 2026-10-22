// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The rain driver's one frame, the sequencing every host runs around the
//! engine: the suspended arm (the weather advances, nothing is drawn, the
//! completion latch stays baselined), the per-frame policy (reduced motion,
//! visibility), the weather's notes (agent output, the shell's Execute edge, a
//! new command completion), the grid scan gated by the host's coherent
//! snapshot, and the emission on the host's clock.
//!
//! The host keeps what is its own: whether a frame may scan at all (the native
//! frame hold, the web's torn-snapshot check), the hidden-cursor damage band it
//! maintains under its own lock, and where the scratch goes
//! (`docs/DESIGN-host-boundary-2026-08-30.md`).

use aterm_core::grid::LineSize;
use aterm_core::grid::extra::ImageRef;
use aterm_core::terminal::RenderCell;
use aterm_render::{RainHalo, SpriteQuad};
use aterm_time::Instant;

use super::{EffectGeom, MatrixRain, RainSignal, RainTickInput, RainVisibility};

/// The rain driver's per-surface memory (a native window; a web page): the
/// two edges the weather reacts to, each keyed by session so a tab switch
/// re-baselines instead of replaying another session's history as news.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RainLatches {
    /// `(session, completed_command_seq)` of the last completion observed.
    /// Seq 0 is "watching, none seen": the None→Some edge inside one session
    /// is a real first completion and fires.
    pub last_cmd: Option<(u64, u64)>,
    /// `(session, executing)` for the OSC 133/633 C rising edge.
    pub shell_executing: Option<(u64, bool)>,
}

impl RainLatches {
    /// Record the shell's Execute state for `session` and return whether this
    /// is its rising edge (executing now, not executing last frame in the same
    /// session). Called every frame, rain on or off, so an enable mid-command
    /// does not read the command's start as news.
    #[inline]
    pub const fn note_shell_executing(&mut self, session: u64, executing: bool) -> bool {
        let rising =
            matches!(self.shell_executing, Some((sid, false)) if sid == session) && executing;
        self.shell_executing = Some((session, executing));
        rising
    }

    /// Keep the completion latch baselined on a frame that draws no rain (a
    /// suspended pane, a disabled session), so a completion during suppression
    /// does not fire a stale ember/wave on resume.
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

    /// A frame on which the surface runs no rain engine: both latches keep
    /// observing (Execute level and completion baseline), so an engine built
    /// on enable reads neither a running command's start nor a completion
    /// from while the rain was off as news.
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

/// The coherent snapshot a frame may scan: the host's own extraction at
/// `epoch`, read under the lock that produced it (native: the frame hold; a
/// page: an untorn `cell_frame`). `None` on a frame that may not scan; the
/// Tier-A bitset then keeps the last good scan.
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
    /// A web host already fed the frame's milliseconds through
    /// [`MatrixRain::advance_ms`]; the frame consumes them
    /// ([`MatrixRain::emit`]).
    Accumulated,
}

/// Everything one rain frame reads from its host.
#[derive(Clone, Copy)]
pub struct RainFrame<'a> {
    pub clock: RainClock,
    /// Draw nothing this frame: the alternate screen under
    /// `suppress_in_alt_screen`, the load-shed latch, or a session whose rain
    /// is off while its engine drains.
    pub suspended: bool,
    /// The motion policy lets rain animate (`MotionEffect::MatrixRain`);
    /// `false` is the engine's reduced-motion arm (it emits nothing).
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
    /// The visible cursor cell the material sampler keeps the typed line out
    /// of (`None` while hidden or in history).
    pub cursor: Option<(u16, u16)>,
    pub geom: EffectGeom,
    pub tick: RainTickInput<'a>,
}

impl MatrixRain {
    /// Run one frame: the suspended arm, or the policy, the notes, the gated
    /// scan and the emission. Returns the frame's fingerprint (`0` when
    /// nothing was drawn).
    pub fn host_frame(
        &mut self,
        f: &RainFrame<'_>,
        latches: &mut RainLatches,
        quads: &mut Vec<SpriteQuad>,
        add: &mut Vec<RainHalo>,
    ) -> u64 {
        if f.suspended {
            // The weather still advances via the cheap suspended tick (no
            // bake, no field walk, no quads): notes starve, the drain
            // completes, and `is_active` self-disarms, so a suspended pane
            // does not keep scheduling wakes.
            match f.clock {
                RainClock::At(now) => self.tick_suspended(now),
                RainClock::Accumulated => self.step_suspended(),
            }
            latches.baseline_completion(f.session, f.cmd_done.map(|(seq, _)| seq));
            quads.clear();
            add.clear();
            return 0;
        }
        // Reduced motion: the engine emits nothing (fp 0). Visibility is fed
        // every frame too (edge-detected in the engine), so a lazily-built
        // engine observes the current focus.
        self.set_reduced_motion(!f.animate);
        self.set_visibility(f.visibility);
        // Agent-output weather signal: only a seq change registers.
        self.note_activity(f.content_seq);
        if f.execute_edge {
            self.note_signal(RainSignal::Execute as u32, 4);
        }
        if let Some(failed) = latches.take_new_completion(f.session, f.cmd_done) {
            self.note_exit_status(failed);
        }
        // Frames that cannot emit (reduced motion, unfocused past the drain)
        // skip the O(rows·cols) rescan. `last_epoch` only advances inside the
        // rescan, so `needs_rescan` stays true until the first eligible frame.
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
            // Output material bank: from the same snapshot and gate, the rain's
            // alphabet becomes literal codepoints from program output,
            // excluding the typed line and the hidden-cursor band.
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

    /// The Execute edge is a rising edge of the same session: a first sight,
    /// a session switch and a held state are not news.
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

    /// A completion is news once, in the session that produced it; a tab
    /// switch and a suspension-era completion re-baseline silently.
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

    /// With no engine running, both latches still observe: a command that
    /// started or finished while the rain was off is not news to the engine
    /// an enable builds. Control: watching Execute alone replays the completion.
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
