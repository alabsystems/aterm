// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE ADVISORY LAUNCH-TIME WARM HINT (docs/DESIGN-warm-successor-2026-09-29.md
//! §3, phase P2).
//!
//! A successor on the launched lane now runs its warm prologue — config,
//! theme, fonts and the backend worker — BEFORE it dials (`main_entry`'s
//! `WarmOrder::BeforeDial`), so the one carried fact that prologue used to read
//! from the authenticated window carry, the outgoing window's FONT ZOOM, is not
//! known yet when the backend is built. The outgoing process therefore names
//! it in the launch environment, beside the grant capabilities, as
//! [`ENV_WARM_HINT`]: the zoom pair the carry will hold, plus the window and
//! session counts and window 0's grid, for the log's hit/miss line.
//!
//! ADVISORY, NEVER AUTHENTICATED. The hint is a guess about a capture that has
//! not happened yet (the automatic lane can hold a dialled successor up to its
//! park cap before it captures), written by a process that is not yet proven
//! to be anything. So:
//!
//! * it decides only WHICH px the backend worker builds and pre-warms at;
//! * the authenticated manifest wins at the intake: the launch font is derived
//!   from the carry exactly as before (`app_config::successor_font_px`), and a
//!   build px that differs is re-selected at the backend join
//!   (`App::warm_miss_px`, the light `activate_px` switch every zoom uses) —
//!   a stale or hostile hint costs a warm miss and says so in the log, nothing
//!   else;
//! * it lives in its own type, disjoint from `seamless::IncomingHandoff`, and
//!   no digest, proof or admission ever reads it (`HintNotInProof` in
//!   `NativeUpdateSuccessorWarmBeforeClaim`; the Tier-1 in
//!   `seamless::handoff_env_conformance` proves the adoption proof is the
//!   parent's expectation with a stale and a hostile hint present).
//!
//! REVERSIBLE. Absent (an older parent, or the fork lane), the successor warms
//! at its config's size — today's behaviour for every unzoomed window — and
//! `main_entry`'s `warm_order` is the one switch that puts the prologue back
//! after the intake.

/// The launch-environment name. Captured (read and cleared) with every other
/// handoff name by `HandoffEnv::capture`; read only from that snapshot.
pub(crate) const ENV_WARM_HINT: &str = "ATERM_HANDOFF_WARM_HINT";

/// The only wire version this build writes or reads. A different one is
/// ignored whole: a newer parent's hint is a guess this build cannot read,
/// and ignoring a guess is always safe.
const VERSION: &str = "w1";

/// Longest value read. The encoded hint is under 80 bytes; anything longer is
/// not one this build wrote.
const MAX_LEN: usize = 256;

/// What the outgoing process expects its capture to carry, at launch time.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct WarmHint {
    /// Windows the outgoing process had open.
    pub(crate) windows: u32,
    /// Live sessions (panes) across all of them.
    pub(crate) sessions: u32,
    /// Window 0's grid, as the carry writes it.
    pub(crate) cols: u16,
    pub(crate) rows: u16,
    /// The live font zoom in thousandths of a physical px, as the carry
    /// writes it (`App::handoff_font_zoom`): both halves or neither.
    pub(crate) zoom: Option<(u32, u32)>,
}

impl WarmHint {
    /// The launch-environment value: `w1;windows=N;sessions=N;cols=N;rows=N` and,
    /// while a zoom is live, `;px=N;reset=N`.
    #[must_use]
    pub(crate) fn encode(&self) -> String {
        let mut out = format!(
            "{VERSION};windows={};sessions={};cols={};rows={}",
            self.windows, self.sessions, self.cols, self.rows
        );
        if let Some((px, reset)) = self.zoom {
            out.push_str(&format!(";px={px};reset={reset}"));
        }
        out
    }

