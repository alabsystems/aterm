// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The subtle TRANSIENT NOTICE: a small floating [`DrawPrim`] card near the top of the
//! window that slides in, holds, then lifts away over a few seconds — used for the
//! non-disruptive update moments:
//!   * [`NoticeKind::UpdateReady`] — a strictly-newer build just STAGED. "Update ready"
//!     (accent badge + a trailing chevron, the two marks that say "this one is a
//!     button"); CLICKING it APPLIES the update in one gesture
//!     (`App::apply_update_or_details` — details-overlay fallback when nothing is
//!     actually staged). The persistent affordances (version-menu ⬆️ on macOS /
//!     tab-strip ↻ elsewhere) stay after it fades.
//!   * [`NoticeKind::LevelUp`] — the app just RELAUNCHED into a newer build (a re-exec
//!     handoff set `$ATERM_UPDATED_FROM`). A quiet, cursor-themed "leveled-up" flourish
//!     ("Updated to build N") that celebrates the swap, then fades — the "level up" analog
//!     the design asked for, without literally saying "level up" or blocking the flow.
//!   * [`NoticeKind::UpdateStatus`] — the background lane reporting for itself. Quiet by
//!     construction: a HOLLOW badge, no chevron, not clickable-to-apply.
//!
//! GLOBAL (App-level), painted into every window like `config_notice` — and it borrows the
//! SAME timed-lifecycle shape (`is_expired` + `deadline`), but renders through the SACRED
//! `settings_card`/`badge_card` composite path as pure [`DrawPrim`]s (native chrome, NOT
//! terminal grid cells). The whole card's alpha is multiplied by [`TransientNotice::alpha`]
//! so the fade is a real ramp, re-rasterized as the quantized alpha changes.
//!
//! # The typographic contract (why this file parses its own caption)
//!
//! Callers author ONE string in a fixed grammar — `"<marker> <title> — <detail>"` (the
//! separator may also be `": "`, and both the marker and the detail are optional) — and
//! this widget paints it as three DIFFERENT things: a pictogram inside a badge, a
//! semibold UI-face title, and a secondary-colour detail. Keeping the authored form a
//! single string is deliberate: the wording IS the contract with the update lane (the
//! `pill()` tests assert on it, and `text()` is the one accessible/loggable rendering),
//! while [`caption_parts`] gives paint the hierarchy the flat string could never express.
//! A caption that does not follow the grammar degrades honestly: no marker ⇒ an
//! attention badge, no separator ⇒ the whole caption is the title.

use std::time::{Duration, Instant};

use aterm_render::Theme;

use crate::settings::{Roles, SettingsGeom, text_w};
use crate::tray_raster::{baseline_centered_at, row_baseline, ui_text_width_for};
use crate::type_scale::{StepPx, TypeStep};
use crate::widget::{DrawPrim, TextFace, TextWeight, TrayInput, rgba, text_prim};

/// How long the notice stays up before it is fully gone (slide-in + hold + lift-away).
const TTL: Duration = Duration::from_millis(5400);
/// The entrance: the card eases down into place and fades up over this opening stretch.
const ENTER: Duration = Duration::from_millis(220);
/// The exit tail: the last stretch of [`TTL`] over which the card lifts and alpha ramps
/// 1→0. Shorter than the old 1.4s linear dissolve — a long linear fade reads as sluggish,
/// an eased 0.8s reads as "it left".
const FADE: Duration = Duration::from_millis(800);
/// Animation cadence while the card is MOVING (entrance/exit) — the deadline granularity.
/// ~60fps: the card is a ~300×34px raster, and the motion is under a second in total.
const FRAME: Duration = Duration::from_millis(16);

/// How far outside the card rect the soft shadow reaches, in px. The compositor
/// ([`crate::App::splice_notice`]) grows its paint region by exactly this, so the shadow
/// cannot be cropped and the two cannot drift.
pub(crate) const SHADOW_MARGIN: f32 = 12.0;

/// The stacked shadow layers as `(spread, dy, alpha)`, widest and faintest last.
///
/// Data rather than a literal inside the loop so [`shadow_stays_inside_its_margin`] can
/// re-derive the furthest reach from the same numbers the renderer draws. The comment
/// beside the loop has always claimed these stay inside [`SHADOW_MARGIN`]; until this
/// was a table, nothing checked it, and a fourth layer or a bigger spread would have
/// been cropped by the compositor's paint region with no test to say so.
const SHADOW_LAYERS: [(f32, f32, u8); 3] = [(1.0, 1.0, 0x22), (3.0, 2.5, 0x16), (6.5, 4.5, 0x0C)];

/// The alpha below which the card stops being a click target. The exit tail runs the
/// card down to nothing, and an all-but-invisible thing that swallows clicks — or worse,
/// re-execs the app — is a trap. Above this the card is plainly on screen.
pub(crate) const CLICK_MIN_ALPHA: f32 = 0.35;

/// The rest position's distance below the first row the card is allowed to occupy, in
/// cell heights. The card floats over terminal OUTPUT by design (it is transient), but it
/// must never float over in-grid CHROME — see `clear_rows` on [`notice_rect`].
const REST_Y_CELLS: f32 = 0.85;
/// How far the card travels on its way in and out, in cell heights.
const SLIDE_CELLS: f32 = 0.42;

/// Which update moment the notice marks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NoticeKind {
    /// A strictly-newer build staged and is ready to install. Clickable → APPLY (one
    /// click; see `App::notice_click`).
    UpdateReady { version: String, build: u64 },
    /// The app relaunched into a newer build (post-update celebration).
    LevelUp { build: u64 },
    /// Nonmodal updater status used by automatic/background paths. It is deliberately
    /// not clickable: details remain in Settings/the Version menu.
    UpdateStatus { text: String },
    /// One of ROBI the helper robot's tips (`aterm_effects::robi::ROBI_TIPS`) — the
    /// bubble half of his show. Not clickable; it fades like every notice. The bank
    /// is `&'static`, so the variant borrows rather than clones.
    RobiTip { text: &'static str },
}

