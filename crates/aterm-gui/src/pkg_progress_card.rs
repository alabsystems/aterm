// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The TOOLCHAIN-PROVISIONING PROGRESS CARD (design §6): a floating [`DrawPrim`]
//! card that shows `atpkg`'s live install pass — overall rainbow bar, one row per
//! program with an honest phase label, sparkles on completions, and the cursor
//! kitty (the owner's "cursor kitty" is `kitty_pet.rs`'s cat — never
//! `kitty_cursor.rs`'s head) riding the bar's leading edge.
//!
//! # Where the data comes from, and what it may claim
//!
//! The one input is WP3's [`crate::PkgProgressSnapshot`] — the classified read of
//! `<prefix>/progress.json` (size-capped, symlink-refusing, staleness-lawed). The
//! card renders ONLY what the snapshot supports:
//!
//! * `running == false` ⇒ **not-running states only** (design §3): a stopped pass
//!   says "stopped" and names the next act; it never animates and never claims a
//!   live phase. A dead installer's file can never claim live progress here.
//! * an unknown `v` ⇒ one generic "Installing…" line — never a guess at fields
//!   whose meaning may have changed.
//! * program names are UNTRUSTED until they round-trip
//!   [`atpkg::store::ToolName`]; error strings are control-stripped and
//!   length-capped ([`atpkg::progress::sanitize_for_tty`]) before they become
//!   glyphs. A name that fails the gate simply has no row.
//!
//! # The slot, and the even/odd fingerprint contract
//!
//! The paint-only chrome cards share ONE composited tray-quad slot per window
//! (`settings → level_up → notice → badge`, see `WindowState::present_card`).
//! Under the WP fences this module may not grow `WindowState` a field, so the
//! progress card RIDES THE NOTICE SLOT (`ws.notice_card`) whenever no transient
//! notice is up — the same "briefly replaces the badge" arrangement the pill
//! already has with the build badge, one rung further down. Ownership inside the
//! shared slot is carried by fingerprint PARITY:
//!
//! * a notice pill's fingerprint is **odd** by construction
//!   (`TransientNotice::fingerprint` ends `| 1`);
//! * this card's slot fingerprint is **even** and nonzero by construction
//!   ([`slot_fp`]).
//!
//! `splice_notice`'s no-notice arm spares an even-fp resident, and
//! [`owns_slot`] answers "is that card ours" for the dismiss hit-test — the
//! `slot_fp_is_even_and_notice_fp_is_odd` test pins both halves of the contract.
//!
//! # Structure
//!
//! [`pkg_progress_tray`] is a PURE layout: snapshot + geometry + chrome + a
//! seconds clock in, [`DrawPrim`]s out, rasterized once per (fp, geom) by
//! `App::splice_pkg_progress` exactly like `splice_notice` (paint region grown by
//! [`crate::notice::SHADOW_MARGIN`] — the cropped-shadow bug documented there).
//! The cat is the one thing `DrawPrim` cannot say (no image variant): it is baked
//! to exact-size RGBA via `aterm_effects::pet_baker` and ALPHA-BLITTED onto the
//! card raster after `rasterize_tray` ([`blit_cat`]).
//!
//! # Motion, honestly
//!
//! `chrome.amp` is `MotionPolicy::amplitude(MotionEffect::PkgProgressCard)`: at 0
//! the bar still snaps to every new value (it is information, the scroll-pill
//! rule) but the hue holds, sparkles do not twinkle, and the cat stands still.
//! `chrome.fancy` folds the `pkg_progress_effects` opt-out and serious mode
//! (`SeriousEffect::PkgProgressFx`): off ⇒ a plain themed accent capsule with
//! text rows, fully functional. The card never costs idle frames: all time-driven
//! decoration is gated by the WP3 deadline (visible AND animating), and the
//! decorative state below ([`advance_fx`]) only ever runs inside a splice that a
//! wake already paid for.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use aterm_effects::cat_baker::CatColorKey;
use aterm_effects::pet_baker::{PetBakeKey, PetBaker};
use aterm_effects::pet_glyphs_gen::PetGlyphId;
use aterm_render::Theme;

use crate::notice::{
    RAINBOW_TURNS_PER_SEC, SHADOW_LAYERS, SPARKLE_MAX_R, SPARKLE_MIN_R, legible_on,
    perimeter_point, rainbow,
};
use crate::settings::{Roles, SettingsGeom, text_w};
use crate::tray_raster::{row_baseline, ui_text_width_for};
use crate::type_scale::TypeStep;
use crate::widget::{DrawPrim, TextFace, TextWeight, TrayInput, rgba, text_prim};

use atpkg::progress::{PROGRESS_VERSION, Phase, sanitize_for_tty};

/// How long a per-row completion sparkle burst lives.
const BURST: Duration = Duration::from_millis(900);
/// How long the 100% celebration sweep runs.
const CELEBRATE: Duration = Duration::from_millis(2200);
/// How long a cleanly-ENDED pass's card holds after its celebration before it
/// retires (via the per-pass dismissal — a new pass re-shows). A STOPPED pass
/// (dead installer, no clean end) deliberately never auto-retires: it names a
/// next act and needs the user, so it stays until dismissed.
const RETIRE_HOLD: Duration = Duration::from_millis(5400);
/// How long a queue reorder (a bump jumping a row to the front) animates.
const REORDER: Duration = Duration::from_millis(280);
/// How long after the last observed overall-byte movement the cat keeps
/// walking. Mirrors WP3's `PKG_PROGRESS_ACTIVE_WINDOW` intent: no bytes, no
/// travel — the cat stands, honestly.
const CAT_MOVE_WINDOW: Duration = Duration::from_secs(1);
/// Most rows the card lists before folding the tail into "+ k more".
const MAX_ROWS: usize = 10;
/// Sparkles in a per-row burst ring / the celebration sweep.
const BURST_SPARKLES: usize = 10;
const CELEBRATE_SPARKLES: usize = 14;
/// Rainbow segments across the overall bar — enough that the fill reads as a
/// gradient, few enough that the clip-per-segment raster stays cheap.
const RAINBOW_SEGMENTS: usize = 24;

/// The look inputs the card needs, grouped like [`crate::notice::NoticeChrome`].
pub(crate) struct CardChrome {
    pub(crate) theme: Theme,
    /// `MotionPolicy::amplitude(MotionEffect::PkgProgressCard)` — 0 freezes
    /// every time-driven decoration while the information keeps updating.
    pub(crate) amp: f32,
    /// Party trim at all: `pkg_progress_effects` config AND serious mode's
    /// `SeriousEffect::PkgProgressFx`. `false` ⇒ plain themed accent capsule.
    pub(crate) fancy: bool,
    /// The worn cursor-kitty identity (coat/iris ramp indices from the user's
    /// `KittyLook`), so the cat on the bar is THEIR cat.
    pub(crate) coat: u8,
    pub(crate) iris: u8,
}

/// Where (in tray px) and how the cat is drawn — the blit spec the splice feeds
/// to [`blit_cat`] after rasterizing, translated into raster-local coordinates.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CatPlace {
    /// The cat's horizontal center — the overall bar's leading edge.
    pub(crate) cx: f32,
    /// The cat's feet line (bottom of the sprite) — it stands ON the bar.
    pub(crate) bottom: f32,
    /// Sprite height in px; width follows the pose's authored aspect.
    pub(crate) h: f32,
    pub(crate) pose: PetGlyphId,
    /// Sprites are authored facing right; a shrinking bar walks it back left.
    pub(crate) facing_left: bool,
}

/// One built card: the prims + the cat spec (the one non-`DrawPrim` layer).
pub(crate) struct CardTray {
    pub(crate) tray: TrayInput,
    pub(crate) cat: Option<CatPlace>,
}

/// The DECORATIVE time-state view [`pkg_progress_tray`] consumes — pure data, so
/// layout stays a function. Produced by [`advance_fx`] in the splice; tests
/// build it directly ([`FxView::still`]).
#[derive(Debug, Clone, Default)]
pub(crate) struct FxView {
    /// Seconds since this pass's card first appeared — the hue/twinkle clock.
    pub(crate) t: f32,
    /// Live completion bursts: (program, progress 0..1).
    pub(crate) bursts: Vec<(String, f32)>,
    /// The 100% celebration sweep's progress 0..1, while it runs.
    pub(crate) celebrate: Option<f32>,
    /// Rows still sliding from a queue reorder: (program, previous display
    /// index), interpolated by [`Self::reorder_t`].
    pub(crate) reorder_from: Vec<(String, usize)>,
    /// Eased 0..1 progress of the current reorder slide.
    pub(crate) reorder_t: f32,
    /// The cat's scripted pose for this frame.
    pub(crate) cat_pose: Option<PetGlyphId>,
    pub(crate) cat_facing_left: bool,
    /// 0..1 bob phase input; layout scales it by the motion amplitude.
    pub(crate) cat_bob: f32,
}

