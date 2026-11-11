// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The encoder: Zstandard frames (RFC 8878 §3.1.1) of compressed blocks, written through a
//! [`Write`] adapter or in one call.
//!
//! Memory is bounded by the window: the encoder keeps at most twice the window plus two
//! blocks of input, and its match tables are sized by the level (and shrunk to the content
//! when the content's size is known to be smaller).

mod bits;
mod block;
mod fse;
mod huffman;
mod ldm;
mod literals;
mod matcher;
mod opt;
mod sequences;
mod tree;

use std::io::{self, Write};

use self::block::BlockEncoder;
use self::ldm::LdmParams;
use self::matcher::{Matcher, Search};
use self::opt::{OptParams, Parser};
use self::sequences::Seq;
use self::tree::TreeParams;
use crate::frame::{BLOCK_MAX, Checksum, MAGIC};
use crate::xxh64::Xxh64;

/// The smallest window a frame header can declare (RFC 8878 §3.1.1.1.2).
pub const WINDOW_LOG_MIN: u8 = 10;

/// The largest window this encoder writes: what [`crate::Decoder`] and libzstd's default
/// decoder accept (`frame::WINDOW_MAX`).
pub const WINDOW_LOG_MAX: u8 = 27;

/// The highest level.
pub const LEVEL_MAX: u8 = 19;

/// The level [`Options::new`] starts from.
pub const LEVEL_DEFAULT: u8 = 3;

/// How a level finds its sequences.
#[derive(Clone, Copy, Debug)]
enum Strategy {
    /// Hash chains, searched greedily or lazily.
    Lazy(Search),
    /// A binary tree, parsed by price.
    Opt(OptParams),
}

/// One row of the level table.
struct Level {
    window_log: u8,
    strategy: Strategy,
}

const fn lazy(
    window_log: u8,
    hash_log: u32,
    chain_log: u32,
    depth: u32,
    lazy: u8,
    target: usize,
) -> Level {
    Level {
        window_log,
        strategy: Strategy::Lazy(Search {
            hash_log,
            chain_log,
            depth,
            lazy,
            target,
        }),
    }
}

/// An optimal-parsing row: `depth` nodes compared per search, matches from `min_match`
/// bytes, a match past `target` taken at once; `ultra` prices in fractions of a bit and
/// prunes nothing, `two_pass` parses the first block twice.
#[expect(
    clippy::too_many_arguments,
    reason = "one row of the level table, read as a table"
)]
const fn opt(
    window_log: u8,
    hash_log: u32,
    tree_log: u32,
    depth: u32,
    min_match: u32,
    target: u32,
    ultra: bool,
    two_pass: bool,
) -> Level {
    Level {
        window_log,
        strategy: Strategy::Opt(OptParams {
            tree: TreeParams {
                hash_log,
                tree_log,
                depth,
                min_match,
                target,
            },
            ultra,
            two_pass,
            ldm: None,
        }),
    }
}

/// Levels 1 to 19: wider windows, deeper searches and lazier parsing as the level rises;
/// from 16 the parse is chosen by price over a binary tree, and a block is cut in parts where
/// that costs less. Level 19's window is libzstd's at `-19` (8 MiB), and its tree holds every
/// position of it; a wider window (`Options::window_log`) adds long matches from anchors.
const LEVELS: [Level; LEVEL_MAX as usize] = [
    lazy(19, 16, 16, 2, 0, 16),
    lazy(20, 17, 17, 4, 0, 24),
    lazy(21, 17, 18, 6, 1, 32),
    lazy(21, 18, 18, 8, 1, 32),
    lazy(21, 18, 19, 8, 2, 48),
    lazy(21, 19, 19, 16, 2, 64),
    lazy(22, 19, 20, 24, 2, 64),
    lazy(22, 20, 20, 32, 2, 96),
    lazy(22, 20, 21, 48, 2, 128),
    lazy(22, 20, 21, 64, 2, 128),
    lazy(22, 21, 22, 96, 2, 192),
    lazy(22, 21, 22, 128, 2, 256),
    lazy(22, 21, 22, 192, 2, 256),
    lazy(22, 22, 22, 256, 2, 384),
    lazy(22, 22, 23, 384, 2, 512),
    opt(22, 22, 21, 32, 3, 48, false, false),
    opt(23, 22, 22, 32, 4, 64, false, false),
    opt(23, 22, 22, 64, 3, 64, true, false),
    opt(23, 22, 23, 128, 3, 256, true, true),
];

