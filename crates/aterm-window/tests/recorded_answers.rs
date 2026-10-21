// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Added by the aterm project in 2026 to its copy of winit 0.30.13
// (see the repository NOTICE).

//! Recorded answers for the leaf types this crate absorbs in winit-core step 5
//! (docs/AUDIT-third-party-2026-10-15.md, WC.5): `aterm_window::dpi` (from `dpi` 0.1.2),
//! `aterm_window::window::CursorIcon` (from `cursor-icon` 1.2.0, the six icons aterm
//! sets, and the eight edge-resize icons LW.5 added) and the `bitflags!` types
//! (`bitflags` 2).
//!
//! Every expected value below was recorded from the third-party crates while
//! they were still what these paths named. The absorb then keeps this file
//! unchanged: what the packages answered is what the first-party code must
//! answer. The rows that convert to an integer are the ones a re-implementation
//! gets wrong, so `the_rounding_rows_reject_every_plausible_wrong_rounding`
//! holds the table to telling round-half-away-from-zero apart from the
//! alternatives, and the table cannot pass vacuously.

use std::panic::catch_unwind;

use aterm_window::dpi::{
    validate_scale_factor, LogicalPosition, LogicalSize, PhysicalPosition, PhysicalSize, Pixel,
    Position, Size,
};
use aterm_window::keyboard::ModifiersState;
use aterm_window::window::CursorIcon;
#[cfg(windows)]
use aterm_window::window::WindowButtons;

/// One recorded conversion: `(input, scale factor, output)`, both as `(x, y)`.
type Row<I, O> = ((I, I), f64, (O, O));

/// A rounding a carve might use in place of the recorded one.
type Rounding = fn(f64) -> f64;

