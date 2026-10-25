// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The paint matrix: one row is a scene typed into a release `aterm`, captured,
//! scanned ([`super::scan`]) and judged against what the row expects.

use super::instance::{CouldNotRun, could_not_run};
use super::scan::{ScanReport, StatusRow};
use super::starvation::{Disposition, SchedReading, ShapeVerdict, Starvation};

/// The ink floor on a take: dynamic saturated pixels across all its frames.
pub const MIN_INK: usize = 150;
/// The hue floor: buckets across the take.
pub const MIN_HUES: usize = 4;
/// The busiest single frame must carry a visible clump.
pub const MIN_BEST_INK: usize = 5;
/// Minimum frame count required for a take to decide anything.
pub const MIN_FRAMES: usize = 5;
/// The excused lead before a recorded band first reaches glass.
pub const MATURE_LEAD_MAX_US: i64 = 500_000;
/// The take's wall-clock budget, against which a take racing it is disowned.
pub const BUDGET_SECS: u64 = 180;

/// What the subject is doing while it is typed into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scene {
    /// The main screen at a `/bin/sh` prompt, per-key echo.
    Prompt,
    /// The fake-Claude TUI: alt screen, DEC-2026 bracketed repaints, a 150 ms spinner, per-key echo.
    FakeClaude,
    /// The fake-Claude TUI with nothing typed: the dark control.
    ColdSpinner,
    /// Alt screen, an ESC7/ESC8 token streamer above the typing row.
    Streamer,
    /// Alt screen, an append-shaped stream walking a shown caret, nothing typed.
    ColdStreamer,
}

impl Scene {
    /// The `fake-tui` shape the scene runs, if any.
    #[must_use]
    pub const fn tui(self) -> Option<&'static str> {
        match self {
            Self::Prompt => None,
            Self::FakeClaude | Self::ColdSpinner => Some("claude"),
            Self::Streamer => Some("streamer"),
            Self::ColdStreamer => Some("cold-streamer"),
        }
    }

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Prompt => "prompt",
            Self::FakeClaude => "fake-claude",
            Self::ColdSpinner => "cold-spinner",
            Self::Streamer => "streamer",
            Self::ColdStreamer => "cold-streamer",
        }
    }
}

/// What a row expects of its take.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Expect {
    /// Effect ink across the take.
    Ink,
    /// Zero effect ink in every frame (a style with no resident companion).
    Dark,
    /// Zero trail, a resident companion allowed.
    TrailDark,
    /// The take fails the ink predicate.
    Quiet,
    /// Typed cursor/text pixels with the canonical off style.
    EffectOff,
}

/// How the take is captured, and so what the instrument perturbs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Capture {
    /// `ctl video`: the recording pins the window's motion focus.
    PinnedVideo,
    /// A burst of `ctl image` renders: no recording, focus as the app has it.
    UnpinnedFocused,
    /// The burst with the window unfocused (`ctl focus out`).
    UnpinnedUnfocused,
}

impl Capture {
    #[must_use]
    pub fn is_video(self) -> bool {
        self == Self::PinnedVideo
    }
}

/// The cursor-trail config line a row runs under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    /// `"rainbow kitty"`: the walking pet.
    Classic,
    /// The key left absent: the shipped default (`rainbow kitty pet`).
    Default,
    Underline,
    /// `"rainbow kitty flying"`: the companion-free style that earns the flying head.
    Flying,
    Tall,
    /// The canonical off spelling: the matched negative control.
    Off,
}

