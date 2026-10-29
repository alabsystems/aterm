// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Terminal escape sequence handler: struct definition and routing.
#![forbid(clippy::disallowed_methods)]

use crate::grid::Grid;

use super::{
    CurrentStyle, DcsState, MAX_RESPONSE_BUFFER_SIZE, TerminalModes, TransientState,
    Vt52CursorState, WindowCallback,
};
use aterm_types::KittyKeyboardState;
use aterm_types::charset::CharacterSetState;

#[path = "handler_actions.rs"]
mod handler_actions;
#[path = "handler_csi.rs"]
mod handler_csi;
#[path = "handler_dcs.rs"]
mod handler_dcs;
#[path = "handler_decrsps.rs"]
pub(super) mod handler_decrsps;
#[path = "handler_esc.rs"]
mod handler_esc;
#[path = "handler_write.rs"]
mod handler_write;

pub(crate) use handler_actions::{
    MAX_KITTY_FRAMES, MAX_KITTY_IMAGE_BYTES, MAX_KITTY_IMAGES, MAX_KITTY_STORE_BYTES,
    MAX_KITTY_VARIANTS, MAX_KITTY_VIRTUALS,
};
pub(crate) use handler_write::is_vs16_emoji_capable;

/// Single-source mapping from `TerminalHandler` fields to `Terminal` borrow paths.
macro_rules! define_terminal_handler {
    (
        $(
            $(#[$meta:meta])*
            $field:ident : $ty:ty => $($path:ident).+
        ),*
        $(,)?
    ) => {
        /// Handler struct for parser callbacks.
        pub(super) struct TerminalHandler<'a> {
            $(
                $(#[$meta])*
                pub(super) $field: &'a mut $ty,
            )*
        }

        impl super::Terminal {
            /// Split `Terminal` into disjoint parser and handler borrows for VT processing.
            pub(super) fn split_for_process(
                &mut self,
            ) -> (&mut crate::parser::Parser, TerminalHandler<'_>) {
                (
                    &mut self.parser,
                    TerminalHandler {
                        $(
                            $(#[$meta])*
                            $field: &mut self.$($path).+,
                        )*
                    },
                )
            }
        }
    };
}

define_terminal_handler! {
    grid: Grid => grid,
    modes: TerminalModes => modes,
    style: CurrentStyle => style,
    charset: CharacterSetState => charset,
    alt_grid: Option<Grid> => alt_grid,
    cursor_save: super::CursorSaveState => cursor_save,
    alt_archive: super::alt_archive::AltArchiveState => alt_archive,

    dcs: DcsState => dcs,
    shell: super::ShellIntegrationState => shell,
    marks_state: super::MarksState => marks_state,
    color: super::ColorState => color,
    sixel: super::SixelState => sixel,
    kitty_keyboard: KittyKeyboardState => kitty_keyboard,
    xterm_keyboard: super::XtermKeyboardState => xterm_keyboard,

    transient: TransientState => transient,
    repaint_blink_epoch: u64 => repaint_blink_epoch,
    evidence_asserted: u16 => evidence_asserted,
    absolute_row_revision: u64 => absolute_row_revision,
    holder_fence: super::HolderFence => holder_fence,
    policy: super::policy_gates::PolicyState => policy,
    current_working_directory: Option<String> => current_working_directory,
    secure_keyboard_entry: bool => secure_keyboard_entry,

    title: super::TitleState => title,
    bell_callback: Option<Box<dyn FnMut() + Send>> => bell_callback,
    kitty_file_resolver: super::kitty_auth::KittyResolverSlot => kitty_file_resolver,
    last_bell_time: Option<aterm_time::Instant> => last_bell_time,
    bell_total: u64 => bell_total,
    notify_total: u64 => notify_total,
    default_cursor_style: aterm_types::CursorStyle => default_cursor_style,
    host_alternate_scroll: bool => host_alternate_scroll,
    notifications: super::NotificationState => notifications,
    clipboard: super::ClipboardState => clipboard,
    iterm2: super::Iterm2State => iterm2,
    window_callback: Option<WindowCallback> => window_callback,
    taskbar_progress: Option<super::TaskbarProgress> => taskbar_progress,
    program_state: super::program_state::ProgramStateReport => program_state,
    clipboard_auth: super::clipboard_auth::ClipboardAuth => clipboard_auth,
    shell_integration_auth: super::shell_integration_auth::ShellIntegrationAuth
        => shell_integration_auth,
    hyperlink_auth: super::hyperlink_auth::HyperlinkAuth => hyperlink_auth,
}

/// Narrow SGR-only borrow view extracted from `TerminalHandler` for style and color mutations.
pub(super) struct SgrStyleHandler<'a> {
    pub(super) grid: &'a mut Grid,
    pub(super) style: &'a mut CurrentStyle,
    pub(super) transient: &'a mut TransientState,
}

/// Narrow cursor/state borrow view extracted from `TerminalHandler`.
pub(super) struct CursorStateHandler<'a> {
    pub(super) grid: &'a mut Grid,
    pub(super) style: &'a mut CurrentStyle,
    pub(super) modes: &'a mut TerminalModes,
    pub(super) charset: &'a mut CharacterSetState,
    pub(super) cursor_save: &'a mut super::CursorSaveState,
    /// Current underline color for DECSC/DECRC save/restore.
    pub(super) underline_color: &'a mut Option<u32>,
    /// Cached `has_transient_extras` flag for DECRC update.
    pub(super) has_transient_extras: &'a mut bool,
    /// Whether a hyperlink is currently set (read-only snapshot for extras recomputation).
    pub(super) has_hyperlink: bool,
}

