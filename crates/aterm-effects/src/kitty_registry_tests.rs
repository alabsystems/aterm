// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// The raw ramp stop `i` as 0–255 float channels.
const fn raw_stop(i: u8) -> (f32, f32, f32) {
    let hex = crate::cat_baker::COAT_RAMP[i as usize];
    (
        ((hex >> 16) & 0xff) as f32,
        ((hex >> 8) & 0xff) as f32,
        (hex & 0xff) as f32,
    )
}

/// Euclidean RGB distance over 0–255 channels, the [`coat_distance`] unit.
fn dist(a: (f32, f32, f32), b: (f32, f32, f32)) -> f32 {
    ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2) + (a.2 - b.2).powi(2)).sqrt()
}

/// One cell of the metric, re-derived from the live colour math: the min over
/// the raw ramp and `cat_baker`'s dark-background lift.
fn live_coat_distance(a: u8, b: u8) -> f32 {
    let lifted = |i: u8| {
        let (r, g, b) = crate::cat_baker::dark_bg_coat_stop(i);
        (r * 255.0, g * 255.0, b * 255.0)
    };
    dist(raw_stop(a), raw_stop(b)).min(dist(lifted(a), lifted(b)))
}

/// Tolerance 0.01: the lift's bisection uses `powf`, which is not correctly rounded on
/// every platform.
#[test]
fn coat_distance_table_is_pinned_to_the_live_lift() {
    for a in 0..16u8 {
        for b in 0..16u8 {
            let live = live_coat_distance(a, b);
            assert!(
                (coat_distance(a, b) - live).abs() < 1e-2,
                "cell ({a},{b}): table {} vs live {live}",
                coat_distance(a, b)
            );
        }
    }
}

/// The flagship coats clear [`SUFFICIENT_DIFFERENCE`] by measurement, with 4× headroom.
#[test]
fn flagship_trio_is_always_sufficiently_different() {
    let coats = [
        ("claude", KittyLook::for_app("claude").coat),
        ("codex", KittyLook::for_app("codex").coat),
        ("agy", KittyLook::for_app("agy").coat),
    ];
    for i in 0..coats.len() {
        for j in i + 1..coats.len() {
            let d = coat_distance(coats[i].1, coats[j].1);
            assert!(
                d >= 4.0 * SUFFICIENT_DIFFERENCE,
                "{}–{}: flagship coats keep 4x headroom (got {d})",
                coats[i].0,
                coats[j].0
            );
        }
    }
}

#[test]
fn look_normalization_never_leaves_an_overlay_as_the_base() {
    let default = KittyLook::default();
    let repaired = KittyLook {
        variant: CatGlyphId::AccBow,
        accessory: Some(CatGlyphId::AccCrown),
        coat: u8::MAX,
        iris: u8::MAX,
        age: CatAge::Kitten,
    }
    .normalized();

    assert_eq!(repaired.variant, default.variant);
    assert_eq!(GLYPHS[repaired.variant as usize].kind, GlyphKind::Head);
    assert_eq!(repaired.accessory, None);
    assert_eq!((repaired.coat, repaired.iris), (15, 7));
    assert_eq!(repaired.age, CatAge::Kitten);

    let special = KittyLook {
        variant: CatGlyphId::SpecSleeping,
        accessory: Some(CatGlyphId::AccBow),
        ..default
    }
    .normalized();
    assert_eq!(special.variant, CatGlyphId::SpecSleeping);
    assert_eq!(special.accessory, None);
}

/// The registry keys are unique and stable: renaming one orphans persisted rows.
#[test]
fn registry_keys_are_unique_and_pinned() {
    let type_keys: Vec<&str> = KittyType::ALL.iter().map(|t| t.config_key()).collect();
    assert_eq!(
        type_keys,
        [
            "head_peek",
            "head_tilt",
            "head_paw_left",
            "head_paw_right",
            "head_two_paws",
            "paw_single",
            "paw_double",
            "paw_classic"
        ],
        "shown-type keys are pinned"
    );
    let magic_keys: Vec<&str> = KittyMagic::ALL.iter().map(|m| m.config_key()).collect();
    assert_eq!(
        magic_keys,
        ["none", "fortune", "nebula", "butterfly", "sakura"],
        "magic keys are pinned"
    );
    let mut dedup = type_keys.clone();
    dedup.sort_unstable();
    dedup.dedup();
    assert_eq!(dedup.len(), type_keys.len(), "keys must be unique");
}

#[test]
fn launch_kitty_is_seed_pure_normalized_head_and_bare() {
    for seed in (0..4096u64).chain([u64::MAX, 0x8000_0000_0000_0000]) {
        let a = KittyLook::for_launch(seed);
        assert_eq!(a, KittyLook::for_launch(seed), "seed {seed}: pure");
        assert_eq!(a, a.normalized(), "seed {seed}: normalized");
        assert_eq!(
            GLYPHS[a.variant as usize].kind,
            GlyphKind::Head,
            "seed {seed}: the launch floor rolls heads only"
        );
        assert!(a.accessory.is_none(), "seed {seed}: no free accessory");
    }
    let distinct: std::collections::HashSet<_> = (0..64u64)
        .map(|s| {
            let l = KittyLook::for_launch(s);
            (l.variant, l.coat, l.iris, l.age as u8)
        })
        .collect();
    assert!(
        distinct.len() >= 32,
        "seeds get visibly different kitties, got {} distinct of 64",
        distinct.len()
    );
    assert_ne!(
        KittyLook::for_launch(0),
        KittyLook::default(),
        "a launch kitty is not the one shared default cat"
    );
}

#[test]
fn app_basename_strips_paths_prefixes_and_case() {
    assert_eq!(app_basename("cargo build"), Some("cargo".into()));
    assert_eq!(
        app_basename("/usr/local/bin/Claude --resume"),
        Some("claude".into())
    );
    assert_eq!(
        app_basename(r"C:\Tools\Codex.exe run"),
        Some("codex".into())
    );
    assert_eq!(
        app_basename("sudo env FOO=bar npx somenewtool --flag"),
        Some("somenewtool".into()),
        "every transparent prefix defers to the token after it"
    );
    assert_eq!(
        app_basename(r"SUDO.EXE ENV.exe NPX.EXE C:\Bin\Agy.EXE --version"),
        Some("agy".into()),
        "case-insensitive .exe and launcher prefixes strip cleanly"
    );
    assert_eq!(
        app_basename("sudo -E cargo test"),
        Some("cargo".into()),
        "launcher flags are plumbing, not apps"
    );
    assert_eq!(
        app_basename("RUST_LOG=debug ./run.sh"),
        Some("run.sh".into())
    );
    assert_eq!(app_basename(""), None);
    assert_eq!(app_basename("   "), None);
    assert_eq!(
        app_basename("FOO=bar"),
        None,
        "assignments alone name no app"
    );
}

/// `for_app` never mints an accessory, for flagships and the hash path;
/// the hash path is stable and deals renderable heads only.
#[test]
fn for_app_never_emits_an_accessory() {
    for id in ["claude", "codex", "agy"] {
        assert_eq!(KittyLook::for_app(id).accessory, None);
    }
    for n in 0..64 {
        let name = format!("tool{n}");
        let look = KittyLook::for_app(&name);
        assert_eq!(look.accessory, None, "{name}");
        assert_eq!(look, KittyLook::for_app(&name), "{name}: stable");
        assert_eq!(look, look.normalized(), "{name}: renderable");
        assert_eq!(
            GLYPHS[look.variant as usize].kind,
            GlyphKind::Head,
            "{name}"
        );
    }
}
