// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Text shaping: control-stripping sanitization, truncation, the detail shaper that keeps a command whole and a path's file name, the split of a sentence across detail lines, and the glass title form.

use crate::{GLASS_TITLE_CHARS, GLASS_TITLE_WORDS, PIECE_SEP};

/// Strip control characters (escape-sequence injection) and the invisible format characters a spoof rides on ([`is_stripped`]), then cap length before a string reaches a cell.
#[must_use]
pub(crate) fn sanitize(s: &str, cap: usize) -> String {
    let mut out = String::with_capacity(s.len().min(cap.saturating_add(4)));
    for (i, c) in s.chars().filter(|c| !is_stripped(*c)).enumerate() {
        if i >= cap {
            out.push('\u{2026}');
            break;
        }
        out.push(c);
    }
    out
}

/// A character no chrome surface prints: a control (C0 and C1, so every ASCII line break and tab), a noncharacter, or an invisible format character that re-orders or hides the text around it (bidi controls, zero-width and filler characters, separators, the tag block's invisible ASCII that an agent reading the text still reads).
#[inline]
#[must_use]
pub const fn is_hidden_char(c: char) -> bool {
    matches!(c, '\u{0}'..='\u{1f}' | '\u{7f}'..='\u{9f}')
        || matches!(
            c,
            '\u{00ad}'
                | '\u{034f}'
                | '\u{061c}'
                | '\u{115f}'..='\u{1160}'
                | '\u{17b4}'..='\u{17b5}'
                | '\u{180b}'..='\u{180f}'
                | '\u{200b}'
                | '\u{200e}'..='\u{200f}'
                | '\u{2028}'..='\u{202e}'
                | '\u{2060}'..='\u{206f}'
                | '\u{3164}'
                | '\u{fdd0}'..='\u{fdef}'
                | '\u{feff}'
                | '\u{ffa0}'
                | '\u{fff0}'..='\u{fffb}'
                | '\u{13430}'..='\u{1343f}'
                | '\u{1bca0}'..='\u{1bca3}'
                | '\u{1d173}'..='\u{1d17a}'
                | '\u{e0000}'..='\u{e007f}'
        )
        || (c as u32) & 0xfffe == 0xfffe
}

/// A character the band and the presence row strip: [`is_hidden_char`] and the two joiners (ZWNJ, ZWJ).
#[inline]
pub(crate) const fn is_stripped(c: char) -> bool {
    is_hidden_char(c) || matches!(c, '\u{200c}' | '\u{200d}')
}

/// `s` in at most `max` chars, the last one `…` when it was cut.
#[must_use]
pub(crate) fn truncate(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut t: String = s.chars().take(max - 1).collect();
    t.push('\u{2026}');
    t
}

/// Sanitize, then fit in `cap` chars including the ellipsis: what every message field is stored as.
#[must_use]
pub(crate) fn clip(s: &str, cap: usize) -> String {
    truncate(&sanitize(s, usize::MAX), cap)
}

/// A sentence received from elsewhere, kept whole across lines of at most `cap` chars (sanitized first): while the rest is longer than `cap`, it is cut at the last `; ` whose head fits (the joint dropped), else the last ` · ` (dropped), else the last `.`
#[must_use]
pub fn split_sentence(s: &str, cap: usize) -> Vec<String> {
    const JOINTS: [(&str, bool); 4] = [
        ("; ", false),
        (PIECE_SEP, false),
        (". ", true),
        (" ", false),
    ];
    let clean = sanitize(s, usize::MAX);
    let cap = cap.max(1);
    let mut lines = Vec::new();
    let mut rest = clean.trim();
    while let Some((limit, _)) = rest.char_indices().nth(cap) {
        let cut = JOINTS.iter().find_map(|(joint, keep_head)| {
            let end = (limit + joint.len()..=rest.len())
                .find(|b| rest.is_char_boundary(*b))
                .unwrap_or(rest.len());
            rest[..end]
                .match_indices(joint)
                .map(|(i, _)| i)
                .filter(|i| *i > 0 && i + usize::from(*keep_head) <= limit)
                .last()
                .map(|i| {
                    let head_end = if *keep_head { i + 1 } else { i };
                    (head_end, i + joint.len())
                })
        });
        let cut = cut.or_else(|| {
            let scan = rest[limit..]
                .chars()
                .next()
                .map_or(rest.len(), |next| limit + next.len_utf8());
            aterm_grapheme::word_breaks(&rest[..scan])
                .into_iter()
                .take_while(|brk| brk.at <= limit)
                .last()
                .map(|brk| (brk.at, brk.at))
        });
        let (head_end, tail_start) = cut.unwrap_or((limit, limit));
        let head = rest[..head_end].trim();
        if !head.is_empty() {
            lines.push(head.to_string());
        }
        rest = rest[tail_start..].trim_start();
    }
    if !rest.is_empty() {
        lines.push(rest.to_string());
    }
    lines
}

