// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Streaming RFC 1951 (raw DEFLATE) and RFC 1952 (gzip) decompression over
//! `std::io::Read`.
//!
//! The one-shot [`inflate`](super::inflate) in the parent module decompresses a
//! whole slice into a whole `Vec`, which is right for a terminal escape payload
//! and wrong for an archive: atpkg's `tar-gz` and `zip` install lanes hand a
//! decompressed stream straight to a tar/zip walker that vets and writes one
//! entry at a time, under a size cap that reaches 2 GiB. Buffering that in
//! memory to reuse the one-shot entry point would have traded a dependency for
//! a hundreds-of-megabytes allocation.
//!
//! So this is the same engine, driven incrementally. It reuses the parent's
//! `Huffman` construction, its root-table accelerator, its dynamic-table reader
//! and its bit reader **unchanged** — the subtle parts are not written twice.
//! The only thing that is new is the driver:
//!
//! * compressed bytes accumulate in a small input buffer refilled from the
//!   source, and the output keeps a 32 KiB history window plus whatever the
//!   caller has not read yet, so memory is bounded no matter how large the
//!   stream is;
//! * every unit of work — a block header, a table read, one symbol, one stored
//!   byte — runs against a CHECKPOINT of the bit reader. If it hits the end of
//!   what has arrived, the checkpoint is restored, more input is read, and the
//!   unit is retried from exactly where it began. Nothing is decoded twice
//!   beyond the one unit that was interrupted, so the work stays linear.
//!
//! Retry-from-checkpoint is what makes reuse possible at all: the parent's
//! decoders signal "out of input" as [`InflateError::Truncated`] and are written
//! as straight-line recursive descent. Rather than rewrite them as a resumable
//! state machine — a rewrite of the most heavily tested code in the crate —
//! the driver treats `Truncated` as "not yet" whenever the SOURCE has more to
//! give, and as a real error once it does not.

use std::io::{self, Read};

use super::{
    BitReader, DIST_BASE, DIST_EXTRA, Huffman, InflateError, LENGTH_BASE, LENGTH_EXTRA,
    ROOT_BUILD_AFTER, fixed_dist, fixed_lit, read_dynamic_tables,
};
use crate::crc32::Crc32;

/// The DEFLATE sliding window: the furthest a back-reference can reach.
const WINDOW: usize = 32 * 1024;
/// How much decoded output one `pump` aims to produce before handing control
/// back. Large enough that the per-call overhead disappears, small enough that
/// the live output buffer stays around 100 KiB.
const PUMP_TARGET: usize = 32 * 1024;
/// How many bytes must be reclaimable before either buffer is compacted. Both
/// compactions are `memmove`s, so they are done in large, infrequent steps
/// rather than on every read.
const COMPACT_SLACK: usize = 64 * 1024;
/// Bytes pulled from the source per refill.
const IN_CHUNK: usize = 32 * 1024;
/// The largest gzip member header this accepts, so a source that never produces
/// a NUL cannot grow the header buffer without bound. RFC 1952 sets no limit;
/// real headers are tens of bytes and the FNAME/FCOMMENT fields are paths.
const MAX_GZIP_HEADER: usize = 64 * 1024;

/// A saved bit-reader position: everything needed to rebuild a [`BitReader`]
/// over a (possibly extended) input buffer and continue exactly where it left
/// off.
#[derive(Debug, Clone, Copy)]
struct Cursor {
    /// Index of the next byte not yet pulled into the accumulator.
    pos: usize,
    /// The accumulator itself.
    bits: u64,
    /// How many of `bits` are valid.
    count: u32,
}

impl Cursor {
    const fn start() -> Self {
        Self {
            pos: 0,
            bits: 0,
            count: 0,
        }
    }

    fn reader<'a>(&self, data: &'a [u8]) -> BitReader<'a> {
        BitReader {
            data,
            byte_pos: self.pos,
            bit_buffer: self.bits,
            bits_in_buffer: self.count,
        }
    }

    fn save(br: &BitReader<'_>) -> Self {
        Self {
            pos: br.byte_pos,
            bits: br.bit_buffer,
            count: br.bits_in_buffer,
        }
    }

    /// Index of the first input byte that is not yet spoken for — `pos` less the
    /// whole bytes still sitting unread in the accumulator.
    ///
    /// After the final block of a member this is where the gzip trailer starts,
    /// which is the only reason the driver tracks the accumulator's depth at all.
    /// It is also the compaction floor: dropping input at or past this index
    /// would discard bytes the stream still has to interpret.
    const fn byte_position(&self) -> usize {
        self.pos.saturating_sub((self.count / 8) as usize)
    }
}

