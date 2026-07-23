// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Character set support (G0-G3, GL/GR, SI/SO, SS2/SS3).
//!
//! Extracted from `aterm-core::terminal::charset` to `aterm-types` (Part of #5663).
//! These are pure data types with no dependencies on aterm-core internals.

// CharacterSet (1 byte) and CharsetState (6 bytes) are trivially copyable
// but use &self for consistency with the rest of the terminal API.
#![allow(clippy::trivially_copy_pass_by_ref)]

// ============================================================================
// Character Set Designations
// ============================================================================

/// Character set designations.
///
/// Based on VT510 character set support. The most commonly used sets are:
/// - `Ascii`: Standard US ASCII (default for G0)
/// - `DecLineDrawing`: DEC Special Graphics (box drawing characters)
/// - `DecSupplemental`: DEC Supplemental (default for G1 on VT220+)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[repr(u8)]
pub enum CharacterSet {
    /// US ASCII (USASCII) - Final byte 'B'
    #[default]
    Ascii,
    /// DEC Special Graphic (line drawing) - Final byte '0'
    DecLineDrawing,
    /// DEC Supplemental Graphic - Final byte '%5' or '<'
    DecSupplemental,
    /// United Kingdom (UK) - Final byte 'A'
    UnitedKingdom,
    /// Dutch - Final byte '4'
    Dutch,
    /// Finnish - Final byte 'C' or '5'
    Finnish,
    /// French - Final byte 'R'
    French,
    /// French Canadian - Final byte 'Q'
    FrenchCanadian,
    /// German - Final byte 'K'
    German,
    /// Italian - Final byte 'Y'
    Italian,
    /// Norwegian/Danish - Final byte 'E', '6', or '`'
    NorwegianDanish,
    /// Spanish - Final byte 'Z'
    Spanish,
    /// Swedish - Final byte 'H' or '7'
    Swedish,
    /// Swiss - Final byte '='
    Swiss,
}

impl CharacterSet {
    /// Create a character set from a serialized index (session restoration).
    ///
    /// Returns `None` for unrecognized indices.
    #[must_use]
    pub fn from_u8(index: u8) -> Option<Self> {
        match index {
            0 => Some(Self::Ascii),
            1 => Some(Self::DecLineDrawing),
            2 => Some(Self::DecSupplemental),
            3 => Some(Self::UnitedKingdom),
            4 => Some(Self::Dutch),
            5 => Some(Self::Finnish),
            6 => Some(Self::French),
            7 => Some(Self::FrenchCanadian),
            8 => Some(Self::German),
            9 => Some(Self::Italian),
            10 => Some(Self::NorwegianDanish),
            11 => Some(Self::Spanish),
            12 => Some(Self::Swedish),
            13 => Some(Self::Swiss),
            _ => None,
        }
    }

    /// Create a character set from the SCS final byte.
    ///
    /// Returns `None` for unrecognized final bytes.
    pub fn from_final_byte(byte: u8) -> Option<Self> {
        match byte {
            b'B' | b'1' => Some(Self::Ascii), // '1' = Alternate ROM Standard (VT100)
            b'0' | b'2' => Some(Self::DecLineDrawing), // '2' = Alternate ROM Special Graphics (VT100)
            b'<' => Some(Self::DecSupplemental),
            b'A' => Some(Self::UnitedKingdom),
            b'4' => Some(Self::Dutch),
            b'C' | b'5' => Some(Self::Finnish),
            b'R' => Some(Self::French),
            b'Q' => Some(Self::FrenchCanadian),
            b'K' => Some(Self::German),
            b'Y' => Some(Self::Italian),
            b'E' | b'6' | b'`' => Some(Self::NorwegianDanish),
            b'Z' => Some(Self::Spanish),
            b'H' | b'7' => Some(Self::Swedish),
            b'=' => Some(Self::Swiss),
            _ => None,
        }
    }

    /// Translate a character using this character set.
    ///
    /// For most character sets, only certain characters in the 0x60-0x7E range
    /// are remapped. Characters outside this range pass through unchanged.
    #[inline]
    pub fn translate(&self, c: char) -> char {
        // Non-ASCII characters (U+0080+) are never remapped by any charset.
        // DEC line drawing maps 0x60-0x7E, UK maps 0x23 — all ASCII range.
        if (c as u32) >= 0x80 {
            return c;
        }
        match self {
            Self::Ascii => c,
            Self::DecLineDrawing => Self::translate_dec_line_drawing(c),
            Self::UnitedKingdom => {
                // UK: # (0x23) → £ (pound sign)
                if c == '#' { '£' } else { c }
            }
            Self::DecSupplemental => Self::translate_dec_supplemental(c),
            Self::Dutch => Self::translate_dutch(c),
            Self::Finnish => Self::translate_finnish(c),
            Self::French => Self::translate_french(c),
            Self::FrenchCanadian => Self::translate_french_canadian(c),
            Self::German => Self::translate_german(c),
            Self::Italian => Self::translate_italian(c),
            Self::NorwegianDanish => Self::translate_norwegian_danish(c),
            Self::Spanish => Self::translate_spanish(c),
            Self::Swedish => Self::translate_swedish(c),
            Self::Swiss => Self::translate_swiss(c),
        }
    }

