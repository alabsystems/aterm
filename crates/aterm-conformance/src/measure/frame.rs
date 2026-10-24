// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One captured frame as the scanner reads it: packed RGBA8, top-down rows.

use std::path::Path;

/// One pixel, `[r, g, b]`.
pub type Rgb = [u8; 3];

const MAX_EDGE: u32 = 16_384;

/// A decoded RGBA8 frame (`width * height * 4` bytes, row-major from top).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    width: usize,
    height: usize,
    rgba: Vec<u8>,
}

/// Why a frame could not be read.
#[derive(Debug)]
pub enum FrameError {
    /// The file could not be read.
    Io(std::io::Error),
    /// The bytes are not a PNG this decoder accepts.
    NotPng,
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "{e}"),
            Self::NotPng => f.write_str("not a decodable 8-bit PNG"),
        }
    }
}

impl std::error::Error for FrameError {}

impl Frame {
    /// A `width x height` frame of one colour.
    #[must_use]
    pub fn filled(width: usize, height: usize, ground: Rgb) -> Self {
        let [r, g, b] = ground;
        Self {
            width,
            height,
            rgba: [r, g, b, 255].repeat(width * height),
        }
    }

    /// Decode a PNG through the workspace's one decoder (`aterm-png`).
    /// # Errors
    /// [`FrameError::NotPng`] for anything that is not a decodable 8-bit PNG.
    pub fn decode_png(bytes: &[u8]) -> Result<Self, FrameError> {
        let (rgba, width, height) =
            aterm_png::decode_rgba8(bytes, MAX_EDGE).ok_or(FrameError::NotPng)?;
        Ok(Self {
            width,
            height,
            rgba,
        })
    }

    /// Read and decode the PNG at `path`.
    /// # Errors
    /// The file cannot be read, or is not a PNG.
    pub fn load(path: &Path) -> Result<Self, FrameError> {
        Self::decode_png(&std::fs::read(path).map_err(FrameError::Io)?)
    }

    /// Set the pixel at `(x, y)`.
    /// # Panics
    /// When `(x, y)` is outside the frame.
    pub fn paint(&mut self, x: usize, y: usize, rgb: Rgb) {
        assert!(
            x < self.width && y < self.height,
            "({x}, {y}) is off the frame"
        );
        let i = (y * self.width + x) * 4;
        let [r, g, b] = rgb;
        self.rgba[i..i + 4].copy_from_slice(&[r, g, b, 255]);
    }

    #[must_use]
    pub const fn width(&self) -> usize {
        self.width
    }

    #[must_use]
    pub const fn height(&self) -> usize {
        self.height
    }

    /// The pixel at index `p` (`y * width + x`).
    #[must_use]
    pub fn pixel(&self, p: usize) -> Rgb {
        let i = p * 4;
        [self.rgba[i], self.rgba[i + 1], self.rgba[i + 2]]
    }

    /// The bytes of row `y`.
    #[must_use]
    pub fn row(&self, y: usize) -> &[u8] {
        let span = self.width * 4;
        &self.rgba[y * span..(y + 1) * span]
    }
}
