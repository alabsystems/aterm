//! libm in pure Rust
#![no_std]
#![cfg_attr(intrinsics_enabled, allow(internal_features))]
#![cfg_attr(intrinsics_enabled, feature(core_intrinsics))]
#![cfg_attr(
    all(intrinsics_enabled, target_family = "wasm"),
    feature(wasm_numeric_instr)
)]
#![cfg_attr(f128_enabled, feature(f128))]
#![cfg_attr(f16_enabled, feature(f16))]
#![allow(clippy::assign_op_pattern)]
#![allow(clippy::deprecated_cfg_attr)]
#![allow(clippy::eq_op)]
#![allow(clippy::excessive_precision)]
#![allow(clippy::float_cmp)]
#![allow(clippy::int_plus_one)]
#![allow(clippy::just_underscores_and_digits)]
#![allow(clippy::many_single_char_names)]
#![allow(clippy::mixed_case_hex_literals)]
#![allow(clippy::needless_late_init)]
#![allow(clippy::needless_return)]
#![allow(clippy::unreadable_literal)]
#![allow(clippy::zero_divided_by_zero)]
// Local vendor addition: libm deliberately re-implements float/int helpers that
// newer std releases stabilize under the same names (`widen`, `NAN`, inherent
// mask constants, ...); the collision lint fires on more such helpers with each
// toolchain advance (16+ sites across int_traits/big/cbrt on rustc 1.99-dev).
// Inherent-impl semantics keep the local definitions winning, so the blanket
// allow stays; individually collision-proof sites (e.g. SIGN_MASK computing
// from the macro's declared width) are still preferred where cheap.
#![allow(unstable_name_collisions)]
#![forbid(unsafe_op_in_unsafe_fn)]

mod libm_helper;
mod math;

use core::{f32, f64};

pub use libm_helper::*;

pub use self::math::*;
