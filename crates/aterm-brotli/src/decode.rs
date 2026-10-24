// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The decoder: a [`Read`] adapter over one Brotli stream (RFC 7932 §§2–10).

use std::io::{self, Read};

use crate::bits::Bits;
use crate::error::{DecodeError, Fault, Step, corrupt};
use crate::prefix::{Code, read_code};
use crate::tables::{
    BLOCK_COUNT, CELLS, COPY, DICTIONARY, DOFFSET, INSERT, Kind, LUT0, LUT1, LUT2, NDBITS,
    TRANSFORMS, WORD_MAX, WORD_MIN,
};

/// Output one step makes before the reader is handed it: a step stops at the first
/// command boundary past this.
const STEP_OUTPUT: usize = 256 * 1024;

/// History is dropped from the front of the output buffer once at least this much of it
/// (and at least the window) is both read and out of reach.
const COMPACT_AT: usize = 1 << 20;

/// The distance ring buffer's start (§4): the last distance is 4, then 11, 15, 16.
const DISTANCES: [u32; 4] = [16, 15, 11, 4];

/// Where the decoder is in the stream.
enum State {
    /// Before the window size.
    Header,
    /// Between meta-blocks.
    MetaHeader,
    /// Inside a compressed meta-block.
    Compressed(Box<MetaBlock>),
    /// Inside an uncompressed meta-block, with `left` bytes to copy.
    Uncompressed { left: usize },
    /// Inside a metadata block, with `left` bytes to skip; `last` when ISLAST was set.
    Metadata { left: usize, last: bool },
    /// After the last meta-block: the padding and the end of the input.
    Trailer,
    /// The stream ended cleanly.
    Done,
    /// An error was returned; nothing past it is trusted.
    Failed,
}

/// One block category's switching state (§6).
struct Blocks {
    /// NBLTYPES.
    types: u32,
    /// The block type and block count codes, when there are two types or more.
    codes: Option<(Code, Code)>,
    /// The second-to-last and the last block type.
    ring: [u32; 2],
    /// The current type.
    current: u32,
    /// Symbols left in the current block.
    left: u32,
}

impl Blocks {
    fn read<R: Read>(b: &mut Bits<R>) -> Step<Self> {
        let types = read_var_256(b)?;
        let mut out = Self {
            types,
            codes: None,
            ring: [1, 0],
            current: 0,
            left: 0,
        };
        if types >= 2 {
            let type_code = read_code(b, types + 2)?;
            let count_code = read_code(b, 26)?;
            out.left = read_count(b, &count_code)?;
            out.codes = Some((type_code, count_code));
        }
        Ok(out)
    }

    /// Account for one symbol of the category, switching blocks first when the current one
    /// is used up; whether it switched.
    #[inline]
    fn tick<R: Read>(&mut self, b: &mut Bits<R>) -> Step<bool> {
        let Some((type_code, count_code)) = &self.codes else {
            return Ok(false);
        };
        let switched = self.left == 0;
        if switched {
            let mut next = match type_code.decode(b)? {
                0 => self.ring[0],
                1 => self.ring[1] + 1,
                k => k - 2,
            };
            if next >= self.types {
                next -= self.types;
            }
            self.ring = [self.ring[1], next];
            self.current = next;
            self.left = read_count(b, count_code)?;
        }
        self.left -= 1;
        Ok(switched)
    }
}

fn read_count<R: Read>(b: &mut Bits<R>, code: &Code) -> Step<u32> {
    let (base, extra) = BLOCK_COUNT[code.decode(b)? as usize];
    Ok(base + b.read(extra)?)
}

/// NBLTYPES and NTREES (§9.2): 1 to 256.
fn read_var_256<R: Read>(b: &mut Bits<R>) -> Step<u32> {
    if b.read(1)? == 0 {
        return Ok(1);
    }
    let n = b.read(3)?;
    if n == 0 {
        return Ok(2);
    }
    Ok((1 << n) + b.read(n)? + 1)
}

