// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Private `Grid` state pods.
//!
//! These keep `Grid` as the stable public facade while separating storage,
//! cursor/region, and presentation concerns.

mod scrollback;
mod storage;

pub use crate::{GridCursorState, GridPresentationState};
pub use storage::GridStorage;
pub(crate) use storage::{PendingScrollbackSettings, UNLIMITED_RING_SCROLLBACK};
