// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! ROBI'S TIP BUBBLE — the speech bubble half of the helper robot's show
//! (`aterm_effects::robi::ROBI_TIPS`): a small floating [`DrawPrim`] card that
//! drops in over the speaker's head, holds while he says his line, then lifts
//! away over a few seconds. A DECORATION, not a message (design D7, R23): it
//! is not posted to the message center, not logged, not announced to
//! assistive technology, and a press on it only dismisses it. GLOBAL
//! (App-level, `App::robi_bubble`) like the robot's own state, painted into
//! the window whose Robi is speaking through the SAME rasterizer + composite
//! path the build badge and the Settings overlay use, as pure [`DrawPrim`]s
//! (native chrome, NOT terminal grid cells) — every text run through
//! [`text_prim`], so `widget::every_text_prim_goes_through_the_funnel` keeps
//! its count.
//!
//! This is what survives of the retired transient notice (`notice.rs`,
//! 2026-09-23): the notice's producers — the update lane, the gesture
//! failures, the admin and file-access cards — are message-band rows now, and
//! the one thing that was never a message kept the widget. The timed shape
//! (`is_expired` + `deadline`), the eased entrance and exit ramps, the
//! quantized fingerprint and the shadow budget are carried over unchanged;
//! the caption grammar, the tones, the two-control layout and the sparkle
//! ring left with the kinds that wore them.
//!
//! # Where it sits
//!
//! The bubble carries an ANCHOR — `(x_center, y_top_of_speaker)` in tray px —
//! refreshed every frame by the redraw so it follows a swinging Robi. It
//! centres over the anchor and sits just above his head, clamped inside the
//! tray and below the in-grid chrome rows (the tab strip: the bubble must
//! never cover chrome the user clicks). Without an anchor — the tip was
//! requested a frame before the sprite's rect was known — it takes the
//! top-centre rest position the retired notice used, so a tip is never lost
//! to a missing coordinate.

use std::time::{Duration, Instant};

use aterm_render::Theme;

use crate::settings::{Roles, SettingsGeom, text_w};
use crate::tray_raster::{baseline_centered_at, row_baseline, ui_text_width_for};
use crate::type_scale::{StepPx, TypeStep};
use crate::widget::{DrawPrim, TextFace, TextWeight, TrayInput, rgba, text_prim};

/// How long the bubble stays up before it is fully gone (slide-in + hold + lift-away).
const TTL: Duration = Duration::from_millis(5400);
/// The entrance: the bubble eases down into place and fades up over this opening stretch.
const ENTER: Duration = Duration::from_millis(220);
/// The exit tail: the last stretch of [`TTL`] over which the bubble lifts and alpha
/// ramps 1→0. An eased 0.8 s reads as "it left"; a long linear fade reads as sluggish.
const FADE: Duration = Duration::from_millis(800);
/// Animation cadence while the bubble is MOVING (entrance/exit) — the deadline
/// granularity. ~60fps: the bubble is a ~300×34px raster, and the motion is under a
/// second in total.
const FRAME: Duration = Duration::from_millis(16);

/// How far outside the bubble rect the soft shadow reaches, in px. The compositor
/// ([`crate::App::splice_robi_bubble`]) grows its paint region by exactly this, so the
/// shadow cannot be cropped and the two cannot drift.
pub(crate) const SHADOW_MARGIN: f32 = 12.0;

/// The stacked shadow layers as `(spread, dy, alpha)`, widest and faintest last.
///
/// Data rather than a literal inside the loop so [`shadow_stays_inside_its_margin`]
/// can re-derive the furthest reach from the same numbers the renderer draws. The
/// comment beside the loop has always claimed these stay inside [`SHADOW_MARGIN`];
/// until this was a table, nothing checked it, and a fourth layer or a bigger
/// spread would have been cropped by the compositor's paint region with no test to
/// say so.
const SHADOW_LAYERS: [(f32, f32, u8); 3] = [(1.0, 1.0, 0x22), (3.0, 2.5, 0x16), (6.5, 4.5, 0x0C)];

/// The alpha below which the bubble stops being a click target. The exit tail runs
/// it down to nothing, and an all-but-invisible thing that swallows clicks — the
/// press under it is a press on the robot, or on a tab — is a trap. Above this the
/// bubble is plainly on screen.
pub(crate) const CLICK_MIN_ALPHA: f32 = 0.35;

/// The rest position's distance below the first row the bubble is allowed to
/// occupy, in cell heights — the anchor-less fallback placement. The bubble floats
/// over terminal OUTPUT by design (it is transient), but it must never float over
/// in-grid CHROME — see `clear_rows` on [`layout`].
const REST_Y_CELLS: f32 = 0.85;
/// How far the bubble travels on its way in and out, in cell heights.
const SLIDE_CELLS: f32 = 0.42;

/// The badge pictogram: the gear Robi's tips have always worn.
const BADGE_GLYPH: &str = "\u{2699}";

/// One of Robi's tips, on its way in, held, or on its way out.
pub(crate) struct RobiBubble {
    /// The tip (`aterm_effects::robi::ROBI_TIPS`, `&'static` — borrowed, never
    /// cloned).
    text: &'static str,
    /// Speech-bubble anchor in tray px — `(x_center, y_top_of_speaker)`. The host
    /// refreshes it each frame so the bubble follows him; `None` takes the
    /// top-centre rest position.
    anchor: Option<(f32, f32)>,
    /// When the tip was posted: the origin of every ramp.
    spawned: Instant,
}

