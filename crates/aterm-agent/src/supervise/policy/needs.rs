// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Parser for `NEEDS YOU:` and `Nothing left:` report lines read by the turn-end policy.

use std::borrow::Cow;

/// The head of a line that names what only a person can give.
pub const NEEDS_YOU: &str = "NEEDS YOU:";

/// The head of the worker's defense that nothing is left to do.
pub const NOTHING_LEFT: &str = "Nothing left:";

/// The restart instruction shared by the continuations and needs acknowledgements.
macro_rules! restart_hint {
    () => {
        "Handle agent restarts yourself: save a handoff; `aterm ctl help restart`."
    };
}
pub(crate) use restart_hint;

/// [`NEEDS_HINT`] as a literal, for the texts `concat!` builds on it.
macro_rules! needs_hint {
    () => {
        concat!(
            "Put what only a person can give on a line starting NEEDS YOU: and carry on. ",
            $crate::supervise::policy::needs::restart_hint!(),
        )
    };
}
pub(crate) use needs_hint;

/// Appended after the answers aterm types to a question (`super::turn_end::prose_answer`, `super::turn_end::reversible_answer`), and to a restart's carry-on.
pub const NEEDS_HINT: &str = needs_hint!();

/// Appended to the act a point types when a NEW set of needs stands, once a set.
pub const NEEDS_ACK: &str = concat!("Needs on screen; carry on. ", restart_hint!(),);

/// THE NEED A BARE HEAD NAMES WITH NO WORDS UNDER IT.
pub const UNREAD_NEED: &str = "see the NEEDS YOU line in its report";

/// The most characters one need keeps ([`needs_you`]); a longer one is cut with `…`.
pub const NEED_CHARS: usize = 160;

const NONE_HEADS: &[&str] = &["none", "nothing", "no", "nope", "nil", "zero", "n/a", "na"];

const NOT_NEEDED: &[&str] = &["applicable", "needed", "required", "necessary"];

const NONE_MODS: &[&str] = &[
    "need",
    "needs",
    "action",
    "actions",
    "input",
    "blocker",
    "blockers",
    "blocking",
    "blocked",
    "item",
    "items",
    "ask",
    "asks",
    "request",
    "requests",
    "decision",
    "decisions",
    "waiting",
    "pending",
    "outstanding",
    "open",
    "left",
    "new",
    "is",
    "are",
    "to",
    "do",
    "give",
    "that",
    "approve",
    "sign",
    "review",
    "decide",
    "publish",
    "merge",
    "confirm",
    "check",
    "add",
    "report",
    "flag",
    "or",
    "and",
    "from",
    "by",
    "on",
    "for",
    "of",
    "you",
    "your",
    "side",
    "end",
    "part",
    "me",
    "my",
    "i",
    "we",
    "our",
    "owner",
    "at",
    "all",
    "whatsoever",
    "else",
    "other",
    "further",
    "more",
    "here",
    "the",
    "as",
    "now",
    "right",
    "moment",
    "this",
    "time",
    "point",
    "stage",
    "present",
    "currently",
    "today",
];

const NONE_STILL: &[&str] = &["yet", "so", "far"];

const NONE_NEEDED: &[&str] = &["needed", "required", "necessary"];

const NONE_OBJECT_WORDS: usize = 4;

const NONE_KEEPS: &[&str] = &[
    "but",
    "except",
    "besides",
    "than",
    "apart",
    "aside",
    "save",
    "beyond",
    "and",
    "only",
    "just",
    "merely",
    "unless",
    "however",
    "though",
    "although",
    "yet",
    "bar",
    "barring",
    "excluding",
    "outside",
];

const NONE_OBJECTS: &[&str] = &["no"];

const NONE_JOINS: &[&str] = &["and", "or", "as"];

const NONE_ASKED: &[&str] = &["i", "we", "you", "your", "our", "my", "need", "needs"];

const RESTATE_SUBJECTS: &[&str] = &[
    "all",
    "every",
    "it",
    "of",
    "the",
    "and",
    "work",
    "changes",
    "fixes",
    "commits",
    "pr",
    "prs",
    "tests",
    "ci",
    "crate",
    "branches",
    "tree",
    "worktree",
    "worktrees",
];

const RESTATE_NAMED: &[&str] = &["change", "fix", "commit", "release", "tag", "branch"];

const RESTATE_THE: &[&str] = &["the", "every"];

const RESTATE_LINKS: &[&str] = &[
    "it's",
    "it’s",
    "is",
    "are",
    "was",
    "were",
    "has",
    "have",
    "been",
    "already",
    "all",
    "everything",
];

const LANDED_SAYS: &[&str] = &[
    "landed",
    "merged",
    "pushed",
    "done",
    "complete",
    "completed",
    "finished",
    "clean",
    "green",
    "pass",
    "passes",
    "passed",
    "published",
    "set",
    "removed",
    "deleted",
    "gone",
];

const RESTATE_AT: &[&str] = &["on", "in", "into", "to"];

const RESTATE_PLACES: &[&str] = &[
    "the",
    "remote",
    "main",
    "origin/main",
    "origin",
    "master",
    "branch",
];

const NONE_TIMES: &[&str] = &[
    "for",
    "now",
    "right",
    "at",
    "the",
    "moment",
    "this",
    "time",
    "point",
    "stage",
    "present",
    "currently",
    "today",
];

const YOU_SET: &[&str] = &[
    "all", "set", "good", "to", "go", "done", "ready", "covered", "fine",
];

/// The word lists the none reader reads by ([`none_said`]): the reader's own
/// are [`NONE_WORDS`]; the needs table's mutation check takes one word out
/// at a time, so every word is shown load-bearing.
#[derive(Clone, Copy)]
struct NoneWords<'a> {
    /// [`NONE_HEADS`].
    heads: &'a [&'a str],
    /// [`NOT_NEEDED`].
    not_needed: &'a [&'a str],
    /// [`NONE_MODS`].
    mods: &'a [&'a str],
    /// [`NONE_STILL`].
    still: &'a [&'a str],
    /// [`NONE_NEEDED`].
    needed: &'a [&'a str],
    /// [`NONE_KEEPS`].
    keeps: &'a [&'a str],
    /// [`NONE_OBJECTS`].
    objects: &'a [&'a str],
    /// [`NONE_JOINS`].
    joins: &'a [&'a str],
    /// [`NONE_ASKED`].
    asked: &'a [&'a str],
    /// [`RESTATE_SUBJECTS`].
    subjects: &'a [&'a str],
    /// [`RESTATE_NAMED`].
    named: &'a [&'a str],
    /// [`RESTATE_THE`].
    the: &'a [&'a str],
    /// [`RESTATE_LINKS`].
    links: &'a [&'a str],
    /// [`LANDED_SAYS`].
    landed: &'a [&'a str],
    /// [`RESTATE_AT`].
    at: &'a [&'a str],
    /// [`RESTATE_PLACES`].
    places: &'a [&'a str],
    /// [`NONE_TIMES`].
    times: &'a [&'a str],
    /// [`YOU_SET`].
    set: &'a [&'a str],
}