/// A single transient, self-expiring notice.
pub(crate) struct TransientNotice {
    kind: NoticeKind,
    spawned: Instant,
    /// Speech-bubble anchor in tray px — `(x_center, y_top_of_speaker)`.
    /// Only Robi's tips carry one: the card centers over the anchor and sits
    /// ABOVE it (his head) instead of at the top-center rest position, and
    /// the host refreshes it each frame so the bubble follows him. `None`
    /// (every update notice) keeps the classic top-center placement.
    anchor: Option<(f32, f32)>,
}

impl TransientNotice {
    pub(crate) fn update_ready(version: String, build: u64, now: Instant) -> Self {
        Self {
            kind: NoticeKind::UpdateReady { version, build },
            spawned: now,
            anchor: None,
        }
    }

    pub(crate) fn level_up(build: u64, now: Instant) -> Self {
        Self {
            kind: NoticeKind::LevelUp { build },
            spawned: now,
            anchor: None,
        }
    }

    pub(crate) fn update_status(text: impl Into<String>, now: Instant) -> Self {
        Self {
            kind: NoticeKind::UpdateStatus { text: text.into() },
            spawned: now,
            anchor: None,
        }
    }

    pub(crate) fn robi_tip(text: &'static str, anchor: Option<(f32, f32)>, now: Instant) -> Self {
        Self {
            kind: NoticeKind::RobiTip { text },
            spawned: now,
            anchor,
        }
    }

    /// Refresh the speech-bubble anchor (Robi moves; his bubble follows).
    /// Meaningful only for [`NoticeKind::RobiTip`]; ignored otherwise so an
    /// update pill can never be dragged around by decoration state.
    pub(crate) fn set_anchor(&mut self, anchor: Option<(f32, f32)>) {
        if self.is_robi_tip() {
            self.anchor = anchor;
        }
    }

    /// Whether this is one of Robi's tip bubbles — the one variant another
    /// Robi tip may freely replace (an update notice must never be clobbered
    /// by a decoration).
    pub(crate) fn is_robi_tip(&self) -> bool {
        matches!(self.kind, NoticeKind::RobiTip { .. })
    }

    /// Whether this notice is `UpdateReady` (the clickable variant).
    pub(crate) fn is_update_ready(&self) -> bool {
        matches!(self.kind, NoticeKind::UpdateReady { .. })
    }

    /// Whether this is the post-update decorative flourish. Serious mode may
    /// discard this variant while preserving actionable/status update notices.
    pub(crate) fn is_level_up(&self) -> bool {
        matches!(self.kind, NoticeKind::LevelUp { .. })
    }

    /// Fully gone (past its whole lifetime) — the caller drops it.
    pub(crate) fn is_expired(&self, now: Instant) -> bool {
        now.duration_since(self.spawned) >= TTL
    }

    /// The next wake time: animate every [`FRAME`] while the card is moving (the
    /// entrance ramp and the exit tail), otherwise just wake at the exit boundary — a
    /// steady hold needs no intermediate repaints.
    pub(crate) fn deadline(&self, now: Instant) -> Instant {
        let elapsed = now.duration_since(self.spawned);
        let fade_start = TTL.saturating_sub(FADE);
        if elapsed < ENTER || elapsed >= fade_start {
            now + FRAME
        } else {
            self.spawned + fade_start
        }
    }

    /// The whole-card alpha at `now`: an eased ramp 0→1 across the entrance, `1.0`
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

    /// The card's vertical displacement from its rest position, as a fraction of
    /// [`SLIDE_CELLS`] — NEGATIVE is above the rest position. It drops in from above and
    /// lifts back out the same way, so the whole life reads as one gesture rather than a
    /// thing that blinks on and dissolves. `0` throughout the hold.
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

    /// A repaint fingerprint folded into `RepaintKey::notice_fp`, quantized so the card
    /// re-presents on each animation step but NOT every idle frame during the hold. `0` is
    /// the no-notice sentinel, so a live notice is forced non-zero.
    pub(crate) fn fingerprint(&self, now: Instant) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        match &self.kind {
            NoticeKind::UpdateReady { version, build } => {
                0u8.hash(&mut h);
                version.hash(&mut h);
                build.hash(&mut h);
            }
            NoticeKind::LevelUp { build } => {
                1u8.hash(&mut h);
                build.hash(&mut h);
            }
            NoticeKind::UpdateStatus { text } => {
                2u8.hash(&mut h);
                text.hash(&mut h);
            }
            NoticeKind::RobiTip { text } => {
                3u8.hash(&mut h);
                text.hash(&mut h);
            }
        }
        // Quantize BOTH animated quantities so a moving card re-rasterizes on each step
        // while a held one hashes stable. 48 steps over a ≤0.8s ramp is finer than the
        // 60fps cadence can consume, so no step is ever quantized away.
        ((self.alpha(now) * 48.0) as u64).hash(&mut h);
        ((self.rise(now).abs() * 48.0) as u64).hash(&mut h);
        // The speech-bubble anchor moves with its speaker — quantized to 2px
        // steps so a swinging Robi re-rasters his bubble along the way.
        if let Some((ax, ay)) = self.anchor {
            (((ax * 0.5) as i64), ((ay * 0.5) as i64)).hash(&mut h);
        }
        h.finish() | 1
    }

    /// The authored caption, in the `"<marker> <title> — <detail>"` grammar this module
    /// documents. This is the ACCESSIBLE/loggable rendering and the string the update-lane
    /// tests pin; paint uses [`caption_parts`] to give it hierarchy.
    ///
    /// `pub(crate)` so update-lane tests can pin the exact user-facing string a
    /// scheduling state produces — the wording IS the contract there ("it comes
    /// back by itself" vs "go press something"), and asserting on it is the only
    /// way a predicate that silently flips has to fail.
    pub(crate) fn text(&self) -> String {
        match &self.kind {
            // A staged build can share the running build's display version (the
            // updater orders by build number — see `menu::staged_apply_label`), so
            // naming only the version would announce the version already running.
            NoticeKind::UpdateReady { version, build } => {
                if version == crate::build_info::version_display() {
                    format!("\u{2191} Update ready \u{2014} build {build}")
                } else {
                    format!("\u{2191} Update ready \u{2014} v{version}")
                }
            }
            // THE VERSION, NOT THE BUILD NUMBER. A build number is a claimed monotonic
            // integer seeded from the unix epoch, so this line read "now on build
            // 1786471542" — ten digits that mean nothing to the person reading them, in
            // the one notice whose whole job is to feel like a small reward. We are
            // already RUNNING the new build here (unlike `UpdateReady`, which must
            // disambiguate a staged build that shares the running version), so the
            // running version is the honest and legible thing to name. The build number
            // stays reachable in the Version menu and the log for anyone who needs it.
            NoticeKind::LevelUp { build } => {
                let version = crate::build_info::version_display();
                if version.is_empty() {
                    format!("\u{2726} Updated \u{2014} now on build {build}")
                } else {
                    format!("\u{2726} Updated \u{2014} now on v{version}")
                }
            }
            NoticeKind::UpdateStatus { text } => text.clone(),
            // The gear pictogram takes the badge; the tip itself is all title
            // (no separator), so narrow windows elide it instead of dropping
            // the half that carries the advice.
            NoticeKind::RobiTip { text } => format!("\u{2699} {text}"),
        }
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

/// The caption split into the three things paint draws differently.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct CaptionParts {
    /// The leading pictogram, if the caption authored one (it goes INSIDE the badge, not
    /// into the sentence).
    pub(crate) marker: Option<String>,
    /// The headline half — semibold, primary colour.
    pub(crate) title: String,
    /// The trailing half after `" — "`/`": "` — regular, secondary colour.
    pub(crate) detail: Option<String>,
}

