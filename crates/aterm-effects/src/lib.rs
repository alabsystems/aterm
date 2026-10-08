// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-effects`: the host-agnostic visual-effects engines (cursor aurora and
//! comet trail, sparkle words, animated ink, cat, supernova) shared by the native
//! app and the web embedders (`aterm-wasm`, `aterm-gpu-web`).
//!
//! ## Contract
//!
//! - **Clockless state machines.** Every `tick`/`rescan` takes an injected
//!   `now: Instant`. On native that is `std::time::Instant`; on wasm32 it is
//!   `aterm_time::Instant`, so a web host can synthesize `now` from `advance(dt)`.
//! - **Render-time overlay only.** Output goes to the `RenderInput` overlay
//!   channels (`cursor_glow_add`, `cursor_trail`, `word_decorations`, `ink`,
//!   `cat_quads` + `cat_atlas`, `nova_add`); the grid is never touched, so the
//!   overlay never reaches copy, clipboard, PTY or recordings.
//! - **Empty means off.** Disabled emitters return fingerprint `0` and empty
//!   vectors; the renderers' empty-overlay paths are byte-identical to no overlay.
//! - **Idle-to-zero.** `is_active()` reports when nothing is animating; the cat
//!   `next_deadline()` seam is disarmed, so the host idles at 0%.
//! - **Flash safety is structural.** The WCAG 2.3.1 limits (350 ms twinkle
//!   floors, the window-wide nova ignition limiter, `reduced_motion`) live in the
//!   state machines and their config clamps, so embedders cannot bypass them.