/// Where in the DEFLATE grammar the decoder is suspended.
enum Phase {
    /// Between blocks: the next three bits are `BFINAL` + `BTYPE`.
    Header,
    /// A stored block's `LEN`/`NLEN` pair has not been read yet.
    StoredHeader,
    /// Inside a stored block, with `remaining` literal bytes still to copy.
    Stored { remaining: u32 },
    /// Inside a Huffman-coded block. `warm` counts the symbols still to decode
    /// before the root tables are built — the parent module's `ROOT_BUILD_AFTER`
    /// deferral, kept so a stream of tiny blocks never pays for tables it cannot
    /// amortise.
    Huff {
        lit: Box<Huffman>,
        dist: Box<Huffman>,
        warm: u32,
    },
    /// The final block is complete.
    Done,
}

/// What one [`Inflater::pump`] achieved.
#[derive(Debug, PartialEq, Eq)]
enum Pump {
    /// Output is available (or the target was already met).
    Output,
    /// The decoder is starved: append to `input` and pump again.
    NeedInput,
    /// The final block completed; no more output will ever be produced.
    Done,
}

/// The resumable DEFLATE core: an input buffer, a bit-reader position into it,
/// the current grammar phase, and the output history window.
struct Inflater {
    input: Vec<u8>,
    cursor: Cursor,
    phase: Phase,
    final_block: bool,
    /// Decoded bytes: at least the last [`WINDOW`] produced (the back-reference
    /// history) plus everything the caller has not taken yet.
    out: Vec<u8>,
    /// Index into `out` of the next byte to hand to the caller.
    out_at: usize,
    /// Total bytes ever decoded, which is the true bound on a back-reference
    /// distance once `out` has been compacted.
    produced: u64,
}

impl Inflater {
    fn new() -> Self {
        Self {
            input: Vec::new(),
            cursor: Cursor::start(),
            phase: Phase::Header,
            final_block: false,
            out: Vec::new(),
            out_at: 0,
            produced: 0,
        }
    }

    /// Bytes decoded but not yet handed to the caller.
    fn pending(&self) -> usize {
        self.out.len().saturating_sub(self.out_at)
    }

    /// Reclaim the front of both buffers, in infrequent large steps.
    ///
    /// The output keeps [`WINDOW`] bytes behind the write head (a back-reference
    /// may reach that far) plus everything undelivered; the input keeps
    /// everything from [`Cursor::byte_position`] on.
    fn compact(&mut self) {
        let drop_out = self.out_at.min(self.out.len().saturating_sub(WINDOW));
        if drop_out >= COMPACT_SLACK {
            self.out.drain(..drop_out);
            self.out_at = self.out_at.saturating_sub(drop_out);
        }
        let drop_in = self.cursor.byte_position();
        if drop_in >= COMPACT_SLACK {
            self.input.drain(..drop_in);
            self.cursor.pos = self.cursor.pos.saturating_sub(drop_in);
        }
    }

