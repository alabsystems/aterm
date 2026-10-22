// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The colour, shell-integration and title sub-projections of a
//! [`TerminalCheckpoint`](super::TerminalCheckpoint): state an application set
//! that an in-session update must carry.
//!
//! * [`ColorRepr`] carries the colour state as a diff from the host's
//!   configured baseline, so adopting into a terminal whose theme changed
//!   applies the application's overrides (OSC 4, OSC 10/11) on the new theme.
//!   The XTPUSHCOLORS stack travels as [`ColorOverridesRepr`] diffs too, so a
//!   pop after the handoff also lands on the new theme.
//! * [`ShellRepr`] carries the OSC 133/633 phase, the in-progress mark and
//!   block, the still-readable completed marks and blocks, and the monotonic
//!   counters observers key on. Rows are absolute and each grid's absolute row
//!   counter is carried, so a mark names the same line after the handoff and a
//!   command running across the update still completes on its D.
//! * [`TitleRepr`] carries the window title (OSC 0/2), the icon name (OSC 0/1)
//!   and the XTWINOPS title stack (CSI 22/23 t). The successor treats a carried
//!   title as OSC input ([`Terminal::restore_title`]): capped, stripped of
//!   controls, the stack bounded.

use std::collections::VecDeque;

use aterm_types::{ColorPalette, Rgb};

use super::Terminal;
use super::callbacks::{COLOR_STACK_MAX_DEPTH, TITLE_STACK_MAX_DEPTH};
use super::grouped_state::{ColorState, ShellIntegrationPhase};
use super::shell::{
    BlockState, COMMAND_MARKS_MAX, CommandMark, OUTPUT_BLOCKS_MAX, OutputBlock, ShellState,
};
use super::{MAX_COMMANDLINE_BYTES, MAX_CWD_PATH_BYTES, MAX_TITLE_BYTES};

/// One palette entry an application changed (OSC 4 / OSC 21).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct PaletteOverrideRepr {
    /// Palette index.
    pub index: u8,
    /// The colour the application set.
    pub rgb: Rgb,
}

/// A dynamic colour slot whose live value may be "unset" (cursor, selection
/// foreground/background). Present in a [`ColorRepr`] only when the slot
/// differs from the host's configured value; `rgb: None` then means the
/// application explicitly cleared it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ColorSlotRepr {
    /// The live value.
    pub rgb: Option<Rgb>,
}

/// One XTPUSHCOLORS (OSC 30001) stack entry, whole: the shape a parent that
/// predates [`ColorRepr::stack_overrides`] writes. Read (as a diff from this
/// engine's baseline), never written.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ColorStackEntryRepr {
    /// All 256 palette entries, index order.
    pub palette: Vec<Rgb>,
    /// Default foreground.
    pub foreground: Rgb,
    /// Default background.
    pub background: Rgb,
    /// Cursor colour.
    pub cursor: Option<Rgb>,
    /// Selection background.
    pub selection_background: Option<Rgb>,
    /// Selection foreground.
    pub selection_foreground: Option<Rgb>,
}

/// The colour slots an application overrode, as a diff from the host's
/// configured baseline. `Default` is "nothing overridden".
///
/// The one colour-diff form: [`ColorRepr`]'s live slots, each XTPUSHCOLORS
/// stack entry, and what `apply_config` replays over a new theme, so a
/// program's colours survive any baseline change.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ColorOverridesRepr {
    /// Palette entries that differ from the configured palette.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Vec::is_empty")
    )]
    pub palette: Vec<PaletteOverrideRepr>,
    /// Foreground, when it differs from the configured one.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub foreground: Option<Rgb>,
    /// Background, when it differs from the configured one.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub background: Option<Rgb>,
    /// Cursor colour, when it differs from the configured one.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub cursor: Option<ColorSlotRepr>,
    /// Selection background, when it differs from the configured one.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub selection_background: Option<ColorSlotRepr>,
    /// Selection foreground, when it differs from the configured one.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub selection_foreground: Option<ColorSlotRepr>,
}