/// Split an authored caption per the module's grammar. Degrades honestly: an unmarked
/// caption keeps `marker: None`, an unseparated one is all title.
pub(crate) fn caption_parts(text: &str) -> CaptionParts {
    let text = text.trim();
    // A marker is a single leading NON-alphanumeric glyph, either followed by a space (the
    // pictograms the update lane authors — ↑ ✦ ✓ ⇣ — ahead of their sentence) or standing
    // as the WHOLE caption. Anything else is just prose: a sentence opening with a quote
    // or a bracket keeps its punctuation instead of losing it to the badge.
    let mut marker = None;
    let mut rest = text;
    let mut chars = text.chars();
    if let Some(first) = chars.next()
        && !first.is_alphanumeric()
    {
        match chars.next() {
            Some(' ') => {
                marker = Some(first.to_string());
                rest = text[first.len_utf8() + 1..].trim_start();
            }
            // Marker only, no sentence: the badge carries it and the title is empty,
            // rather than painting a pictogram as prose beside a fabricated "!" badge.
            None => {
                marker = Some(first.to_string());
                rest = "";
            }
            _ => {}
        }
    }
    // The separator: an em dash between spaces (the house form), else a colon+space (the
    // roster form, "…installed: ay, trust"). First match wins; a dash inside the DETAIL
    // therefore stays in the detail.
    let split = rest
        .find(" \u{2014} ")
        .map(|i| (i, i + " \u{2014} ".len()))
        .or_else(|| rest.find(": ").map(|i| (i, i + 2)));
    let (title, detail) = match split {
        Some((end, start)) => {
            let detail = rest[start..].trim();
            (
                rest[..end].trim().to_string(),
                (!detail.is_empty()).then(|| detail.to_string()),
            )
        }
        None => (rest.to_string(), None),
    };
    CaptionParts {
        marker,
        title,
        detail,
    }
}

/// How loud the badge is. The hierarchy IS the information: only an actionable notice
/// gets a filled accent badge and a chevron, so "there is something to press" is legible
/// before a single word is read.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tone {
    /// Actionable: filled accent badge + chevron.
    Action,
    /// The post-update flourish: filled badge in the live cursor colour.
    Celebrate,
    /// A completed background job (`✓`): filled success badge.
    Success,
    /// Everything else the background lane says: a hollow, quiet badge.
    Quiet,
}

impl Tone {
    fn of(kind: &NoticeKind, marker: Option<&str>) -> Self {
        match kind {
            NoticeKind::UpdateReady { .. } => Self::Action,
            NoticeKind::LevelUp { .. } => Self::Celebrate,
            // A status pill is never clickable, so it never wears the actionable badge
            // however its caption is marked — except a ✓ completion, which has earned a
            // positive mark and is not asking for anything.
            NoticeKind::UpdateStatus { .. } => {
                if marker == Some("\u{2713}") {
                    Self::Success
                } else {
                    Self::Quiet
                }
            }
            // Robi's tips wear the celebration badge (the live cursor colour,
            // legibility-conditioned below): friendly, and never mistakable
            // for the actionable accent.
            NoticeKind::RobiTip { .. } => Self::Celebrate,
        }
    }

    /// Whether the badge is a filled disc (loud) or a hairline ring (quiet).
    fn filled(self) -> bool {
        !matches!(self, Self::Quiet)
    }

    /// The badge colour for this tone.
    fn color(self, r: &Roles, cursor: [u8; 3]) -> [u8; 3] {
        match self {
            Self::Action => r.accent,
            Self::Celebrate => cursor,
            Self::Success => r.success,
            Self::Quiet => r.text_secondary,
        }
    }
}

/// Rec. 709 relative luminance of a gamma-encoded colour, 0..255. Good enough to decide
/// "can these two be told apart", which is all this widget asks of it.
fn luma(c: [u8; 3]) -> f32 {
    0.2126 * f32::from(c[0]) + 0.7152 * f32::from(c[1]) + 0.0722 * f32::from(c[2])
}

/// Black or white, whichever stays legible ON `fill`. The badge pictogram sits on a
/// themed disc whose luminance is not knowable ahead of time (the accent is user
/// configurable and `Celebrate` takes the live cursor colour), so the contrast is
/// computed rather than assumed.
fn on_fill(fill: [u8; 3]) -> [u8; 3] {
    if luma(fill) > 140.0 {
        [0x10, 0x12, 0x16]
    } else {
        [0xFF, 0xFF, 0xFF]
    }
}

/// The minimum luminance gap (0..255) the badge must hold against the card surface.
const BADGE_CONTRAST: f32 = 52.0;