const NONE_WORDS: NoneWords<'static> = NoneWords {
    heads: NONE_HEADS,
    not_needed: NOT_NEEDED,
    mods: NONE_MODS,
    still: NONE_STILL,
    needed: NONE_NEEDED,
    keeps: NONE_KEEPS,
    objects: NONE_OBJECTS,
    joins: NONE_JOINS,
    asked: NONE_ASKED,
    subjects: RESTATE_SUBJECTS,
    named: RESTATE_NAMED,
    the: RESTATE_THE,
    links: RESTATE_LINKS,
    landed: LANDED_SAYS,
    at: RESTATE_AT,
    places: RESTATE_PLACES,
    times: NONE_TIMES,
    set: YOU_SET,
};

const NONE_CLAUSE_MARKS: &[char] = &[
    '—', '–', ';', ':', ',', '(', ')', '[', ']', '.', '!', '?', '{', '}',
];

const QUOTE_MARKS: [char; 4] = ['>', '▎', '│', '┃'];

fn unquoted(t: &str) -> &str {
    let mut t = t.trim_start();
    while let Some(r) = t.strip_prefix(QUOTE_MARKS) {
        t = r.trim_start();
    }
    t
}

const MENTION_NOUNS: &[&str] = &[
    "line", "lines", "head", "heads", "row", "rows", "block", "blocks", "label", "labels", "token",
    "tokens", "prefix", "marker", "markers",
];

const MENTION_ARTICLES: &[&str] = &["a", "an", "the", "each", "every", "any", "one", "that"];

const HEAD_NAMING: &[&str] = &[
    "starting",
    "starts",
    "beginning",
    "begins",
    "prefixed",
    "headed",
];

const HEAD_NAMING_LINKS: &[&str] = &["with", "by"];