impl ColorOverridesRepr {
    /// The same diff with at most one palette entry per index (the last listed
    /// wins, as applying the list in order would), in index order, so a wire
    /// entry costs no more than the engine's own capture.
    #[must_use]
    fn normalized(&self) -> Self {
        let mut slots = [None; 256];
        for e in &self.palette {
            slots[usize::from(e.index)] = Some(e.rgb);
        }
        Self {
            palette: (0..=u8::MAX)
                .zip(slots)
                .filter_map(|(index, rgb)| rgb.map(|rgb| PaletteOverrideRepr { index, rgb }))
                .collect(),
            ..self.clone()
        }
    }
}

/// The colour state applications set, as a diff from the configured baseline
/// (see the module docs). `Default` is "nothing overridden".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ColorRepr {
    /// Palette entries that differ from the configured palette.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Vec::is_empty")
    )]
    pub palette: Vec<PaletteOverrideRepr>,
    /// OSC 10 foreground, when it differs from the configured one.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub foreground: Option<Rgb>,
    /// OSC 11 background, when it differs from the configured one.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub background: Option<Rgb>,
    /// OSC 12 cursor colour, when it differs from the configured one.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub cursor: Option<ColorSlotRepr>,
    /// OSC 17 selection background, when it differs from the configured one.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub selection_background: Option<ColorSlotRepr>,
    /// OSC 19 selection foreground, when it differs from the configured one.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub selection_foreground: Option<ColorSlotRepr>,
    /// The XTPUSHCOLORS stack, oldest first, as whole entries. Read only when
    /// `stack_overrides` is empty; never written.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Vec::is_empty")
    )]
    pub stack: Vec<ColorStackEntryRepr>,
    /// The XTPUSHCOLORS stack, oldest first, each entry a diff from the
    /// configured baseline. A parent that predates this field carries `stack`.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Vec::is_empty")
    )]
    pub stack_overrides: Vec<ColorOverridesRepr>,
}

impl ColorRepr {
    /// Whether nothing is overridden (the wire omits the whole projection).
    #[inline]
    #[must_use]
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Whether the carry holds a pushed XTPUSHCOLORS stack, in either shape.
    #[inline]
    #[must_use]
    pub fn has_stack(&self) -> bool {
        !self.stack.is_empty() || !self.stack_overrides.is_empty()
    }

    /// Drop the pushed stack, in either shape.
    #[inline]
    pub fn clear_stack(&mut self) {
        self.stack.clear();
        self.stack_overrides.clear();
    }

    /// The live slots' diff, without the stack.
    #[inline]
    fn overrides(&self) -> ColorOverridesRepr {
        ColorOverridesRepr {
            palette: self.palette.clone(),
            foreground: self.foreground,
            background: self.background,
            cursor: self.cursor,
            selection_background: self.selection_background,
            selection_foreground: self.selection_foreground,
        }
    }
}

impl ColorState {
    /// The palette entry the host configured at `index` (the theme palette, or
    /// xterm's default when the theme sets none).
    #[inline]
    fn configured_palette_entry(&self, index: u8) -> Rgb {
        self.configured_palette
            .as_ref()
            .map_or_else(|| ColorPalette::default_color(index), |p| p.get(index))
    }

    /// The live colours as a diff from the configured baseline: what a program
    /// set (OSC 4/10/11/12/17/19/21) and nothing the theme supplies.
    pub(super) fn capture_overrides(&self) -> ColorOverridesRepr {
        let palette = (0..=u8::MAX)
            .filter_map(|index| {
                let rgb = self.palette().get(index);
                (rgb != self.configured_palette_entry(index))
                    .then_some(PaletteOverrideRepr { index, rgb })
            })
            .collect();
        let slot = |live: Option<Rgb>, configured: Option<Rgb>| {
            (live != configured).then_some(ColorSlotRepr { rgb: live })
        };
        ColorOverridesRepr {
            palette,
            foreground: (self.default_foreground != self.configured_foreground)
                .then_some(self.default_foreground),
            background: (self.default_background != self.configured_background)
                .then_some(self.default_background),
            cursor: slot(self.cursor_color, self.configured_cursor),
            selection_background: slot(
                self.selection_background,
                self.configured_selection_background,
            ),
            selection_foreground: slot(
                self.selection_foreground,
                self.configured_selection_foreground,
            ),
        }
    }

