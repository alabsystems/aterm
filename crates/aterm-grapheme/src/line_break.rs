// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! UAX #14 line-breaking and word-boundary rules for chrome text wrapping.

use crate::GraphemeClusters as _;

/// What kind of place a line may break at ([`line_breaks`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Kind of line-break opportunity ([`line_breaks`]).
pub enum BreakKind {
    /// Break after a run of whitespace.
    Space,
    /// Break between ideographic/kana/Hangul characters.
    Ideographic,
    /// Break at a prose hyphen/slash, path slash, or URL delimiter.
    Joint,
}

impl BreakKind {
    /// Whether this break keeps whole words: a space, or a character of a
    /// script whose every character is a word's worth (a cut there reads
    /// as words, never as a word cut short).
    #[must_use]
    #[inline]
    /// Whether this break preserves whole words (`Space` or `Ideographic`).
    pub const fn is_word_boundary(self) -> bool {
        matches!(self, Self::Space | Self::Ideographic)
    }
}

/// A place a line may break: the next line would start at byte `at` of the
/// text (never 0 and never the text's end), after a break of `kind`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Line-break opportunity at byte offset `at` (never `0` or `text.len()`).
pub struct LineBreak {
    pub at: usize,
    pub kind: BreakKind,
}

/// How a grapheme takes part in line breaking, by its first character.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Class {
    /// White space a line breaks after.
    Space,
    /// A no-break space: glue.
    Glue,
    /// A character of a script written without spaces (UAX #14 `ID`).
    Ideographic,
    /// CJK closing punctuation and the characters that may not start a line
    /// (UAX #14 `CL`, `CP`, `NS`, `EX` and their fullwidth `IS`): no break
    /// before it.
    Closing,
    /// CJK opening punctuation (UAX #14 `OP`): no break after it.
    Opening,
    /// A hyphen or a slash: a word of prose may break after it, a path
    /// before it.
    Joint,
    /// Anything else: a letter, a digit, Latin punctuation.
    Other,
}

/// The kinsoku table's closing side: may not start a line.
#[inline]
const fn is_closing(c: char) -> bool {
    matches!(
        c,
        '\u{3001}'
            | '\u{3002}'
            | '\u{3009}'
            | '\u{300B}'
            | '\u{300D}'
            | '\u{300F}'
            | '\u{3011}'
            | '\u{3015}'
            | '\u{3017}'
            | '\u{3019}'
            | '\u{301B}'
            | '\u{301C}'
            | '\u{301E}'
            | '\u{301F}'
            | '\u{3005}'
            | '\u{303B}'
            | '\u{309B}'..='\u{309E}'
            | '\u{30A0}'
            | '\u{30FB}'
            | '\u{30FC}'
            | '\u{30FD}'..='\u{30FE}'
            | '\u{3041}' | '\u{3043}' | '\u{3045}' | '\u{3047}' | '\u{3049}'
            | '\u{3063}' | '\u{3083}' | '\u{3085}' | '\u{3087}' | '\u{308E}'
            | '\u{3095}' | '\u{3096}'
            | '\u{30A1}' | '\u{30A3}' | '\u{30A5}' | '\u{30A7}' | '\u{30A9}'
            | '\u{30C3}' | '\u{30E3}' | '\u{30E5}' | '\u{30E7}' | '\u{30EE}'
            | '\u{30F5}' | '\u{30F6}'
            | '\u{31F0}'..='\u{31FF}'
            | '\u{FE10}'..='\u{FE16}'
            | '\u{FE18}'
            | '\u{FE36}' | '\u{FE38}' | '\u{FE3A}' | '\u{FE3C}' | '\u{FE3E}'
            | '\u{FE40}' | '\u{FE42}' | '\u{FE44}' | '\u{FE48}'
            | '\u{FE50}'..='\u{FE57}'
            | '\u{FE5A}' | '\u{FE5C}' | '\u{FE5E}'
            | '\u{FF01}'
            | '\u{FF09}'
            | '\u{FF0C}'
            | '\u{FF0E}'
            | '\u{FF1A}'
            | '\u{FF1B}'
            | '\u{FF1F}'
            | '\u{FF3D}'
            | '\u{FF5D}'
            | '\u{FF60}'
            | '\u{FF61}'
            | '\u{FF63}'
            | '\u{FF64}'
            | '\u{FF65}'
            | '\u{FF67}'..='\u{FF70}'
            | '\u{FF9E}'..='\u{FF9F}'
            | ')' | ']' | '}' | ',' | '.' | ':' | ';' | '!' | '?' | '%'
            | '\u{2019}'
            | '\u{201D}'
            | '\u{00BB}'
            | '\u{2025}'
            | '\u{2026}'
            | '\u{203C}'
            | '\u{2047}'..='\u{2049}'
    )
}