    /// Translate a character using DEC Special Graphics (line drawing).
    ///
    /// Maps characters 0x60-0x7E to box drawing and other special characters.
    fn translate_dec_line_drawing(c: char) -> char {
        match c {
            '_' => ' ', // Blank (U+0020, per xterm; DEC spec says U+00A0)
            '`' => '◆', // Diamond
            'a' => '▒', // Checkerboard
            'b' => '␉', // HT symbol
            'c' => '␌', // FF symbol
            'd' => '␍', // CR symbol
            'e' => '␊', // LF symbol
            'f' => '°', // Degree symbol
            'g' => '±', // Plus/minus
            'h' => '␤', // NL symbol
            'i' => '␋', // VT symbol
            'j' => '┘', // Lower right corner
            'k' => '┐', // Upper right corner
            'l' => '┌', // Upper left corner
            'm' => '└', // Lower left corner
            'n' => '┼', // Crossing lines
            'o' => '⎺', // Scan line 1
            'p' => '⎻', // Scan line 3
            'q' => '─', // Horizontal line (scan line 5)
            'r' => '⎼', // Scan line 7
            's' => '⎽', // Scan line 9
            't' => '├', // Left T
            'u' => '┤', // Right T
            'v' => '┴', // Bottom T
            'w' => '┬', // Top T
            'x' => '│', // Vertical line
            'y' => '≤', // Less than or equal
            'z' => '≥', // Greater than or equal
            '{' => 'π', // Pi
            '|' => '≠', // Not equal
            '}' => '£', // Pound sign
            '~' => '·', // Centered dot (bullet)
            _ => c,
        }
    }

    /// Translate using DEC Supplemental (ISO Latin-1 supplement subset).
    ///
    /// DEC Supplemental maps a handful of positions in 0x20-0x7E differently
    /// from ASCII. In practice, most characters pass through unchanged.
    fn translate_dec_supplemental(c: char) -> char {
        // DEC Supplemental remaps very few ASCII-range positions.
        // The main distinction is that 0x24 maps to a general currency sign
        // and a few positions map to diacritical marks. Most apps rely on
        // UTF-8 and never activate this set, so a minimal mapping suffices.
        match c {
            '$' => '\u{00A4}', // Currency sign
            _ => c,
        }
    }

    /// Translate using Dutch NRCS (final byte '4').
    fn translate_dutch(c: char) -> char {
        match c {
            '#' => '£',
            '@' => '¾',
            '[' => 'ĳ',
            '\\' => '½',
            ']' => '|',
            '{' => '¨',
            '|' => 'ƒ',
            '}' => '¼',
            '~' => '´',
            _ => c,
        }
    }

    /// Translate using Finnish NRCS (final byte 'C' or '5').
    fn translate_finnish(c: char) -> char {
        match c {
            '[' => 'Ä',
            '\\' => 'Ö',
            ']' => 'Å',
            '^' => 'Ü',
            '`' => 'é',
            '{' => 'ä',
            '|' => 'ö',
            '}' => 'å',
            '~' => 'ü',
            _ => c,
        }
    }

    /// Translate using French NRCS (final byte 'R').
    fn translate_french(c: char) -> char {
        match c {
            '#' => '£',
            '@' => 'à',
            '[' => '°',
            '\\' => 'ç',
            ']' => '§',
            '{' => 'é',
            '|' => 'ù',
            '}' => 'è',
            '~' => '¨',
            _ => c,
        }
    }

    /// Translate using French Canadian NRCS (final byte 'Q').
    fn translate_french_canadian(c: char) -> char {
        match c {
            '@' => 'à',
            '[' => 'â',
            '\\' => 'ç',
            ']' => 'ê',
            '^' => 'î',
            '`' => 'ô',
            '{' => 'é',
            '|' => 'ù',
            '}' => 'è',
            '~' => 'û',
            _ => c,
        }
    }

