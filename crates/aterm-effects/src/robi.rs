// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! ROBI, the resident helper robot — ported from the user's Nitro Keyboard
//! game, where he lives on the menu screens, hops around the furniture, and
//! hands out tips.
//!
//! In aterm he is a PERMANENT RESIDENT: once born (config on, motion allowed)
//! he is on the glass at all times, cycling forever through his repertoire —
//! walk along the prompt line (the row the caret is typing on — his
//! "textbox"), jumping jacks with a starter tip, extend a ladder up past the
//! top of the grid, climb it, swing across the tab bar like MONKEY BARS with
//! a deeper tip, drop, and rest. Two tips per cycle, drawn deterministically
//! from a wide bank (aterm features, shell tricks, Claude Code tricks), so
//! every cycle keeps teaching new things. Typing his name (`robi` / `robot`)
//! restarts the cycle on the spot — he hustles over and greets you.
//!
//! ## Clockless (the [`crate::dog_cameo`] discipline)
//!
//! Everything is a PURE FUNCTION of `(birth instant, seed, now, geometry)`:
//! no timers, no randomness at tick time. The host re-requests presents only
//! while the evaluated frame says [`RobiFrame::animating`] — his idle stands
//! are deliberately STATIC (a statuesque robot is in character), so a resting
//! Robi costs zero wakes and simply resumes his rounds on the next natural
//! redraw. Reduced motion and serious mode are host gates: under either he is
//! not on the glass at all (bypass-to-final-state, the matrix-rain rule).
//!
//! ## Geometry contract
//!
//! All positions are PRE-SPLICE GRID pixels — the same frame
//! [`FreeSprite`](aterm_core::render::FreeSprite) speaks. Negative `y` is
//! above row 0: the strip-splice shifts sprites down by the tab-strip height,
//! so `y ∈ [-strip_px, 0)` lands ON the in-grid tab bar, and anything higher
//! reaches the titlebar band (the head allowance). The host resolves where
//! the monkey-bar hand line actually is ([`RobiSense::bar_y`]) and where the
//! handholds sit ([`RobiSense::handholds`]) — tab-chip centers when a strip
//! exists, an even rhythm otherwise — so the brain never needs to know which
//! tab flavour is on screen. The host also hands over the window's vertical
//! extent ([`RobiSense::win_top`] / [`RobiSense::win_bot`]): every evaluated
//! scene — body and ladder both — is clamped fully inside it (owner
//! directive: he is never CLIPPED by a window edge), and a window too small
//! to hold his body at all HIDES him ([`RobiShow::frame`] → `None`) rather
//! than clipping him.
//!
//! ## Caret avoidance
//!
//! The caret's ROW is his floor; its COLUMN is the user's workspace. Every
//! stationary ground scene — the jumping jacks, both idle stands, the long
//! rest — keeps his body center at least [`caret_clearance`] away from the
//! caret column's center: the per-cycle stage marks are pushed to the nearer
//! edge of that exclusion zone (the far edge when the grid margin leaves the
//! nearer side no room; unchanged when the grid is too narrow for either),
//! and since the walks TARGET those marks, he also aims away with no
//! special-casing. Still a pure function of `(birth, seed, now, sense)`: the
//! zone is re-derived from the live caret on every evaluation, so a caret
//! arriving under a resting Robi displaces him on the next natural redraw —
//! typing is what repaints, so no wake is armed and the zero-cost-idle
//! contract holds. Exempt on purpose: mid-walk positions (strolling PAST the
//! typist is fine; loitering on the word being typed is not) and the whole
//! ladder/climb/bars arc — the ladder must stay planted on `holds[0]`, and
//! up there he is above the text anyway.

use aterm_time::Instant;

use crate::genome;
use crate::robi_glyphs_gen::RobiGlyphId;
use crate::word_decorations::EffectGeom;

/// Robi's on-screen height in rows in a SMALL window — and the floor of the
/// window-scaled height ([`art_rows`]). Taller than the shared atlas's 2-row
/// host-tile ceiling on purpose: the emitter bakes him at full size and
/// splits the texels into vertically stacked slices, each under the ceiling.
pub const ART_ROWS: f32 = 3.2;

/// Ceiling of the window-scaled height ([`art_rows`]) — what the shared atlas
/// can actually hold him at. Two structural caps bound his box, and the WIDTH
/// one binds first: a tile may be at most `4·cell_h` wide (`CatBaker::slot_w`,
/// the §5.2 cap made structural) and at most `2·cell_h` tall per slice, of
/// which the emitter stacks four. `5.4 · ART_ASPECT ≈ 3.93` cell-heights wide
/// clears the width cap with rounding room, and 5.4 rows is under the 8-row
/// height ceiling. Overshooting is NOT a squashed robot but an INVISIBLE one:
/// `host_tile` refuses an oversized tile, the body never resolves, and he
/// silently keeps whatever size he was last baked at.
pub const ART_ROWS_MAX: f32 = 5.4;

/// The share of the window's HEIGHT Robi fills. A helper who reads at 3.2
/// rows in a 32-row window is a speck in a 70-row fullscreen one — rows are
/// not a size, they are a count. Sizing him as a FRACTION of the viewport
/// keeps him the same apparent size on any window: [`ART_ROWS`] ÷ 32 rows.
const ART_ROWS_PER_ROW: f32 = ART_ROWS / 32.0;

/// Robi's on-screen height in rows for a given viewport — [`ART_ROWS`] in a
/// 32-row window, growing with the window up to [`ART_ROWS_MAX`].
///
/// THE one sizing law: the brain ([`RobiShow::frame`]), the emitter
/// (`WordDecorations::robi`) and the host's speech-bubble anchor all derive
/// his box from this, so his body, his ladder, his footing and his tip can
/// never disagree about how big he is.
#[must_use]
pub fn art_rows(geom: &EffectGeom) -> f32 {
    (f32::from(geom.rows) * ART_ROWS_PER_ROW).clamp(ART_ROWS, ART_ROWS_MAX)
}

/// Authored pose viewbox aspect (width ÷ height) — `robi.py`'s 128 × 176.
pub const ART_ASPECT: f32 = 128.0 / 176.0;

/// The caret-avoidance half-width in grid px: how far Robi's body CENTER
/// must stay from the caret column's center while he loiters on the ground
/// (see the module doc's "Caret avoidance"). Half his body width plus two
/// cells — at that distance his nearest edge clears a full two columns of
/// whatever is being typed, close enough to keep the user company, far
/// enough that he never squats on the word under the cursor. Shared with the
/// tests so the law is pinned in one place, like [`art_rows`].
#[must_use]
pub fn caret_clearance(geom: &EffectGeom) -> i32 {
    let body_w = (art_rows(geom) * ART_ASPECT * f32::from(geom.cell_h)).round() as i32;
    body_w / 2 + 2 * i32::from(geom.cell_w).max(1)
}

/// Fraction of the pose viewbox height where standing feet rest
/// (`GROUND / VB_H` in `robi.py`).
pub const FEET_FRAC: f32 = 170.0 / 176.0;