/// The kinsoku table's opening side: may not end a line.
#[inline]
const fn is_opening(c: char) -> bool {
    matches!(
        c,
        '\u{3008}'
            | '\u{300A}'
            | '\u{300C}'
            | '\u{300E}'
            | '\u{3010}'
            | '\u{3014}'
            | '\u{3016}'
            | '\u{3018}'
            | '\u{301A}'
            | '\u{301D}'
            | '\u{FE17}'
            | '\u{FE35}'
            | '\u{FE37}'
            | '\u{FE39}'
            | '\u{FE3B}'
            | '\u{FE3D}'
            | '\u{FE3F}'
            | '\u{FE41}'
            | '\u{FE43}'
            | '\u{FE47}'
            | '\u{FE59}'
            | '\u{FE5B}'
            | '\u{FE5D}'
            | '\u{FF08}'
            | '\u{FF3B}'
            | '\u{FF5B}'
            | '\u{FF5F}'
            | '\u{FF62}'
            | '('
            | '['
            | '{'
            | '\u{2018}'
            | '\u{201C}'
            | '\u{00AB}'
    )
}

/// A character of a script written without spaces between its words, or
/// CJK punctuation and symbols (UAX #14 `ID`, and the fullwidth forms).
#[inline]
const fn is_ideographic(c: char) -> bool {
    matches!(
        c,
        '\u{1100}'..='\u{11FF}'
            | '\u{2E80}'..='\u{2FFF}'
            | '\u{3001}'..='\u{303F}'
            | '\u{3040}'..='\u{30FF}'
            | '\u{3100}'..='\u{31FF}'
            | '\u{3200}'..='\u{33FF}'
            | '\u{3400}'..='\u{4DBF}'
            | '\u{4E00}'..='\u{9FFF}'
            | '\u{A000}'..='\u{A4CF}'
            | '\u{A960}'..='\u{A97F}'
            | '\u{AC00}'..='\u{D7FF}'
            | '\u{F900}'..='\u{FAFF}'
            | '\u{FE10}'..='\u{FE1F}'
            | '\u{FE30}'..='\u{FE4F}'
            | '\u{FF01}'..='\u{FF60}'
            | '\u{FF61}'..='\u{FFDC}'
            | '\u{FFE0}'..='\u{FFE6}'
            | '\u{1B000}'..='\u{1B16F}'
            | '\u{20000}'..='\u{3FFFD}'
    )
}

/// White space that is glue: a no-break space keeps its neighbours on one
/// line.
#[inline]
const fn is_glue(c: char) -> bool {
    matches!(c, '\u{00A0}' | '\u{2007}' | '\u{202F}')
}

#[inline]
fn class_of(c: char) -> Class {
    if is_glue(c) {
        Class::Glue
    } else if c.is_whitespace() {
        Class::Space
    } else if is_closing(c) {
        Class::Closing
    } else if is_opening(c) {
        Class::Opening
    } else if is_ideographic(c) {
        Class::Ideographic
    } else if matches!(c, '/' | '-') {
        Class::Joint
    } else {
        Class::Other
    }
}

/// WHETHER `token` — a run of text between spaces — NEVER STARTS A LINE,
/// even after a space: it is nothing but closing punctuation or slashes
/// (UAX #14 LB13: `]`, `)`, `!`, `?`, `;`, `:`, `,`, `.`, `/`), which goes
/// with the word before it ("GPU /" / "RAM /", never "GPU" / "/ RAM /").
#[must_use]
#[inline]
/// Whether `token` consists solely of closing/no-start characters and never starts a line.
pub fn never_starts_a_line(token: &str) -> bool {
    !token.is_empty()
        && token
            .chars()
            .all(|c| matches!(c, ')' | ']' | '}' | '!' | '?' | ';' | ':' | ',' | '.' | '/'))
}

/// Whether `c` is CJK: an ideograph, kana, Hangul or the CJK and fullwidth
/// punctuation that goes with them.
#[inline]
const fn is_cjk(c: char) -> bool {
    is_ideographic(c)
}

