// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The heap a checkpoint and a replay carry hold ([`TerminalCheckpoint::memory_used`],
//! [`super::ReplayCarry::memory_used`]): what dropping one hands back to the allocator, less
//! what an `Arc` shares with the engine it was taken from.
//!
//! Each struct is destructured whole, so a field added to one does not compile until it is
//! counted here; a type counted as holding nothing is checked to have no drop glue.

use std::collections::HashMap;
use std::mem::size_of;
use std::sync::Arc;

use aterm_parser::ParserCarry;
use aterm_scrollback::ImageSpan;

use super::checkpoint::{GridCursorRepr, TerminalCheckpoint};
use super::checkpoint_holder::{HolderMarkRepr, HolderRepr, HolderSealRepr};
use super::checkpoint_output::{HyperlinkRepr, OutputStateRepr};
use super::checkpoint_state::{
    ColorOverridesRepr, ColorRepr, ColorStackEntryRepr, CwdReportRepr, OutputBlockRepr, ShellRepr,
    ShellSpanRepr, TitleRepr,
};
use super::kitty_graphics::KittyCommand;
use super::transient_state::{
    KittyAnimation, KittyFloating, KittyFrame, KittyRelation, KittyVariant, KittyVirtual,
};

/// The heap bytes a value holds past its own `size_of`, as its allocations' layouts ask for them.
pub(super) trait Heap {
    /// Whether [`Self::heap`] is 0 for every value of the type, so a `Vec` of them is its capacity alone.
    const NONE: bool = false;

    /// The bytes dropping this value hands back, its own size aside.
    fn heap(&self) -> usize;
}

/// Count each type as holding no heap, checked at compile time: it has no drop glue, so dropping one frees nothing.
macro_rules! no_heap {
    ($($t:ty),* $(,)?) => {$(
        const _: () = assert!(
            !std::mem::needs_drop::<$t>(),
            "a type with drop glue holds heap: count its fields"
        );
        impl $crate::terminal::checkpoint_heap::Heap for $t {
            const NONE: bool = true;
            #[inline]
            fn heap(&self) -> usize {
                0
            }
        }
    )*};
}
pub(super) use no_heap;

no_heap!(
    bool,
    char,
    u8,
    u16,
    u32,
    u64,
    i32,
    f32,
    usize,
    std::time::Duration,
    aterm_time::Instant,
    std::num::NonZeroU32,
    aterm_types::Rgb,
    aterm_types::Appearance,
    aterm_types::CursorStyle,
    aterm_types::KittyPlacementTag,
    aterm_types::KittyKeyboardStateSnapshot,
    aterm_types::XtermKeyboardState,
    aterm_types::TaskbarProgress,
    aterm_types::charset::CharacterSetState,
    aterm_parser::State,
    aterm_parser::TmuxPassthrough,
    aterm_grid::GridHandbackCarry,
    crate::config::BiDiConfig,
    super::types::TerminalModes,
    super::checkpoint::SavedCursorRepr,
    super::checkpoint::StyleRepr,
    super::checkpoint::ShellIntegrationNonce,
    super::checkpoint_state::PaletteOverrideRepr,
    super::checkpoint_state::ColorSlotRepr,
    super::checkpoint_output::SgrPushRepr,
    super::checkpoint_output::SavedModeRepr,
    super::checkpoint_output::Vt52AddressRepr,
    super::checkpoint_holder::HolderAnchorRepr,
    super::checkpoint_holder::HolderSpanRepr,
    super::program_state::ProgramStateReport,
    super::shell_integration_auth::ShellIntegrationAuth,
    super::clipboard_auth::ClipboardAuth,
    super::state::ConfiguredModes,
    super::response_rate_limiter::ResponseRateLimiter,
    super::transient_state::KittyPlayState,
    super::transient_state::KittyFloatParent,
    super::transient_state::KittyParent,
);

impl<T: Heap> Heap for Vec<T> {
    fn heap(&self) -> usize {
        let own = self.capacity().saturating_mul(size_of::<T>());
        if T::NONE {
            own
        } else {
            self.iter()
                .fold(own, |sum, item| sum.saturating_add(item.heap()))
        }
    }
}

impl Heap for String {
    #[inline]
    fn heap(&self) -> usize {
        self.capacity()
    }
}