    /// Set every live slot to its configured baseline — the palette only when
    /// `palette` (a caller whose palette writes are gated passes the gate).
    /// The stack is left alone.
    pub(super) fn restore_baseline(&mut self, palette: bool) {
        if palette {
            match self.configured_palette.clone() {
                Some(configured) => *self.palette_mut() = configured,
                None => self.palette_mut().reset(),
            }
        }
        self.default_foreground = self.configured_foreground;
        self.default_background = self.configured_background;
        self.cursor_color = self.configured_cursor;
        self.selection_background = self.configured_selection_background;
        self.selection_foreground = self.configured_selection_foreground;
    }

    /// Write a diff's slots over the live colours — its palette entries only
    /// when `palette`. Slots the diff leaves out keep their live value.
    pub(super) fn apply_overrides(&mut self, overrides: &ColorOverridesRepr, palette: bool) {
        if palette && !overrides.palette.is_empty() {
            let live = self.palette_mut();
            for e in &overrides.palette {
                live.set(e.index, e.rgb);
            }
        }
        if let Some(fg) = overrides.foreground {
            self.default_foreground = fg;
        }
        if let Some(bg) = overrides.background {
            self.default_background = bg;
        }
        if let Some(slot) = overrides.cursor {
            self.cursor_color = slot.rgb;
        }
        if let Some(slot) = overrides.selection_background {
            self.selection_background = slot.rgb;
        }
        if let Some(slot) = overrides.selection_foreground {
            self.selection_foreground = slot.rgb;
        }
    }

    /// A whole (pre-diff) stack entry as a diff from this engine's baseline.
    fn overrides_from_whole(&self, e: &ColorStackEntryRepr) -> ColorOverridesRepr {
        let slot = |saved: Option<Rgb>, configured: Option<Rgb>| {
            (saved != configured).then_some(ColorSlotRepr { rgb: saved })
        };
        ColorOverridesRepr {
            palette: (0..=u8::MAX)
                .zip(e.palette.iter())
                .filter(|&(index, rgb)| *rgb != self.configured_palette_entry(index))
                .map(|(index, rgb)| PaletteOverrideRepr { index, rgb: *rgb })
                .collect(),
            foreground: (e.foreground != self.configured_foreground).then_some(e.foreground),
            background: (e.background != self.configured_background).then_some(e.background),
            cursor: slot(e.cursor, self.configured_cursor),
            selection_background: slot(
                e.selection_background,
                self.configured_selection_background,
            ),
            selection_foreground: slot(
                e.selection_foreground,
                self.configured_selection_foreground,
            ),
        }
    }
}

/// The titles a program set: the window title, the icon name, and the
/// XTWINOPS title stack, whole. `Default` is a terminal nothing has titled.
///
/// Every string is what the engine stores, already capped and stripped of controls;
/// a restore does both again ([`Terminal::restore_title`]), as the wire is untrusted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct TitleRepr {
    /// The window title (OSC 0 or 2) — the one a tab is labelled with.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "String::is_empty")
    )]
    pub window: String,
    /// The icon name (OSC 0 or 1).
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "String::is_empty")
    )]
    pub icon: String,
    /// The title stack, bottom first, as `(icon name, window title)` pairs —
    /// the engine's own order. An entry pushed for one of the two holds an
    /// empty string for the other, read back as a slot that push did not save
    /// when reading the legacy form without [`Self::stack_saved`].
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Vec::is_empty")
    )]
    pub stack: Vec<(String, String)>,
    /// Presence of the icon and window slots in each stack entry, bottom first.
    /// Emitted only when the stack contains a saved empty title, which the legacy
    /// string pairs cannot distinguish from an unsaved slot. A missing entry uses
    /// the legacy rule (a sanitized nonempty string means saved). Older consumers
    /// can still read `stack`, with their original ambiguity for saved empties.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Vec::is_empty")
    )]
    pub stack_saved: Vec<(bool, bool)>,
}

