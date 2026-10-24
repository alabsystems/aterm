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
mod literals;
mod matcher;
mod sequences;

use std::io::{self, Write};

use self::block::BlockEncoder;
use self::matcher::{Matcher, Search};
use self::sequences::Seq;
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

/// One row of the level table.
struct Level {
    window_log: u8,
    search: Search,
}

const fn level(
    window_log: u8,
    hash_log: u32,
    chain_log: u32,
    depth: u32,
    lazy: u8,
    target: usize,
) -> Level {
    Level {
        window_log,
        search: Search {
            hash_log,
            chain_log,
            depth,
            lazy,
            target,
        },
    }
}

/// Levels 1 to 19: wider windows, deeper chains and lazier parsing as the level rises.
/// Level 19's window is libzstd's at `-19` (8 MiB).
///
/// The rows past about 12 are placeholders until a tuned parser (S17 E2): deeper chains
/// buy little there, and they cost most on a small alphabet, where every chain is long
/// and each search walks its full depth. 2 MiB of four symbols at random encodes at
/// 0.49 MB/s at level 19 against `zstd -19`'s 4.73 (one thread, release build, measured
/// 2026-10-23 with the machine under load), still linear in the input: the walk stops at
/// the depth, and at the ring.
const LEVELS: [Level; LEVEL_MAX as usize] = [
    level(19, 16, 16, 2, 0, 16),
    level(20, 17, 17, 4, 0, 24),
    level(21, 17, 18, 6, 1, 32),
    level(21, 18, 18, 8, 1, 32),
    level(21, 18, 19, 8, 2, 48),
    level(21, 19, 19, 16, 2, 64),
    level(22, 19, 20, 24, 2, 64),
    level(22, 20, 20, 32, 2, 96),
    level(22, 20, 21, 48, 2, 128),
    level(22, 20, 21, 64, 2, 128),
    level(22, 21, 22, 96, 2, 192),
    level(22, 21, 22, 128, 2, 256),
    level(22, 21, 22, 192, 2, 256),
    level(22, 22, 22, 256, 2, 384),
    level(22, 22, 23, 384, 2, 512),
    level(23, 22, 23, 512, 2, 768),
    level(23, 22, 23, 640, 2, 1024),
    level(23, 22, 23, 768, 2, 2048),
    level(23, 22, 23, 1024, 2, 4096),
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
/// Input is held until a block (128 KiB, or the window if smaller) more than fits, so
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
    matcher: Option<Matcher>,
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
            let by = (self.base - 1) / m.chain_len() * m.chain_len();
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
            if let Some(m) = self.matcher.as_mut() {
                m.find(&self.hist, base, start, end, frame.max_dist, &mut self.seqs);
            }
            Some(self.seqs.as_slice())
        } else {
            None
        };
        self.blocks.write(src, searched, last, &mut self.staged);
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
        let row = self.options.row().search;
        let search = Search {
            hash_log: row.hash_log.min(table_log + 1),
            chain_log: row.chain_log.min(table_log),
            ..row
        };
        self.matcher = Some(Matcher::new(search, self.base + self.encoded as u32));
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
mod tests {
    use std::io::{Read, Write};

    use super::{Encoder, Options, write_header};
    use crate::Decoder;
    use crate::decode::header;
    use crate::frame::Checksum;

    /// Text from a small vocabulary: matches near and far, at every distance a window holds.
    fn text(len: usize, seed: u64) -> Vec<u8> {
        const WORDS: [&[u8]; 12] = [
            b"frame ",
            b"block ",
            b"window ",
            b"offset ",
            b"literal ",
            b"match ",
            b"table ",
            b"state ",
            b"weight ",
            b"stream ",
            b"repeat ",
            b"\n",
        ];
        let mut x = seed | 1;
        let mut out = Vec::with_capacity(len + 16);
        while out.len() < len {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            out.extend_from_slice(WORDS[(x % 12) as usize]);
            if x.is_multiple_of(97) {
                out.extend_from_slice(&x.to_le_bytes());
            }
        }
        out.truncate(len);
        out
    }

    fn decode(zst: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        Decoder::with_buffer(zst).read_to_end(&mut out).unwrap();
        out
    }

