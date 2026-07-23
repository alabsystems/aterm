// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Shared perception data types for terminal content classification.
//!
//! These types describe semantic regions of terminal content (prompts, commands,
//! errors, code blocks). They live in `aterm-types` so both `aterm-core`
//! (which detects regions) and extraction crates (`aterm-agent`)
//! can reference them without importing `aterm-core`.

/// Unique identifier for a region.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RegionId(pub u32);

impl RegionId {
    /// Create a new region ID.
    #[must_use]
    #[inline]
    pub const fn new(id: u32) -> Self {
        Self(id)
    }
}

/// Types of semantic regions.
///
/// These represent the different kinds of content that can appear
/// in a terminal, helping AI agents understand what they're seeing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum RegionType {
    /// Shell prompt ($ / # / % / >).
    Prompt,
    /// User command being entered or just run.
    Command,
    /// Generic output (default).
    Output,
    /// Error message or error output.
    Error,
    /// Code snippet or block.
    Code,
    /// Tabular data.
    Table,
    /// JSON data.
    Json,
    /// YAML data.
    Yaml,
    /// Interactive chart or visualization.
    Chart,
    /// Clickable/interactive region.
    Interactive,
    /// URL or hyperlink detected in content.
    Link,
}

impl RegionType {
    /// Get a human-readable name for the region type.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Prompt => "prompt",
            Self::Command => "command",
            Self::Output => "output",
            Self::Error => "error",
            Self::Code => "code",
            Self::Table => "table",
            Self::Json => "json",
            Self::Yaml => "yaml",
            Self::Chart => "chart",
            Self::Interactive => "interactive",
            Self::Link => "link",
        }
    }

    /// Parse a region type from its string name.
    ///
    /// Inverse of [`RegionType::name`]. Returns `None` for unrecognized names.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "prompt" => Some(Self::Prompt),
            "command" => Some(Self::Command),
            "output" => Some(Self::Output),
            "error" => Some(Self::Error),
            "code" => Some(Self::Code),
            "table" => Some(Self::Table),
            "json" => Some(Self::Json),
            "yaml" => Some(Self::Yaml),
            "chart" => Some(Self::Chart),
            "interactive" => Some(Self::Interactive),
            "link" => Some(Self::Link),
            _ => None,
        }
    }
}

/// Region position in terminal grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegionBounds {
    /// Start row (0-indexed).
    pub start_row: u32,
    /// End row (exclusive).
    pub end_row: u32,
    /// Start column (0 for full-width).
    pub start_col: u32,
    /// End column (cols for full-width).
    pub end_col: u32,
}

impl RegionBounds {
    /// Create bounds for a single row.
    #[must_use]
    pub fn single_row(row: u32, cols: u32) -> Self {
        Self {
            start_row: row,
            end_row: row.saturating_add(1),
            start_col: 0,
            end_col: cols,
        }
    }

    /// Create bounds spanning multiple rows.
    #[must_use]
    pub fn rows(start: u32, end: u32, cols: u32) -> Self {
        Self {
            start_row: start,
            end_row: end,
            start_col: 0,
            end_col: cols,
        }
    }

    /// Get the number of rows in this region.
    #[must_use]
    pub fn row_count(&self) -> u32 {
        self.end_row.saturating_sub(self.start_row)
    }

    /// Check if a point is within this region.
    #[must_use]
    pub fn contains(&self, row: u32, col: u32) -> bool {
        row >= self.start_row && row < self.end_row && col >= self.start_col && col < self.end_col
    }
}

/// A semantic region of the screen.
#[derive(Debug, Clone)]
pub struct Region {
    /// Unique identifier.
    pub id: RegionId,
    /// What type of content.
    pub kind: RegionType,
    /// Position in terminal grid.
    pub bounds: RegionBounds,
    /// Extracted text content.
    pub content: String,
    /// Detection confidence (0.0-1.0).
    pub confidence: f32,
    /// Language (for code regions).
    pub language: Option<String>,
}

impl Region {
    /// Create a new region.
    #[must_use]
    pub fn new(
        id: RegionId,
        region_type: RegionType,
        bounds: RegionBounds,
        content: String,
    ) -> Self {
        Self {
            id,
            kind: region_type,
            bounds,
            content,
            confidence: 1.0,
            language: None,
        }
    }

    /// Set confidence level.
    #[must_use]
    pub fn with_confidence(mut self, confidence: f32) -> Self {
        self.confidence = confidence;
        self
    }