impl TerminalHandler<'_> {
    /// Fail-closed marker for the DEC 2026 close+reopen present license.
    #[inline]
    pub(super) const fn note_sync_open_action(&mut self) {
        if self.modes.synchronized_output {
            self.transient.sync_open_dirty = true;
        }
        if self.transient.app_sync_open {
            self.transient.app_sync_dirty = true;
        }
    }

    /// Apply any pending logical-row insertion before creating new absolute-row metadata.
    #[inline]
    pub(super) fn sync_absolute_row_metadata(&mut self) {
        if let Some(update) = self.grid.take_absolute_row_update() {
            apply_absolute_row_update(
                update,
                self.shell,
                self.marks_state,
                self.transient,
                self.absolute_row_revision,
            );
        }
    }

    #[inline]
    pub(super) const fn sgr_style(&mut self) -> SgrStyleHandler<'_> {
        SgrStyleHandler {
            grid: &mut *self.grid,
            style: &mut *self.style,
            transient: &mut *self.transient,
        }
    }

    #[inline]
    pub(super) const fn cursor_state(&mut self) -> CursorStateHandler<'_> {
        CursorStateHandler {
            grid: &mut *self.grid,
            style: &mut *self.style,
            modes: &mut *self.modes,
            charset: &mut *self.charset,
            cursor_save: &mut *self.cursor_save,
            underline_color: &mut self.transient.current_underline_color,
            has_transient_extras: &mut self.transient.has_transient_extras,
            has_hyperlink: self.transient.current_hyperlink.is_some(),
        }
    }

    /// Handle BEL (`0x07`) and fire the rate-limited bell callback.
    pub(super) fn handle_bell(&mut self) {
        const MIN_BELL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);

        let now = self.transient.process_now;
        if let Some(last) = *self.last_bell_time {
            if now.duration_since(last) < MIN_BELL_INTERVAL {
                return;
            }
        }
        *self.last_bell_time = Some(now);

        self.transient.bell_pending = true;
        *self.bell_total = self.bell_total.saturating_add(1);

        if let Some(callback) = self.bell_callback {
            callback();
        }
    }

    /// Invalidate the render-time BiDi projection of every stored row.
    #[inline]
    pub(super) fn invalidate_bidi_all(&mut self) {
        self.grid.damage_mut().mark_full();
    }

    /// Perform a line feed, honoring DECLRMM left/right margins.
    #[inline]
    pub(super) fn margined_line_feed(&mut self, left_right_margin_mode: bool) {
        self.grid.line_feed_margined(left_right_margin_mode);
    }

    /// Write a response (DSR/DA and other replies) to the response buffer.
    pub(super) fn send_response(
        &mut self,
        _cap: &super::response_capability::ResponseCapability,
        response: &[u8],
    ) {
        if !self.can_append_response_bytes(response.len()) {
            return;
        }
        if !program_response_admitted(self.policy, self.transient, response.len()) {
            return;
        }
        if crate::fault::triggered("response.append_alloc")
            || self
                .transient
                .response_buffer
                .try_reserve(response.len())
                .is_err()
        {
            return;
        }
        self.transient.response_buffer.extend_from_slice(response);
    }

    /// Queue a report the program's own output provoked (DEC 2048's enable, or the size report it
    /// held for the alternate screen) with the reports sent unasked (`take_reports`), once it
    /// passes the gates every program reply passes ([`program_response_admitted`]).
    pub(super) fn send_report(
        &mut self,
        _cap: &super::response_capability::ResponseCapability,
        report: &[u8],
    ) {
        if self.can_append_response_bytes(report.len())
            && program_response_admitted(self.policy, self.transient, report.len())
        {
            let _ = self.transient.push_report(report);
        }
    }

    /// Returns whether appending `additional_len` would stay within the response buffer cap.
    #[inline]
    pub(super) const fn can_append_response_bytes(&self, additional_len: usize) -> bool {
        self.transient.owed_len().saturating_add(additional_len) <= MAX_RESPONSE_BUFFER_SIZE
    }

    /// Queue an OSC app-event `(code, payload)` for the host to poll.
    #[inline]
    pub(super) fn queue_osc_event(&mut self, code: u32, payload: String) {
        self.transient.osc_events.push(code, payload);
    }

    /// Filter DCS data for echoing in responses.
    pub(super) fn filter_dcs_data_for_response(data: &[u8]) -> Vec<u8> {
        data.iter()
            .copied()
            .filter(|&b| (0x20..=0x7E).contains(&b))
            .collect()
    }
}