impl TitleRepr {
    /// Whether nothing has been titled.
    #[inline]
    #[must_use]
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// A carried title string, made what the OSC 0/1/2 handler would have stored:
/// capped at [`MAX_TITLE_BYTES`] on a character boundary, then stripped of C0,
/// C1 and bidi controls (`handler_osc::sanitize_title`) — in the handler's own
/// order, so a carried title is never longer than a live one could be.
fn carried_title(text: &str) -> String {
    super::handler_osc::sanitize_title(&text[..text.floor_char_boundary(MAX_TITLE_BYTES)])
}

/// The span fields an OSC 133 command mark and an output block share.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ShellSpanRepr {
    /// Absolute row where the prompt started.
    pub prompt_start_row: u64,
    /// Column where the prompt started.
    pub prompt_start_col: u16,
    /// Absolute row where command input started.
    pub command_start_row: Option<u64>,
    /// Column where command input started.
    pub command_start_col: Option<u16>,
    /// Absolute row where output started.
    pub output_start_row: Option<u64>,
    /// Absolute row where output ended (a mark's `output_end_row`, a block's
    /// `end_row`).
    pub end_row: Option<u64>,
    /// Exit code.
    pub exit_code: Option<i32>,
    /// Working directory at the prompt.
    pub working_directory: Option<String>,
    /// Explicit command line (OSC 633 ; E).
    pub commandline: Option<String>,
    /// Prompt time (ms since the Unix epoch).
    pub prompt_time_ms: Option<u64>,
    /// Command-input start time.
    pub command_input_start_time_ms: Option<u64>,
    /// Command-execution start time.
    pub command_exec_start_time_ms: Option<u64>,
    /// Command end time.
    pub command_end_time_ms: Option<u64>,
}

/// An output block: its identity, lifecycle state and span.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct OutputBlockRepr {
    /// Block id (monotonic per terminal).
    pub id: u64,
    /// `0` prompt only, `1` entering command, `2` executing, `3` complete.
    pub state: u8,
    /// Whether the user collapsed it.
    pub collapsed: bool,
    /// A full clear blanked rows it reads its text from after its command
    /// finished (`OutputBlock::text_cleared`). Absent from a predecessor that
    /// did not record it, and left out when false, so that state re-encodes as
    /// it was written.
    #[cfg_attr(feature = "serde", serde(default, skip_serializing_if = "is_false"))]
    pub text_cleared: bool,
    /// Its command rows were lost while it was being built: a full clear
    /// blanked them or a scrollback erase took them
    /// (`OutputBlock::command_text_cleared`). Absent from a predecessor that
    /// did not record it, and left out when false, as `text_cleared` is.
    #[cfg_attr(feature = "serde", serde(default, skip_serializing_if = "is_false"))]
    pub command_text_cleared: bool,
    /// Rows, times and text.
    pub span: ShellSpanRepr,
}

/// Shell-integration state (see the module docs). `Default` is a terminal
/// that never saw a mark.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ShellRepr {
    /// A→B→C→D phase: `0` none, `1` prompt, `2` command input, `3` executing,
    /// `4` finished.
    #[cfg_attr(feature = "serde", serde(default))]
    pub phase: u8,
    /// Coarse shell state: `0` ground, `1` prompt, `2` entering, `3` executing.
    #[cfg_attr(feature = "serde", serde(default))]
    pub state: u8,
    /// The mark being built.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub current_mark: Option<ShellSpanRepr>,
    /// Completed marks still readable in the carried history, oldest first.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Vec::is_empty")
    )]
    pub command_marks: Vec<ShellSpanRepr>,
    /// The block being built.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub current_block: Option<OutputBlockRepr>,
    /// Finished blocks still readable in the carried history, oldest first.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Vec::is_empty")
    )]
    pub output_blocks: Vec<OutputBlockRepr>,
    /// The next block id to assign.
    #[cfg_attr(feature = "serde", serde(default))]
    pub next_block_id: u64,
    /// Completed-command counter observers key on; never replayed backwards.
    #[cfg_attr(feature = "serde", serde(default))]
    pub completed_seq: u64,
    /// Started-command counter (accepted OSC 133/633 C); never replayed
    /// backwards, nor restored below the carried blocks that reached C.
    /// Absent from a predecessor that did not count it, and left out at `0`,
    /// so a terminal that never started a command encodes as a predecessor's
    /// did.
    #[cfg_attr(feature = "serde", serde(default, skip_serializing_if = "is_zero_seq"))]
    pub started_seq: u64,
    /// The shell signs its own cwd report (`ShellIntegrationState::signs_cwd`).
    /// Absent from a predecessor that did not read signatures, and left out when
    /// false.
    #[cfg_attr(feature = "serde", serde(default, skip_serializing_if = "is_false"))]
    pub signs_cwd: bool,
    /// The shell finished a command it never marked with `C`
    /// (`ShellIntegrationState::unmarked_run`). Absent or left out when false.
    #[cfg_attr(feature = "serde", serde(default, skip_serializing_if = "is_false"))]
    pub unmarked_run: bool,
    /// The phase predates the nonce the shell just took: only its next accepted
    /// `A` makes the phase current. Absent from predecessors and left out when false.
    #[cfg_attr(feature = "serde", serde(default, skip_serializing_if = "is_false"))]
    pub phase_stale: bool,
    /// An OSC 7 held at the prompt of a shell that does not sign its own
    /// (`ShellIntegrationState::held_cwd`), which the shell's next `A` applies.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub held_cwd: Option<CwdReportRepr>,
    /// The cwd a foreground job's report replaced at the shell's prompt
    /// (`ShellIntegrationState::cwd_before_job`), which comes back once the host
    /// says no job holds the foreground.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub cwd_before_job: Option<CwdReportRepr>,
}