    /// Translate using German NRCS (final byte 'K').
    fn translate_german(c: char) -> char {
        match c {
            '@' => '§',
            '[' => 'Ä',
            '\\' => 'Ö',
            ']' => 'Ü',
            '{' => 'ä',
            '|' => 'ö',
            '}' => 'ü',
            '~' => 'ß',
            _ => c,
        }
    }

    /// Translate using Italian NRCS (final byte 'Y').
    fn translate_italian(c: char) -> char {
        match c {
            '#' => '£',
            '@' => '§',
            '[' => '°',
            '\\' => 'ç',
            ']' => 'é',
            '`' => 'ù',
            '{' => 'à',
            '|' => 'ò',
            '}' => 'è',
            '~' => 'ì',
            _ => c,
        }
    }

    /// Translate using Norwegian/Danish NRCS (final byte 'E', '6', or '`').
    fn translate_norwegian_danish(c: char) -> char {
        match c {
            '@' => 'Ä',
            '[' => 'Æ',
            '\\' => 'Ø',
            ']' => 'Å',
            '^' => 'Ü',
            '`' => 'ä',
            '{' => 'æ',
            '|' => 'ø',
            '}' => 'å',
            '~' => 'ü',
            _ => c,
        }
    }

    /// Translate using Spanish NRCS (final byte 'Z').
    fn translate_spanish(c: char) -> char {
        match c {
            '#' => '£',
            '@' => '§',
            '[' => '¡',
            '\\' => 'Ñ',
            ']' => '¿',
            '{' => '°',
            '|' => 'ñ',
            '}' => 'ç',
            _ => c,
        }
    }

    /// Translate using Swedish NRCS (final byte 'H' or '7').
    fn translate_swedish(c: char) -> char {
        match c {
            '@' => 'É',
            '[' => 'Ä',
            '\\' => 'Ö',
            ']' => 'Å',
            '^' => 'Ü',
            '`' => 'é',
            '{' => 'ä',
            '|' => 'ö',
            '}' => 'å',
            '~' => 'ü',
            _ => c,
        }
    }

    /// Translate using Swiss NRCS (final byte '=').
    fn translate_swiss(c: char) -> char {
        match c {
            '#' => 'ù',
            '@' => 'à',
            '[' => 'é',
            '\\' => 'ç',
            ']' => 'ê',
            '^' => 'î',
            '_' => 'è',
            '`' => 'ô',
            '{' => 'ä',
            '|' => 'ö',
            '}' => 'ü',
            '~' => 'û',
            _ => c,
        }
    }
}

// ============================================================================
// GL Mapping and Single Shift
// ============================================================================

/// Which G-set is currently mapped to GL (left half, 0x20-0x7F).
///
/// All four G-sets are defined by the VT220 spec.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub enum GlMapping {
    /// G0 character set (default)
    #[default]
    G0,
    /// G1 character set
    G1,
    /// G2 character set (VT220 LS2)
    G2,
    /// G3 character set (VT220 LS3)
    G3,
}

impl From<u8> for GlMapping {
    /// Decode from serialized index — used by session restore.
    fn from(index: u8) -> Self {
        match index {
            0 => Self::G0,
            1 => Self::G1,
            2 => Self::G2,
            3 => Self::G3,
            _ => Self::G0,
        }
    }
}

impl From<GlMapping> for u8 {
    fn from(gl: GlMapping) -> Self {
        match gl {
            GlMapping::G0 => 0,
            GlMapping::G1 => 1,
            GlMapping::G2 => 2,
            GlMapping::G3 => 3,
        }
    }
}

/// Which G-set is currently mapped to GR (right half, 0xA0-0xFF).
///
/// Per VT220 spec, GR defaults to G2. Only G1-G3 can be invoked into GR.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub enum GrMapping {
    /// G1 character set (via LS1R, ESC ~)
    G1,
    /// G2 character set (default, via LS2R, ESC })
    #[default]
    G2,
    /// G3 character set (via LS3R, ESC |)
    G3,
}

impl From<u8> for GrMapping {
    fn from(index: u8) -> Self {
        match index {
            0 => Self::G1,
            1 => Self::G2,
            2 => Self::G3,
            _ => Self::G2,
        }
    }
}

impl From<GrMapping> for u8 {
    fn from(gr: GrMapping) -> Self {
        match gr {
            GrMapping::G1 => 0,
            GrMapping::G2 => 1,
            GrMapping::G3 => 2,
        }
    }
}

/// Single shift state for SS2/SS3.
///
/// When active, the next printable character uses the specified G-set
/// instead of the GL mapping, then the state clears automatically.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum SingleShift {
    /// No single shift active (default)
    #[default]
    None,
    /// SS2: Use G2 for next character
    Ss2,
    /// SS3: Use G3 for next character
    Ss3,
}

