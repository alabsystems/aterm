// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-effects` — the host-agnostic visual-effects engines behind aterm's
//! cursor aurora / comet trail and the sparkle-words feature (v1 sparkle + paw,
//! v2 animated ink · peeking cat · supernova), extracted verbatim from
//! `aterm-gui` so the native app and the web embedders (`aterm-wasm`,
//! `aterm-gpu-web`) drive **one** implementation of the art.
//!
//! ## Contract (unchanged from the in-GUI modules)
//!
//! - **Clockless state machines.** Every `tick`/`rescan` takes an injected
//!   `now: Instant`; nothing here reads a wall clock on the hot path. On native
//!   `Instant` is exactly `std::time::Instant`; on wasm32 it is `web_time::Instant`
//!   (the engine-wide idiom, see `aterm-core/src/bell.rs`), so a web host can
//!   synthesize `now` from an `advance(dt)` stream.
//! - **Render-time overlay only.** Output is the `RenderInput` overlay channels
//!   (`cursor_glow_add`, `cursor_trail`, `word_decorations`, `ink`, `cat_quads` +
//!   `cat_atlas`, `nova_add`); the grid is never touched, so the overlay can never
//!   leak into copy/clipboard/PTY/recordings.
//! - **Empty == byte-identical off.** All emitters return fingerprint `0` and
//!   empty vectors when disabled; the renderers' empty-overlay paths are proven
//!   byte-identical by the render suites.
//! - **Idle-to-zero.** `is_active()` reports when nothing is animating;
//!   the retained cat `next_deadline()` seam is permanently disarmed in v4,
//!   so the host returns to 0% idle.
//! - **Flash safety is structural.** The WCAG 2.3.1 posture (≥350 ms twinkle
//!   floors, the window-wide nova ignition limiter, `reduced_motion`) lives in
//!   the state machines and their config clamps — hosts expose it, embedders
//!   cannot bypass it.

pub mod cat_baker;
/// Cat-art v4 codegen (docs/cat-art-v4-design.md §1): the generator that turns the
/// semantic glyph asset TOMLs into the checked-in [`cat_glyphs_gen`] const drawlists.
pub mod cat_glyphs_codegen;
/// `@generated` — the checked-in const glyph drawlists (do not edit by hand). Produced
/// by `cargo run -p aterm-effects --example gen_cat_glyphs`; kept honest by the
/// `cat_glyphs_gen_matches_assets` drift test in [`cat_glyphs_codegen`]. Pulled in with
/// `include!` (not `mod`) so `cargo fmt` — whose single source of truth for this file is
/// the generator, not rustfmt — never reflows the const drawlists out from under it.
pub mod cat_glyphs_gen {
    include!("cat_glyphs_gen.rs");
}
pub mod color_math;
pub mod cursor_beam;
pub mod cursor_comet;
pub mod cursor_droplet;
pub mod cursor_fireball;
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
/// THE TYPED-KITTY CAMEO — the standalone toy that appears where you typed the
/// word. Deliberately not the cursor escort ([`kitty_cursor`]): owner, 2026-08-09,
/// "when I type 'kitty' … I want THE kitty to appear".
pub mod kitty_cameo;
/// The rainbow kitty that flies in front of the cursor on the `rainbow kitty`
/// trail style — its art, pose, and exit choreography.
pub mod kitty_cursor;
/// The rainbow kitty **pet** — the full-body companion that inhabits the text
/// plane and chases the caret: gait, behaviour, and screen awareness. The
/// decision twin of [`kitty_cursor`], for the pet art roster rather than the
/// flying head.
pub mod kitty_pet;
pub mod kitty_registry;
/// The kitty's SING-ALONG — the held-key celebration: repeat detector +
/// beat clock + the ♪/♫ music-note sprite field (see the module doc).
pub mod kitty_sing;
pub mod matrix_overlay;
pub mod matrix_rain;
pub mod nova;
/// The mushroom cloud — the rarest f-bomb detonation tier.
pub mod nuke;
/// The pet roster's bake path: one authored full-body pose → an exact-size
/// RGBA tile, handed to the shared cat atlas through `CatBaker::host_tile`.
pub mod pet_baker;
/// `@generated` — the checked-in const drawlists for the PET roster (do not edit
/// by hand). Produced by `cargo run -p aterm-effects --example gen_pet_glyphs`;
/// kept honest by the `pet_glyphs_gen_matches_assets` drift test. `include!`d for
/// the same reason [`cat_glyphs_gen`] is.
pub mod pet_glyphs_gen {
    include!("pet_glyphs_gen.rs");
}
pub mod pipeline;
pub mod spec;
pub mod supernova;
/// Tone-of-typing — the tiny hashed-char-n-gram neural classifier that turns
/// the line being typed into a coarse mood hint for `trail_sound`'s tone
/// tables (`docs`: see the module header; trained offline by
/// `examples/tone_train.rs`, weights shipped as a verified binary asset).
pub mod tone;
/// Trail Packs — user-generated cursor trails compiled from bounded, fail-closed
/// TOML into the `Copy` [`trail_pack::TrailParams`] the cursor-glow engine's
/// custom interpreter drives (`docs/trail-packs-design.md`). The strict
/// contribution twin of the sparkle [`spec`] Toy Pack lane.
pub mod trail_pack;
pub mod trail_sound;
pub mod trail_sweep;
pub mod typing_momentum;
pub mod word_decorations;

/// Sparkle Words v3 §4: the orca class is SUSPENDED — a soft gate, single
/// source of truth. Both resolvers (the native `Config::sparkle_deco_config`
/// and the web `EffectsPipeline::set_sparkle_classes`) AND their orca gate
/// with `!ORCA_SUSPENDED`; the engine, lexicon entries, splash emitter, and
/// engine unit tests (which build `DecoConfig` directly) stay intact and
/// green. Re-enable = flip this const (the orca redo then rides the §6
/// framework as an `Orcas` collection).
pub const ORCA_SUSPENDED: bool = true;