/// HOW A RUN OF TEXT BETWEEN SPACES READS ([`token_kind`]): what decides
/// where it may break inside itself, and whether a hyphen may be put in it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Classification of a whitespace-delimited token ([`token_kind`]).
pub enum TokenKind {
    /// Prose word (single-case or capitalized letters, optionally joined by `-`, `/`, or `'`).
    Word,
    /// Identifier, path, URL, version, flag, or mixed-case token.
    Identifier,
}

/// Punctuation a word of prose may open with ([`token_kind`]).
#[inline]
const fn opens_a_word(c: char) -> bool {
    matches!(
        c,
        '(' | '['
            | '{'
            | '"'
            | '\''
            | '\u{201C}'
            | '\u{2018}'
            | '\u{AB}'
            | '\u{2039}'
            | '\u{BF}'
            | '\u{A1}'
    )
}

/// Punctuation a word of prose may close with ([`token_kind`]).
#[inline]
const fn closes_a_word(c: char) -> bool {
    matches!(
        c,
        '.' | ','
            | ';'
            | ':'
            | '!'
            | '?'
            | ')'
            | ']'
            | '}'
            | '"'
            | '\''
            | '\u{201D}'
            | '\u{2019}'
            | '\u{BB}'
            | '\u{203A}'
            | '\u{2026}'
            | '\u{B7}'
    )
}

fn cased_as_prose(run: &str) -> bool {
    let mut chars = run.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    let (mut upper, mut lower) = (false, false);
    for c in chars {
        upper |= c.is_uppercase();
        lower |= c.is_lowercase();
    }
    !upper || (!first.is_lowercase() && !lower)
}

/// HOW `token` — a run of text with no breaking space in it — READS
/// ([`TokenKind`]): a word of prose, or an identifier.
#[must_use]
/// Classify a non-whitespace `token` as [`TokenKind::Word`] or [`TokenKind::Identifier`].
pub fn token_kind(token: &str) -> TokenKind {
    let core = token
        .trim_start_matches(opens_a_word)
        .trim_end_matches(closes_a_word);
    if core.is_empty() {
        return TokenKind::Word;
    }
    if core.matches('/').count() > 1 {
        return TokenKind::Identifier;
    }
    let joiner = |c: char| matches!(c, '-' | '/' | '\'' | '\u{2019}');
    let prose = core
        .split(joiner)
        .all(|run| !run.is_empty() && run.chars().all(char::is_alphabetic) && cased_as_prose(run));
    if prose {
        TokenKind::Word
    } else {
        TokenKind::Identifier
    }
}

/// A grapheme as line breaking reads it: where it starts, its first
/// character and that character's class.
#[derive(Clone, Copy)]
struct Cluster {
    at: usize,
    first: char,
    class: Class,
}

/// A run of text between breaking white space, as [`line_breaks`] reads it.
#[derive(Clone, Copy)]
struct Token {
    kind: TokenKind,
    start: usize,
    end: usize,
}

/// `text`'s graphemes ([`Cluster`]).
fn clusters(text: &str) -> (Vec<Cluster>, Vec<Option<Token>>) {
    let clusters: Vec<Cluster> = text
        .grapheme_indices()
        .filter_map(|(at, grapheme)| {
            let first = grapheme.chars().next()?;
            Some(Cluster {
                at,
                first,
                class: class_of(first),
            })
        })
        .collect();
    let mut tokens = vec![None; clusters.len()];
    let mut index = 0;
    while index < clusters.len() {
        if clusters[index].class == Class::Space {
            index += 1;
            continue;
        }
        let start = index;
        while index < clusters.len() && clusters[index].class != Class::Space {
            index += 1;
        }
        let from = clusters[start].at;
        let to = clusters.get(index).map_or(text.len(), |cluster| cluster.at);
        let kind = token_kind(&text[from..to]);
        for slot in &mut tokens[start..index] {
            *slot = Some(Token {
                kind,
                start,
                end: index,
            });
        }
    }
    (clusters, tokens)
}

/// Whether a path may break before a `/` (or a Windows path's `\`) that
/// follows `c`: after a folder's name, never after the `~`, `.` or `:` that
/// leads a path, nor inside a run of slashes.
#[inline]
fn ends_a_folder(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | ')' | ']' | '}' | '\'' | '"' | '%' | '*')
}