/// Character count — the width function the tests inject.
#[inline]
#[must_use]
pub fn char_width(s: &str) -> usize {
    s.chars().count()
}

/// A detail that always keeps its command and its file name.
#[must_use]
pub fn shape_detail(detail: &str, cap: usize) -> String {
    let mut clean = sanitize(detail, usize::MAX);
    if char_width(&clean) <= cap {
        return clean;
    }
    if let Some((_, fix)) = clean.split_once(" \u{2192} ")
        && !fix.contains(" \u{2192} ")
        && !fix.trim().is_empty()
    {
        let arrowed = format!("\u{2192} {}", fix.trim());
        return if char_width(&arrowed) <= cap {
            arrowed
        } else {
            String::new()
        };
    }
    if clean.contains('`') && clean.contains(PIECE_SEP) {
        let pieces: Vec<&str> = clean.split(PIECE_SEP).collect();
        let anchor = pieces
            .iter()
            .position(|p| p.contains("Run `"))
            .or_else(|| pieces.iter().rposition(|p| p.contains('`')))
            .map_or(1, |i| i + 1);
        for keep in (anchor..pieces.len()).rev() {
            let joined = pieces[..keep].join(PIECE_SEP);
            if char_width(&joined) <= cap {
                return joined;
            }
        }
        clean = pieces[..anchor.min(pieces.len())].join(PIECE_SEP);
        if char_width(&clean) <= cap {
            return clean;
        }
    }
    if let Some(command) = command_anchor(&clean) {
        return keep_command(&clean, command, cap);
    }
    if let Some(shaped) = shape_path_in(&clean, cap) {
        return shaped;
    }
    trim_words(&clean, cap)
}

/// The least a cut prose head may be before it gives way whole to the command
/// it stood in front of: two words and twelve cells.
const MIN_HEAD_WORDS: usize = 2;
const MIN_HEAD_CELLS: usize = 12;

/// An excerpt's lead clause: its words before its first joint (a `: ` or the ` · ` that ends its first piece), where they are a whole statement on their own: at least [`MIN_HEAD_WORDS`] words and [`MIN_HEAD_CELLS`] cells, no command quoting, and words after it.
#[must_use]
pub(crate) fn lead_clause(detail: &str) -> Option<String> {
    let clean = sanitize(detail, usize::MAX);
    let (at, joint) = [": ", PIECE_SEP]
        .into_iter()
        .filter_map(|joint| clean.find(joint).map(|at| (at, joint.len())))
        .min()?;
    let (lead, rest) = (clean[..at].trim(), &clean[at + joint..]);
    (!rest.trim().is_empty()
        && !lead.contains('`')
        && lead.split_whitespace().count() >= MIN_HEAD_WORDS
        && char_width(lead) >= MIN_HEAD_CELLS)
        .then(|| lead.to_string())
}

/// The prose before a command gives way to the command: back up to the start
/// of the sentence (or ` · ` clause) the backtick sits in, so the kept tail
/// reads as a sentence; when even that sentence is too long, the tail starts
/// at the command itself.
fn keep_command(clean: &str, command: usize, cap: usize) -> String {
    let sentence = sentence_start(clean, command);
    for (at, head_from) in [(sentence, 0), (command, sentence)] {
        let tail = &clean[at..];
        let tail_len = char_width(tail);
        if tail_len + 2 > cap {
            continue;
        }
        let head_budget = cap - tail_len - 2;
        let head = clean[head_from..at].trim_end();
        let mut out: String = head.chars().take(head_budget).collect();
        if head
            .chars()
            .nth(head_budget)
            .is_some_and(|next| next != ' ')
            && let Some(space) = out.rfind(' ')
            && space > 0
        {
            out.truncate(space);
        }
        out = out.trim_end().to_string();
        if out.split_whitespace().count() < MIN_HEAD_WORDS || char_width(&out) < MIN_HEAD_CELLS {
            return tail.to_string();
        }
        out.push_str("\u{2026} ");
        out.push_str(tail);
        return out;
    }
    let tail = &clean[command..];
    let shaped = trim_words(tail, cap);
    let kept = shaped
        .strip_suffix('\u{2026}')
        .map_or(shaped.len(), str::len);
    let command_len = tail
        .find('`')
        .and_then(|open| tail[open + 1..].find('`').map(|close| open + 1 + close + 1));
    if command_len.is_some_and(|len| kept >= len) {
        shaped
    } else {
        truncate(tail, cap)
    }
}

/// Where the command a cut detail must keep begins: a `Run` sentence (`Run`
/// followed by a backtick-quoted command) when there is one, else the first
/// backtick of the last ` · ` clause that carries one.
fn command_anchor(clean: &str) -> Option<usize> {
    if let Some(at) = clean.find("Run `") {
        return Some(at);
    }
    let mut found = None;
    let mut start = 0;
    for clause in clean.split(PIECE_SEP) {
        if let Some(at) = clause.find('`') {
            found = Some(start + at);
        }
        start += clause.len() + PIECE_SEP.len();
    }
    found
}