// ============================================================================
// 96-Character Set Designations
// ============================================================================

/// 96-character set designations (ISO 2022).
///
/// These sets map all 96 positions (0x20-0x7F) when invoked into GL/GR.
/// Designated via ESC - (G1), ESC . (G2), ESC / (G3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub enum CharacterSet96 {
    /// ISO 8859-1 Latin-1 Supplemental — final byte 'A'
    IsoLatin1Supplemental,
    /// ISO 8859-2 Latin-2 Supplemental — final byte 'B'
    IsoLatin2Supplemental,
    /// ISO 8859-9 Latin-5 Supplemental — final byte 'M'
    IsoLatin5Supplemental,
    /// ISO 8859-5 Latin/Cyrillic — final byte 'L'
    IsoLatinCyrillic,
    /// ISO 8859-7 Latin/Greek — final byte 'F'
    IsoLatinGreek,
}

impl CharacterSet96 {
    /// Create a 96-character set from the SCS final byte.
    ///
    /// Returns `None` for unrecognized final bytes.
    pub fn from_final_byte(byte: u8) -> Option<Self> {
        match byte {
            b'A' => Some(Self::IsoLatin1Supplemental),
            b'B' => Some(Self::IsoLatin2Supplemental),
            b'M' => Some(Self::IsoLatin5Supplemental),
            b'L' => Some(Self::IsoLatinCyrillic),
            b'F' => Some(Self::IsoLatinGreek),
            _ => None,
        }
    }

    /// Serialize to a byte for checkpoint storage (#7750).
    #[must_use]
    pub fn to_u8(self) -> u8 {
        match self {
            Self::IsoLatin1Supplemental => 1,
            Self::IsoLatin2Supplemental => 2,
            Self::IsoLatin5Supplemental => 3,
            Self::IsoLatinCyrillic => 4,
            Self::IsoLatinGreek => 5,
        }
    }

    /// Deserialize from a checkpoint byte (#7750).
    #[must_use]
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            1 => Some(Self::IsoLatin1Supplemental),
            2 => Some(Self::IsoLatin2Supplemental),
            3 => Some(Self::IsoLatin5Supplemental),
            4 => Some(Self::IsoLatinCyrillic),
            5 => Some(Self::IsoLatinGreek),
            _ => None,
        }
    }

    /// Translate a 96-character set offset (0-95) to a Unicode codepoint.
    /// Offset 0 = position 0xA0, offset 95 = position 0xFF.
    /// Returns U+FFFD REPLACEMENT CHARACTER for out-of-range offsets.
    pub fn translate(&self, offset: u8) -> char {
        if offset >= 96 {
            return '\u{FFFD}';
        }
        // Trust L0: `table.get` instead of `table[..]` — the native model
        // checker falsely refutes the bounds check even though the guard
        // above proves `offset < 96`. `get` is total (no panic path, no
        // bounds obligation); the `None` arm is unreachable and returns the
        // same replacement char as the out-of-range guard.
        let idx = usize::from(offset);
        let lookup = |table: &'static [char; 96]| match table.get(idx) {
            Some(c) => *c,
            None => '\u{FFFD}',
        };
        match self {
            Self::IsoLatin1Supplemental => char::from(offset.wrapping_add(0xA0)),
            Self::IsoLatin2Supplemental => lookup(&ISO_8859_2_TABLE),
            Self::IsoLatin5Supplemental => lookup(&ISO_8859_9_TABLE),
            Self::IsoLatinCyrillic => lookup(&ISO_8859_5_TABLE),
            Self::IsoLatinGreek => lookup(&ISO_8859_7_TABLE),
        }
    }
}

// ============================================================================
// ISO 8859 translation tables (96 entries each, positions 0xA0-0xFF) (#7695)
// ============================================================================

