// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#![cfg_attr(all(anchor_rules, any(test, debug_assertions)), feature(macro_attr))]

//! Host-agnostic visual-effects engines shared by native and web embedders.

/// The animal roster's bake path.
pub mod animal_baker;
/// The typing-momentum glow as an Aos task.
pub mod aos_momentum;
/// `@generated` animal drawlists.
pub mod animal_glyphs_gen {
    include!("animal_glyphs_gen.rs");
}
pub mod cat_baker;
/// The examples share this generator source with the unit drift/schema tests.
#[cfg(test)]
mod cat_glyphs_codegen;
/// `@generated` cat drawlists.
pub mod cat_glyphs_gen {
    include!("cat_glyphs_gen.rs");
}
/// The top line's marks.
pub mod cli_marks;
pub mod color_math;
/// Per-surface [`kitty_pet`] companion driver.
pub mod companion;
/// Which cat the companions wear ([`companion_look::LookPolicy`]).
pub mod companion_look;
/// The companions a window keeps beside its resident pet ([`companions::Companions`]).
pub mod companions;
/// The dog roster's bake path.
pub mod dog_baker;
/// The typed-word dog cameo that trots up when you type `dog` past the typed-a-lot gate.
pub mod dog_cameo;
/// `@generated` dog drawlists.
pub mod dog_glyphs_gen {
    include!("dog_glyphs_gen.rs");
}
/// The original cursor aurora, kept as the `classic` trail style.
pub mod classic_wake;
pub mod cursor_beam;
pub mod cursor_comet;
pub mod cursor_droplet;
/// The cursor family's frame step.
pub mod cursor_fx;
pub mod cursor_glow;
pub mod cursor_phaser;
pub mod cursor_rainbow;
pub mod cursor_trail;
mod effect_util;
/// WCAG 2.3.1's general-flash bound every flash rate here is held under.
pub use effect_util::TWINKLE_FLASH_BOUND_HZ;
/// Hardened, bounded file admission shared by every native visual-feed loader.
pub mod file_feed;
pub mod genome;
/// The harness card.
pub mod harness_card;
/// The agents panel's glass.
pub mod harness_panel;
/// Host boundary contract for effect engines.
pub mod host;
/// The integer bag-of-ids MLP job kind and its CPU reference (Aos M5a).
#[cfg(any(test, debug_assertions, feature = "int-bag-mlp"))]
pub mod int_bag_mlp;
/// The rainbow kitty that flies in front of the cursor on the `rainbow kitty` trail style.
pub mod kitty_cursor;
/// The rainbow kitty pet.
pub mod kitty_pet;
pub mod kitty_registry;
/// The kitty's sing-along for held keys.
pub mod kitty_sing;
/// The typed-word detector: a feline word typed at the prompt records a Kitty Log sighting.
pub mod kitty_summon;
pub mod matrix_rain;
/// Motion policy: the single accessibility gate for decorative animation.
pub mod motion;
pub mod nova;
/// The mushroom cloud.
pub mod nuke;
/// Prism wake for program output.
pub mod output_streak;
/// The pet roster's bake path.
pub mod pet_baker;
mod pet_stroke;
/// Bounded, coherent terminal occupancy and text-free content anchors.
pub mod pet_world;
/// `@generated` pet drawlists.
pub mod pet_glyphs_gen {
    include!("pet_glyphs_gen.rs");
}
pub mod cursor_momentum;
pub mod pane;
pub mod pipeline;
/// Rainbow kitty ([`rainbow_kitty::Engine`]).
pub mod rainbow_kitty;
/// Glint wave field for Claude Code's mascot ripple.
pub mod ripple;
/// Sound-cue policy.
pub mod sound_policy;
pub mod spec;
/// The rainbow family's colour law ([`spectrum::spectrum`], [`spectrum::spectrum_snap`]).
pub mod spectrum;
/// The trail engines' state words and the frame's light report, for `lab trail-diff`
/// ([`state_words::StateWords`], [`state_words::ArmsDrawn`]).
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
pub mod state_words;
pub mod supernova;
/// The tests' replay pool ([`test_pool::par_map`]), for the unit tests and the integration suite.
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
pub mod test_pool;
/// Hashed char-n-gram classifier mapping typed text to a mood hint.
pub mod tone;
/// The tone-of-typing tracker.
pub mod tone_infer;
/// tone-int v1, the tone classifier as an integer bag-of-ids MLP (Aos M5a).
#[cfg(any(test, debug_assertions, feature = "int-bag-mlp"))]
pub mod tone_int;
pub mod trail_sound;
pub mod trail_sweep;
mod trick_flash;
/// Kitty-command listener.
pub mod typed_tricks;
pub mod typing_momentum;
/// The ⬆️'s hover ring on a composer's top rule.
pub mod up_ring;
pub mod word_decorations;

#[cfg(test)]
#[path = "../tests/conformance_trick_flash.rs"]
mod conformance_trick_flash;
#[cfg(test)]
#[path = "../tests/conformance_trick_listener.rs"]
mod conformance_trick_listener;