impl Heap for Box<str> {
    #[inline]
    fn heap(&self) -> usize {
        self.len()
    }
}

impl<T: Heap> Heap for Option<T> {
    const NONE: bool = T::NONE;

    #[inline]
    fn heap(&self) -> usize {
        self.as_ref().map_or(0, T::heap)
    }
}

impl<T: Heap, const N: usize> Heap for [T; N] {
    const NONE: bool = T::NONE;

    fn heap(&self) -> usize {
        if T::NONE {
            0
        } else {
            self.iter()
                .fold(0, |sum, item| sum.saturating_add(item.heap()))
        }
    }
}

impl<A: Heap, B: Heap> Heap for (A, B) {
    const NONE: bool = A::NONE && B::NONE;

    #[inline]
    fn heap(&self) -> usize {
        self.0.heap().saturating_add(self.1.heap())
    }
}

impl<A: Heap, B: Heap, C: Heap> Heap for (A, B, C) {
    const NONE: bool = A::NONE && B::NONE && C::NONE;

    #[inline]
    fn heap(&self) -> usize {
        self.0
            .heap()
            .saturating_add(self.1.heap())
            .saturating_add(self.2.heap())
    }
}

/// An `Arc`'s pointee is shared with the engine it was cloned from, so dropping this copy frees it only once that engine let it go (a policy's rate-limit ids after the engine replaced its policy): an image payload is charged by [`super::ReplayCarry::retained_image_bytes`] instead, and any other pointee is not charged.
impl<T: ?Sized> Heap for Arc<T> {
    const NONE: bool = true;

    #[inline]
    fn heap(&self) -> usize {
        0
    }
}

impl<K: Heap, V: Heap, S> Heap for HashMap<K, V, S> {
    fn heap(&self) -> usize {
        let table = aterm_alloc::heap::table_bytes::<(K, V)>(self.capacity());
        if K::NONE && V::NONE {
            table
        } else {
            self.iter().fold(table, |sum, (k, v)| {
                sum.saturating_add(k.heap()).saturating_add(v.heap())
            })
        }
    }
}

impl Heap for aterm_types::ColorPalette {
    #[inline]
    fn heap(&self) -> usize {
        self.heap_bytes()
    }
}

impl Heap for aterm_policy::engine::PolicyEngine {
    #[inline]
    fn heap(&self) -> usize {
        self.heap_bytes()
    }
}

impl TerminalCheckpoint {
    /// The bytes this checkpoint holds: its own size and every allocation it owns, its grids' records, marks, titles and parser carry among them.
    #[must_use]
    pub fn memory_used(&self) -> usize {
        size_of::<Self>().saturating_add(self.heap())
    }
}

impl Heap for TerminalCheckpoint {
    fn heap(&self) -> usize {
        let Self {
            rows,
            cols,
            grid,
            history_lines,
            cursor,
            alt_grid,
            alt_cursor,
            saved_cursor_main,
            saved_cursor_alt,
            modes,
            style,
            charset,
            kitty_keyboard,
            xterm_keyboard,
            taskbar_progress,
            secure_keyboard_entry,
            current_working_directory,
            parser_ground,
            parser,
            shell_integration_nonce,
            absolute_row_counter,
            alt_absolute_row_counter,
            color,
            shell,
            shell_integration_rev,
            title,
            output_state,
            holder,
        } = self;
        [
            rows.heap(),
            cols.heap(),
            grid.heap(),
            history_lines.heap(),
            cursor.heap(),
            alt_grid.heap(),
            alt_cursor.heap(),
            saved_cursor_main.heap(),
            saved_cursor_alt.heap(),
            modes.heap(),
            style.heap(),
            charset.heap(),
            kitty_keyboard.heap(),
            xterm_keyboard.heap(),
            taskbar_progress.heap(),
            secure_keyboard_entry.heap(),
            current_working_directory.heap(),
            parser_ground.heap(),
            parser.heap(),
            shell_integration_nonce.heap(),
            absolute_row_counter.heap(),
            alt_absolute_row_counter.heap(),
            color.heap(),
            shell.heap(),
            shell_integration_rev.heap(),
            title.heap(),
            output_state.heap(),
            holder.heap(),
        ]
        .into_iter()
        .fold(0, usize::saturating_add)
    }
}

