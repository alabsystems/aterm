// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What each block of the merged top deck draws (`docs/DESIGN-top-deck-merged-2026-10-21.md` §5–§7), as pure
//! functions of a [`Snapshot`](crate::model::Snapshot): the facts its painter lays out, kept here so they are
//! tested without a window. One submodule per block or concern.

pub mod chips;
pub mod cpu;
pub mod needs;
pub mod network;