/// Fraction of the pose viewbox height where a hanging hand grips
/// (`GRIP_Y / VB_H` in `robi.py`).
pub const GRIP_FRAC: f32 = 10.0 / 176.0;

/// Robi's drawn-body SIZE in px for a viewport — `(w, h)`: `h` is the
/// rounded [`art_rows`] height, `w` follows [`ART_ASPECT`] under the shared
/// atlas's `4·cell_h` slot cap (`CatBaker::slot_w`). The cap is the ATLAS
/// CONTRACT, not a taste: a wider tile is REFUSED by `host_tile` and the
/// body silently stops resolving — [`ART_ROWS_MAX`] is chosen to stay
/// inside it, so the clamp only ever catches rounding. ONE copy of the law,
/// the pet's `body_px` discipline: the emitter (`WordDecorations::robi`)
/// bakes with it and the host's dismiss hit-box derives from it, so the
/// click box IS the sprite, not a model of it.
#[must_use]
pub fn body_size_px(geom: &EffectGeom) -> (u16, u16) {
    let h = ((art_rows(geom) * f32::from(geom.cell_h)).round() as i32)
        .clamp(1, i32::from(u16::MAX)) as u16;
    let slot = (i32::from(geom.cell_h) * 4).max(1);
    let w = ((f32::from(h) * ART_ASPECT).round() as i32)
        .clamp(1, slot)
        .min(i32::from(u16::MAX)) as u16;
    (w, h)
}

/// Place a `w × h` body at an evaluated frame's anchor: the dest rect
/// `(x0, x1, y0, y1)` (right/bottom exclusive, grid px) — top from the
/// anchor fraction ([`FEET_FRAC`] / [`GRIP_FRAC`]), `frame.x` a CENTER.
/// Split from [`body_px`] because the emitter must place the RESOLVED tile
/// with the same law — on a bake miss that is `robi_last_body` at the
/// PREVIOUS size, the one accepted transient.
#[must_use]
pub fn body_rect_px(frame: &RobiFrame, w: u16, h: u16) -> (i32, i32, i32, i32) {
    let anchor_frac = match frame.anchor {
        RobiAnchor::Feet => FEET_FRAC,
        RobiAnchor::Grip => GRIP_FRAC,
    };
    let top = frame.anchor_y - (f32::from(h) * anchor_frac).round() as i32;
    let x0 = frame.x - i32::from(w) / 2;
    (x0, x0 + i32::from(w), top, top + i32::from(h))
}

/// Robi's LIVE drawn body for one evaluated frame, in grid px —
/// [`body_size_px`] placed by [`body_rect_px`]: exactly the dest rect the
/// emitter draws when the bake resolves at the current size. `None` when
/// nothing is on glass (`alpha == 0`) or the cell metrics are degenerate —
/// the same frames the emitter draws nothing for, which is what clears a
/// host's stashed hit-box.
#[must_use]
pub fn body_px(frame: &RobiFrame, geom: &EffectGeom) -> Option<(i32, i32, i32, i32)> {
    if frame.alpha == 0 || geom.cell_w == 0 || geom.cell_h == 0 {
        return None;
    }
    let (w, h) = body_size_px(geom);
    Some(body_rect_px(frame, w, h))
}

/// Ladder tile aspect (width ÷ height) — `robi.py`'s 48 × 32 segment.
pub const LADDER_ASPECT: f32 = 48.0 / 32.0;

/// Ladder width in cell-height fractions — scaled to read right beside his
/// 3.2-row body (the NK original's 34px ladder against his 90px frame).
pub const LADDER_W_ROWS: f32 = 1.05;

/// The ladder tile's baked size `(w, h)` in px for a viewport — ONE copy of
/// the segment law, like [`body_size_px`]: the emitter
/// (`WordDecorations::robi`) bakes with it, and the window clamp
/// ([`RobiShow::frame`]) reads the same numbers because the emitter's
/// segment stack may overshoot `ladder.top_y` by up to a quarter tile (its
/// reach-the-bar slack) — the clamp must hold the STACK inside the window,
/// not just the abstract extent.
#[must_use]
pub fn ladder_tile_px(geom: &EffectGeom) -> (u16, u16) {
    // The authored proportion against his body, so the ladder grows with him
    // instead of thinning into a wire beside a big robot.
    let w_rows = LADDER_W_ROWS * art_rows(geom) / ART_ROWS;
    let w = (w_rows * f32::from(geom.cell_h)).round().max(2.0) as u16;
    let h = ((f32::from(w) / LADDER_ASPECT).round()).max(2.0) as u16;
    (w, h)
}

/// The most handholds a cycle will traverse (and the most the host need
/// resolve). Fixed-size so [`RobiSense`] stays `Copy`.
pub const MAX_HANDHOLDS: usize = 8;

// ── one cycle of his rounds (ms within the cycle) ──────────────────────────
// The cycle opens with the greeting (walk + jacks + tip) so a name-summon —
// which restarts the cycle — answers within a few seconds, exactly like
// clicking NK Robi. Idle stands are static (no wakes; see the module doc).
pub const FADE_IN_MS: u64 = 200;
pub const WALK_A_END: u64 = 2_500;
pub const JACKS_END: u64 = 10_500;
pub const IDLE_1_END: u64 = 17_000;
pub const WALK_B_END: u64 = 19_500;
pub const LADDER_EXTEND_END: u64 = 20_200;
pub const CLIMB_END: u64 = 24_200;
pub const BARS_END: u64 = 38_000;
pub const DROP_END: u64 = 38_600;
pub const IDLE_2_END: u64 = 46_000;
pub const WANDER_END: u64 = 49_000;
pub const CYCLE_MS: u64 = 76_000;

/// When (within a cycle) the ladder starts retracting — he is safely on the
/// bars — and how long the retract takes.
const LADDER_RETRACT_START: u64 = 25_400;
const LADDER_RETRACT_MS: u64 = 600;

/// Tip windows within a cycle: the starter tip rides the jumping jacks; the
/// deeper tip rides the monkey bars. Both are the NK bubble's ~5 s hold.
const TIP_A_START: u64 = 3_000;
const TIP_A_END: u64 = 8_200;
const TIP_B_START: u64 = 25_000;
const TIP_B_END: u64 = 30_200;

/// Walk gait: one walk-frame flip per this many ms.
const WALK_FRAME_MS: u64 = 220;
/// Jumping-jack half-cycle (NK: 0.44 s full cycle).
const JACK_FRAME_MS: u64 = 220;
/// Ladder climb half-cycle (NK: 0.5 s).
const CLIMB_FRAME_MS: u64 = 250;
/// One handhold-to-handhold swing on the monkey bars.
const SWING_MS: u64 = 960;

/// What a tip is about — used to pick a STARTER tip for the jacks scene and a
/// non-starter tip for the bars scene.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TipKind {
    /// Getting-started aterm guidance (shown during the jumping jacks).
    Starter,
    /// Deeper aterm features.
    Aterm,
    /// General shell / terminal tricks.
    Terminal,
    /// Claude Code tricks.
    Claude,
}