/// Apply a grid coordinate-space update to every durable absolute-row anchor.
pub(super) fn apply_absolute_row_update(
    update: aterm_grid::AbsoluteRowUpdate,
    shell: &mut super::ShellIntegrationState,
    marks_state: &mut super::MarksState,
    transient: &mut TransientState,
    absolute_row_revision: &mut u64,
) {
    *absolute_row_revision = absolute_row_revision.saturating_add(1);
    let aterm_grid::AbsoluteRowUpdate::Splice { at, inserted } = update else {
        shell.command_marks.clear();
        shell.current_mark = None;
        shell.output_blocks.clear();
        shell.current_block = None;
        marks_state.marks.clear();
        marks_state.annotations.clear();
        transient
            .osc_events
            .retain(|code, payload| shell_event_row(payload, code).is_none());
        return;
    };

    transient.osc_events.rewrite(|code, payload| {
        let (command, row, col) = shell_event_row(payload, code)?;
        (row >= at).then(|| format!("{command};row={};col={col}", row.saturating_add(inserted)))
    });

    let shift_point = |row: &mut u64| {
        if *row >= at {
            *row = row.saturating_add(inserted);
        }
    };
    let shift_optional_point = |row: &mut Option<u64>| {
        if let Some(row) = row {
            shift_point(row);
        }
    };

    for mark in shell
        .command_marks
        .iter_mut()
        .chain(shell.current_mark.iter_mut())
    {
        shift_point(&mut mark.prompt_start_row);
        shift_optional_point(&mut mark.command_start_row);
        shift_optional_point(&mut mark.output_start_row);
        shift_optional_point(&mut mark.output_end_row);
    }

    for block in shell
        .output_blocks
        .iter_mut()
        .chain(shell.current_block.iter_mut())
    {
        let starts_at_or_after_splice = block.prompt_start_row >= at;
        shift_point(&mut block.prompt_start_row);
        shift_optional_point(&mut block.command_start_row);
        shift_optional_point(&mut block.output_start_row);
        if let Some(end_row) = &mut block.end_row
            && (*end_row > at || starts_at_or_after_splice)
        {
            *end_row = end_row.saturating_add(inserted);
        }
    }

    for mark in &mut marks_state.marks {
        shift_point(&mut mark.row);
    }
    for annotation in &mut marks_state.annotations {
        shift_point(&mut annotation.row);
    }
}

/// Prune absolute-row anchors evicted by a scrollback erase (`< oldest`).
pub(super) fn forget_erased_history(
    oldest: u64,
    shell: &mut super::ShellIntegrationState,
    marks_state: &mut super::MarksState,
) {
    while shell
        .output_blocks
        .front()
        .is_some_and(|b| block_rows_end(b) <= oldest)
    {
        shell.output_blocks.pop_front();
    }
    shell.output_blocks.make_contiguous();
    while shell
        .command_marks
        .front()
        .is_some_and(|m| mark_rows_end(m) <= oldest)
    {
        shell.command_marks.pop_front();
    }
    shell.command_marks.make_contiguous();
    marks_state.marks.retain(|m| m.row >= oldest);
    marks_state.marks.make_contiguous();
    marks_state.annotations.retain(|a| a.row >= oldest);
    marks_state.annotations.make_contiguous();
    if let Some(block) = &mut shell.current_block
        && block.state != super::BlockState::Complete
    {
        if block
            .command_rows()
            .is_some_and(|(start, end)| end > start && start < oldest)
        {
            block.command_text_cleared = true;
        }
        for row in block_rows_mut(block) {
            *row = (*row).max(oldest);
        }
    }
    if let Some(mark) = &mut shell.current_mark {
        for row in mark_rows_mut(mark) {
            *row = (*row).max(oldest);
        }
    }
}