/// `(icon, CSS name, XCursor fallback names, Debug)`.
type IconRow = (CursorIcon, &'static str, &'static [&'static str], &'static str);

/// `(logical, scale factor, physical)`: `LogicalSize<f64>::to_physical::<u32>`.
const LOGICAL_TO_PHYSICAL_U32: &[Row<f64, u32>] = &[
    ((100.5, 33.3), 1.0, (101, 33)),
    ((100.0, 50.0), 1.5, (150, 75)),
    ((10.25, 10.75), 2.0, (21, 22)),
    ((0.5, 1.5), 1.0, (1, 2)),
    ((2.5, 3.5), 1.0, (3, 4)),
    ((-1.6, 0.4), 1.0, (0, 0)),
    ((1.0e10, 7.49), 1.0, (u32::MAX, 7)),
];

/// `(physical, scale factor, logical)`: `PhysicalSize<u32>::to_logical::<u32>`.
const PHYSICAL_TO_LOGICAL_U32: &[Row<u32, u32>] =
    &[((3, 5), 2.0, (2, 3)), ((301, 7), 2.0, (151, 4)), ((1, 1), 3.0, (0, 0))];

/// `(logical, scale factor, physical)`: `LogicalPosition<f64>::to_physical::<i32>`.
const LOGICAL_TO_PHYSICAL_I32: &[Row<f64, i32>] = &[
    ((-2.5, 2.5), 1.0, (-3, 3)),
    ((1.25, -1.25), 2.0, (3, -3)),
    ((-0.5, 0.49), 1.0, (-1, 0)),
    ((-1.0e10, 1.0e10), 1.0, (i32::MIN, i32::MAX)),
];

#[test]
fn sizes_and_positions_convert_as_recorded() {
    for &((w, h), sf, want) in LOGICAL_TO_PHYSICAL_U32 {
        let got: PhysicalSize<u32> = LogicalSize::new(w, h).to_physical(sf);
        assert_eq!((got.width, got.height), want, "LogicalSize({w}, {h}) at {sf}");
    }
    for &((w, h), sf, want) in PHYSICAL_TO_LOGICAL_U32 {
        let got: LogicalSize<u32> = PhysicalSize::new(w, h).to_logical(sf);
        assert_eq!((got.width, got.height), want, "PhysicalSize({w}, {h}) at {sf}");
    }
    for &((x, y), sf, want) in LOGICAL_TO_PHYSICAL_I32 {
        let got: PhysicalPosition<i32> = LogicalPosition::new(x, y).to_physical(sf);
        assert_eq!((got.x, got.y), want, "LogicalPosition({x}, {y}) at {sf}");
    }

    let logical: LogicalSize<f64> = PhysicalSize::new(301u32, 3).to_logical(2.0);
    assert_eq!(logical, LogicalSize::new(150.5, 1.5));
    let logical: LogicalPosition<f64> = PhysicalPosition::new(-7i32, 9).to_logical(2.0);
    assert_eq!(logical, LogicalPosition::new(-3.5, 4.5));
    let physical: PhysicalSize<f64> = LogicalSize::new(1.5f64, 2.0).to_physical(1.25);
    assert_eq!(physical, PhysicalSize::new(1.875, 2.5));
}

#[test]
fn casts_round_half_away_from_zero_and_saturate() {
    assert_eq!(
        PhysicalPosition::new(-3.5f64, 1.0e10).cast::<i32>(),
        PhysicalPosition::new(-4, i32::MAX)
    );
    assert_eq!(PhysicalPosition::new(f64::NAN, 300.7).cast::<u8>(), PhysicalPosition::new(0, 255));
    assert_eq!(
        PhysicalPosition::new(-129.4f64, 127.5).cast::<i8>(),
        PhysicalPosition::new(-128, 127)
    );
    assert_eq!(PhysicalSize::new(65535.5f64, -0.5).cast::<u16>(), PhysicalSize::new(65535, 0));
    assert_eq!(PhysicalPosition::new(-0.5f64, 0.49).cast::<i16>(), PhysicalPosition::new(-1, 0));
    assert_eq!(LogicalSize::new(2.5f64, -2.5).cast::<i32>(), LogicalSize::new(3, -3));
    assert_eq!(LogicalPosition::new(7u32, 9).cast::<f64>(), LogicalPosition::new(7.0, 9.0));
    let narrowed = PhysicalSize::new(0.1f64, 1.0e40).cast::<f32>();
    assert_eq!((narrowed.width, narrowed.height), (0.1f32, f32::INFINITY));
    assert_eq!(<f64 as Pixel>::cast::<u32>(2.5), 3);
    assert_eq!(<f64 as Pixel>::from_f64(-0.25), -0.25);
    assert_eq!(<u8 as Pixel>::from_f64(254.5), 255);
    assert_eq!(<i32 as Pixel>::cast::<f32>(-3), -3.0);
}

#[test]
fn size_and_position_enums_convert_as_recorded() {
    let logical = Size::Logical(LogicalSize::new(10.5, 20.0));
    assert_eq!(logical.to_physical::<u32>(2.0), PhysicalSize::new(21, 40));
    assert_eq!(logical.to_logical::<u32>(3.0), LogicalSize::new(11, 20));
    let physical = Size::Physical(PhysicalSize::new(21, 41));
    assert_eq!(physical.to_logical::<f64>(2.0), LogicalSize::new(10.5, 20.5));
    assert_eq!(physical.to_physical::<u32>(3.0), PhysicalSize::new(21, 41));

    let clamped = Size::clamp(
        LogicalSize::new(50.0, 500.0),
        LogicalSize::new(100.0, 100.0),
        LogicalSize::new(200.0, 200.0),
        2.0,
    );
    assert_eq!(clamped, Size::Physical(PhysicalSize::new(200, 400)));

    let position = Position::Logical(LogicalPosition::new(1.25, -1.25));
    assert_eq!(position.to_physical::<i32>(2.0), PhysicalPosition::new(3, -3));
    let position = Position::Physical(PhysicalPosition::new(3, -3));
    assert_eq!(position.to_logical::<f64>(2.0), LogicalPosition::new(1.5, -1.5));

    assert_eq!(Size::from(PhysicalSize::new(1.5f64, 2.5)), Size::Physical(PhysicalSize::new(2, 3)));
    assert_eq!(
        Position::from(LogicalPosition::new(1i32, 2)),
        Position::Logical(LogicalPosition::new(1.0, 2.0))
    );
    assert_eq!(Size::new(LogicalSize::new(4u32, 5)), Size::Logical(LogicalSize::new(4.0, 5.0)));
    assert_eq!(
        Position::new(PhysicalPosition::new(-1.5f64, 1.5)),
        Position::Physical(PhysicalPosition::new(-2, 2))
    );
}

#[test]
fn tuple_conversions_round_as_recorded() {
    assert_eq!(PhysicalSize::<u32>::from((1.6f64, 2.4f64)), PhysicalSize::new(2, 2));
    assert_eq!(<(f64, f64)>::from(PhysicalPosition::new(3i32, -4)), (3.0, -4.0));
    assert_eq!(<(f64, f64)>::from(PhysicalSize::new(3u32, 4)), (3.0, 4.0));
    assert_eq!(LogicalSize::<f64>::from((2u32, 3u32)), LogicalSize::new(2.0, 3.0));
}

#[test]
fn scale_factors_are_validated_as_recorded() {
    for (sf, valid) in [
        (1.0, true),
        (0.5, true),
        (f64::MIN_POSITIVE, true),
        (0.0, false),
        (-0.0, false),
        (-1.0, false),
        (f64::NAN, false),
        (f64::INFINITY, false),
        (f64::MIN_POSITIVE / 2.0, false),
    ] {
        assert_eq!(validate_scale_factor(sf), valid, "{sf}");
    }
    // Every scaling conversion asserts the factor; a bad one panics, never clamps.
    assert!(catch_unwind(|| LogicalSize::new(1.0f64, 1.0).to_physical::<u32>(0.0)).is_err());
    assert!(catch_unwind(|| PhysicalSize::new(1u32, 1).to_logical::<f64>(f64::NAN)).is_err());
    assert!(catch_unwind(|| LogicalPosition::new(1.0f64, 1.0).to_physical::<i32>(-2.0)).is_err());
    assert!(
        catch_unwind(|| PhysicalPosition::new(1i32, 1).to_logical::<f64>(f64::INFINITY)).is_err()
    );
}

#[test]
fn dpi_derives_answer_as_recorded() {
    assert_eq!(format!("{:?}", PhysicalSize::new(1u32, 2)), "PhysicalSize { width: 1, height: 2 }");
    assert_eq!(
        format!("{:?}", LogicalPosition::new(1.5f64, -2.0)),
        "LogicalPosition { x: 1.5, y: -2.0 }"
    );
    assert_eq!(
        format!("{:?}", Size::Physical(PhysicalSize::new(3, 4))),
        "Physical(PhysicalSize { width: 3, height: 4 })"
    );
    assert_eq!(PhysicalSize::<u32>::default(), PhysicalSize::new(0, 0));
    assert_eq!(LogicalPosition::<f64>::default(), LogicalPosition::new(0.0, 0.0));
    assert!(PhysicalSize::new(1u32, 5) < PhysicalSize::new(2u32, 0));
    assert!(PhysicalPosition::new(1i32, -5) < PhysicalPosition::new(1i32, 0));
}

/// The negative control. Each mutant is a plausible wrong rounding a carve
/// could ship; the integer rows above must refute every one of them, or they
/// would pass a carve that rounds differently.
#[test]
fn the_rounding_rows_reject_every_plausible_wrong_rounding() {
    let mutants: [(&str, Rounding); 4] = [
        ("truncation", f64::trunc),
        ("round half to even", f64::round_ties_even),
        ("floor", f64::floor),
        ("ceil", f64::ceil),
    ];
    for (what, round) in mutants {
        let refuted = LOGICAL_TO_PHYSICAL_U32
            .iter()
            .any(|&((w, h), sf, want)| (round(w * sf) as u32, round(h * sf) as u32) != want)
            && LOGICAL_TO_PHYSICAL_I32
                .iter()
                .any(|&((x, y), sf, want)| (round(x * sf) as i32, round(y * sf) as i32) != want)
            && PHYSICAL_TO_LOGICAL_U32.iter().any(|&((w, h), sf, want)| {
                (round(f64::from(w) / sf) as u32, round(f64::from(h) / sf) as u32) != want
            });
        assert!(refuted, "the recorded rows cannot tell {what} from the recorded rounding");
    }
    // And the recorded rounding itself reproduces them through the same arithmetic,
    // so the control is measuring the rounding and nothing else.
    assert!(LOGICAL_TO_PHYSICAL_U32
        .iter()
        .all(|&((w, h), sf, want)| ((w * sf).round() as u32, (h * sf).round() as u32) == want));
}

/// The icons aterm sets: six from winit-core step 5, and the eight edge-resize icons of
/// aterm's own Wayland frame (LW.5, 2026-10-17), recorded from cursor-icon 1.2.0's `name` and
/// `alt_names` by a scratch crate outside the workspace.
const CURSOR_ICONS: [IconRow; 14] = [
    (
        CursorIcon::Default,
        "default",
        &["left_ptr", "arrow", "top_left_arrow", "left_arrow"],
        "Default",
    ),
    (CursorIcon::Pointer, "pointer", &["hand2", "hand1", "hand", "pointing_hand"], "Pointer"),
    (CursorIcon::Text, "text", &["xterm", "ibeam"], "Text"),
    (CursorIcon::Grabbing, "grabbing", &["closedhand"], "Grabbing"),
    (
        CursorIcon::ColResize,
        "col-resize",
        &["split_h", "h_double_arrow", "sb_h_double_arrow"],
        "ColResize",
    ),
    (
        CursorIcon::RowResize,
        "row-resize",
        &["split_v", "v_double_arrow", "sb_v_double_arrow"],
        "RowResize",
    ),
    (CursorIcon::NResize, "n-resize", &["top_side"], "NResize"),
    (CursorIcon::NeResize, "ne-resize", &["top_right_corner"], "NeResize"),
    (CursorIcon::EResize, "e-resize", &["right_side"], "EResize"),
    (CursorIcon::SeResize, "se-resize", &["bottom_right_corner"], "SeResize"),
    (CursorIcon::SResize, "s-resize", &["bottom_side"], "SResize"),
    (CursorIcon::SwResize, "sw-resize", &["bottom_left_corner"], "SwResize"),
    (CursorIcon::WResize, "w-resize", &["left_side"], "WResize"),
    (CursorIcon::NwResize, "nw-resize", &["top_left_corner"], "NwResize"),
];

#[test]
fn cursor_icons_answer_as_recorded() {
    for (icon, name, alt_names, debug) in CURSOR_ICONS {
        assert_eq!(icon.name(), name);
        assert_eq!(icon.alt_names(), alt_names, "{name}");
        assert_eq!(format!("{icon:?}"), debug);
    }
    assert_eq!(CursorIcon::default(), CursorIcon::Default);
    // Distinct icons: a carve that collapsed two of them onto one row would
    // still match every name above if the table repeated itself.
    for (i, a) in CURSOR_ICONS.iter().enumerate() {
        for b in &CURSOR_ICONS[i + 1..] {
            assert_ne!(a.0, b.0);
            assert_ne!(a.1, b.1);
        }
    }
}

#[test]
fn flag_types_answer_as_recorded() {
    let named = [
        (ModifiersState::SHIFT, 0b100),
        (ModifiersState::CONTROL, 0b100 << 3),
        (ModifiersState::ALT, 0b100 << 6),
        (ModifiersState::SUPER, 0b100 << 9),
    ];
    let mut all = ModifiersState::empty();
    for (flag, bits) in named {
        assert_eq!(flag.bits(), bits);
        all |= flag;
    }
    assert_eq!(ModifiersState::default(), ModifiersState::empty());
    assert_eq!(ModifiersState::all(), all);
    assert_eq!(ModifiersState::from_bits_truncate(u32::MAX), all);
    assert_eq!(ModifiersState::from_bits(1), None);
    assert_eq!(ModifiersState::from_bits(0b100 << 3), Some(ModifiersState::CONTROL));
    assert_eq!(
        !ModifiersState::SHIFT,
        ModifiersState::CONTROL | ModifiersState::ALT | ModifiersState::SUPER
    );
    assert_eq!(
        (ModifiersState::SHIFT | ModifiersState::ALT) - ModifiersState::SHIFT,
        ModifiersState::ALT
    );
    assert!(ModifiersState::SHIFT < ModifiersState::CONTROL);

    let mods = ModifiersState::SHIFT | ModifiersState::SUPER;
    assert!(mods.shift_key() && mods.super_key() && !mods.control_key() && !mods.alt_key());
    assert!(
        mods.contains(ModifiersState::SHIFT)
            && !mods.contains(ModifiersState::ALT | ModifiersState::SHIFT)
    );
    assert!(mods.intersects(ModifiersState::ALT | ModifiersState::SHIFT));
}

/// `WindowButtons` left macOS with `set_enabled_buttons` (winit-core step 5, 2026-10-16) and
/// Linux (WC.8, 2026-10-21); the Windows backend still reads it.
#[cfg(windows)]
#[test]
fn window_buttons_answer_as_recorded() {
    assert_eq!(WindowButtons::all().bits(), 0b111);
    assert_eq!(WindowButtons::CLOSE.bits() | WindowButtons::MINIMIZE.bits(), 0b011);
    assert_eq!(WindowButtons::MAXIMIZE.bits(), 0b100);
    assert!(WindowButtons::empty().is_empty());
}