    /// Decode until [`PUMP_TARGET`] bytes are pending, the input runs dry, or
    /// the final block ends.
    ///
    /// Every arm follows the same shape: checkpoint, attempt, and on
    /// [`InflateError::Truncated`] restore the checkpoint and report
    /// [`Pump::NeedInput`] — leaving the decoder byte-for-byte as it was before
    /// the attempt, so the retry after a refill is not a partial re-decode.
    fn pump(&mut self) -> Result<Pump, InflateError> {
        let Self {
            input,
            cursor,
            phase,
            final_block,
            out,
            out_at,
            produced,
        } = self;
        let mut br = cursor.reader(input);
        loop {
            if out.len().saturating_sub(*out_at) >= PUMP_TARGET {
                *cursor = Cursor::save(&br);
                return Ok(Pump::Output);
            }
            match phase {
                Phase::Done => {
                    *cursor = Cursor::save(&br);
                    return Ok(Pump::Done);
                }
                Phase::Header => {
                    let save = Cursor::save(&br);
                    let header = (|| {
                        let bfinal = br.read_bits(1)?;
                        let btype = br.read_bits(2)?;
                        Ok::<_, InflateError>((bfinal == 1, btype))
                    })();
                    match header {
                        Err(InflateError::Truncated) => {
                            *cursor = save;
                            return Ok(Pump::NeedInput);
                        }
                        Err(other) => return Err(other),
                        Ok((bfinal, btype)) => {
                            *final_block = bfinal;
                            match btype {
                                0 => *phase = Phase::StoredHeader,
                                1 => {
                                    *phase = Phase::Huff {
                                        lit: Box::new(fixed_lit()),
                                        dist: Box::new(fixed_dist()),
                                        warm: ROOT_BUILD_AFTER,
                                    };
                                }
                                2 => {
                                    // The whole table read is ONE unit: on
                                    // starvation it is retried from the block
                                    // header's end, never resumed half-built.
                                    match read_dynamic_tables(&mut br) {
                                        Err(InflateError::Truncated) => {
                                            *cursor = save;
                                            return Ok(Pump::NeedInput);
                                        }
                                        Err(other) => return Err(other),
                                        Ok((lit, dist)) => {
                                            *phase = Phase::Huff {
                                                lit: Box::new(lit),
                                                dist: Box::new(dist),
                                                warm: ROOT_BUILD_AFTER,
                                            };
                                        }
                                    }
                                }
                                _ => return Err(InflateError::BadBlockType),
                            }
                            *cursor = Cursor::save(&br);
                        }
                    }
                }
                Phase::StoredHeader => {
                    let save = Cursor::save(&br);
                    let head = (|| {
                        br.align_to_byte();
                        let len = br.read_aligned_u16()?;
                        let nlen = br.read_aligned_u16()?;
                        Ok::<_, InflateError>((len, nlen))
                    })();
                    match head {
                        Err(InflateError::Truncated) => {
                            *cursor = save;
                            return Ok(Pump::NeedInput);
                        }
                        Err(other) => return Err(other),
                        Ok((len, nlen)) => {
                            if len != !nlen {
                                return Err(InflateError::BadStoredLength);
                            }
                            *phase = Phase::Stored {
                                remaining: u32::from(len),
                            };
                            *cursor = Cursor::save(&br);
                        }
                    }
                }
                Phase::Stored { remaining } => {
                    while *remaining > 0 {
                        if out.len().saturating_sub(*out_at) >= PUMP_TARGET {
                            break;
                        }
                        let save = Cursor::save(&br);
                        match br.read_aligned_byte() {
                            Err(InflateError::Truncated) => {
                                *cursor = save;
                                return Ok(Pump::NeedInput);
                            }
                            Err(other) => return Err(other),
                            Ok(byte) => {
                                out.push(byte);
                                *produced = produced.saturating_add(1);
                                *remaining = remaining.saturating_sub(1);
                            }
                        }
                    }
                    if *remaining == 0 {
                        *phase = if *final_block {
                            Phase::Done
                        } else {
                            Phase::Header
                        };
                    }
                    *cursor = Cursor::save(&br);
                }
                Phase::Huff { lit, dist, warm } => {
                    loop {
                        if out.len().saturating_sub(*out_at) >= PUMP_TARGET {
                            break;
                        }
                        // The parent module's two-phase decode: the first
                        // `ROOT_BUILD_AFTER` symbols run the bit-at-a-time walk,
                        // then both root tables are armed once and the fast
                        // decoder takes over for the rest of the block.
                        if *warm == 0 {
                            lit.arm_root();
                            dist.arm_root();
                        }
                        let save = Cursor::save(&br);
                        match decode_one(&mut br, lit, dist, *warm > 0, out, produced) {
                            Err(InflateError::Truncated) => {
                                *cursor = save;
                                return Ok(Pump::NeedInput);
                            }
                            Err(other) => return Err(other),
                            Ok(false) => {
                                *warm = warm.saturating_sub(1);
                            }
                            Ok(true) => {
                                *phase = if *final_block {
                                    Phase::Done
                                } else {
                                    Phase::Header
                                };
                                break;
                            }
                        }
                    }
                    *cursor = Cursor::save(&br);
                }
            }
        }
    }
}

