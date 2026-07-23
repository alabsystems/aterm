// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Prelude for convenient imports.
//!
//! This module is a convenience import surface, not the canonical API.
//!
//! Re-exports commonly used types for ergonomic imports:
//!
//! ```text
//! use aterm_core::prelude::*;
//! ```
//!
//! Prefer `aterm_core::<module>::...` for library code. Use `prelude::*` when
//! you explicitly want a convenience import bag.
//!
//! ## Stable Convenience Exports (Always Available)
//!
//! | Module | Types |
//! |--------|-------|
//! | `checkpoint` | `CheckpointConfig`, `CheckpointHeader`, `CheckpointManager`, `CheckpointManagerExt`, `CheckpointTerminal`, `CheckpointVersion`, `CHECKPOINT_MAGIC` |
//! | `config` | `BiDiMode`, `ConfigChange`, `DiskBackendConfig`, `ScrollbackBackend`, `TerminalConfig` |
//! | `grid` | `Cell`, `CellFlags`, `Cursor`, `Damage`, `Grid`, `PackedColor`, `PackedColors`, `Row`, `RowFlags` |
//! | `scrollback` | `Line`, `Scrollback` |
//! | `search` | `SearchIndex`, `SearchMatch`, `TerminalSearch` |
//! | `terminal` | `ColorPalette`, `CursorStyle`, `Rgb`, `ShellState`, `Terminal`, `TerminalModes` |
//! | `ui` | `CallbackId`, `Event`, `EventKind`, `TerminalId`, `TerminalState`, `UIBridge`, `UIError`, `UIState`, `MAX_CALLBACKS`, `MAX_QUEUE` |
//!
//! ## Aliased Types
//!
//! To avoid naming collisions, some types are re-exported with prefixes:
//!
//! | Original | Alias |
//! |----------|-------|
//! No aliased types are currently exported by the prelude.
//!
//! ## Fuzzing-Only Exports
//!
//! Available only with `cfg(fuzzing)`:
//!
//! - `deserialize_lines`, `serialize_lines`
//! - `SixelDecoder`, `MAX_COLOR_REGISTERS`, `SIXEL_MAX_DIMENSION`
//!
//! Essential convenience imports for downstream crates, benches, tests, and fuzz.
//!
//! Removed from prelude (accessible via full module paths):
//! search internals (`BloomFilter`, `SearchDirection`), grid internals
//! (`LineDamageBounds`, `LineSize`), terminal internals (callbacks, marks,
//! shell events), and full gpu/embeddings/jsonrpc groups.
//!
//! Keep tables in sync by running: `aterm audit docs`.

// -- stable convenience exports from public aterm-core modules --
pub use crate::config::{
    BiDiMode, ConfigChange, DiskBackendConfig, ScrollbackBackend, TerminalConfig,
};

pub use crate::grid::{
    Cell, CellFlags, Cursor, Damage, Grid, PackedColor, PackedColors, Row, RowFlags,
};

pub use crate::scrollback::{Line, Scrollback};

pub use crate::search::{SearchIndex, SearchMatch, TerminalSearch};

pub use crate::terminal::{ColorPalette, CursorStyle, Rgb, ShellState, Terminal, TerminalModes};

pub use crate::ui::{
    CallbackId, Event, EventKind, MAX_CALLBACKS, MAX_QUEUE, TerminalId, TerminalState, UIBridge,
    UIError, UIState,
};

// MCP prelude re-exports removed — MCP system removed.

// -- fuzzing-only: pub(crate) types needed by fuzz targets --
// `cfg(fuzzing)` is set by cargo-fuzz at build time.
#[cfg(fuzzing)]
pub use crate::scrollback::{deserialize_lines, deserialize_lines_strict, serialize_lines};
#[cfg(fuzzing)]
pub use crate::sixel::{MAX_COLOR_REGISTERS, SIXEL_MAX_DIMENSION, SixelDecoder};
