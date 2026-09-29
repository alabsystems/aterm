// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **THE RAIN DRIVER'S ONE FRAME** — the sequencing every host runs around the
//! PHOSPHOR engine: the suspended arm (the weather advances, nothing is
//! drawn, the completion latch stays baselined), the policy fed each frame
//! (reduced motion, visibility), the weather's notes (agent output, the
//! shell's Execute edge, a NEW command completion), the grid scan gated by
//! the host's coherent snapshot, and the emission on the host's clock.
//!
//! This was `aterm-gui`'s single-pane PHOSPHOR block, and the web pipeline
//! ran a thinner twin that never heard a command finish or start
//! (`docs/DESIGN-host-boundary-2026-08-30.md` Phase 4, decision 7). The
//! host keeps what is the host's: whether a frame may scan at all (the
//! native frame hold, the web's torn-snapshot check), the hidden-cursor
//! damage band it maintains under its own lock, and where the scratch goes.

use aterm_core::grid::LineSize;
use aterm_core::grid::extra::ImageRef;
use aterm_core::terminal::RenderCell;
use aterm_render::{RainHalo, SpriteQuad};
use aterm_time::Instant;

use super::{EffectGeom, MatrixRain, RainSignal, RainTickInput, RainVisibility};

/// The rain driver's per-surface memory (a native window; a web page): the
/// two edges the weather reacts to, each keyed by the session that produced
/// it so a tab switch re-BASELINES instead of replaying another session's
/// history as news.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RainLatches {
    /// `(session, completed_command_seq)` of the last completion observed.
    /// Seq 0 is "watching, none seen": the None→Some edge inside one session
    /// is a REAL first completion and fires.
    pub last_cmd: Option<(u64, u64)>,
    /// `(session, executing)` for the OSC 133/633 C rising edge.
    pub shell_executing: Option<(u64, bool)>,
}

impl RainLatches {
    /// Record the shell's Execute state for `session` and answer whether this
    /// is its RISING edge: executing now, and the same session was NOT
    /// executing last frame. The host calls this every frame, rain on or off,
    /// so an enable mid-command never reads the command's start as news.
    pub fn note_shell_executing(&mut self, session: u64, executing: bool) -> bool {
        let rising =
            matches!(self.shell_executing, Some((sid, false)) if sid == session) && executing;
        self.shell_executing = Some((session, executing));
        rising
    }

    /// Keep the completion latch BASELINED on a frame that draws no rain (a
    /// suspended pane, a disabled session): a command finishing during a long
    /// suppression must not be observed as "new" minutes later on resume and
    /// fire a stale ember/wave. Suspension-era completions are absorbed.
    pub fn baseline_completion(&mut self, session: u64, cmd_seq: Option<u64>) {
        self.last_cmd = Some((session, cmd_seq.unwrap_or(0)));
    }

    /// A frame on which the surface runs NO rain engine (rain off, never
    /// built): both latches keep observing — the Execute level and the
    /// completion baseline — so the engine an enable builds reads neither
    /// the running command's start nor a command that finished while the rain
    /// was off as news.
    pub fn observe_without_engine(&mut self, session: u64, executing: bool, cmd_seq: Option<u64>) {
        let _ = self.note_shell_executing(session, executing);
        self.baseline_completion(session, cmd_seq);
    }

    /// EXIT STATUS → weather (OSC 133/633): `Some(failed)` once per NEW
    /// completion in the SAME session; a tab switch re-baselines silently.
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
/// page: an untorn `cell_frame`). `None` on a frame that may not scan — the
/// Tier-A bitset then keeps the last honest scan.
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
    /// THE ONE FRAME: the suspended arm, or the policy, the notes, the gated
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
            // The WEATHER machine still advances via the cheap suspended tick
            // (no bake, no field walk, no quads): notes starve, the weather
            // sleeps, the drain completes, and `is_active` self-disarms — a
            // suspended pane must never leak perpetual wakes off a frozen
            // Working/Calm state.
            match f.clock {
                RainClock::At(now) => self.tick_suspended(now),
                RainClock::Accumulated => self.step_suspended(),
            }
            latches.baseline_completion(f.session, f.cmd_done.map(|(seq, _)| seq));
            quads.clear();
            add.clear();
            return 0;
        }
        // W11: a Reduced policy means the engine emits NOTHING (fp 0) —
        // bypass-to-final-state, proven exactly-zero by the motion totality
        // tests. Visibility every frame too (cheap: edge-detected in the
        // engine), so a lazily-built engine observes the CURRENT focus.
        self.set_reduced_motion(!f.animate);
        self.set_visibility(f.visibility);
        // The agent-output weather signal: only an actual seq change registers.
        self.note_activity(f.content_seq);
        if f.execute_edge {
            self.note_signal(RainSignal::Execute as u32, 4);
        }
        if let Some(failed) = latches.take_new_completion(f.session, f.cmd_done) {
            self.note_exit_status(failed);
        }
        // Frames that CANNOT emit (reduced motion, unfocused past the drain)
        // skip the O(rows·cols) rescan: emission is gated there anyway, and
        // `last_epoch` only advances inside the rescan, so `needs_rescan`
        // stays true and the scan runs on the first eligible frame.
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
            // OUTPUT MATERIAL BANK: the same snapshot, the same gate — the
            // rain's alphabet becomes literal codepoints from program output,
            // the typed line and the hidden-cursor band excluded.
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

    /// The Execute edge is a RISING edge of the SAME session: a first sight,
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
    /// an enable builds. The latch that only watched Execute (the web
    /// pipeline's off arm before 2026-09-27) replays the completion.
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