/// Whether `c` separates a path's folders.
#[inline]
const fn is_path_separator(c: char) -> bool {
    matches!(c, '/' | '\\')
}

/// The Joint break before cluster `index` of `token` ([`line_breaks`]).
fn joint_before(clusters: &[Cluster], index: usize, token: Token) -> bool {
    let Token { kind, start, end } = token;
    let cur = clusters[index].first;
    let prev = clusters[index - 1].first;
    match kind {
        TokenKind::Word => {
            matches!(prev, '-' | '/')
                && index >= start + 2
                && clusters[index - 2].first.is_alphabetic()
                && cur.is_alphabetic()
        }
        TokenKind::Identifier => {
            let before_separator = is_path_separator(cur)
                && index > start
                && ends_a_folder(prev)
                && clusters
                    .get(index + 1)
                    .filter(|_| index + 1 < end)
                    .is_some_and(|next| !is_path_separator(next.first));
            let after_scheme = prev == '/'
                && index >= start + 3
                && clusters[index - 2].first == '/'
                && clusters[index - 3].first == ':'
                && cur.is_alphanumeric();
            before_separator || after_scheme
        }
    }
}

/// EVERY PLACE `text` MAY BREAK, in order ([`LineBreak`]): after each run
/// of white space, between CJK characters and beside them (kinsoku kept),
/// inside a word of prose after a hyphen or a slash, and inside a path
/// before a slash ([`BreakKind::Joint`]).
#[must_use]
/// Return all [`LineBreak`] opportunities in `text` in ascending byte order.
pub fn line_breaks(text: &str) -> Vec<LineBreak> {
    let (clusters, tokens) = clusters(text);
    let mut breaks = Vec::new();
    let mut seen_text = false;
    for (index, cluster) in clusters.iter().enumerate() {
        let Cluster { at, first, class } = *cluster;
        let Some(prev) = index.checked_sub(1).map(|before| clusters[before]) else {
            seen_text = class != Class::Space;
            continue;
        };
        if class == Class::Space {
            continue;
        }
        if prev.class == Class::Space {
            let closer_after_space = (class == Class::Closing && is_cjk(first))
                || tokens[index].is_some_and(|token| {
                    let to = clusters.get(token.end).map_or(text.len(), |next| next.at);
                    never_starts_a_line(&text[at..to])
                });
            if seen_text && !closer_after_space {
                breaks.push(LineBreak {
                    at,
                    kind: BreakKind::Space,
                });
            }
            seen_text = true;
            continue;
        }
        seen_text = true;
        let kind = if prev.class == Class::Glue || class == Class::Glue {
            None
        } else if (is_cjk(prev.first) || is_cjk(first))
            && prev.class != Class::Opening
            && class != Class::Closing
        {
            Some(BreakKind::Ideographic)
        } else if class != Class::Ideographic
            && let Some(token) = tokens[index]
            && joint_before(&clusters, index, token)
        {
            Some(BreakKind::Joint)
        } else {
            None
        };
        if let Some(kind) = kind {
            breaks.push(LineBreak { at, kind });
        }
    }
    breaks
}

/// Where an identifier wider than a whole line may break inside itself ([`break_inside`]), in
/// the tiers a typesetter tries in turn.
struct IdentifierJoints {
    /// Before a separator (`/`, `.`, `_`, `@`, …), after a hyphen (the hyphen ends its line,
    /// never starts the next), after a `:` between letters.
    joints: Vec<usize>,
    /// Before the `.` of a file's extension, which stays with its word while any other
    /// joint will do ("sunset-over-the-" / "bay.png", never "sunset-over-the-bay" / ".png").
    extension: Vec<usize>,
    /// Before a hyphen that ends a word of letters, only where the word and its hyphen are
    /// wider than the line and the word has no syllable that fits ("/aterm" / "-lab", never
    /// the word cut in two: "/ate" / "rm-"). A run with digits in it (a hash, a build id) is
    /// cut inside instead: no word is cut there.
    before_hyphen: Vec<usize>,
    /// Between a number's digit groups ("192.168.1" / ".114").
    in_numbers: Vec<usize>,
}

/// Whether `rest`, what follows a `.`, is a file's extension: letters and digits (one letter
/// at least) to the token's end, closing punctuation aside.
fn is_extension(rest: &[(usize, char)]) -> bool {
    let core = rest
        .iter()
        .rposition(|(_, c)| !closes_a_word(*c))
        .map_or(&rest[..0], |last| &rest[..=last]);
    !core.is_empty()
        && core.iter().all(|(_, c)| c.is_alphanumeric())
        && core.iter().any(|(_, c)| c.is_alphabetic())
}