/// Decode ONE literal or match into `out`. Returns `true` at end-of-block.
///
/// Byte-for-byte the parent module's `block_loop` body with two differences, both
/// forced by streaming: there is no `max_output` ceiling (nothing accumulates —
/// the caller's own cap is what bounds an expansion bomb, exactly as it did
/// under the retired `flate2`), and a back-reference distance is checked against
/// the TOTAL bytes produced rather than `out.len()`, because `out` has been
/// compacted down to the window and no longer holds the whole output.
fn decode_one(
    br: &mut BitReader<'_>,
    lit: &Huffman,
    dist: &Huffman,
    bitwise: bool,
    out: &mut Vec<u8>,
    produced: &mut u64,
) -> Result<bool, InflateError> {
    let sym = if bitwise {
        lit.decode_bitwise(br)?
    } else {
        lit.decode(br)?
    };
    if sym < 256 {
        // `sym < 256`, so the narrowing cast is lossless.
        out.push(sym as u8);
        *produced = produced.saturating_add(1);
        return Ok(false);
    }
    if sym == 256 {
        return Ok(true);
    }
    // `sym >= 257` here, so the subtraction never actually saturates.
    let li = (sym as usize).saturating_sub(257);
    let (base, extra) = match (LENGTH_BASE.get(li), LENGTH_EXTRA.get(li)) {
        (Some(&b), Some(&e)) => (b, e),
        _ => return Err(InflateError::BadSymbol),
    };
    // `extra <= 5` and RFC 1951 lengths top out at 258; the clamps are no-ops on
    // every real path and bound `length` locally.
    let length = (base as usize + (br.read_bits(extra)?.min(31)) as usize).min(258);
    let dsym = if bitwise {
        dist.decode_bitwise(br)? as usize
    } else {
        dist.decode(br)? as usize
    };
    let (dbase, dextra) = match (DIST_BASE.get(dsym), DIST_EXTRA.get(dsym)) {
        (Some(&b), Some(&e)) => (b, e),
        _ => return Err(InflateError::BadDistance),
    };
    let distance = dbase as usize + br.read_bits(dextra)? as usize;
    // A distance may reach back over bytes already handed to the caller, so the
    // legality test is against everything ever produced; the retained window is
    // what makes it *reachable*. `distance <= WINDOW` always (the largest
    // distance code is 32768), and `compact` never shrinks `out` below `WINDOW`
    // bytes, so a distance that passes here is always inside `out`.
    if distance == 0 || distance as u64 > *produced {
        return Err(InflateError::BadDistance);
    }
    let cur = out.len();
    let Some(start) = cur.checked_sub(distance) else {
        return Err(InflateError::BadDistance);
    };
    out.reserve(length);
    if distance >= length {
        // Non-overlapping: the whole source range is already decoded, so it is
        // one copy.
        out.extend_from_within(start..start.saturating_add(length));
    } else {
        // True RLE overlap must stay byte-by-byte.
        for i in 0..length {
            let Some(&byte) = out.get(start.saturating_add(i)) else {
                return Err(InflateError::BadDistance);
            };
            out.push(byte);
        }
    }
    *produced = produced.saturating_add(length as u64);
    Ok(false)
}

/// Map a decode failure onto an `io::Error` the way a decompressing reader must.
fn io_err(err: InflateError) -> io::Error {
    match err {
        InflateError::Truncated => {
            io::Error::new(io::ErrorKind::UnexpectedEof, "truncated deflate stream")
        }
        other => io::Error::new(io::ErrorKind::InvalidData, other),
    }
}

impl std::fmt::Display for InflateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Truncated => "truncated deflate stream",
            Self::BadZlibHeader => "invalid zlib header",
            Self::BadBlockType => "reserved deflate block type",
            Self::BadStoredLength => "stored block length check failed",
            Self::BadSymbol => "corrupt deflate stream: unresolvable Huffman code",
            Self::BadDistance => "corrupt deflate stream: back-reference out of range",
            Self::OutputTooLarge => "decompressed output exceeds the caller's ceiling",
            Self::BadChecksum => "checksum mismatch",
        })
    }
}