/// How a frame is written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Options {
    level: u8,
    window_log: Option<u8>,
    checksum: Checksum,
    content_size: Option<u64>,
}

impl Default for Options {
    fn default() -> Self {
        Self::new()
    }
}

impl Options {
    /// Level [`LEVEL_DEFAULT`], its window, a content checksum, no declared size.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            level: LEVEL_DEFAULT,
            window_log: None,
            checksum: Checksum::Xxh64,
            content_size: None,
        }
    }

    /// The compression level, clamped to `1..=`[`LEVEL_MAX`].
    #[must_use]
    pub const fn level(mut self, level: u8) -> Self {
        self.level = if level == 0 {
            1
        } else if level > LEVEL_MAX {
            LEVEL_MAX
        } else {
            level
        };
        self
    }

    /// The window as a power of two, clamped to [`WINDOW_LOG_MIN`]`..=`[`WINDOW_LOG_MAX`];
    /// the level's own when not set. A match reaches at most this far back.
    #[must_use]
    pub const fn window_log(mut self, log: u8) -> Self {
        self.window_log = Some(if log < WINDOW_LOG_MIN {
            WINDOW_LOG_MIN
        } else if log > WINDOW_LOG_MAX {
            WINDOW_LOG_MAX
        } else {
            log
        });
        self
    }

    /// Whether the frame ends with a `Content_Checksum`.
    #[must_use]
    pub const fn checksum(mut self, checksum: Checksum) -> Self {
        self.checksum = checksum;
        self
    }

    /// Declares the content's size up front: the frame header carries it from the first
    /// block, and [`Encoder::finish`] refuses a stream of another size.
    #[must_use]
    pub const fn content_size(mut self, size: u64) -> Self {
        self.content_size = Some(size);
        self
    }

    fn row(&self) -> &'static Level {
        let i = usize::from(self.level.clamp(1, LEVEL_MAX) - 1);
        LEVELS.get(i).unwrap_or(&LEVELS[0])
    }

    fn resolved_window_log(&self) -> u8 {
        self.window_log.unwrap_or(self.row().window_log)
    }
}

/// `content` as one frame.
///
/// ```
/// use std::io::Read;
/// use aterm_zstd::encode::{self, Options};
///
/// let text = b"a frame of text, a frame of text, a frame of text".repeat(100);
/// let zst = encode::compress(&text, &Options::new().level(19));
/// assert!(zst.len() < text.len() / 20);
/// let mut back = Vec::new();
/// aterm_zstd::Decoder::new(&zst[..]).read_to_end(&mut back).unwrap();
/// assert_eq!(back, text);
/// ```
#[must_use]
pub fn compress(content: &[u8], options: &Options) -> Vec<u8> {
    let options = options.content_size(content.len() as u64);
    let mut encoder = Encoder::new(Vec::with_capacity(content.len() / 4 + 64), options);
    let written = encoder.write_all(content).and_then(|()| encoder.finish());
    // Writing to a Vec fails only on allocation, which aborts; the size is the declared one.
    written.unwrap_or_default()
}

