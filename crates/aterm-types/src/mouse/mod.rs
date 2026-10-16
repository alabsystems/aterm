// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Shared mouse encoding: tracking modes, encoding formats, buttons, modifier masks and the
//! pure byte encoders.

mod encode;
mod types;

pub use encode::{encode_mouse, encode_sgr, encode_urxvt, encode_utf8, encode_x10};
pub use types::{ALT_MASK, CTRL_MASK, MouseButton, MouseEncoding, MouseMode, SHIFT_MASK, WheelDir};