    /// Bytes no match shrinks.
    fn noise(len: usize, seed: u64) -> Vec<u8> {
        let mut x = seed | 1;
        (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                (x >> 32) as u8
            })
            .collect()
    }

    const BLOCK: usize = 128 << 10;

    /// Three blocks. Noise; noise with two 4-byte copies from 125,000 and 130,000 bytes
    /// back near its start, which cost more to code than they save, so the block is
    /// written raw after its sequences were coded; then 16 noise bytes, 32 bytes copied
    /// from 130,000 back, and text. After the raw block a decoder's repeat offsets are
    /// still the frame's first ones; an encoder that kept the raw block's would name the
    /// third block's copy by a repeat offset the decoder does not hold.
    fn raw_after_coding() -> Vec<u8> {
        let mut h = noise(2 * BLOCK + 16, 0x5EED);
        for (at, dist) in [(BLOCK + 40, 125_000), (BLOCK + 150, 130_000)] {
            h.copy_within(at - dist..at - dist + 4, at);
        }
        let from = h.len() - 130_000;
        h.extend_from_within(from..from + 32);
        h.extend(text(BLOCK - 48, 3));
        h
    }

    /// [`raw_after_coding`]'s frames at levels 3 and 9 (level 1's hash table is too small to
    /// keep the far copies' sources), written in one call (its size declared, a single
    /// segment) and streamed a block at a time with and without a flush after each, with
    /// the rollback's fault switched on or not; each with the number of blocks whose coded
    /// sequences were written raw.
    fn raw_after_coding_frames(keep_rejected: bool) -> Vec<(String, Vec<u8>, usize)> {
        let input = raw_after_coding();
        let mut out = Vec::new();
        for level in [3, 9] {
            for (declared, flush) in [(true, false), (false, false), (false, true)] {
                let mut options = Options::new().level(level);
                if declared {
                    options = options.content_size(input.len() as u64);
                }
                let mut enc = Encoder::new(Vec::new(), options);
                enc.blocks.rollback.keep_rejected = keep_rejected;
                for p in input.chunks(BLOCK) {
                    enc.write_all(p).unwrap();
                    if flush {
                        enc.flush().unwrap();
                    }
                }
                let rejected = enc.blocks.rollback.rejected;
                let what = format!("level {level} declared {declared} flush {flush}");
                out.push((what, enc.finish().unwrap(), rejected));
            }
        }
        out
    }

    /// A block whose sequences were coded but which is written raw leaves the entropy
    /// state a decoder carries as it was (RFC 8878 §3.1.1.2: a raw block carries no
    /// table and moves no repeat offset), so the blocks after it name their tables and
    /// offsets from the state the decoder holds.
    #[test]
    fn a_block_written_raw_leaves_the_decoder_state_alone() {
        let input = raw_after_coding();
        for (what, zst, rejected) in raw_after_coding_frames(false) {
            assert!(rejected >= 1, "{what}: a block was coded, then written raw");
            assert!(decode(&zst) == input, "{what}");
        }
    }

    /// The control: an encoder that keeps the raw block's state (the fault the rollback
    /// prevents) writes frames of the same input that the decoder refuses or decodes to
    /// other bytes, so the test above sees that fault.
    #[test]
    fn keeping_a_raw_block_s_state_breaks_the_frame() {
        let input = raw_after_coding();
        for (what, zst, rejected) in raw_after_coding_frames(true) {
            assert!(rejected >= 1, "{what}");
            let mut back = Vec::new();
            let read = Decoder::with_buffer(&zst[..]).read_to_end(&mut back);
            assert!(
                read.is_err() || back != input,
                "{what}: the fault went unseen"
            );
        }
    }

    /// Streams `input` in writes of `piece` through a 1 KiB window, rebasing positions past
    /// `rebase_past`; returns the frame and the encoder's last base.
    fn streamed(input: &[u8], level: u8, piece: usize, rebase_past: u32) -> (Vec<u8>, u32) {
        let mut enc = Encoder::new(Vec::new(), Options::new().level(level).window_log(10));
        enc.rebase_past = rebase_past;
        for p in input.chunks(piece) {
            enc.write_all(p).unwrap();
        }
        let base = enc.base;
        (enc.finish().unwrap(), base)
    }

    /// Positions move down many times over a long stream, and the frame still decodes:
    /// the match finder's tables and the buffer's base move together.
    #[test]
    fn a_stream_past_many_rebases_decodes() {
        let input = text(3 << 20, 7);
        for level in [1, 5, 19] {
            let (zst, base) = streamed(&input, level, 7919, 1 << 14);
            assert!(base < 1 << 15, "level {level}: base {base} was moved down");
            assert!(
                zst.len() < input.len() / 2,
                "level {level}: it still compresses"
            );
            assert!(decode(&zst) == input, "level {level}");
        }
    }

    /// The control: at the default threshold the same stream never rebases, so the test
    /// above saw a path this one does not, and both frames are the same bytes.
    #[test]
    fn below_the_threshold_nothing_rebases() {
        let input = text(3 << 20, 7);
        let (rebased, _) = streamed(&input, 5, 7919, 1 << 14);
        let (plain, base) = streamed(&input, 5, 7919, super::REBASE_PAST);
        assert!(base > (3 << 20) - 8192, "base {base} grew with the input");
        assert!(plain == rebased, "a rebase changes no match");
    }

    /// At a chain ring wider than a block (level 7 under a 2^20 window rings 2^20 links)
    /// and with flushes cutting blocks at odd places, the buffer slides by amounts that
    /// are not multiples of the ring, yet each rebase moves positions by one, so the
    /// rebased stream's frame is the plain one's byte for byte. (At a 2^10 window, as in
    /// the tests above, ring, block and window are all 1 KiB and any drop is a multiple.)
    /// A rebase by another amount files chain links under other ring slots and changes
    /// matches.
    #[test]
    fn a_rebase_past_a_wide_ring_changes_no_match() {
        let input = text(6 << 20, 13);
        let run = |rebase_past: u32| {
            let mut enc = Encoder::new(Vec::new(), Options::new().level(7).window_log(20));
            enc.rebase_past = rebase_past;
            let mut rebases = 0;
            let mut moved = 0;
            for p in input.chunks(100_003) {
                enc.write_all(p).unwrap();
                enc.flush().unwrap();
                // Unmoved, hist[0]'s position is 1 plus the bytes dropped before it.
                let now = enc.total as usize - enc.hist.len() + 1 - enc.base as usize;
                assert_eq!(now % (1 << 20), 0, "moved by {now}");
                rebases += usize::from(now > moved);
                moved = now;
            }
            let ring = enc.matcher.as_ref().map(super::Matcher::chain_len);
            assert_eq!(ring, Some(1 << 20), "the ring is wider than a block");
            (enc.finish().unwrap(), rebases)
        };
        let (rebased, rebases) = run(1 << 20);
        let (plain, none) = run(super::REBASE_PAST);
        assert!(rebases >= 2 && none == 0, "{rebases} rebases, {none}");
        assert!(rebased == plain, "a rebase changes no match");
        assert!(decode(&rebased) == input);
    }

    /// However much is streamed and in whatever writes, the buffer holds at most two
    /// windows and two blocks; it does fill, so the bound is the one that held it.
    #[test]
    fn the_buffer_stays_within_its_bound() {
        let input = text(8 << 20, 11);
        for (window_log, pieces) in [(16u8, [1000, 1 << 20]), (20, [65_537, 3 << 20])] {
            for piece in pieces {
                let mut enc =
                    Encoder::new(Vec::new(), Options::new().level(3).window_log(window_log));
                let cap = enc.cap();
                let mut most = 0;
                for p in input.chunks(piece) {
                    enc.write_all(p).unwrap();
                    most = most.max(enc.hist.capacity());
                    assert!(
                        enc.hist.capacity() <= cap,
                        "{window_log} {piece}: {most} > {cap}"
                    );
                }
                assert!(
                    most > cap / 2,
                    "{window_log} {piece}: the buffer filled ({most} of {cap})"
                );
                assert!(
                    decode(&enc.finish().unwrap()) == input,
                    "{window_log} {piece}"
                );
            }
        }
    }

    /// Every header shape reads back through the decoder's header parser.
    #[test]
    fn headers_read_back() {
        for (log, size, single) in [
            (10u8, None, false),
            (23, None, false),
            (27, Some(5u64 << 30), false),
            (17, Some(300), false),
            (20, Some(0), true),
            (20, Some(255), true),
            (20, Some(256), true),
            (20, Some(65_791), true),
            (20, Some(65_792), true),
            (27, Some(1 << 27), true),
        ] {
            for checksum in [Checksum::Xxh64, Checksum::Omitted] {
                let mut out = Vec::new();
                write_header(&mut out, log, size, single, checksum);
                let h = header::read(&mut &out[4..]).unwrap();
                assert_eq!(h.content_size, size, "{log} {size:?} {single}");
                assert_eq!(h.checksum, checksum == Checksum::Xxh64);
                let window = if single {
                    size.unwrap().max(1024) as usize
                } else {
                    1 << log
                };
                assert_eq!(h.window, window);
            }
        }
    }

    /// The negative control: a single-segment header read as if windowed differs.
    #[test]
    fn a_header_with_its_segment_bit_cleared_reads_otherwise() {
        let mut out = Vec::new();
        write_header(&mut out, 20, Some(300), true, Checksum::Xxh64);
        out[4] &= !0x20;
        let h = header::read(&mut &out[4..]);
        assert!(h.is_err() || h.unwrap().content_size != Some(300));
    }
}