fn names_a_head(lead: &str) -> bool {
    let said: Vec<&str> = lead
        .rsplit(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .take(3)
        .collect();
    let words: Vec<String> = said.iter().map(|w| w.to_lowercase()).collect();
    let words: Vec<&str> = words.iter().map(String::as_str).collect();
    let (naming, verb_at) = match words.as_slice() {
        [link, rest @ ..] if HEAD_NAMING_LINKS.contains(link) => (rest, 1),
        all => (all, 0),
    };
    match naming {
        [verb, noun, ..] => HEAD_NAMING.contains(verb) && MENTION_NOUNS.contains(noun),
        [verb] => {
            HEAD_NAMING.contains(verb)
                && said[verb_at].chars().next().is_some_and(char::is_lowercase)
        }
        [] => false,
    }
}

fn past_marker(t: &str) -> Option<&str> {
    for m in ["- ", "* ", "+ ", "• ", "◦ ", "‣ "] {
        if let Some(rest) = t.strip_prefix(m) {
            return Some(rest.trim_start());
        }
    }
    let digits = t.bytes().take_while(u8::is_ascii_digit).count();
    let rest = t.get(digits..).filter(|_| (1..=3).contains(&digits))?;
    rest.strip_prefix(". ")
        .or_else(|| rest.strip_prefix(") "))
        .map(str::trim_start)
}

fn past_checkbox(t: &str) -> Option<&str> {
    ["[ ]", "[x]", "[X]", "[]"]
        .into_iter()
        .find_map(|b| t.strip_prefix(b))
        .filter(|r| r.is_empty() || r.starts_with(char::is_whitespace))
        .map(str::trim_start)
}

fn leads_need(c: char) -> bool {
    !c.is_ascii()
        && !c.is_alphanumeric()
        && !c.is_whitespace()
        && !QUOTE_MARKS.contains(&c)
        && !matches!(
            c,
            '“' | '”'
                | '‘'
                | '’'
                | '«'
                | '»'
                | '„'
                | '‚'
                | '‹'
                | '›'
                | '「'
                | '」'
                | '『'
                | '』'
                | '⎿'
        )
        && !('\u{2500}'..='\u{257F}').contains(&c)
}

fn past_need_lead(t: &str) -> &str {
    let mut t = t.trim_start();
    loop {
        let before = t.len();
        t = past_heading(t).0;
        t = past_marker(t).unwrap_or(t);
        t = past_checkbox(t).unwrap_or(t);
        if let Some(r) = ["**", "__", "*", "_"]
            .into_iter()
            .find_map(|m| t.strip_prefix(m))
        {
            t = r.trim_start();
        }
        let lead: usize = t
            .chars()
            .take_while(|c| leads_need(*c))
            .map(char::len_utf8)
            .sum();
        t = t[lead..].trim_start();
        if t.len() == before {
            return t;
        }
    }
}

fn needs_token(t: &str) -> Option<(usize, bool)> {
    let need = t.get(..4).filter(|w| w.eq_ignore_ascii_case("need"))?;
    let s = usize::from(t[4..].starts_with(['s', 'S']));
    let word = 4 + s;
    let gap: usize = t[word..]
        .chars()
        .take_while(|c| matches!(c, ' ' | '\t' | '\u{a0}' | '\u{202f}' | '-' | '_'))
        .map(char::len_utf8)
        .sum();
    let you = t
        .get(word + gap..word + 3 + gap)
        .filter(|w| gap > 0 && w.eq_ignore_ascii_case("you"))?;
    let capitals = need == "NEED" && (s == 0 || &t[4..5] == "S") && you == "YOU";
    Some((word + 3 + gap, capitals))
}

fn your_head(rest: &str) -> Option<&str> {
    let words = rest
        .get(1..)
        .filter(|w| rest.starts_with(['r', 'R']) && w.starts_with([' ', '\t']))?;
    let end = words.find([':', '—', '–']).or_else(|| words.find(" - "))?;
    let named = words[..end].trim().trim_matches(['*', '_']).trim();
    let count = named.split_whitespace().count();
    if !(1..=3).contains(&count)
        || !named
            .chars()
            .all(|c| c.is_alphabetic() || c.is_whitespace() || matches!(c, '-' | '\'' | '’'))
    {
        return None;
    }
    past_separator(words[end..].trim_start()).map(str::trim)
}

fn need_separator(rest: &str, capitals: bool) -> Option<&str> {
    rest.strip_prefix("--")
        .or_else(|| rest.strip_prefix("..."))
        .or_else(|| rest.strip_prefix('…'))
        .or_else(|| past_separator(rest))
        .or_else(|| {
            ["→", "->", "=>"]
                .into_iter()
                .find_map(|a| rest.strip_prefix(a))
        })
        .or_else(|| rest.strip_prefix(['!', ';', '?']).filter(|_| capitals))
}

fn need_rest(line: &str) -> Option<&str> {
    head_from(past_need_lead(unquoted(line)), false)
}

fn head_from(t: &str, named: bool) -> Option<&str> {
    let wrap = t.chars().next().filter(|c| matches!(c, '[' | '|' | '`'));
    let t = match wrap {
        Some(c) => past_need_lead(&t[c.len_utf8()..]),
        None => t,
    };
    let named = named || wrap == Some('`');
    let (len, capitals) = needs_token(t)?;
    let rest = &t[len..];
    if let Some(words) = your_head(rest) {
        return Some(if says_none(words) || words.is_empty() {
            words
        } else {
            t[len - 3..].trim()
        });
    }
    let mut next = rest.chars();
    match next.next() {
        Some(c) if c.is_alphanumeric() || matches!(c, '\'' | '’') => return None,
        Some('-') if next.next().is_some_and(char::is_alphanumeric) => return None,
        _ => {}
    }
    fn emphasis(r: &str) -> &str {
        r.trim_start_matches(['*', '_']).trim_start()
    }
    let after = rest.trim_start();
    let mut emphasized = after.starts_with(['*', '_']);
    let mut rest = emphasis(after);
    let mut separated = false;
    if let Some(open) = wrap {
        let close = if open == '[' { ']' } else { open };
        match rest.strip_prefix(close) {
            Some(r) => {
                rest = emphasis(r.trim_start());
                emphasized = true;
            }
            None if open == '`' => {
                let r = emphasis(need_separator(rest, capitals)?);
                rest = match r.strip_prefix('`') {
                    Some(after) => emphasis(after),
                    None => r.trim_end().trim_end_matches('`').trim_end(),
                };
                separated = true;
            }
            None => {}
        }
        if open == '|' {
            rest = rest.trim_end().trim_end_matches('|').trim_end();
        }
    }
    let mut qualifier = None;
    if !separated {
        for (open, close) in [('(', ')'), ('[', ']')] {
            if let Some(inner) = rest.strip_prefix(open)
                && let Some(end) = inner.find(close)
            {
                qualifier = Some(inner[..end].trim());
                rest = emphasis(inner[end + close.len_utf8()..].trim_start());
                break;
            }
        }
    }
    if !separated
        && qualifier.is_none()
        && let Some(inner) = rest.strip_prefix(',')
    {
        let who = inner.find([':', '—', '–']).and_then(|end| {
            let who = inner[..end].trim().trim_matches(['*', '_']);
            let words = who.split_whitespace().count();
            ((1..=3).contains(&words)
                && who.chars().all(|c| {
                    c.is_alphabetic() || c.is_whitespace() || matches!(c, '.' | '\'' | '’')
                }))
            .then_some((who, end))
        });
        match who {
            Some((who, end)) => {
                qualifier = Some(who);
                rest = emphasis(inner[end..].trim_start());
            }
            None if capitals => {
                rest = emphasis(inner);
                separated = true;
            }
            None => return None,
        }
    }
    if !separated && let Some(r) = need_separator(rest, capitals) {
        rest = emphasis(r);
        separated = true;
    }
    let mut rest = rest.trim();
    if !separated && qualifier.is_none() {
        if rest.starts_with(['.', ',', '?', '!', ';']) {
            return None;
        }
        if !capitals && !emphasized {
            return None;
        }
    }
    let bare = !separated && qualifier.is_none();
    if named || bare {
        let first = rest
            .trim_start_matches(|c: char| !c.is_alphanumeric())
            .split(|c: char| !c.is_alphanumeric())
            .next()
            .unwrap_or("")
            .to_lowercase();
        if MENTION_NOUNS.contains(&first.as_str()) {
            return None;
        }
    }
    if qualifier.is_some() && !separated && rest.chars().all(|c| c == '.') {
        rest = "";
    }
    match qualifier {
        Some(q) if rest.is_empty() && says_none(q) => Some(q),
        Some(q) if rest.is_empty() && !separated && q.split_whitespace().count() >= 3 => Some(q),
        _ => Some(rest),
    }
}

fn past_heading(t: &str) -> (&str, bool) {
    let hashes = t.bytes().take_while(|b| *b == b'#').count();
    match t.get(hashes..) {
        Some(rest) if (1..=6).contains(&hashes) && rest.starts_with([' ', '\t']) => {
            (rest.trim_start(), true)
        }
        _ => (t, false),
    }
}

fn head_words(head: &str) -> &'static [&'static str] {
    if head == NOTHING_LEFT {
        &[" to do", " to land"]
    } else {
        &[]
    }
}

fn past_separator(rest: &str) -> Option<&str> {
    rest.strip_prefix(':')
        .or_else(|| rest.strip_prefix('—'))
        .or_else(|| rest.strip_prefix('–'))
        .or_else(|| {
            rest.strip_prefix('-')
                .filter(|r| r.is_empty() || r.starts_with(char::is_whitespace))
        })
}

fn head_rest<'a>(line: &'a str, head: &str) -> Option<&'a str> {
    let t = line.trim_start();
    if t.starts_with(QUOTE_MARKS) {
        return None;
    }
    let (t, heading) = past_heading(t);
    let t = past_marker(t).unwrap_or(t);
    let mark = ["**", "__", "*", "_"]
        .into_iter()
        .find(|m| t.starts_with(m))
        .unwrap_or("");
    let t = &t[mark.len()..];
    let name = head.trim_end_matches(':');
    let said = t.get(..name.len())?;
    if !said.eq_ignore_ascii_case(name) {
        return None;
    }
    let mut rest = &t[name.len()..];
    for words in head_words(head) {
        if let Some(r) = rest
            .get(..words.len())
            .filter(|w| w.eq_ignore_ascii_case(words))
            .map(|_| &rest[words.len()..])
        {
            rest = r;
            break;
        }
    }
    let mut rest = rest.trim_start();
    let mut closed = mark.is_empty();
    if !closed && let Some(r) = rest.strip_prefix(mark) {
        rest = r.trim_start();
        closed = true;
    }
    rest = match past_separator(rest) {
        Some(r) => r,
        None if heading && rest.trim().is_empty() => return Some(""),
        None => return None,
    };
    if !closed && let Some(r) = rest.strip_prefix(mark) {
        rest = r;
        closed = true;
    }
    let mut rest = rest.trim();
    if !closed {
        rest = rest.strip_suffix(mark).unwrap_or(rest).trim_end();
    }
    Some(rest)
}