/// `fill` pushed away from `surface` until the two can be told apart.
///
/// `Celebrate` takes the LIVE CURSOR COLOUR, which the user owns and which has no
/// relationship to the card's elevated surface — a dark-grey cursor on a dark card paints
/// an invisible badge, and the flourish silently becomes a floating pictogram. Rather than
/// discard the user's colour, it is walked toward white (on a dark card) or black (on a
/// light one) by the smallest step that clears [`BADGE_CONTRAST`], so the hue survives and
/// the disc is always seen.
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
    // Sixteen bounded steps: enough to resolve BADGE_CONTRAST from any starting pair, and
    // it terminates whether or not the target itself clears the gap (a mid-grey surface
    // against pure black is only ~127 apart, which it does).
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

/// The resolved geometry + fitted text of one notice card. Painted by [`notice_tray`] and
/// hit-tested by `App::notice_click` through [`notice_rect`], so the pixels and the click
/// target are the SAME arithmetic by construction.
struct Pill {
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
    marker: String,
    title: String,
    title_x: f32,
    detail: Option<(String, f32)>,
    chevron_cx: Option<f32>,
    baseline: f32,
    tone: Tone,
}

/// Lay one notice out.
///
/// `motion` is the reduced-motion amplitude
/// (`MotionPolicy::amplitude(MotionEffect::NoticePill)`): at `0` the card holds its rest
/// position for its whole life and only the alpha ramps, which is exactly what
/// reduced-motion asks for — information kept, movement removed.
///
/// `clear_rows` is the number of IN-GRID chrome rows at the top of the terminal area the
/// card must sit below — the tab strip. The old pill was pinned half a row down from the
/// grid's origin, which put it ON TOP of the strip: it hid the tabs it floated over, and
/// because the mouse path checks the pill before the strip, a click meant for a tab hit
/// the pill instead (or, for a non-clickable status, fell through the pill and closed a
/// tab).
fn layout(
    n: &TransientNotice,
    g: &SettingsGeom,
    now: Instant,
    motion: f32,
    clear_rows: f32,
) -> Pill {
    let (cw, ch, px) = (g.cw, g.ch, g.font_px);
    let tray_w = g.cols as f32 * cw;
    // Body text at the Secondary step (0.9×) rather than the old Caption (0.8×): this is
    // a sentence the user is meant to READ across a room-width window, not a chip label.
    let size = TypeStep::Secondary.px_clamped(px, 12.0, f32::INFINITY);
    let glyph_px = TypeStep::Caption.px_clamped(px, 10.0, f32::INFINITY);
    let s = size.get();

    let parts = caption_parts(&n.text());
    let tone = Tone::of(&n.kind, parts.marker.as_deref());
    // An unmarked caption still gets a badge — an attention mark, because a marker-less
    // status is exactly the "something needs you" case (`Wake::UpdateHealth`).
    let marker = parts.marker.unwrap_or_else(|| "!".to_string());

    let badge_r = s * 0.72;
    let pad_x = s * 0.80;
    let gap_badge = s * 0.52;
    let gap_detail = s * 0.46;
    let (chev_w, chev_gap) = if n.is_update_ready() {
        (s * 0.30, s * 0.62)
    } else {
        (0.0, 0.0)
    };

    let h = (2.0 * badge_r).max(s) + s * 1.10;
    let radius = (h * 0.5).min(16.0);
    // The card may not exceed the tray minus a cell of air on each side.
    let max_w = (tray_w - 2.0 * cw).max(0.0);
    let lead = pad_x + 2.0 * badge_r + gap_badge;
    let trail = chev_gap + chev_w + pad_x;

    let title_w = |t: &str| ui_text_width_for(TextFace::UiBold, t, s);
    let detail_w = |t: &str| ui_text_width_for(TextFace::Ui, t, s);

    // FIT ORDER: full → drop the detail → elide the title. The detail goes first because
    // it is the qualifier ("v0.5.15", "see Version menu"); the title is the state, and a
    // user who can read only one of the two must get the state.
    let mut detail = parts.detail;
    let mut title = parts.title;
    let mut w = lead
        + title_w(&title)
        + trail
        + detail.as_deref().map_or(0.0, |d| gap_detail + detail_w(d));
    if w > max_w && detail.is_some() {
        detail = None;
        w = lead + title_w(&title) + trail;
    }
    if w > max_w {
        let room = max_w - lead - trail;
        title = elided(&title, room, s);
        w = lead + title_w(&title) + trail;
    }
    // A window too narrow for even the badge and its padding gets NO card rather than a
    // clamped sliver: the clamp would keep a zero-width rect while the badge and caption
    // still painted from their un-clamped anchors, i.e. a pictogram floating on the
    // terminal with no card under it.
    let w = if w > max_w { 0.0 } else { w.max(0.0) };

    // Placement: top-center rest position — unless this is a SPEECH BUBBLE
    // (Robi's tips carry an anchor): then the card centers over the speaker
    // and sits just ABOVE his head, clamped inside the tray and below the
    // in-grid chrome.
    let (x, rest_y) = match n.anchor {
        Some((ax, ay)) if w > 0.0 => {
            let x = (ax - w * 0.5).clamp(cw * 0.5, (tray_w - w - cw * 0.5).max(0.0));
            let min_y = clear_rows.max(0.0) * ch + 2.0;
            let rest_y = (ay - h - ch * 0.35).max(min_y);
            (x, rest_y)
        }
        _ => (
            ((tray_w - w) * 0.5).max(0.0),
            (clear_rows.max(0.0) + REST_Y_CELLS) * ch,
        ),
    };
    let y = rest_y + n.rise(now) * SLIDE_CELLS * ch * motion.clamp(0.0, 1.0);

    let badge_cx = x + pad_x + badge_r;
    let badge_cy = y + h * 0.5;
    let title_x = x + lead;
    let detail_x = title_x + title_w(&title) + gap_detail;
    let detail = detail.map(|d| (d, detail_x));
    let chevron_cx = n.is_update_ready().then_some(x + w - pad_x - chev_w * 0.5);

    Pill {
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
        marker,
        title,
        title_x,
        detail,
        chevron_cx,
        baseline: row_baseline(y, h, s),
        tone,
    }
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

/// The card rect `(x, y, w, h)` in tray px — where [`notice_tray`] draws it and where a
/// click on an `UpdateReady` notice is tested. `now`/`motion` are part of the geometry
/// because the card MOVES: the hit region must track the pixels through the slide.
pub(crate) fn notice_rect(
    n: &TransientNotice,
    g: &SettingsGeom,
    now: Instant,
    motion: f32,
    clear_rows: f32,
) -> (f32, f32, f32, f32) {
    let p = layout(n, g, now, motion, clear_rows);
    (p.x, p.y, p.w, p.h)
}

/// Build the notice card as pure [`DrawPrim`]s, with the whole card's alpha scaled by
/// `n.alpha(now)` (the fade). The tone decides the badge: filled accent for the clickable
/// "Update ready", the live cursor colour for the level-up flourish, a hollow ring for the
/// quiet background statuses.
pub(crate) fn notice_tray(
    n: &TransientNotice,
    g: &SettingsGeom,
    theme: Theme,
    cursor: [u8; 3],
    now: Instant,
    motion: f32,
    clear_rows: f32,
) -> TrayInput {
    let r = Roles::from_theme(theme);
    let p = layout(n, g, now, motion, clear_rows);
    let a = n.alpha(now);
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
    // last. The old single hard-edged offset rect read as a dark halo stuck to the pill;
    // stacking approximates a falloff, which is what a shadow looks like. Kept inside
    // SHADOW_MARGIN so the compositor's paint region always covers it.
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
    // Body: an elevated surface, near-opaque so the caption never fights the terminal
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
    // A HAIRLINE rim in the separator role — enough to seat the card against a
    // same-luminance background, without the full-perimeter accent ring the old pill wore
    // (that ring shouted, and it shouted the same for "ready" and "paused" alike).
    prims.push(DrawPrim::Stroke {
        x,
        y,
        w,
        h,
        radius,
        width: 1.0,
        color: rgba(r.separator, sa(0x99)),
    });

    // The badge: the pictogram lives HERE, not as the first letter of the sentence. (The
    // old pill accent-coloured the caption's first CHARACTER, which on an unmarked
    // caption tinted a random letter — "U" in "Update stopped safely".)
    let badge = legible_on(p.tone.color(&r, cursor), r.elevated);
    let glyph_color = if p.tone.filled() {
        prims.push(DrawPrim::Dot {
            cx: p.badge_cx,
            cy: p.badge_cy,
            r: p.badge_r,
            color: rgba(badge, sa(0xFF)),
            breathe: false,
        });
        on_fill(badge)
    } else {
        prims.push(DrawPrim::Stroke {
            x: p.badge_cx - p.badge_r,
            y: p.badge_cy - p.badge_r,
            w: 2.0 * p.badge_r,
            h: 2.0 * p.badge_r,
            radius: p.badge_r,
            width: 1.2,
            color: rgba(badge, sa(0xCC)),
        });
        badge
    };
    // The pictogram keeps the MONO face: it is glyph art (↑ ✦ ✓ ⇣), and the mono stack is
    // the one with the DejaVu coverage fallback behind it. The words take the native UI
    // face like every other piece of chrome.
    prims.push(text_prim(
        p.badge_cx - text_w(&p.marker, p.glyph_px.get()) * 0.5,
        baseline_centered_at(p.badge_cy, p.glyph_px.get()),
        p.marker.clone(),
        p.glyph_px,
        TextWeight::Regular,
        TextFace::Mono,
        rgba(glyph_color, sa(0xFF)),
    ));
    if !p.title.is_empty() {
        prims.push(text_prim(
            p.title_x,
            p.baseline,
            p.title.clone(),
            p.size,
            TextWeight::Bold,
            TextFace::UiBold,
            rgba(r.text_primary, sa(0xFF)),
        ));
    }
    if let Some((detail, dx)) = p.detail.as_ref() {
        prims.push(text_prim(
            *dx,
            p.baseline,
            detail.clone(),
            p.size,
            TextWeight::Regular,
            TextFace::Ui,
            rgba(r.text_secondary, sa(0xE6)),
        ));
    }
    // The chevron: two round-capped strokes, the standard "this row goes somewhere" mark.
    // Only the clickable notice gets one, so the affordance is honest.
    if let Some(cx) = p.chevron_cx {
        let arm = p.size.get() * 0.28;
        let cy = p.badge_cy;
        let color = rgba(r.text_tertiary, sa(0xFF));
        prims.push(DrawPrim::Line {
            x1: cx - arm * 0.5,
            y1: cy - arm,
            x2: cx + arm * 0.5,
            y2: cy,
            width: 1.5,
            color,
        });
        prims.push(DrawPrim::Line {
            x1: cx + arm * 0.5,
            y1: cy,
            x2: cx - arm * 0.5,
            y2: cy + arm,
            width: 1.5,
            color,
        });
    }

    TrayInput {
        prims,
        card: (x, y, w, h),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shadow must stay inside [`SHADOW_MARGIN`], because the compositor grows the
    /// notice's paint region by exactly that and no more — anything beyond is CROPPED,
    /// silently, with the card still looking fine in isolation.
    ///
    /// The loop's comment asserted this in prose for its whole life while the spreads sat
    /// in an unchecked literal. Deriving the reach from the same table the renderer walks
    /// is what makes the claim answerable.
    #[test]
    fn shadow_stays_inside_its_margin() {
        // Each layer is drawn at (-spread, -spread + dy) with size (w + 2*spread,
        // h + 2*spread), so relative to the card it reaches `spread` left/right,
        // `spread - dy` above, and `spread + dy` below. Downward is always the furthest
        // because the shadow is offset down; take the max over every direction anyway, so
        // the test keeps holding if a layer is ever given a negative dy.
        let reach = SHADOW_LAYERS
            .iter()
            .fold(0.0_f32, |worst, &(spread, dy, _)| {
                worst.max(spread).max(spread + dy).max(spread - dy)
            });
        assert!(
            reach <= SHADOW_MARGIN,
            "the shadow reaches {reach}px but the compositor only pads {SHADOW_MARGIN}px, \
             so the outermost layer is cropped: shrink the spread or raise SHADOW_MARGIN \
             (and splice_notice's paint region with it)"
        );
        // Non-vacuity: an empty table would satisfy the bound while proving nothing.
        assert!(
            !SHADOW_LAYERS.is_empty(),
            "no layers means nothing was checked"
        );
    }

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

    /// Measure a painted run with the metric its OWN face uses — the mono pictogram and
    /// the proportional words do not share a width function.
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

    #[test]
    fn alpha_ramps_in_holds_then_ramps_out() {
        let now = t0();
        let n = TransientNotice::update_ready("0.5.15".into(), 830, now);
        // The entrance is a real ramp, not a pop.
        assert_eq!(n.alpha(now), 0.0, "invisible at spawn");
        let opening = n.alpha(now + ENTER / 2);
        assert!(
            opening > 0.0 && opening < 1.0,
            "mid-entrance alpha {opening}"
        );
        assert_eq!(n.alpha(now + ENTER), 1.0, "fully in once the entrance ends");
        // Just before the exit tail: still full.
        assert_eq!(n.alpha(now + (TTL - FADE) - Duration::from_millis(1)), 1.0);
        // Deep in the exit: below full, above zero.
        let mid = n.alpha(now + TTL - FADE / 2);
        assert!(mid > 0.0 && mid < 1.0, "mid-fade alpha {mid}");
        // At/after TTL: gone.
        assert_eq!(n.alpha(now + TTL), 0.0);
        assert!(n.is_expired(now + TTL));
        assert!(!n.is_expired(now));
    }

    /// The card must come from ABOVE and leave upward, and be exactly at rest for the
    /// whole hold — a notice that drifts while you read it is worse than one that never
    /// moved.
    #[test]
    fn rise_travels_in_settles_and_lifts_away() {
        let now = t0();
        let n = TransientNotice::level_up(830, now);
        assert_eq!(n.rise(now), -1.0, "starts a full slide above rest");
        assert!(n.rise(now + ENTER / 2) < 0.0);
        assert_eq!(n.rise(now + ENTER), 0.0, "settled when the entrance ends");
        assert_eq!(
            n.rise(now + TTL - FADE),
            0.0,
            "still at rest at the exit boundary"
        );
        assert!(n.rise(now + TTL - FADE / 2) < 0.0, "lifts away on exit");
        // The whole travel stays within one slide in each direction.
        for ms in (0..TTL.as_millis() as u64).step_by(17) {
            let r = n.rise(now + Duration::from_millis(ms));
            assert!((-1.0..=0.0).contains(&r), "rise {r} out of range at {ms}ms");
        }
    }

    /// Reduced motion keeps the information and removes the movement: the card is pinned
    /// to its rest position for its whole life, and only the alpha ramp remains.
    #[test]
    fn reduced_motion_pins_the_card_to_its_rest_position() {
        let now = t0();
        let n = TransientNotice::update_ready("0.5.15".into(), 830, now);
        let g = geom();
        let rest = REST_Y_CELLS * g.ch;
        for ms in [0_u64, 100, 1_000, 5_000] {
            let (_, y, _, _) = notice_rect(&n, &g, now + Duration::from_millis(ms), 0.0, 0.0);
            assert!(
                (y - rest).abs() < f32::EPSILON,
                "pinned at {ms}ms: {y} != {rest}"
            );
        }
        // …and with motion allowed it genuinely moves, so the test above is not vacuous.
        let (_, moving, _, _) = notice_rect(&n, &g, now, 1.0, 0.0);
        assert!(moving < rest, "the entrance really does displace the card");
    }

    #[test]
    fn fingerprint_zeroes_never_and_changes_across_the_motion() {
        let now = t0();
        let n = TransientNotice::level_up(830, now);
        assert_ne!(n.fingerprint(now), 0);
        // The hold is stable; both ramps change the fingerprint.
        let hold_a = n.fingerprint(now + ENTER + Duration::from_millis(50));
        let hold_b = n.fingerprint(now + ENTER + Duration::from_millis(250));
        assert_eq!(hold_a, hold_b, "stable during hold");
        let enter_a = n.fingerprint(now + Duration::from_millis(20));
        let enter_b = n.fingerprint(now + Duration::from_millis(120));
        assert_ne!(enter_a, enter_b, "changes across the entrance");
        let fade_a = n.fingerprint(now + TTL - FADE + Duration::from_millis(100));
        let fade_b = n.fingerprint(now + TTL - FADE + Duration::from_millis(500));
        assert_ne!(fade_a, fade_b, "changes across the exit");
    }

    /// Every animation step the 60fps cadence can deliver must survive quantization, or
    /// the card would visibly step instead of glide.
    #[test]
    fn every_frame_of_the_ramps_is_a_distinct_fingerprint_step() {
        let now = t0();
        let n = TransientNotice::update_ready("0.5.15".into(), 830, now);
        let sample = |ms: u64| n.fingerprint(now + Duration::from_millis(ms));
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
        let n = TransientNotice::update_ready("0.5.15".into(), 830, now);
        assert_eq!(n.deadline(now), now + FRAME, "entrance animates");
        let held = now + ENTER + Duration::from_millis(100);
        assert_eq!(
            n.deadline(held),
            now + (TTL - FADE),
            "the hold sleeps to the exit boundary"
        );
        let leaving = now + TTL - FADE / 2;
        assert_eq!(n.deadline(leaving), leaving + FRAME, "the exit animates");
    }

    #[test]
    fn update_ready_is_clickable_level_up_is_not() {
        let now = t0();
        assert!(TransientNotice::update_ready("0.5.15".into(), 830, now).is_update_ready());
        assert!(!TransientNotice::level_up(830, now).is_update_ready());
        assert!(
            !TransientNotice::update_status("Update paused", now).is_update_ready(),
            "automatic status notices never trigger another apply attempt"
        );
    }

    /// The authored grammar, and every way a caption can miss it.
    #[test]
    fn caption_parts_splits_the_authored_grammar_and_degrades_honestly() {
        let p = caption_parts("\u{2191} Update ready \u{2014} v0.5.15");
        assert_eq!(p.marker.as_deref(), Some("\u{2191}"));
        assert_eq!(p.title, "Update ready");
        assert_eq!(p.detail.as_deref(), Some("v0.5.15"));
        // The roster form uses a colon.
        let p = caption_parts("\u{2713} ALab toolchain installed: ay, trust");
        assert_eq!(p.marker.as_deref(), Some("\u{2713}"));
        assert_eq!(p.title, "ALab toolchain installed");
        assert_eq!(p.detail.as_deref(), Some("ay, trust"));
        // No marker: the whole thing is still split, and paint supplies an attention mark.
        let p = caption_parts("Update needs attention \u{2014} open the Version menu");
        assert_eq!(p.marker, None);
        assert_eq!(p.title, "Update needs attention");
        assert_eq!(p.detail.as_deref(), Some("open the Version menu"));
        // No separator at all: all title, no invented detail.
        let p = caption_parts("Update paused");
        assert_eq!(p.title, "Update paused");
        assert_eq!(p.detail, None);
        // A dash inside the detail stays in the detail (first separator wins).
        let p = caption_parts("A \u{2014} b \u{2014} c");
        assert_eq!(p.title, "A");
        assert_eq!(p.detail.as_deref(), Some("b \u{2014} c"));
        // Degenerate input never panics and never fabricates prose.
        assert_eq!(caption_parts("").title, "");
        assert_eq!(caption_parts("").marker, None);
        // A caption that is ONLY a pictogram is all badge and no sentence.
        let p = caption_parts("\u{2191} ");
        assert_eq!(p.marker.as_deref(), Some("\u{2191}"));
        assert_eq!(p.title, "");
        assert_eq!(p.detail, None);
        // …but ordinary punctuation opening a sentence is NOT a marker: it belongs to the
        // words, and stealing it would silently edit the caption.
        let p = caption_parts("\"columns\" applies on next launch");
        assert_eq!(p.marker, None);
        assert_eq!(p.title, "\"columns\" applies on next launch");
    }

    #[test]
    fn tray_paints_badge_title_and_detail_inside_the_card() {
        let now = t0();
        let n = TransientNotice::update_ready("0.5.15".into(), 830, now + Duration::ZERO);
        let g = geom();
        let at = now + ENTER; // settled, so the rect is the rest rect
        let t = notice_tray(&n, &g, Theme::default(), [0, 255, 0], at, 1.0, 0.0);
        let painted = texts(&t);
        assert!(
            painted.iter().any(|(s, _, _)| s == "Update ready"),
            "the title is painted as its own run: {painted:?}"
        );
        assert!(
            painted
                .iter()
                .any(|(s, _, _)| s.contains("0.5.15") || s.contains("830")),
            "the detail is painted as its own run: {painted:?}"
        );
        assert!(
            painted.iter().any(|(s, _, _)| s == "\u{2191}"),
            "the marker is painted as the badge pictogram, not as part of the sentence"
        );
        let (x, _, w, _) = notice_rect(&n, &g, at, 1.0, 0.0);
        let tray_w = g.cols as f32 * g.cw;
        assert!(x >= 0.0 && x + w <= tray_w, "the card fits within the tray");
        for (s, tx, tw) in &painted {
            assert!(*tx >= x, "{s:?} starts inside the card");
            assert!(*tx + *tw <= x + w + 0.5, "{s:?} ends inside the card");
        }
        // The clickable notice wears a chevron; the quiet ones must not.
        let lines = t
            .prims
            .iter()
            .filter(|p| matches!(p, DrawPrim::Line { .. }))
            .count();
        assert_eq!(lines, 2, "the actionable notice gets a chevron");
        let quiet =
            TransientNotice::update_status("\u{2191} Update paused \u{2014} see Version menu", at);
        let qt = notice_tray(
            &quiet,
            &g,
            Theme::default(),
            [0, 255, 0],
            at + ENTER,
            1.0,
            0.0,
        );
        assert_eq!(
            qt.prims
                .iter()
                .filter(|p| matches!(p, DrawPrim::Line { .. }))
                .count(),
            0,
            "a non-clickable status must not advertise a click"
        );
    }

    /// A LONG STATUS MUST STILL BE READABLE ON A NARROW WINDOW.
    ///
    /// The card is clamped to the tray, so a caption that cannot fit has to LOSE
    /// something. It loses the qualifier first and the state last: the automatic-update
    /// statuses are the longest captions this widget carries, and "Update waiting" is the
    /// part the user cannot do without.
    #[test]
    fn a_long_status_drops_its_detail_before_it_elides_its_title() {
        let now = t0();
        let long = "\u{2191} Update waiting \u{2014} retries on its own, or use the Version menu";
        // 20 columns is the narrow end this widget has to survive.
        let narrow = SettingsGeom { cols: 20, ..geom() };
        let n = TransientNotice::update_status(long, now);
        let at = now + ENTER;
        let size = TypeStep::Secondary.px_clamped(narrow.font_px, 12.0, f32::INFINITY);
        // PRECONDITION: the full caption genuinely does NOT fit, so a pass below is about
        // the fit rule and not about a caption that was short.
        let inner = (narrow.cols as f32 * narrow.cw) - 2.0 * narrow.cw;
        assert!(
            ui_text_width_for(TextFace::Ui, long, size.get()) > inner,
            "the fixture caption must overflow a 20-column tray or this proves nothing"
        );

        let (x, _, w, _) = notice_rect(&n, &narrow, at, 1.0, 0.0);
        let t = notice_tray(&n, &narrow, Theme::default(), [0, 255, 0], at, 1.0, 0.0);
        let painted = texts(&t);
        assert!(!painted.is_empty(), "something is still painted");
        for (s, tx, tw) in &painted {
            assert!(*tx >= x, "{s:?} starts inside the card ({tx} < {x})");
            assert!(*tx + *tw <= x + w + 0.5, "{s:?} ends inside the card");
        }
        // The pictogram survives whatever else is dropped — it is the badge, and the badge
        // is never the thing that gets cut.
        assert!(
            painted.iter().any(|(s, _, _)| s.contains('\u{2191}')),
            "the marker pictogram is still painted"
        );
        // The state survives; the qualifier is what was dropped.
        assert!(
            painted.iter().any(|(s, _, _)| s.starts_with("Update")),
            "the title survives: {painted:?}"
        );
        assert!(
            !painted.iter().any(|(s, _, _)| s.contains("Version menu")),
            "the qualifier was dropped rather than overflowing: {painted:?}"
        );
    }

    /// The card must never paint outside the tray, at ANY width — including widths so
    /// small that nothing fits at all.
    #[test]
    fn the_card_stays_inside_even_absurdly_narrow_trays() {
        let now = t0();
        let n = TransientNotice::update_ready("0.5.15".into(), 830, now);
        let mut saw_a_card = false;
        for cols in [1_usize, 2, 3, 5, 8, 13, 21, 40, 200] {
            let g = SettingsGeom { cols, ..geom() };
            let at = now + ENTER;
            let (x, y, w, h) = notice_rect(&n, &g, at, 1.0, 0.0);
            let tray_w = cols as f32 * g.cw;
            assert!(x >= 0.0 && w >= 0.0 && h > 0.0, "sane rect at {cols} cols");
            assert!(
                x + w <= tray_w + 0.5,
                "fits at {cols} cols: {x}+{w} > {tray_w}"
            );
            assert!(y > 0.0, "the card never rides off the top at {cols} cols");
            let t = notice_tray(&n, &g, Theme::default(), [0, 255, 0], at, 1.0, 0.0);
            saw_a_card |= !t.prims.is_empty();
            // Below the width that fits a badge the widget paints NOTHING — never a
            // caption hanging off a clamped zero-width card.
            if w <= 0.0 {
                assert!(
                    t.prims.is_empty(),
                    "a card too narrow to draw paints nothing"
                );
            }
            for (s, tx, tw) in texts(&t) {
                assert!(
                    tx + tw <= x + w + 0.5,
                    "{s:?} overflows the card at {cols} cols"
                );
            }
        }
        assert!(
            saw_a_card,
            "the wide end still paints — the loop is not vacuous"
        );
    }

    /// THE CARD MUST NOT FLOAT OVER IN-GRID CHROME. The tab strip lives in the first
    /// `tab_strip_rows` grid rows, and the mouse path tests the pill BEFORE the strip: a
    /// card overlapping the strip both hides the tabs and eats (or, for a non-clickable
    /// status, passes through and misroutes) clicks meant for them.
    #[test]
    fn the_card_clears_the_in_grid_chrome_it_is_given() {
        let now = t0();
        let n = TransientNotice::update_ready("0.5.15".into(), 830, now);
        let g = geom();
        let at = now + ENTER;
        for clear_rows in [0.0_f32, 1.0, 2.0, 3.0] {
            let (_, y, _, h) = notice_rect(&n, &g, at, 1.0, clear_rows);
            assert!(
                y >= clear_rows * g.ch,
                "card top {y} rides into the {clear_rows} reserved chrome rows"
            );
            // Even mid-entrance, when the card is at its highest, it stays clear.
            let (_, y_in, _, _) = notice_rect(&n, &g, now, 1.0, clear_rows);
            assert!(
                y_in >= clear_rows * g.ch,
                "the entrance overshoots into chrome at {clear_rows} rows: {y_in}"
            );
            assert!(h > 0.0);
        }
    }

    /// The celebration badge takes the LIVE CURSOR COLOUR, which the user owns — including
    /// colours that match the card surface. The disc must still be visible.
    #[test]
    fn a_badge_is_conditioned_until_it_can_be_seen_on_the_card() {
        let surface = [0x1E, 0x20, 0x24];
        // A cursor colour that all but matches the surface is pushed until it separates.
        let near = [0x22, 0x24, 0x28];
        let fixed = legible_on(near, surface);
        assert!(
            (luma(fixed) - luma(surface)).abs() >= BADGE_CONTRAST,
            "{fixed:?} still blends into {surface:?}"
        );
        // A colour that already separates is returned UNTOUCHED — the user's hue survives.
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

    #[test]
    fn elision_is_a_no_op_when_the_text_already_fits() {
        assert_eq!(elided("Ready", 10_000.0, 12.0), "Ready");
        // Degenerate widths never panic and never return junk.
        assert_eq!(elided("Ready", 0.0, 12.0), "");
        assert_eq!(elided("Ready", -5.0, 12.0), "");
    }

    /// The badge tone IS the affordance: only the clickable notice wears the filled
    /// accent, and a background status stays quiet however its caption is marked.
    #[test]
    fn tone_matches_what_the_notice_can_actually_do() {
        let ready = NoticeKind::UpdateReady {
            version: "1".into(),
            build: 1,
        };
        assert_eq!(Tone::of(&ready, Some("\u{2191}")), Tone::Action);
        assert!(Tone::of(&ready, Some("\u{2191}")).filled());
        assert_eq!(
            Tone::of(&NoticeKind::LevelUp { build: 1 }, None),
            Tone::Celebrate
        );
        let status = NoticeKind::UpdateStatus {
            text: "\u{2191} Update paused".into(),
        };
        assert_eq!(
            Tone::of(&status, Some("\u{2191}")),
            Tone::Quiet,
            "an up-arrow does not make a status actionable"
        );
        assert!(!Tone::of(&status, Some("\u{2191}")).filled());
        assert_eq!(Tone::of(&status, Some("\u{2713}")), Tone::Success);
    }

    /// The pictogram sits on a themed disc whose luminance is not knowable ahead of
    /// time, so its colour is COMPUTED. Both branches must be reachable and legible.
    #[test]
    fn badge_glyph_contrasts_with_every_badge_fill() {
        assert_eq!(on_fill([0xFF, 0xFF, 0xFF]), [0x10, 0x12, 0x16]);
        assert_eq!(on_fill([0x10, 0x10, 0x10]), [0xFF, 0xFF, 0xFF]);
        // A mid accent still resolves to the higher-contrast side of the pair.
        for fill in [[0x2F, 0x6F, 0xED], [0xF5, 0xC2, 0x42], [0x00, 0xC8, 0x77]] {
            let on = on_fill(fill);
            let l = |c: [u8; 3]| {
                0.2126 * f32::from(c[0]) + 0.7152 * f32::from(c[1]) + 0.0722 * f32::from(c[2])
            };
            assert!(
                (l(on) - l(fill)).abs() > 60.0,
                "{fill:?} on {on:?} is too close to read"
            );
        }
    }
}