/// A context map (§7.3).
fn read_context_map<R: Read>(b: &mut Bits<R>, size: usize, trees: u32) -> Step<Vec<u8>> {
    let mut map = vec![0u8; size];
    if trees < 2 {
        return Ok(map);
    }
    let rle_max = if b.read(1)? == 1 { b.read(4)? + 1 } else { 0 };
    let code = read_code(b, trees + rle_max)?;
    let mut i = 0;
    while i < size {
        let c = code.decode(b)?;
        if c == 0 {
            i += 1;
        } else if c <= rle_max {
            let run = ((1u32 << c) + b.read(c)?) as usize;
            if i + run > size {
                return corrupt("a context map's run of zeros runs past its end");
            }
            i += run;
        } else {
            map[i] = (c - rle_max) as u8;
            i += 1;
        }
    }
    if b.read(1)? == 1 {
        let mut mtf: [u8; 256] = std::array::from_fn(|i| i as u8);
        for v in &mut map {
            let at = usize::from(*v);
            let value = mtf[at];
            *v = value;
            mtf.copy_within(..at, 1);
            mtf[0] = value;
        }
    }
    if map.iter().any(|&v| u32::from(v) >= trees) {
        return corrupt("a context map names a prefix code that does not exist");
    }
    Ok(map)
}

/// A compressed meta-block's header and where its commands are.
struct MetaBlock {
    /// Bytes still to produce.
    left: usize,
    /// ISLAST.
    last: bool,
    literal: Blocks,
    command: Blocks,
    distance: Blocks,
    npostfix: u32,
    ndirect: u32,
    /// The context mode of each literal block type.
    modes: Vec<u8>,
    literal_map: Vec<u8>,
    distance_map: Vec<u8>,
    literal_codes: Vec<Code>,
    command_codes: Vec<Code>,
    distance_codes: Vec<Code>,
}

impl MetaBlock {
    fn read<R: Read>(b: &mut Bits<R>, left: usize, last: bool) -> Step<Self> {
        let literal = Blocks::read(b)?;
        let command = Blocks::read(b)?;
        let distance = Blocks::read(b)?;
        let npostfix = b.read(2)?;
        let ndirect = b.read(4)? << npostfix;
        let mut modes = Vec::with_capacity(literal.types as usize);
        for _ in 0..literal.types {
            modes.push(b.read(2)? as u8);
        }
        let literal_trees = read_var_256(b)?;
        let literal_map = read_context_map(b, 64 * literal.types as usize, literal_trees)?;
        let distance_trees = read_var_256(b)?;
        let distance_map = read_context_map(b, 4 * distance.types as usize, distance_trees)?;
        let mut literal_codes = Vec::with_capacity(literal_trees as usize);
        for _ in 0..literal_trees {
            literal_codes.push(read_code(b, 256)?);
        }
        let mut command_codes = Vec::with_capacity(command.types as usize);
        for _ in 0..command.types {
            command_codes.push(read_code(b, 704)?);
        }
        let distance_alphabet = 16 + ndirect + (48 << npostfix);
        let mut distance_codes = Vec::with_capacity(distance_trees as usize);
        for _ in 0..distance_trees {
            distance_codes.push(read_code(b, distance_alphabet)?);
        }
        Ok(Self {
            left,
            last,
            literal,
            command,
            distance,
            npostfix,
            ndirect,
            modes,
            literal_map,
            distance_map,
            literal_codes,
            command_codes,
            distance_codes,
        })
    }
}

/// Decodes one Brotli stream (RFC 7932) read from `R`.
///
/// # Errors
///
/// Nothing in the input panics the decoder. A stream breaking a rule of the format is an
/// [`io::ErrorKind::InvalidData`] error carrying a [`DecodeError`] (the large-window
/// extension and any byte after the last meta-block included); input ending inside the
/// stream is [`io::ErrorKind::UnexpectedEof`]; errors from `R` pass through (an
/// [`io::ErrorKind::Interrupted`] read is retried). After any error every later read
/// fails, and nothing the failing step decoded is handed out. Brotli carries no
/// checksum: a stream that decodes is only as trustworthy as whatever vouched for its
/// bytes.
///
/// ```
/// use std::io::Read;
///
/// let mut br = Vec::new();
/// aterm_brotli::write_uncompressed(&mut br, b"hello").unwrap();
/// let mut out = Vec::new();
/// aterm_brotli::Decoder::new(&br[..]).read_to_end(&mut out).unwrap();
/// assert_eq!(out, b"hello");
/// ```
pub struct Decoder<R> {
    bits: Bits<R>,
    state: State,
    /// The window's history followed by output not yet read; `out[read..]` is unread.
    out: Vec<u8>,
    read: usize,
    /// Bytes the stream has decoded to so far.
    total: u64,
    /// The largest backward distance: `(1 << WBITS) - 16`.
    window: usize,
    /// The distance ring buffer; the last distance is at `next_distance - 1` (mod 4).
    distances: [u32; 4],
    next_distance: usize,
    /// The last and the second-to-last byte decoded.
    p1: u8,
    p2: u8,
    /// Where the input was after the last dictionary reference that wrote nothing.
    empty_at: Option<u64>,
}

