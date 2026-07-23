// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Terminal orchestration boundary traits and shared DTOs.
//!
//! These contracts let orchestrator-style consumers interact with terminal state
//! without importing concrete `aterm-core` internals.

use crate::TerminalSize;
use crate::perception::{CellStyle, Region, RegionType};
use crate::shell_blocks::RowSpan;

/// Stable block lifecycle state for orchestration consumers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum TerminalBlockState {
    /// Block contains only prompt text.
    PromptOnly,
    /// User is entering a command.
    EnteringCommand,
    /// Command is executing and may stream output.
    Executing,
    /// Command completed and has final status.
    Complete,
}

/// Shared block summary for command/output navigation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalBlockSnapshot {
    /// Session-unique block identifier.
    pub id: u64,
    /// Block lifecycle state.
    pub state: TerminalBlockState,
    /// Absolute prompt start row.
    pub prompt_start_row: u64,
    /// Absolute command start row (if command text began).
    pub command_start_row: Option<u64>,
    /// Absolute output start row (if output started).
    pub output_start_row: Option<u64>,
    /// Absolute block end row, exclusive.
    pub end_row: Option<u64>,
    /// Process exit code if command completed.
    pub exit_code: Option<i32>,
    /// Working directory captured for this block, if available.
    pub working_directory: Option<String>,
    /// Explicit command line text if provided by shell integration.
    pub commandline: Option<String>,
    /// Whether output is currently collapsed.
    pub collapsed: bool,
}

impl TerminalBlockSnapshot {
    /// Whether the block's command completed successfully (exit code 0).
    #[must_use]
    pub fn succeeded(&self) -> bool {
        self.exit_code == Some(0)
    }

    /// Whether the block's command failed (non-zero exit code).
    #[must_use]
    pub fn failed(&self) -> bool {
        matches!(self.exit_code, Some(code) if code != 0)
    }

    /// Whether this block is complete (finished executing).
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.state == TerminalBlockState::Complete
    }

    /// Typed row span for the output portion, if available.
    #[must_use]
    pub fn output_row_span(&self) -> Option<RowSpan> {
        let start = self.output_start_row?;
        let end = self.end_row.unwrap_or(start.saturating_add(1));
        Some(RowSpan::new(start, end))
    }

    /// Compatibility shim for callers still using tuple row ranges.
    #[must_use]
    pub fn output_rows(&self) -> Option<(u64, u64)> {
        self.output_row_span().map(RowSpan::as_tuple)
    }
}

/// Shared perception payload for non-rendering consumers.
///
/// Contains text-mode content (lines, full_text), styled-mode content
/// (styled_lines, styled_text with ANSI escapes), and semantic-mode
/// content (regions, hints). The `regions` field enables extraction crates
/// to access typed content regions without importing `aterm-core`.
#[derive(Clone, Debug)]
pub struct TerminalPerceptionSnapshot {
    /// Visible lines in the current viewport (plain text, no escapes).
    pub lines: Vec<String>,
    /// Visible content as one newline-joined string.
    pub full_text: String,
    /// Lines with ANSI escape sequences preserving cell colors and attributes.
    pub styled_lines: Vec<String>,
    /// Full styled text as one newline-joined string (with ANSI escapes).
    pub styled_text: String,
    /// Current terminal viewport size.
    pub size: TerminalSize,
    /// Current working directory hint, if known.
    pub current_working_directory: Option<String>,
    /// Whether shell appears ready for input.
    pub prompt_ready: bool,
    /// Last observed command text, if available.
    pub last_command: Option<String>,
    /// Last observed exit code, if available.
    pub last_exit_code: Option<i32>,
    /// Whether a command is currently executing.
    pub is_executing: bool,
    /// Typed content regions (prompts, commands, errors, code, etc.).
    pub regions: Vec<Region>,
    /// Per-cell style data for layout mode (non-empty cells only).
    pub cells: Vec<CellStyle>,
    /// Security: injection risk level (`"low"`, `"medium"`, `"high"`).
    pub security_risk_level: String,
    /// Security: recommended action (`"proceed"`, `"verify"`, `"reject"`).
    pub security_recommendation: String,
    /// Security: whether shell integration is verified.
    pub security_shell_integration: bool,
    /// Security: per-region trust levels, parallel to `regions`.
    ///
    /// Each entry is `"trusted"`, `"untrusted"`, or `"suspicious"`.
    pub region_trust_levels: Vec<String>,
}