/// The start of the sentence or ` · ` clause that `at` sits in, whichever
/// is nearer; `at` itself when neither precedes it.
fn sentence_start(clean: &str, at: usize) -> usize {
    let dot = clean[..at].rfind(". ").map(|dot| dot + 2);
    let clause = clean[..at]
        .rfind(PIECE_SEP)
        .map(|sep| sep + PIECE_SEP.len());
    dot.max(clause).unwrap_or(at).min(at)
}

/// The last whitespace-delimited token that is a path atom (starts with `/`
/// or `~/` and has a `/` past its first character), as a byte range.
fn path_atom(clean: &str) -> Option<(usize, usize)> {
    let mut best = None;
    let mut start = None;
    for (i, c) in clean.char_indices() {
        match (c.is_whitespace(), start) {
            (false, None) => start = Some(i),
            (true, Some(s)) => {
                if is_path_atom(&clean[s..i]) {
                    best = Some((s, i));
                }
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start
        && is_path_atom(&clean[s..])
    {
        best = Some((s, clean.len()));
    }
    best
}

fn is_path_atom(token: &str) -> bool {
    let body = token.strip_prefix('~').unwrap_or(token);
    body.starts_with('/') && body[1..].contains('/')
}

/// A path cut from the left so it fits in `cap` chars: the longest tail starting at a `/` boundary, prefixed `…` (`…/aterm/crash.log`, `…/crash.log`); when even the file name is too long, the file name's own tail.
#[must_use]
pub(crate) fn shape_path(path: &str, cap: usize) -> String {
    if char_width(path) <= cap {
        return path.to_string();
    }
    if cap < 2 {
        return truncate(path, cap);
    }
    let mut best: Option<&str> = None;
    for (i, c) in path.char_indices().skip(1) {
        if c == '/' {
            let tail = &path[i..];
            if char_width(tail) < cap {
                best = Some(tail);
                break;
            }
        }
    }
    let tail = best.map_or_else(
        || {
            let keep = cap - 1;
            let n = char_width(path);
            path.chars().skip(n - keep).collect::<String>()
        },
        str::to_string,
    );
    format!("\u{2026}{tail}")
}

/// The path rule inside a detail: the last path atom cut from the left with
/// [`shape_path`] so the whole line fits `cap`; `None` when the line has no
/// path atom or the words around it leave no room for `…/`.
fn shape_path_in(clean: &str, cap: usize) -> Option<String> {
    let (s, e) = path_atom(clean)?;
    let around = char_width(&clean[..s]) + char_width(&clean[e..]);
    let avail = cap.checked_sub(around)?;
    if avail < 2 {
        return None;
    }
    let shaped = shape_path(&clean[s..e], avail);
    Some(format!("{}{shaped}{}", &clean[..s], &clean[e..]))
}

/// Prose cut to at most `cap` chars on a word boundary, ending in `…`: "these are what…", never "these are wha…".
#[must_use]
pub(crate) fn trim_words(s: &str, cap: usize) -> String {
    if char_width(s) <= cap {
        return s.to_string();
    }
    if cap == 0 {
        return String::new();
    }
    let keep = cap - 1;
    let limit = s.char_indices().nth(keep).map_or(s.len(), |(at, _)| at);
    let mut out = &s[..limit];
    if s[limit..].starts_with(|next: char| next != ' ')
        && let Some(brk) = aterm_grapheme::word_breaks(s)
            .into_iter()
            .take_while(|brk| brk.at <= limit)
            .last()
        && !s[..brk.at].trim_end().is_empty()
    {
        out = &s[..brk.at];
    }
    let mut out = out.trim_end().to_string();
    out.push('\u{2026}');
    out
}

/// The words of a title: whitespace tokens carrying a letter, so a route's `▸`, a name's `&` and a bare number (the `2` of `in tab 2`) spend no word.
#[must_use]
pub fn title_words(title: &str) -> usize {
    title
        .split_whitespace()
        .filter(|token| token.chars().any(char::is_alphabetic))
        .count()
}

/// The clause seams a glass title may not carry.
const TITLE_SEAMS: [&str; 3] = [" \u{2014} ", "; ", ": "];

/// Why `title` may not be a glass title, or `None`: the one copy of the rule, read by the host's `attention` and by the wire's parser.
#[must_use]
pub fn glass_title_fault(title: &str) -> Option<&'static str> {
    if title_words(title) > GLASS_TITLE_WORDS {
        Some("a glass title over six words")
    } else if title.chars().count() > GLASS_TITLE_CHARS {
        Some("a glass title over 48 characters")
    } else if TITLE_SEAMS.iter().any(|seam| title.contains(seam)) {
        Some("a clause in a glass title")
    } else if title.ends_with('.') {
        Some("a sentence for a glass title")
    } else {
        None
    }
}

#[cfg(test)]
#[path = "text_tests.rs"]
mod tests;