impl<R: Read> Decoder<R> {
    /// A decoder reading `input`, which it buffers itself.
    pub fn new(input: R) -> Self {
        Self {
            bits: Bits::new(input),
            state: State::Header,
            out: Vec::new(),
            read: 0,
            total: 0,
            window: 0,
            distances: DISTANCES,
            next_distance: 0,
            p1: 0,
            p2: 0,
            empty_at: None,
        }
    }

    /// Bytes the stream has decoded to so far (read or not).
    #[must_use]
    pub const fn decoded(&self) -> u64 {
        self.total
    }

    /// Advance the stream by one step: a header, or up to about [`STEP_OUTPUT`] bytes.
    fn step(&mut self) -> Step<()> {
        self.compact();
        self.state = match std::mem::replace(&mut self.state, State::Failed) {
            State::Header => {
                self.read_window()?;
                State::MetaHeader
            }
            State::MetaHeader => self.meta_header()?,
            State::Compressed(mut mb) => {
                let stop = self.out.len() + STEP_OUTPUT;
                while mb.left > 0 && self.out.len() < stop {
                    self.command(&mut mb)?;
                }
                match (mb.left, mb.last) {
                    (0, true) => State::Trailer,
                    (0, false) => State::MetaHeader,
                    _ => State::Compressed(mb),
                }
            }
            State::Uncompressed { left } => {
                let n = left.min(STEP_OUTPUT);
                let Self { bits, out, .. } = self;
                bits.take_bytes(n, &mut |piece| out.extend_from_slice(piece))?;
                self.total += n as u64;
                self.track_context();
                match left - n {
                    0 => State::MetaHeader,
                    left => State::Uncompressed { left },
                }
            }
            State::Metadata { left, last } => {
                let n = left.min(STEP_OUTPUT);
                self.bits.take_bytes(n, &mut |_| {})?;
                match (left - n, last) {
                    (0, true) => State::Trailer,
                    (0, false) => State::MetaHeader,
                    (left, last) => State::Metadata { left, last },
                }
            }
            State::Trailer => {
                self.bits
                    .align("the padding after the last meta-block is not zero")?;
                if !self.bits.at_end()? {
                    return Err(Fault::Stream(DecodeError::TrailingBytes));
                }
                State::Done
            }
            done @ State::Done => done,
            State::Failed => return corrupt("an earlier error ended this stream"),
        };
        Ok(())
    }

    /// Drop history that is both read and out of every distance's reach.
    fn compact(&mut self) {
        let out_of_reach = self.out.len().saturating_sub(self.window);
        let droppable = self.read.min(out_of_reach);
        if droppable >= self.window.max(COMPACT_AT) {
            self.out.drain(..droppable);
            self.read -= droppable;
        }
    }

    /// WBITS (§9.1).
    fn read_window(&mut self) -> Step<()> {
        let b = &mut self.bits;
        let wbits = if b.read(1)? == 0 {
            16
        } else {
            match b.read(3)? {
                0 => match b.read(3)? {
                    0 => 17,
                    1 => return Err(Fault::Stream(DecodeError::LargeWindow)),
                    m => 8 + m,
                },
                n => 17 + n,
            }
        };
        self.window = (1 << wbits) - 16;
        Ok(())
    }

    /// A meta-block header (§9.2), and the state it starts.
    fn meta_header(&mut self) -> Step<State> {
        let b = &mut self.bits;
        let last = b.read(1)? == 1;
        if last && b.read(1)? == 1 {
            return Ok(State::Trailer);
        }
        let nibbles = b.read(2)?;
        if nibbles == 3 {
            if b.read(1)? != 0 {
                return corrupt("a metadata block sets its reserved bit");
            }
            let bytes = b.read(2)?;
            let mut skip = 0usize;
            for i in 0..bytes {
                let v = b.read(8)?;
                if i + 1 == bytes && bytes > 1 && v == 0 {
                    return corrupt("a metadata length's last byte is zero");
                }
                skip |= (v as usize) << (8 * i);
            }
            if bytes > 0 {
                skip += 1;
            }
            b.align("a metadata block's padding is not zero")?;
            return Ok(State::Metadata { left: skip, last });
        }
        let nibbles = nibbles + 4;
        let mut len = 0usize;
        for i in 0..nibbles {
            let v = b.read(4)?;
            if i + 1 == nibbles && nibbles > 4 && v == 0 {
                return corrupt("a meta-block length's last nibble is zero");
            }
            len |= (v as usize) << (4 * i);
        }
        len += 1;
        if !last && b.read(1)? == 1 {
            b.align("an uncompressed meta-block's padding is not zero")?;
            return Ok(State::Uncompressed { left: len });
        }
        Ok(State::Compressed(Box::new(MetaBlock::read(b, len, last)?)))
    }