/// Writes one Zstandard frame of everything written to it into `W`.
///
/// Input is held until more than a block (128 KiB, or the window if smaller) is pending, so
/// small writes cost nothing; [`Write::flush`] ends the pending block early.
/// [`Encoder::finish`] ends the frame: dropping the encoder without it leaves the frame
/// unfinished, which a decoder refuses as an early end.
///
/// ```
/// use std::io::{Read, Write};
/// use aterm_zstd::encode::{Encoder, Options};
///
/// let mut enc = Encoder::new(Vec::new(), Options::new());
/// for line in 0..1000 {
///     writeln!(enc, "line {line}").unwrap();
/// }
/// let zst = enc.finish().unwrap();
/// let mut back = String::new();
/// aterm_zstd::Decoder::new(&zst[..]).read_to_string(&mut back).unwrap();
/// assert!(back.ends_with("line 999\n"));
/// ```
pub struct Encoder<W: Write> {
    out: W,
    options: Options,
    window_log: u8,
    /// History and pending input: `hist[..encoded]` is encoded, the rest is not.
    hist: Vec<u8>,
    encoded: usize,
    /// The virtual position of `hist[0]` (see [`matcher`]).
    base: u32,
    /// The match finder, made at the first block.
    matcher: Option<Finder>,
    blocks: BlockEncoder,
    hash: Xxh64,
    total: u64,
    frame: Option<Frame>,
    staged: Vec<u8>,
    seqs: Vec<Seq>,
    /// A write failed mid-frame: every later call fails.
    failed: bool,
    /// The base past which positions are moved down ([`REBASE_PAST`]).
    rebase_past: u32,
}

/// What the frame header declared.
#[derive(Clone, Copy, Debug)]
struct Frame {
    /// The farthest a match may reach.
    max_dist: usize,
}

/// A level's match finder.
#[derive(Debug)]
enum Finder {
    Lazy(Matcher),
    Opt(Box<Parser>),
}

impl Finder {
    /// The ring's length: a rebase must move positions by a multiple of it.
    const fn ring_len(&self) -> u32 {
        match self {
            Self::Lazy(m) => m.chain_len(),
            Self::Opt(p) => p.ring_len(),
        }
    }

    /// Moves every position down by `by`, forgetting those below `keep_from`.
    fn rebase(&mut self, by: u32, keep_from: u32) {
        match self {
            Self::Lazy(m) => m.rebase(by, keep_from),
            Self::Opt(p) => p.rebase(by, keep_from),
        }
    }
}

/// The base past which positions are moved down, well before they could overflow.
const REBASE_PAST: u32 = 1 << 30;

impl<W: Write> Encoder<W> {
    /// An encoder writing one frame into `out` as `options` say.
    pub fn new(out: W, options: Options) -> Self {
        Self {
            out,
            options,
            window_log: options.resolved_window_log(),
            hist: Vec::new(),
            encoded: 0,
            base: 1,
            matcher: None,
            blocks: BlockEncoder::new(),
            hash: Xxh64::new(0),
            total: 0,
            frame: None,
            staged: Vec::new(),
            seqs: Vec::new(),
            failed: false,
            rebase_past: REBASE_PAST,
        }
    }

    /// Ends the frame: the last block, then the checksum. Returns the inner writer.
    ///
    /// # Errors
    /// [`io::ErrorKind::InvalidInput`] when a declared content size was not what was
    /// written; otherwise whatever `W` returns.
    pub fn finish(mut self) -> io::Result<W> {
        self.alive()?;
        if let Some(declared) = self.options.content_size
            && declared != self.total
        {
            self.failed = true;
            return Err(size_mismatch(declared, self.total));
        }
        let pending = self.hist.len() - self.encoded;
        self.block(pending, true)?;
        self.out.flush()?;
        Ok(self.out)
    }

    /// The writer the frame goes to.
    pub const fn get_ref(&self) -> &W {
        &self.out
    }

    fn alive(&self) -> io::Result<()> {
        if self.failed {
            Err(io::Error::other(
                "zstd encoder: an earlier error left this frame unfinished",
            ))
        } else {
            Ok(())
        }
    }

    /// The largest block this frame may hold (RFC 8878 §3.1.1.2.4).
    fn block_max(&self) -> usize {
        BLOCK_MAX.min(1 << self.window_log)
    }

    /// The window: what the buffer keeps behind the first pending byte.
    fn window(&self) -> usize {
        1 << self.window_log
    }

    /// The most input the buffer holds: the window behind the first pending byte, room to
    /// slide by another window at a time, and the pending block.
    fn cap(&self) -> usize {
        2 * self.window() + 2 * self.block_max()
    }

