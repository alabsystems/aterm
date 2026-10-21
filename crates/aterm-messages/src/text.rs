// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Text shaping: control-stripping sanitization, truncation, the detail
//! shaper that keeps a command whole and a path's file name, the split of a
//! sentence across detail lines, and the glass title form. Pure,
//! char-counted (the host measures cells with its own width function; these
//! functions only ever make strings shorter, so a grapheme-aware measure can
//! only find them narrower than counted).

use crate::{GLASS_TITLE_CHARS, GLASS_TITLE_WORDS, PIECE_SEP};

/// Strip control characters (escape-sequence injection) and the invisible
/// format characters a spoof rides on ([`is_stripped`]), then cap length
/// before a string reaches a cell. Stripped first, an invisible character
/// cannot hide a clause seam or a period from the glass title form. The cap
/// is in characters, applied after the strip; an elided tail is marked `…`,
/// so an elided result is `cap + 1` chars (the
/// `atpkg::progress::sanitize_for_tty` shape, without the dependency).
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

/// A character no chrome surface prints: a control (C0 and C1, so every ASCII
/// line break and tab), a noncharacter, or an invisible format character that
/// re-orders or hides the text around it (bidi controls, zero-width and
/// filler characters, separators, the tag block's invisible ASCII that an
/// agent reading the text still reads).
///
/// The one table: the band's strip (`is_stripped`), the presence row's
/// tokens ([`crate::presence::sanitize_token`]) and the host's user-metadata
/// gate (`aterm-gui`'s `is_forbidden_metadata_char`) all read it. ZWJ, ZWNJ
/// and the variation selectors are not here: emoji sequences, joining scripts
/// and ideographic variants need them; a surface that drops the joiners too
/// says so.
#[must_use]
pub fn is_hidden_char(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{00ad}' // SOFT HYPHEN
                | '\u{034f}' // COMBINING GRAPHEME JOINER
                | '\u{061c}' // ARABIC LETTER MARK
                | '\u{115f}'..='\u{1160}' // Hangul fillers
                | '\u{17b4}'..='\u{17b5}' // Khmer inherent-vowel controls
                | '\u{180b}'..='\u{180f}' // Mongolian selectors / vowel separator
                | '\u{200b}' // ZERO WIDTH SPACE
                | '\u{200e}'..='\u{200f}' // directional marks
                | '\u{2028}'..='\u{202e}' // line/paragraph separators, bidi embedding/override
                | '\u{2060}'..='\u{206f}' // word joiner, invisible operators, bidi isolates
                | '\u{3164}' // HANGUL FILLER
                | '\u{fdd0}'..='\u{fdef}' // noncharacters
                | '\u{feff}' // BOM / ZERO WIDTH NO-BREAK SPACE
                | '\u{ffa0}' // HALFWIDTH HANGUL FILLER
                | '\u{fff0}'..='\u{fffb}' // specials, interlinear annotation anchors
                | '\u{13430}'..='\u{1343f}' // Egyptian hieroglyph format controls
                | '\u{1bca0}'..='\u{1bca3}' // shorthand format controls
                | '\u{1d173}'..='\u{1d17a}' // musical symbol format controls
                | '\u{e0000}'..='\u{e007f}' // the tag block
        )
        // The noncharacters at the end of every plane (U+xFFFE, U+xFFFF).
        || u32::from(c) & 0xfffe == 0xfffe
}

/// A character the band and the presence row strip: [`is_hidden_char`] and
/// the two joiners (ZWNJ, ZWJ). The band prints paths and commands a joiner
/// could hide inside (and the log page copies them to a clipboard), and the
/// row paints one character per cell, where a joiner would only fold the
/// glyph after it out of sight.
pub(crate) fn is_stripped(c: char) -> bool {
    is_hidden_char(c) || matches!(c, '\u{200c}' | '\u{200d}')
}

/// `s` in at most `max` chars, the last one `…` when it was cut. `max == 0`
/// is the empty string.
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