impl FxView {
    /// A motionless view (t = 0, no bursts, cat standing) — the test fixture.
    /// Production always derives its view from [`advance_fx`], so this is
    /// test-only by construction, not by neglect.
    #[cfg(test)]
    pub(crate) fn still() -> Self {
        Self {
            cat_pose: Some(PetGlyphId::PetStand),
            ..Self::default()
        }
    }
}

// ---------------------------------------------------------------------------
// Decorative state (module-held).
// ---------------------------------------------------------------------------

/// What one [`advance_fx`] step tells the splice to DO — the two levers WP3
/// exposed on `PkgProgressUi`, surfaced as data so this module stays free of
/// `App` borrows.
pub(crate) struct FxOut {
    pub(crate) view: FxView,
    /// Forward to `PkgProgressUi::note_fx`: decoration (burst / reorder /
    /// celebration / retire hold) needs frames until this instant.
    pub(crate) hold_fx_until: Option<Instant>,
    /// The cleanly-ended pass's hold elapsed: dismiss the card (per-pass — a
    /// new pass re-shows it).
    pub(crate) retire: bool,
}

/// The decoration's memory between frames: which completions already burst,
/// what the queue order was, when the pass ended. DECORATION ONLY — nothing
/// here feeds layout truth (phases, bytes, order all come from the snapshot
/// each frame), so its worst failure mode is a missed sparkle.
///
/// Held in a `thread_local` because the WP fences bar this package from
/// growing `App`/`WindowState` (both live in `lib.rs`, WP3's file): the event
/// loop is one thread, and the state is keyed by pass identity, so the cell is
/// effectively "the current pass's scratch". Tests on other threads get their
/// own empty cell.
#[derive(Default)]
struct CardFx {
    /// (pass, started_unix) — a new identity resets everything below.
    pass_id: Option<(String, u64)>,
    origin: Option<Instant>,
    prev_phase: BTreeMap<String, Phase>,
    bursts: Vec<(String, Instant)>,
    celebrate_started: Option<Instant>,
    ended_seen: Option<Instant>,
    /// The retire hold already fired for this pass — a ONE-SHOT: without it,
    /// the Settings ▸ Packages reopen of an ended pass's card would be
    /// re-dismissed on its very next frame (the hold stays elapsed forever),
    /// making the reopen affordance a one-frame flash.
    retired: bool,
    prev_order: Vec<String>,
    reorder_started: Option<Instant>,
    reorder_from: Vec<(String, usize)>,
    /// Overall bytes at the last step + when they last moved — the cat's
    /// walk/stand decision.
    last_bytes: Option<u64>,
    last_moved: Option<Instant>,
    prev_lead: f32,
    facing_left: bool,
    /// Per-window "was Settings ▸ Packages front last frame" — the reopen
    /// affordance's edge detector (law in `crate::packages_screen`).
    packages_front: BTreeMap<u64, bool>,
    /// One-entry bake memo so the cat is not re-rasterized at an unchanged
    /// (pose, colours, size) — see [`blit_cat`]. Stored as raw (w, h, RGBA)
    /// because `aterm_scene::Tile` is neither a gui dependency nor `Clone`.
    baked: Option<(PetBakeKey, u32, u32, Vec<u8>)>,
}

thread_local! {
    static FX: RefCell<CardFx> = RefCell::new(CardFx::default());
}