impl Heap for GridCursorRepr {
    fn heap(&self) -> usize {
        let Self {
            cursor_row: _,
            cursor_col: _,
            pending_wrap: _,
            follows_glyph: _,
            scroll_top: _,
            scroll_bottom: _,
            margin_left: _,
            margin_right: _,
            tab_stops,
            tab_defaults_suppressed: _,
            handback_rows: _,
            handback_above: _,
        } = self;
        tab_stops.heap()
    }
}

impl Heap for ParserCarry {
    fn heap(&self) -> usize {
        let Self {
            state,
            params,
            subparam_mask: _,
            current_param: _,
            param_started: _,
            last_was_colon: _,
            intermediates,
            osc,
            osc_overflowed: _,
            utf8_tail,
            utf8_expected: _,
            string_utf8_tail,
            tmux,
        } = self;
        [
            state.heap(),
            params.heap(),
            intermediates.heap(),
            osc.heap(),
            utf8_tail.heap(),
            string_utf8_tail.heap(),
            tmux.heap(),
        ]
        .into_iter()
        .fold(0, usize::saturating_add)
    }
}

impl Heap for ColorRepr {
    fn heap(&self) -> usize {
        let Self {
            palette,
            foreground,
            background,
            cursor,
            selection_background,
            selection_foreground,
            stack,
            stack_overrides,
        } = self;
        [
            palette.heap(),
            foreground.heap(),
            background.heap(),
            cursor.heap(),
            selection_background.heap(),
            selection_foreground.heap(),
            stack.heap(),
            stack_overrides.heap(),
        ]
        .into_iter()
        .fold(0, usize::saturating_add)
    }
}

impl Heap for ColorStackEntryRepr {
    fn heap(&self) -> usize {
        let Self {
            palette,
            foreground: _,
            background: _,
            cursor: _,
            selection_background: _,
            selection_foreground: _,
        } = self;
        palette.heap()
    }
}

impl Heap for ColorOverridesRepr {
    fn heap(&self) -> usize {
        let Self {
            palette,
            foreground: _,
            background: _,
            cursor: _,
            selection_background: _,
            selection_foreground: _,
        } = self;
        palette.heap()
    }
}

impl Heap for ShellRepr {
    fn heap(&self) -> usize {
        let Self {
            phase: _,
            state: _,
            current_mark,
            command_marks,
            current_block,
            output_blocks,
            next_block_id: _,
            completed_seq: _,
            started_seq: _,
            signs_cwd: _,
            unmarked_run: _,
            phase_stale: _,
            held_cwd,
            cwd_before_job,
        } = self;
        [
            current_mark.heap(),
            command_marks.heap(),
            current_block.heap(),
            output_blocks.heap(),
            held_cwd.heap(),
            cwd_before_job.heap(),
        ]
        .into_iter()
        .fold(0, usize::saturating_add)
    }
}

impl Heap for ShellSpanRepr {
    fn heap(&self) -> usize {
        let Self {
            prompt_start_row: _,
            prompt_start_col: _,
            command_start_row: _,
            command_start_col: _,
            output_start_row: _,
            end_row: _,
            exit_code: _,
            working_directory,
            commandline,
            prompt_time_ms: _,
            command_input_start_time_ms: _,
            command_exec_start_time_ms: _,
            command_end_time_ms: _,
        } = self;
        working_directory.heap().saturating_add(commandline.heap())
    }
}

impl Heap for OutputBlockRepr {
    fn heap(&self) -> usize {
        let Self {
            id: _,
            state: _,
            collapsed: _,
            text_cleared: _,
            command_text_cleared: _,
            span,
        } = self;
        span.heap()
    }
}

impl Heap for CwdReportRepr {
    fn heap(&self) -> usize {
        let Self { path, host } = self;
        path.heap().saturating_add(host.heap())
    }
}

impl Heap for TitleRepr {
    fn heap(&self) -> usize {
        let Self {
            window,
            icon,
            stack,
            stack_saved,
        } = self;
        [window.heap(), icon.heap(), stack.heap(), stack_saved.heap()]
            .into_iter()
            .fold(0, usize::saturating_add)
    }
}

