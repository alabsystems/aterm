// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Grid state pods: presentation and cursor/region state.

mod cursor;
mod presentation;

pub use cursor::GridCursorState;
pub use presentation::{AbsoluteRowUpdate, GridPresentationState};