/// `t^2·(3−2t)` — the classic smoothstep, for the reorder slide.
fn smooth(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Record whether Settings ▸ Packages is the front content of window `wid` and
/// report the NOT-FRONT → FRONT edge — the moment the reopen affordance fires
/// (`crate::packages_screen::reopen_on_packages_visit` is the law; this is its
/// per-window memory).
pub(crate) fn note_packages_front(wid: u64, front: bool) -> bool {
    FX.with(|fx| {
        let mut fx = fx.borrow_mut();
        let was = fx.packages_front.insert(wid, front).unwrap_or(false);
        crate::packages_screen::reopen_on_packages_visit(was, front)
    })
}

/// Advance the decorative state one frame against `snap` and hand back the pure
/// view plus the two `PkgProgressUi` levers. Runs only inside a splice — a wake
/// something else already scheduled — so it can never be a frame source itself.
pub(crate) fn advance_fx(snap: &crate::PkgProgressSnapshot, now: Instant) -> FxOut {
    FX.with(|fx| {
        let mut fx = fx.borrow_mut();
        let id = (snap.file.pass.clone(), snap.file.started_unix);
        if fx.pass_id.as_ref() != Some(&id) {
            let keep_front = std::mem::take(&mut fx.packages_front);
            *fx = CardFx {
                pass_id: Some(id),
                origin: Some(now),
                packages_front: keep_front,
                ..CardFx::default()
            };
        }
        let origin = *fx.origin.get_or_insert(now);
        let t = now.duration_since(origin).as_secs_f32();

        let mut hold: Option<Instant> = None;
        let mut latch = |until: Instant| {
            hold = Some(hold.map_or(until, |h| h.max(until)));
        };

        // Completion bursts: a row newly Done sparkles once.
        for (name, row) in &snap.file.programs {
            let prev = fx.prev_phase.insert(name.clone(), row.phase);
            if row.phase == Phase::Done && prev.is_some_and(|p| p != Phase::Done) {
                fx.bursts.push((name.clone(), now));
            }
        }
        fx.bursts
            .retain(|(_, started)| now.duration_since(*started) < BURST);
        let bursts: Vec<(String, f32)> = fx
            .bursts
            .iter()
            .map(|(n, started)| {
                (
                    n.clone(),
                    (now.duration_since(*started).as_secs_f32() / BURST.as_secs_f32())
                        .clamp(0.0, 1.0),
                )
            })
            .collect();
        if let Some((_, started)) = fx.bursts.last() {
            latch(*started + BURST);
        }

        // Queue reorder (a bumped row jumping forward): remember where each row
        // WAS so layout can slide it to where it now IS.
        let order = display_order(snap);
        if order != fx.prev_order {
            fx.reorder_from = fx
                .prev_order
                .iter()
                .enumerate()
                .filter(|(_, n)| order.contains(*n))
                .map(|(i, n)| (n.clone(), i))
                .collect();
            fx.reorder_started = Some(now);
            fx.prev_order = order;
        }
        let reorder_t = fx.reorder_started.map_or(1.0, |started| {
            (now.duration_since(started).as_secs_f32() / REORDER.as_secs_f32()).clamp(0.0, 1.0)
        });
        if let Some(started) = fx.reorder_started {
            if reorder_t < 1.0 {
                latch(started + REORDER);
            } else {
                fx.reorder_started = None;
                fx.reorder_from.clear();
            }
        }

        // The terminal snapshot: celebration once, then the bounded retire hold.
        let ended = snap.file.ended_unix.is_some();
        let failed = snap
            .file
            .programs
            .values()
            .any(|r| r.phase == Phase::Failed);
        let mut celebrate = None;
        let mut retire = false;
        if ended {
            let seen = *fx.ended_seen.get_or_insert(now);
            if !failed && snap.file.overall.programs_total > 0 {
                let started = *fx.celebrate_started.get_or_insert(now);
                let age = now.duration_since(started).as_secs_f32() / CELEBRATE.as_secs_f32();
                if age < 1.0 {
                    celebrate = Some(age);
                }
            }
            // +200ms slack past the hold so the frame that crosses the boundary
            // is actually scheduled — the deadline stops the moment the card
            // dismisses, never one wake early. One-shot: after the hold has
            // retired the card once, a deliberate reopen STAYS (dismissed only
            // by a click or the next pass), so no fresh frames are latched.
            if !fx.retired {
                latch(seen + RETIRE_HOLD + Duration::from_millis(200));
                if now.duration_since(seen) >= RETIRE_HOLD {
                    fx.retired = true;
                    retire = true;
                }
            }
        }

        // The cat: walks only while overall bytes move, faces its travel.
        let bytes = snap.file.overall.bytes_done
            + snap
                .file
                .programs
                .values()
                .map(|r| r.bytes_done)
                .sum::<u64>();
        if snap.running && fx.last_bytes.is_some_and(|b| b != bytes) {
            fx.last_moved = Some(now);
        }
        fx.last_bytes = Some(bytes);
        let moving = snap.running
            && fx
                .last_moved
                .is_some_and(|at| now.duration_since(at) < CAT_MOVE_WINDOW);
        let lead = overall_frac(&snap.file);
        if (lead - fx.prev_lead).abs() > f32::EPSILON {
            fx.facing_left = lead < fx.prev_lead;
            fx.prev_lead = lead;
        }
        let done = snap.file.overall.programs_total > 0
            && snap.file.overall.programs_done >= snap.file.overall.programs_total;
        let cat_pose = if ended || done {
            PetGlyphId::PetSit
        } else if moving {
            // A little 4-frame walk cycle at ~6 fps — scripted, not PetBrain.
            const WALK: [PetGlyphId; 4] = [
                PetGlyphId::PetWalk0,
                PetGlyphId::PetWalk1,
                PetGlyphId::PetWalk2,
                PetGlyphId::PetWalk3,
            ];
            WALK[((t * 6.0) as usize) % WALK.len()]
        } else {
            PetGlyphId::PetStand
        };

        FxOut {
            view: FxView {
                t,
                bursts,
                celebrate,
                reorder_from: fx.reorder_from.clone(),
                reorder_t: smooth(reorder_t),
                cat_pose: Some(cat_pose),
                cat_facing_left: fx.facing_left,
                cat_bob: if moving { 1.0 } else { 0.0 },
            },
            hold_fx_until: hold,
            retire,
        }
    })
}

// ---------------------------------------------------------------------------
// The slot-ownership parity contract.
// ---------------------------------------------------------------------------

/// This card's slot fingerprint: EVEN and nonzero by construction, so it can
/// never be mistaken for a notice pill's (always odd — `fingerprint` ends
/// `| 1`) inside the shared `notice_card` slot. `content_fp` is WP3's
/// `PkgProgressUi::fingerprint`; `paint_key` folds every other paint input
/// (theme, roles, amplitude, fancy, cat look) so a stale raster can never be
/// reused across a look change.
pub(crate) fn slot_fp(content_fp: u64, paint_key: u64) -> u64 {
    let mut h = content_fp ^ 0x9e37_79b9_7f4a_7c15u64.wrapping_mul(paint_key | 1);
    h ^= h >> 33;
    h = h.wrapping_mul(0xff51_afd7_ed55_8ccd);
    // `| 1` guarantees nonzero before the shift; `<< 1` makes it even and
    // keeps that low set bit at bit 1, so the result is never 0.
    (h | 1) << 1
}

/// Whether a card resident in the shared notice slot is OURS (even fp) rather
/// than a notice pill's (odd fp). See the module doc's parity contract.
pub(crate) fn owns_slot(card: &crate::SettingsCard) -> bool {
    card.fp & 1 == 0
}

// ---------------------------------------------------------------------------
// The pure layout.
// ---------------------------------------------------------------------------

/// The overall pass fraction 0..1 — bytes when the pass has a byte total,
/// programs otherwise (the seed pass has no download bytes).
fn overall_frac(f: &atpkg::progress::ProgressFile) -> f32 {
    if f.overall.bytes_total > 0 {
        (f.overall.bytes_done as f32 / f.overall.bytes_total as f32).clamp(0.0, 1.0)
    } else if f.overall.programs_total > 0 {
        (f32::from(u16::try_from(f.overall.programs_done).unwrap_or(u16::MAX))
            / f32::from(u16::try_from(f.overall.programs_total).unwrap_or(u16::MAX)))
        .clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// `"4.1"` — one decimal of decimal megabytes, the design doc's own spelling.
fn mb(bytes: u64) -> String {
    format!("{:.1}", bytes as f64 / 1_000_000.0)
}

/// A display-safe rendering of an UNTRUSTED program name: admitted through the
/// `ToolName` gate (sensitive/malformed names have no row at all), then
/// control-stripped and capped like every string that becomes glyphs.
fn admitted_name(raw: &str) -> Option<String> {
    atpkg::store::ToolName::new(raw)?;
    Some(sanitize_for_tty(raw, 24))
}

/// One row's phase, spelled honestly. `queued_pos` is the row's 1-based place
/// in the remaining queue, when it is queued.
fn phase_label(row: &atpkg::progress::ProgramProgress, queued_pos: Option<usize>) -> String {
    match row.phase {
        Phase::Queued => match queued_pos {
            Some(p) => format!("queued ({p})"),
            None => "queued".to_string(),
        },
        Phase::Download => {
            if row.bytes_total > 0 {
                format!("{} of {} MB", mb(row.bytes_done), mb(row.bytes_total))
            } else {
                "downloading".to_string()
            }
        }
        // Label-only phases — they are not byte streams atpkg can meter, and
        // the card does not pretend otherwise (design §3).
        Phase::Verify => "verifying".to_string(),
        Phase::Extract => {
            if row.bytes_total > 0 {
                format!(
                    "extracting {} of {} MB",
                    mb(row.bytes_done),
                    mb(row.bytes_total)
                )
            } else {
                "extracting".to_string()
            }
        }
        Phase::Link => "linking".to_string(),
        Phase::Done => "installed".to_string(),
        Phase::Failed => match row.error.as_deref() {
            Some(e) => format!("failed — {}", sanitize_for_tty(e, 60)),
            None => "failed — see Settings ▸ Packages".to_string(),
        },
        Phase::Skipped => "already current".to_string(),
    }
}

/// The status pictogram for one phase (mono glyph art, like the notice badge).
const fn phase_glyph(phase: Phase) -> &'static str {
    match phase {
        Phase::Queued => "\u{22ef}",   // ⋯ waiting its turn
        Phase::Download => "\u{2193}", // ↓ bytes coming down
        Phase::Verify => "\u{25c9}",   // ◉ checking what landed
        Phase::Extract => "\u{2191}",  // ↑ unpacking up and out
        Phase::Link => "\u{2192}",     // → wiring the shim
        Phase::Done => "\u{2713}",     // ✓
        Phase::Failed => "\u{2715}",   // ✕
        Phase::Skipped => "\u{2713}",  // ✓ (already there)
    }
}

/// The card's display order: in-flight rows first (they are what the user is
/// waiting on), then the remaining queue IN QUEUE ORDER (the array a bump
/// permutes — design §4), then finished rows, stably by name. Only admitted
/// names appear. Pure and `pub(crate)` so the ordering law is test-pinned.
pub(crate) fn display_order(snap: &crate::PkgProgressSnapshot) -> Vec<String> {
    let f = &snap.file;
    let mut out: Vec<String> = Vec::new();
    let mut push = |name: &str| {
        if admitted_name(name).is_some() && !out.iter().any(|n| n == name) {
            out.push(name.to_string());
        }
    };
    for (name, row) in &f.programs {
        if matches!(
            row.phase,
            Phase::Download | Phase::Verify | Phase::Extract | Phase::Link
        ) {
            push(name);
        }
    }
    for name in &f.queue {
        if f.programs
            .get(name)
            .is_none_or(|r| r.phase == Phase::Queued)
        {
            push(name);
        }
    }
    for (name, row) in &f.programs {
        if matches!(row.phase, Phase::Done | Phase::Failed | Phase::Skipped) {
            push(name);
        }
    }
    out
}

/// The party cat's drawn HEIGHT for a given cell height — one copy, read both by
/// the layout that reserves its air and by the placement that draws it.
fn cat_height(ch: f32) -> f32 {
    (ch * 1.35).clamp(18.0, 42.0)
}

/// Peak excursion of the walking cat's bob, in px. The bob is a `sin`, so it
/// reaches this far UP as well as down — the reservation must cover the up half or
/// a walking cat clips its ears into the text on every other frame.
fn cat_bob_amplitude(ch: f32) -> f32 {
    ch * 0.08
}

/// How much extra vertical air the bar row needs so a cat standing on the bar
/// stays entirely BELOW the text above it.
///
/// The cat's feet sit at `bar_y + bar_h·0.35` and its body runs `cat_h` px up from
/// there (plus `bob` at the top of its stride), while the bar row already offers
/// `(bar_row_h − bar_h)/2` px of lead-in above the bar. The shortfall is what the
/// layout inserts between the subtitle and the bar. Never negative: a card whose
/// bar row is already generous reserves nothing and is byte-identical to before.
fn cat_clearance_needed(cat_h: f32, cat_bob_amp: f32, bar_h: f32, bar_row_h: f32) -> f32 {
    (cat_h + cat_bob_amp - bar_h * 0.35 - (bar_row_h - bar_h) * 0.5).max(0.0)
}

/// Build the card as pure [`DrawPrim`]s + the cat spec. See the module doc for
/// the honesty rules; geometry mirrors the notice card (top area, below the
/// in-grid chrome given by `clear_rows`) but sits at the tray's RIGHT edge so
/// it reads as a status surface, not an announcement.
pub(crate) fn pkg_progress_tray(
    snap: &crate::PkgProgressSnapshot,
    g: &SettingsGeom,
    chrome: &CardChrome,
    fx: &FxView,
    clear_rows: f32,
) -> CardTray {
    let r = Roles::from_theme(chrome.theme);
    let (cw, ch, px) = (g.cw, g.ch, g.font_px);
    let tray_w = g.cols as f32 * cw;
    let s = TypeStep::Secondary.px_clamped(px, 12.0, f32::INFINITY);
    let cap = TypeStep::Caption.px_clamped(px, 10.0, f32::INFINITY);
    let sv = s.get();
    let cv = cap.get();

    let mut prims: Vec<DrawPrim> = Vec::new();
    let empty = |x: f32, y: f32| CardTray {
        tray: TrayInput {
            prims: Vec::new(),
            card: (x, y, 0.0, 0.0),
        },
        cat: None,
    };

    // Width: a fixed type-relative measure, clamped to the tray. Too narrow for
    // an honest card ⇒ nothing at all (the notice's no-sliver rule).
    let max_w = (tray_w - 2.0 * cw).max(0.0);
    let w = (sv * 26.0).min(max_w);
    let x = (tray_w - w - cw * 0.75).max(cw * 0.5);
    let y = (clear_rows.max(0.0) + 0.85) * ch;
    if w < sv * 14.0 {
        return empty(x, y);
    }

    let pad_x = sv * 0.9;
    let pad_y = sv * 0.7;
    let title_h = sv * 1.5;
    let bar_h = sv * 0.62;
    let bar_row_h = bar_h + sv * 0.7;
    let row_h = sv * 1.45;

    let f = &snap.file;
    let unknown_version = f.v != PROGRESS_VERSION;
    let stopped = !snap.running && f.ended_unix.is_none();
    let ended = f.ended_unix.is_some();
    let failed_count = f
        .programs
        .values()
        .filter(|r| r.phase == Phase::Failed)
        .count();

    // Rows (none for an unknown version — one generic line instead).
    let order = if unknown_version {
        Vec::new()
    } else {
        display_order(snap)
    };
    let shown = order.len().min(MAX_ROWS);
    let folded = order.len() - shown;
    let sub_line = cv * 1.35;
    let note_line = if stopped || unknown_version || (ended && failed_count > 0) {
        cv * 1.5
    } else {
        0.0
    };
    let more_line = if folded > 0 { cv * 1.4 } else { 0.0 };
    // THE CAT'S OWN AIR. The party cat stands ON the bar, so its body reaches
    // `cat_h` px UP from the bar's midline — and the bar row was sized for a
    // capsule, not for an animal. With no reservation the cat's dest rect
    // overlapped the two text rows directly above it (title + subtitle), and
    // `blit_cat` is a straight alpha OVER onto the FINISHED tray raster, so the
    // cat literally painted out the card's own "Installing the ALab toolchain"
    // line and the "n of m · x of y MB" beneath it. Worse, its whole switch is
    // `pkg_progress_effects` — `cursor_trail = false` does not reach it — and
    // `cat_bob` drops to 0 the moment overall bytes stop moving, so a stalled
    // download left a MOTIONLESS cat sitting on unreadable progress text.
    //
    // Those three properties are what the 2026-08 Windows audit reported of the
    // sprite it caught frozen over the "Installing the ALab toolchain" toast,
    // occluding its progress text and surviving `cursor_trail = false` — and this
    // cat is the only decoration on that card that has all three (the resident pet
    // fails the third: every pet emitter gates on the trail master, so the first
    // frame after that key goes false draws no pet at all). Attributing the
    // reported sighting is still an INFERENCE; the overlap itself is not — it is
    // arithmetic, and `the_party_cat_never_covers_the_cards_own_text` pins it.
    //
    // So the layout reserves the space instead: the bar (and everything under it)
    // moves down by exactly the clearance the cat needs, and the card grows by the
    // same amount. `chrome.fancy` is already folded in, so an effects-off card is
    // byte-identical to before — the reservation and the cat appear and vanish
    // together, and neither can exist without the other.
    let cat_h = cat_height(ch);
    let cat_bob_amp = cat_bob_amplitude(ch);
    let cat_drawn = chrome.fancy && !unknown_version && fx.cat_pose.is_some();
    let cat_headroom = if cat_drawn {
        cat_clearance_needed(cat_h, cat_bob_amp, bar_h, bar_row_h)
    } else {
        0.0
    };
    let h = pad_y * 2.0
        + title_h
        + sub_line
        + cat_headroom
        + bar_row_h
        + note_line
        + shown as f32 * row_h
        + more_line;
    let radius = (sv * 0.9).min(14.0);

    // Shadow — the notice's proven table, inside SHADOW_MARGIN by the
    // `shadow_stays_inside_its_margin` test.
    for (spread, dy, alpha) in SHADOW_LAYERS {
        prims.push(DrawPrim::Panel {
            x: x - spread,
            y: y - spread + dy,
            w: w + 2.0 * spread,
            h: h + 2.0 * spread,
            radius: radius + spread,
            fill: rgba([0, 0, 0], alpha),
            blur: false,
        });
    }
    // Body + hairline rim, the notice's quiet seating.
    prims.push(DrawPrim::Panel {
        x,
        y,
        w,
        h,
        radius,
        fill: rgba(r.elevated, 0xFA),
        blur: false,
    });
    prims.push(DrawPrim::Stroke {
        x,
        y,
        w,
        h,
        radius,
        width: 1.0,
        color: rgba(r.separator, 0x99),
    });

    // Title row: what is happening + the dismiss mark (the whole card is the
    // dismiss target; the ✕ just says so).
    let title = if unknown_version {
        "Installing packages\u{2026}".to_string()
    } else {
        match f.pass.as_str() {
            "seed" => "Preparing the ALab toolchain".to_string(),
            "net" => "Installing the ALab toolchain".to_string(),
            _ => "Installing packages".to_string(),
        }
    };
    let title_baseline = row_baseline(y + pad_y, title_h, sv);
    prims.push(text_prim(
        x + pad_x,
        title_baseline,
        title,
        s,
        TextWeight::Bold,
        TextFace::UiBold,
        rgba(r.text_primary, 0xFF),
    ));
    prims.push(text_prim(
        x + w - pad_x - text_w("\u{2715}", cv),
        title_baseline,
        "\u{2715}".to_string(),
        cap,
        TextWeight::Regular,
        TextFace::Mono,
        rgba(r.text_tertiary, 0xCC),
    ));

    // Sub-line: the overall numbers, or the honest terminal/stopped summary.
    let sub = if unknown_version {
        "a newer aterm is writing this progress format".to_string()
    } else if ended {
        if failed_count == 0 {
            format!("all {} installed", f.overall.programs_total)
        } else {
            format!(
                "{} of {} installed \u{2014} {failed_count} failed",
                f.overall
                    .programs_total
                    .saturating_sub(u32::try_from(failed_count).unwrap_or(u32::MAX)),
                f.overall.programs_total
            )
        }
    } else if f.overall.bytes_total > 0 {
        format!(
            "{} of {} \u{00b7} {} of {} MB",
            f.overall.programs_done,
            f.overall.programs_total,
            mb(f.overall.bytes_done),
            mb(f.overall.bytes_total)
        )
    } else {
        format!(
            "{} of {}",
            f.overall.programs_done, f.overall.programs_total
        )
    };
    let sub_y = y + pad_y + title_h;
    prims.push(text_prim(
        x + pad_x,
        row_baseline(sub_y, sub_line, cv),
        sub,
        cap,
        TextWeight::Regular,
        TextFace::Ui,
        rgba(r.text_secondary, 0xE6),
    ));

    // The overall bar.
    let frac = overall_frac(f);
    let bar_x = x + pad_x;
    let bar_w = w - 2.0 * pad_x;
    let bar_y = sub_y + sub_line + cat_headroom + (bar_row_h - bar_h) * 0.5;
    if chrome.fancy && !unknown_version {
        // Rainbow fill: `Capsule` takes one colour, so the gradient is
        // hue-stepped Panels each showing one clipped WINDOW of the full
        // rounded capsule — rounded ends survive, the interior steps hue.
        prims.push(DrawPrim::Capsule {
            x: bar_x,
            y: bar_y,
            w: bar_w,
            h: bar_h,
            frac: 0.0,
            fill: rgba(r.accent, 0),
            track: rgba(r.control_track, 0xFF),
        });
        let fill_w = bar_w * frac;
        if fill_w > 0.5 {
            // Seed from the pass identity (stable per pass), spin with time ×
            // amplitude — amp 0 holds a still rainbow, the reduced-motion rule.
            let seed = (f.started_unix % 997) as f32 / 997.0;
            let spin = fx.t * RAINBOW_TURNS_PER_SEC * chrome.amp.clamp(0.0, 1.0);
            let seg_w = fill_w / RAINBOW_SEGMENTS as f32;
            for i in 0..RAINBOW_SEGMENTS {
                let sx = bar_x + i as f32 * seg_w;
                let hue = seed + spin + i as f32 / RAINBOW_SEGMENTS as f32;
                prims.push(DrawPrim::ClipPush {
                    x: sx,
                    y: bar_y,
                    w: seg_w + 0.75, // hairline overlap: no AA seams between steps
                    h: bar_h,
                });
                prims.push(DrawPrim::Panel {
                    x: bar_x,
                    y: bar_y,
                    w: fill_w.max(bar_h),
                    h: bar_h,
                    radius: bar_h * 0.5,
                    fill: rgba(legible_on(rainbow(hue), r.elevated), 0xFF),
                    blur: false,
                });
                prims.push(DrawPrim::ClipPop);
            }
        }
    } else {
        // Plain themed accent capsule — the effects-off / reduced / serious /
        // unknown-version rendering. Fully functional: same value, same snap.
        prims.push(DrawPrim::Capsule {
            x: bar_x,
            y: bar_y,
            w: bar_w,
            h: bar_h,
            frac: if unknown_version { 0.0 } else { frac },
            fill: rgba(r.accent, 0xFF),
            track: rgba(r.control_track, 0xFF),
        });
    }

    // The stopped / failed / unknown-version note — every failure names its
    // next act, no silent green.
    let mut cursor_y = bar_y + bar_h + (bar_row_h - bar_h) * 0.5;
    if note_line > 0.0 {
        let (note, color) = if unknown_version {
            (
                "details in Settings \u{25b8} Packages".to_string(),
                r.text_secondary,
            )
        } else if stopped {
            (
                "stopped \u{2014} reopen aterm or run: aterm pkg update".to_string(),
                r.danger,
            )
        } else {
            (
                "details in Settings \u{25b8} Packages".to_string(),
                r.danger,
            )
        };
        prims.push(text_prim(
            x + pad_x,
            row_baseline(cursor_y, note_line, cv),
            note,
            cap,
            TextWeight::Regular,
            TextFace::Ui,
            rgba(color, 0xFF),
        ));
        cursor_y += note_line;
    }

    // Per-program rows.
    let rows_top = cursor_y;
    let queue_pos: BTreeMap<&str, usize> = f
        .queue
        .iter()
        .enumerate()
        .map(|(i, n)| (n.as_str(), i + 1))
        .collect();
    let burst_of =
        |name: &str| -> Option<f32> { fx.bursts.iter().find(|(n, _)| n == name).map(|(_, p)| *p) };
    let from_of = |name: &str| -> Option<usize> {
        fx.reorder_from
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, i)| *i)
    };
    for (i, name) in order.iter().take(shown).enumerate() {
        // A bumped row slides from where it WAS to where it now is; amp 0 (or a
        // finished slide) snaps — information first, motion second.
        let slide = chrome.amp.clamp(0.0, 1.0)
            * from_of(name).map_or(0.0, |from| {
                (from.min(MAX_ROWS) as f32 - i as f32) * (1.0 - fx.reorder_t)
            });
        let ry = rows_top + (i as f32 + slide) * row_h;
        // Rows displaced past the card's bottom edge stay inside it.
        if ry + row_h > y + h + 0.5 || ry < rows_top - row_h {
            continue;
        }
        let row = f.programs.get(name);
        let phase = row.map_or(Phase::Queued, |r| r.phase);
        let baseline = row_baseline(ry, row_h, cv);
        let display = admitted_name(name).unwrap_or_default();

        let glyph_color = match phase {
            Phase::Done | Phase::Skipped => r.success,
            Phase::Failed => r.danger,
            Phase::Queued => r.text_tertiary,
            _ => r.accent,
        };
        prims.push(text_prim(
            x + pad_x,
            baseline,
            phase_glyph(phase).to_string(),
            cap,
            TextWeight::Regular,
            TextFace::Mono,
            rgba(glyph_color, 0xFF),
        ));
        let name_x = x + pad_x + cv * 1.5;
        prims.push(text_prim(
            name_x,
            baseline,
            display.clone(),
            cap,
            TextWeight::Bold,
            TextFace::Mono,
            rgba(
                if phase == Phase::Queued {
                    r.text_secondary
                } else {
                    r.text_primary
                },
                0xFF,
            ),
        ));
        // The phase label, right-aligned; a bumped queued row earns its tag.
        let mut label = row.map_or_else(
            || {
                phase_label(
                    &atpkg::progress::ProgramProgress {
                        phase: Phase::Queued,
                        bytes_done: 0,
                        bytes_total: 0,
                        build: None,
                        bumped: false,
                        error: None,
                    },
                    queue_pos.get(name.as_str()).copied(),
                )
            },
            |row| phase_label(row, queue_pos.get(name.as_str()).copied()),
        );
        let bumped = row.is_some_and(|r| r.bumped);
        if bumped && phase == Phase::Queued {
            // The design's own words (§4): the bump answered a real request.
            // Replaces the plain position — "you asked" outranks "you wait".
            label = "\u{2191} bumped \u{2014} you asked for this".to_string();
        }
        let label_color = match phase {
            Phase::Failed => r.danger,
            _ if bumped => r.accent,
            _ => r.text_secondary,
        };
        let name_end = name_x + text_w(&display, cv);
        let max_label = x + w - pad_x - name_end - cv;
        if max_label <= 0.0 {
            label.clear();
        } else {
            let mut label_w = ui_text_width_for(TextFace::Ui, &label, cv);
            if label_w > max_label {
                while !label.is_empty() && label_w > max_label {
                    label.pop();
                    label_w = ui_text_width_for(TextFace::Ui, &label, cv)
                        + ui_text_width_for(TextFace::Ui, "\u{2026}", cv);
                }
                label.push('\u{2026}');
            }
        }
        let label_x = x + w - pad_x - ui_text_width_for(TextFace::Ui, &label, cv);
        if !label.is_empty() {
            prims.push(text_prim(
                label_x,
                baseline,
                label,
                cap,
                TextWeight::Regular,
                TextFace::Ui,
                rgba(label_color, 0xE6),
            ));
        }
        // The metered phases carry a mini capsule between name and label — a
        // per-row echo of the overall bar, plain accent in every mode (the
        // rainbow is the OVERALL bar's; a row's job is legibility).
        if let Some(row) = row
            && matches!(phase, Phase::Download | Phase::Extract)
            && row.bytes_total > 0
        {
            let mini_x = name_end + cv * 0.7;
            let mini_w = (label_x - cv * 0.7 - mini_x).min(w * 0.28);
            if mini_w > cv * 2.0 {
                prims.push(DrawPrim::Capsule {
                    x: mini_x,
                    y: ry + (row_h - cv * 0.4) * 0.5,
                    w: mini_w,
                    h: cv * 0.4,
                    frac: (row.bytes_done as f32 / row.bytes_total as f32).clamp(0.0, 1.0),
                    fill: rgba(r.accent, 0xFF),
                    track: rgba(r.control_track, 0xFF),
                });
            }
        }
        // A completion burst: a small twinkle ring around the row (fancy only).
        if chrome.fancy
            && let Some(p) = burst_of(name)
        {
            let ring_x = x + pad_x * 0.5;
            let ring_w = w - pad_x;
            for k in 0..BURST_SPARKLES {
                let fq = k as f32 / BURST_SPARKLES as f32;
                let (cx, cy) = perimeter_point(ring_x, ry + row_h * 0.15, ring_w, row_h * 0.7, fq);
                let tw = (1.0 - p) * (0.5 + 0.5 * (fq * std::f32::consts::TAU * 2.0).sin());
                let rr = SPARKLE_MIN_R + (SPARKLE_MAX_R - SPARKLE_MIN_R) * tw;
                prims.push(DrawPrim::Dot {
                    cx,
                    cy,
                    r: rr,
                    color: rgba(
                        rainbow(fq + p * 0.4),
                        (0x30 as f32 + 0xBF as f32 * tw) as u8,
                    ),
                    breathe: false,
                });
            }
        }
    }
    if folded > 0 {
        prims.push(text_prim(
            x + pad_x,
            row_baseline(rows_top + shown as f32 * row_h, more_line, cv),
            format!("+ {folded} more"),
            cap,
            TextWeight::Regular,
            TextFace::Ui,
            rgba(r.text_tertiary, 0xE6),
        ));
    }

    // The 100% celebration: one sweep of the notice's 14-dot twinkle ring
    // around the whole card, hues walking with the sweep.
    if chrome.fancy
        && let Some(p) = fx.celebrate
    {
        for k in 0..CELEBRATE_SPARKLES {
            let fq = k as f32 / CELEBRATE_SPARKLES as f32;
            let (cx, cy) = perimeter_point(x - 3.0, y - 3.0, w + 6.0, h + 6.0, fq + p);
            let tw = 0.5 + 0.5 * ((p * 6.0 + fq * std::f32::consts::TAU * 2.0).sin());
            let fade = 1.0 - (2.0 * p - 1.0).abs(); // in, bright, out
            let rr = SPARKLE_MIN_R + (SPARKLE_MAX_R - SPARKLE_MIN_R) * tw;
            prims.push(DrawPrim::Dot {
                cx,
                cy,
                r: rr,
                color: rgba(
                    rainbow(fq + p),
                    ((0x50 as f32 + 0x9F as f32 * tw) * fade) as u8,
                ),
                breathe: false,
            });
        }
    }

    // The cat rides the bar's leading edge (fancy only; it is the sanctioned
    // cut line and the first thing effects-off removes). Its feet sit on the
    // bar; the bob is time-driven and therefore amplitude-scaled. The air it
    // stands in was reserved above (`cat_headroom`) under the SAME `cat_drawn`
    // predicate and from the SAME `cat_height`/`cat_bob_amplitude` — one copy of
    // each number, so the reservation cannot drift from the sprite.
    let cat = cat_drawn
        .then(|| {
            fx.cat_pose.map(|pose| {
                let bob = if fx.cat_bob > 0.0 {
                    (fx.t * std::f32::consts::TAU * 1.6).sin()
                        * cat_bob_amp
                        * chrome.amp.clamp(0.0, 1.0)
                        * fx.cat_bob
                } else {
                    0.0
                };
                CatPlace {
                    cx: bar_x + bar_w * frac,
                    bottom: bar_y + bar_h * 0.35 + bob,
                    h: cat_h,
                    pose,
                    facing_left: fx.cat_facing_left,
                }
            })
        })
        .flatten();

    CardTray {
        tray: TrayInput {
            prims,
            card: (x, y, w, h),
        },
        cat,
    }
}