/// A reported cwd as a checkpoint carries it: the path it stores (none: it
/// clears the cwd) and the host its OSC 7 named (none: a local one). Installed
/// only when the engine would store it from an OSC 7.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct CwdReportRepr {
    /// The decoded local path, or `None` for a report that clears the cwd.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub path: Option<String>,
    /// The host the report named, or `None` for a local one.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub host: Option<String>,
}

impl CwdReportRepr {
    #[inline]
    fn capture(report: &super::shell::CwdReport) -> Self {
        Self {
            path: report.path.clone(),
            host: report.host.clone(),
        }
    }

    #[inline]
    fn to_report(&self) -> Option<super::shell::CwdReport> {
        super::shell::CwdReport::of(self.path.as_deref(), self.host.as_deref())
    }
}

#[cfg(feature = "serde")]
#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if hands the predicate a reference"
)]
#[inline]
const fn is_zero_seq(n: &u64) -> bool {
    *n == 0
}

#[cfg(feature = "serde")]
#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if hands the predicate a reference"
)]
#[inline]
const fn is_false(b: &bool) -> bool {
    !*b
}

impl ShellRepr {
    /// Whether this is a terminal that never saw a mark (the wire omits it).
    #[inline]
    #[must_use]
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

#[inline]
fn bounded(text: Option<&str>, max: usize) -> Option<String> {
    text.filter(|t| t.len() <= max).map(str::to_owned)
}

impl ShellSpanRepr {
    fn capture_mark(m: &CommandMark) -> Self {
        Self {
            prompt_start_row: m.prompt_start_row,
            prompt_start_col: m.prompt_start_col,
            command_start_row: m.command_start_row,
            command_start_col: m.command_start_col,
            output_start_row: m.output_start_row,
            end_row: m.output_end_row,
            exit_code: m.exit_code,
            working_directory: m.working_directory.as_deref().map(str::to_owned),
            commandline: m.commandline.as_deref().map(str::to_owned),
            prompt_time_ms: m.prompt_time_ms,
            command_input_start_time_ms: m.command_input_start_time_ms,
            command_exec_start_time_ms: m.command_exec_start_time_ms,
            command_end_time_ms: m.command_end_time_ms,
        }
    }

    fn into_mark(self) -> CommandMark {
        let mut m = CommandMark::new(self.prompt_start_row, self.prompt_start_col);
        m.command_start_row = self.command_start_row;
        m.command_start_col = self.command_start_col;
        m.output_start_row = self.output_start_row;
        m.output_end_row = self.end_row;
        m.exit_code = self.exit_code;
        m.working_directory =
            bounded(self.working_directory.as_deref(), MAX_CWD_PATH_BYTES).map(Into::into);
        m.commandline = bounded(self.commandline.as_deref(), MAX_COMMANDLINE_BYTES).map(Into::into);
        m.prompt_time_ms = self.prompt_time_ms;
        m.command_input_start_time_ms = self.command_input_start_time_ms;
        m.command_exec_start_time_ms = self.command_exec_start_time_ms;
        m.command_end_time_ms = self.command_end_time_ms;
        m
    }
}

impl OutputBlockRepr {
    fn capture(b: &OutputBlock) -> Self {
        let state = match b.state {
            BlockState::PromptOnly => 0,
            BlockState::EnteringCommand => 1,
            BlockState::Executing => 2,
            _ => 3,
        };
        Self {
            id: b.id,
            state,
            collapsed: b.collapsed,
            text_cleared: b.text_cleared,
            command_text_cleared: b.command_text_cleared,
            span: ShellSpanRepr {
                prompt_start_row: b.prompt_start_row,
                prompt_start_col: b.prompt_start_col,
                command_start_row: b.command_start_row,
                command_start_col: b.command_start_col,
                output_start_row: b.output_start_row,
                end_row: b.end_row,
                exit_code: b.exit_code,
                working_directory: b.working_directory.as_deref().map(str::to_owned),
                commandline: b.commandline.as_deref().map(str::to_owned),
                prompt_time_ms: b.prompt_time_ms,
                command_input_start_time_ms: b.command_input_start_time_ms,
                command_exec_start_time_ms: b.command_exec_start_time_ms,
                command_end_time_ms: b.command_end_time_ms,
            },
        }
    }