/// ISO 8859-2 (Latin-2): Central European. Positions 0xA0-0xFF → Unicode.
#[rustfmt::skip]
static ISO_8859_2_TABLE: [char; 96] = [
    '\u{00A0}', '\u{0104}', '\u{02D8}', '\u{0141}', '\u{00A4}', '\u{013D}', '\u{015A}', '\u{00A7}',
    '\u{00A8}', '\u{0160}', '\u{015E}', '\u{0164}', '\u{0179}', '\u{00AD}', '\u{017D}', '\u{017B}',
    '\u{00B0}', '\u{0105}', '\u{02DB}', '\u{0142}', '\u{00B4}', '\u{013E}', '\u{015B}', '\u{02C7}',
    '\u{00B8}', '\u{0161}', '\u{015F}', '\u{0165}', '\u{017A}', '\u{02DD}', '\u{017E}', '\u{017C}',
    '\u{0154}', '\u{00C1}', '\u{00C2}', '\u{0102}', '\u{00C4}', '\u{0139}', '\u{0106}', '\u{00C7}',
    '\u{010C}', '\u{00C9}', '\u{0118}', '\u{00CB}', '\u{011A}', '\u{00CD}', '\u{00CE}', '\u{010E}',
    '\u{0110}', '\u{0143}', '\u{0147}', '\u{00D3}', '\u{00D4}', '\u{0150}', '\u{00D6}', '\u{00D7}',
    '\u{0158}', '\u{016E}', '\u{00DA}', '\u{0170}', '\u{00DC}', '\u{00DD}', '\u{0162}', '\u{00DF}',
    '\u{0155}', '\u{00E1}', '\u{00E2}', '\u{0103}', '\u{00E4}', '\u{013A}', '\u{0107}', '\u{00E7}',
    '\u{010D}', '\u{00E9}', '\u{0119}', '\u{00EB}', '\u{011B}', '\u{00ED}', '\u{00EE}', '\u{010F}',
    '\u{0111}', '\u{0144}', '\u{0148}', '\u{00F3}', '\u{00F4}', '\u{0151}', '\u{00F6}', '\u{00F7}',
    '\u{0159}', '\u{016F}', '\u{00FA}', '\u{0171}', '\u{00FC}', '\u{00FD}', '\u{0163}', '\u{02D9}',
];

/// ISO 8859-5 (Latin/Cyrillic). Positions 0xA0-0xFF → Unicode.
#[rustfmt::skip]
static ISO_8859_5_TABLE: [char; 96] = [
    '\u{00A0}', '\u{0401}', '\u{0402}', '\u{0403}', '\u{0404}', '\u{0405}', '\u{0406}', '\u{0407}',
    '\u{0408}', '\u{0409}', '\u{040A}', '\u{040B}', '\u{040C}', '\u{00AD}', '\u{040E}', '\u{040F}',
    '\u{0410}', '\u{0411}', '\u{0412}', '\u{0413}', '\u{0414}', '\u{0415}', '\u{0416}', '\u{0417}',
    '\u{0418}', '\u{0419}', '\u{041A}', '\u{041B}', '\u{041C}', '\u{041D}', '\u{041E}', '\u{041F}',
    '\u{0420}', '\u{0421}', '\u{0422}', '\u{0423}', '\u{0424}', '\u{0425}', '\u{0426}', '\u{0427}',
    '\u{0428}', '\u{0429}', '\u{042A}', '\u{042B}', '\u{042C}', '\u{042D}', '\u{042E}', '\u{042F}',
    '\u{0430}', '\u{0431}', '\u{0432}', '\u{0433}', '\u{0434}', '\u{0435}', '\u{0436}', '\u{0437}',
    '\u{0438}', '\u{0439}', '\u{043A}', '\u{043B}', '\u{043C}', '\u{043D}', '\u{043E}', '\u{043F}',
    '\u{0440}', '\u{0441}', '\u{0442}', '\u{0443}', '\u{0444}', '\u{0445}', '\u{0446}', '\u{0447}',
    '\u{0448}', '\u{0449}', '\u{044A}', '\u{044B}', '\u{044C}', '\u{044D}', '\u{044E}', '\u{044F}',
    '\u{2116}', '\u{0451}', '\u{0452}', '\u{0453}', '\u{0454}', '\u{0455}', '\u{0456}', '\u{0457}',
    '\u{0458}', '\u{0459}', '\u{045A}', '\u{045B}', '\u{045C}', '\u{00A7}', '\u{045E}', '\u{045F}',
];