impl Style {
    /// The config line, or `None` for the shipped default.
    #[must_use]
    pub const fn config_line(self) -> Option<&'static str> {
        match self {
            Self::Classic => Some("cursor_trail_style = \"rainbow kitty\""),
            Self::Default => None,
            Self::Underline => Some("cursor_trail_style = \"rainbow kitty underline\""),
            Self::Flying => Some("cursor_trail_style = \"rainbow kitty flying\""),
            Self::Tall => Some("cursor_trail_style = \"rainbow kitty tall\""),
            Self::Off => Some("cursor_trail_style = \"off\""),
        }
    }

    /// The `style="…"` the engine must report, and the tokens that bind it to what it resolved to.
    #[must_use]
    pub const fn binding(self) -> (&'static str, &'static [(&'static str, &'static str)]) {
        const TALL: &[(&str, &str)] = &[("resolved", "rainbow-kitty"), ("ribbon_look", "tall")];
        match self {
            Self::Classic => ("rainbow kitty", TALL),
            Self::Default => ("rainbow kitty pet", TALL),
            Self::Underline => (
                "rainbow kitty underline",
                &[("resolved", "rainbow-kitty"), ("ribbon_look", "underline")],
            ),
            Self::Flying => ("rainbow kitty flying", TALL),
            Self::Tall => ("rainbow kitty tall", TALL),
            Self::Off => ("off", &[("effective", "false")]),
        }
    }

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Classic => "classic",
            Self::Default => "default",
            Self::Underline => "underline",
            Self::Flying => "flying",
            Self::Tall => "tall",
            Self::Off => "off",
        }
    }

    /// Whether `row` reports this style, bound to what it resolved to.
    #[must_use]
    pub fn bound_by(self, row: &StatusRow) -> bool {
        let (style, tokens) = self.binding();
        row.quoted("style") == Some(style) && tokens.iter().all(|&(k, v)| row.value(k) == Some(v))
    }
}

/// A companion whose engine state and drawn bitmap are both obligations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Companion {
    /// The resident pet: claimed and visibly whole from frame zero on.
    Pet,
    /// The flying head: earned by a sustained run, then whole in three isolated final stills.
    Cat,
}

/// A split before the scene runs, so every frame goes through the composed path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Split {
    V,
    H,
}

/// One matrix row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PaintRow {
    pub scene: Scene,
    /// Comma-separated `ctl key` names; empty for a cold row.
    pub keys: &'static str,
    pub expect: Expect,
    pub capture: Capture,
    pub style: Style,
    pub companion: Option<Companion>,
    pub split: Option<Split>,
}

impl PaintRow {
    #[must_use]
    pub fn keys(&self) -> Vec<&'static str> {
        self.keys.split(',').filter(|k| !k.is_empty()).collect()
    }

    /// The combinations that could not measure what they claim.
    ///
    /// # Errors
    /// The row contradicts itself.
    pub fn validate(&self) -> Result<(), CouldNotRun> {
        if self.expect == Expect::EffectOff
            && (self.style != Style::Off || self.keys.is_empty() || self.companion.is_some())
        {
            return could_not_run(
                "an effect-off row needs typed keys, the off style and no companion",
            );
        }
        if let Some(c) = self.companion {
            if self.capture.is_video() {
                return could_not_run(
                    "a companion row needs an image burst, so every claim binds to its frame",
                );
            }
            if self.scene != Scene::Prompt {
                return could_not_run("companion morphology uses the isolated prompt plane");
            }
            let ok = match c {
                Companion::Pet => matches!(self.style, Style::Default | Style::Classic),
                Companion::Cat => self.style == Style::Flying,
            };
            if !ok {
                return could_not_run("the companion judges a spelling its style does not select");
            }
        }
        Ok(())
    }

    /// What a red on this row means.
    #[must_use]
    pub const fn meaning(&self) -> &'static str {
        match (self.expect, self.capture) {
            (Expect::EffectOff, _) => {
                "the canonical off style leaked trail geometry, or the scanner attributed unrelated \
                 pixels to a ribbon"
            }
            (_, Capture::PinnedVideo) => {
                "the shipped binary does not paint its flagship effect: this window was FOCUSED and \
                 RECORDED, every excuse already granted"
            }
            (_, Capture::UnpinnedFocused) => {
                "the engine and the captured pixels disagree about the trail or the companion, or the \
                 rainbow has an interior blackout"
            }
            (_, Capture::UnpinnedUnfocused) => {
                "an UNFOCUSED window being typed into does not paint its trail (the typed-wake term of \
                 App::cursor_fx_focus); a recording would have hidden this"
            }
        }
    }

    /// The recorded take's length, at least 5 s.
    #[must_use]
    pub fn record_secs(&self) -> u64 {
        let n = self.keys().len();
        if n == 0 {
            return 5;
        }
        #[allow(clippy::cast_precision_loss)]
        let need = 0.7 + (n as f64 - 1.0) * 0.12 + n as f64 * 0.15 + 1.0;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let need = need.ceil() as u64;
        need.max(5)
    }
}