/// Sanitize, then fit in `cap` chars including the ellipsis: what every
/// message field is stored as.
#[must_use]
pub(crate) fn clip(s: &str, cap: usize) -> String {
    truncate(&sanitize(s, usize::MAX), cap)
}

/// A sentence received from elsewhere, kept whole across lines of at most
/// `cap` chars (sanitized first): while the rest is longer than `cap`, it is
/// cut at the last `; ` whose head fits (the joint dropped), else the last
/// ` · ` (dropped), else the last `. ` (the period kept), else the last space,
/// else between the last two CJK characters that fit, kinsoku kept
/// ([`aterm_grapheme::word_breaks`]), else hard at `cap`. Pieces are trimmed and never empty; rejoined at the
/// joints they cut, they are the sanitized sentence. No reporter pre-cuts a
/// sentence it received: the log entry is the whole of it, and the band
/// shapes only `detail[0]`.
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
    // Linear in the sentence: each cut looks at the head it cuts and no
    // further, so a sentence of any length costs one pass (a caller's error
    // text is not bounded before it gets here). `limit` is the byte offset
    // just past the rest's `cap`-th char: a joint must end inside the head
    // for the head to fit.
    while let Some((limit, _)) = rest.char_indices().nth(cap) {
        let cut = JOINTS.iter().find_map(|(joint, keep_head)| {
            // Every joint that could end inside the head starts at or before
            // `limit`, so it lies within the head plus one joint's length.
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
        // A sentence written without spaces breaks between its characters:
        // read through the char just past the head, which says whether the
        // head may end before it, and no further.
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
#[must_use]
pub fn char_width(s: &str) -> usize {
    s.chars().count()
}

/// A detail that always keeps its command and its file name. Sanitized like
/// every other cell string; when longer than `cap`, a detail of ` · `-joined
/// pieces first sheds its trailing pieces whole, down to the piece that holds
/// the command. Still too long, the prose gives way: the tail from the
/// backtick-quoted command sentence to the end is kept whole and the prefix
/// is cut to fit, with the ellipsis on the prose. A detail with a path atom
/// (`~/…` or `/…`, no spaces) and no command has the path cut from the left,
/// keeping the file name ([`shape_path`]); plain prose is cut on a word
/// boundary. The result is at most `cap` chars, ellipsis included.
#[must_use]
pub fn shape_detail(detail: &str, cap: usize) -> String {
    let mut clean = sanitize(detail, usize::MAX);
    if char_width(&clean) <= cap {
        return clean;
    }
    // A correction (`windw_padding → window_padding`) is cut to its right side
    // with the arrow, so the fix never reads as the misspelling; a fix too long
    // for the cap is dropped whole, since a clipped key is a key that is not
    // there.
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
        // The piece the cut below anchors on is never shed: everything after
        // it is a trailing figure, worth showing only whole. It is the same
        // command `command_anchor` picks (a `Run \`…\`` sentence anywhere
        // first, else the last piece carrying a backtick), so the two stages
        // never keep different pieces.
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
/// it stood in front of: two words and twelve cells. Below either, a fragment
/// ("1… `…`") says nothing a person can act on.
const MIN_HEAD_WORDS: usize = 2;
const MIN_HEAD_CELLS: usize = 12;

/// An excerpt's lead clause: its words before its first joint (a `: ` or the
/// ` · ` that ends its first piece), where they are a whole statement on their
/// own: at least [`MIN_HEAD_WORDS`] words and [`MIN_HEAD_CELLS`] cells, no
/// command quoting, and words after it. `behind for 7 h: its turn is still
/// running` → `behind for 7 h`; `was: …`, `line 3: …` and `tab 1 · …` say
/// nothing alone. The width law paints it whole, with no ellipsis, where the
/// floor would drop the excerpt or a cut would say no word past it
/// ([`crate::glass`]).
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
/// at the command itself. A head cut below [`MIN_HEAD_WORDS`] /
/// [`MIN_HEAD_CELLS`] is dropped whole rather than emitted as a stub.
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
        // A cut head ends on a word, not inside one ("1 tab from…", not "1
        // tab from befo…"), when it has a word to end on.
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
        // A cut head is a clause or nothing: below the floor the prose goes
        // whole and the command stands alone.
        if out.split_whitespace().count() < MIN_HEAD_WORDS || char_width(&out) < MIN_HEAD_CELLS {
            return tail.to_string();
        }
        out.push_str("\u{2026} ");
        out.push_str(tail);
        return out;
    }
    // Even the sentence is too long: the tail from the backtick, cut on a word
    // boundary while the command itself survives whole; else as much of the
    // command as fits.
    let tail = &clean[command..];
    let shaped = trim_words(tail, cap);
    let kept = shaped
        .strip_suffix('\u{2026}')
        .map_or(shaped.len(), str::len);
    // The command runs to the backtick closing the first one in the tail
    // (after `Run ` when the anchor is a `Run \`…\`` sentence).
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
/// backtick of the last ` · ` clause that carries one. A remedy is said last,
/// and it is the remedy, not the name of the program the row is about, that a
/// person acts on.
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

/// A path cut from the left so it fits in `cap` chars: the longest tail
/// starting at a `/` boundary, prefixed `…` (`…/aterm/crash.log`,
/// `…/crash.log`); when even the file name is too long, the file name's own
/// tail. Whole when it fits; `cap < 2` is `…` alone (or nothing at 0).
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
                break; // the first boundary past the start is the longest tail
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

/// Prose cut to at most `cap` chars on a word boundary, ending in `…`:
/// "these are what…", never "these are wha…". A word ends at a space, and
/// in a script written without spaces at each of its characters, kinsoku
/// kept ([`aterm_grapheme::word_breaks`]).
#[must_use]
pub(crate) fn trim_words(s: &str, cap: usize) -> String {
    if char_width(s) <= cap {
        return s.to_string();
    }
    if cap == 0 {
        return String::new();
    }
    let keep = cap - 1;
    // The byte just past the `keep` chars that fit.
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

/// The words of a title: whitespace tokens carrying a letter, so a route's
/// `▸`, a name's `&` and a bare number (the `2` of `in tab 2`) spend no word.
#[must_use]
pub fn title_words(title: &str) -> usize {
    title
        .split_whitespace()
        .filter(|token| token.chars().any(char::is_alphabetic))
        .count()
}

/// The clause seams a glass title may not carry.
const TITLE_SEAMS: [&str; 3] = [" \u{2014} ", "; ", ": "];

/// Why `title` may not be a glass title, or `None`: the one copy of the rule,
/// read by the host's `attention` and by the wire's parser. In order: at most
/// [`GLASS_TITLE_WORDS`] words ([`title_words`]), at most
/// [`GLASS_TITLE_CHARS`] characters, no clause seam (` — `, `; `, `: `), no
/// trailing period.
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
mod tests {
    use super::*;

    /// The title form is `attention`'s rule, fault for fault and in its order;
    /// a lone symbol spends no word, and the words check wins when both fail.
    #[test]
    fn glass_title_fault_is_the_attention_rule() {
        assert_eq!(glass_title_fault("Updating aterm"), None);
        assert_eq!(glass_title_fault("Settings \u{25b8} Messages & more"), None);
        assert_eq!(title_words("Settings \u{25b8} Messages & more"), 3);
        assert_eq!(
            title_words("Typing slowed by yes in tab 2"),
            6,
            "a number is no word"
        );
        assert_eq!(glass_title_fault("Typing slowed by yes in tab 2"), None);
        assert_eq!(
            glass_title_fault("one two three four five six seven"),
            Some("a glass title over six words")
        );
        assert_eq!(
            glass_title_fault(&"x".repeat(GLASS_TITLE_CHARS + 1)),
            Some("a glass title over 48 characters")
        );
        assert_eq!(glass_title_fault(&"x".repeat(GLASS_TITLE_CHARS)), None);
        for seam in ["a \u{2014} b", "a; b", "Build: failed"] {
            assert_eq!(
                glass_title_fault(seam),
                Some("a clause in a glass title"),
                "{seam}"
            );
        }
        assert_eq!(
            glass_title_fault("Build failed."),
            Some("a sentence for a glass title")
        );
        assert_eq!(
            glass_title_fault(&format!("{} a b c d e f", "y".repeat(60))),
            Some("a glass title over six words"),
            "words first"
        );
        assert_eq!(
            glass_title_fault(""),
            None,
            "emptiness is the parser's to refuse"
        );
    }

    /// Every piece within the cap, none empty, and rejoined at the joints the
    /// cut used, the sanitized sentence itself; a message at the line cap
    /// still round-trips the log codec.
    #[test]
    fn a_long_sentence_splits_at_its_joints_and_loses_nothing() {
        let clause = |n: usize| format!("clause {n} {}", "w".repeat(40));
        let long: String = (0..20).map(clause).collect::<Vec<_>>().join("; ");
        assert!(long.chars().count() > 900);
        let lines = split_sentence(&long, 240);
        assert!(lines.len() > 3, "{lines:?}");
        assert!(lines.iter().all(|l| !l.is_empty() && char_width(l) <= 240));
        assert_eq!(lines.join("; "), long);
        let run: String = (0..100)
            .map(|n| format!("word{n}"))
            .collect::<Vec<_>>()
            .join(" ");
        let lines = split_sentence(&run, 80);
        assert!(lines.iter().all(|l| char_width(l) <= 80), "{lines:?}");
        assert_eq!(lines.join(" "), run);
        let hard = "x".repeat(500);
        let lines = split_sentence(&hard, 240);
        assert_eq!(lines.concat(), hard, "no joint: a hard cut, nothing lost");
        assert_eq!(split_sentence("short", 240), vec!["short"]);
        assert!(split_sentence("", 240).is_empty());
        // A period keeps its place at the end of its line.
        let dots = format!("{}. {}", "a".repeat(30), "b".repeat(30));
        assert_eq!(
            split_sentence(&dots, 40),
            vec![format!("{}.", "a".repeat(30)), "b".repeat(30)]
        );
        // Hostile text is sanitized first.
        assert_eq!(split_sentence("a\u{1b}[31mb", 240), vec!["a[31mb"]);
        // Megabytes of words split and rejoin exactly, in one pass.
        let huge: String = (0..400_000)
            .map(|n| format!("w{n}"))
            .collect::<Vec<_>>()
            .join(" ");
        assert!(huge.len() > 3_000_000);
        let lines = split_sentence(&huge, 240);
        assert!(lines.len() > 10_000, "{}", lines.len());
        assert!(lines.iter().all(|l| char_width(l) <= 240));
        assert_eq!(lines.join(" "), huge);
        // A message of such lines round-trips the record the log writes.
        let msg = crate::model::Message::new(crate::tags::UPDATE, crate::Severity::Warn, "t")
            .sentence(&long);
        assert_eq!(msg.detail.join("; "), long);
    }

    const CRASH: &str = "crash log at ~/Library/Logs/aterm/crash-signal-8123-1758470000.log.seen";

    /// A path atom is cut from the left and keeps its file name.
    #[test]
    fn a_path_is_cut_from_the_left_and_keeps_its_file_name() {
        assert_eq!(CRASH.chars().count(), 71);
        assert_eq!(shape_detail(CRASH, 71), CRASH);
        // The 120-column crash row: room 57 → cap 54 → the file name, whole.
        let at_54 = shape_detail(CRASH, 54);
        assert_eq!(
            at_54,
            "crash log at \u{2026}/crash-signal-8123-1758470000.log.seen"
        );
        assert_eq!(at_54.chars().count(), 52);
        // Wider: the longest tail at a `/` boundary that fits.
        assert_eq!(
            shape_detail(CRASH, 60),
            "crash log at \u{2026}/aterm/crash-signal-8123-1758470000.log.seen"
        );
        // Narrower than the file name: the file name's own tail.
        let at_40 = shape_detail(CRASH, 40);
        assert_eq!(at_40.chars().count(), 40);
        assert!(
            at_40.starts_with("crash log at \u{2026}") && at_40.ends_with(".log.seen"),
            "{at_40}"
        );
        // No room for even `…/`: prose gives way on a word boundary instead.
        assert_eq!(shape_detail(CRASH, 14), "crash log at\u{2026}");
        assert_eq!(shape_path("/a/b/c.log", 100), "/a/b/c.log");
        assert_eq!(shape_path("/a/b/c.log", 7), "\u{2026}/c.log");
        assert_eq!(shape_path("/a/b/c.log", 9), "\u{2026}/b/c.log");
        assert_eq!(shape_path("/a/b/c.log", 1), "\u{2026}");
        assert_eq!(shape_path("/a/b/c.log", 0), "");
        // Prose without a path cuts on a word, and never leaves a stub.
        let prose = "skipping \"cmd+shift+k\": unknown action \"foo\"";
        assert_eq!(
            shape_detail(prose, 42),
            "skipping \"cmd+shift+k\": unknown action\u{2026}"
        );
        let staged = "build 1234 \u{2014} verified; auto-apply is off \u{2014} apply it";
        assert_eq!(
            shape_detail(staged, 26),
            "build 1234 \u{2014} verified;\u{2026}"
        );
        for cap in 0..80 {
            assert!(shape_detail(prose, cap).chars().count() <= cap, "cap {cap}");
            assert!(shape_detail(CRASH, cap).chars().count() <= cap, "cap {cap}");
        }
    }

    /// A detail of ` · ` pieces sheds its trailing pieces whole before it cuts
    /// inside one; a cut detail keeps its command and the prose carries the
    /// ellipsis.
    #[test]
    fn a_command_survives_the_cut() {
        let pointer = "undo: `aterm pkg doctor` prints the revert";
        let revert = "defaults -currentHost delete com.apple.universalcontrol Disable; \
                      defaults -currentHost delete com.apple.universalcontrol DisableMagicEdges";
        let detail = format!("{pointer} \u{00b7} {revert}");
        assert!(revert.chars().count() > 100);
        assert_eq!(shape_detail(&detail, 300), detail);
        let whole = detail.chars().count();
        for cap in [whole - 1, whole - 40, pointer.chars().count()] {
            assert_eq!(shape_detail(&detail, cap), pointer, "cap {cap}");
        }
        // Narrower than the pointer: the cut lands inside it, on a word, with
        // its command whole, never the front of the revert.
        assert_eq!(
            shape_detail(&detail, 35),
            "`aterm pkg doctor` prints the\u{2026}"
        );
        assert_eq!(
            shape_detail(&detail, 30),
            "`aterm pkg doctor` prints the\u{2026}"
        );
        assert_eq!(shape_detail(&detail, 25), "`aterm pkg doctor`\u{2026}");
        let cut = shape_detail(&detail, 30);
        assert!(!cut.contains("defaults"), "{cut}");
        // The commands stay whole while the prose carries the ellipsis.
        let managed = "what `claude` and `codex` run in every aterm tab, this one too \u{00b7} build 2026091001";
        let d = shape_detail(managed, 40);
        assert!(d.contains("`claude`") && d.contains('\u{2026}'), "{d}");
        assert!(d.chars().count() <= 40, "{d}");
        // A `Run \`…\`` sentence is the anchor wherever it sits.
        let run = "the shims are stale. Run `aterm pkg repair` to relay them \u{00b7} 3 tabs";
        let d = shape_detail(run, 40);
        assert!(d.contains("Run `aterm pkg repair`"), "{d}");
        // Even the command's own words are too long: the command's front.
        let d = shape_detail("see `a very long command that goes on and on`", 12);
        assert!(d.starts_with("`a very") && d.ends_with('\u{2026}'), "{d}");
        assert_eq!(d.chars().count(), 12);
    }

    /// A head fragment shorter than two words or twelve cells is dropped whole
    /// and the command stands alone; a clause survives with the ellipsis.
    #[test]
    fn a_stub_head_gives_way_to_the_command_whole() {
        let command = "`. ~/.aterm/shell.d/00-atpkg.zsh`";
        let installed = format!(
            "ay, trust \u{2014} ready in every tab opened since this update \u{00b7} \
             1 tab from before it picks them up with {command}"
        );
        let cmd_len = command.chars().count();
        assert_eq!(cmd_len, 33);
        // Room for the command and a stub ("", "1", "1 tab", "1 tab from") is
        // room for the command alone.
        for cap in cmd_len..cmd_len + 19 {
            assert_eq!(shape_detail(&installed, cap), command, "cap {cap}");
        }
        // Room for a clause keeps it, ellipsis on the prose.
        assert_eq!(
            shape_detail(&installed, cmd_len + 19),
            format!("1 tab from before\u{2026} {command}")
        );
        assert_eq!(
            shape_detail(&installed, cmd_len + 22),
            format!("1 tab from before it\u{2026} {command}")
        );
        // The prose before a `Run` sentence goes the same way.
        let run = "the shims are stale. Run `aterm pkg repair` to relay them";
        assert_eq!(
            shape_detail(run, 40),
            "Run `aterm pkg repair` to relay them"
        );
        for cap in 0..100 {
            assert!(
                shape_detail(&installed, cap).chars().count() <= cap,
                "cap {cap}"
            );
        }
    }

    /// Escape sequences, bells and other control characters never reach a
    /// cell string.
    #[test]
    fn hostile_text_is_sanitized_before_it_becomes_a_message() {
        let bad = "bad\u{1b}[31m thing\u{7}\r\n\ttail";
        let clean = sanitize(bad, usize::MAX);
        assert_eq!(clean, "bad[31m thingtail");
        assert!(clean.chars().all(|c| !c.is_control()));
        // The invisible format characters go too: a bidi override could
        // paint `Run \`rm -rf x\`` reversed, a zero-width joiner could hide
        // inside a path the page copies to the clipboard.
        assert_eq!(
            sanitize(
                "a\u{202e}b\u{200b}c\u{feff}d\u{2066}e\u{2028}f\u{200d}g\u{2029}h",
                100
            ),
            "abcdefgh",
            "bidi overrides, zero-width characters and line separators are stripped"
        );
        // The rest of the one table goes too: a combining grapheme joiner,
        // the Hangul fillers, a Mongolian selector, Khmer's inherent vowels,
        // a shorthand and a musical format control.
        assert_eq!(
            sanitize(
                "a\u{034f}b\u{115f}c\u{3164}d\u{ffa0}e\u{180b}f\u{17b4}g\u{1bca0}h\u{1d173}i\u{e0000}j",
                100
            ),
            "abcdefghij",
            "every invisible character the metadata gate strips"
        );
        assert_eq!(
            sanitize("h\u{e9}llo \u{6f22}", 100),
            "h\u{e9}llo \u{6f22}",
            "printable non-ASCII stays"
        );
        assert_eq!(
            sanitize("abcdef", 3),
            "abc\u{2026}",
            "atpkg's cap-plus-ellipsis shape"
        );
        assert_eq!(clip("abcdef", 3), "ab\u{2026}", "clip fits INSIDE the cap");
        assert_eq!(clip("abc", 3), "abc");
        assert_eq!(truncate("abc", 0), "");
        assert_eq!(truncate("héllo wörld", 6), "héllo\u{2026}");
        assert_eq!(trim_words("a b c", 5), "a b c");
        assert_eq!(trim_words("aaaa bbbb", 6), "aaaa\u{2026}");
        assert_eq!(trim_words("aaaaaaaa", 4), "aaa\u{2026}");
        assert_eq!(trim_words("aaaa", 0), "");
    }
}