impl RobiBubble {
    /// A tip posted at `now`, anchored over the speaker when his rect is known.
    pub(crate) fn new(text: &'static str, anchor: Option<(f32, f32)>, now: Instant) -> Self {
        Self {
            text,
            anchor,
            spawned: now,
        }
    }

    /// Refresh the speech-bubble anchor (the speaker moves; the bubble follows).
    pub(crate) fn set_anchor(&mut self, anchor: Option<(f32, f32)>) {
        self.anchor = anchor;
    }

    /// Fully gone (past its whole lifetime) — the caller drops it.
    pub(crate) fn is_expired(&self, now: Instant) -> bool {
        now.duration_since(self.spawned) >= TTL
    }

    /// The next wake time: animate every [`FRAME`] while the bubble is moving (the
    /// entrance ramp and the exit tail), otherwise just wake at the exit boundary — a
    /// steady hold needs no intermediate repaints (FL-1: a settled window with a
    /// bubble up wakes once, at the fade).
    pub(crate) fn deadline(&self, now: Instant) -> Instant {
        let elapsed = now.duration_since(self.spawned);
        let fade_start = TTL - FADE;
        if elapsed < ENTER || elapsed >= fade_start {
            now + FRAME
        } else {
            self.spawned + fade_start
        }
    }

    /// The whole-bubble alpha at `now`: an eased ramp 0→1 across the entrance, `1.0`
    /// through the hold, then an eased ramp 1→0 across the exit tail.
    pub(crate) fn alpha(&self, now: Instant) -> f32 {
        let elapsed = now.duration_since(self.spawned).as_secs_f32();
        let (ttl, enter, fade) = (TTL.as_secs_f32(), ENTER.as_secs_f32(), FADE.as_secs_f32());
        let fade_start = ttl - fade;
        if elapsed <= 0.0 {
            0.0
        } else if elapsed < enter {
            ease_out_cubic(elapsed / enter)
        } else if elapsed <= fade_start {
            1.0
        } else {
            (1.0 - ease_in_out_cubic((elapsed - fade_start) / fade)).clamp(0.0, 1.0)
        }
    }

    /// The bubble's vertical displacement from its rest position, as a fraction of
    /// [`SLIDE_CELLS`] — NEGATIVE is above the rest position. It drops in from above
    /// and lifts back out the same way, so the whole life reads as one gesture rather
    /// than a thing that blinks on and dissolves. `0` throughout the hold.
    pub(crate) fn rise(&self, now: Instant) -> f32 {
        let elapsed = now.duration_since(self.spawned).as_secs_f32();
        let (ttl, enter, fade) = (TTL.as_secs_f32(), ENTER.as_secs_f32(), FADE.as_secs_f32());
        let fade_start = ttl - fade;
        if elapsed <= 0.0 {
            -1.0
        } else if elapsed < enter {
            -(1.0 - ease_out_cubic(elapsed / enter))
        } else if elapsed <= fade_start {
            0.0
        } else {
            -ease_in_out_cubic(((elapsed - fade_start) / fade).clamp(0.0, 1.0))
        }
    }

    /// A repaint fingerprint folded into `RepaintKey::bubble_fp`, quantized so the
    /// bubble re-presents on each animation step but NOT every idle frame during the
    /// hold. `0` is the no-bubble sentinel, so a live bubble is forced non-zero.
    pub(crate) fn fingerprint(&self, now: Instant) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.text.hash(&mut h);
        // Quantize BOTH animated quantities so a moving bubble re-rasterizes on each
        // step while a held one hashes stable. 48 steps over a ≤0.8 s ramp is finer
        // than the 60fps cadence can consume, so no step is ever quantized away.
        ((self.alpha(now) * 48.0) as u64).hash(&mut h);
        ((self.rise(now).abs() * 48.0) as u64).hash(&mut h);
        // The anchor moves with its speaker — quantized to 2px steps so a swinging
        // Robi re-rasters his bubble along the way.
        if let Some((ax, ay)) = self.anchor {
            (((ax * 0.5) as i64), ((ay * 0.5) as i64)).hash(&mut h);
        }
        h.finish() | 1
    }
}

/// `1-(1-t)³` — decelerating. The entrance: fast off the mark, settles softly.
fn ease_out_cubic(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    let inv = 1.0 - t;
    1.0 - inv * inv * inv
}

/// Symmetric cubic ease — the exit, where a linear ramp reads mechanical.
fn ease_in_out_cubic(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        let f = -2.0 * t + 2.0;
        1.0 - f * f * f / 2.0
    }
}

/// Rec. 709 relative luminance of a gamma-encoded colour, 0..255. Good enough to
/// decide "can these two be told apart", which is all this widget asks of it.
fn luma(c: [u8; 3]) -> f32 {
    0.2126 * f32::from(c[0]) + 0.7152 * f32::from(c[1]) + 0.0722 * f32::from(c[2])
}

/// Black or white, whichever stays legible ON `fill`. The badge pictogram sits on a
/// disc in the LIVE CURSOR COLOUR, whose luminance is not knowable ahead of time
/// (the user owns it), so the contrast is computed rather than assumed.
fn on_fill(fill: [u8; 3]) -> [u8; 3] {
    if luma(fill) > 140.0 {
        [0x10, 0x12, 0x16]
    } else {
        [0xFF, 0xFF, 0xFF]
    }
}