/// ISO 8859-7 (Latin/Greek). Positions 0xA0-0xFF → Unicode.
#[rustfmt::skip]
static ISO_8859_7_TABLE: [char; 96] = [
    '\u{00A0}', '\u{2018}', '\u{2019}', '\u{00A3}', '\u{20AC}', '\u{20AF}', '\u{00A6}', '\u{00A7}',
    '\u{00A8}', '\u{00A9}', '\u{037A}', '\u{00AB}', '\u{00AC}', '\u{00AD}', '\u{FFFD}', '\u{2015}',
    '\u{00B0}', '\u{00B1}', '\u{00B2}', '\u{00B3}', '\u{0384}', '\u{0385}', '\u{0386}', '\u{00B7}',
    '\u{0388}', '\u{0389}', '\u{038A}', '\u{00BB}', '\u{038C}', '\u{00BD}', '\u{038E}', '\u{038F}',
    '\u{0390}', '\u{0391}', '\u{0392}', '\u{0393}', '\u{0394}', '\u{0395}', '\u{0396}', '\u{0397}',
    '\u{0398}', '\u{0399}', '\u{039A}', '\u{039B}', '\u{039C}', '\u{039D}', '\u{039E}', '\u{039F}',
    '\u{03A0}', '\u{03A1}', '\u{FFFD}', '\u{03A3}', '\u{03A4}', '\u{03A5}', '\u{03A6}', '\u{03A7}',
    '\u{03A8}', '\u{03A9}', '\u{03AA}', '\u{03AB}', '\u{03AC}', '\u{03AD}', '\u{03AE}', '\u{03AF}',
    '\u{03B0}', '\u{03B1}', '\u{03B2}', '\u{03B3}', '\u{03B4}', '\u{03B5}', '\u{03B6}', '\u{03B7}',
    '\u{03B8}', '\u{03B9}', '\u{03BA}', '\u{03BB}', '\u{03BC}', '\u{03BD}', '\u{03BE}', '\u{03BF}',
    '\u{03C0}', '\u{03C1}', '\u{03C2}', '\u{03C3}', '\u{03C4}', '\u{03C5}', '\u{03C6}', '\u{03C7}',
    '\u{03C8}', '\u{03C9}', '\u{03CA}', '\u{03CB}', '\u{03CC}', '\u{03CD}', '\u{03CE}', '\u{FFFD}',
];

/// ISO 8859-9 (Latin-5/Turkish). Identical to 8859-1 except 6 positions.
#[rustfmt::skip]
static ISO_8859_9_TABLE: [char; 96] = [
    '\u{00A0}', '\u{00A1}', '\u{00A2}', '\u{00A3}', '\u{00A4}', '\u{00A5}', '\u{00A6}', '\u{00A7}',
    '\u{00A8}', '\u{00A9}', '\u{00AA}', '\u{00AB}', '\u{00AC}', '\u{00AD}', '\u{00AE}', '\u{00AF}',
    '\u{00B0}', '\u{00B1}', '\u{00B2}', '\u{00B3}', '\u{00B4}', '\u{00B5}', '\u{00B6}', '\u{00B7}',
    '\u{00B8}', '\u{00B9}', '\u{00BA}', '\u{00BB}', '\u{00BC}', '\u{00BD}', '\u{00BE}', '\u{00BF}',
    '\u{00C0}', '\u{00C1}', '\u{00C2}', '\u{00C3}', '\u{00C4}', '\u{00C5}', '\u{00C6}', '\u{00C7}',
    '\u{00C8}', '\u{00C9}', '\u{00CA}', '\u{00CB}', '\u{00CC}', '\u{00CD}', '\u{00CE}', '\u{00CF}',
    '\u{011E}', '\u{00D1}', '\u{00D2}', '\u{00D3}', '\u{00D4}', '\u{00D5}', '\u{00D6}', '\u{00D7}',
    '\u{00D8}', '\u{00D9}', '\u{00DA}', '\u{00DB}', '\u{00DC}', '\u{0130}', '\u{015E}', '\u{00DF}',
    '\u{00E0}', '\u{00E1}', '\u{00E2}', '\u{00E3}', '\u{00E4}', '\u{00E5}', '\u{00E6}', '\u{00E7}',
    '\u{00E8}', '\u{00E9}', '\u{00EA}', '\u{00EB}', '\u{00EC}', '\u{00ED}', '\u{00EE}', '\u{00EF}',
    '\u{011F}', '\u{00F1}', '\u{00F2}', '\u{00F3}', '\u{00F4}', '\u{00F5}', '\u{00F6}', '\u{00F7}',
    '\u{00F8}', '\u{00F9}', '\u{00FA}', '\u{00FB}', '\u{00FC}', '\u{0131}', '\u{015F}', '\u{00FF}',
];

// ============================================================================
// Character Set State
// ============================================================================

/// Complete character set state.
///
/// Tracks:
/// - G0-G3 character set designations
/// - Which G-set is mapped to GL (via SI/SO)
/// - Any pending single shift (SS2/SS3)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct CharacterSetState {
    /// G0 character set designation
    pub g0: CharacterSet,
    /// G1 character set designation
    pub g1: CharacterSet,
    /// G2 character set designation
    pub g2: CharacterSet,
    /// G3 character set designation
    pub g3: CharacterSet,
    /// Which G-set is mapped to GL
    pub gl: GlMapping,
    /// Pending single shift
    pub single_shift: SingleShift,
    /// Which G-set is mapped to GR (right half, 0xA0-0xFF) (#7546)
    pub gr: GrMapping,
    /// G1 96-character set override (#7547)
    g1_96: Option<CharacterSet96>,
    /// G2 96-character set override (#7547)
    g2_96: Option<CharacterSet96>,
    /// G3 96-character set override (#7547)
    g3_96: Option<CharacterSet96>,
}