impl Heap for OutputStateRepr {
    fn heap(&self) -> usize {
        let Self {
            rep: _,
            underline_color: _,
            hyperlink,
            vt52_address: _,
            zwj_pending: _,
            sgr_stack,
            saved_modes,
            program_state,
        } = self;
        [
            hyperlink.heap(),
            sgr_stack.heap(),
            saved_modes.heap(),
            program_state.heap(),
        ]
        .into_iter()
        .fold(0, usize::saturating_add)
    }
}

impl Heap for HyperlinkRepr {
    fn heap(&self) -> usize {
        let Self { uri, id } = self;
        uri.heap().saturating_add(id.heap())
    }
}

impl Heap for HolderRepr {
    fn heap(&self) -> usize {
        let Self {
            current: _,
            clock: _,
            marks,
            anchors,
            anchors_lost: _,
            seal_epoch: _,
            seed_untrusted: _,
        } = self;
        marks.heap().saturating_add(anchors.heap())
    }
}

impl Heap for HolderMarkRepr {
    fn heap(&self) -> usize {
        let Self {
            token: _,
            spans,
            tainted: _,
            alt_fresh: _,
            seal,
            suspended_at: _,
        } = self;
        spans.heap().saturating_add(seal.heap())
    }
}

impl Heap for HolderSealRepr {
    fn heap(&self) -> usize {
        let Self {
            rows,
            base: _,
            epoch: _,
        } = self;
        rows.heap()
    }
}

/// A pending transfer's chunks are charged by [`super::ReplayCarry::pending_transfer_bytes`], so only the command's own fields count here, and they hold nothing.
impl Heap for KittyCommand {
    fn heap(&self) -> usize {
        let Self {
            action: _,
            quiet: _,
            format: _,
            medium: _,
            id: _,
            number: _,
            placement: _,
            x: _,
            y: _,
            crop_width: _,
            crop_height: _,
            cursor_stays: _,
            virtual_placement: _,
            width: _,
            height: _,
            columns: _,
            rows: _,
            z_index: _,
            cell_x_offset: _,
            cell_y_offset: _,
            parent_id: _,
            parent_placement: _,
            parent_dx: _,
            parent_dy: _,
            more: _,
            compressed: _,
            delete_target: _,
            payload: _,
        } = self;
        0
    }
}

impl Heap for KittyAnimation {
    fn heap(&self) -> usize {
        let Self {
            frames,
            state,
            max_loops: _,
            loops: _,
            shown_at,
            image_serial,
        } = self;
        [
            frames.heap(),
            state.heap(),
            shown_at.heap(),
            image_serial.heap(),
        ]
        .into_iter()
        .fold(0, usize::saturating_add)
    }
}

impl Heap for KittyFrame {
    fn heap(&self) -> usize {
        let Self {
            image,
            gap_ms: _,
            undecodable: _,
        } = self;
        image.heap()
    }
}

impl Heap for KittyVariant {
    fn heap(&self) -> usize {
        let Self { source, image } = self;
        source.heap().saturating_add(image.heap())
    }
}

impl Heap for KittyVirtual {
    fn heap(&self) -> usize {
        let Self { tag, image } = self;
        tag.heap().saturating_add(image.heap())
    }
}

impl Heap for KittyFloating {
    fn heap(&self) -> usize {
        let Self {
            tag,
            image,
            parent,
            dx: _,
            dy: _,
        } = self;
        [tag.heap(), image.heap(), parent.heap()]
            .into_iter()
            .fold(0, usize::saturating_add)
    }
}

impl Heap for KittyRelation {
    fn heap(&self) -> usize {
        let Self {
            child,
            parent,
            dx: _,
            dy: _,
        } = self;
        child.heap().saturating_add(parent.heap())
    }
}

impl Heap for aterm_grid::GridImageCarry {
    fn heap(&self) -> usize {
        let Self {
            visible,
            history_rev,
        } = self;
        visible.heap().saturating_add(history_rev.heap())
    }
}

impl Heap for ImageSpan {
    fn heap(&self) -> usize {
        let Self {
            start_col: _,
            end_col: _,
            image_row: _,
            first_cell_col: _,
            image,
            kitty,
        } = self;
        image.heap().saturating_add(kitty.heap())
    }
}