/// One bubble's worth of Robi wisdom.
#[derive(Clone, Copy, Debug)]
pub struct RobiTip {
    pub kind: TipKind,
    pub text: &'static str,
}

/// The tip bank. Robi's LAW (carried over from Nitro Keyboard): tips are
/// on-topic, straightforward, and correct — every aterm claim here names a
/// real feature of this build, and every command is the real spelling.
pub const ROBI_TIPS: &[RobiTip] = &[
    // ── starters ──────────────────────────────────────────────────────────
    RobiTip {
        kind: TipKind::Starter,
        text: "Welcome to aterm! Press Cmd+, any time — Settings has a tour.",
    },
    RobiTip {
        kind: TipKind::Starter,
        text: "Pick a cursor trail in Settings > Cursor: phaser, fire, water, comet, beam...",
    },
    RobiTip {
        kind: TipKind::Starter,
        text: "Cmd+F opens find. Esc closes it again.",
    },
    RobiTip {
        kind: TipKind::Starter,
        text: "Try typing the word kitty. Go on, I'll wait.",
    },
    RobiTip {
        kind: TipKind::Starter,
        text: "Type animal names — lots of species pop their heads up to say hi.",
    },
    RobiTip {
        kind: TipKind::Starter,
        text: "You can put a wallpaper behind your terminal: Settings > Wallpaper.",
    },
    RobiTip {
        kind: TipKind::Starter,
        text: "Keep typing! After enough keys, typing dog summons a real puppy.",
    },
    // ── deeper aterm ──────────────────────────────────────────────────────
    RobiTip {
        kind: TipKind::Aterm,
        text: "Every cursor trail has its own sound. Settings > Cursor toggles them.",
    },
    RobiTip {
        kind: TipKind::Aterm,
        text: "Feeling blocky? Set game_font = \"minecraft\" in aterm.toml.",
    },
    RobiTip {
        kind: TipKind::Aterm,
        text: "wallpaper_dim in aterm.toml sets how much the wallpaper fades back.",
    },
    RobiTip {
        kind: TipKind::Aterm,
        text: "The Settings search box finds any knob — just start typing its name.",
    },
    RobiTip {
        kind: TipKind::Aterm,
        text: "aterm updates itself quietly — the Version menu shows what's staged.",
    },
    RobiTip {
        kind: TipKind::Aterm,
        text: "Type dog in different languages. Chien! Perro! Hund! They all count.",
    },
    // ── terminal tricks ───────────────────────────────────────────────────
    RobiTip {
        kind: TipKind::Terminal,
        text: "Ctrl+R searches your shell history as you type.",
    },
    RobiTip {
        kind: TipKind::Terminal,
        text: "cd - jumps straight back to the previous directory.",
    },
    RobiTip {
        kind: TipKind::Terminal,
        text: "!! reruns your last command. sudo !! saves the day.",
    },
    RobiTip {
        kind: TipKind::Terminal,
        text: "Ctrl+A jumps to the start of the line, Ctrl+E to the end.",
    },
    RobiTip {
        kind: TipKind::Terminal,
        text: "Ctrl+U wipes the whole line. Ctrl+W eats one word.",
    },
    RobiTip {
        kind: TipKind::Terminal,
        text: "Pipe anything into pbcopy to send it to your clipboard.",
    },
    RobiTip {
        kind: TipKind::Terminal,
        text: "open . opens the current folder in Finder.",
    },
    RobiTip {
        kind: TipKind::Terminal,
        text: "Ctrl+L clears the screen without losing scrollback.",
    },
    RobiTip {
        kind: TipKind::Terminal,
        text: "mkdir -p deep/nested/path makes every folder in one go.",
    },
    // ── Claude Code tricks ────────────────────────────────────────────────
    RobiTip {
        kind: TipKind::Claude,
        text: "Tired of permission prompts? claude --dangerously-skip-permissions skips them all.",
    },
    RobiTip {
        kind: TipKind::Claude,
        text: "claude --resume picks your last conversation right back up.",
    },
    RobiTip {
        kind: TipKind::Claude,
        text: "claude -p \"question\" answers one-shot, no session needed.",
    },
    RobiTip {
        kind: TipKind::Claude,
        text: "In Claude Code, start a line with ! to run a shell command yourself.",
    },
    RobiTip {
        kind: TipKind::Claude,
        text: "Ask Claude Code to use a worktree to keep experiments off your branch.",
    },
];

/// How a pose anchors to [`RobiFrame::anchor_y`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RobiAnchor {
    /// `anchor_y` is the ground line the feet stand on.
    Feet,
    /// `anchor_y` is the bar line the hands grip.
    Grip,
}

/// The ladder's current extent, body-center at `x`, spanning `top_y..bot_y`
/// in grid px (`top_y` is usually negative — it pokes above the grid).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RobiLadder {
    pub x: i32,
    pub top_y: i32,
    pub bot_y: i32,
}

/// One evaluated frame of the resident — everything the emitter needs.
#[derive(Clone, Copy, Debug)]
pub struct RobiFrame {
    pub pose: RobiGlyphId,
    /// Body-center x in grid px.
    pub x: i32,
    /// Feet line or grip line, per `anchor` (grid px; negative = above row 0).
    pub anchor_y: i32,
    pub anchor: RobiAnchor,
    pub flip_x: bool,
    pub alpha: u8,
    pub ladder: Option<RobiLadder>,
    /// Index into [`ROBI_TIPS`] while a bubble should be up.
    pub tip: Option<u16>,
    /// Whether this scene is in motion. `false` = a static idle stand: the
    /// host must NOT arm another wake for it (he resumes on the next natural
    /// redraw — the zero-cost-idle contract).
    pub animating: bool,
}

/// The host's geometry snapshot for one evaluation.
#[derive(Clone, Copy, Debug)]
pub struct RobiSense {
    pub geom: EffectGeom,
    /// The caret cell — Robi walks the bottom edge of this row.
    pub cursor: (u16, u16),
    /// The monkey-bar hand line in grid px (negative: above row 0). The host
    /// aims this at the tab chips.
    pub bar_y: i32,
    /// Handhold x-centers in grid px, left→right, `handhold_count` valid.
    pub handholds: [i32; MAX_HANDHOLDS],
    pub handhold_count: u8,
    /// The window's TOP edge in grid px (≤ 0 — how far above row 0 the glass
    /// really extends: `-(pad_top + head + strip_px)`). The brain clamps his
    /// body and ladder into `win_top..win_bot` so no scene is ever clipped
    /// by a window edge; a window too small to hold his body hides him
    /// instead ([`RobiShow::frame`] → `None`).
    pub win_top: i32,
    /// The window's BOTTOM edge in grid px (≥ `rows·cell_h` — the bottom
    /// padding sits below the grid). The other half of the `win_top` clamp.
    pub win_bot: i32,
}