    /// Read a value [`Self::encode`] wrote. `None` for anything else: another
    /// version, an oversized value, a malformed or repeated field, or a
    /// missing count. A zoom with one half missing reads as no zoom, as the
    /// carry's does. Unknown fields are ignored, so a later `w1` writer may add
    /// facts without a version bump.
    #[must_use]
    pub(crate) fn parse(raw: &str) -> Option<Self> {
        if raw.len() > MAX_LEN {
            return None;
        }
        let mut fields = raw.split(';');
        if fields.next()? != VERSION {
            return None;
        }
        let (mut windows, mut sessions, mut cols, mut rows, mut px, mut reset) =
            (None, None, None, None, None, None);
        for field in fields {
            let (key, value) = field.split_once('=')?;
            let slot = match key {
                "windows" => &mut windows,
                "sessions" => &mut sessions,
                "cols" => &mut cols,
                "rows" => &mut rows,
                "px" => &mut px,
                "reset" => &mut reset,
                _ => continue,
            };
            if slot.is_some() {
                return None;
            }
            *slot = Some(value.parse::<u32>().ok()?);
        }
        Some(Self {
            windows: windows?,
            sessions: sessions?,
            cols: u16::try_from(cols?).ok()?,
            rows: u16::try_from(rows?).ok()?,
            zoom: px.zip(reset),
        })
    }

    /// The zoom pair in the carry's shape (`WindowCarry::font_px_milli`,
    /// `font_reset_px_milli`), for `app_config::launch_font_for_zoom`.
    #[must_use]
    pub(crate) fn zoom_milli(&self) -> (Option<u32>, Option<u32>) {
        self.zoom
            .map_or((None, None), |(px, reset)| (Some(px), Some(reset)))
    }
}

/// Take the hint out of the handoff snapshot. `Ok(None)`: none was offered;
/// `Err(raw)`: one was offered and is not readable (said in the log, and
/// treated as none).
pub(crate) fn take_from(
    env: &mut crate::handoff_env::HandoffEnv,
) -> Result<Option<WarmHint>, String> {
    let Some(raw) = env.take(ENV_WARM_HINT) else {
        return Ok(None);
    };
    let raw = raw.to_string_lossy();
    WarmHint::parse(&raw)
        .map(Some)
        .ok_or_else(|| raw.into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hint_round_trips_with_and_without_a_zoom() {
        let plain = WarmHint {
            windows: 3,
            sessions: 6,
            cols: 100,
            rows: 30,
            zoom: None,
        };
        assert_eq!(plain.encode(), "w1;windows=3;sessions=6;cols=100;rows=30");
        assert_eq!(WarmHint::parse(&plain.encode()), Some(plain));
        let zoomed = WarmHint {
            zoom: Some((28_000, 24_000)),
            ..plain
        };
        assert_eq!(WarmHint::parse(&zoomed.encode()), Some(zoomed));
        assert_eq!(zoomed.zoom_milli(), (Some(28_000), Some(24_000)));
        assert_eq!(plain.zoom_milli(), (None, None));
    }

    /// The successor is the hostile-input boundary: whatever arrives, the
    /// answer is a hint this build wrote or none.
    #[test]
    fn anything_else_reads_as_no_hint() {
        for raw in [
            "",
            "w2;windows=1;sessions=1;cols=80;rows=24",
            "windows=1;sessions=1;cols=80;rows=24",
            "w1;windows=1;sessions=1;cols=80",
            "w1;windows=1;sessions=1;cols=80;rows=24;rows=25",
            "w1;windows=-1;sessions=1;cols=80;rows=24",
            "w1;windows=1;sessions=1;cols=70000;rows=24",
            "w1;windows=1;sessions=1;cols=80;rows=24;px",
            "w1;windows=1;sessions=1;cols=80;rows=24;px=abc;reset=1",
        ] {
            assert_eq!(WarmHint::parse(raw), None, "{raw:?}");
        }
        let long = format!(
            "w1;windows=1;sessions=1;cols=80;rows=24;pad={}",
            "9".repeat(300)
        );
        assert_eq!(WarmHint::parse(&long), None, "oversized");
        // A half pair is no zoom, as in the carry; an unknown field is skipped.
        assert_eq!(
            WarmHint::parse("w1;windows=1;sessions=2;cols=80;rows=24;px=20000;glyphs=x")
                .map(|hint| hint.zoom),
            Some(None)
        );
    }
}
