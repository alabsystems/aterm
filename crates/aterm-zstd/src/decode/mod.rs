// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The decoder: a [`Read`] adapter over a stream of Zstandard frames.

pub(crate) mod bits;
mod block;
mod error;
pub(crate) mod fse;
pub(crate) mod header;
pub(crate) mod huffman;
pub(crate) mod literals;
pub(crate) mod sequences;
mod window;

use std::io::{self, BufRead, BufReader, Read};

pub use error::DecodeError;

use self::block::BlockDecoder;
use self::header::FrameHeader;
use self::window::{BlockBuf, Window};
use crate::frame::MAGIC;
use crate::xxh64::Xxh64;

/// Skippable frames carry any of these 16 magic numbers (RFC 8878 §3.1.2).
const SKIPPABLE_MAGIC: std::ops::RangeInclusive<u32> = 0x184D_2A50..=0x184D_2A5F;

/// Where the decoder is in the stream.
#[derive(Clone, Copy, Debug)]
enum State {
    /// Between frames (or before the first).
    FrameStart,
    /// Inside a frame's blocks.
    Blocks(FrameHeader),
    /// The input ended cleanly after a frame.
    Done,
    /// An error was returned; the stream is not trusted past it. It came at a
    /// frame's magic number or header when `between_frames` holds.
    Failed { between_frames: bool },
}

/// Decodes a stream of Zstandard frames read from `R`: the shape of `zstd::Decoder` (a [`Read`] adapter), for what atpkg's installer reads.
/// # Errors
///
/// Nothing in the input panics the decoder. A frame breaking a rule is an
/// [`io::ErrorKind::InvalidData`] error carrying a [`DecodeError`]; input
/// ending inside a frame is [`io::ErrorKind::UnexpectedEof`]; errors from
/// `R` pass through. After any error every later read fails.
///
/// ```
/// use std::io::Read;
/// use aterm_zstd::Decoder;
/// use aterm_zstd::frame::{self, Checksum};
///
/// let mut zst = Vec::new();
/// frame::write_raw(&mut zst, b"hello", Checksum::Xxh64).unwrap();
/// let mut out = Vec::new();
/// Decoder::new(&zst[..]).read_to_end(&mut out).unwrap();
/// assert_eq!(out, b"hello");
/// ```
pub struct Decoder<R> {
    input: R,
    state: State,
    /// Whole frames (skippable ones included) decoded so far.
    frames: u64,
    blocks: BlockDecoder,
    history: Window,
    /// The current block's output; `out[pos..]` is not yet read.
    out: BlockBuf,
    pos: usize,
    /// Bytes the current frame has decoded, and their hash when the frame
    /// carries a checksum.
    produced: u64,
    hash: Xxh64,
    /// Refuse a frame that carries no content checksum.
    require_checksum: bool,
    /// Refuse a skippable frame, and any byte after the first frame ends.
    one_frame: bool,
}

impl<R: Read> Decoder<BufReader<R>> {
    /// A decoder reading `reader` through a [`BufReader`].
    pub fn new(reader: R) -> Self {
        Self::with_buffer(BufReader::new(reader))
    }
}

impl<R: BufRead> Decoder<R> {
    /// A decoder reading an already-buffered `reader`.
    pub fn with_buffer(reader: R) -> Self {
        Self {
            input: reader,
            state: State::FrameStart,
            frames: 0,
            blocks: BlockDecoder::new(),
            history: Window::default(),
            out: BlockBuf::default(),
            pos: 0,
            produced: 0,
            hash: Xxh64::new(0),
            require_checksum: false,
            one_frame: false,
        }
    }

    /// Refuses every frame that carries no content checksum ([`DecodeError::NoChecksum`]) at its header, before any of its content is read.
    #[must_use]
    pub const fn require_checksum(mut self) -> Self {
        self.require_checksum = true;
        self
    }

    /// Takes the stream as exactly one zstd frame: a skippable frame, or any byte after the
    /// first frame ends, is [`DecodeError::NotOneFrame`], refused before it is read.
    #[must_use]
    pub const fn one_frame(mut self) -> Self {
        self.one_frame = true;
        self
    }

    /// Advances one frame header or block. `Ok(false)` once the stream has
    /// ended cleanly.
    fn step(&mut self) -> io::Result<bool> {
        match self.state {
            State::FrameStart => self.start_frame(),
            State::Blocks(frame) => {
                self.next_block(frame)?;
                Ok(true)
            }
            State::Done => Ok(false),
            State::Failed { .. } => {
                Err(io::Error::other("zstd: an earlier error ended this stream"))
            }
        }
    }

    /// Whether all this decoder has decoded belongs to whole frames: before
    /// the first frame, between frames, at the end, and after an error at a
    /// frame's magic number or header, before any of that frame's content.
    /// `false` inside a frame's blocks and after an error there.
    #[must_use]
    pub const fn between_frames(&self) -> bool {
        match self.state {
            State::FrameStart | State::Done => true,
            State::Blocks(_) => false,
            State::Failed { between_frames } => between_frames,
        }
    }