/// A Markdown code-fence run and the text after it.
pub(super) fn code_fence(line: &str) -> Option<(u8, usize, &str)> {
    let line = line.trim();
    let mark = *line.as_bytes().first()?;
    if !matches!(mark, b'`' | b'~') {
        return None;
    }
    let width = line.bytes().take_while(|&b| b == mark).count();
    (width >= 3).then(|| (mark, width, &line[width..]))
}

fn fence(line: &str) -> bool {
    code_fence(line).is_some()
}

/// Whether `line` belongs to a fenced block, including its delimiter rows.
pub(super) fn fenced_line(line: &str, open: &mut Option<(u8, usize)>) -> bool {
    if let Some((mark, width)) = *open {
        if code_fence(line)
            .is_some_and(|(m, n, rest)| m == mark && n >= width && rest.trim().is_empty())
        {
            *open = None;
        }
        return true;
    }
    if let Some((mark, width, _)) = code_fence(line) {
        *open = Some((mark, width));
        return true;
    }
    false
}

fn clause_words(clause: &str) -> Vec<&str> {
    clause
        .split(|c: char| !(c.is_alphanumeric() || matches!(c, '/' | '\'' | '’' | '-')))
        .map(|w| w.trim_matches(|c: char| matches!(c, '/' | '\'' | '’' | '-')))
        .filter(|w| !w.is_empty())
        .collect()
}

fn none_clause(words: &[&str], by: &NoneWords<'_>) -> bool {
    let (head, rest) = match words {
        ["not", w, rest @ ..] if by.not_needed.contains(w) => ("not", rest),
        [w, rest @ ..] if by.heads.contains(w) => (*w, rest),
        _ => return false,
    };
    if rest
        .windows(2)
        .any(|p| by.joins.contains(&p[0]) && by.asked.contains(&p[1]))
    {
        return false;
    }
    if !rest.is_empty() && rest.iter().all(|w| by.still.contains(w)) {
        return true;
    }
    let rest = match rest.iter().position(|w| by.needed.contains(w)) {
        Some(k)
            if k <= NONE_OBJECT_WORDS
                && !rest[..k].iter().any(|w| by.keeps.contains(w))
                && (by.objects.contains(&head)
                    || rest[..k].iter().all(|w| by.mods.contains(w))) =>
        {
            &rest[k + 1..]
        }
        _ => rest,
    };
    rest.iter().all(|w| by.mods.contains(w))
}

fn you_set(words: &[&str], by: &NoneWords<'_>) -> bool {
    let rest = match words {
        ["you're" | "you’re", rest @ ..] | ["you", "are", rest @ ..] => rest,
        _ => return false,
    };
    !rest.is_empty() && rest.iter().all(|w| by.set.contains(w))
}

fn restated(words: &[&str], by: &NoneWords<'_>) -> bool {
    let mut k = 0;
    while let Some(w) = words.get(k) {
        let named = by.named.contains(w) && k > 0 && by.the.contains(&words[k - 1]);
        if !(by.subjects.contains(w) || named || w.bytes().all(|b| b.is_ascii_digit())) {
            break;
        }
        k += 1;
    }
    while words.get(k).is_some_and(|w| by.links.contains(w)) {
        k += 1;
    }
    let linked = words[..k].iter().any(|w| by.links.contains(w));
    let place = |at: usize| {
        let end = at
            + 1
            + words[at + 1..]
                .iter()
                .take_while(|w| by.places.contains(w))
                .count();
        (end > at + 1).then_some(end)
    };
    match words.get(k) {
        Some(w) if by.landed.contains(w) => k += 1,
        Some(w) if linked && by.at.contains(w) => match place(k) {
            Some(end) => k = end,
            None => return false,
        },
        _ => return false,
    }
    while let Some(w) = words.get(k) {
        if *w == "and" && words.get(k + 1).is_some_and(|w| by.landed.contains(w)) {
            k += 2;
        } else if by.at.contains(w) {
            match place(k) {
                Some(end) => k = end,
                None => return false,
            }
        } else if by.times.contains(w) {
            k += 1;
        } else {
            return false;
        }
    }
    true
}

fn says_none(rest: &str) -> bool {
    none_said(rest, &NONE_WORDS).is_some()
}

fn none_said(rest: &str, by: &NoneWords<'_>) -> Option<String> {
    let cleaned: String = rest.chars().filter(|c| *c != '`').collect();
    let cleaned = cleaned.trim().to_string();
    let lower = cleaned.to_lowercase();
    let mut lower_chars = lower.chars();
    let same_offsets = cleaned.chars().all(|c| {
        lower_chars
            .next()
            .is_some_and(|lower| c.len_utf8() == lower.len_utf8())
    }) && lower_chars.next().is_none();
    let written = if same_offsets {
        cleaned.as_str()
    } else {
        lower.as_str()
    };
    let body = lower.trim_start_matches(|c: char| {
        matches!(c, '—' | '–' | '-' | '*' | '_') || c.is_whitespace()
    });
    if body.is_empty() {
        return lower.contains(['—', '–', '-']).then(String::new);
    }
    let from = lower.len() - body.len();
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let mut start = from;
    for (at, c) in lower[from..].char_indices().map(|(k, c)| (k + from, c)) {
        let spaced_dash = c == '-'
            && lower[..at].ends_with(' ')
            && lower[at..].trim_start_matches('-').starts_with(' ');
        if NONE_CLAUSE_MARKS.contains(&c) || spaced_dash {
            spans.push((start, at));
            start = at + c.len_utf8();
        }
    }
    spans.push((start, lower.len()));
    let mut clauses = spans
        .into_iter()
        .map(|(a, b)| (b, clause_words(&lower[a..b])))
        .filter(|(_, words)| !words.is_empty());
    let (after, first) = clauses.next()?;
    if !none_clause(&first, by) {
        return None;
    }
    let mut restates = false;
    let bare = first.len() == 1 || first[0] == "not" && first.len() == 2;
    for (_, words) in clauses {
        let timed = words
            .iter()
            .all(|w| by.times.contains(w) || bare && by.still.contains(w));
        if none_clause(&words, by) || timed || you_set(&words, by) {
            continue;
        }
        if !restated(&words, by) {
            return None;
        }
        restates = true;
    }
    if !restates {
        return Some(String::new());
    }
    let clause = written[after..].trim_start_matches(|c: char| !c.is_alphanumeric());
    let clause =
        clause.trim_end_matches(|c: char| matches!(c, ')' | ']' | '*' | '_') || c.is_whitespace());
    Some(clause.to_string())
}

