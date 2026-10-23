// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Combined VT conformance test binary (`autotests = false` in `Cargo.toml`, so
//! every `tests/*.rs` file must be a `mod` here or a standalone `[[test]]`).

mod conformance;
mod kitty_mouse;
mod osc8_schemes;
mod rect_ops;
mod responses;
mod sixel;
mod vt_categories;
mod vt_categories2;
mod vt_categories3;
mod vt_oracle;
mod xterm_alternate_finals;
mod xterm_differential_regressions;