/// Every row `b` names (prompt, command, output, end), for a transform that keeps their order.
pub(super) fn block_rows_mut(b: &mut super::OutputBlock) -> impl Iterator<Item = &mut u64> {
    std::iter::once(&mut b.prompt_start_row).chain(
        [
            &mut b.command_start_row,
            &mut b.output_start_row,
            &mut b.end_row,
        ]
        .into_iter()
        .flatten(),
    )
}

fn mark_rows_mut(m: &mut super::CommandMark) -> impl Iterator<Item = &mut u64> {
    std::iter::once(&mut m.prompt_start_row).chain(
        [
            &mut m.command_start_row,
            &mut m.output_start_row,
            &mut m.output_end_row,
        ]
        .into_iter()
        .flatten(),
    )
}

/// One past the last row `b` names.
pub(super) fn block_rows_end(b: &super::OutputBlock) -> u64 {
    [
        Some(b.prompt_start_row),
        b.command_start_row,
        b.output_start_row,
    ]
    .into_iter()
    .flatten()
    .map(|row| row.saturating_add(1))
    .chain(b.end_row)
    .max()
    .unwrap_or(0)
}

/// One past the last row `m` names (its `output_end_row` is the D's own row).
pub(super) fn mark_rows_end(m: &super::CommandMark) -> u64 {
    [
        Some(m.prompt_start_row),
        m.command_start_row,
        m.output_start_row,
        m.output_end_row,
    ]
    .into_iter()
    .flatten()
    .map(|row| row.saturating_add(1))
    .max()
    .unwrap_or(0)
}

/// Update output blocks and command marks on a full main-screen clear from `top`.
pub(super) fn note_full_clear(top: u64, from_home: bool, shell: &mut super::ShellIntegrationState) {
    let reaches = |ranges: [Option<(u64, u64)>; 2]| {
        ranges
            .into_iter()
            .flatten()
            .any(|(start, end)| end > start && end > top)
    };
    for block in &mut shell.output_blocks {
        if reaches([block.command_rows(), block.output_rows()]) {
            block.text_cleared = true;
        }
    }
    let marks = &shell.command_marks;
    if let Some(block) = &mut shell.current_block {
        match block.state {
            super::BlockState::Executing => {
                if reaches([block.command_rows(), None]) {
                    block.command_text_cleared = true;
                }
                for row in block_rows_mut(block) {
                    *row = (*row).min(top);
                }
            }
            super::BlockState::Complete => {
                let d_row = marks
                    .back()
                    .filter(|m| {
                        m.prompt_start_row == block.prompt_start_row
                            && m.output_start_row == block.output_start_row
                    })
                    .and_then(|m| m.output_end_row);
                let output = block
                    .output_start_row
                    .map(|start| (start, d_row.unwrap_or(u64::MAX)));
                if reaches([block.command_rows(), output]) {
                    block.text_cleared = true;
                }
                for row in block_rows_mut(block) {
                    *row = (*row).min(top);
                }
            }
            _ if from_home => restart_prompt(
                &mut block.prompt_start_row,
                &mut block.command_start_row,
                top,
            ),
            _ => {}
        }
    }
    if let Some(mark) = &mut shell.current_mark {
        if mark.output_start_row.is_some() {
            for row in mark_rows_mut(mark) {
                *row = (*row).min(top);
            }
        } else if from_home {
            restart_prompt(&mut mark.prompt_start_row, &mut mark.command_start_row, top);
        }
    }
}

#[inline]
const fn restart_prompt(prompt: &mut u64, command: &mut Option<u64>, top: u64) {
    if let Some(row) = command {
        *row = top.saturating_add(row.saturating_sub(*prompt));
    }
    *prompt = top;
}

#[cfg(test)]
#[path = "scrollback_erase_anchor_tests.rs"]
mod scrollback_erase_anchor_tests;