/// The resident's whole state: a birth instant plus the roll (or [`Default`]:
/// not yet born). Everything else derives per frame.
#[derive(Clone, Copy, Debug, Default)]
pub struct RobiShow {
    since: Option<Instant>,
    seed: u64,
}

impl RobiShow {
    /// Be born (or, on a name-summon, restart the cycle from the greeting) at
    /// `now`. `seed` folds the per-window counter so tips rotate.
    pub fn start(&mut self, now: Instant, seed: u64) {
        self.since = Some(now);
        self.seed = genome::mix(seed);
    }

    /// Leave the glass (config-off / reduced-motion / serious hygiene).
    pub fn stop(&mut self) {
        self.since = None;
    }

    /// Whether Robi has been born. A born resident never expires.
    #[must_use]
    pub fn born(&self) -> bool {
        self.since.is_some()
    }

    /// The starter tip cycle `c` tells during the jacks (an index into
    /// [`ROBI_TIPS`], always a `TipKind::Starter`).
    #[must_use]
    pub fn tip_a(&self, cycle: u64) -> u16 {
        pick_tip(self.seed ^ cycle.rotate_left(7) ^ 0xA5A5, |k| {
            k == TipKind::Starter
        })
    }

    /// The deeper tip cycle `c` tells on the monkey bars (never a starter).
    #[must_use]
    pub fn tip_b(&self, cycle: u64) -> u16 {
        pick_tip(self.seed ^ cycle.rotate_left(7) ^ 0x5A5A, |k| {
            k != TipKind::Starter
        })
    }