impl Default for CharacterSetState {
    fn default() -> Self {
        Self {
            g0: CharacterSet::Ascii,
            // xterm resetCharsets() does initCharset(screen, 1, nrc_ASCII):
            // G1 defaults to ASCII at VT100+ level (DEC Special Graphics is
            // the G1 default only in VT52 graphics mode).
            g1: CharacterSet::Ascii,
            g2: CharacterSet::Ascii,
            g3: CharacterSet::Ascii,
            gl: GlMapping::G0,
            single_shift: SingleShift::None,
            gr: GrMapping::G2,
            g1_96: None,
            g2_96: None,
            g3_96: None,
        }
    }
}

impl CharacterSetState {
    /// Create a new character set state with defaults.
    pub fn new() -> Self {
        Self::default()
    }

    /// Construct from 94-character set designations only (no 96-char overrides).
    ///
    /// Provided for external callers that cannot use struct literals due to
    /// private fields (`g1_96`, `g2_96`, `g3_96`).
    #[must_use]
    pub fn from_94(
        g0: CharacterSet,
        g1: CharacterSet,
        g2: CharacterSet,
        g3: CharacterSet,
        gl: GlMapping,
        single_shift: SingleShift,
    ) -> Self {
        Self {
            g0,
            g1,
            g2,
            g3,
            gl,
            single_shift,
            gr: GrMapping::G2,
            g1_96: None,
            g2_96: None,
            g3_96: None,
        }
    }

    /// Construct with all fields including GR mapping and 96-char sets (#7750).
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn from_full(
        g0: CharacterSet,
        g1: CharacterSet,
        g2: CharacterSet,
        g3: CharacterSet,
        gl: GlMapping,
        single_shift: SingleShift,
        gr: GrMapping,
        g1_96: Option<CharacterSet96>,
        g2_96: Option<CharacterSet96>,
        g3_96: Option<CharacterSet96>,
    ) -> Self {
        Self {
            g0,
            g1,
            g2,
            g3,
            gl,
            single_shift,
            gr,
            g1_96,
            g2_96,
            g3_96,
        }
    }

    /// Get the GR mapping (#7750).
    #[must_use]
    pub fn gr(&self) -> GrMapping {
        self.gr
    }

    /// Get the G1 96-character set override (#7750).
    #[must_use]
    pub fn g1_96(&self) -> Option<CharacterSet96> {
        self.g1_96
    }

    /// Get the G2 96-character set override (#7750).
    #[must_use]
    pub fn g2_96(&self) -> Option<CharacterSet96> {
        self.g2_96
    }

    /// Get the G3 96-character set override (#7750).
    #[must_use]
    pub fn g3_96(&self) -> Option<CharacterSet96> {
        self.g3_96
    }

    /// Get the effective character set for translation.
    ///
    /// If a single shift is active, returns that G-set and clears the shift.
    /// Otherwise returns the GL-mapped G-set.
    #[must_use]
    pub fn effective_charset(&self) -> CharacterSet {
        match self.single_shift {
            SingleShift::Ss2 => self.g2,
            SingleShift::Ss3 => self.g3,
            SingleShift::None => match self.gl {
                GlMapping::G0 => self.g0,
                GlMapping::G1 => self.g1,
                GlMapping::G2 => self.g2,
                GlMapping::G3 => self.g3,
            },
        }
    }

    /// Clear single shift after a character is printed.
    pub fn clear_single_shift(&mut self) {
        self.single_shift = SingleShift::None;
    }

    /// Check if charset is ASCII passthrough (no translation needed).
    ///
    /// Returns true when:
    /// - GL maps to G0
    /// - G0 is ASCII (or equivalent - passes ASCII unchanged)
    /// - No single shift pending
    ///
    /// This allows skipping charset translation for ASCII bulk writes.
    #[must_use]
    #[inline]
    pub fn is_ascii_passthrough(&self) -> bool {
        self.single_shift == SingleShift::None
            && self.gl == GlMapping::G0
            && self.g0 == CharacterSet::Ascii
    }

