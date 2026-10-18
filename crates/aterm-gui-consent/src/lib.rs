// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! aterm-gui's access card, extracted below the window crate: its
//! pure decision and words, the marker files that record a person's answer,
//! and the durability flushes those writes share. Nothing here reaches `App`,
//! a window or the event loop, so a one-line edit in aterm-gui recompiles none
//! of it. aterm-gui re-exports each module under its old name.

/// Config-directory marker files: the exclusive, bounded, never-written-through
/// read/write rules.
pub mod config_marker;
/// The macOS access card's decision and words (design §3.4).
pub mod consent_card;
/// The durability flushes every atomic publish and the crash journal take.
pub mod durable;