/// What the take observed besides its pixels.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ledger {
    /// One `trail status` row per frame (image bursts).
    pub rows: Option<Vec<StatusRow>>,
    /// One row read after a recorded take.
    pub final_row: Option<StatusRow>,
    /// Effect ticks that composed the ribbon body's stream (`fx_under_frames`).
    pub fx_under_frames: u64,
}

impl Ledger {
    fn all(&self, key: &str, value: &str) -> bool {
        match (&self.rows, &self.final_row) {
            (Some(rows), _) => rows.iter().all(|r| r.value(key) == Some(value)),
            (None, Some(row)) => row.value(key) == Some(value),
            (None, None) => false,
        }
    }

    /// The engine's quiet set: nothing licensed or spawned, and no live light.
    #[must_use]
    pub fn trail_quiet(&self) -> bool {
        [
            ("licensed", "0"),
            ("spawns", "0"),
            ("ribbon_active", "false"),
            ("sparks", "0"),
            ("glow_active", "false"),
        ]
        .iter()
        .all(|(k, v)| self.all(k, v))
    }

    /// No companion claimed anywhere.
    #[must_use]
    pub fn companions_off(&self) -> bool {
        self.all("pet_active", "false") && self.all("cat_active", "false")
    }

    /// Every row reports `style`, and an image burst paired a row with every frame.
    #[must_use]
    pub fn style_bound(&self, style: Style, frames: usize) -> bool {
        match (&self.rows, &self.final_row) {
            (Some(rows), _) => rows.len() == frames && rows.iter().all(|r| style.bound_by(r)),
            (None, Some(row)) => style.bound_by(row),
            (None, None) => false,
        }
    }
}

fn continuity_holds(row: &PaintRow, s: &ScanReport) -> Result<bool, CouldNotRun> {
    if s.driven.dark_us < 0 {
        if row.capture.is_video() {
            return could_not_run(
                "the driven window was never bounded on a video take (a `video status` bracket \
                 read did not answer): nothing was decided about continuity",
            );
        }
        return Ok(s.rainbow_gap <= 1);
    }
    Ok(2 * s.driven.dark_us <= 3 * s.driven.nominal_dt_us
        && s.driven.mature_lead_us <= MATURE_LEAD_MAX_US)
}

fn ink_holds(row: &PaintRow, s: &ScanReport) -> Result<bool, CouldNotRun> {
    let floors = s.total_ink >= MIN_INK
        && s.union_hues >= MIN_HUES
        && s.best_ink >= MIN_BEST_INK
        && s.rainbow_frames >= 2
        && s.rainbow_run >= 2;
    if !floors || !continuity_holds(row, s)? {
        return Ok(false);
    }
    Ok(s.ribbon_dark == 0 && (s.ribbon_bound < 2 || s.ribbon_window_hues >= 4))
}

/// The row's expectation against the take, before the instrument's account.
///
/// # Errors
/// The take was too short to decide anything, or a recorded take lost its
/// driven bracket.
pub fn judge(row: &PaintRow, ledger: &Ledger, s: &ScanReport) -> Result<ShapeVerdict, CouldNotRun> {
    if s.frames < MIN_FRAMES {
        return could_not_run(format!(
            "the take carried only {} frame(s): too few to decide anything",
            s.frames
        ));
    }
    let pass = match row.expect {
        Expect::Ink => {
            ink_holds(row, s)?
                && (s.status_frames == 0 || s.ribbon_bound >= 2)
                && ledger.fx_under_frames >= 2
        }
        Expect::Dark => s.total_ink == 0,
        Expect::TrailDark => ledger.trail_quiet() && s.ribbon_claimed == 0 && s.ribbon_dark == 0,
        Expect::Quiet => !ink_holds(row, s)?,
        Expect::EffectOff => {
            let window_dark = !row.capture.is_video()
                || (s.driven.dark_us >= 0
                    && s.driven.nominal_dt_us > 0
                    && 2 * s.driven.dark_us > 3 * s.driven.nominal_dt_us);
            ledger.trail_quiet()
                && ledger.companions_off()
                && window_dark
                && s.total_ink > 0
                && s.rainbow_frames == 0
                && s.ribbon_claimed == 0
                && ledger.fx_under_frames == 0
        }
    };
    let style_ok = ledger.style_bound(row.style, s.frames);
    let companion_ok = match row.companion {
        None => true,
        Some(Companion::Pet) => {
            s.pet_claimed == s.frames
                && s.cat_claimed == 0
                && s.base_comp.is_some()
                && s.companion_stack == 0
                && s.pet_final_run >= 3
        }
        Some(Companion::Cat) => {
            s.pet_claimed == 0
                && s.cat_claimed >= 4
                && s.companion_frames >= 3
                && s.cat_final_run >= 3
                && s.companion_stack == 0
                && s.cat_last_present
        }
    };
    Ok(if pass && style_ok && companion_ok {
        ShapeVerdict::Pass
    } else {
        ShapeVerdict::Fail
    })
}