    /// Room for more input, sliding the buffer when it is full.
    fn room(&mut self) -> usize {
        let cap = self.cap();
        if self.hist.len() + 1 > cap {
            self.slide();
        }
        cap.saturating_sub(self.hist.len())
    }

    /// Drops the history no match can reach any more.
    fn slide(&mut self) {
        let drop = self.encoded.saturating_sub(self.window());
        if drop == 0 {
            return;
        }
        self.hist.drain(..drop);
        self.encoded -= drop;
        self.base += drop as u32;
        if self.base > self.rebase_past
            && let Some(m) = self.matcher.as_mut()
        {
            let ring = m.ring_len();
            let by = (self.base - 1) / ring * ring;
            m.rebase(by, self.base);
            self.base -= by;
        }
    }

    /// Encodes the next `len` pending bytes as one block.
    fn block(&mut self, len: usize, last: bool) -> io::Result<()> {
        self.staged.clear();
        let frame = match self.frame {
            Some(f) => f,
            None => self.start_frame(last),
        };
        let (start, end) = (self.encoded, self.encoded + len);
        let src = self.hist.get(start..end).unwrap_or_default();
        let searched = if src.len() > 1 && !src.iter().all(|&b| Some(&b) == src.first()) {
            let base = self.base;
            match self.matcher.as_mut() {
                Some(Finder::Lazy(m)) => {
                    m.find(&self.hist, base, start, end, frame.max_dist, &mut self.seqs);
                }
                Some(Finder::Opt(p)) => p.find(
                    &self.hist,
                    base,
                    start,
                    end,
                    frame.max_dist,
                    self.blocks.reps(),
                    &mut self.seqs,
                ),
                None => self.seqs.clear(),
            }
            Some(self.seqs.as_slice())
        } else {
            None
        };
        // Only the parse by price splits its blocks: a lazy level's speed is worth more
        // than the few bytes.
        let split = matches!(self.matcher, Some(Finder::Opt(_)));
        self.blocks
            .write(src, searched, split, last, &mut self.staged);
        self.encoded = end;
        if last && self.options.checksum == Checksum::Xxh64 {
            let sum = self.hash.digest() as u32;
            self.staged.extend_from_slice(&sum.to_le_bytes());
        }
        let wrote = self.out.write_all(&self.staged);
        if wrote.is_err() {
            self.failed = true;
        }
        wrote
    }

    /// Writes the frame header into `staged` and makes the match finder. The content
    /// size is known when this is the last block and nothing was written before, or when
    /// it was declared; a frame whose content fits the window is a single segment.
    fn start_frame(&mut self, last: bool) -> Frame {
        let size = if last { Some(self.total) } else { None }.or(self.options.content_size);
        let window = self.window() as u64;
        let single = size.filter(|&s| s <= window);
        let table_log = match single {
            // Tables need be no larger than the content.
            Some(s) => (u64::BITS - s.max(1).leading_zeros()).clamp(10, u32::from(self.window_log)),
            None => u32::from(self.window_log),
        };
        let first = self.base + self.encoded as u32;
        self.matcher = Some(match self.options.row().strategy {
            Strategy::Lazy(row) => Finder::Lazy(Matcher::new(
                Search {
                    hash_log: row.hash_log.min(table_log + 1),
                    chain_log: row.chain_log.min(table_log),
                    ..row
                },
                first,
            )),
            Strategy::Opt(row) => {
                let tree = TreeParams {
                    hash_log: row.tree.hash_log.min(table_log + 1),
                    tree_log: row.tree.tree_log.min(table_log),
                    ..row.tree
                };
                // A window wider than the tree's ring: long matches past it come from
                // anchors (about one position in 16 under `ultra`, 128 otherwise).
                let ldm = (table_log > tree.tree_log).then(|| {
                    let rate_log = if row.ultra { 4 } else { 7 };
                    LdmParams {
                        table_log: table_log - rate_log,
                        bucket_log: if row.ultra { 8 } else { 3 },
                        min_len: if row.ultra { 32 } else { 64 },
                        rate_log,
                    }
                });
                Finder::Opt(Box::new(Parser::new(OptParams { tree, ldm, ..row }, first)))
            }
        });
        write_header(
            &mut self.staged,
            self.window_log,
            size,
            single.is_some(),
            self.options.checksum,
        );
        // Offsets stay below the window size (RFC 8878 §3.1.1.4); in a single segment the
        // window is the content, which no offset can reach past.
        let frame = Frame {
            max_dist: self.window() - 1,
        };
        self.frame = Some(frame);
        frame
    }
}