impl TerminalPerceptionSnapshot {
    /// Get text content with optional line limit.
    ///
    /// # Arguments
    ///
    /// * `_include_scrollback` - Reserved for future scrollback integration
    /// * `max_lines` - Maximum lines to return (0 = unlimited)
    #[must_use]
    pub fn text_with_scrollback(&self, _include_scrollback: bool, max_lines: usize) -> String {
        if max_lines > 0 && max_lines < self.lines.len() {
            // `max_lines < self.lines.len()` was just checked, so the total
            // `get(..max_lines)` always yields `Some` — behaviour is unchanged;
            // it only discharges the Trust gate's slice-bounds obligation.
            match self.lines.get(..max_lines) {
                Some(slice) => slice.join("\n"),
                None => self.full_text.clone(),
            }
        } else {
            self.full_text.clone()
        }
    }

    /// Check if there are any error regions.
    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.regions.iter().any(|r| r.kind == RegionType::Error)
    }
}

/// Read/write terminal host surface needed by orchestrator paths.
pub trait TerminalHost {
    /// Feed parser input bytes into the terminal.
    fn process(&mut self, input: &[u8]);

    /// Resize viewport dimensions.
    fn resize(&mut self, rows: u16, cols: u16);

    /// Get current viewport dimensions.
    fn size(&self) -> TerminalSize;

    /// Get visible viewport content.
    fn visible_content(&self) -> String;

    /// Get current working directory.
    fn current_working_directory(&self) -> Option<String>;

    /// Get total scrollback line count.
    fn scrollback_line_count(&self) -> usize;

    /// Get one scrollback line by index (0 = oldest).
    fn scrollback_line(&self, index: usize) -> Option<String>;

    /// Get one scrollback line by reverse index (0 = newest).
    fn scrollback_line_from_end(&self, reverse_index: usize) -> Option<String>;
}

/// Block inspection surface for command/output navigation.
pub trait TerminalBlockAccess {
    /// List completed and in-progress blocks.
    fn blocks(&self) -> Vec<TerminalBlockSnapshot>;

    /// Get command text for a block ID.
    fn block_command(&self, block_id: u64) -> Option<String>;

    /// Get output text for a block ID.
    fn block_output(&self, block_id: u64) -> Option<String>;
}

/// AI-perception view for terminal state snapshots.
pub trait TerminalPerception {
    /// Build a shared perception snapshot.
    fn perception_snapshot(&self) -> TerminalPerceptionSnapshot;
}

/// Combined trait for types that implement the full orchestration surface.
///
/// Rust does not allow multiple non-auto traits in a single trait object
/// (`dyn A + B + C` is `E0225`). This supertrait gathers all three so that
/// orchestrator code can hold `&dyn TerminalHostFull` or `&mut dyn TerminalHostFull`.
///
/// The `Send` bound is required because I/O drivers and orchestrator code may
/// need to move terminal references across thread boundaries.
pub trait TerminalHostFull: TerminalHost + TerminalBlockAccess + TerminalPerception + Send {}