    /// Evaluate the resident at `now` against live geometry. `None` only
    /// before birth (or after `stop`).
    #[must_use]
    pub fn frame(&self, now: Instant, sense: &RobiSense) -> Option<RobiFrame> {
        let at = self.since?;
        let t = ms_since(now, at);
        let g = sense.geom;
        if g.cell_w == 0 || g.cell_h == 0 || g.cols == 0 || g.rows == 0 {
            return None;
        }
        let cw = i32::from(g.cell_w).max(1);
        let ch = i32::from(g.cell_h).max(1);
        let grid_w = i32::from(g.cols) * cw;
        let art_rows = art_rows(&g);
        let body_w = (art_rows * ART_ASPECT * f32::from(g.cell_h)).round() as i32;
        let margin = body_w / 2 + 2;
        let clamp_x = |x: i32| x.clamp(margin, (grid_w - margin).max(margin));

        // THE WINDOW CLAMP (owner directive: "he must never be clipped by
        // the window edge"). `win_top..win_bot` is the window's vertical
        // extent in grid px; the fit test first — a window too small to hold
        // his drawn body HIDES him (clamping a body that cannot fit would
        // clip it at one edge or the other). Horizontal fit is against the
        // grid (stricter than the padded window), the same margin `clamp_x`
        // keeps.
        let body_h = i32::from(body_size_px(&g).1);
        if grid_w < 2 * margin || body_h > sense.win_bot - sense.win_top {
            return None;
        }
        // The vertical body clamp — the SAME top law as [`body_rect_px`]
        // (anchor fraction, same rounding), so the drawn rect lands exactly
        // inside `win_top..win_bot`, not approximately.
        let clamp_anchor_y = |anchor_y: i32, anchor: RobiAnchor| -> i32 {
            let frac = match anchor {
                RobiAnchor::Feet => FEET_FRAC,
                RobiAnchor::Grip => GRIP_FRAC,
            };
            let off = (body_h as f32 * frac).round() as i32;
            let top = (anchor_y - off).clamp(sense.win_top, sense.win_bot - body_h);
            top + off
        };
        // The ladder clamp: the emitter's segment stack overshoots `top_y`
        // by up to a quarter tile ([`ladder_tile_px`] — its reach-the-bar
        // slack), so the top holds that much inside the edge.
        let ladder_lh = i32::from(ladder_tile_px(&g).1);
        let clamp_ladder = |l: RobiLadder| RobiLadder {
            x: l.x,
            top_y: l.top_y.max(sense.win_top + ladder_lh / 4),
            bot_y: l.bot_y.min(sense.win_bot),
        };

        // The "textbox" ground: the bottom edge of the caret's row.
        let ground = (i32::from(sense.cursor.0) + 1) * ch;

        // Caret avoidance (module doc): a ground mark inside the exclusion
        // zone around the caret column steps to the nearer zone edge, the far
        // edge when `clamp_x` pulls the nearer one back inside, and stays put
        // only when the grid is too narrow to honor either side.
        let caret_cx = i32::from(sense.cursor.1) * cw + cw / 2;
        let clearance = caret_clearance(&g);
        let avoid_x = |x: i32| -> i32 {
            let x = clamp_x(x);
            if (x - caret_cx).abs() >= clearance {
                return x;
            }
            let (near, far) = if x <= caret_cx {
                (caret_cx - clearance, caret_cx + clearance)
            } else {
                (caret_cx + clearance, caret_cx - clearance)
            };
            let near = clamp_x(near);
            if (near - caret_cx).abs() >= clearance {
                return near;
            }
            let far = clamp_x(far);
            if (far - caret_cx).abs() >= clearance {
                return far;
            }
            x
        };

        let cycle = t / CYCLE_MS;
        let p = t % CYCLE_MS;

        // Per-cycle stage marks (deterministic wander): where he does the
        // jacks and where he rests, as grid fractions. Caret-avoided — these
        // are exactly the spots he LOITERS on, and they double as the walk
        // targets, so avoidance here steers the walks away too.
        let frac = |c: u64, salt: u64| -> f32 {
            (genome::mix(self.seed ^ c.rotate_left(11) ^ salt) % 1000) as f32 / 1000.0
        };
        let x_jacks = avoid_x(((0.30 + 0.40 * frac(cycle, 1)) * grid_w as f32) as i32);
        let x_rest = avoid_x(((0.50 + 0.40 * frac(cycle, 2)) * grid_w as f32) as i32);
        // Where this cycle's opening walk starts: the previous cycle's rest
        // spot (avoided the same way, so the cycle seam has no position pop)
        // — or, on the very first cycle, the right edge (his entrance).
        let x_from = if cycle == 0 {
            (grid_w - margin).max(margin)
        } else {
            avoid_x(((0.50 + 0.40 * frac(cycle - 1, 2)) * grid_w as f32) as i32)
        };

        // Handholds (the host guarantees at least two).
        let n = usize::from(sense.handhold_count).clamp(1, MAX_HANDHOLDS);
        let holds = &sense.handholds[..n];
        let x_ladder = clamp_x(holds[0]);
        // Where he stands once he is back on the floor after the bars: the
        // last hold's column, stepped aside if the caret waits beneath it.
        let x_land = avoid_x(holds[n - 1]);
        let bar_y = sense.bar_y;

        let ladder_full = RobiLadder {
            x: x_ladder,
            top_y: bar_y + ch / 4,
            bot_y: ground,
        };
        let hang_feet = bar_y + ((FEET_FRAC - GRIP_FRAC) * art_rows * ch as f32) as i32;

        let walk_pose = |t: u64| {
            if (t / WALK_FRAME_MS).is_multiple_of(2) {
                RobiGlyphId::RobiWalk0
            } else {
                RobiGlyphId::RobiWalk1
            }
        };

        // (pose, x, anchor_y, anchor, flip, ladder, animating)
        let (pose, x, anchor_y, anchor, flip_x, ladder, animating) = if p < WALK_A_END {
            let pr = fraction(p, 0, WALK_A_END);
            let x = lerp_i(x_from, x_jacks, pr);
            let flip = x_jacks > x_from;
            (walk_pose(p), x, ground, RobiAnchor::Feet, flip, None, true)
        } else if p < JACKS_END {
            let pose = if ((p - WALK_A_END) / JACK_FRAME_MS).is_multiple_of(2) {
                RobiGlyphId::RobiJacks0
            } else {
                RobiGlyphId::RobiJacks1
            };
            (pose, x_jacks, ground, RobiAnchor::Feet, false, None, true)
        } else if p < IDLE_1_END {
            (
                RobiGlyphId::RobiStand,
                x_jacks,
                ground,
                RobiAnchor::Feet,
                false,
                None,
                false,
            )
        } else if p < WALK_B_END {
            let pr = fraction(p, IDLE_1_END, WALK_B_END);
            let x = lerp_i(x_jacks, x_ladder, pr);
            let flip = x_ladder > x_jacks;
            (walk_pose(p), x, ground, RobiAnchor::Feet, flip, None, true)
        } else if p < LADDER_EXTEND_END {
            let pr = fraction(p, WALK_B_END, LADDER_EXTEND_END);
            let top = lerp_i(ladder_full.bot_y, ladder_full.top_y, pr);
            let ladder = RobiLadder {
                top_y: top,
                ..ladder_full
            };
            (
                RobiGlyphId::RobiStand,
                x_ladder,
                ground,
                RobiAnchor::Feet,
                false,
                Some(ladder),
                true,
            )
        } else if p < CLIMB_END {
            let pr = fraction(p, LADDER_EXTEND_END, CLIMB_END);
            let pose = if ((p - LADDER_EXTEND_END) / CLIMB_FRAME_MS).is_multiple_of(2) {
                RobiGlyphId::RobiClimb0
            } else {
                RobiGlyphId::RobiClimb1
            };
            let y = lerp_i(ground, hang_feet, pr);
            (
                pose,
                x_ladder,
                y,
                RobiAnchor::Feet,
                false,
                Some(ladder_full),
                true,
            )
        } else if p < BARS_END {
            let swung = ((p - CLIMB_END) / SWING_MS) as usize;
            let pr = fraction((p - CLIMB_END) % SWING_MS, 0, SWING_MS);
            let (x, pose, moving) = if swung + 1 < n {
                let x = lerp_i(clamp_x(holds[swung]), clamp_x(holds[swung + 1]), pr);
                let pose = match ((p - CLIMB_END) / (SWING_MS / 3)) % 3 {
                    0 => RobiGlyphId::RobiHangR,
                    1 => RobiGlyphId::RobiHangBoth,
                    _ => RobiGlyphId::RobiHangL,
                };
                (x, pose, true)
            } else {
                // Out of bars: a happy static dangle on the last hold.
                (clamp_x(holds[n - 1]), RobiGlyphId::RobiHangBoth, false)
            };
            let ladder = ladder_retract(p, ladder_full);
            // While the ladder is still animating away, wakes must flow even
            // if he is already dangling.
            let animating = moving
                || (LADDER_RETRACT_START..LADDER_RETRACT_START + LADDER_RETRACT_MS).contains(&p);
            (pose, x, bar_y, RobiAnchor::Grip, false, ladder, animating)
        } else if p < DROP_END {
            let pr = fraction(p, BARS_END, DROP_END);
            let y = lerp_i(hang_feet, ground, pr * pr);
            // He hops off SIDEWAYS when the caret waits below the last hold —
            // the drop lands on `x_land`, so the following stand needs no
            // corrective shuffle (with the caret elsewhere this lerp is a
            // constant and the drop is byte-identical to the straight fall).
            (
                RobiGlyphId::RobiJacks0,
                lerp_i(clamp_x(holds[n - 1]), x_land, pr),
                y,
                RobiAnchor::Feet,
                false,
                None,
                true,
            )
        } else if p < IDLE_2_END {
            (
                RobiGlyphId::RobiStand,
                x_land,
                ground,
                RobiAnchor::Feet,
                false,
                None,
                false,
            )
        } else if p < WANDER_END {
            let pr = fraction(p, IDLE_2_END, WANDER_END);
            let x0 = x_land;
            let x = lerp_i(x0, x_rest, pr);
            (
                walk_pose(p),
                x,
                ground,
                RobiAnchor::Feet,
                x_rest > x0,
                None,
                true,
            )
        } else {
            // The long calm at his rest spot — static, zero wakes.
            (
                RobiGlyphId::RobiStand,
                x_rest,
                ground,
                RobiAnchor::Feet,
                false,
                None,
                false,
            )
        };

        let tip = if (TIP_A_START..TIP_A_END).contains(&p) {
            Some(self.tip_a(cycle))
        } else if (TIP_B_START..TIP_B_END).contains(&p) {
            Some(self.tip_b(cycle))
        } else {
            None
        };

        // Birth is the only fade; a resident never fades out.
        let alpha = if t < FADE_IN_MS {
            ((t * 255) / FADE_IN_MS.max(1)) as u8
        } else {
            255
        };

        Some(RobiFrame {
            pose,
            x: clamp_x(x),
            anchor_y: clamp_anchor_y(anchor_y, anchor),
            anchor,
            flip_x,
            alpha,
            ladder: ladder.map(clamp_ladder),
            tip,
            animating: animating || t < FADE_IN_MS,
        })
    }
}

/// Milliseconds from `at` to `now`, saturating (the house clock arithmetic).
fn ms_since(now: Instant, at: Instant) -> u64 {
    now.saturating_duration_since(at).as_millis() as u64
}

/// Deterministic tip pick: filter the bank by `want`, index by the seed.
///
/// Allocation-free ON PURPOSE (PET-02): this runs on every frame of a tip
/// window (~13.7 % of all presented frames while Robi is on), and the old
/// form re-collected a `Vec` over the 27-entry const bank each call only to
/// index it once and drop it. Two passes over `ROBI_TIPS` instead: count the
/// matches, then walk to the k-th one. `filter` preserves source order, so
/// the k-th survivor IS the `pool[k]` the Vec held — the returned index is
/// bit-identical for every `(seed, want)` pair.
fn pick_tip(seed: u64, want: impl Fn(TipKind) -> bool) -> u16 {
    let n = ROBI_TIPS.iter().filter(|tip| want(tip.kind)).count();
    debug_assert!(n > 0);
    let k = (genome::mix(seed) % n.max(1) as u64) as usize;
    match ROBI_TIPS
        .iter()
        .enumerate()
        .filter(|(_, tip)| want(tip.kind))
        .nth(k)
    {
        Some((i, _)) => i as u16,
        // `k < n` by construction, so this arm is reachable only when the
        // bank has NO match at all — the same impossible state the old form
        // met by indexing into an empty Vec (a panic there too; the
        // tip-bank test pins every `want` at five-plus matches).
        None => unreachable!("pick_tip: no ROBI_TIPS entry matches `want`"),
    }
}

