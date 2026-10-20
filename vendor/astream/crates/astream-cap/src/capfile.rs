//! The capability FILE: one canonical `<grant> <tag-hex>` line format, and the one
//! reader for it.
//!
//! A cap file holds a node's whole ring, one minted capability per line, in exactly
//! the shape `asb mint` prints — so the file a mint wrote is the file every face
//! reads, and a ring one face accepts is never refused by another:
//!
//! ```text
//! # this node's ring: the fleet's read half, and its own write lane
//! ro:/f/F/pub/>  <64 hex digits>
//! rw,p=n-a1b2c3d4:/f/F/in/*/*/n-a1b2c3d4/*  <64 hex digits>
//! ```
//!
//! THE RULES, and why each is the one:
//!
//! * A line is trimmed. Empty lines, and lines whose first character is `#`, are
//!   skipped. A grant never begins with `#` (it begins with `/`, `rw` or `ro`),
//!   so a comment can never swallow a capability; a comment is a WHOLE line, and a
//!   `#` after the tag is a malformed tag, never a trailing comment.
//! * The line splits at its LAST ASCII whitespace, never its first, because THE
//!   GRANT MAY CONTAIN A SPACE. `Filter::new` rejects only bytes below 0x20 and
//!   0x7f, and 0x20 is neither, so `ro:/f/F/pub a/>` is a legal grant that a mint
//!   seals and prints. Split at the first whitespace, that line read back as the
//!   grant `ro:/f/F/pub` with the tag `a/>`, and the whole ring was refused. The
//!   last whitespace is exact rather than merely better: the tag is the fixed-width
//!   whitespace-free tail, so even a grant that ENDS in a space round-trips.
//! * The grant half must be non-empty. It is otherwise NOT validated here: it is the
//!   string the tag seals, byte for byte, and whether it is a grant at all is
//!   `Grant::parse`'s question, asked where authority is decided.
//! * The tag half is EXACTLY 64 ASCII hex digits, either case: 32 bytes, one
//!   HMAC-SHA256 output. Strict — no sign, no `0x`, no other length — and decoded by
//!   BYTE, so a multibyte character is simply not hex, never a char-boundary panic.
//!   No error message echoes the tag, because the tag is the secret.
//!
//! [`format_line`] writes a line and [`parse_line`] reads one back; [`parse_text`]
//! and [`read_file`] read a whole ring and name `path:line` for the first line that
//! breaks a rule. A ring is all or nothing: one malformed line refuses the file.
//!
//! SELF-CONTAINED ON PURPOSE: std only, nothing from the rest of this crate. `asb`
//! reads `--cap-file` in its DEFAULT build too — a malformed line is a usage error
//! there as well, and only the attach itself needs the MAC — but a default broker
//! does not depend on `astream-cap` (capabilities are its opt-in `cap` feature),
//! so `asb` compiles THIS FILE into itself by path. Keep it free of
//! `crate::` items and of every dependency: one source, two builds, and no second
//! reader to drift from the first.

use std::fmt;
use std::path::Path;

#[cfg(clean_islands)]
#[path = "capfile_island.rs"]
mod capfile_island;
#[cfg(clean_islands)]
pub use capfile_island::capfile_line_advance;

/// `capfile_line_advance` as `capfile_island.rs` defines it in Clean:
/// `if cur < cand then cand else cur`.
#[cfg(not(clean_islands))]
#[must_use]
pub const fn capfile_line_advance(cur: u64, cand: u64) -> u64 {
    if cur < cand {
        cand
    } else {
        cur
    }
}

contract_fn! {
    /// Verified check that a capability tag hex slice is 64 ASCII bytes (`len == 64`),
    /// linked via `clean-kernel-defeq` to `capfile_tag_hex_len_valid_isl`.
    #[must_use]
    pub const fn capfile_tag_hex_len_valid(len: usize) -> bool
    clauses [ensures result == (len == 64)]
    {
        len == 64
    }
}

/// A tag's width in bytes: one HMAC-SHA256 output.
pub(crate) const TAG_BYTES: usize = 32;

/// The digits [`format_line`] writes: lowercase, the one spelling a mint emits.
const HEX: &[u8; 16] = b"0123456789abcdef";