impl<T: TerminalHost + TerminalBlockAccess + TerminalPerception + Send> TerminalHostFull for T {}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockTerminal;

    impl TerminalHost for MockTerminal {
        fn process(&mut self, _input: &[u8]) {}

        fn resize(&mut self, _rows: u16, _cols: u16) {}

        fn size(&self) -> TerminalSize {
            TerminalSize::new(24, 80)
        }

        fn visible_content(&self) -> String {
            "hello".to_string()
        }

        fn current_working_directory(&self) -> Option<String> {
            Some("/tmp".to_string())
        }

        fn scrollback_line_count(&self) -> usize {
            1
        }

        fn scrollback_line(&self, index: usize) -> Option<String> {
            (index == 0).then(|| "line".to_string())
        }

        fn scrollback_line_from_end(&self, reverse_index: usize) -> Option<String> {
            (reverse_index == 0).then(|| "line".to_string())
        }
    }

    impl TerminalBlockAccess for MockTerminal {
        fn blocks(&self) -> Vec<TerminalBlockSnapshot> {
            vec![TerminalBlockSnapshot {
                id: 1,
                state: TerminalBlockState::Complete,
                prompt_start_row: 0,
                command_start_row: Some(0),
                output_start_row: Some(1),
                end_row: Some(2),
                exit_code: Some(0),
                working_directory: Some("/tmp".to_string()),
                commandline: Some("echo hi".to_string()),
                collapsed: false,
            }]
        }

        fn block_command(&self, block_id: u64) -> Option<String> {
            (block_id == 1).then(|| "echo hi".to_string())
        }

        fn block_output(&self, block_id: u64) -> Option<String> {
            (block_id == 1).then(|| "hi".to_string())
        }
    }

    impl TerminalPerception for MockTerminal {
        fn perception_snapshot(&self) -> TerminalPerceptionSnapshot {
            TerminalPerceptionSnapshot {
                lines: vec!["hello".to_string()],
                full_text: "hello".to_string(),
                styled_lines: vec!["hello".to_string()],
                styled_text: "hello".to_string(),
                size: TerminalSize::new(24, 80),
                current_working_directory: Some("/tmp".to_string()),
                prompt_ready: true,
                last_command: Some("echo hi".to_string()),
                last_exit_code: Some(0),
                is_executing: false,
                regions: Vec::new(),
                cells: Vec::new(),
                security_risk_level: "low".to_string(),
                security_recommendation: "proceed".to_string(),
                security_shell_integration: false,
                region_trust_levels: Vec::new(),
            }
        }
    }

    #[test]
    fn traits_compile_with_mock_terminal() {
        let mut terminal = MockTerminal;
        terminal.process(b"ls");
        terminal.resize(30, 100);
        assert_eq!(terminal.size(), TerminalSize::new(24, 80));
        assert_eq!(terminal.visible_content(), "hello");
        assert_eq!(
            terminal.current_working_directory().as_deref(),
            Some("/tmp")
        );
        assert_eq!(terminal.scrollback_line_count(), 1);
        assert_eq!(terminal.scrollback_line(0).as_deref(), Some("line"));
        assert_eq!(
            terminal.scrollback_line_from_end(0).as_deref(),
            Some("line")
        );

        let blocks = terminal.blocks();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].state, TerminalBlockState::Complete);
        assert_eq!(blocks[0].output_row_span(), Some(RowSpan::new(1, 2)));
        assert_eq!(terminal.block_command(1).as_deref(), Some("echo hi"));
        assert_eq!(terminal.block_output(1).as_deref(), Some("hi"));

        let perception = terminal.perception_snapshot();
        assert_eq!(perception.size, TerminalSize::new(24, 80));
        assert!(perception.prompt_ready);
    }

    #[test]
    fn terminal_block_snapshot_row_span_helpers_preserve_exclusive_end() {
        let snapshot = TerminalBlockSnapshot {
            id: 7,
            state: TerminalBlockState::Complete,
            prompt_start_row: 10,
            command_start_row: Some(10),
            output_start_row: Some(12),
            end_row: Some(15),
            exit_code: Some(0),
            working_directory: None,
            commandline: None,
            collapsed: false,
        };

        assert_eq!(snapshot.output_row_span(), Some(RowSpan::new(12, 15)));
        assert_eq!(snapshot.output_rows(), Some((12, 15)));
    }

    #[test]
    fn terminal_block_snapshot_defaults_missing_end_row_to_single_output_row() {
        let snapshot = TerminalBlockSnapshot {
            id: 8,
            state: TerminalBlockState::Executing,
            prompt_start_row: 4,
            command_start_row: Some(4),
            output_start_row: Some(6),
            end_row: None,
            exit_code: None,
            working_directory: None,
            commandline: None,
            collapsed: false,
        };

        assert_eq!(snapshot.output_row_span(), Some(RowSpan::new(6, 7)));
    }

    /// Regression: u64::MAX + 1 wrapped to 0 before saturating_add fix (#5715).
    #[test]
    fn terminal_block_snapshot_output_row_span_saturates_at_u64_max() {
        let snapshot = TerminalBlockSnapshot {
            id: 0,
            state: TerminalBlockState::Executing,
            prompt_start_row: 0,
            command_start_row: None,
            output_start_row: Some(u64::MAX),
            end_row: None,
            exit_code: None,
            working_directory: None,
            commandline: None,
            collapsed: false,
        };
        let span = snapshot.output_row_span().expect("should have output span");
        assert!(
            span.start_row <= span.end_row_exclusive,
            "output_row_span must not wrap: start={}, end={}",
            span.start_row,
            span.end_row_exclusive
        );
        assert_eq!(span.row_count(), 0);
    }
}