    /// The last two bytes, read from the end of the output.
    fn track_context(&mut self) {
        let n = self.out.len();
        self.p1 = if n >= 1 { self.out[n - 1] } else { 0 };
        self.p2 = if n >= 2 { self.out[n - 2] } else { 0 };
    }

    /// One command (§5): its literals, then a copy or a dictionary word unless the
    /// meta-block ends first.
    fn command(&mut self, mb: &mut MetaBlock) -> Step<()> {
        let b = &mut self.bits;
        mb.command.tick(b)?;
        let code = mb.command_codes[mb.command.current as usize].decode(b)?;
        let (insert_code, copy_code, implicit) = CELLS[(code >> 6) as usize];
        let (base, extra) = INSERT[(insert_code + ((code >> 3) & 7)) as usize];
        let insert = (base + b.read(extra)?) as usize;
        let (base, extra) = COPY[(copy_code + (code & 7)) as usize];
        let copy = (base + b.read(extra)?) as usize;
        if insert > mb.left {
            return corrupt("an insert runs past its meta-block");
        }
        let (mut p1, mut p2) = (self.p1, self.p2);
        let mut mode = mb.modes[mb.literal.current as usize];
        let mut map = 64 * mb.literal.current as usize;
        for _ in 0..insert {
            if mb.literal.tick(b)? {
                mode = mb.modes[mb.literal.current as usize];
                map = 64 * mb.literal.current as usize;
            }
            let context = match mode {
                0 => p1 & 0x3f,
                1 => p1 >> 2,
                2 => LUT0[usize::from(p1)] | LUT1[usize::from(p2)],
                _ => (LUT2[usize::from(p1)] << 3) | LUT2[usize::from(p2)],
            };
            let tree = mb.literal_map[map + usize::from(context)];
            let byte = mb.literal_codes[usize::from(tree)].decode(b)? as u8;
            self.out.push(byte);
            p2 = p1;
            p1 = byte;
        }
        (self.p1, self.p2) = (p1, p2);
        mb.left -= insert;
        self.total += insert as u64;
        if mb.left == 0 {
            return Ok(());
        }
        let dcode = if implicit {
            0
        } else {
            mb.distance.tick(b)?;
            let context = if copy > 4 { 3 } else { copy - 2 };
            let tree = mb.distance_map[4 * mb.distance.current as usize + context];
            mb.distance_codes[usize::from(tree)].decode(b)?
        };
        let distance = self.distance(mb, dcode)?;
        let reach = (self.window as u64).min(self.total);
        if distance > reach {
            self.word(mb, copy, distance - reach - 1)?;
        } else {
            self.copy(mb, copy, distance as usize)?;
            if dcode != 0 {
                self.distances[self.next_distance & 3] = distance as u32;
                self.next_distance = self.next_distance.wrapping_add(1);
            }
        }
        self.track_context();
        Ok(())
    }

    /// The distance `dcode` names (§4).
    fn distance(&mut self, mb: &MetaBlock, dcode: u32) -> Step<u64> {
        let last = |k: usize| i64::from(self.distances[(self.next_distance + 4 - k) & 3]);
        let distance = if dcode < 16 {
            const RING: [(usize, i64); 16] = [
                (1, 0),
                (2, 0),
                (3, 0),
                (4, 0),
                (1, -1),
                (1, 1),
                (1, -2),
                (1, 2),
                (1, -3),
                (1, 3),
                (2, -1),
                (2, 1),
                (2, -2),
                (2, 2),
                (2, -3),
                (2, 3),
            ];
            let (k, delta) = RING[dcode as usize];
            last(k) + delta
        } else if dcode < 16 + mb.ndirect {
            i64::from(dcode - 15)
        } else {
            let x = dcode - mb.ndirect - 16;
            let bits = 1 + (x >> (mb.npostfix + 1));
            let high = x >> mb.npostfix;
            let low = x & ((1 << mb.npostfix) - 1);
            let offset = ((2 + (high & 1)) << bits) - 4;
            let extra = self.bits.read(bits)?;
            ((i64::from(offset) + i64::from(extra)) << mb.npostfix)
                + i64::from(low)
                + i64::from(mb.ndirect)
                + 1
        };
        if distance <= 0 {
            return corrupt("a distance is not positive");
        }
        Ok(distance as u64)
    }