impl<W: Write> Write for Encoder<W> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.alive()?;
        if data.is_empty() {
            return Ok(0);
        }
        let n = data.len().min(self.room());
        if let Some(declared) = self.options.content_size
            && self.total + n as u64 > declared
        {
            self.failed = true;
            return Err(size_mismatch(declared, self.total + n as u64));
        }
        let taken = data.get(..n).unwrap_or_default();
        // Grow by doubling, but never past the cap: the buffer's memory is bounded.
        let need = self.hist.len() + n;
        if need > self.hist.capacity() {
            let grown = (2 * self.hist.capacity()).max(need).min(self.cap());
            self.hist.reserve_exact(grown - self.hist.len());
        }
        self.hist.extend_from_slice(taken);
        if self.options.checksum == Checksum::Xxh64 {
            self.hash.update(taken);
        }
        self.total += n as u64;
        let block_max = self.block_max();
        while self.hist.len() - self.encoded > block_max {
            self.block(block_max, false)?;
        }
        Ok(n)
    }

    /// Encodes what is pending as a block (not the last) and flushes `W`, so everything
    /// written so far can be decoded from what `W` holds.
    fn flush(&mut self) -> io::Result<()> {
        self.alive()?;
        let pending = self.hist.len() - self.encoded;
        if pending > 0 {
            self.block(pending, false)?;
        }
        self.out.flush()
    }
}

impl<W: Write> std::fmt::Debug for Encoder<W> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Encoder")
            .field("options", &self.options)
            .field("total", &self.total)
            .field("pending", &(self.hist.len() - self.encoded))
            .finish_non_exhaustive()
    }
}

fn size_mismatch(declared: u64, written: u64) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("zstd encoder: {written} bytes written for a declared content size of {declared}"),
    )
}

/// The magic number and frame header (RFC 8878 §3.1.1.1): the descriptor, the
/// `Window_Descriptor` unless the frame is a single segment, no dictionary, and the
/// `Frame_Content_Size` in the narrowest field that holds it.
fn write_header(
    out: &mut Vec<u8>,
    window_log: u8,
    size: Option<u64>,
    single: bool,
    checksum: Checksum,
) {
    out.extend_from_slice(&MAGIC.to_le_bytes());
    // Flag 0 is no field, or one byte in a single segment; flag 1 is two bytes holding
    // the size less 256; flags 2 and 3 are four and eight bytes (§3.1.1.1.4).
    let flag: u8 = match size {
        None => 0,
        Some(s) if single && s < 256 => 0,
        Some(s) if (256..65_792).contains(&s) => 1,
        Some(s) if s <= u64::from(u32::MAX) => 2,
        Some(_) => 3,
    };
    let checksum_flag = u8::from(checksum == Checksum::Xxh64) << 2;
    out.push(flag << 6 | u8::from(single) << 5 | checksum_flag);
    if !single {
        // Exponent in bits 3-7 (the log less 10), mantissa 0 (§3.1.1.1.2).
        out.push((window_log - WINDOW_LOG_MIN) << 3);
    }
    match (size, flag) {
        (Some(s), 0) => out.push(s as u8),
        (Some(s), 1) => out.extend_from_slice(&((s - 256) as u16).to_le_bytes()),
        (Some(s), 2) => out.extend_from_slice(&(s as u32).to_le_bytes()),
        (Some(s), _) => out.extend_from_slice(&s.to_le_bytes()),
        (None, _) => {}
    }
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