/// The NEEDS YOU lines of a report, read once ([`needs_lines`]).
#[derive(Default)]
struct NeedLines {
    /// Which lines of the tail are needs (heads, their items, their wrapped
    /// rows), by index.
    lines: Vec<usize>,
    /// The lines a head opens past their start ([`mid_head`]: `Nothing
    /// left: all on origin/main. NEEDS YOU: none`), by index, each with
    /// where its head starts: the words before it stay every other
    /// reader's.
    cuts: Vec<(usize, usize)>,
    /// Each need's words as written, wrapped rows joined.
    needs: Vec<String>,
    /// A line said none stands ([`none_said`]).
    cleared: bool,
    /// The clauses none lines said after their none (`nothing — everything
    /// is on origin/main`: [`none_said`]), by the index of the line that
    /// keeps each in [`without_needs`]: no need, but words the readers of
    /// open work read.
    clauses: Vec<(usize, String)>,
    /// In the go-ahead reader's view ([`go_ahead_view`]), the rows a need took only there
    /// ([`go_ahead_wrap`]), and the rows it took after them, each run with the need's words joined
    /// and before it ([`Moved`]); every other reader's view takes none.
    moved: Vec<Moved>,
}

impl NeedLines {
    fn none_at(&mut self, at: usize, clause: String) {
        self.cleared = true;
        if !clause.is_empty() {
            self.clauses.push((at, clause));
        }
    }

    fn said(&mut self, at: usize, words: &str, by: &NoneWords<'_>) {
        match none_said(words, by) {
            Some(clause) => self.none_at(at, clause),
            None => self.needs.push(words.to_string()),
        }
    }
}

fn a_head(line: &str) -> bool {
    need_rest(line).is_some() || defense_rest(line).is_some()
}

fn paragraph(under: &str) -> bool {
    let t = unquoted(under).trim_end();
    !t.is_empty() && !fence(t) && !a_head(under)
}

/// Whether the row `next` continues `need`, a row the screen wrapped: the two are not [`apart`]
/// and the row opens lowercase or with a digit. THE IRREVERSIBLE GUARD'S CUT ([`without_needs`])
/// and every reader of the needs join a wrapped row so and only so: 246d1ddc2f's rule, which no
/// go-ahead code reads into or widens. Only the go-ahead reader's own view ([`go_ahead_view`])
/// takes more ([`go_ahead_wrap`]).
fn wraps(need: &str, next: &str) -> bool {
    !apart(need, next) && opens_lower(unquoted(next))
}

/// Whether `t` opens on a lowercase letter or a digit.
fn opens_lower(t: &str) -> bool {
    t.chars()
        .next()
        .is_some_and(|c| c.is_lowercase() || c.is_ascii_digit())
}

/// Whether row `next` opens a line of its own under the line `line`, whatever its first letter
/// and the room the row above it left: a blank row, a fence, a list item, a need's or a defense's
/// head, or any row under a line that ends a sentence or opens a list. A screen's reader joins a
/// row the pane's width wrapped only where this says it may ([`aterm_phase::reader::Apart`]).
pub(crate) fn apart(line: &str, next: &str) -> bool {
    let t = unquoted(next).trim_end();
    t.is_empty()
        || fence(t)
        || past_marker(t).is_some()
        || a_head(next)
        || line.trim_end().ends_with(['.', '!', '?', ':'])
}

/// How a need takes the row under it ([`needs_lines_by`]): `None` where it does not, `Some(false)`
/// where it [`wraps`] it, `Some(true)` where it takes it only in the go-ahead reader's own view.
type Join = fn(&str, &str) -> Option<bool>;

/// The join of every reader but the go-ahead reader's own view: [`wraps`] alone.
fn wrap_join(need: &str, next: &str) -> Option<bool> {
    wraps(need, next).then_some(false)
}

/// THE GO-AHEAD READER'S JOIN ([`go_ahead_view`]): `Some(false)` where the row [`wraps`] the
/// need, and `Some(true)` where, the two not [`apart`], it joins only in that view: it opens
/// lowercase past an opening quote or bracket (`… then say` above `"rerun the checks" so …`), or
/// the need stops mid-clause (`… say "rerun the checks" so` above `I can build …`). No reader but
/// [`go_ahead_view`] passes it.
fn go_ahead_wrap(need: &str, next: &str) -> Option<bool> {
    if wraps(need, next) {
        return Some(false);
    }
    (!apart(need, next)
        && (opens_lower(unquoted(next).trim_start_matches(['"', '\'', '`', '(', '“', '‘']))
            || mid_clause(need)))
    .then_some(true)
}

/// Joins to `need` the rows from `i` on that `join` takes, each pushed to `out.lines`; the rows
/// from the first that joins only in the go-ahead reader's view on are booked in `out.moved`,
/// with the need's words before that row. Returns the row after the last joined.
fn join_wrapped(
    rows: &[&str],
    mut i: usize,
    need: &mut String,
    join: Join,
    out: &mut NeedLines,
) -> usize {
    let mut moved = Vec::new();
    let mut base = None;
    while let Some(late) = rows.get(i).and_then(|next| join(need, next)) {
        if late && moved.is_empty() {
            base = Some(need.clone());
        }
        need.push(' ');
        need.push_str(unquoted(rows[i]).trim_end());
        out.lines.push(i);
        if late || !moved.is_empty() {
            moved.push(i);
        }
        i += 1;
    }
    if let Some(base) = base {
        out.moved.push(Moved {
            need: need.clone(),
            base,
            rows: moved,
        });
    }
    i
}

/// Whether `need` stops mid-clause: on a comma, or on a word no sentence ends on.
fn mid_clause(need: &str) -> bool {
    const OPEN_ENDS: &[&str] = &[
        "so", "and", "or", "but", "then", "to", "the", "a", "an", "of", "for", "with", "which",
        "if", "when", "once", "until", "unless", "before", "after", "because", "since", "while",
        "whether", "can", "will", "would", "could", "should", "my", "your", "its", "our", "their",
        "is", "are", "was", "were", "be", "on", "in", "at", "into", "onto", "from", "by", "as",
        "than",
    ];
    let t = need.trim_end();
    t.ends_with(',')
        || t.rsplit(char::is_whitespace)
            .next()
            .is_some_and(|w| OPEN_ENDS.contains(&w.to_lowercase().as_str()))
}

const CUT_INSTRUCTIONS: [&str; 3] = [
    "and carry on.",
    "and carry on with everything else.",
    "and land the rest.",
];

fn cut_instruction(rest: &str, rows: &[&str]) -> bool {
    const CLOSERS: &[char] = &[')', ']', '"', '\'', '`', '”', '’', '*', '_'];
    let is_cut = |words: &str| {
        let words = words.trim_end_matches(CLOSERS);
        CUT_INSTRUCTIONS.iter().any(|inst| {
            words == *inst
                || words
                    .strip_prefix(inst)
                    .is_some_and(|after| after.trim_start_matches(CLOSERS).starts_with(' '))
        })
    };
    let mut words = rest.to_string();
    if is_cut(&words) {
        return true;
    }
    for row in rows {
        if !wraps(&words, row) {
            break;
        }
        words.push(' ');
        words.push_str(unquoted(row).trim_end());
        if is_cut(&words) {
            return true;
        }
    }
    false
}