fn identifier_joints(token: &str) -> IdentifierJoints {
    let chars: Vec<(usize, char)> = token.char_indices().collect();
    let mut tiers = IdentifierJoints {
        joints: Vec::new(),
        extension: Vec::new(),
        before_hyphen: Vec::new(),
        in_numbers: Vec::new(),
    };
    for index in 1..chars.len() {
        let (at, cur) = chars[index];
        let prev = chars[index - 1].1;
        let next = chars.get(index + 1).map(|(_, c)| *c);
        let before = matches!(
            cur,
            '.' | '_' | '+' | '@' | '#' | '~' | '?' | '&' | '=' | ',' | '/' | '\\'
        ) && (prev.is_alphanumeric() || matches!(prev, ')' | ']' | '}'))
            && next.is_some_and(|next| !next.is_whitespace());
        let after_hyphen = prev == '-'
            && cur.is_alphanumeric()
            && index >= 2
            && chars[index - 2].1.is_alphanumeric();
        let after_colon = prev == ':'
            && cur.is_alphanumeric()
            && index >= 2
            && chars[index - 2].1.is_alphanumeric();
        let in_number = matches!(cur, '.' | ',')
            && prev.is_ascii_digit()
            && next.is_some_and(|next| next.is_ascii_digit());
        if before && in_number {
            tiers.in_numbers.push(at);
        } else if before && cur == '.' && is_extension(&chars[index + 1..]) {
            tiers.extension.push(at);
        } else if before || after_hyphen || after_colon {
            tiers.joints.push(at);
        }
        let ends_a_word = chars[..index]
            .iter()
            .rev()
            .take_while(|(_, c)| c.is_alphanumeric())
            .all(|(_, c)| c.is_alphabetic());
        if cur == '-'
            && prev.is_alphabetic()
            && ends_a_word
            && next.is_some_and(char::is_alphanumeric)
        {
            tiers.before_hyphen.push(at);
        }
    }
    tiers
}

fn clamp_piece(text: &str, piece: std::ops::Range<usize>) -> std::ops::Range<usize> {
    let start = crate::word::floor_char_boundary(text, piece.start);
    let end = crate::word::floor_char_boundary(text, piece.end).max(start);
    start..end
}

/// A PIECE WIDER THAN A WHOLE LINE, BROKEN INSIDE ITSELF where a typesetter
/// would break it: the lines `text[piece]` takes in a box `fits` measures,
/// each `(the byte of text it starts at, what it paints)`, every line but
/// the last as full as the box holds.
#[must_use]
/// Break an overwide piece into lines `fits` measures.
pub fn break_inside(
    text: &str,
    piece: std::ops::Range<usize>,
    mark: &str,
    fits: &dyn Fn(&str) -> bool,
) -> Vec<(usize, String)> {
    let piece = clamp_piece(text, piece);
    let inside = Inside::new(text, piece.clone(), mark);
    let mut lines = Vec::new();
    let mut start = piece.start;
    loop {
        let rest = &text[start..piece.end];
        if fits(rest) || start >= inside.word_end {
            lines.push((start, rest.trim_start().to_string()));
            break;
        }
        let (at, with_mark) = inside
            .point(start, fits)
            .unwrap_or_else(|| inside.anywhere(start, fits));
        if at >= piece.end {
            lines.push((start, rest.trim_start().to_string()));
            break;
        }
        lines.push((start, inside.marked(start, at, with_mark)));
        start = at;
    }
    lines
}

/// THE FIRST PART OF A PIECE NO WHOLE LINE HOLDS THAT THE LINE IT WOULD
/// START ON STILL HOLDS, as a typesetter fills a line before it breaks a
/// word: `fits_here` measures the part on that line (its words, and what
/// joins the piece to them), `fits_line` a whole line.
#[must_use]
/// Split off the longest prefix of `piece` that `fits_here` holds; for a piece that is not
/// prose, only where `fits_line` holds the rest.
pub fn break_into_line(
    text: &str,
    piece: std::ops::Range<usize>,
    mark: &str,
    fits_here: &dyn Fn(&str) -> bool,
    fits_line: &dyn Fn(&str) -> bool,
) -> Option<(usize, String)> {
    let piece = clamp_piece(text, piece);
    let inside = Inside::new(text, piece.clone(), mark);
    let start = piece.start;
    let point = if inside.word {
        inside.point(start, fits_here)
    } else {
        inside.tiers.first().and_then(|joints| {
            joints
                .iter()
                .rev()
                .find(|(at, with_mark)| {
                    fits_here(&inside.marked(start, *at, *with_mark))
                        && fits_line(&text[*at..piece.end])
                })
                .copied()
        })
    };
    point.map(|(at, with_mark)| (at, inside.marked(start, at, with_mark)))
}

