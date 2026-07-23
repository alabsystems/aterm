// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Universal block model for structured terminal content.
//!
//! A [`Block`] represents a single exchange in the terminal: something was
//! asked, something was answered. Shell commands, REPL inputs, AI prompts —
//! one type, one API, one navigation model.
//!
//! Block sources:
//! - **OSC 133** shell integration (bash/zsh/fish with prompt marks)
//! - **Heuristic parsers** (Python `>>>`, IPython `In[N]:`, etc.)
//! - **Process-specific parsers** (AI Assistant, AI Model, AI Model CLI)

use std::ops::Range;
use std::time::Duration;

/// A single exchange: something was asked, something was answered.
///
/// Shell command + output. REPL input + result. AI prompt + response.
/// This is the universal unit of structured terminal content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    /// What kind of block this is.
    pub kind: BlockKind,
    /// The input portion (command, expression, prompt text).
    pub input: BlockContent,
    /// The output portion (command output, REPL result, AI response).
    pub output: BlockContent,
    /// Row range for the input portion (absolute row numbers).
    pub input_rows: Range<u64>,
    /// Row range for the output portion (absolute row numbers).
    pub output_rows: Range<u64>,
    /// Additional metadata about this block.
    pub metadata: BlockMetadata,
}

/// What program produced this block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum BlockKind {
    /// Shell command via OSC 133 (bash, zsh, fish, etc.)
    ShellCommand,
    /// REPL input/output cycle.
    Repl(ReplKind),
    /// AI CLI conversation exchange.
    AiExchange(AiKind),
    /// Best-effort parse without specific program detection.
    Heuristic,
}

/// Known REPL types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ReplKind {
    /// Python interactive (`>>>` prompt).
    Python,
    /// IPython (`In [N]:` prompt).
    IPython,
    /// Node.js (`>` prompt).
    Node,
    /// Ruby IRB (`irb(...)>` prompt).
    RubyIrb,
    /// Lua interactive.
    Lua,
}

/// Known AI CLI types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum AiKind {
    /// AI Provider AI Assistant CLI.
    ClaudeCode,
    /// AI Provider AI Model CLI.
    Codex,
    /// Google AI Model CLI.
    GeminiCli,
}

/// Text content within a block (input or output portion).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockContent {
    /// The text content.
    pub text: String,
    /// Row range (absolute row numbers, same as parent block's corresponding range).
    pub rows: Range<u64>,
    /// Language hint for syntax highlighting.
    pub language: Option<LanguageId>,
}

impl BlockContent {
    /// Create empty content.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            text: String::new(),
            rows: 0..0,
            language: None,
        }
    }

    /// Whether this content is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }
}

/// Language identifier for syntax highlighting.
/// Language identifier for syntax highlighting.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum LanguageId {
    /// Python source.
    Python,
    /// JavaScript/TypeScript.
    JavaScript,
    /// Ruby.
    Ruby,
    /// Lua.
    Lua,
    /// Shell (bash/zsh/fish).
    Shell,
    /// Rust.
    Rust,
    /// Markdown.
    Markdown,
    /// JSON.
    Json,
    /// Custom language identifier.
    Other(String),
}

/// Metadata about a block.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BlockMetadata {
    /// Exit code (shell commands).
    pub exit_code: Option<i32>,
    /// Working directory at time of execution.
    pub cwd: Option<String>,
    /// Execution duration.
    pub duration: Option<Duration>,
    /// Sequence number (IPython `In[N]`, conversation turn number).
    pub sequence: Option<u64>,
    /// Timestamp when the block started (milliseconds since Unix epoch).
    pub timestamp_ms: Option<u64>,
}

impl Block {
    /// Whether this block is complete (has both input and output).
    #[must_use]
    pub fn is_complete(&self) -> bool {
        !self.output.is_empty() || self.metadata.exit_code.is_some()
    }

    /// Whether this block represents an error (non-zero exit code or error output).
    #[must_use]
    pub fn is_error(&self) -> bool {
        matches!(self.metadata.exit_code, Some(code) if code != 0)
    }