/// One capability line: the grant and the tag that seals it.
///
/// Its `Debug` output names the grant but redacts the tag, as `Capability`'s does:
/// the tag IS the bearer secret, and debug output ends up in logs.
#[derive(Clone, PartialEq, Eq)]
pub struct Line {
    /// The grant, byte for byte as it was minted — the string the tag is over.
    pub grant: String,
    /// The HMAC-SHA256 tag, decoded from the line's 64 hex digits.
    pub tag: [u8; TAG_BYTES],
}

impl fmt::Debug for Line {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Line")
            .field("grant", &self.grant)
            .field("tag", &"<redacted>")
            .finish()
    }
}

/// Why one line is not a capability line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// No ASCII whitespace separates a non-empty grant from a tag: the line holds a
    /// grant with no tag, or a tag with no grant.
    Shape,
    /// The tag half is not 64 bytes long; this is the length it has.
    TagLength(usize),
    /// The tag half holds a byte that is not an ASCII hex digit, at this index.
    TagNotHex(usize),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Shape => f.write_str("expected `<grant> <tag-hex>`"),
            Error::TagLength(n) => write!(
                f,
                "tag: expected {} hex chars ({TAG_BYTES} bytes), got {n} bytes",
                TAG_BYTES * 2
            ),
            Error::TagNotHex(i) => write!(f, "tag: not valid hex at char {i}"),
        }
    }
}

impl std::error::Error for Error {}

/// A malformed line, located — displayed `<path>:<line>: <why>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Malformed {
    /// Where the text came from, as the caller named it (usually the file's path).
    pub path: String,
    /// The 1-based line number, counting blank and comment lines.
    pub line: usize,
    /// What is wrong with that line.
    pub error: Error,
}

impl fmt::Display for Malformed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}: {}", self.path, self.line, self.error)
    }
}

impl std::error::Error for Malformed {}

/// Why a cap file could not be read into a ring. The two are kept apart because
/// they are different mistakes: a path that is wrong, and a file that is wrong.
#[derive(Debug)]
pub enum ReadError {
    /// The file could not be read at all — absent, unreadable, or not UTF-8.
    /// Displayed `<path>: <error>`.
    Io {
        /// The path, as the caller gave it.
        path: String,
        /// What reading it failed with.
        error: std::io::Error,
    },
    /// The file was read, and a line of it breaks a rule.
    Malformed(Malformed),
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReadError::Io { path, error } => write!(f, "{path}: {error}"),
            ReadError::Malformed(m) => m.fmt(f),
        }
    }
}

impl std::error::Error for ReadError {}

#[inline]
const fn write_hex_byte(out: &mut [u8; 2], b: u8) {
    out[0] = HEX[(b >> 4) as usize];
    out[1] = HEX[(b & 0x0f) as usize];
}

/// The one line a cap file holds for a capability: `<grant> <tag-hex>`, the tag as
/// 64 LOWERCASE hex digits after ONE space.
///
/// [`parse_line`] reads it back to the same grant and tag for every grant that is
/// non-empty, starts with neither whitespace nor `#`, and holds no line break —
/// which every string `Grant::parse` accepts does. A space INSIDE the grant, or at
/// its end, is fine: that is what splitting at the last whitespace is for.
#[must_use]
pub fn format_line(grant: &str, tag: &[u8; TAG_BYTES]) -> String {
    // The copy of `grant` sizes itself; only the constant-size tail is reserved,
    // so no length sum of ours can overflow.
    const TAIL: usize = 1 + TAG_BYTES * 2;
    let mut line = grant.to_owned();
    line.reserve(TAIL);
    line.push(' ');
    let mut pair = [0u8; 2];
    let mut i = 0usize;
    while i < TAG_BYTES {
        write_hex_byte(&mut pair, tag[i]);
        if let Ok(s) = std::str::from_utf8(&pair) {
            line.push_str(s);
        }
        i += 1;
    }
    line
}