/// A piece too wide for a line, as [`break_inside`] reads it.
struct Inside<'a> {
    text: &'a str,
    mark: &'a str,
    piece_end: usize,
    /// Where the piece's own word ends: before what a wrap glued on (" ·").
    word_end: usize,
    /// Whether the piece's word is prose ([`TokenKind::Word`]), which a break marks.
    word: bool,
    /// The places it may break, in tiers tried in turn, each with whether
    /// it takes the mark: a word's syllables, then those a narrow measure
    /// takes (two letters after the hyphen); an identifier's joints, then
    /// the dot of its file extension, then the points between a number's
    /// digits, then the syllables of its runs of letters, then before a
    /// hyphen that ends a word, with none.
    tiers: Vec<Vec<(usize, bool)>>,
    /// Every one of them, in order.
    points: Vec<usize>,
    /// Every character boundary inside the word: the last resort.
    boundaries: Vec<usize>,
}

impl<'a> Inside<'a> {
    fn new(text: &'a str, piece: std::ops::Range<usize>, mark: &'a str) -> Self {
        let token_start = text[..piece.start]
            .char_indices()
            .rev()
            .find(|(_, c)| is_break_space(*c))
            .map_or(0, |(at, c)| at + c.len_utf8());
        let token_end = text[piece.start..]
            .char_indices()
            .find(|(_, c)| is_break_space(*c))
            .map_or(text.len(), |(at, _)| piece.start + at);
        let word_end = token_end.min(piece.end);
        let token = &text[token_start..token_end];
        let word = !mark.is_empty() && token_kind(token) == TokenKind::Word;
        let inside = |points: Vec<usize>, with_mark: bool| -> Vec<(usize, bool)> {
            points
                .into_iter()
                .map(|at| token_start + at)
                .filter(|at| piece.start < *at && *at < word_end)
                .map(|at| (at, with_mark))
                .collect()
        };
        let (tiers, before_hyphen): (Vec<Vec<(usize, bool)>>, Option<usize>) = if word {
            let narrow = crate::hyphenate::narrow_hyphen_points(token);
            (
                vec![
                    inside(crate::hyphen_points(token), true),
                    inside(narrow, true),
                ],
                None,
            )
        } else {
            let IdentifierJoints {
                joints,
                extension,
                before_hyphen,
                in_numbers,
            } = identifier_joints(token);
            let tiers = vec![
                inside(joints, false),
                inside(extension, false),
                inside(in_numbers, false),
                inside(crate::hyphenate::syllable_points(token), false),
                inside(before_hyphen, false),
            ];
            (tiers, Some(4))
        };
        // A break before a hyphen is the one after it, moved back: it ends no unit of its own.
        let mut points: Vec<usize> = tiers
            .iter()
            .enumerate()
            .filter(|(tier, _)| Some(*tier) != before_hyphen)
            .flat_map(|(_, tier)| tier.iter().map(|(at, _)| *at))
            .collect();
        points.sort_unstable();
        let boundaries = text[piece.start..word_end]
            .grapheme_indices()
            .map(|(at, _)| piece.start + at)
            .skip(1)
            .collect();
        Self {
            text,
            mark,
            piece_end: piece.end,
            word_end,
            word,
            tiers,
            points,
            boundaries,
        }
    }

    fn marked(&self, from: usize, to: usize, with_mark: bool) -> String {
        format!(
            "{}{}",
            &self.text[from..to],
            if with_mark { self.mark } else { "" }
        )
    }

    fn point(&self, start: usize, fits: &dyn Fn(&str) -> bool) -> Option<(usize, bool)> {
        self.tiers.iter().find_map(|tier| {
            tier.iter()
                .rev()
                .filter(|(at, _)| *at > start && self.says_something(start, *at))
                .find(|(at, with_mark)| fits(&self.marked(start, *at, *with_mark)))
                .copied()
        })
    }