/// The minimum luminance gap (0..255) the badge must hold against the bubble surface.
const BADGE_CONTRAST: f32 = 52.0;

/// `fill` pushed away from `surface` until the two can be told apart.
///
/// The badge takes the LIVE CURSOR COLOUR, which the user owns and which has no
/// relationship to the bubble's elevated surface — a dark-grey cursor on a dark
/// bubble paints an invisible badge, and the tip silently becomes a floating
/// pictogram. Rather than discard the user's colour, it is walked toward white (on
/// a dark bubble) or black (on a light one) by the smallest step that clears
/// [`BADGE_CONTRAST`], so the hue survives and the disc is always seen.
fn legible_on(fill: [u8; 3], surface: [u8; 3]) -> [u8; 3] {
    let ls = luma(surface);
    if (luma(fill) - ls).abs() >= BADGE_CONTRAST {
        return fill;
    }
    let toward: [u8; 3] = if ls > 127.0 {
        [0x00, 0x00, 0x00]
    } else {
        [0xFF, 0xFF, 0xFF]
    };
    // Sixteen bounded steps: enough to resolve BADGE_CONTRAST from any starting pair,
    // and it terminates whether or not the target itself clears the gap (a mid-grey
    // surface against pure black is only ~127 apart, which it does).
    for step in 1u8..=16 {
        let t = f32::from(step) / 16.0;
        let mixed: [u8; 3] = std::array::from_fn(|i| {
            (f32::from(fill[i]) + (f32::from(toward[i]) - f32::from(fill[i])) * t).round() as u8
        });
        if (luma(mixed) - ls).abs() >= BADGE_CONTRAST {
            return mixed;
        }
    }
    toward
}

/// The resolved geometry + fitted text of one bubble. Painted by [`bubble_tray`] and
/// hit-tested by [`bubble_hit`] through the same [`layout`], so the pixels and the
/// click target are the SAME arithmetic by construction.
struct Bubble {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    radius: f32,
    size: StepPx,
    glyph_px: StepPx,
    badge_cx: f32,
    badge_cy: f32,
    badge_r: f32,
    /// The tip, elided to fit.
    tip: String,
    tip_x: f32,
    baseline: f32,
}

/// Lay one bubble out.
///
/// `motion` is the reduced-motion amplitude
/// (`MotionPolicy::amplitude(MotionEffect::NoticePill)`): at `0` the bubble holds its
/// rest position for its whole life and only the alpha ramps, which is exactly what
/// reduced-motion asks for — information kept, movement removed.
///
/// `clear_rows` is the number of IN-GRID chrome rows at the top of the terminal area
/// the bubble must sit below — the tab strip. A bubble pinned over the strip would
/// hide the tabs it floats over, and because the mouse path checks the bubble before
/// the strip, a click meant for a tab would dismiss the bubble instead.
fn layout(b: &RobiBubble, g: &SettingsGeom, now: Instant, motion: f32, clear_rows: f32) -> Bubble {
    let (cw, ch, px) = (g.cw, g.ch, g.font_px);
    let tray_w = g.cols as f32 * cw;
    // The tip at the Secondary step (0.9×) rather than the Caption (0.8×): this is a
    // sentence the user is meant to READ across a room-width window, not a chip label.
    let size = TypeStep::Secondary.px_clamped(px, 12.0, f32::INFINITY);
    let glyph_px = TypeStep::Caption.px_clamped(px, 10.0, f32::INFINITY);
    let s = size.get();

    let badge_r = s * 0.72;
    let pad_x = s * 0.80;
    let gap_badge = s * 0.52;

    let h = (2.0 * badge_r).max(s) + s * 1.10;
    let radius = (h * 0.5).min(16.0);
    // The bubble may not exceed the tray minus a cell of air on each side.
    let max_w = (tray_w - 2.0 * cw).max(0.0);
    let lead = pad_x + 2.0 * badge_r + gap_badge;
    let trail = pad_x;

    let tip_w = |t: &str| ui_text_width_for(TextFace::UiBold, t, s);

    // The tip is all title (no separator), so a narrow window elides it rather than
    // dropping the half that carries the advice.
    let mut tip = b.text.to_string();
    let mut w = lead + tip_w(&tip) + trail;
    if w > max_w {
        let room = max_w - lead - trail;
        tip = elided(&tip, room, s);
        w = lead + tip_w(&tip) + trail;
    }
    // A window too narrow for even the badge and its padding gets NO bubble rather
    // than a clamped sliver: the clamp would keep a zero-width rect while the badge
    // and tip still painted from their un-clamped anchors, i.e. a pictogram floating
    // on the terminal with no bubble under it.
    let w = if w > max_w { 0.0 } else { w.max(0.0) };

    // Placement: over the speaker's head when he is known, clamped inside the tray
    // and below the in-grid chrome; else the top-centre rest position.
    let min_y = clear_rows.max(0.0) * ch + 2.0;
    let (x, rest_y) = match b.anchor {
        Some((ax, ay)) if w > 0.0 => {
            let x = (ax - w * 0.5).clamp(cw * 0.5, (tray_w - w - cw * 0.5).max(0.0));
            let rest_y = (ay - h - ch * 0.35).max(min_y);
            (x, rest_y)
        }
        _ => (
            ((tray_w - w) * 0.5).max(0.0),
            (clear_rows.max(0.0) + REST_Y_CELLS) * ch,
        ),
    };
    // The slide never carries the bubble into the chrome it must clear: a speaker
    // high enough that his bubble rests against the strip gets a bubble that fades
    // in place there, rather than one that drops out of the tab strip (the
    // retired notice's anchored card did, for the length of its entrance).
    let y = (rest_y + b.rise(now) * SLIDE_CELLS * ch * motion.clamp(0.0, 1.0)).max(min_y);

    let badge_cx = x + pad_x + badge_r;
    let badge_cy = y + h * 0.5;
    let tip_x = x + lead;

    Bubble {
        x,
        y,
        w,
        h,
        radius,
        size,
        glyph_px,
        badge_cx,
        badge_cy,
        badge_r,
        tip,
        tip_x,
        baseline: row_baseline(y, h, s),
    }
}

