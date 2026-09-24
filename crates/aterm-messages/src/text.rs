// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Text shaping: control-stripping sanitization, truncation, the detail
//! shaper that keeps a command whole and a path's file name, and a word
//! wrap for the log page. Pure, char-counted (the host measures cells with
//! its own width function; these functions only ever make strings shorter,
//! so a grapheme-aware measure can only find them narrower than counted).

use crate::PIECE_SEP;

/// Strip control characters (terminal escape-sequence injection) and the
/// invisible FORMAT characters a spoof rides on — the bidi overrides and
/// isolates (U+202A–U+202E, U+2066–U+2069), the zero-width joiners, spaces
/// and marks (U+200B–U+200F, U+FEFF) and the line and paragraph separators
/// (U+2028, U+2029) — then cap length before a string reaches a cell. The
/// cap is in characters, applied after the strip; an elided tail is marked
/// with `…` — so an elided result is `cap + 1` chars long, the
/// `atpkg::progress::sanitize_for_tty` shape (re-implemented here so the
/// crate takes no dependency on it; the format-character strip is this
/// crate's own hardening, because the band prints paths and commands that a
/// reversed run could forge, and the log page copies them to a clipboard).
#[must_use]
pub fn sanitize(s: &str, cap: usize) -> String {
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

/// A control character, or one of the invisible format characters that
/// re-orders or hides what follows it.
fn is_stripped(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{200b}'..='\u{200f}'
                | '\u{202a}'..='\u{202e}'
                | '\u{2066}'..='\u{2069}'
                | '\u{feff}'
                | '\u{2028}'
                | '\u{2029}'
        )
}