// ---------------------------------------------------------------------------
// The cat blit.
// ---------------------------------------------------------------------------

fn pack_rgb(c: [u8; 3]) -> u32 {
    (u32::from(c[0]) << 16) | (u32::from(c[1]) << 8) | u32::from(c[2])
}

/// Bake the cat's pose to exact-size RGBA (`aterm_effects::pet_baker`, honoring
/// the worn coat/iris) and alpha-blit it onto the card raster — `DrawPrim` has
/// no image variant, and the pet's normal lane is pane-bound FreeSprites, so
/// the least-invasive path is this one straight-alpha OVER onto the finished
/// tray bytes (the raster and the compositor both speak straight alpha).
/// `place` is in RASTER-LOCAL px (the splice translates, like its prims).
pub(crate) fn blit_cat(
    rgba_buf: &mut [u8],
    pw: u32,
    ph: u32,
    place: &CatPlace,
    chrome: &CardChrome,
) {
    let r = Roles::from_theme(chrome.theme);
    let h = place.h.round().max(1.0) as u16;
    let aspect = PetBaker::aspect(place.pose);
    let w = ((f32::from(h) * aspect).round().max(1.0)) as u16;
    let key = PetBakeKey {
        pose: place.pose,
        coat: chrome.coat,
        iris: chrome.iris,
        colors: CatColorKey::from_rgb(
            pack_rgb(r.elevated),
            pack_rgb(r.text_primary),
            pack_rgb(r.elevated),
        ),
        w,
        h,
    };
    // One-entry memo: the pose changes at ~6 fps while the blit runs per frame.
    let (tw, th, src) = FX.with(|fx| {
        let mut fx = fx.borrow_mut();
        if fx.baked.as_ref().is_none_or(|(k, ..)| *k != key) {
            let tile = key.bake();
            fx.baked = Some((key, tile.width(), tile.height(), tile.pixels().to_vec()));
        }
        let (_, tw, th, px) = fx.baked.as_ref().expect("just baked");
        (i64::from(*tw), i64::from(*th), px.clone())
    });
    let src = src.as_slice();
    let x0 = (place.cx - f32::from(w) * 0.5).round() as i64;
    let y0 = (place.bottom - f32::from(h)).round() as i64;
    for ty in 0..th {
        let py = y0 + ty;
        if py < 0 || py >= i64::from(ph) {
            continue;
        }
        for tx in 0..tw {
            let px = x0 + tx;
            if px < 0 || px >= i64::from(pw) {
                continue;
            }
            // Authored facing right; mirror the read for a leftward cat.
            let sx = if place.facing_left { tw - 1 - tx } else { tx };
            let si = ((ty * tw + sx) * 4) as usize;
            let sa = u32::from(src[si + 3]);
            if sa == 0 {
                continue;
            }
            let di = ((py * i64::from(pw) + px) * 4) as usize;
            let da = u32::from(rgba_buf[di + 3]);
            // Straight-alpha OVER, stored straight (guarded un-premultiply).
            let oa = sa + da * (255 - sa) / 255;
            if oa == 0 {
                continue;
            }
            for c in 0..3 {
                let sc = u32::from(src[si + c]);
                let dc = u32::from(rgba_buf[di + c]);
                rgba_buf[di + c] = ((sc * sa + dc * da * (255 - sa) / 255) / oa).min(255) as u8;
            }
            rgba_buf[di + 3] = oa.min(255) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atpkg::progress::{Overall, ProgramProgress, ProgressFile};

    fn program(phase: Phase, done: u64, total: u64) -> ProgramProgress {
        ProgramProgress {
            phase,
            bytes_done: done,
            bytes_total: total,
            build: Some(210),
            bumped: false,
            error: None,
        }
    }

    fn snapshot(running: bool) -> crate::PkgProgressSnapshot {
        let mut programs = BTreeMap::new();
        programs.insert("ay".to_string(), program(Phase::Done, 0, 0));
        programs.insert(
            "robi".to_string(),
            program(Phase::Download, 4_100_000, 9_800_000),
        );
        programs.insert("trust".to_string(), program(Phase::Queued, 0, 0));
        programs.insert("hej".to_string(), program(Phase::Queued, 0, 0));
        crate::PkgProgressSnapshot {
            file: ProgressFile {
                v: PROGRESS_VERSION,
                pid: running.then_some(4321),
                pass: "net".to_string(),
                started_unix: 100,
                heartbeat_unix: 100,
                overall: Overall {
                    programs_done: 1,
                    programs_total: 4,
                    bytes_done: 18_022_400,
                    bytes_total: 96_411_648,
                },
                queue: vec!["robi".to_string(), "trust".to_string(), "hej".to_string()],
                programs,
                ended_unix: None,
            },
            running,
        }
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

    fn chrome(theme: Theme, fancy: bool, amp: f32) -> CardChrome {
        CardChrome {
            theme,
            amp,
            fancy,
            coat: 8,
            iris: 4,
        }
    }

    fn texts(prims: &[DrawPrim]) -> Vec<String> {
        prims
            .iter()
            .filter_map(|p| match p {
                DrawPrim::Text { s, .. } => Some(s.clone()),
                _ => None,
            })
            .collect()
    }

    /// The slot-sharing parity contract this module documents: our slot fp is
    /// EVEN and nonzero for any input; the notice pill's is ODD by its `| 1`.
    /// `splice_notice`'s spare-the-resident guard and the dismiss hit-test
    /// both stand on this.
    #[test]
    fn slot_fp_is_even_and_notice_fp_is_odd() {
        for (content, paint) in [(0u64, 0u64), (1, 1), (u64::MAX, u64::MAX), (0xdead, 0xbeef)] {
            let fp = slot_fp(content, paint);
            assert_eq!(fp & 1, 0, "slot fp must be even ({content:#x},{paint:#x})");
            assert_ne!(fp, 0, "slot fp must be nonzero");
        }
        let n = crate::notice::TransientNotice::level_up(830, Instant::now());
        assert_eq!(
            n.fingerprint(Instant::now(), true) & 1,
            1,
            "the notice fingerprint must stay odd — the parity contract's other half"
        );
    }

    /// Display order law: in-flight first, then the queue IN QUEUE ORDER (the
    /// array a bump permutes), then finished rows.
    #[test]
    fn display_order_is_inflight_then_queue_then_finished() {
        let snap = snapshot(true);
        assert_eq!(display_order(&snap), ["robi", "trust", "hej", "ay"]);
        // A bump permuting the queue permutes the middle — nothing else.
        let mut bumped = snap.clone();
        bumped.file.queue = vec!["hej".to_string(), "trust".to_string()];
        assert_eq!(display_order(&bumped), ["robi", "hej", "trust", "ay"]);
    }

    /// A bumped queued row wears the design's own tag — the bump answered a
    /// real request, and the card says so in §4's words.
    #[test]
    fn bumped_row_wears_its_tag() {
        let mut snap = snapshot(true);
        if let Some(row) = snap.file.programs.get_mut("trust") {
            row.bumped = true;
        }
        let card = pkg_progress_tray(
            &snap,
            &geom(),
            &chrome(Theme::default(), false, 0.0),
            &FxView::still(),
            0.0,
        );
        let t = texts(&card.tray.prims).join("\n");
        assert!(
            t.contains("\u{2191} bumped \u{2014} you asked for this"),
            "the bumped tag renders:\n{t}"
        );
    }

    /// A name that fails the `ToolName` gate has no row — a hostile
    /// progress.json can neither shim-squat a sensitive name into the UI nor
    /// walk a path separator into a row.
    #[test]
    fn unadmitted_names_get_no_row() {
        let mut snap = snapshot(true);
        snap.file
            .programs
            .insert("sudo".to_string(), program(Phase::Download, 1, 2));
        snap.file
            .programs
            .insert("../evil".to_string(), program(Phase::Queued, 0, 0));
        snap.file.queue.push("../evil".to_string());
        let order = display_order(&snap);
        assert!(!order.iter().any(|n| n == "sudo" || n == "../evil"));
    }

    /// Effects off ⇒ the plain themed accent capsule with text rows — fully
    /// functional (bar present with the real fraction, no sparkle dots, no
    /// cat), which is exactly the reduced/serious/opted-out rendering.
    #[test]
    fn effects_off_degrades_to_a_plain_functional_capsule() {
        let card = pkg_progress_tray(
            &snapshot(true),
            &geom(),
            &chrome(Theme::default(), false, 1.0),
            &FxView::still(),
            0.0,
        );
        let (_, _, w, h) = card.tray.card;
        assert!(w > 0.0 && h > 0.0, "the card renders");
        assert!(card.cat.is_none(), "no cat without effects");
        let caps: Vec<f32> = card
            .tray
            .prims
            .iter()
            .filter_map(|p| match p {
                DrawPrim::Capsule { frac, .. } => Some(*frac),
                _ => None,
            })
            .collect();
        assert_eq!(
            caps.len(),
            2,
            "the plain overall capsule plus robi's metered mini bar"
        );
        assert!(
            caps.iter()
                .any(|f| (f - 18_022_400.0 / 96_411_648.0).abs() < 1e-3),
            "the overall capsule carries the real pass fraction"
        );
        assert!(
            caps.iter()
                .any(|f| (f - 4_100_000.0 / 9_800_000.0).abs() < 1e-3),
            "robi's row meters its own bytes"
        );
        assert!(
            !card
                .tray
                .prims
                .iter()
                .any(|p| matches!(p, DrawPrim::Dot { .. })),
            "no sparkles without effects"
        );
        // The information survives: every program row and its phase label.
        let t = texts(&card.tray.prims).join("\n");
        for needle in [
            "robi",
            "4.1 of 9.8 MB",
            "trust",
            "queued",
            "ay",
            "installed",
        ] {
            assert!(t.contains(needle), "missing {needle:?} in:\n{t}");
        }
    }

    /// Fancy mode paints the hue-stepped fill (clipped panels), and REDUCED
    /// amplitude freezes it: two frames far apart in time are prim-identical
    /// at amp 0 — no time-driven frames — while amp 1 genuinely moves.
    #[test]
    fn reduced_amplitude_freezes_the_rainbow() {
        let g = geom();
        let snap = snapshot(true);
        let build = |amp: f32, t: f32| {
            let fx = FxView {
                t,
                ..FxView::still()
            };
            let card = pkg_progress_tray(&snap, &g, &chrome(Theme::default(), true, amp), &fx, 0.0);
            format!("{:?}", card.tray.prims)
        };
        assert_eq!(build(0.0, 0.0), build(0.0, 7.5), "amp 0 ⇒ frozen hues");
        assert_ne!(build(1.0, 0.0), build(1.0, 7.5), "amp 1 ⇒ the hue walks");
        // And the rainbow really is there: clipped segments, not one capsule.
        let card = pkg_progress_tray(
            &snap,
            &g,
            &chrome(Theme::default(), true, 1.0),
            &FxView::still(),
            0.0,
        );
        let clips = card
            .tray
            .prims
            .iter()
            .filter(|p| matches!(p, DrawPrim::ClipPush { .. }))
            .count();
        assert_eq!(clips, RAINBOW_SEGMENTS, "one clip window per hue step");
    }

    /// Dark/light role coverage: every text colour the card paints comes from
    /// `Roles::from_theme`, so the two appearances genuinely differ and the
    /// title wears each theme's primary text role.
    #[test]
    fn dark_and_light_themes_paint_their_own_roles() {
        let light = Theme {
            fg: 0x0020_2020,
            bg: 0x00F5_F5F2,
            cursor: 0x0050_FA7B,
            selection: 0x00B0_C4DE,
        };
        let dark = Theme::default();
        let g = geom();
        let snap = snapshot(true);
        let title_color = |theme: Theme| {
            let card =
                pkg_progress_tray(&snap, &g, &chrome(theme, false, 0.0), &FxView::still(), 0.0);
            card.tray
                .prims
                .iter()
                .find_map(|p| match p {
                    DrawPrim::Text { s, color, .. }
                        if s.contains("Installing the ALab toolchain") =>
                    {
                        Some(*color)
                    }
                    _ => None,
                })
                .expect("title present")
        };
        let (lc, dc) = (title_color(light), title_color(dark));
        assert_eq!(
            lc[..3],
            Roles::from_theme(light).text_primary,
            "light title wears the light primary role"
        );
        assert_eq!(
            dc[..3],
            Roles::from_theme(dark).text_primary,
            "dark title wears the dark primary role"
        );
        assert_ne!(lc, dc, "the two appearances are actually different");
    }

    /// A not-running snapshot renders ONLY not-running states: the stopped
    /// banner with its next act, and never a live-sounding overall claim. A
    /// dead installer's file cannot claim live progress (design §3).
    #[test]
    fn stopped_pass_names_its_next_act() {
        let card = pkg_progress_tray(
            &snapshot(false),
            &geom(),
            &chrome(Theme::default(), true, 1.0),
            &FxView::still(),
            0.0,
        );
        let t = texts(&card.tray.prims).join("\n");
        assert!(
            t.contains("stopped \u{2014} reopen aterm or run: aterm pkg update"),
            "the stopped state names its next act:\n{t}"
        );
    }

    /// An unknown schema version renders one generic line — never a guess at
    /// fields whose meaning may have changed (the untrusted-reader rule).
    #[test]
    fn unknown_version_renders_generic() {
        let mut snap = snapshot(true);
        snap.file.v = PROGRESS_VERSION + 1;
        let card = pkg_progress_tray(
            &snap,
            &geom(),
            &chrome(Theme::default(), true, 1.0),
            &FxView::still(),
            0.0,
        );
        let t = texts(&card.tray.prims).join("\n");
        assert!(t.contains("Installing packages\u{2026}"));
        assert!(
            !t.contains("robi") && !t.contains("MB"),
            "no field of an unknown schema is interpreted:\n{t}"
        );
    }

    /// A failed row's error is control-stripped and capped before it becomes
    /// glyphs — terminal-escape hygiene holds even off the TTY.
    #[test]
    fn failed_row_error_is_sanitized() {
        let mut snap = snapshot(true);
        snap.file.programs.insert(
            "hej".to_string(),
            ProgramProgress {
                phase: Phase::Failed,
                bytes_done: 0,
                bytes_total: 0,
                build: None,
                bumped: false,
                error: Some("\u{1b}[2Jboom\u{7}".repeat(40)),
            },
        );
        let card = pkg_progress_tray(
            &snap,
            &geom(),
            &chrome(Theme::default(), false, 0.0),
            &FxView::still(),
            0.0,
        );
        for s in texts(&card.tray.prims) {
            assert!(
                !s.chars().any(char::is_control),
                "control characters must never reach a prim: {s:?}"
            );
        }
    }

    /// The scripted walker's pose law: sits when the pass is done, walks only
    /// while bytes move, stands otherwise — and the fancy card places it at
    /// the bar's leading edge.
    #[test]
    fn cat_rides_the_leading_edge_and_sits_at_done() {
        let g = geom();
        let snap = snapshot(true);
        let walking = FxView {
            cat_pose: Some(PetGlyphId::PetWalk1),
            cat_bob: 1.0,
            ..FxView::still()
        };
        let card = pkg_progress_tray(
            &snap,
            &g,
            &chrome(Theme::default(), true, 1.0),
            &walking,
            0.0,
        );
        let cat = card.cat.expect("fancy card carries the cat");
        assert_eq!(cat.pose, PetGlyphId::PetWalk1);
        // Leading edge: cx sits at frac of the bar's width (bar spans the card
        // minus padding on each side).
        let (x, _, w, _) = card.tray.card;
        let frac = 18_022_400.0 / 96_411_648.0;
        let s = TypeStep::Secondary
            .px_clamped(g.font_px, 12.0, f32::INFINITY)
            .get();
        let expect = (x + s * 0.9) + (w - 2.0 * s * 0.9) * frac;
        assert!((cat.cx - expect).abs() < 0.6, "cat at the leading edge");
        // And the fx state machine chooses Sit once the snapshot says ended.
        let mut done = snapshot(false);
        done.file.ended_unix = Some(999);
        done.file.overall.programs_done = 4;
        let out = advance_fx(&done, Instant::now());
        assert_eq!(out.view.cat_pose, Some(PetGlyphId::PetSit));
    }

    /// NOTHING DECORATIVE MAY OCCLUDE THE CARD'S OWN WORDS.
    ///
    /// The party cat stands ON the progress bar and its body reaches `cat_h`
    /// (18–42 px) straight up from there, while the bar row was sized for a
    /// capsule. `blit_cat` is a straight alpha OVER onto the FINISHED tray raster,
    /// so with no reservation the cat painted out the two text rows above the bar
    /// — the card's own "Installing the ALab toolchain" title and its "n of m ·
    /// x of y MB" line. The overlap is arithmetic, and this test is the proof.
    ///
    /// It also carries, term for term, the three properties the 2026-08 Windows
    /// audit reported of the sprite it caught frozen over that toast: it occludes
    /// the card's progress text (here), its switch is `pkg_progress_effects`
    /// (folded into `chrome.fancy`) so `cursor_trail = false` does not reach it,
    /// and `cat_bob` drops to 0 the moment overall bytes stop moving, so a stalled
    /// download parks a MOTIONLESS cat. Whether it is the same sighting stays an
    /// inference; the overlap this test pins does not depend on that.
    ///
    /// Asserted over a conservative ink box per text run — cap height above the
    /// baseline, a quarter-em of descender below — at several font sizes, at both
    /// ends of the bar, and at BOTH bob extremes.
    ///
    /// The bob phase matters and its sign is easy to get backwards. `bottom` is
    /// `bar_y + bar_h·0.35 + bob` in a y-DOWN space, so `sin == +1` pushes the cat
    /// DOWN (away from the text) and `sin == −1` lifts it UP into the text. The
    /// dangerous frame is the negative one; both are driven here so a reservation
    /// that covered only the easy half would fail.
    #[test]
    fn the_party_cat_never_covers_the_cards_own_text() {
        // `bob = sin(t·TAU·1.6)·amp`: t = 1/(4·1.6) puts the sine at +1 (cat
        // pushed down, the SAFE extreme), t = 3/(4·1.6) at −1 (cat lifted into
        // the text, the extreme the reservation exists for).
        for (phase, t) in [("down", 1.0_f32 / (4.0 * 1.6)), ("up", 3.0 / (4.0 * 1.6))] {
            for font_px in [11.0_f32, 14.0, 18.0, 24.0] {
                for (frac_done, total) in [(0u64, 96_411_648u64), (18_022_400, 96_411_648)] {
                    let mut snap = snapshot(true);
                    snap.file.overall.bytes_done = frac_done;
                    snap.file.overall.bytes_total = total;
                    let g = SettingsGeom { font_px, ..geom() };
                    let walking = FxView {
                        cat_pose: Some(PetGlyphId::PetWalk1),
                        cat_bob: 1.0,
                        t,
                        ..FxView::still()
                    };
                    let card = pkg_progress_tray(
                        &snap,
                        &g,
                        &chrome(Theme::default(), true, 1.0),
                        &walking,
                        0.0,
                    );
                    let cat = card.cat.expect("the fancy card carries the cat");
                    let (cy0, cy1) = (cat.bottom - cat.h, cat.bottom);
                    // VERTICAL BANDS ONLY, deliberately. The cat rides the bar
                    // from one end to the other and every text run is
                    // left-aligned inside the same card, so any horizontal test
                    // would just be a slower way of asking whether this
                    // particular `frac` happens to dodge this particular string.
                    // The law is stronger than that: the cat gets its own row,
                    // and no text row may share it at ANY progress value.
                    for p in &card.tray.prims {
                        let DrawPrim::Text {
                            baseline, s, px, ..
                        } = p
                        else {
                            continue;
                        };
                        if s.is_empty() {
                            continue;
                        }
                        // Cap height above the baseline, a quarter-em of
                        // descender below — a conservative ink box for a run of
                        // this size.
                        let (ty0, ty1) = (*baseline - *px, *baseline + *px * 0.25);
                        assert!(
                            cy1 <= ty0 || ty1 <= cy0,
                            "at font_px {font_px} with the bob {phase} the cat band \
                             {cy0:.1}..{cy1:.1} overlaps {s:?} at {ty0:.1}..{ty1:.1}"
                        );
                    }
                }
            }
        }
    }

    /// …and the air is reserved ONLY for a cat that exists. An effects-off card
    /// draws no cat and must be exactly as tall as it always was, so turning the
    /// trim off cannot leave a band of empty card behind it.
    #[test]
    fn the_cats_headroom_appears_and_vanishes_with_the_cat() {
        let g = geom();
        let snap = snapshot(true);
        let plain = pkg_progress_tray(
            &snap,
            &g,
            &chrome(Theme::default(), false, 0.0),
            &FxView::still(),
            0.0,
        );
        let fancy = pkg_progress_tray(
            &snap,
            &g,
            &chrome(Theme::default(), true, 1.0),
            &FxView::still(),
            0.0,
        );
        assert!(plain.cat.is_none(), "no cat without effects");
        assert!(fancy.cat.is_some(), "a cat with effects");
        let (_, _, _, plain_h) = plain.tray.card;
        let (_, _, _, fancy_h) = fancy.tray.card;
        let reserved = cat_clearance_needed(
            cat_height(g.ch),
            cat_bob_amplitude(g.ch),
            TypeStep::Secondary
                .px_clamped(g.font_px, 12.0, f32::INFINITY)
                .get()
                * 0.62,
            TypeStep::Secondary
                .px_clamped(g.font_px, 12.0, f32::INFINITY)
                .get()
                * 1.32,
        );
        assert!(reserved > 0.0, "fixture: this geometry does need headroom");
        assert!(
            (fancy_h - plain_h - reserved).abs() < 0.01,
            "the fancy card is taller by EXACTLY the cat's reservation \
             ({plain_h} vs {fancy_h}, reserved {reserved})"
        );
    }

    /// The retire hold: a cleanly-ended pass asks for frames through its hold,
    /// then retires; a merely-STOPPED pass never auto-retires (it needs the
    /// user and says so).
    #[test]
    fn ended_pass_retires_after_its_hold_and_stopped_never_does() {
        let now = Instant::now();
        let mut ended = snapshot(false);
        ended.file.ended_unix = Some(999);
        let first = advance_fx(&ended, now);
        assert!(!first.retire, "the hold starts, not ends, at first sight");
        assert!(
            first.hold_fx_until.is_some_and(|u| u > now),
            "the hold schedules its own frames"
        );
        let later = advance_fx(&ended, now + RETIRE_HOLD + Duration::from_millis(1));
        assert!(later.retire, "the hold elapses into the per-pass dismissal");
        // ONE-SHOT: a reopened card (Settings ▸ Packages) must not be
        // re-dismissed on its next frame by the same elapsed hold.
        let reopened = advance_fx(&ended, now + RETIRE_HOLD * 2);
        assert!(
            !reopened.retire,
            "the retire fires once per pass — a reopen sticks"
        );
        // Same pass identity, merely stopped: no retirement, ever.
        let stopped = snapshot(false);
        let out = advance_fx(&stopped, now + RETIRE_HOLD * 3);
        assert!(!out.retire, "a stopped pass stays until the user acts");
    }

    /// A row's completion sparkles exactly once — the burst arms on the
    /// NOT-done → done edge, not on every frame that sees `done`.
    #[test]
    fn completion_bursts_once_per_row() {
        let now = Instant::now();
        let mut snap = snapshot(true);
        // Fresh pass identity so this test never inherits another's state.
        snap.file.started_unix = 4242;
        let _ = advance_fx(&snap, now);
        snap.file
            .programs
            .insert("robi".to_string(), program(Phase::Done, 0, 0));
        let armed = advance_fx(&snap, now + Duration::from_millis(50));
        assert!(
            armed.view.bursts.iter().any(|(n, _)| n == "robi"),
            "the edge arms a burst"
        );
        let after = advance_fx(&snap, now + Duration::from_millis(50) + BURST * 2);
        assert!(
            !after.view.bursts.iter().any(|(n, _)| n == "robi"),
            "the burst is bounded — no steady-state sparkle"
        );
    }

    /// The reopen edge detector fires exactly on NOT-FRONT → FRONT.
    #[test]
    fn packages_front_edge_reopens_once() {
        let wid = 0xC0FFEE;
        assert!(!note_packages_front(wid, false));
        assert!(note_packages_front(wid, true), "the visit edge fires");
        assert!(!note_packages_front(wid, true), "staying put does not");
        assert!(!note_packages_front(wid, false));
        assert!(note_packages_front(wid, true), "each fresh visit fires");
    }

    /// The cat blit really lands pixels inside the buffer and honors facing:
    /// mirrored output differs from unmirrored on an asymmetric pose.
    #[test]
    fn cat_blit_lands_and_mirrors() {
        let chrome = chrome(Theme::default(), true, 1.0);
        let (pw, ph) = (64u32, 64u32);
        let place = CatPlace {
            cx: 32.0,
            bottom: 60.0,
            h: 32.0,
            pose: PetGlyphId::PetWalk1,
            facing_left: false,
        };
        let mut right = vec![0u8; (pw * ph * 4) as usize];
        blit_cat(&mut right, pw, ph, &place, &chrome);
        assert!(
            right.iter().skip(3).step_by(4).any(|&a| a > 0),
            "the cat left pixels"
        );
        let mut left = vec![0u8; (pw * ph * 4) as usize];
        blit_cat(
            &mut left,
            pw,
            ph,
            &CatPlace {
                facing_left: true,
                ..place
            },
            &chrome,
        );
        assert_ne!(right, left, "facing flips the sprite");
    }
}