/// Whether a press at `(px, py)` (tray px, the painter's coordinates) lands on the
/// bubble. The SAME `layout` the painter uses, so the target is the pixels.
pub(crate) fn bubble_hit(
    b: &RobiBubble,
    g: &SettingsGeom,
    now: Instant,
    motion: f32,
    clear_rows: f32,
    px: f32,
    py: f32,
) -> bool {
    let p = layout(b, g, now, motion, clear_rows);
    p.w > 0.0 && px >= p.x && px < p.x + p.w && py >= p.y && py < p.y + p.h
}

/// `text` shortened with a trailing ellipsis until its UI-face measure fits `max_w`.
/// Empty when not even the ellipsis fits — painting nothing is the only honest option
/// left, and the badge still shows.
fn elided(text: &str, max_w: f32, px: f32) -> String {
    if max_w <= 0.0 {
        return String::new();
    }
    if ui_text_width_for(TextFace::UiBold, text, px) <= max_w {
        return text.to_string();
    }
    let mut chars: Vec<char> = text.chars().collect();
    while !chars.is_empty() {
        chars.pop();
        let mut candidate: String = chars.iter().collect();
        candidate.push('\u{2026}');
        if ui_text_width_for(TextFace::UiBold, &candidate, px) <= max_w {
            return candidate;
        }
    }
    String::new()
}

/// The bubble rect `(x, y, w, h)` in tray px — where [`bubble_tray`] draws it and
/// what [`bubble_hit`] tests; the geometry tests pin this. `now`/`motion` are part of
/// the geometry because the bubble MOVES: the region tracks the pixels through the
/// slide.
#[cfg(test)]
pub(crate) fn bubble_rect(
    b: &RobiBubble,
    g: &SettingsGeom,
    now: Instant,
    motion: f32,
    clear_rows: f32,
) -> (f32, f32, f32, f32) {
    let p = layout(b, g, now, motion, clear_rows);
    (p.x, p.y, p.w, p.h)
}