    fn into_block(self) -> OutputBlock {
        let s = self.span;
        let mut b = OutputBlock::new(self.id, s.prompt_start_row, s.prompt_start_col);
        b.state = match self.state {
            0 => BlockState::PromptOnly,
            1 => BlockState::EnteringCommand,
            2 => BlockState::Executing,
            _ => BlockState::Complete,
        };
        b.collapsed = self.collapsed;
        b.text_cleared = self.text_cleared;
        b.command_text_cleared = self.command_text_cleared;
        b.command_start_row = s.command_start_row;
        b.command_start_col = s.command_start_col;
        b.output_start_row = s.output_start_row;
        b.end_row = s.end_row;
        b.exit_code = s.exit_code;
        b.working_directory =
            bounded(s.working_directory.as_deref(), MAX_CWD_PATH_BYTES).map(Into::into);
        b.commandline = bounded(s.commandline.as_deref(), MAX_COMMANDLINE_BYTES).map(Into::into);
        b.prompt_time_ms = s.prompt_time_ms;
        b.command_input_start_time_ms = s.command_input_start_time_ms;
        b.command_exec_start_time_ms = s.command_exec_start_time_ms;
        b.command_end_time_ms = s.command_end_time_ms;
        b
    }
}

#[inline]
const fn phase_to_wire(p: ShellIntegrationPhase) -> u8 {
    match p {
        ShellIntegrationPhase::None => 0,
        ShellIntegrationPhase::PromptStart => 1,
        ShellIntegrationPhase::CommandStart => 2,
        ShellIntegrationPhase::CommandExec => 3,
        ShellIntegrationPhase::CommandFinished => 4,
    }
}

#[inline]
const fn phase_from_wire(p: u8) -> ShellIntegrationPhase {
    match p {
        1 => ShellIntegrationPhase::PromptStart,
        2 => ShellIntegrationPhase::CommandStart,
        3 => ShellIntegrationPhase::CommandExec,
        4 => ShellIntegrationPhase::CommandFinished,
        _ => ShellIntegrationPhase::None,
    }
}

#[inline]
const fn state_to_wire(s: ShellState) -> u8 {
    match s {
        ShellState::ReceivingPrompt => 1,
        ShellState::EnteringCommand => 2,
        ShellState::Executing => 3,
        _ => 0,
    }
}

#[inline]
const fn state_from_wire(s: u8) -> ShellState {
    match s {
        1 => ShellState::ReceivingPrompt,
        2 => ShellState::EnteringCommand,
        3 => ShellState::Executing,
        _ => ShellState::Ground,
    }
}

impl Terminal {
    /// Project the application-set colour state (see [`ColorRepr`]).
    pub(super) fn capture_color_repr(&self) -> ColorRepr {
        let ColorOverridesRepr {
            palette,
            foreground,
            background,
            cursor,
            selection_background,
            selection_foreground,
        } = self.color.capture_overrides();
        ColorRepr {
            palette,
            foreground,
            background,
            cursor,
            selection_background,
            selection_foreground,
            stack: Vec::new(),
            stack_overrides: self.color.stack.iter().cloned().collect(),
        }
    }

    /// Write a program's colour diff over this terminal's configured colours
    /// (the stack excluded). Sets the slots only: [`Self::apply_color_repr`]
    /// adds the callbacks a handoff fires, `apply_config` its own change set.
    #[inline]
    pub(super) fn apply_color_overrides(&mut self, overrides: &ColorOverridesRepr) {
        self.color.apply_overrides(overrides, true);
    }

