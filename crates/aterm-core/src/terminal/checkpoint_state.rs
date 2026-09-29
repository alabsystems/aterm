// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The colour and shell-integration sub-projections of a
//! [`TerminalCheckpoint`](super::TerminalCheckpoint).
//!
//! Both are state an APPLICATION set, which an in-session update used to drop:
//! a base16-shell palette (OSC 4) or an OSC 10/11 theme reverted to the host's
//! colours, and the OSC 133/633 command blocks vanished — worse, a command
//! running across the update never completed, because the successor's A→B→C→D
//! state machine sat at "no prompt" and ignored the D that ended it.
//!
//! * [`ColorRepr`] carries the colour state as a DIFF from the host's
//!   configured baseline, so adopting into a terminal whose theme changed
//!   applies the application's overrides on top of the NEW theme rather than
//!   resurrecting the old one. The XTPUSHCOLORS stack travels whole: each entry
//!   is exactly what the application asked to have restored.
//! * [`ShellRepr`] carries the phase, the in-progress mark and block, the
//!   completed marks and blocks that are still readable, and the monotonic
//!   counters observers key on. Their rows are absolute, and the checkpoint
//!   carries each grid's absolute row counter so the restored grids continue
//!   the same numbering: a mark means the same line after the handoff.
//! * [`TitleRepr`] carries the window title (OSC 0/2), the icon name (OSC 0/1)
//!   and the XTWINOPS title stack (CSI 22/23 t). An in-session update used to
//!   drop all three: the tab kept its label only until the successor's first
//!   frame, then fell back to the working directory until the program happened
//!   to set a title again (the 2026-09-28 round-four plan, item 8), and a
//!   program that pushed its caller's title to restore it on exit restored
//!   nothing. The successor treats a carried title as OSC input
//!   ([`Terminal::restore_title`]): capped, stripped of controls, the stack
//!   bounded.

use std::collections::VecDeque;

use aterm_types::{ColorPalette, Rgb};

use super::Terminal;
use super::callbacks::{COLOR_STACK_MAX_DEPTH, TITLE_STACK_MAX_DEPTH};
use super::grouped_state::{ColorStackEntry, ShellIntegrationPhase};
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

/// One XTPUSHCOLORS (OSC 30001) stack entry, whole.
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
    /// The XTPUSHCOLORS stack, oldest first.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Vec::is_empty")
    )]
    pub stack: Vec<ColorStackEntryRepr>,
}

impl ColorRepr {
    /// Whether nothing is overridden (the wire omits the whole projection).
    #[must_use]
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// The titles a program set: the window title, the icon name, and the
/// XTWINOPS title stack, whole. `Default` is a terminal nothing has titled.
///
/// Every string is exactly what the engine stores, which is already capped and
/// stripped of controls on the way in; a restore does both again
/// ([`Terminal::restore_title`]), because the wire is not the engine.
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
    /// empty string for the other, which a pop leaves alone.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Vec::is_empty")
    )]
    pub stack: Vec<(String, String)>,
}

impl TitleRepr {
    /// Whether nothing has been titled.
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
}

impl ShellRepr {
    /// Whether this is a terminal that never saw a mark (the wire omits it).
    #[must_use]
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

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

fn phase_to_wire(p: ShellIntegrationPhase) -> u8 {
    match p {
        ShellIntegrationPhase::None => 0,
        ShellIntegrationPhase::PromptStart => 1,
        ShellIntegrationPhase::CommandStart => 2,
        ShellIntegrationPhase::CommandExec => 3,
        ShellIntegrationPhase::CommandFinished => 4,
    }
}

fn phase_from_wire(p: u8) -> ShellIntegrationPhase {
    match p {
        1 => ShellIntegrationPhase::PromptStart,
        2 => ShellIntegrationPhase::CommandStart,
        3 => ShellIntegrationPhase::CommandExec,
        4 => ShellIntegrationPhase::CommandFinished,
        _ => ShellIntegrationPhase::None,
    }
}

fn state_to_wire(s: ShellState) -> u8 {
    match s {
        ShellState::ReceivingPrompt => 1,
        ShellState::EnteringCommand => 2,
        ShellState::Executing => 3,
        _ => 0,
    }
}

fn state_from_wire(s: u8) -> ShellState {
    match s {
        1 => ShellState::ReceivingPrompt,
        2 => ShellState::EnteringCommand,
        3 => ShellState::Executing,
        _ => ShellState::Ground,
    }
}

impl Terminal {
    /// The palette entry the host configured at `index` (the theme palette, or
    /// xterm's default when the theme sets none).
    fn configured_palette_entry(&self, index: u8) -> Rgb {
        self.color
            .configured_palette
            .as_ref()
            .map_or_else(|| ColorPalette::default_color(index), |p| p.get(index))
    }

    /// Project the application-set colour state (see [`ColorRepr`]).
    pub(super) fn capture_color_repr(&self) -> ColorRepr {
        let c = &self.color;
        let palette = (0..=u8::MAX)
            .filter_map(|index| {
                let rgb = c.palette.get(index);
                (rgb != self.configured_palette_entry(index))
                    .then_some(PaletteOverrideRepr { index, rgb })
            })
            .collect();
        let slot = |live: Option<Rgb>, configured: Option<Rgb>| {
            (live != configured).then_some(ColorSlotRepr { rgb: live })
        };
        ColorRepr {
            palette,
            foreground: (c.default_foreground != c.configured_foreground)
                .then_some(c.default_foreground),
            background: (c.default_background != c.configured_background)
                .then_some(c.default_background),
            cursor: slot(c.cursor_color, c.configured_cursor),
            selection_background: slot(c.selection_background, c.configured_selection_background),
            selection_foreground: slot(c.selection_foreground, c.configured_selection_foreground),
            stack: c
                .stack
                .iter()
                .map(|e| ColorStackEntryRepr {
                    palette: (0..=u8::MAX).map(|i| e.palette.get(i)).collect(),
                    foreground: e.default_foreground,
                    background: e.default_background,
                    cursor: e.cursor_color,
                    selection_background: e.selection_background,
                    selection_foreground: e.selection_foreground,
                })
                .collect(),
        }
    }