    /// The full row range (input start to output end).
    #[must_use]
    pub fn row_range(&self) -> Range<u64> {
        let start = self.input_rows.start.min(self.output_rows.start);
        let end = self.input_rows.end.max(self.output_rows.end);
        start..end
    }

    /// Whether a given absolute row falls within this block.
    #[must_use]
    pub fn contains_row(&self, row: u64) -> bool {
        let range = self.row_range();
        row >= range.start && row < range.end
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_shell_block(input_rows: Range<u64>, output_rows: Range<u64>) -> Block {
        Block {
            kind: BlockKind::ShellCommand,
            input: BlockContent {
                text: "ls -la".into(),
                rows: input_rows.clone(),
                language: Some(LanguageId::Shell),
            },
            output: BlockContent {
                text: "total 42\ndrwxr-xr-x  10 user staff".into(),
                rows: output_rows.clone(),
                language: None,
            },
            input_rows,
            output_rows,
            metadata: BlockMetadata {
                exit_code: Some(0),
                ..Default::default()
            },
        }
    }

    #[test]
    fn block_is_complete_with_output() {
        let block = make_shell_block(0..1, 1..3);
        assert!(block.is_complete());
    }

    #[test]
    fn block_is_complete_with_exit_code_only() {
        let block = Block {
            kind: BlockKind::ShellCommand,
            input: BlockContent {
                text: "true".into(),
                rows: 0..1,
                language: None,
            },
            output: BlockContent::empty(),
            input_rows: 0..1,
            output_rows: 1..1,
            metadata: BlockMetadata {
                exit_code: Some(0),
                ..Default::default()
            },
        };
        assert!(block.is_complete());
    }

    #[test]
    fn block_not_complete_when_empty() {
        let block = Block {
            kind: BlockKind::Heuristic,
            input: BlockContent {
                text: "typing...".into(),
                rows: 5..6,
                language: None,
            },
            output: BlockContent::empty(),
            input_rows: 5..6,
            output_rows: 6..6,
            metadata: BlockMetadata::default(),
        };
        assert!(!block.is_complete());
    }

    #[test]
    fn block_is_error_with_nonzero_exit() {
        let mut block = make_shell_block(0..1, 1..3);
        block.metadata.exit_code = Some(1);
        assert!(block.is_error());
    }

    #[test]
    fn block_is_not_error_with_zero_exit() {
        let block = make_shell_block(0..1, 1..3);
        assert!(!block.is_error());
    }

    #[test]
    fn block_row_range_spans_input_and_output() {
        let block = make_shell_block(5..7, 7..12);
        assert_eq!(block.row_range(), 5..12);
    }

    #[test]
    fn block_contains_row_in_range() {
        let block = make_shell_block(5..7, 7..12);
        assert!(!block.contains_row(4));
        assert!(block.contains_row(5));
        assert!(block.contains_row(11));
        assert!(!block.contains_row(12));
    }

    #[test]
    fn block_content_empty() {
        let content = BlockContent::empty();
        assert!(content.is_empty());
        assert_eq!(content.rows, 0..0);
    }

    #[test]
    fn block_kind_equality() {
        assert_eq!(BlockKind::ShellCommand, BlockKind::ShellCommand);
        assert_eq!(
            BlockKind::Repl(ReplKind::Python),
            BlockKind::Repl(ReplKind::Python)
        );
        assert_ne!(
            BlockKind::Repl(ReplKind::Python),
            BlockKind::Repl(ReplKind::IPython)
        );
        assert_ne!(BlockKind::ShellCommand, BlockKind::Heuristic);
    }

    #[test]
    fn repl_block_with_sequence() {
        let block = Block {
            kind: BlockKind::Repl(ReplKind::IPython),
            input: BlockContent {
                text: "x = 42".into(),
                rows: 0..1,
                language: Some(LanguageId::Python),
            },
            output: BlockContent::empty(),
            input_rows: 0..1,
            output_rows: 1..1,
            metadata: BlockMetadata {
                sequence: Some(1),
                ..Default::default()
            },
        };
        assert_eq!(block.metadata.sequence, Some(1));
        assert_eq!(block.kind, BlockKind::Repl(ReplKind::IPython));
    }
}