    /// Check if GR-range characters (U+00A0-U+00FF) pass through unchanged.
    ///
    /// Returns true when the GR-mapped G-set has no 96-character set
    /// designation and its 94-character set is ASCII (which passes GR
    /// characters unchanged). When false, characters in U+00A0-U+00FF
    /// must go through `translate()` for correct GR mapping.
    #[must_use]
    #[inline]
    pub fn gr_is_passthrough(&self) -> bool {
        let (cs94, cs96) = match self.gr {
            GrMapping::G1 => (self.g1, self.g1_96),
            GrMapping::G2 => (self.g2, self.g2_96),
            GrMapping::G3 => (self.g3, self.g3_96),
        };
        cs96.is_none() && cs94 == CharacterSet::Ascii
    }

    /// Get the effective 96-character set override, if any.
    ///
    /// Returns `Some(cs96)` when the active GL G-set (or single-shifted G-set)
    /// has a 96-character set designated.
    #[must_use]
    fn effective_96(&self) -> Option<CharacterSet96> {
        match self.single_shift {
            SingleShift::Ss2 => self.g2_96,
            SingleShift::Ss3 => self.g3_96,
            SingleShift::None => match self.gl {
                GlMapping::G0 => None, // G0 cannot hold 96-char sets
                GlMapping::G1 => self.g1_96,
                GlMapping::G2 => self.g2_96,
                GlMapping::G3 => self.g3_96,
            },
        }
    }

    /// Translate a character using the effective character set.
    ///
    /// This also clears any single shift state.
    #[inline]
    pub fn translate(&mut self, c: char) -> char {
        let cp = c as u32;

        // GR range (U+00A0-U+00FF): route through GR-mapped G-set (#7546).
        // Check 96-character set designation first (#7547), then fall back
        // to the 94-character set.
        if (0xA0..=0xFF).contains(&cp) {
            self.clear_single_shift();
            // Check if the GR-mapped G-set has a 96-char designation.
            let cs96 = match self.gr {
                GrMapping::G1 => self.g1_96,
                GrMapping::G2 => self.g2_96,
                GrMapping::G3 => self.g3_96,
            };
            if let Some(cs96) = cs96 {
                // 96-char sets map positions 0xA0-0xFF as offsets 0-95. Mask to
                // a byte before the cast so the Trust gate sees the narrowing is
                // lossless, and use `wrapping_sub` so it sees the subtraction
                // cannot underflow (`cp` is 0xA0..=0xFF here — the enclosing
                // `contains` check — so neither the `- 0xA0` nor the `& 0xFF`
                // ever changes the value).
                let offset = (cp.wrapping_sub(0xA0) & 0xFF) as u8;
                return cs96.translate(offset);
            }
            // Same discharge as above: `cp >= 0xA0 > 0x80`, so `wrapping_sub`
            // never wraps and the mask is identity.
            let masked = (cp.wrapping_sub(0x80) & 0xFF) as u8;
            let charset = match self.gr {
                GrMapping::G1 => self.g1,
                GrMapping::G2 => self.g2,
                GrMapping::G3 => self.g3,
            };
            if charset == CharacterSet::Ascii {
                return c;
            }
            return charset.translate(masked as char);
        }

        // Non-ASCII characters above 0xFF are never remapped — skip charset
        // lookup entirely. Single shift must still be consumed.
        if cp >= 0x80 {
            self.clear_single_shift();
            return c;
        }

        // 96-character set check: if the active GL G-set has a 96-char override,
        // use it for the full 0x20-0x7F range (#7547).
        if let Some(cs96) = self.effective_96() {
            self.clear_single_shift();
            if (0x20..=0x7F).contains(&cp) {
                let offset = (cp - 0x20) as u8;
                return cs96.translate(offset);
            }
            return c;
        }

        let charset = self.effective_charset();
        self.clear_single_shift();
        charset.translate(c)
    }

    /// Designate a character set to a G-set.
    ///
    /// This also clears any 96-character set override on the same G-set,
    /// since 94-char and 96-char designations are mutually exclusive.
    pub fn designate(&mut self, g_set: u8, charset: CharacterSet) {
        match g_set {
            0 => self.g0 = charset,
            1 => {
                self.g1 = charset;
                self.g1_96 = None;
            }
            2 => {
                self.g2 = charset;
                self.g2_96 = None;
            }
            3 => {
                self.g3 = charset;
                self.g3_96 = None;
            }
            _ => {}
        }
    }

    /// Designate a 96-character set to a G-set (#7547).
    ///
    /// G0 cannot hold 96-character sets per ISO 2022.
    pub fn designate_96(&mut self, g_set: u8, charset: CharacterSet96) {
        match g_set {
            0 => {} // G0 cannot hold 96-char sets
            1 => self.g1_96 = Some(charset),
            2 => self.g2_96 = Some(charset),
            3 => self.g3_96 = Some(charset),
            _ => {}
        }
    }

    /// Reset to default state.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}