const MID_OPENS: &[char] = &['*', '_', '`', '['];

const MID_QUOTES: &[char] = &['"', '“', '‘', '\''];

fn mid_head<'a>(row: &'a str, above: &str, below: &[&str]) -> Option<(usize, &'a str)> {
    if unquoted(row).starts_with('⎿') {
        return None;
    }
    row.match_indices("NEED").find_map(|(at, _)| {
        if row[..at]
            .chars()
            .next_back()
            .is_some_and(char::is_alphanumeric)
        {
            return None;
        }
        let (len, capitals) = needs_token(&row[at..])?;
        let marked = row[at + len..].trim_start_matches(['*', '_', '`', '"', '”', '’', '\'']);
        let after = marked.trim_start();
        let closed = after.starts_with([':', '—', '–', '(', '[', ',', '?', '!', ';', '…', '→'])
            || ["--", "->", "=>", "..."]
                .iter()
                .any(|p| after.starts_with(p))
            || (after.starts_with("- ") && marked.len() > after.len());
        if !capitals || !closed {
            return None;
        }
        let start = row[..at].trim_end_matches(MID_OPENS).len();
        if row[..at].matches('`').count() % 2 == 1 && !row[start..at].contains('`') {
            return None;
        }
        let lead = row[..start].trim_end_matches(MID_QUOTES);
        let quoted = lead.len() < start;
        let before = lead
            .trim_end()
            .rsplit(|c: char| !c.is_alphanumeric())
            .next()
            .unwrap_or("")
            .to_lowercase();
        if matches!(before.as_str(), "no" | "zero" | "without") {
            return None;
        }
        if names_a_head(&format!("{above} {lead}")) {
            return None;
        }
        let article = MENTION_ARTICLES.contains(&before.as_str());
        let cut = lead.trim_end().len();
        let own =
            lead.trim_end().is_empty() || lead.trim_end().ends_with(['.', ';', '!', '?', '—', '–']);
        head_from(past_need_lead(&row[start..]), quoted || article)
            .filter(|rest| !cut_instruction(rest, below))
            .filter(|rest| own || rest.chars().any(char::is_alphanumeric))
            .map(|rest| (cut, rest))
    })
}

fn items_under(
    rows: &[&str],
    (head, mut i): (usize, usize),
    colon: bool,
    out: &mut NeedLines,
    by: &NoneWords<'_>,
) -> (usize, usize) {
    let indent = |r: &str| r.len() - r.trim_start().len();
    let sibling = past_marker(rows[head].trim_start()).is_some();
    let start = i;
    let mut items = 0;
    let mut blanks = Vec::new();
    while let Some(&under) = rows.get(i) {
        if colon && items == 0 && unquoted(under).is_empty() {
            blanks.push(i);
            i += 1;
            continue;
        }
        let Some(item) = past_marker(unquoted(under)) else {
            break;
        };
        if fence(under) || a_head(under) || (sibling && indent(under) <= indent(rows[head])) {
            break;
        }
        out.lines.append(&mut blanks);
        out.lines.push(i);
        out.said(i, item, by);
        items += 1;
        i += 1;
    }
    if items == 0 {
        return (start, 0);
    }
    (i, items)
}

/// The NEEDS YOU lines of `tail` as the irreversible guard and every reader of the needs read them,
/// a need's wrapped rows joined by [`wraps`] alone ([`wrap_join`]).
fn needs_lines(tail: &str) -> NeedLines {
    needs_lines_by(tail, &NONE_WORDS, wrap_join)
}

/// The NEEDS YOU lines of `tail`, a need's wrapped rows joined as `join` takes them: [`wrap_join`]
/// for every reader, [`go_ahead_wrap`] for the go-ahead reader's own view alone.
fn needs_lines_by(tail: &str, by: &NoneWords<'_>, join: Join) -> NeedLines {
    let rows: Vec<&str> = tail.lines().collect();
    let mut out = NeedLines::default();
    let mut block = None;
    let mut i = 0;
    while i < rows.len() {
        let row = rows[i];
        if fenced_line(row, &mut block) {
            i += 1;
            continue;
        }
        let above = if i > 0 { rows[i - 1] } else { "" };
        if names_a_head(above) && need_rest(row).is_some() {
            i += 1;
            continue;
        }
        if need_rest(row).is_some_and(|rest| cut_instruction(rest, &rows[i + 1..])) {
            i += 1;
            continue;
        }
        let head = i;
        let named_above = above
            .rsplit(|c: char| !c.is_alphanumeric())
            .find(|w| !w.is_empty())
            .is_some_and(|w| MENTION_ARTICLES.contains(&w.to_lowercase().as_str()))
            && above.trim_end().ends_with(char::is_alphanumeric);
        let opens = if named_above {
            head_from(past_need_lead(unquoted(row)), true)
        } else {
            need_rest(row)
        };
        let rest = match opens {
            Some(rest) => {
                out.lines.push(i);
                rest
            }
            None if named_above && need_rest(row).is_some() => {
                i += 1;
                continue;
            }
            None => match mid_head(row, above, &rows[i + 1..]) {
                Some((cut, rest)) => {
                    out.cuts.push((i, cut));
                    rest
                }
                None => {
                    i += 1;
                    continue;
                }
            },
        };
        i += 1;
        if rest.is_empty() {
            let mut items = 0;
            let mut blanks = Vec::new();
            while let Some(&under) = rows.get(i) {
                if unquoted(under).is_empty() && items == 0 {
                    blanks.push(i);
                    i += 1;
                    continue;
                }
                let Some(item) = past_marker(unquoted(under)) else {
                    break;
                };
                if a_head(under) {
                    break;
                }
                out.lines.append(&mut blanks);
                out.lines.push(i);
                out.said(i, item, by);
                items += 1;
                i += 1;
            }
            if items > 0 {
                continue;
            }
            match rows.get(i).filter(|under| paragraph(under)) {
                Some(&under) => {
                    out.lines.append(&mut blanks);
                    out.lines.push(i);
                    let at = i;
                    i += 1;
                    let mut need = unquoted(under).trim_end().to_string();
                    i = join_wrapped(&rows, i, &mut need, join, &mut out);
                    out.said(at, &need, by);
                }
                None => out.needs.push(UNREAD_NEED.to_string()),
            }
            continue;
        }
        let mut words = rest.to_string();
        i = join_wrapped(&rows, i, &mut words, join, &mut out);
        let colon = words.trim_end().ends_with(':');
        if colon {
            let (next, items) = items_under(&rows, (head, i), true, &mut out, by);
            if items == 0 {
                out.said(head, &words, by);
            }
            i = next;
        } else if none_said(&words, by).is_some() {
            out.said(head, &words, by);
            i = items_under(&rows, (head, i), false, &mut out, by).0;
        } else {
            out.said(head, &words, by);
        }
    }
    out
}