    /// Apply a carried [`ColorRepr`] over this terminal's configured colours, taking
    /// the OSC mutation's authority and damage path for the restored foreground,
    /// background, cursor and palette.
    pub(super) fn apply_color_repr(&mut self, repr: &ColorRepr) {
        use super::ColorTarget;
        self.apply_color_overrides(&repr.overrides());
        // A parent that predates `stack_overrides` sent whole entries; they become
        // diffs from this terminal's baseline.
        let stack: Vec<ColorOverridesRepr> = if repr.stack_overrides.is_empty() {
            repr.stack
                .iter()
                .map(|e| self.color.overrides_from_whole(e))
                .collect()
        } else {
            repr.stack_overrides
                .iter()
                .map(ColorOverridesRepr::normalized)
                .collect()
        };
        let keep = stack.len().saturating_sub(COLOR_STACK_MAX_DEPTH);
        self.color.stack = stack.into_iter().skip(keep).collect::<VecDeque<_>>();

        let (_parser, mut handler) = self.split_for_process();
        if repr.foreground.is_some() {
            handler.note_color_change(ColorTarget::Foreground);
        }
        if repr.background.is_some() {
            handler.note_color_change(ColorTarget::Background);
        }
        if repr.cursor.is_some() {
            handler.note_color_change(ColorTarget::Cursor);
        }
        if !repr.palette.is_empty() {
            handler.note_color_change(ColorTarget::Palette);
        }
    }

    /// Project the shell-integration state (see [`ShellRepr`]). Completed marks
    /// and blocks every row of which is older than `oldest_carried_row` name
    /// only lines the carry does not hold, so they are left behind, oldest
    /// first up to the first one that names a line it holds: the ids carried
    /// stay one run, by the rule `forget_erased_history` keeps them by
    /// ([`super::handler::block_rows_end`]). It goes by the last row each
    /// names, not its prompt row, which a full clear can move above the
    /// previous block's (a `clear` moves the rows of the block it caught
    /// running to the cleared screen's top, and Ctrl-L restarts the prompt
    /// there). One whose earlier rows the carry does not hold reads them as
    /// `BlockText::Evicted`, as the live terminal does once the ring evicts
    /// them. The in-progress mark and block always travel (a running command
    /// must still complete).
    pub(super) fn capture_shell_repr(&self, oldest_carried_row: u64) -> ShellRepr {
        let s = &self.shell;
        ShellRepr {
            phase: phase_to_wire(s.phase),
            state: state_to_wire(s.state),
            current_mark: s.current_mark.as_ref().map(ShellSpanRepr::capture_mark),
            command_marks: s
                .command_marks
                .iter()
                .skip_while(|m| super::handler::mark_rows_end(m) <= oldest_carried_row)
                .map(ShellSpanRepr::capture_mark)
                .collect(),
            current_block: s.current_block.as_ref().map(OutputBlockRepr::capture),
            output_blocks: s
                .output_blocks
                .iter()
                .skip_while(|b| super::handler::block_rows_end(b) <= oldest_carried_row)
                .map(OutputBlockRepr::capture)
                .collect(),
            next_block_id: s.next_block_id,
            completed_seq: s.completed_seq,
            started_seq: s.started_seq,
            signs_cwd: s.signs_cwd,
            unmarked_run: s.unmarked_run,
            phase_stale: s.phase_stale,
            held_cwd: s.held_cwd.as_ref().map(CwdReportRepr::capture),
            cwd_before_job: s.cwd_before_job.as_ref().map(CwdReportRepr::capture),
        }
    }

    /// Project the titles a program set (see [`TitleRepr`]).
    pub(super) fn capture_title_repr(&self) -> TitleRepr {
        let has_saved_empty = self
            .title
            .stack
            .iter()
            .any(|(icon, window)| icon.as_deref() == Some("") || window.as_deref() == Some(""));
        let stack_saved = if has_saved_empty {
            self.title
                .stack
                .iter()
                .map(|(icon, window)| (icon.is_some(), window.is_some()))
                .collect()
        } else {
            Vec::new()
        };
        TitleRepr {
            window: self.title.window.to_string(),
            icon: self.title.icon.to_string(),
            stack: self
                .title
                .stack
                .iter()
                .map(|(icon, window)| {
                    let wire = |slot: &Option<std::sync::Arc<str>>| {
                        slot.as_deref().unwrap_or("").to_string()
                    };
                    (wire(icon), wire(window))
                })
                .collect(),
            stack_saved,
        }
    }