fn shell_event_row(payload: &str, code: u32) -> Option<(char, u64, u16)> {
    if code != 133 {
        return None;
    }
    let (command, rest) = payload.split_once(";row=")?;
    let command = match command {
        "A" => 'A',
        "B" => 'B',
        "C" => 'C',
        _ => return None,
    };
    let (row, col) = rest.split_once(";col=")?;
    Some((command, row.parse().ok()?, col.parse().ok()?))
}

/// The gates every program-originated reply passes, debiting the rate limit at the batch's clock when both open.
pub(super) fn program_response_admitted(
    policy: &mut super::policy_gates::PolicyState,
    transient: &mut TransientState,
    len: usize,
) -> bool {
    if !policy.gates().response_sink().resolve(true) {
        return false;
    }
    let clock = super::response_rate_limiter::BatchClock(transient.process_now);
    if let Some(permitted) = policy.response_rate_limit_try_consume(len as u64, &clock) {
        permitted
    } else {
        transient.response_rate_limiter.try_consume(len, &clock)
    }
}

#[cfg(test)]
mod absolute_row_update_tests {
    use super::super::{
        Annotation, MarksState, OutputBlock, ShellIntegrationState, TerminalMark, TransientState,
    };
    use super::apply_absolute_row_update;

    #[test]
    fn splice_preserves_exclusive_end_boundaries_and_remaps_all_anchor_families() {
        let mut shell = ShellIntegrationState::new();
        let mut before = OutputBlock::new(0, 2, 0);
        before.end_row = Some(3);
        shell.output_blocks.push_back(before);
        let mut at = OutputBlock::new(1, 3, 0);
        at.end_row = Some(3);
        shell.output_blocks.push_back(at);

        let mut marks = MarksState::new();
        marks.marks.push_back(TerminalMark::new(0, 3, 0));
        marks
            .annotations
            .push_back(Annotation::new(0, 3, 0, "footer".into()));

        let mut revision = 0;
        let mut transient = TransientState::new();
        transient.osc_events.push(133, "A;row=3;col=0".into());
        transient.osc_events.push(133, "D;exit=0".into());
        transient.osc_events.push(7, "/tmp".into());
        apply_absolute_row_update(
            aterm_grid::AbsoluteRowUpdate::Splice { at: 3, inserted: 1 },
            &mut shell,
            &mut marks,
            &mut transient,
            &mut revision,
        );

        assert_eq!(shell.output_blocks[0].prompt_start_row, 2);
        assert_eq!(shell.output_blocks[0].end_row, Some(3));
        assert_eq!(shell.output_blocks[1].prompt_start_row, 4);
        assert_eq!(shell.output_blocks[1].end_row, Some(4));
        assert_eq!(marks.marks[0].row, 4);
        assert_eq!(marks.annotations[0].row, 4);
        assert_eq!(
            transient.osc_events.iter().cloned().collect::<Vec<_>>(),
            vec![
                (133, "A;row=4;col=0".into()),
                (133, "D;exit=0".into()),
                (7, "/tmp".into()),
            ]
        );
        assert_eq!(
            transient.osc_events.bytes(),
            "A;row=4;col=0D;exit=0/tmp".len()
        );
        assert_eq!(revision, 1);
    }

    #[test]
    fn invalidation_drops_only_coordinate_bearing_shell_events() {
        let mut shell = ShellIntegrationState::new();
        let mut marks = MarksState::new();
        let mut transient = TransientState::new();
        for event in [
            (133, "A;row=3;col=0".into()),
            (133, "B;row=3;col=1".into()),
            (133, "C;row=3;col=2".into()),
            (133, "D;exit=0".into()),
            (7, "/tmp".into()),
            (52, "clipboard".into()),
        ] {
            transient.osc_events.push(event.0, event.1);
        }
        let mut revision = 0;

        apply_absolute_row_update(
            aterm_grid::AbsoluteRowUpdate::Invalidate,
            &mut shell,
            &mut marks,
            &mut transient,
            &mut revision,
        );

        assert_eq!(
            transient.osc_events.iter().cloned().collect::<Vec<_>>(),
            vec![
                (133, "D;exit=0".into()),
                (7, "/tmp".into()),
                (52, "clipboard".into()),
            ]
        );
        assert_eq!(transient.osc_events.bytes(), "D;exit=0/tmpclipboard".len());
        assert_eq!(revision, 1);
    }
}