pub(super) fn sanitized(need: &str) -> String {
    let clean: String = need
        .chars()
        .map(|c| {
            if c.is_control() || aterm_messages::text::is_hidden_char(c) {
                ' '
            } else {
                c
            }
        })
        .collect();
    let words = clean.split_whitespace().collect::<Vec<_>>().join(" ");
    let words = words.trim_matches(|c: char| matches!(c, '*' | '_') || c.is_whitespace());
    if words.chars().count() <= NEED_CHARS {
        return words.to_string();
    }
    let mut cut: String = words.chars().take(NEED_CHARS - 1).collect();
    cut.truncate(cut.trim_end().len());
    cut.push('…');
    cut
}

/// What the report `tail` names only a person can give, each need one line.
#[must_use]
pub fn needs_you(tail: &str) -> Vec<String> {
    let read = needs_lines(tail);
    let mut out: Vec<String> = Vec::new();
    for need in &read.needs {
        let need = sanitized(need);
        if !need.is_empty() && !out.contains(&need) {
            out.push(need);
        }
    }
    if out.is_empty() && !read.cleared && !read.needs.is_empty() {
        out.push(UNREAD_NEED.to_string());
    }
    out
}

/// Whether `tail` says no need stands.
#[must_use]
pub fn says_no_needs(tail: &str) -> bool {
    let read = needs_lines(tail);
    read.cleared && read.needs.iter().all(|n| sanitized(n).is_empty())
}

/// THE AGENT'S LAST WORD ON NEEDS in a turn (v6 §14.17.13, the post-landing review): what the
/// turn's earlier messages `before` said still stands beside its last message `last` unless a
/// later word retracts it. A need stands until retracted, and a word that says none stands
/// ([`says_no_needs`], `NEEDS YOU: nothing`) retracts every need the turn named before it: a last
/// message that says it leaves `before` read for none (`None`), and an earlier paragraph that says
/// it alone leaves `before` from that paragraph on (its none, and what came after it). The
/// readers join a turn's messages with a blank line, so a message's own paragraphs are read as
/// messages are; one paragraph that names a need and a none both is read as itself, a kept need
/// ([`needs_you`]), and so is the last message, read whole. The one rule by which every reader of
/// a turn's needs reads it: the land check's answer (`land_check::land_check_answer`), the fold
/// that raises the needs on the menu bar (`TurnEndState::fold_turn_needs`), the readers of
/// `TurnEndReading::needs_before`, and the go-ahead reader ([`retracted_cut`]).
#[must_use]
pub fn standing_before<'a>(before: Option<&'a str>, last: &str) -> Option<&'a str> {
    let before = before?;
    match retracted_to(before, last) {
        Retracted::All => None,
        Retracted::From(at) => Some(&before[at..]),
    }
}

/// `before` WITHOUT THE NEEDS THE TURN'S LAST WORD RETRACTED ([`standing_before`]), its prose
/// kept: the go-ahead reader's view of a turn's earlier messages (`go_ahead::parked`), whose
/// sentences still say what the work plans and lacks, though a need they named was taken back.
#[must_use]
pub fn retracted_cut<'a>(before: &'a str, last: &str) -> Cow<'a, str> {
    match retracted_to(before, last) {
        Retracted::All => without_needs(before),
        Retracted::From(0) => Cow::Borrowed(before),
        Retracted::From(at) => Cow::Owned(format!(
            "{}\n\n{}",
            without_needs(&before[..at]).trim_end(),
            &before[at..]
        )),
    }
}

/// How much of a turn's earlier messages its last word retracts ([`standing_before`]).
enum Retracted {
    /// The last message says no need stands.
    All,
    /// The needs named before this byte: the start of the last paragraph that says none alone.
    From(usize),
}

fn retracted_to(before: &str, last: &str) -> Retracted {
    if says_no_needs(last) {
        return Retracted::All;
    }
    let mut paragraphs: Vec<(usize, usize)> = Vec::new();
    let mut open: Option<usize> = None;
    let mut at = 0;
    for line in before.split_inclusive('\n') {
        if line.trim().is_empty() {
            if let Some(start) = open.take() {
                paragraphs.push((start, at));
            }
        } else if open.is_none() {
            open = Some(at);
        }
        at += line.len();
    }
    if let Some(start) = open {
        paragraphs.push((start, at));
    }
    Retracted::From(
        paragraphs
            .iter()
            .rev()
            .find(|&&(start, end)| says_no_needs(&before[start..end]))
            .map_or(0, |&(start, _)| start),
    )
}

/// `tail` without its NEEDS YOU lines (heads, the items under a head, their wrapped rows).
#[must_use]
pub fn without_needs(tail: &str) -> Cow<'_, str> {
    cut_needs(tail, &needs_lines(tail))
}

/// THE GO-AHEAD READER'S OWN VIEW of the report `tail` (`go_ahead::parked`): its prose without
/// its NEEDS YOU lines, each need's words, and the rows a need took only here. A row the screen
/// wrapped after a need that stops mid-clause (`… say "rerun the checks" so` above `I can build
/// …`), or that opens lowercase past a quote mark, is the need's here ([`go_ahead_wrap`]), so its
/// reading does not hang on the tab's width. No other reader reads this view: the irreversible
/// guard and every reader of open work read [`without_needs`], as before the go-ahead reader.
pub(super) struct GoAheadView<'a> {
    /// `tail` without its needs, the mid-clause rows cut with them.
    pub prose: Cow<'a, str>,
    /// Each need's words as written, wrapped rows joined, before [`needs_you`] cleans and cuts
    /// it: the reader judges a need on all of its words.
    pub needs: Vec<String>,
    /// Each run of rows a need took only in this view: every other reader reads them as the
    /// reply's own prose.
    pub moved: Vec<Moved>,
}

/// A run of rows a need took only in the go-ahead reader's view ([`GoAheadView::moved`]).
pub(super) struct Moved {
    /// The need's words, the run joined.
    pub need: String,
    /// The need's words before the run: the need as every other reader joins it ([`wrap_join`]),
    /// and so the line [`needs_you`] names for it, once cleaned.
    pub base: String,
    /// The rows of `tail`, by index.
    pub rows: Vec<usize>,
}

/// Where the words of `row` start past its quote marks and indent: the row as a need's wrap
/// joins it is `row[quote_lead(row)..]`, its end trimmed.
pub(super) fn quote_lead(row: &str) -> usize {
    row.len() - unquoted(row).len()
}