/// One row's measured outcome.
#[derive(Clone, Debug)]
pub struct PaintReport {
    pub row: PaintRow,
    pub backend: String,
    pub scan: ScanReport,
    pub ledger: Ledger,
    pub shape: ShapeVerdict,
    pub sched: SchedReading,
    pub starvation: Starvation,
    pub disposition: Disposition,
    pub wall_us: u64,
    /// How long the alt-screen scene took to switch past its launch.
    pub alt_wait_ms: u64,
    /// The cadence the subject observed typed keys at (image takes): median and worst gap.
    pub typed_gap_ms: Option<(u64, u64)>,
    /// A video take's driven bracket on the recording's own clock, `lo:hi` ms.
    pub driven_ms: Option<(i64, i64)>,
    /// The scheduling band the subject ran in.
    pub band: super::instance::Band,
    /// The scheduling band the bench (the driver and the sampler) ran in.
    pub bench_band: super::instance::Band,
}

impl std::fmt::Display for PaintReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (r, s) = (&self.row, &self.scan);
        write!(
            f,
            "PAINT scene={} style={} companion={:?} capture={:?} backend={} expect={:?} \
             frames={} best_ink={} best_hues={} total_ink={} union_hues={} cyan_px={} \
             cyan_frames={} rainbow_frames={} rainbow_run={} rainbow_gap={} driven_dark_us={} \
             driven_frames={} sampling_hole_us={} nominal_dt_us={} mature_lead_us={} \
             status_frames={} ribbon_claimed={} ribbon_bound={} ribbon_dark={} \
             ribbon_window_hues={} pet_claimed={} pet_missing={} pet_final_run={} cat_claimed={} \
             cat_missing={} cat_final_run={} companion_frames={} companion_stack={} \
             fx_under_frames={} typed_gap_ms={:?} driven_ms={} alt_wait_ms={} wall_us={} sched_ticks={} \
             sched_late_max_us={} sched_window_ticks={} sched_window_late_max_us={} qos={:?} \
             subject_band={} bench_band={} starved={:?} reason=\"{}\" shape={:?} verdict={:?}",
            r.scene.name(),
            r.style.name(),
            r.companion,
            r.capture,
            self.backend,
            r.expect,
            s.frames,
            s.best_ink,
            s.best_hues,
            s.total_ink,
            s.union_hues,
            s.cyan_px,
            s.cyan_frames,
            s.rainbow_frames,
            s.rainbow_run,
            s.rainbow_gap,
            s.driven.dark_us,
            s.driven.frames,
            s.driven.sampling_hole_us,
            s.driven.nominal_dt_us,
            s.driven.mature_lead_us,
            s.status_frames,
            s.ribbon_claimed,
            s.ribbon_bound,
            s.ribbon_dark,
            s.ribbon_window_hues,
            s.pet_claimed,
            s.pet_missing,
            s.pet_final_run,
            s.cat_claimed,
            s.cat_missing,
            s.cat_final_run,
            s.companion_frames,
            s.companion_stack,
            self.ledger.fx_under_frames,
            self.typed_gap_ms,
            self.driven_ms
                .map_or_else(|| "none".to_string(), |(lo, hi)| format!("{lo}:{hi}")),
            self.alt_wait_ms,
            self.wall_us,
            self.sched.ticks,
            self.sched.late_max_us,
            self.sched.win_ticks,
            self.sched.win_late_max_us,
            self.sched.qos,
            self.band,
            self.bench_band,
            self.starvation.label(),
            self.starvation,
            self.shape,
            self.disposition,
        )
    }
}

#[cfg(test)]
#[path = "paint_tests.rs"]
mod tests;