/// Read one line of a cap file (its terminator may be left on: it is trimmed like
/// any other whitespace). `Ok(None)` for a blank or `#` comment line.
///
/// # Errors
/// [`Error::Shape`] when no ASCII whitespace separates a non-empty grant from a
/// tag; [`Error::TagLength`] / [`Error::TagNotHex`] when the tag is not exactly 64
/// ASCII hex digits. The rules are the module's.
pub fn parse_line(line: &str) -> Result<Option<Line>, Error> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return Ok(None);
    }
    let Some((grant, tag_hex)) = line.rsplit_once([' ', '\t', '\n', '\r', '\x0c']) else {
        return Err(Error::Shape);
    };
    if grant.is_empty() {
        return Err(Error::Shape);
    }
    match decode_tag(tag_hex.as_bytes()) {
        Ok(tag) => Ok(Some(Line {
            grant: grant.to_string(),
            tag,
        })),
        Err(e) => Err(e),
    }
}

/// Read a whole ring from `text`, in order, skipping blank and comment lines.
/// `path` names the text in an error and is not opened.
///
/// # Errors
/// The FIRST malformed line, as `path:line`; no partial ring is returned.
pub fn parse_text(path: &str, text: &str) -> Result<Vec<Line>, Malformed> {
    let mut out = Vec::new();
    let mut line_no = 0usize;
    for raw in text.lines() {
        let next_no = line_no.saturating_add(1) as u64;
        line_no = capfile_line_advance(line_no as u64, next_no) as usize;
        match parse_line(raw) {
            Ok(Some(item)) => out.push(item),
            Ok(None) => {}
            Err(error) => {
                return Err(Malformed {
                    path: path.to_string(),
                    line: line_no,
                    error,
                });
            }
        }
    }
    Ok(out)
}

/// Read a whole ring from the file at `path` — [`parse_text`] over its contents,
/// with errors naming the path as [`Path::display`] shows it.
///
/// # Errors
/// [`ReadError::Io`] when the file cannot be read as UTF-8 text at all;
/// [`ReadError::Malformed`] naming `path:line` for its first malformed line.
pub fn read_file(path: impl AsRef<Path>) -> Result<Vec<Line>, ReadError> {
    let path = path.as_ref();
    let shown = path.display().to_string();
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(error) => {
            return Err(ReadError::Io { path: shown, error });
        }
    };
    let text = match core::str::from_utf8(&bytes) {
        Ok(t) => t,
        Err(e) => {
            return Err(ReadError::Io {
                path: shown,
                error: std::io::Error::new(std::io::ErrorKind::InvalidData, e),
            });
        }
    };
    match parse_text(&shown, text) {
        Ok(lines) => Ok(lines),
        Err(m) => Err(ReadError::Malformed(m)),
    }
}

const fn nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b.saturating_sub(b'0')),
        b'a'..=b'f' => Some(b.saturating_sub(b'a' - 10)),
        b'A'..=b'F' => Some(b.saturating_sub(b'A' - 10)),
        _ => None,
    }
}

const fn decode_tag_byte(hi_b: u8, lo_b: u8, at: usize, at1: usize) -> Result<u8, Error> {
    let Some(hi) = nibble(hi_b) else {
        return Err(Error::TagNotHex(at));
    };
    let Some(lo) = nibble(lo_b) else {
        return Err(Error::TagNotHex(at1));
    };
    Ok(((hi & 0x0f) << 4) | (lo & 0x0f))
}

/// Decode exactly [`TAG_BYTES`] bytes from 64 ASCII hex digits. Byte-indexed, so a
/// multibyte character is a non-hex byte like any other, never a panic.
const fn decode_tag(hex: &[u8]) -> Result<[u8; TAG_BYTES], Error> {
    if hex.len() != TAG_BYTES * 2 || !capfile_tag_hex_len_valid(hex.len()) {
        return Err(Error::TagLength(hex.len()));
    }
    let mut tag = [0u8; TAG_BYTES];
    let mut i = 0usize;
    while i < TAG_BYTES {
        let idx = i & 31;
        let at = idx * 2;
        let at1 = at + 1;
        if at1 >= hex.len() {
            return Err(Error::TagLength(hex.len()));
        }
        match decode_tag_byte(hex[at], hex[at1], at, at1) {
            Ok(b) => tag[idx] = b,
            Err(e) => return Err(e),
        }
        i += 1;
    }
    Ok(tag)
}