/// [`GoAheadView`] of `tail`.
pub(super) fn go_ahead_view(tail: &str) -> GoAheadView<'_> {
    let read = needs_lines_by(tail, &NONE_WORDS, go_ahead_wrap);
    GoAheadView {
        prose: cut_needs(tail, &read),
        moved: read.moved,
        needs: read.needs,
    }
}

/// `tail` without the need lines `read` names.
fn cut_needs<'a>(tail: &'a str, read: &NeedLines) -> Cow<'a, str> {
    if read.lines.is_empty() && read.cuts.is_empty() {
        return Cow::Borrowed(tail);
    }
    let clause_at = |i: usize| {
        read.clauses
            .iter()
            .find(|(at, _)| *at == i)
            .map(|(_, clause)| capitalized(clause))
    };
    let kept: Vec<String> = tail
        .lines()
        .enumerate()
        .filter_map(|(i, l)| {
            if let Some(&(_, cut)) = read.cuts.iter().find(|(at, _)| *at == i) {
                let before = l[..cut].trim_end();
                return Some(match clause_at(i) {
                    Some(clause) => format!("{before} {clause}"),
                    None => before.to_string(),
                });
            }
            if !read.lines.contains(&i) {
                return Some(l.to_string());
            }
            clause_at(i)
        })
        .collect();
    Cow::Owned(kept.join("\n"))
}

fn capitalized(text: &str) -> String {
    let mut cs = text.chars();
    match cs.next() {
        Some(c) => c.to_uppercase().chain(cs).collect(),
        None => String::new(),
    }
}

const DEFENSE_EXCEPTIONS: &[&str] = &[
    "except for",
    "except",
    "excepting",
    "excluding",
    "besides",
    "but for",
    "but",
    "other than",
    "apart from",
    "aside from",
    "save for",
    "save",
    "barring",
    "bar",
    "beyond",
    "short of",
    "outside of",
    "minus",
];

fn excepted(rest: &str) -> Option<&str> {
    let rest = rest.trim_start_matches(['*', '_', ',']).trim_start();
    DEFENSE_EXCEPTIONS.iter().find_map(|w| {
        let after = rest
            .get(..w.len())
            .filter(|p| p.eq_ignore_ascii_case(w))
            .map(|_| &rest[w.len()..])?;
        if !after.starts_with(|c: char| c.is_whitespace() || c == ':') {
            return None;
        }
        let left = after
            .trim_start_matches(|c: char| c.is_whitespace() || c == ':')
            .trim_end();
        (!left.is_empty()).then_some(left)
    })
}

fn defense_rest(line: &str) -> Option<Cow<'_, str>> {
    let except = |left: &str| Cow::Owned(format!("except {left}"));
    if let Some(rest) = head_rest(line, NOTHING_LEFT) {
        return Some(excepted(rest).map_or(Cow::Borrowed(rest), except));
    }
    let t = line.trim_start();
    if t.starts_with(QUOTE_MARKS) {
        return None;
    }
    let t = past_heading(t).0;
    let t = past_marker(t).unwrap_or(t);
    let t = t.trim_start_matches(['*', '_']);
    let name = NOTHING_LEFT.trim_end_matches(':');
    if !t.get(..name.len())?.eq_ignore_ascii_case(name) {
        return None;
    }
    let mut rest = &t[name.len()..];
    for words in head_words(NOTHING_LEFT) {
        if let Some(r) = rest
            .get(..words.len())
            .filter(|w| w.eq_ignore_ascii_case(words))
            .map(|_| &rest[words.len()..])
        {
            rest = r;
            break;
        }
    }
    let rest = rest.trim_start_matches(['*', '_']);
    if !rest.starts_with(|c: char| c.is_whitespace() || c == ',') {
        return None;
    }
    excepted(rest).map(except)
}

/// The worker's defense line, trimmed, and the lines of `tail` after it: the one scan
/// [`defense_line`] and [`defense_and_after`] share.
fn defense_at(tail: &str) -> Option<(&str, std::str::Lines<'_>)> {
    let mut block = None;
    let mut lines = tail.lines();
    while let Some(line) = lines.next() {
        if fenced_line(line, &mut block) {
            continue;
        }
        if defense_rest(line).is_some() {
            return Some((line.trim(), lines));
        }
    }
    None
}

/// The worker's defense line.
#[must_use]
pub fn defense_line(tail: &str) -> Option<&str> {
    defense_at(tail).map(|(line, _)| line)
}

/// The worker's defense line ([`defense_line`]) and every non-blank line after it, each trimmed;
/// `None` where `tail` holds no defense (a landing's receipt, `harness::land_receipt`).
#[must_use]
pub fn defense_and_after(tail: &str) -> Option<Vec<&str>> {
    let (line, after) = defense_at(tail)?;
    let after = after.map(str::trim).filter(|l| !l.is_empty());
    Some(std::iter::once(line).chain(after).collect())
}

/// `tail` as Land's strict reading of open work takes it.
#[must_use]
pub fn defense_body(tail: &str) -> String {
    let rest = without_needs(tail);
    let mut block = None;
    let mut out: Vec<String> = Vec::new();
    for line in rest.lines() {
        if fenced_line(line, &mut block) {
            out.push(line.to_string());
            continue;
        }
        match defense_rest(line) {
            Some(body) => out.push(capitalized(&body)),
            None => out.push(line.to_string()),
        }
    }
    out.join("\n")
}

/// The key of a set of needs: equal sets, equal keys; `0` for none.
#[must_use]
pub fn needs_key(needs: &[String]) -> u64 {
    if needs.is_empty() {
        return 0;
    }
    aterm_hash::fnv1a_64(needs.join("\n").as_bytes()).max(1)
}

/// What a row of the needs table (`fixtures/needs-table.txt`) says its report must read as.
#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum NeedsRow {
    /// No need stands: no need named, and a clean defense lands.
    None,
    /// A need, carrying these words: named, and nothing lands.
    Need(String),
    /// An ask with no head, these words in it: no landing, and the nudge names what it read open.
    Headless(String),
}

/// The rows of the needs table.
#[cfg(test)]
pub(super) fn needs_table() -> Vec<(NeedsRow, String)> {
    include_str!("fixtures/needs-table.txt")
        .lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .map(|line| {
            let (label, report) = line
                .split_once(" || ")
                .unwrap_or_else(|| panic!("a row with no ` || `: {line:?}"));
            let row = match label.split_once(' ') {
                None if label == "NONE" => NeedsRow::None,
                Some(("NEED", words)) => NeedsRow::Need(words.to_string()),
                Some(("HEADLESS", words)) => NeedsRow::Headless(words.to_string()),
                _ => panic!("a row with no label: {line:?}"),
            };
            (row, report.replace("\\n", "\n"))
        })
        .collect()
}

#[cfg(test)]
#[path = "needs_tests.rs"]
mod tests;