    /// Set language (for code regions).
    #[must_use]
    pub fn with_language(mut self, language: impl Into<String>) -> Self {
        self.language = Some(language.into());
        self
    }
}

/// Color value for cell-level layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellColor {
    /// Terminal default color (fg or bg).
    Default,
    /// Indexed palette color (0-255).
    Indexed(u8),
    /// True color RGB.
    Rgb(u8, u8, u8),
}

/// Per-cell style information for AI layout mode.
///
/// Only cells with non-space characters or non-default styles are included.
/// This gives AI agents structured access to individual cell positions and
/// visual attributes, enabling layout understanding beyond row-level text.
#[derive(Debug, Clone)]
pub struct CellStyle {
    /// Row position (0-indexed from top of viewport).
    pub row: u32,
    /// Column position (0-indexed).
    pub col: u32,
    /// Character content (may be multi-codepoint grapheme).
    pub text: String,
    /// Display width: 1 for normal, 2 for wide (CJK) characters.
    pub width: u8,
    /// Foreground color.
    pub fg: CellColor,
    /// Background color.
    pub bg: CellColor,
    /// Bold attribute.
    pub bold: bool,
    /// Dim/faint attribute.
    pub dim: bool,
    /// Italic attribute.
    pub italic: bool,
    /// Underline attribute.
    pub underline: bool,
    /// Strikethrough attribute.
    pub strikethrough: bool,
    /// Inverse/reverse video attribute.
    pub inverse: bool,
}

impl CellStyle {
    /// Whether this cell has any non-default style attributes.
    #[must_use]
    pub fn has_style(&self) -> bool {
        self.fg != CellColor::Default
            || self.bg != CellColor::Default
            || self.bold
            || self.dim
            || self.italic
            || self.underline
            || self.strikethrough
            || self.inverse
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn region_bounds_single_row() {
        let bounds = RegionBounds::single_row(5, 80);
        assert_eq!(bounds.start_row, 5);
        assert_eq!(bounds.end_row, 6);
        assert_eq!(bounds.row_count(), 1);
        assert!(bounds.contains(5, 0));
        assert!(bounds.contains(5, 79));
        assert!(!bounds.contains(6, 0));
    }

    #[test]
    fn region_bounds_single_row_saturates_at_max() {
        let bounds = RegionBounds::single_row(u32::MAX, 80);
        assert_eq!(bounds.start_row, u32::MAX);
        assert_eq!(bounds.end_row, u32::MAX);
        assert_eq!(bounds.row_count(), 0);
    }

    #[test]
    fn region_bounds_multi_row() {
        let bounds = RegionBounds::rows(10, 15, 80);
        assert_eq!(bounds.row_count(), 5);
        assert!(bounds.contains(10, 40));
        assert!(bounds.contains(14, 40));
        assert!(!bounds.contains(15, 40));
    }

    #[test]
    fn region_type_names() {
        assert_eq!(RegionType::Prompt.name(), "prompt");
        assert_eq!(RegionType::Error.name(), "error");
        assert_eq!(RegionType::Code.name(), "code");
    }

    #[test]
    fn region_type_from_name_round_trip() {
        let all = [
            RegionType::Prompt,
            RegionType::Command,
            RegionType::Output,
            RegionType::Error,
            RegionType::Code,
            RegionType::Table,
            RegionType::Json,
            RegionType::Yaml,
            RegionType::Chart,
            RegionType::Interactive,
            RegionType::Link,
        ];
        for kind in all {
            assert_eq!(RegionType::from_name(kind.name()), Some(kind));
        }
        assert_eq!(RegionType::from_name("unknown"), None);
        assert_eq!(RegionType::from_name(""), None);
    }

    #[test]
    fn region_with_language() {
        let region = Region::new(
            RegionId::new(0),
            RegionType::Code,
            RegionBounds::single_row(0, 80),
            "fn main() {}".to_string(),
        )
        .with_language("rust");

        assert_eq!(region.language, Some("rust".to_string()));
    }

    #[test]
    fn region_default_confidence() {
        let region = Region::new(
            RegionId::new(0),
            RegionType::Output,
            RegionBounds::single_row(0, 80),
            String::new(),
        );
        assert!((region.confidence - 1.0).abs() < f32::EPSILON);
        assert!(region.language.is_none());
    }
}