impl std::error::Error for InflateError {}

/// A raw RFC 1951 DEFLATE stream, decompressed as it is read.
///
/// Reading stops at the final block; bytes after it in `source` are left
/// unconsumed, which is what a zip member needs — the member's compressed
/// extent is decided by the central directory, not by the decompressor.
pub struct DeflateReader<R> {
    source: R,
    inflater: Inflater,
    source_eof: bool,
}

impl<R: Read> DeflateReader<R> {
    /// Wrap `source`.
    pub fn new(source: R) -> Self {
        Self {
            source,
            inflater: Inflater::new(),
            source_eof: false,
        }
    }

    /// Consume the wrapper and return the source.
    pub fn into_inner(self) -> R {
        self.source
    }

    /// Pull one chunk from the source into the input buffer. `Ok(false)` means
    /// the source is exhausted.
    fn refill(&mut self) -> io::Result<bool> {
        if self.source_eof {
            return Ok(false);
        }
        let mut chunk = [0u8; IN_CHUNK];
        loop {
            match self.source.read(&mut chunk) {
                Ok(0) => {
                    self.source_eof = true;
                    return Ok(false);
                }
                Ok(n) => {
                    // `n <= chunk.len()`, so the slice is in range; `get` keeps
                    // the bound local and the `None` arm is unreachable.
                    if let Some(fresh) = chunk.get(..n) {
                        self.inflater.input.extend_from_slice(fresh);
                    }
                    return Ok(true);
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
    }

    /// Decode until output is pending or the stream ends.
    fn advance(&mut self) -> io::Result<bool> {
        loop {
            if self.inflater.pending() > 0 {
                return Ok(true);
            }
            match self.inflater.pump().map_err(io_err)? {
                Pump::Output => {
                    if self.inflater.pending() > 0 {
                        return Ok(true);
                    }
                }
                Pump::Done => return Ok(self.inflater.pending() > 0),
                Pump::NeedInput => {
                    if !self.refill()? {
                        return Err(io_err(InflateError::Truncated));
                    }
                }
            }
        }
    }
}

impl<R: Read> Read for DeflateReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        self.inflater.compact();
        if !self.advance()? {
            return Ok(0);
        }
        Ok(take_pending(&mut self.inflater, buf, None))
    }
}

/// Copy up to `buf.len()` pending bytes out of `inf`, optionally folding them
/// into `crc` on the way past. Every produced byte crosses this function exactly
/// once, which is what makes the gzip checksum exact without a second pass.
fn take_pending(inf: &mut Inflater, buf: &mut [u8], crc: Option<&mut Crc32>) -> usize {
    let pending = inf.pending();
    let n = pending.min(buf.len());
    let from = inf.out_at;
    let (Some(src), Some(dst)) = (inf.out.get(from..from.saturating_add(n)), buf.get_mut(..n))
    else {
        return 0;
    };
    dst.copy_from_slice(src);
    if let Some(crc) = crc {
        crc.update(src);
    }
    inf.out_at = inf.out_at.saturating_add(n);
    n
}

/// Why a gzip stream was refused.
fn gz_err(msg: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

/// Parse one RFC 1952 member header out of `buf`.
///
/// `Ok(None)` means "not enough bytes yet"; `Ok(Some(n))` means the header
/// occupies the first `n` bytes and the deflate stream starts at `n`.
fn parse_gzip_header(buf: &[u8]) -> io::Result<Option<usize>> {
    const FHCRC: u8 = 1 << 1;
    const FEXTRA: u8 = 1 << 2;
    const FNAME: u8 = 1 << 3;
    const FCOMMENT: u8 = 1 << 4;

    let Some(fixed) = buf.get(..10) else {
        return Ok(None);
    };
    // Indices 0..10 exist by the `get` above; `get`/`copied` keep each bound
    // local and no `None` arm is reachable.
    if fixed.first().copied() != Some(0x1f) || fixed.get(1).copied() != Some(0x8b) {
        return Err(gz_err("not a gzip stream: bad magic"));
    }
    if fixed.get(2).copied() != Some(8) {
        return Err(gz_err("unsupported gzip compression method"));
    }
    let flg = fixed.get(3).copied().unwrap_or(0);
    if flg & 0xE0 != 0 {
        return Err(gz_err("gzip header sets a reserved flag bit"));
    }
    let mut at = 10usize;
    if flg & FEXTRA != 0 {
        let (Some(lo), Some(hi)) = (buf.get(at).copied(), buf.get(at.saturating_add(1)).copied())
        else {
            return Ok(None);
        };
        let xlen = usize::from(u16::from_le_bytes([lo, hi]));
        at = at.saturating_add(2).saturating_add(xlen);
        if at > buf.len() {
            return if at > MAX_GZIP_HEADER {
                Err(gz_err("gzip header is implausibly long"))
            } else {
                Ok(None)
            };
        }
    }
    for present in [flg & FNAME != 0, flg & FCOMMENT != 0] {
        if !present {
            continue;
        }
        let Some(rest) = buf.get(at..) else {
            return Ok(None);
        };
        let Some(nul) = rest.iter().position(|&b| b == 0) else {
            return if buf.len() > MAX_GZIP_HEADER {
                Err(gz_err("gzip header is implausibly long"))
            } else {
                Ok(None)
            };
        };
        at = at.saturating_add(nul).saturating_add(1);
    }
    if flg & FHCRC != 0 {
        let end = at.saturating_add(2);
        let (Some(head), Some(lo), Some(hi)) = (
            buf.get(..at),
            buf.get(at).copied(),
            buf.get(at.saturating_add(1)).copied(),
        ) else {
            return Ok(None);
        };
        let stored = u16::from_le_bytes([lo, hi]);
        // RFC 1952 §2.3.1: FHCRC is the low 16 bits of the CRC-32 of the header
        // bytes up to (not including) the CRC16 itself.
        let want = (Crc32::of(head) & 0xFFFF) as u16;
        if stored != want {
            return Err(gz_err("gzip header checksum mismatch"));
        }
        at = end;
    }
    if at > MAX_GZIP_HEADER {
        return Err(gz_err("gzip header is implausibly long"));
    }
    Ok(Some(at))
}

/// Which part of a gzip stream the reader is in.
enum GzState {
    /// Looking for the next member header.
    Header,
    /// Inside a member's deflate stream.
    Body,
    /// The member's deflate stream ended; its 8-byte trailer is next.
    Trailer,
    /// Every member has been read and verified.
    Eof,
}

/// A multi-member RFC 1952 gzip stream, decompressed as it is read.
///
/// Multi-member because that is what `gzip -d` does and what the retired
/// `flate2::read::MultiGzDecoder` did: a concatenation of gzip files is itself a
/// gzip file, and real `.tar.gz` producers emit them.
///
/// Every member's CRC-32 and length trailer is CHECKED. That is the property
/// that makes a truncated or corrupted download fail loudly here rather than
/// become a short tar the extractor might otherwise treat as merely small.
pub struct GzipReader<R> {
    source: R,
    /// Bytes read from the source that the framing layer still owns — header
    /// and trailer bytes. Handed to the inflater on entering a member's body and
    /// taken back, minus what the body consumed, on leaving it.
    frame: Vec<u8>,
    inflater: Inflater,
    state: GzState,
    source_eof: bool,
    crc: Crc32,
    member_len: u64,
    /// How many members have been read and verified. A gzip stream must contain
    /// at least one: an EMPTY input is a truncated file, not a zero-member
    /// archive, and the `tar-gz` lane must refuse a zero-byte download rather
    /// than hand the extractor an empty tar. (`flate2`'s `MultiGzDecoder`
    /// refuses it; the differential oracle caught this crate accepting it.)
    members: u64,
}

impl<R: Read> GzipReader<R> {
    /// Wrap `source`.
    pub fn new(source: R) -> Self {
        Self {
            source,
            frame: Vec::new(),
            inflater: Inflater::new(),
            state: GzState::Header,
            source_eof: false,
            crc: Crc32::new(),
            member_len: 0,
            members: 0,
        }
    }

    /// Consume the wrapper and return the source.
    pub fn into_inner(self) -> R {
        self.source
    }

    fn read_source(&mut self, sink: &mut Vec<u8>) -> io::Result<bool> {
        if self.source_eof {
            return Ok(false);
        }
        let mut chunk = [0u8; IN_CHUNK];
        loop {
            match self.source.read(&mut chunk) {
                Ok(0) => {
                    self.source_eof = true;
                    return Ok(false);
                }
                Ok(n) => {
                    if let Some(fresh) = chunk.get(..n) {
                        sink.extend_from_slice(fresh);
                    }
                    return Ok(true);
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
    }

    /// Move the framing buffer into a fresh inflater and start a member body.
    fn begin_body(&mut self, header_len: usize) {
        let rest = self.frame.split_off(header_len.min(self.frame.len()));
        self.frame.clear();
        self.inflater = Inflater::new();
        self.inflater.input = rest;
        self.crc = Crc32::new();
        self.member_len = 0;
        self.state = GzState::Body;
    }

    /// Take the undecoded tail back from the inflater and move to the trailer.
    fn end_body(&mut self) {
        let at = self.inflater.cursor.byte_position();
        let mut rest = std::mem::take(&mut self.inflater.input);
        rest.drain(..at.min(rest.len()));
        self.frame = rest;
        self.state = GzState::Trailer;
    }
}

impl<R: Read> Read for GzipReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            match self.state {
                GzState::Eof => return Ok(0),
                GzState::Header => {
                    match parse_gzip_header(&self.frame)? {
                        Some(len) => self.begin_body(len),
                        None => {
                            if !self.read_source_into_frame()? {
                                self.state = GzState::Eof;
                                return if !self.frame.is_empty() {
                                    Err(gz_err("trailing bytes are not a gzip member"))
                                } else if self.members == 0 {
                                    // Nothing at all: a truncated file, not an
                                    // empty archive.
                                    Err(io::Error::new(
                                        io::ErrorKind::UnexpectedEof,
                                        "empty gzip stream",
                                    ))
                                } else {
                                    // A clean end after a whole number of
                                    // members: exactly where `gzip -d` stops.
                                    Ok(0)
                                };
                            }
                        }
                    }
                }
                GzState::Body => {
                    self.inflater.compact();
                    if self.inflater.pending() > 0 {
                        let n = take_pending(&mut self.inflater, buf, Some(&mut self.crc));
                        self.member_len = self.member_len.saturating_add(n as u64);
                        return Ok(n);
                    }
                    match self.inflater.pump().map_err(io_err)? {
                        Pump::Output => {}
                        Pump::Done => {
                            if self.inflater.pending() == 0 {
                                self.end_body();
                            }
                        }
                        Pump::NeedInput => {
                            let mut sink = std::mem::take(&mut self.inflater.input);
                            let got = self.read_source(&mut sink);
                            self.inflater.input = sink;
                            if !got? {
                                return Err(io_err(InflateError::Truncated));
                            }
                        }
                    }
                }
                GzState::Trailer => {
                    self.verify_trailer()?;
                    self.members = self.members.saturating_add(1);
                    self.state = GzState::Header;
                }
            }
        }
    }
}

impl<R: Read> GzipReader<R> {
    fn read_source_into_frame(&mut self) -> io::Result<bool> {
        let mut sink = std::mem::take(&mut self.frame);
        let got = self.read_source(&mut sink);
        self.frame = sink;
        got
    }

    fn verify_trailer(&mut self) -> io::Result<()> {
        while self.frame.len() < 8 {
            if !self.read_source_into_frame()? {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "truncated gzip trailer",
                ));
            }
        }
        let Some(trailer) = self.frame.get(..8) else {
            return Err(gz_err("short gzip trailer"));
        };
        let mut c = [0u8; 4];
        let mut n = [0u8; 4];
        for (dst, src) in c.iter_mut().zip(trailer.iter()) {
            *dst = *src;
        }
        for (dst, src) in n.iter_mut().zip(trailer.iter().skip(4)) {
            *dst = *src;
        }
        if u32::from_le_bytes(c) != self.crc.finish() {
            return Err(gz_err("gzip member checksum mismatch"));
        }
        if u32::from_le_bytes(n) != (self.member_len & 0xFFFF_FFFF) as u32 {
            return Err(gz_err("gzip member length mismatch"));
        }
        self.frame.drain(..8);
        Ok(())
    }
}