/// Build the bubble as pure [`DrawPrim`]s, with the whole bubble's alpha scaled by
/// `b.alpha(now)` (the fade). `cursor` is the live cursor colour: the badge wears it
/// (legibility-conditioned) — friendly, and never mistakable for the band's
/// actionable accent.
pub(crate) fn bubble_tray(
    b: &RobiBubble,
    g: &SettingsGeom,
    theme: Theme,
    cursor: [u8; 3],
    now: Instant,
    motion: f32,
    clear_rows: f32,
) -> TrayInput {
    let r = Roles::from_theme(theme);
    let p = layout(b, g, now, motion, clear_rows);
    let a = b.alpha(now);
    let sa = |base: u8| -> u8 { (f32::from(base) * a) as u8 };
    let (x, y, w, h, radius) = (p.x, p.y, p.w, p.h, p.radius);

    let mut prims: Vec<DrawPrim> = Vec::new();
    if w <= 0.0 {
        // Too narrow to draw honestly — see `layout`. An empty tray, not a partial one.
        return TrayInput {
            prims,
            card: (x, y, 0.0, h),
        };
    }
    // A LAYERED shadow: three stacked rounded rects, each wider and fainter than the
    // last — stacking approximates a falloff, which is what a shadow looks like. Kept
    // inside SHADOW_MARGIN so the compositor's paint region always covers it.
    for (spread, dy, alpha) in SHADOW_LAYERS {
        prims.push(DrawPrim::Panel {
            x: x - spread,
            y: y - spread + dy,
            w: w + 2.0 * spread,
            h: h + 2.0 * spread,
            radius: radius + spread,
            fill: rgba([0, 0, 0], sa(alpha)),
            blur: false,
        });
    }
    // Body: an elevated surface, near-opaque so the tip never fights the terminal
    // text behind it.
    prims.push(DrawPrim::Panel {
        x,
        y,
        w,
        h,
        radius,
        fill: rgba(r.elevated, sa(0xFA)),
        blur: false,
    });
    // A HAIRLINE rim in the separator role — enough to seat the bubble against a
    // same-luminance background without a full-perimeter accent ring.
    prims.push(DrawPrim::Stroke {
        x,
        y,
        w,
        h,
        radius,
        width: 1.0,
        color: rgba(r.separator, sa(0x99)),
    });
    // The badge: a filled disc in the cursor colour, conditioned until it can be
    // seen, with the gear on it in whichever ink reads on that disc.
    let badge = legible_on(cursor, r.elevated);
    prims.push(DrawPrim::Dot {
        cx: p.badge_cx,
        cy: p.badge_cy,
        r: p.badge_r,
        color: rgba(badge, sa(0xFF)),
        breathe: false,
    });
    // The pictogram keeps the MONO face: it is glyph art, and the mono stack is the
    // one with the DejaVu coverage fallback behind it. The words take the native UI
    // face like every other piece of chrome.
    prims.push(text_prim(
        p.badge_cx - text_w(BADGE_GLYPH, p.glyph_px.get()) * 0.5,
        baseline_centered_at(p.badge_cy, p.glyph_px.get()),
        BADGE_GLYPH.to_string(),
        p.glyph_px,
        TextWeight::Regular,
        TextFace::Mono,
        rgba(on_fill(badge), sa(0xFF)),
    ));
    if !p.tip.is_empty() {
        prims.push(text_prim(
            p.tip_x,
            p.baseline,
            p.tip.clone(),
            p.size,
            TextWeight::Bold,
            TextFace::UiBold,
            rgba(r.text_primary, sa(0xFF)),
        ));
    }

    TrayInput {
        prims,
        card: (x, y, w, h),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TIP: &str = "Try Cmd-Shift-O to co-view a session in a second window";

    fn geom() -> SettingsGeom {
        SettingsGeom {
            cw: 9.0,
            ch: 20.0,
            font_px: 14.0,
            cols: 120,
            panel_rows: 40,
        }
    }

    fn t0() -> Instant {
        Instant::now()
    }

    /// Measure a painted run with the metric its OWN face uses — the mono pictogram
    /// and the proportional words do not share a width function.
    fn prim_w(p: &DrawPrim) -> f32 {
        match p {
            DrawPrim::Text { s, px, face, .. } => match face {
                TextFace::Mono => text_w(s, *px),
                other => ui_text_width_for(*other, s, *px),
            },
            _ => 0.0,
        }
    }

    fn texts(t: &TrayInput) -> Vec<(String, f32, f32)> {
        t.prims
            .iter()
            .filter_map(|p| match p {
                DrawPrim::Text { s, x, .. } => Some((s.clone(), *x, prim_w(p))),
                _ => None,
            })
            .collect()
    }

    /// The shadow must stay inside [`SHADOW_MARGIN`], because the compositor grows
    /// the bubble's paint region by exactly that and no more — anything beyond is
    /// CROPPED, silently, with the bubble still looking fine in isolation.
    #[test]
    fn shadow_stays_inside_its_margin() {
        // Each layer is drawn at (-spread, -spread + dy) with size (w + 2*spread,
        // h + 2*spread), so relative to the bubble it reaches `spread` left/right,
        // `spread - dy` above, and `spread + dy` below. Take the max over every
        // direction so the test keeps holding if a layer is ever given a negative dy.
        let reach = SHADOW_LAYERS
            .iter()
            .fold(0.0_f32, |worst, &(spread, dy, _)| {
                worst.max(spread).max(spread + dy).max(spread - dy)
            });
        assert!(
            reach <= SHADOW_MARGIN,
            "the shadow reaches {reach}px but the compositor only pads {SHADOW_MARGIN}px, \
             so the outermost layer is cropped: shrink the spread or raise SHADOW_MARGIN \
             (and splice_robi_bubble's paint region with it)"
        );
        // Non-vacuity: an empty table would satisfy the bound while proving nothing.
        assert!(
            !SHADOW_LAYERS.is_empty(),
            "no layers means nothing was checked"
        );
    }

    #[test]
    fn alpha_ramps_in_holds_then_ramps_out() {
        let now = t0();
        let b = RobiBubble::new(TIP, None, now);
        // The entrance is a real ramp, not a pop.
        assert_eq!(b.alpha(now), 0.0, "invisible at spawn");
        let opening = b.alpha(now + ENTER / 2);
        assert!(
            opening > 0.0 && opening < 1.0,
            "mid-entrance alpha {opening}"
        );
        assert_eq!(b.alpha(now + ENTER), 1.0, "fully in once the entrance ends");
        // Just before the exit tail: still full.
        assert_eq!(b.alpha(now + (TTL - FADE) - Duration::from_millis(1)), 1.0);
        // Deep in the exit: below full, above zero.
        let mid = b.alpha(now + TTL - FADE / 2);
        assert!(mid > 0.0 && mid < 1.0, "mid-fade alpha {mid}");
        // At/after TTL: gone.
        assert_eq!(b.alpha(now + TTL), 0.0);
        assert!(b.is_expired(now + TTL));
        assert!(!b.is_expired(now));
        // The click floor sits inside the fade: a bubble a person can plainly see
        // is a target, a bubble that has all but left is not.
        assert!(b.alpha(now + ENTER) >= CLICK_MIN_ALPHA);
        assert!(b.alpha(now + TTL - Duration::from_millis(60)) < CLICK_MIN_ALPHA);
    }

    /// The bubble must come from ABOVE and leave upward, and be exactly at rest for
    /// the whole hold — a tip that drifts while you read it is worse than one that
    /// never moved.
    #[test]
    fn rise_travels_in_settles_and_lifts_away() {
        let now = t0();
        let b = RobiBubble::new(TIP, None, now);
        assert_eq!(b.rise(now), -1.0, "starts a full slide above rest");
        assert!(b.rise(now + ENTER / 2) < 0.0);
        assert_eq!(b.rise(now + ENTER), 0.0, "settled when the entrance ends");
        assert_eq!(
            b.rise(now + TTL - FADE),
            0.0,
            "still at rest at the exit boundary"
        );
        assert!(b.rise(now + TTL - FADE / 2) < 0.0, "lifts away on exit");
        // The whole travel stays within one slide in each direction.
        for ms in (0..TTL.as_millis() as u64).step_by(17) {
            let r = b.rise(now + Duration::from_millis(ms));
            assert!((-1.0..=0.0).contains(&r), "rise {r} out of range at {ms}ms");
        }
    }

    /// Reduced motion keeps the information and removes the movement: the bubble is
    /// pinned to its rest position for its whole life, and only the alpha ramp
    /// remains.
    #[test]
    fn reduced_motion_pins_the_bubble_to_its_rest_position() {
        let now = t0();
        let b = RobiBubble::new(TIP, None, now);
        let g = geom();
        let rest = REST_Y_CELLS * g.ch;
        for ms in [0_u64, 100, 1_000, 5_000] {
            let (_, y, _, _) = bubble_rect(&b, &g, now + Duration::from_millis(ms), 0.0, 0.0);
            assert!(
                (y - rest).abs() < f32::EPSILON,
                "pinned at {ms}ms: {y} != {rest}"
            );
        }
        // …and with motion allowed it genuinely moves, so the test above is not
        // vacuous.
        let (_, moving, _, _) = bubble_rect(&b, &g, now, 1.0, 0.0);
        assert!(
            moving < rest,
            "the entrance really does displace the bubble"
        );
    }

    /// Never the no-bubble sentinel; stable through the hold (FL-1: a settled window
    /// with a bubble up re-presents nothing); moving across both ramps; and moving
    /// with the speaker, since the anchor is part of the picture.
    #[test]
    fn fingerprint_is_never_zero_holds_still_and_moves_with_the_ramps_and_the_speaker() {
        let now = t0();
        let mut b = RobiBubble::new(TIP, Some((100.0, 80.0)), now);
        assert_ne!(b.fingerprint(now), 0);
        let hold_a = b.fingerprint(now + ENTER + Duration::from_millis(50));
        let hold_b = b.fingerprint(now + ENTER + Duration::from_millis(250));
        assert_eq!(hold_a, hold_b, "stable during hold");
        let enter_a = b.fingerprint(now + Duration::from_millis(20));
        let enter_b = b.fingerprint(now + Duration::from_millis(120));
        assert_ne!(enter_a, enter_b, "changes across the entrance");
        let fade_a = b.fingerprint(now + TTL - FADE + Duration::from_millis(100));
        let fade_b = b.fingerprint(now + TTL - FADE + Duration::from_millis(500));
        assert_ne!(fade_a, fade_b, "changes across the exit");
        // A swinging Robi carries his bubble: a 2px step re-rasters, a sub-pixel
        // wobble does not.
        b.set_anchor(Some((100.5, 80.0)));
        assert_eq!(
            b.fingerprint(now + ENTER + Duration::from_millis(50)),
            hold_a,
            "a sub-step wobble is quantized away"
        );
        b.set_anchor(Some((104.0, 80.0)));
        assert_ne!(
            b.fingerprint(now + ENTER + Duration::from_millis(50)),
            hold_a,
            "the bubble follows its speaker"
        );
    }

    /// Every animation step the 60fps cadence can deliver must survive
    /// quantization, or the bubble would visibly step instead of glide.
    #[test]
    fn every_frame_of_the_ramps_is_a_distinct_fingerprint_step() {
        let now = t0();
        let b = RobiBubble::new(TIP, None, now);
        let sample = |ms: u64| b.fingerprint(now + Duration::from_millis(ms));
        let enter_steps = (0..ENTER.as_millis() as u64)
            .step_by(FRAME.as_millis() as usize)
            .map(sample)
            .collect::<std::collections::HashSet<_>>();
        assert!(
            enter_steps.len() >= 8,
            "the entrance quantizes to {} distinct frames",
            enter_steps.len()
        );
    }

    #[test]
    fn deadline_wakes_per_frame_while_moving_and_sleeps_through_the_hold() {
        let now = t0();
        let b = RobiBubble::new(TIP, None, now);
        assert_eq!(b.deadline(now), now + FRAME, "entrance animates");
        let held = now + ENTER + Duration::from_millis(100);
        assert_eq!(
            b.deadline(held),
            now + (TTL - FADE),
            "the hold sleeps to the exit boundary"
        );
        let leaving = now + TTL - FADE / 2;
        assert_eq!(b.deadline(leaving), leaving + FRAME, "the exit animates");
    }

    /// The bubble centres over the speaker and sits above his head, clamped
    /// inside the tray on both edges; without an anchor it takes the top-centre
    /// rest position rather than vanishing.
    #[test]
    fn the_bubble_sits_over_its_speaker_and_inside_the_tray() {
        let now = t0();
        let g = geom();
        let at = now + ENTER;
        let tray_w = g.cols as f32 * g.cw;
        let (ax, ay) = (400.0, 300.0);
        let b = RobiBubble::new(TIP, Some((ax, ay)), now);
        let (x, y, w, h) = bubble_rect(&b, &g, at, 1.0, 0.0);
        assert!(w > 0.0 && h > 0.0);
        assert!(
            ((x + w * 0.5) - ax).abs() < 0.5,
            "centred over the speaker: {x}+{w}/2 vs {ax}"
        );
        assert!(y + h < ay, "sits above his head: bottom {} vs {ay}", y + h);
        // Pushed against either edge, the bubble stays whole inside the tray.
        let left = RobiBubble::new(TIP, Some((0.0, ay)), now);
        let (lx, _, lw, _) = bubble_rect(&left, &g, at, 1.0, 0.0);
        assert!(lx >= 0.0 && lx + lw <= tray_w);
        let right = RobiBubble::new(TIP, Some((tray_w, ay)), now);
        let (rx, _, rw, _) = bubble_rect(&right, &g, at, 1.0, 0.0);
        assert!(rx >= 0.0 && rx + rw <= tray_w + 0.5);
        // A speaker at the very top cannot push the bubble off the glass.
        let high = RobiBubble::new(TIP, Some((ax, 0.0)), now);
        let (_, hy, _, _) = bubble_rect(&high, &g, at, 1.0, 0.0);
        assert!(hy >= 0.0);
        // No anchor: the rest position, centred.
        let free = RobiBubble::new(TIP, None, now);
        let (fx, fy, fw, _) = bubble_rect(&free, &g, at, 1.0, 0.0);
        assert!(((fx + fw * 0.5) - tray_w * 0.5).abs() < 0.5);
        assert!((fy - REST_Y_CELLS * g.ch).abs() < f32::EPSILON);
    }

    /// The badge and the tip are painted as their own runs, inside the bubble; the
    /// gear rides the badge, never the sentence.
    #[test]
    fn tray_paints_the_badge_and_the_tip_inside_the_bubble() {
        let now = t0();
        let b = RobiBubble::new(TIP, Some((500.0, 300.0)), now);
        let g = geom();
        let at = now + ENTER; // settled, so the rect is the rest rect
        let t = bubble_tray(&b, &g, Theme::default(), [0, 255, 0], at, 1.0, 0.0);
        let painted = texts(&t);
        assert!(
            painted.iter().any(|(s, _, _)| s == TIP),
            "the tip is painted whole as its own run: {painted:?}"
        );
        assert!(
            painted.iter().any(|(s, _, _)| s == BADGE_GLYPH),
            "the gear is painted as the badge pictogram, not as part of the sentence"
        );
        assert_eq!(painted.len(), 2, "two runs and nothing else: {painted:?}");
        let (x, _, w, _) = bubble_rect(&b, &g, at, 1.0, 0.0);
        let tray_w = g.cols as f32 * g.cw;
        assert!(
            x >= 0.0 && x + w <= tray_w,
            "the bubble fits within the tray"
        );
        for (s, tx, tw) in &painted {
            assert!(*tx >= x, "{s:?} starts inside the bubble");
            assert!(*tx + *tw <= x + w + 0.5, "{s:?} ends inside the bubble");
        }
        // The badge is a filled disc; nothing here advertises a press.
        assert_eq!(
            t.prims
                .iter()
                .filter(|p| matches!(p, DrawPrim::Dot { .. }))
                .count(),
            1
        );
        assert!(
            !t.prims.iter().any(|p| matches!(p, DrawPrim::Line { .. })),
            "a decoration must not advertise a click"
        );
    }

    /// The bubble must never paint outside the tray, at ANY width — including widths
    /// so small that nothing fits at all — and a long tip is elided, not overflowed.
    #[test]
    fn the_bubble_stays_inside_even_absurdly_narrow_trays() {
        let now = t0();
        let b = RobiBubble::new(TIP, Some((60.0, 200.0)), now);
        let mut saw_a_bubble = false;
        let mut saw_an_elision = false;
        for cols in [1_usize, 2, 3, 5, 8, 13, 21, 40, 200] {
            let g = SettingsGeom { cols, ..geom() };
            let at = now + ENTER;
            let (x, y, w, h) = bubble_rect(&b, &g, at, 1.0, 0.0);
            let tray_w = cols as f32 * g.cw;
            assert!(x >= 0.0 && w >= 0.0 && h > 0.0, "sane rect at {cols} cols");
            assert!(
                x + w <= tray_w + 0.5,
                "fits at {cols} cols: {x}+{w} > {tray_w}"
            );
            assert!(y > 0.0, "the bubble never rides off the top at {cols} cols");
            let t = bubble_tray(&b, &g, Theme::default(), [0, 255, 0], at, 1.0, 0.0);
            saw_a_bubble |= !t.prims.is_empty();
            // Below the width that fits a badge the widget paints NOTHING — never a
            // tip hanging off a clamped zero-width bubble.
            if w <= 0.0 {
                assert!(
                    t.prims.is_empty(),
                    "a bubble too narrow to draw paints nothing"
                );
            }
            for (s, tx, tw) in texts(&t) {
                saw_an_elision |= s.ends_with('\u{2026}');
                assert!(
                    tx + tw <= x + w + 0.5,
                    "{s:?} overflows the bubble at {cols} cols"
                );
            }
        }
        assert!(
            saw_a_bubble,
            "the wide end still paints — the loop is not vacuous"
        );
        assert!(
            saw_an_elision,
            "a narrow tray elides the tip rather than dropping it"
        );
    }

    /// THE BUBBLE MUST NOT FLOAT OVER IN-GRID CHROME. The tab strip lives in the first
    /// `tab_strip_rows` grid rows, and the mouse path tests the bubble BEFORE the
    /// strip: a bubble overlapping the strip both hides the tabs and eats clicks
    /// meant for them.
    #[test]
    fn the_bubble_clears_the_in_grid_chrome_it_is_given() {
        let now = t0();
        let g = geom();
        let at = now + ENTER;
        for anchor in [None, Some((300.0, 10.0))] {
            let b = RobiBubble::new(TIP, anchor, now);
            for clear_rows in [0.0_f32, 1.0, 2.0, 3.0] {
                let (_, y, _, h) = bubble_rect(&b, &g, at, 1.0, clear_rows);
                assert!(
                    y >= clear_rows * g.ch,
                    "bubble top {y} rides into the {clear_rows} reserved chrome rows ({anchor:?})"
                );
                // Even mid-entrance, when the bubble is at its highest, it stays clear.
                let (_, y_in, _, _) = bubble_rect(&b, &g, now, 1.0, clear_rows);
                assert!(
                    y_in >= clear_rows * g.ch,
                    "the entrance overshoots into chrome at {clear_rows} rows: {y_in}"
                );
                assert!(h > 0.0);
            }
        }
    }

    /// A press lands on the bubble exactly where it is painted — the same layout —
    /// and nowhere else; a bubble too narrow to paint is never a target.
    #[test]
    fn a_press_lands_on_the_bubble_only_where_it_is_painted() {
        let now = t0();
        let g = geom();
        let at = now + ENTER;
        let b = RobiBubble::new(TIP, Some((500.0, 300.0)), now);
        let (x, y, w, h) = bubble_rect(&b, &g, at, 1.0, 0.0);
        assert!(bubble_hit(&b, &g, at, 1.0, 0.0, x + w * 0.5, y + h * 0.5));
        assert!(bubble_hit(&b, &g, at, 1.0, 0.0, x, y));
        assert!(!bubble_hit(&b, &g, at, 1.0, 0.0, x - 1.0, y + h * 0.5));
        assert!(!bubble_hit(&b, &g, at, 1.0, 0.0, x + w, y + h * 0.5));
        assert!(!bubble_hit(&b, &g, at, 1.0, 0.0, x + w * 0.5, y + h));
        // The target tracks the slide: mid-entrance the rect is higher.
        let (_, y_in, _, _) = bubble_rect(&b, &g, now, 1.0, 0.0);
        assert!(y_in < y);
        assert!(bubble_hit(&b, &g, now, 1.0, 0.0, x + w * 0.5, y_in + 1.0));
        let narrow = SettingsGeom { cols: 1, ..geom() };
        assert!(!bubble_hit(&b, &narrow, at, 1.0, 0.0, 0.0, 0.0));
    }

    /// The badge takes the LIVE CURSOR COLOUR, which the user owns — including
    /// colours that match the bubble surface. The disc must still be visible.
    #[test]
    fn a_badge_is_conditioned_until_it_can_be_seen_on_the_bubble() {
        let surface = [0x1E, 0x20, 0x24];
        // A cursor colour that all but matches the surface is pushed until it
        // separates.
        let near = [0x22, 0x24, 0x28];
        let fixed = legible_on(near, surface);
        assert!(
            (luma(fixed) - luma(surface)).abs() >= BADGE_CONTRAST,
            "{fixed:?} still blends into {surface:?}"
        );
        // A colour that already separates is returned UNTOUCHED — the user's hue
        // survives.
        let vivid = [0x3B, 0xC8, 0xFF];
        assert_eq!(legible_on(vivid, surface), vivid);
        // Both surface polarities terminate and satisfy the gap.
        for surface in [[0xF7, 0xF7, 0xF8], [0x00, 0x00, 0x00], [0x80, 0x80, 0x80]] {
            for fill in [[0x80, 0x80, 0x80], [0x7F, 0x81, 0x7E], surface] {
                let out = legible_on(fill, surface);
                assert!(
                    (luma(out) - luma(surface)).abs() >= BADGE_CONTRAST - 1.0,
                    "{fill:?} on {surface:?} resolved to {out:?}"
                );
            }
        }
    }

    /// The pictogram sits on a disc whose luminance is not knowable ahead of time,
    /// so its colour is COMPUTED. Both branches must be reachable and legible.
    #[test]
    fn badge_glyph_contrasts_with_every_badge_fill() {
        assert_eq!(on_fill([0xFF, 0xFF, 0xFF]), [0x10, 0x12, 0x16]);
        assert_eq!(on_fill([0x10, 0x10, 0x10]), [0xFF, 0xFF, 0xFF]);
        // A mid colour still resolves to the higher-contrast side of the pair.
        for fill in [[0x2F, 0x6F, 0xED], [0xF5, 0xC2, 0x42], [0x00, 0xC8, 0x77]] {
            let on = on_fill(fill);
            assert!(
                (luma(on) - luma(fill)).abs() > 60.0,
                "{fill:?} on {on:?} is too close to read"
            );
        }
    }

    #[test]
    fn elision_is_a_no_op_when_the_text_already_fits() {
        assert_eq!(elided("Ready", 10_000.0, 12.0), "Ready");
        // Degenerate widths never panic and never return junk.
        assert_eq!(elided("Ready", 0.0, 12.0), "");
        assert_eq!(elided("Ready", -5.0, 12.0), "");
    }
}