    /// Install a carried [`TitleRepr`] as if a program had sent it: every string
    /// capped and stripped as the OSC handler stores one, and the stack cut to its
    /// bottom [`TITLE_STACK_MAX_DEPTH`] entries, as a live engine drops a push onto a
    /// full stack. A real window-title change bumps [`Terminal::title_epoch`].
    ///
    /// Not the host's `set_title`, which caps but does not sanitize: a carried title
    /// is a program's words read back from a file, so it gets the program's rules.
    pub fn restore_title(&mut self, repr: &TitleRepr) {
        let window = carried_title(&repr.window);
        if *self.title.window != *window {
            self.title
                .epoch
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        self.title.window = window.into();
        self.title.icon = carried_title(&repr.icon).into();
        self.title.stack = repr
            .stack
            .iter()
            .take(TITLE_STACK_MAX_DEPTH)
            .enumerate()
            .map(|(index, (icon, window))| {
                let saved = repr.stack_saved.get(index);
                let slot = |s: &str, saved: Option<bool>| {
                    let s = carried_title(s);
                    saved
                        .unwrap_or(!s.is_empty())
                        .then(|| std::sync::Arc::<str>::from(s))
                };
                (
                    slot(icon, saved.map(|s| s.0)),
                    slot(window, saved.map(|s| s.1)),
                )
            })
            .collect();
    }

    /// Install a carried [`ShellRepr`], bounding what a wire can hand in to the
    /// engine's own caps.
    pub(super) fn apply_shell_repr(&mut self, repr: &ShellRepr) {
        let s = &mut self.shell;
        s.phase = phase_from_wire(repr.phase);
        s.state = state_from_wire(repr.state);
        s.current_mark = repr.current_mark.clone().map(ShellSpanRepr::into_mark);
        let skip = repr.command_marks.len().saturating_sub(COMMAND_MARKS_MAX);
        s.command_marks = repr
            .command_marks
            .iter()
            .skip(skip)
            .cloned()
            .map(ShellSpanRepr::into_mark)
            .collect();
        s.current_block = repr.current_block.clone().map(OutputBlockRepr::into_block);
        let skip = repr.output_blocks.len().saturating_sub(OUTPUT_BLOCKS_MAX);
        s.output_blocks = repr
            .output_blocks
            .iter()
            .skip(skip)
            .cloned()
            .map(OutputBlockRepr::into_block)
            .collect();
        // Monotonic counters only move forward, even onto a terminal that already
        // counted commands of its own.
        s.next_block_id = s.next_block_id.max(repr.next_block_id);
        s.completed_seq = s.completed_seq.max(repr.completed_seq);
        // Each carried block that reached C took a C of its own, so the count
        // never reads below them: a predecessor that did not count C's carries
        // none, and a live shell's stream must not read as one that never
        // started a command.
        let proven = s
            .output_blocks
            .iter()
            .chain(s.current_block.as_ref())
            .filter(|b| matches!(b.state, BlockState::Executing | BlockState::Complete))
            .count() as u64;
        s.started_seq = s.started_seq.max(repr.started_seq).max(proven);
        // The shell's habits and the reports its prompt left pending are the
        // carry's: a report held at this engine's previous prompt, or a cwd
        // its own job replaced, belongs to a shell this projection replaces.
        s.signs_cwd = repr.signs_cwd;
        s.unmarked_run = repr.unmarked_run;
        s.phase_stale = repr.phase_stale;
        s.held_cwd = repr.held_cwd.as_ref().and_then(CwdReportRepr::to_report);
        s.cwd_before_job = repr
            .cwd_before_job
            .as_ref()
            .and_then(CwdReportRepr::to_report);
        // Nor is the stored cwd's host: the restore installs the carried cwd,
        // which this engine's host never qualified. The shell's next OSC 7
        // names it again.
        s.cwd_host = None;
        // The cwd installed beside this projection is the carry's: no report
        // this engine counted stands behind it (`Terminal::reported_cwd_seq`).
        s.cwd_carried = true;
        // Nor is the last D's copy: a block id it names may belong to this
        // terminal's own blocks rather than the carried ones.
        s.last_finished = None;
    }
}