    /// Apply a carried [`ColorRepr`] over this terminal's configured colours,
    /// through the same authority and damage path an OSC mutation takes, so the
    /// host's colour-change callback hears about every restored slot.
    pub(super) fn apply_color_repr(&mut self, repr: &ColorRepr) {
        use super::{ColorChangeOp, ColorTarget};
        for e in &repr.palette {
            self.color.palette.set(e.index, e.rgb);
        }
        if let Some(fg) = repr.foreground {
            self.color.default_foreground = fg;
        }
        if let Some(bg) = repr.background {
            self.color.default_background = bg;
        }
        if let Some(slot) = repr.cursor {
            self.color.cursor_color = slot.rgb;
        }
        if let Some(slot) = repr.selection_background {
            self.color.selection_background = slot.rgb;
        }
        if let Some(slot) = repr.selection_foreground {
            self.color.selection_foreground = slot.rgb;
        }
        let keep = repr.stack.len().saturating_sub(COLOR_STACK_MAX_DEPTH);
        self.color.stack = repr
            .stack
            .iter()
            .skip(keep)
            .map(|e| {
                let mut palette = ColorPalette::new();
                for (index, rgb) in (0..=u8::MAX).zip(e.palette.iter()) {
                    palette.set(index, *rgb);
                }
                ColorStackEntry {
                    palette,
                    default_foreground: e.foreground,
                    default_background: e.background,
                    cursor_color: e.cursor,
                    selection_background: e.selection_background,
                    selection_foreground: e.selection_foreground,
                }
            })
            .collect::<VecDeque<_>>();

        let (_parser, mut handler) = self.split_for_process();
        if let Some(fg) = repr.foreground {
            handler.fire_color_change_callback(ColorTarget::Foreground, fg, ColorChangeOp::Set);
        }
        if let Some(bg) = repr.background {
            handler.fire_color_change_callback(ColorTarget::Background, bg, ColorChangeOp::Set);
        }
        if let Some(slot) = repr.cursor {
            let shown = slot.rgb.unwrap_or(handler.color.default_foreground);
            handler.fire_color_change_callback(ColorTarget::Cursor, shown, ColorChangeOp::Set);
        }
        if !repr.palette.is_empty() {
            handler.fire_color_change_callback(
                ColorTarget::Palette,
                Rgb { r: 0, g: 0, b: 0 },
                ColorChangeOp::Set,
            );
        }
    }

    /// Project the shell-integration state (see [`ShellRepr`]). Completed marks
    /// and blocks whose prompt row is older than `oldest_carried_row` name
    /// lines the carry does not hold, so they are left behind; the in-progress
    /// mark and block always travel (a running command must still complete).
    pub(super) fn capture_shell_repr(&self, oldest_carried_row: u64) -> ShellRepr {
        let s = &self.shell;
        ShellRepr {
            phase: phase_to_wire(s.phase),
            state: state_to_wire(s.state),
            current_mark: s.current_mark.as_ref().map(ShellSpanRepr::capture_mark),
            command_marks: s
                .command_marks
                .iter()
                .filter(|m| m.prompt_start_row >= oldest_carried_row)
                .map(ShellSpanRepr::capture_mark)
                .collect(),
            current_block: s.current_block.as_ref().map(OutputBlockRepr::capture),
            output_blocks: s
                .output_blocks
                .iter()
                .filter(|b| b.prompt_start_row >= oldest_carried_row)
                .map(OutputBlockRepr::capture)
                .collect(),
            next_block_id: s.next_block_id,
            completed_seq: s.completed_seq,
        }
    }

    /// Project the titles a program set (see [`TitleRepr`]).
    pub(super) fn capture_title_repr(&self) -> TitleRepr {
        TitleRepr {
            window: self.title.window.to_string(),
            icon: self.title.icon.to_string(),
            stack: self
                .title
                .stack
                .iter()
                .map(|(icon, window)| (icon.to_string(), window.to_string()))
                .collect(),
        }
    }

    /// Install a carried [`TitleRepr`] AS IF A PROGRAM HAD SENT IT: every string
    /// capped and stripped the way the OSC handler stores one, and the stack cut
    /// to [`TITLE_STACK_MAX_DEPTH`] entries — its BOTTOM ones, because that is
    /// what a live engine keeps when a push finds the stack full (the push is
    /// dropped). A real change of the window title bumps the title epoch, so a
    /// host polling [`Terminal::title_epoch`] relabels the tab.
    ///
    /// Why not the host's `set_title`: it caps but does not sanitize, since a
    /// host sets its own words. A carried title is the previous process's copy
    /// of a PROGRAM's words, read back from a file, and gets the program's rules.
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
            .map(|(icon, window)| (carried_title(icon).into(), carried_title(window).into()))
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
        // Monotonic counters only ever move forward, even onto a terminal that
        // already counted some commands of its own.
        s.next_block_id = s.next_block_id.max(repr.next_block_id);
        s.completed_seq = s.completed_seq.max(repr.completed_seq);
    }
}