    /// Reads a frame's magic and header, or skips a skippable frame; at the
    /// end of the input, ends the stream.
    fn start_frame(&mut self) -> io::Result<bool> {
        let at_end = loop {
            match self.input.fill_buf() {
                Ok(buf) => break buf.is_empty(),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        };
        if at_end {
            if self.frames == 0 {
                return Err(early_end());
            }
            self.state = State::Done;
            return Ok(false);
        }
        if self.one_frame && self.frames > 0 {
            return Err(DecodeError::NotOneFrame.into());
        }
        let mut word = [0u8; 4];
        self.input.read_exact(&mut word).map_err(inside_frame)?;
        let magic = u32::from_le_bytes(word);
        if SKIPPABLE_MAGIC.contains(&magic) {
            if self.one_frame {
                return Err(DecodeError::NotOneFrame.into());
            }
            self.input.read_exact(&mut word).map_err(inside_frame)?;
            let len = u64::from(u32::from_le_bytes(word));
            let skipped = io::copy(&mut (&mut self.input).take(len), &mut io::sink())?;
            if skipped != len {
                return Err(early_end());
            }
            self.frames += 1;
            return Ok(true);
        }
        if magic != MAGIC {
            return Err(DecodeError::BadMagic(magic).into());
        }
        let frame = header::read(&mut self.input).map_err(inside_frame)?;
        if self.require_checksum && !frame.checksum {
            return Err(DecodeError::NoChecksum.into());
        }
        self.blocks.reset();
        self.history.reset(frame.window);
        self.produced = 0;
        self.hash = Xxh64::new(0);
        self.state = State::Blocks(frame);
        Ok(true)
    }

    /// Decodes the next block into `out`; after the last one, checks the
    /// content size and checksum before any of it is read.
    fn next_block(&mut self, frame: FrameHeader) -> io::Result<()> {
        self.pos = 0;
        let last = self
            .blocks
            .decode(
                &mut self.input,
                frame.block_max,
                &self.history,
                &mut self.out,
            )
            .map_err(inside_frame)?;
        self.produced += self.out.len() as u64;
        if let Some(declared) = frame.content_size
            && self.produced > declared
        {
            return Err(DecodeError::ContentSize {
                declared,
                decoded: self.produced,
            }
            .into());
        }
        if frame.checksum {
            self.hash.update(self.out.as_slice());
        }
        if !last {
            self.history.push(self.out.as_slice());
            return Ok(());
        }

        if let Some(declared) = frame.content_size
            && self.produced != declared
        {
            return Err(DecodeError::ContentSize {
                declared,
                decoded: self.produced,
            }
            .into());
        }
        if frame.checksum {
            let mut stored = [0u8; 4];
            self.input.read_exact(&mut stored).map_err(inside_frame)?;
            let stored = u32::from_le_bytes(stored);
            let computed = self.hash.digest() as u32;
            if stored != computed {
                return Err(DecodeError::Checksum { stored, computed }.into());
            }
        }
        self.frames += 1;
        self.state = State::FrameStart;
        Ok(())
    }
}

impl<R: BufRead> Read for Decoder<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            let pending = self.out.as_slice().get(self.pos..).unwrap_or_default();
            if !pending.is_empty() {
                let n = pending.len().min(buf.len());
                if let (Some(dst), Some(src)) = (buf.get_mut(..n), pending.get(..n)) {
                    dst.copy_from_slice(src);
                }
                self.pos += n;
                return Ok(n);
            }
            match self.step() {
                Ok(true) => {}
                Ok(false) => return Ok(0),
                Err(e) => {
                    self.state = State::Failed {
                        between_frames: self.between_frames(),
                    };
                    self.out.start(0);
                    self.pos = 0;
                    return Err(e);
                }
            }
        }
    }
}

impl<R> std::fmt::Debug for Decoder<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Decoder")
            .field("state", &self.state)
            .field("frames", &self.frames)
            .field("produced", &self.produced)
            .finish_non_exhaustive()
    }
}

/// The `Frame_Content_Size` the frame at the start of `input` declares, read from its magic
/// number and header alone: `None` when the header declares none. No block is read.
///
/// # Errors
/// [`io::ErrorKind::InvalidData`] carrying a [`DecodeError`] when `input` does not start with
/// a zstd frame (a skippable frame included) or its header breaks a rule the decoder holds it
/// to; [`io::ErrorKind::UnexpectedEof`] when it ends inside the header; errors from `input`.
pub fn declared_content_size(mut input: impl Read) -> io::Result<Option<u64>> {
    let mut word = [0u8; 4];
    input.read_exact(&mut word).map_err(inside_frame)?;
    let magic = u32::from_le_bytes(word);
    if magic != MAGIC {
        return Err(DecodeError::BadMagic(magic).into());
    }
    Ok(header::read(&mut input).map_err(inside_frame)?.content_size)
}

/// The stream ended where a frame was required.
fn early_end() -> io::Error {
    io::Error::new(
        io::ErrorKind::UnexpectedEof,
        "zstd: input ends inside a frame",
    )
}

/// Names an early end inside a frame as such; other errors pass through.
fn inside_frame(e: io::Error) -> io::Error {
    if e.kind() == io::ErrorKind::UnexpectedEof {
        early_end()
    } else {
        e
    }
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