/// The animal roster's bake path: one species head to an exact-size RGBA tile,
/// handed to the shared cat atlas through `CatBaker::host_tile`.
pub mod animal_baker;
/// `@generated` const drawlists for the animal roster (do not edit by hand).
/// Produced by `cargo run -p aterm-effects --example gen_animal_glyphs`; checked
/// by the `animal_glyphs_gen_matches_assets` drift test.
pub mod animal_glyphs_gen {
    include!("animal_glyphs_gen.rs");
}
/// Test-only: exact-bit pins asserted on every target but x86_64 macOS, where
/// they are reported instead.
#[cfg(test)]
mod arm64_pin;
pub mod cat_baker;
/// Cat-art v4 codegen (docs/cat-art-v4-design.md §1): the generator that turns the
/// semantic glyph asset TOMLs into the checked-in [`cat_glyphs_gen`] const drawlists.
pub mod cat_glyphs_codegen;
/// `@generated` const glyph drawlists (do not edit by hand). Produced by
/// `cargo run -p aterm-effects --example gen_cat_glyphs`; checked by the
/// `cat_glyphs_gen_matches_assets` drift test in [`cat_glyphs_codegen`].
/// `include!`d rather than `mod` so rustfmt never reflows the generated file.
pub mod cat_glyphs_gen {
    include!("cat_glyphs_gen.rs");
}
pub mod color_math;
/// The pet driver: `CompanionOwner` owns a resident [`kitty_pet`] brain per
/// surface and senses for it, dresses it, decides custody and draws it, for
/// every host.
pub mod companion;
/// WHICH CAT the companions wear: the program cats' tenure gate, the
/// homecoming rate law and the precedence law (favourite > program > launch
/// kitty), one [`companion_look::LookPolicy`] per window (host-boundary
/// Phase 5).
pub mod companion_look;
/// The companions a window keeps beside its resident pet: the flying head,
/// the sing-along, the typed-word detectors, the typed dog and Robi
/// ([`companions::Companions`], host-boundary Phase 5).
pub mod companions;
/// The dog roster's bake path: one authored breed head → an exact-size RGBA
/// tile, handed to the shared cat atlas through `CatBaker::host_tile` — the
/// [`pet_baker`] pattern applied to the typed-word dog cameo.
pub mod dog_baker;
/// The typed-word dog cameo that trots up when you type `dog` past the
/// typed-a-lot gate: lifecycle envelope, breed/coat roll and entry bounce.
pub mod dog_cameo;
/// `@generated` const drawlists for the dog roster (do not edit by hand).
/// Produced by `cargo run -p aterm-effects --example gen_dog_glyphs`; checked by
/// the `dog_glyphs_gen_matches_assets` drift test.
pub mod dog_glyphs_gen {
    include!("dog_glyphs_gen.rs");
}
/// The v0.28 cursor aurora, salvaged and offered as the `classic` trail style.
pub mod classic_wake;
pub mod cursor_beam;
pub mod cursor_comet;
pub mod cursor_droplet;
/// The cursor family's frame step: aurora, cadence comet and every block-cursor
/// body, ticked in one order with one caret-ownership law, for every host.
pub mod cursor_fx;
pub mod cursor_glow;
pub mod cursor_phaser;
pub mod cursor_rainbow;
pub mod cursor_trail;
/// Crate-private shared render kit for the cursor emitters (the two per-cell-row
/// quad pushers, colour ramps, and the one twinkle star).
mod effect_util;
/// Hardened, bounded file admission shared by every native visual-feed loader.
pub mod file_feed;
pub mod genome;
/// The host contract (`docs/DESIGN-host-boundary-2026-08-30.md` §3): the plain
/// data a host hands the effects driver for one frame (`HostFrameInput`,
/// `TerminalFacts`) and gets back (`PressOutcome`). Uses `aterm_time::Instant` only.
pub mod host;
/// The rainbow kitty that flies in front of the cursor on the `rainbow kitty`
/// trail style — its art, pose, and exit choreography.
pub mod kitty_cursor;
/// The rainbow kitty pet: the full-body companion that lives in the text plane
/// and chases the caret (gait, behaviour, screen awareness).
pub mod kitty_pet;
pub mod kitty_registry;
/// The kitty's sing-along for held keys: repeat detector, beat clock and the
/// music-note sprite field.
pub mod kitty_sing;
/// The typed-word detector: a feline word typed at the prompt records a Kitty
/// Log sighting, a curse winces the companion, and `dog` past the typed-a-lot
/// gate pops a visiting dog — fed from the host's key path, never the screen.
pub mod kitty_summon;
pub mod matrix_overlay;
pub mod matrix_rain;
/// Motion policy: the single accessibility gate for decorative animation,
/// resolving `motion`, the OS Reduce Motion flag and focus to an amplitude.
pub mod motion;
pub mod nova;
/// The mushroom cloud — the rarest f-bomb detonation tier.
pub mod nuke;
/// Prism wake for program output: a per-theme rainbow comet on the newest
/// damaged row plus one pip, metered by typing momentum, blind to echoed
/// keystrokes, coalescing to one ribbon under a flood
/// (`docs/DESIGN-output-streak-2026-08-30.md`).
pub mod output_streak;
/// The pet roster's bake path: one authored full-body pose → an exact-size
/// RGBA tile, handed to the shared cat atlas through `CatBaker::host_tile`.
pub mod pet_baker;
mod pet_stroke;
/// Bounded, coherent terminal occupancy and text-free content anchors.
pub mod pet_world;
/// `@generated` const drawlists for the pet roster (do not edit by hand).
/// Produced by `cargo run -p aterm-effects --example gen_pet_glyphs`; checked by
/// the `pet_glyphs_gen_matches_assets` drift test.
pub mod pet_glyphs_gen {
    include!("pet_glyphs_gen.rs");
}
pub mod pane;
pub mod pipeline;
/// Rainbow kitty v2, the `rainbow kitty` cursor theme with pixels and sound as
/// one design (`docs/design/RAINBOW-KITTY-V2.md`). [`rainbow_kitty::Engine`] is
/// what `CursorGlow` delegates to at its seam points; other styles are unaffected.
pub mod rainbow_kitty;
/// Robi, the chrome-walking helper robot: walks the prompt line, climbs a
/// ladder, swings the tab bar, does jumping jacks and hands out tips.
pub mod robi;
/// The Robi roster's bake path: one robot pose to an exact-size RGBA tile,
/// handed to the shared cat atlas through `CatBaker::host_tile`.
pub mod robi_baker;
/// Sound-cue policy: the gain laws (focus, music master, volume, per-cue
/// switches) and the drains that turn engine cues into synth events. The sink
/// is the host's.
pub mod sound_policy;
/// `@generated` const drawlists for the Robi roster (do not edit by hand).
/// Produced by `cargo run -p aterm-effects --example gen_robi_glyphs`; checked by
/// the `robi_glyphs_gen_matches_assets` drift test.
pub mod robi_glyphs_gen {
    include!("robi_glyphs_gen.rs");
}
pub mod cursor_momentum;
pub mod spec;
/// The rainbow family's colour law (`docs/design/RAINBOW-TRAIL-ONE-STORY.md` §2):
/// a monotone C¹ hue curve through seven named stops at one relative luminance,
/// baked to a 1 KB LUT. Colours resolve through [`spectrum::spectrum`]; point
/// marks snap to a name through [`spectrum::spectrum_snap`].
pub mod spectrum;
pub mod supernova;
/// Tone of typing: a small hashed char-n-gram classifier that maps the line
/// being typed to a mood hint for `trail_sound`'s tone tables. Trained by
/// `examples/tone_train.rs`; weights ship as a verified binary asset.
pub mod tone;
/// The tone-of-typing tracker: the typed-line window and the throttled
/// classifier cache whose verdict rides the trail's sound events.
pub mod tone_infer;
/// Trail Packs — user-generated cursor trails compiled from bounded, fail-closed
/// TOML into the `Copy` [`trail_pack::TrailParams`] the cursor-glow engine's
/// custom interpreter drives (`docs/trail-packs.md`). The strict
/// contribution twin of the sparkle [`spec`] Toy Pack lane.
pub mod trail_pack;
pub mod trail_sound;
pub mod trail_sweep;
/// Kitty-command word flash: the word just typed to the pet turns rainbow for
/// about a second. State is owned by [`word_decorations::WordDecorations`].
mod trick_flash;
/// Kitty-command listener: the typed-line state machine that hears `sit`,
/// `kitty jump`, `good kitty` on a line of pure pet talk and reports fire,
/// confirm or revoke. Vocabulary is `aterm_lexicon::tricks`.
pub mod typed_tricks;
pub mod typing_momentum;
pub mod word_decorations;