/// `s` in at most `max` chars, the last one `…` when it was cut
/// (`status_bars.rs:2645`). `max == 0` is the empty string.
#[must_use]
pub fn truncate(s: &str, max: usize) -> String {
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

/// Sanitize, then fit in `cap` chars INCLUDING the ellipsis — what every
/// message field is stored as.
#[must_use]
pub fn clip(s: &str, cap: usize) -> String {
    truncate(&sanitize(s, usize::MAX), cap)
}

/// A sentence received from elsewhere, KEPT WHOLE across lines of at most
/// `cap` chars (sanitized first): while the rest is longer than `cap`, it is
/// cut at the last `; ` whose head fits (the joint dropped), else the last
/// ` · ` (dropped), else the last `. ` (the period kept), else the last
/// space, else hard at `cap`. Pieces are trimmed and never empty; rejoined
/// at the joints they cut, they are the sanitized sentence (design ruling
/// 63: no reporter pre-cuts a sentence it received — the durable entry is
/// the whole of it, and the band shapes only `detail[0]`).
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
    // LINEAR in the sentence: each cut looks at the head it cuts and no
    // further — the rest is never re-measured or re-searched per line, so a
    // sentence of any length costs one pass (a caller's error text is not
    // bounded before it gets here).
    //
    // While the rest is longer than `cap` chars, `limit` is the byte offset
    // just past its `cap`-th char: a joint must END inside the head for the
    // head to fit.
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

/// A detail that ALWAYS keeps its command, and now its file name. Sanitized
/// like every other cell string; when longer than `cap`, a detail made of
/// ` · `-joined pieces first sheds its trailing pieces WHOLE, last piece
/// first, down to the piece that holds the command sentence. Still too long,
/// the PROSE gives way — the tail from the first backtick-quoted command
/// sentence to the end is kept whole and the prefix is cut to fit, with the
/// ellipsis on the prose. A detail without a command but with a path atom
/// (`~/…` or `/…`, no spaces) has the path cut from the LEFT keeping the
/// file name ([`shape_path`]); plain prose is cut on a word boundary. The
/// result is at most `cap` chars, ellipsis included. Pure, so the width law
/// is testable on literal values (ported from status_bars.rs:329-405).
#[must_use]
pub fn shape_detail(detail: &str, cap: usize) -> String {
    let mut clean = sanitize(detail, usize::MAX);
    if char_width(&clean) <= cap {
        return clean;
    }
    if clean.contains('`') && clean.contains(PIECE_SEP) {
        let pieces: Vec<&str> = clean.split(PIECE_SEP).collect();
        // The piece the cut below anchors on is never shed: everything after
        // it is a trailing figure, each worth showing only whole. The same
        // command the cut anchors on — a `Run \`…\`` sentence anywhere first,
        // else the LAST piece carrying a backtick (a remedy is said last) —
        // so the two stages never keep different pieces.
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

/// The least a cut prose head may be before it gives way whole to the
/// command it stood in front of: two words AND twelve cells. Below either,
/// a fragment says nothing a person can act on — "1… `…`" is a count
/// without its clause — and the command alone is the honest cut.
const MIN_HEAD_WORDS: usize = 2;
const MIN_HEAD_CELLS: usize = 12;

/// The prose before a command gives way to the command: back up to the
/// start of the sentence (or ` · ` clause) the backtick sits in, so the kept
/// tail reads as a sentence and not as half of one. When even that sentence
/// is too long, the tail starts at the command itself — the remedy must
/// survive the cut, not the words in front of it. A head cut below
/// [`MIN_HEAD_WORDS`] / [`MIN_HEAD_CELLS`] is dropped whole rather than
/// emitted as a stub.
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
        // A cut head is a CLAUSE or nothing: fewer than two words, or fewer
        // than twelve cells, and the count stands without what it counts
        // ("1… `…`", the installed row at 80 cols once ruling 19 took the
        // meter and left it room for a detail) — the prose goes whole and
        // the command stands alone.
        if out.split_whitespace().count() < MIN_HEAD_WORDS || char_width(&out) < MIN_HEAD_CELLS {
            return tail.to_string();
        }
        out.push_str("\u{2026} ");
        out.push_str(tail);
        return out;
    }
    // Even the sentence is too long: the tail from the backtick, cut on a
    // word boundary (`\`aterm pkg doctor\` prints…`, never `prints the
    // reve…`) as long as the command itself survives the cut whole; when
    // even the command's own words are too long, as much of THEM as fits —
    // the command is at the front.
    let tail = &clean[command..];
    let shaped = trim_words(tail, cap);
    let kept = shaped
        .strip_suffix('\u{2026}')
        .map_or(shaped.len(), str::len);
    let command_len = tail[1..].find('`').map(|close| close + 2);
    if command_len.is_some_and(|len| kept >= len) {
        shaped
    } else {
        truncate(tail, cap)
    }
}

/// Where the command a cut detail must keep begins: a `Run` sentence
/// (`Run` followed by a backtick-quoted command) when there is one, else the
/// first backtick of the LAST ` · ` clause that carries one — a remedy is
/// said last, and it is the remedy, not the name of the program the row is
/// about, that a person acts on.
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

/// The LAST whitespace-delimited token that is a path atom — starts with
/// `/` or `~/` and has a `/` past its first character — as a byte range.
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

/// A path cut from the LEFT so it fits in `cap` chars: the longest tail
/// starting at a `/` boundary, prefixed `…` (`…/aterm/crash.log`,
/// `…/crash.log`); when even the file name is too long, the file name's own
/// tail. Whole when it fits; `cap < 2` is `…` alone (or nothing at 0).
#[must_use]
pub fn shape_path(path: &str, cap: usize) -> String {
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
/// "these are what…", never "these are wha…".
#[must_use]
pub fn trim_words(s: &str, cap: usize) -> String {
    if char_width(s) <= cap {
        return s.to_string();
    }
    if cap == 0 {
        return String::new();
    }
    let keep = cap - 1;
    let mut out: String = s.chars().take(keep).collect();
    if s.chars().nth(keep).is_some_and(|next| next != ' ')
        && let Some(space) = out.rfind(' ')
        && space > 0
    {
        out.truncate(space);
    }
    let mut out = out.trim_end().to_string();
    out.push('\u{2026}');
    out
}

/// Greedy word wrap to `width` chars per line; a word longer than the width
/// is split hard. `width == 0` yields the text as one line.
#[must_use]
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut line_w = 0;
    for word in text.split(' ') {
        let mut word = word;
        let mut w = char_width(word);
        while w > width {
            // A word wider than the line: flush what is there, then hard-split.
            if line_w > 0 {
                lines.push(std::mem::take(&mut line));
                line_w = 0;
            }
            let cut = word
                .char_indices()
                .nth(width)
                .map_or(word.len(), |(i, _)| i);
            lines.push(word[..cut].to_string());
            word = &word[cut..];
            w = char_width(word);
        }
        let need = if line_w == 0 { w } else { line_w + 1 + w };
        if need > width && line_w > 0 {
            lines.push(std::mem::take(&mut line));
            line_w = 0;
        }
        if line_w > 0 {
            line.push(' ');
            line_w += 1;
        }
        line.push_str(word);
        line_w += w;
    }
    lines.push(line);
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A SENTENCE RECEIVED FROM ELSEWHERE IS KEPT WHOLE (design ruling 64):
    /// every piece within the cap, none empty, and rejoined at the joints the
    /// cut used, the sanitized sentence itself — a 1000-char sentence with
    /// `; ` joints, a run with no joints wrapped at spaces, and a message at
    /// the line cap that still round-trips the log codec.
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
        // A sentence of any length is one pass (each cut looks at its own head
        // only): four megabytes of words — thousands of lines — split and
        // rejoin exactly; searched and re-measured whole per line, as the first
        // cut did, this was quadratic and ran for minutes.
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

    /// Invariant 15a: a path atom is cut from the LEFT and keeps its file
    /// name (the config band's right-ellipsis is what cut the crash path).
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

    /// Invariant 15b (port of status_bars.rs:4991 / :5067): a detail of ` · `
    /// pieces sheds its trailing pieces WHOLE before it cuts inside one; a
    /// cut detail keeps its command and the prose carries the ellipsis.
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
        // Narrower than the pointer: the cut lands inside the pointer, on a
        // WORD, with its command whole — never the front of the revert and
        // never a mid-word stub (`prints the reve…`, review 2026-09-22).
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
        // The managed row's detail: the commands are whole while the prose
        // carries the ellipsis.
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

    /// A CUT HEAD IS A CLAUSE OR NOTHING (review 2026-09-22). With the meter
    /// gone from the installed row (ruling 19) the 80-col row had room for a
    /// detail, and the cut read "1… `. ~/.aterm/shell.d/00-atpkg.zsh`": the
    /// count without its clause, in front of the command — exactly the
    /// reading the pill's own pin forbade (`lib.rs`
    /// `the_pills_frozen_clause_survives_the_cut_with_its_count`, retired
    /// with the pill's row by the silent toolchain lane, upstream dbf97ecff;
    /// the band's `the_frozen_tab_note_keeps_its_command_whole_wherever_it_fits`
    /// keeps the law on glass).
    /// A head fragment shorter than two words or twelve cells is dropped
    /// whole and the command stands alone; a clause survives with the
    /// ellipsis on the prose, as before.
    #[test]
    fn a_stub_head_gives_way_to_the_command_whole() {
        let command = "`. ~/.aterm/shell.d/00-atpkg.zsh`";
        let installed = format!(
            "ay, trust \u{2014} ready in every tab opened since this update \u{00b7} \
             1 tab from before it picks them up with {command}"
        );
        let cmd_len = command.chars().count();
        assert_eq!(cmd_len, 33);
        // Room for the command and a stub — "", "1", "1 tab", "1 tab from",
        // "1 tab from befor" cut back to "1 tab from" — is room for the
        // command alone.
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
        // The prose before a `Run` sentence goes the same way: "th… Run
        // `aterm pkg repair` to relay them" was the stub at 40.
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

    /// Invariant 24b (port of status_bars.rs:4894): escape sequences, bells
    /// and other control characters never reach a cell string.
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

    #[test]
    fn wrap_breaks_on_words_and_splits_long_ones() {
        assert_eq!(wrap("one two three", 7), vec!["one two", "three"]);
        assert_eq!(wrap("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
        assert_eq!(wrap("a abcdefgh b", 4), vec!["a", "abcd", "efgh", "b"]);
        assert_eq!(wrap("", 4), vec![""]);
        assert_eq!(wrap("x y", 0), vec!["x y"]);
        assert_eq!(wrap("héllo wörld", 5), vec!["héllo", "wörld"]);
    }
}