    /// Whether a line from `start` to `at` holds a letter or a digit: never a lone separator
    /// ("-" / "lab").
    fn says_something(&self, start: usize, at: usize) -> bool {
        self.text[start..at].chars().any(char::is_alphanumeric)
    }

    fn anywhere(&self, start: usize, fits: &dyn Fn(&str) -> bool) -> (usize, bool) {
        let count = |from: usize, to: usize| {
            self.boundaries
                .iter()
                .filter(|at| from < **at && **at < to)
                .count()
                + 1
        };
        let ahead: Vec<usize> = self
            .boundaries
            .iter()
            .copied()
            .filter(|at| *at > start)
            .collect();
        let mut at = ahead
            .iter()
            .rev()
            .find(|at| fits(&self.marked(start, **at, self.word)))
            .or(ahead.first())
            .copied()
            .unwrap_or(self.word_end);
        let unit_end = self
            .points
            .iter()
            .copied()
            .find(|point| *point > at)
            .unwrap_or(self.word_end);
        // The cut carries three graphemes of its unit over while the line keeps more than
        // three, and never one alone: "Ope-" / "n" is "Op-" / "en".
        for least in [3, 2] {
            while count(at, unit_end) < least && count(start, at) > least {
                let Some(back) = ahead.iter().rev().find(|back| **back < at).copied() else {
                    break;
                };
                if unit_end == self.word_end && !fits(&self.text[back..self.piece_end]) {
                    break;
                }
                at = back;
            }
        }
        (at, self.word && at < self.word_end)
    }
}

/// The places `text` may break that keep whole words
/// ([`BreakKind::is_word_boundary`]): its spaces, and between CJK
/// characters.
#[must_use]
/// Return word-boundary [`LineBreak`]s in `text` ([`BreakKind::is_word_boundary`]).
pub fn word_breaks(text: &str) -> Vec<LineBreak> {
    let mut breaks = line_breaks(text);
    breaks.retain(|brk| brk.kind.is_word_boundary());
    breaks
}

/// A piece of a line of text between two places it may break
/// ([`pieces`]): what a greedy wrap lays on a line whole, or carries to the
/// next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// A slice of text between two break opportunities ([`pieces`]).
pub struct Piece<'a> {
    /// Trimmed slice of the source text.
    pub text: &'a str,
    /// Starting byte offset in the source text.
    pub at: usize,
    /// Whether whitespace preceded this piece.
    pub spaced: bool,
}

/// `text` CUT AT EVERY PLACE A LINE OF IT MAY BREAK OF A KIND `keep` TAKES
/// ([`line_breaks`]), in order, white space trimmed and empty pieces
/// dropped: the one cut every greedy word wrap of aterm's chrome lays its
/// lines from.
#[must_use]
/// Split `text` at every [`LineBreak`] accepted by `keep` ([`line_breaks`]).
pub fn pieces<'a>(text: &'a str, keep: fn(BreakKind) -> bool) -> Vec<Piece<'a>> {
    let mut out: Vec<Piece<'a>> = Vec::new();
    let push = |out: &mut Vec<Piece<'a>>, from: usize, to: usize, spaced: bool| {
        let piece = text[from..to].trim();
        if !piece.is_empty() {
            let at = from + (text[from..to].len() - text[from..to].trim_start().len());
            let spaced = spaced && !out.is_empty();
            out.push(Piece {
                text: piece,
                at,
                spaced,
            });
        }
    };
    let mut from = 0;
    let mut spaced = false;
    for brk in line_breaks(text).into_iter().filter(|brk| keep(brk.kind)) {
        push(&mut out, from, brk.at, spaced);
        from = brk.at;
        spaced = brk.kind == BreakKind::Space;
    }
    push(&mut out, from, text.len(), spaced);
    out
}

/// Whether a line may break after `c` as after white space: white space
/// that is not a no-break space ([`line_breaks`]' glue, U+00A0, U+2007,
/// U+202F).
#[must_use]
#[inline]
/// Whether `c` is breaking whitespace (whitespace other than no-break spaces) ([`line_breaks`]).
pub fn is_break_space(c: char) -> bool {
    c.is_whitespace() && !is_glue(c)
}

#[cfg(test)]
#[path = "line_break_tests.rs"]
mod tests;