/// The ladder's extent at cycle-phase `p`: full until the retract starts,
/// then it sinks back into the floor and disappears.
fn ladder_retract(p: u64, full: RobiLadder) -> Option<RobiLadder> {
    if p < LADDER_RETRACT_START {
        return Some(full);
    }
    let gone = p.saturating_sub(LADDER_RETRACT_START);
    if gone >= LADDER_RETRACT_MS {
        return None;
    }
    let pr = fraction(gone, 0, LADDER_RETRACT_MS);
    Some(RobiLadder {
        top_y: lerp_i(full.top_y, full.bot_y, pr),
        ..full
    })
}

/// `t`'s progress through `[a, b)` as `0.0..=1.0`.
fn fraction(t: u64, a: u64, b: u64) -> f32 {
    if b <= a {
        return 1.0;
    }
    ((t.saturating_sub(a)) as f32 / (b - a) as f32).clamp(0.0, 1.0)
}

fn lerp_i(a: i32, b: i32, p: f32) -> i32 {
    a + ((b - a) as f32 * p).round() as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> Instant {
        Instant::now()
    }

    fn at(base: Instant, ms: u64) -> Instant {
        base + std::time::Duration::from_millis(ms)
    }

    fn sense() -> RobiSense {
        let mut handholds = [0i32; MAX_HANDHOLDS];
        for (i, h) in handholds.iter_mut().enumerate() {
            *h = 60 + (i as i32) * 90;
        }
        RobiSense {
            geom: EffectGeom {
                cell_w: 10,
                cell_h: 20,
                rows: 30,
                cols: 100,
            },
            cursor: (28, 40),
            bar_y: -10,
            // Headroom past the hanging body and footroom past the grid, so
            // the window clamp is inert everywhere the beat tests probe.
            win_top: -30,
            win_bot: 30 * 20 + 10,
            handholds,
            handhold_count: 6,
        }
    }

    /// THE sizing law. A helper sized in ROWS is a speck in a fullscreen
    /// window, so his height tracks the viewport — but only up to what the
    /// shared atlas can hold. The ceiling is not cosmetic: a body wider than
    /// `4·cell_h` (`CatBaker::slot_w`) is REFUSED by `host_tile`, and the
    /// emitter then keeps re-drawing the last body that did bake — the robot
    /// stops growing instead of failing loudly. Sweeping every reachable
    /// viewport against the slot width is what makes that unfakeable.
    #[test]
    fn art_rows_scales_with_the_window_and_always_fits_the_atlas_slot() {
        let g = |rows: u16| EffectGeom {
            cell_w: 10,
            cell_h: 20,
            rows,
            cols: 100,
        };
        let near = |a: f32, b: f32| (a - b).abs() < 1e-3;
        // A small window keeps the authored size…
        assert!(near(art_rows(&g(12)), ART_ROWS));
        assert!(near(art_rows(&g(32)), ART_ROWS));
        // …a bigger one grows him…
        assert!(art_rows(&g(48)) > ART_ROWS);
        assert!(art_rows(&g(71)) > art_rows(&g(48)));
        // …and the growth stops at the atlas ceiling.
        assert!(near(art_rows(&g(400)), ART_ROWS_MAX));

        // Monotone, and EVERY reachable viewport bakes a body the shared
        // atlas will accept (`w ≤ 4·cell_h`, `h ≤ 4 slices of 2·cell_h`).
        let mut prev = 0.0f32;
        for rows in 1u16..=400 {
            let r = art_rows(&g(rows));
            assert!(r >= prev, "rows={rows}: shrank");
            prev = r;
            let cell_h = 20.0f32;
            let h = (r * cell_h).round();
            let w = (h * ART_ASPECT).round();
            assert!(
                w <= 4.0 * cell_h,
                "rows={rows}: a {w}px-wide body overflows the {}px atlas slot",
                4.0 * cell_h
            );
            assert!(
                h <= 4.0 * 2.0 * cell_h,
                "rows={rows}: a {h}px-tall body needs more than four slices"
            );
        }
    }

    /// THE body law, pinned the way the pet pins `PetFrame::body_px`: the
    /// SAME functions the emitter bakes and places by are what a host's
    /// dismiss hit-box reads, so this test is what keeps the click box from
    /// drifting off the drawn robot when the clamp or an anchor changes.
    #[test]
    fn body_px_is_the_emitters_body_law() {
        let g = sense().geom; // 10×20 cells, 30 rows ⇒ art_rows = ART_ROWS
        // h = round(3.2·20) = 64, w = round(64·128/176) = 47 — inside the
        // 4·cell_h = 80 px atlas slot.
        assert_eq!(body_size_px(&g), (47, 64));
        let stand = RobiFrame {
            pose: RobiGlyphId::RobiStand,
            x: 500,
            anchor_y: 580,
            anchor: RobiAnchor::Feet,
            flip_x: false,
            alpha: 255,
            ladder: None,
            tip: None,
            animating: false,
        };
        // Feet: top = anchor_y − round(64·FEET_FRAC) = 580 − 62; center-x.
        assert_eq!(body_px(&stand, &g), Some((477, 524, 518, 582)));
        // Grip hangs him from the hands — a bar ABOVE row 0 keeps its
        // negative overhang (the host's strip splice owns the shift):
        // top = −10 − round(64·GRIP_FRAC) = −14.
        let hang = RobiFrame {
            anchor: RobiAnchor::Grip,
            anchor_y: -10,
            ..stand
        };
        assert_eq!(body_px(&hang, &g), Some((477, 524, -14, 50)));
        // Not-on-glass frames have NO body — the `None` is what clears a
        // host's stash — and degenerate metrics never divide.
        assert_eq!(body_px(&RobiFrame { alpha: 0, ..stand }, &g), None);
        let dead = EffectGeom { cell_w: 0, ..g };
        assert_eq!(body_px(&stand, &dead), None);
        // The atlas cap is structural across every reachable viewport.
        for rows in 1u16..=400 {
            let g = EffectGeom {
                cell_w: 10,
                cell_h: 20,
                rows,
                cols: 100,
            };
            let (w, _) = body_size_px(&g);
            assert!(
                i32::from(w) <= 4 * 20,
                "rows={rows}: a {w}px-wide body overflows the atlas slot"
            );
        }
    }

    fn show(base: Instant) -> RobiShow {
        let mut s = RobiShow::default();
        s.start(base, 7);
        s
    }

    /// Default = not born; a born resident NEVER expires — he is present in
    /// every cycle, hours in.
    #[test]
    fn a_resident_never_disappears() {
        let base = t0();
        let none = RobiShow::default();
        assert!(!none.born());
        assert!(none.frame(at(base, 5_000), &sense()).is_none());

        let s = show(base);
        assert!(s.born());
        for ms in [0, CYCLE_MS - 1, CYCLE_MS, 3 * CYCLE_MS + 12_345, 3_600_000] {
            let f = s.frame(at(base, ms), &sense());
            assert!(f.is_some(), "vanished at t={ms}");
        }
        // Birth fades in, then he is opaque forever (no fade-out exists).
        assert!(s.frame(at(base, 1), &sense()).unwrap().alpha < 30);
        assert_eq!(s.frame(at(base, 9_000), &sense()).unwrap().alpha, 255);
        assert_eq!(
            s.frame(at(base, 5 * CYCLE_MS - 10), &sense())
                .unwrap()
                .alpha,
            255
        );
    }

    /// The cycle hits every promised beat: textbox walk, jumping jacks with a
    /// STARTER tip, a ladder that extends, a climb, monkey bars with a deeper
    /// tip, the drop, and the rest stands.
    #[test]
    fn the_rounds_hit_every_beat() {
        let base = t0();
        let s = show(base);
        let se = sense();
        let ground = 29 * 20; // (cursor row + 1) · cell_h

        let walk = s.frame(at(base, 1_200), &se).unwrap();
        assert!(matches!(
            walk.pose,
            RobiGlyphId::RobiWalk0 | RobiGlyphId::RobiWalk1
        ));
        assert_eq!(walk.anchor, RobiAnchor::Feet);
        assert_eq!(walk.anchor_y, ground, "he walks ON the prompt line");
        assert!(walk.animating);

        let jacks = s.frame(at(base, 5_000), &se).unwrap();
        assert!(matches!(
            jacks.pose,
            RobiGlyphId::RobiJacks0 | RobiGlyphId::RobiJacks1
        ));
        let tip_a = jacks.tip.expect("starter tip during the jacks");
        assert_eq!(ROBI_TIPS[tip_a as usize].kind, TipKind::Starter);

        let extend = s.frame(at(base, 19_800), &se).unwrap();
        let ladder = extend.ladder.expect("ladder while extending");
        assert!(ladder.top_y > se.bar_y, "still extending upward");
        assert_eq!(ladder.bot_y, ground);

        let climb = s.frame(at(base, 22_000), &se).unwrap();
        assert!(matches!(
            climb.pose,
            RobiGlyphId::RobiClimb0 | RobiGlyphId::RobiClimb1
        ));
        assert!(climb.anchor_y < ground, "off the floor");
        assert!(climb.ladder.is_some());

        let bars = s.frame(at(base, 26_000), &se).unwrap();
        assert!(matches!(
            bars.pose,
            RobiGlyphId::RobiHangL | RobiGlyphId::RobiHangR | RobiGlyphId::RobiHangBoth
        ));
        assert_eq!(bars.anchor, RobiAnchor::Grip);
        assert_eq!(bars.anchor_y, se.bar_y);
        let tip_b = bars.tip.expect("deeper tip on the bars");
        assert_ne!(ROBI_TIPS[tip_b as usize].kind, TipKind::Starter);

        let rest = s.frame(at(base, CYCLE_MS - 5_000), &se).unwrap();
        assert_eq!(rest.pose, RobiGlyphId::RobiStand);
        assert_eq!(rest.anchor_y, ground);
    }

    /// Idle stands are STATIC (zero-wake contract): not animating, and two
    /// probes seconds apart inside one idle window evaluate identically.
    #[test]
    fn idle_is_static_and_costs_no_wakes() {
        let base = t0();
        let s = show(base);
        let se = sense();
        for (a, b) in [(12_000, 15_000), (40_000, 44_000), (60_000, 74_000)] {
            let fa = s.frame(at(base, a), &se).unwrap();
            let fb = s.frame(at(base, b), &se).unwrap();
            assert!(!fa.animating, "idle at {a} must not arm wakes");
            assert_eq!(fa.pose, fb.pose);
            assert_eq!((fa.x, fa.anchor_y, fa.alpha), (fb.x, fb.anchor_y, fb.alpha));
        }
        // …and the moving scenes really do animate (non-vacuity).
        assert!(s.frame(at(base, 5_000), &se).unwrap().animating);
        assert!(s.frame(at(base, 22_000), &se).unwrap().animating);
    }

    /// The ladder retracts while he is on the bars and is gone by the drop.
    #[test]
    fn ladder_retracts_behind_him() {
        let base = t0();
        let s = show(base);
        let se = sense();
        assert!(s.frame(at(base, 25_000), &se).unwrap().ladder.is_some());
        assert!(s.frame(at(base, 27_000), &se).unwrap().ladder.is_none());
        assert!(
            s.frame(at(base, BARS_END + 100), &se)
                .unwrap()
                .ladder
                .is_none()
        );
    }

    /// Pure function: identical inputs ⇒ identical frames. Tips rotate BOTH
    /// across seeds and across cycles of one resident.
    #[test]
    fn deterministic_and_tips_rotate() {
        let base = t0();
        let s = show(base);
        let se = sense();
        let a = s.frame(at(base, 26_000), &se).unwrap();
        let b = s.frame(at(base, 26_000), &se).unwrap();
        assert_eq!(a.tip, b.tip);
        assert_eq!((a.x, a.anchor_y, a.alpha), (b.x, b.anchor_y, b.alpha));

        let seed_picks: std::collections::BTreeSet<u16> = (0..24u64)
            .map(|i| {
                let mut s = RobiShow::default();
                s.start(base, i);
                s.tip_b(0)
            })
            .collect();
        assert!(seed_picks.len() >= 6, "24 seeds should span many tips");

        let cycle_picks: std::collections::BTreeSet<u16> = (0..24u64).map(|c| s.tip_b(c)).collect();
        assert!(
            cycle_picks.len() >= 6,
            "one resident's cycles should span many tips"
        );
    }

    /// THE WINDOW CLAMP (owner directive: never clipped by a window edge).
    /// A shallow window — NO headroom above row 0, NO padding below the
    /// grid, the bar line above the top edge (the chromeless-window band),
    /// the caret on the TOP row so every ground scene would naturally poke
    /// his head past the top — must keep his DRAWN body ([`body_px`], the
    /// emitter's own placement law) and his ladder stack (top segment
    /// overshoot included, [`ladder_tile_px`]) fully inside the window, in
    /// every scene of every cycle.
    #[test]
    fn every_scene_stays_inside_a_shallow_window() {
        let base = t0();
        let s = show(base);
        let mut se = sense();
        se.cursor = (0, 40); // ground = 20 px — a standing body wants top −42
        se.bar_y = -7; // the no-headroom hand line (−ch/3)
        se.win_top = 0; // chromeless: the glass ends AT row 0
        se.win_bot = 30 * 20; // …and at the grid's last row
        let grid_w = 100 * 10;
        let lh = i32::from(ladder_tile_px(&se.geom).1);
        // From the end of the birth fade (an alpha-0 frame has no body).
        for t in (FADE_IN_MS..2 * CYCLE_MS).step_by(97) {
            let f = s.frame(at(base, t), &se).expect("the window holds him");
            let (x0, x1, y0, y1) = body_px(&f, &se.geom).expect("on glass");
            assert!(y0 >= se.win_top, "t={t}: body top {y0} pokes past the edge");
            assert!(y1 <= se.win_bot, "t={t}: body bottom {y1} clips");
            assert!(x0 >= 0 && x1 <= grid_w, "t={t}: body x {x0}..{x1} clips");
            if let Some(l) = f.ladder {
                assert!(
                    l.top_y - lh / 4 >= se.win_top,
                    "t={t}: ladder stack top {} clips (top_y {})",
                    l.top_y - lh / 4,
                    l.top_y
                );
                assert!(l.bot_y <= se.win_bot, "t={t}: ladder foot {} clips", l.bot_y);
            }
        }
    }

    /// A window too small to hold his body HIDES him — `frame` → `None`, no
    /// clipped sliver — and he walks right back once the window can hold him
    /// again (still born; hiding is per-evaluation, not a state change).
    #[test]
    fn a_window_too_small_to_hold_him_hides_him() {
        let base = t0();
        let s = show(base);
        // Too shallow: 2 rows of glass (40 px) against his 64 px body.
        let mut shallow = sense();
        shallow.geom.rows = 2;
        shallow.cursor = (1, 40);
        shallow.win_top = 0;
        shallow.win_bot = 2 * 20;
        assert!(s.frame(at(base, 5_000), &shallow).is_none());
        // Too narrow: 4 columns (40 px) against his 47 px body + margin.
        let mut narrow = sense();
        narrow.geom.cols = 4;
        narrow.cursor = (28, 1);
        assert!(s.frame(at(base, 5_000), &narrow).is_none());
        // The same instant against a window that fits: present.
        assert!(s.born());
        assert!(s.frame(at(base, 5_000), &sense()).is_some());
    }

    /// Every frame stays horizontally inside the grid, across two full cycles.
    #[test]
    fn stays_inside_the_grid() {
        let base = t0();
        let s = show(base);
        let se = sense();
        let grid_w = 100 * 10;
        for t in (0..2 * CYCLE_MS).step_by(97) {
            if let Some(f) = s.frame(at(base, t), &se) {
                assert!(f.x >= 0 && f.x <= grid_w, "t={t}: x={} escapes", f.x);
            }
        }
    }

    /// CARET AVOIDANCE: every scene where he LOITERS on the ground — the
    /// jumping jacks, both idle stands, the long rest — keeps his body center
    /// at least [`caret_clearance`] from the caret column's center, wherever
    /// the caret is and whichever cycle's marks are in play.
    #[test]
    fn stationary_scenes_stay_out_of_the_typists_column() {
        let base = t0();
        let s = show(base);
        let clearance = caret_clearance(&sense().geom);
        // One probe inside each stationary window: jacks, idle-1, idle-2, rest.
        let probes = [5_000u64, 12_000, 40_000, 60_000];
        for col in [0u16, 10, 25, 40, 55, 70, 85, 99] {
            let mut se = sense();
            se.cursor = (28, col);
            let caret_cx = i32::from(col) * 10 + 5;
            for cycle in 0..4u64 {
                for off in probes {
                    let f = s.frame(at(base, cycle * CYCLE_MS + off), &se).unwrap();
                    assert!(
                        (f.x - caret_cx).abs() >= clearance,
                        "cycle {cycle} t+{off} col {col}: x={} loiters within \
                         {clearance}px of caret center {caret_cx}",
                        f.x
                    );
                }
            }
        }
    }

    /// CARET AVOIDANCE is PURE and keeps the zero-wake idle contract: park
    /// the caret exactly where he would otherwise rest and he stands aside —
    /// still `animating == false`, and two probes of the identical sense
    /// seconds apart evaluate to the identical static frame.
    #[test]
    fn a_caret_parked_under_a_resting_robi_displaces_him_purely() {
        let base = t0();
        let s = show(base);
        let mut far = sense();
        far.cursor = (28, 0);
        // Where he rests with the caret far away…
        let rest = s.frame(at(base, 60_000), &far).unwrap();
        assert!(!rest.animating, "the long rest is static");
        // …then park the caret in that very column.
        let col = (rest.x / 10) as u16;
        let mut under = sense();
        under.cursor = (28, col);
        let clearance = caret_clearance(&under.geom);
        let caret_cx = i32::from(col) * 10 + 5;
        let a = s.frame(at(base, 60_000), &under).unwrap();
        let b = s.frame(at(base, 74_000), &under).unwrap();
        assert!(
            (a.x - caret_cx).abs() >= clearance,
            "x={} still loiters on the caret at {caret_cx}",
            a.x
        );
        assert_ne!(a.x, rest.x, "the parked caret really displaced him");
        assert!(!a.animating, "displacement must not arm wakes");
        assert_eq!(a.pose, b.pose);
        assert_eq!(
            (a.x, a.anchor_y, a.alpha, a.animating),
            (b.x, b.anchor_y, b.alpha, b.animating),
            "identical sense ⇒ identical static frame"
        );
    }

    /// The ladder/climb arc is EXEMPT from caret avoidance: the ladder must
    /// stay planted on `holds[0]` even with the caret right beneath it (up
    /// there he is above the text anyway).
    #[test]
    fn the_ladder_stays_planted_even_with_the_caret_beneath_it() {
        let base = t0();
        let s = show(base);
        let mut se = sense();
        // holds[0] = 60 ⇒ column 6's center (65) is well inside the zone.
        se.cursor = (28, 6);
        let climb = s.frame(at(base, 22_000), &se).unwrap();
        assert_eq!(climb.x, 60, "the climb tracks the first handhold");
        assert_eq!(climb.ladder.expect("ladder while climbing").x, 60);
    }

    /// The bank keeps its promises: enough variety in every category, the
    /// famous permission-prompt tip included verbatim, and bubble-sized text.
    #[test]
    fn tip_bank_is_wide_and_on_topic() {
        let count = |k: TipKind| ROBI_TIPS.iter().filter(|t| t.kind == k).count();
        assert!(count(TipKind::Starter) >= 5);
        assert!(count(TipKind::Aterm) >= 5);
        assert!(count(TipKind::Terminal) >= 8);
        assert!(count(TipKind::Claude) >= 4);
        assert!(
            ROBI_TIPS
                .iter()
                .any(|t| t.text.contains("claude --dangerously-skip-permissions")),
            "the permission-prompt tip is the headliner"
        );
        for tip in ROBI_TIPS {
            assert!(
                !tip.text.is_empty() && tip.text.len() <= 90,
                "bubble-sized: {:?}",
                tip.text
            );
        }
    }
}
