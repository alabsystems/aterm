// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Text sizing types (OSC 66 - Kitty protocol).

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

/// Alignment for text sizing (OSC 66 v/h parameters).
#[non_exhaustive]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TextSizingAlignment {
    /// Start alignment (left/top). Value 0.
    #[default]
    Start,
    /// Center alignment. Value 1.
    Center,
    /// End alignment (right/bottom). Value 2.
    End,
}

impl TextSizingAlignment {
    /// Parse alignment from OSC 66 parameter value (0-2).
    pub fn from_param(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Start),
            1 => Some(Self::Center),
            2 => Some(Self::End),
            _ => None,
        }
    }

    /// Convert to OSC 66 parameter value.
    #[must_use]
    pub fn to_param(self) -> u8 {
        match self {
            Self::Start => 0,
            Self::Center => 1,
            Self::End => 2,
        }
    }
}

/// Text sizing operation (OSC 66 - Kitty protocol).
///
/// Allows client applications to control character dimensions and solve
/// Unicode width coordination problems. Introduced in Kitty v0.40.0.
///
/// # Protocol
///
/// Format: `ESC ] 66 ; metadata ; text BEL/ST`
///
/// Metadata is a colon-separated list of key=value pairs:
/// - `s`: Scale (1-7)
/// - `w`: Width (0-7)
/// - `n`: Numerator (0-15)
/// - `d`: Denominator (0-15)
/// - `v`: Vertical alignment (0-2)
/// - `h`: Horizontal alignment (0-2)
///
/// # Reference
///
/// <https://sw.kovidgoyal.net/kitty/text-sizing-protocol/>
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextSizingOperation {
    /// Scale factor (s parameter, 1-7).
    pub scale: Option<u8>,
    /// Explicit cell width (w parameter, 0-7).
    pub width: Option<u8>,
    /// Fractional scaling numerator (n parameter, 0-15).
    pub numerator: Option<u8>,
    /// Fractional scaling denominator (d parameter, 0-15).
    pub denominator: Option<u8>,
    /// Vertical alignment (v parameter).
    pub vertical_align: TextSizingAlignment,
    /// Horizontal alignment (h parameter).
    pub horizontal_align: TextSizingAlignment,
    /// The text content to render with these sizing hints.
    pub text: String,
}

impl TextSizingOperation {
    /// Create a new text sizing operation with default alignment.
    // Skip: `impl Into<String>` is a CALLER-CHOSEN conversion — `Into::into`
    // dispatches into the caller's impl (open-trait user code, may panic by
    // design); the String alloc is the idiomatic-alloc class besides.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            scale: None,
            width: None,
            numerator: None,
            denominator: None,
            vertical_align: TextSizingAlignment::default(),
            horizontal_align: TextSizingAlignment::default(),
            text: text.into(),
        }
    }

    /// Create a text sizing operation with explicit width.
    // Skip: `impl Into<String>` is a CALLER-CHOSEN conversion — `Into::into`
    // dispatches into the caller's impl (open-trait user code, may panic by
    // design); the String alloc is the idiomatic-alloc class besides.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn with_width(width: u8, text: impl Into<String>) -> Self {
        Self {
            width: Some(width),
            text: text.into(),
            ..Self::new("")
        }
    }

    /// Create a text sizing operation with fractional scaling.
    // Skip: `impl Into<String>` is a CALLER-CHOSEN conversion — `Into::into`
    // dispatches into the caller's impl (open-trait user code, may panic by
    // design); the String alloc is the idiomatic-alloc class besides.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn with_fraction(numerator: u8, denominator: u8, text: impl Into<String>) -> Self {
        Self {
            numerator: Some(numerator),
            denominator: Some(denominator),
            text: text.into(),
            ..Self::new("")
        }
    }

    /// Parse OSC 66 metadata string and text into a text sizing operation.
    pub fn parse(metadata: &str, text: &str) -> Self {
        let mut op = Self::new(text);

        for param in metadata.split(':') {
            let mut parts = param.splitn(2, '=');
            let key = match parts.next() {
                Some(k) => k,
                None => continue,
            };
            let value = match parts.next() {
                Some(v) => v,
                None => continue,
            };

            let parsed = value.parse::<u8>().ok();
            match key {
                "s" => op.scale = parsed.filter(|v| (1..=7).contains(v)).or(op.scale),
                "w" => op.width = parsed.filter(|&v| v <= 7).or(op.width),
                "n" => op.numerator = parsed.filter(|&v| v <= 15).or(op.numerator),
                "d" => op.denominator = parsed.filter(|&v| v <= 15).or(op.denominator),
                "v" => {
                    if let Some(a) = parsed.and_then(TextSizingAlignment::from_param) {
                        op.vertical_align = a;
                    }
                }
                "h" => {
                    if let Some(a) = parsed.and_then(TextSizingAlignment::from_param) {
                        op.horizontal_align = a;
                    }
                }
                _ => {} // Ignore unknown keys for forward compatibility
            }
        }

        op
    }
}