    /// Copy `len` bytes from `distance` back.
    fn copy(&mut self, mb: &mut MetaBlock, len: usize, distance: usize) -> Step<()> {
        if len > mb.left {
            return corrupt("a copy runs past its meta-block");
        }
        // The output holds at least the window's reach of history, so `distance` is in it.
        let start = self.out.len() - distance;
        let mut left = len;
        while left > 0 {
            // From `start` the output repeats every `distance` bytes, and the bytes at
            // `start..` are a whole number of repeats, so copying them continues it.
            let n = left.min(self.out.len() - start);
            self.out.extend_from_within(start..start + n);
            left -= n;
        }
        mb.left -= len;
        self.total += len as u64;
        Ok(())
    }

    /// The static dictionary's word `word_id` of `len` bytes, transformed (§8).
    fn word(&mut self, mb: &mut MetaBlock, len: usize, word_id: u64) -> Step<()> {
        if !(WORD_MIN as usize..=WORD_MAX as usize).contains(&len) {
            return corrupt("a dictionary reference's length is not 4 to 24");
        }
        let bits = NDBITS[len];
        let index = (word_id & ((1 << bits) - 1)) as usize;
        let Some(transform) = TRANSFORMS.get((word_id >> bits) as usize) else {
            return corrupt("a dictionary reference names a transform past the 121");
        };
        let base = &DICTIONARY[DOFFSET[len] + index * len..][..len];
        let at = self.out.len();
        self.out.extend_from_slice(transform.prefix);
        let word = match transform.kind {
            Kind::OmitFirst(n) => base.get(usize::from(n)..).unwrap_or(&[]),
            Kind::OmitLast(n) => &base[..len.saturating_sub(usize::from(n))],
            Kind::Identity | Kind::FermentFirst | Kind::FermentAll => base,
        };
        let word_at = self.out.len();
        self.out.extend_from_slice(word);
        let w = &mut self.out[word_at..];
        match transform.kind {
            Kind::FermentFirst if !w.is_empty() => {
                ferment(w, 0);
            }
            Kind::FermentAll => {
                let mut i = 0;
                while i < w.len() {
                    i += ferment(w, i);
                }
            }
            _ => {}
        }
        self.out.extend_from_slice(transform.suffix);
        let n = self.out.len() - at;
        if n > mb.left {
            return corrupt("a dictionary word runs past its meta-block");
        }
        if n == 0 {
            // A command that wrote nothing must have taken input, or the next would be it
            // again, for ever.
            let here = self.bits.position();
            if self.empty_at == Some(here) {
                return corrupt("a dictionary reference writes nothing and takes no input");
            }
            self.empty_at = Some(here);
        }
        mb.left -= n;
        self.total += n as u64;
        Ok(())
    }
}

/// Upper-case the character at `w[pos]` the format's way, returning its length (Appendix
/// B's Ferment).
fn ferment(w: &mut [u8], pos: usize) -> usize {
    if w[pos] < 192 {
        if w[pos].is_ascii_lowercase() {
            w[pos] ^= 32;
        }
        1
    } else if w[pos] < 224 {
        if pos + 1 < w.len() {
            w[pos + 1] ^= 32;
        }
        2
    } else {
        if pos + 2 < w.len() {
            w[pos + 2] ^= 5;
        }
        3
    }
}

impl<R: Read> Read for Decoder<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            let pending = &self.out[self.read..];
            if !pending.is_empty() {
                let n = pending.len().min(buf.len());
                buf[..n].copy_from_slice(&pending[..n]);
                self.read += n;
                return Ok(n);
            }
            match self.state {
                State::Done => return Ok(0),
                State::Failed => {
                    return Err(io::Error::other(
                        "brotli: an earlier error ended this stream",
                    ));
                }
                _ => {}
            }
            if let Err(e) = self.step() {
                // What the failed step wrote is never handed out: nothing past a refusal is.
                self.out.truncate(self.read);
                self.state = State::Failed;
                return Err(e.into());
            }
        }
    }
}
