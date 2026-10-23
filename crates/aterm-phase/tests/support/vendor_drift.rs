// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared vendor-file readers and matchers for deterministic fixtures and the explicit vendor-drift
//! bench.

use crate::anchors::*;

/// Whether `needle` occurs in `hay`.
pub(crate) fn contains_bytes(hay: &[u8], needle: &[u8]) -> bool {
    find_bytes(hay, needle, 0).is_some()
}

/// Where `needle` first occurs in `hay` at or after `from` (the search
/// [`contains_bytes`] makes).
fn find_bytes(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    let n = needle.len();
    if n == 0 {
        return Some(from.min(hay.len()));
    }
    if hay.len() < n {
        return None;
    }
    let mut skip = [n; 256];
    for (i, &b) in needle[..n - 1].iter().enumerate() {
        skip[usize::from(b)] = n - 1 - i;
    }
    let mut at = from;
    while at + n <= hay.len() {
        if &hay[at..at + n] == needle {
            return Some(at);
        }
        at += skip[usize::from(hay[at + n - 1])];
    }
    None
}

/// In a template, the slot of a name the vendor's minifier chose (a
/// binding, a parameter): any one JavaScript identifier stands there. The
/// minifier renames them between builds that change no word (2.1.309's
/// `sr(i)` is 2.1.308's `sr(a)`), so a template that pins one names a
/// rename as a rewording. A private-use char, which no template's words
/// hold.
pub(crate) const JS_NAME: char = '\u{E000}';

/// Whether `b` may continue a JavaScript identifier (ASCII: the minifier's
/// names are).
fn js_name_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

/// The length of the identifier `bytes` open with, or 0. JavaScript reads
/// the longest one (`sa.` is the name `sa`, never `s` then `a.`).
fn js_name_len(bytes: &[u8]) -> usize {
    match bytes.first() {
        Some(&b) if js_name_char(b) && !b.is_ascii_digit() => {
            1 + bytes[1..].iter().take_while(|&&b| js_name_char(b)).count()
        }
        _ => 0,
    }
}

/// Whether `pieces` (a template split at its [`JS_NAME`] slots) stand at
/// `at` in `hay`: each piece byte for byte, one whole identifier between
/// each two.
fn pieces_at(hay: &[u8], at: usize, pieces: &[&str]) -> bool {
    let Some((first, rest)) = pieces.split_first() else {
        return true;
    };
    let end = at + first.len();
    if !hay.get(at..end).is_some_and(|b| b == first.as_bytes()) {
        return false;
    }
    if rest.is_empty() {
        return true;
    }
    let name = js_name_len(&hay[end..]);
    name > 0 && pieces_at(hay, end + name, rest)
}

/// Whether `bytes` hold `template` whole, each [`JS_NAME`] slot as any one
/// identifier and every other byte as written, at ANY place its first
/// piece occurs (the store bundle holds the limit names' switch head
/// `case"five_hour":` ten times on 2.1.308 and on 2.1.309, the switch the
/// ninth). A template opens with words, never a slot, and no slot touches
/// an identifier's char (the name would take it in).
pub(crate) fn holds(bytes: &[u8], template: &str) -> bool {
    let pieces: Vec<&str> = template.split(JS_NAME).collect();
    if pieces.len() == 1 {
        return contains(bytes, template);
    }
    let first = pieces[0].as_bytes();
    assert!(!first.is_empty(), "a template opens with a slot");
    let (before, after) = (&pieces[..pieces.len() - 1], &pieces[1..]);
    assert!(
        before
            .iter()
            .all(|p| !p.bytes().last().is_some_and(js_name_char))
            && after
                .iter()
                .all(|p| !p.bytes().next().is_some_and(js_name_char)),
        "a slot touches an identifier's char: {template:?}"
    );
    let mut from = 0;
    while let Some(at) = find_bytes(bytes, first, from) {
        if pieces_at(bytes, at, &pieces) {
            return true;
        }
        from = at + 1;
    }
    false
}

/// The version a `--version` line names.
pub(crate) fn version_in(text: &str) -> Option<String> {
    let dotted = |v: &str| {
        v.split('.').count() >= 2
            && v.split('.')
                .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
    };
    let words: Vec<&str> = text
        .split_whitespace()
        .map(|w| w.trim_matches(|c: char| matches!(c, '(' | ')' | ',')))
        .collect();
    words
        .iter()
        .find(|w| {
            w.split_once("-R")
                .is_some_and(|(v, rev)| dotted(v) && (dotted(rev) || rev.parse::<u64>().is_ok()))
        })
        .or_else(|| words.iter().find(|w| dotted(w)))
        .map(|w| (*w).to_string())
}

pub(crate) fn contains(bytes: &[u8], needle: &str) -> bool {
    contains_bytes(bytes, needle.as_bytes())
}

/// THE LIMIT RESET'S TEMPLATES, WHOLE, as the 2.1.304 bundle spells them.
pub(crate) fn reset_templates() -> Vec<(&'static str, Vec<String>)> {
    use crate::reset::ResetKind;
    let escaped = |id: &str| anchor(id).replace('·', "\\xB7");
    let limit_name = |limit_type: &str| {
        crate::reset::reset_limit_name(limit_type).expect("a window the confirmation names")
    };
    let (now, stays) = anchor("reset.confirm.stays")
        .split_once(" · ")
        .expect("the week's join has two sides");
    let left = anchor("reset.confirm.use_by")
        .split(" · ")
        .next()
        .expect("the detail's join opens with its word");
    let switch = |word: &dyn Fn(ResetKind) -> String| {
        let cases: String = [
            ResetKind::Session,
            ResetKind::Fable,
            ResetKind::Full,
            ResetKind::Weekly,
        ]
        .into_iter()
        .map(|kind| format!("case\"{}\":return\"{}\"", kind.word(), word(kind)))
        .collect::<Vec<_>>()
        .join(";");
        format!("{cases}}}")
    };
    let mut templates: Vec<(&'static str, Vec<String>)> = [
        (
            "grant row",
            format!(
                "{} {{kind}} ({{left}} {} {{date}})",
                anchor("reset.row"),
                anchor("reset.row.until")
            ),
        ),
        (
            "confirmation title",
            format!("{} {{kind}}?", anchor("reset.row")),
        ),
        (
            "confirmation yes",
            format!("{} {{kind}}", anchor("reset.yes")),
        ),
        (
            "confirmation body",
            format!(
                "{} {{limits}} {now} \\xB7 {{returns}} \\xB7 {stays} {{week}}",
                anchor("reset.confirm.refills")
            ),
        ),
        (
            "confirmation detail",
            format!(
                "{{kindResets}} {} {{deadline}}",
                escaped("reset.confirm.use_by")
            ),
        ),
        (
            "early line",
            format!(
                "{} {{kind}} {}",
                anchor("reset.early"),
                anchor("reset.early.anyway")
            ),
        ),
        ("kind names", switch(&|kind| kind.name().to_string())),
        (
            "five-hour count noun",
            format!("\"session\"?\"{}\":", anchor("reset.kind.session_count")),
        ),
        (
            "kind count grammar",
            "return s===0?`no ${d}s`:s===1?`1 ${d}`:`${s} ${d}s`}".to_string(),
        ),
        (
            "capitalised kind",
            "\"{Kind}\":n.kind===void 0?void 0:n.kind.charAt(0).toUpperCase()+n.kind.slice(1),"
                .to_string(),
        ),
        (
            "spent heads",
            switch(&|kind| kind.done().replace('·', "\\xB7")),
        ),
        (
            "spent line",
            format!("{{done}} \\xB7 {stays} {{week}} \\xB7 {{kindResets}} {left}"),
        ),
        (
            "still-walled line",
            format!(
                "{{Kind}} {} {{limit}} until {{date}} \\xB7 {{kindResets}} {left}",
                escaped("reset.walled")
            ),
        ),
        (
            "kept line",
            format!("{{Kind}} {} \\xB7 run /", anchor("reset.kept")),
        ),
        ("lifted line", escaped("reset.lifted")),
        ("login line", format!("{} \\xB7 ", anchor("reset.login"))),
        // The set and the reset are the minifier's names (`s` and `a` on 2.1.308, `i` on 2.1.309).
        (
            "limit names",
            format!(
                "case\"five_hour\":{n}.add(\"{}\");break;case\"seven_day\":{n}.add(\"{}\");break;\
                 case\"seven_day_overage_included\":{n}.add({n}.givesBackWeekly?\"Fable\":\"{}\");\
                 break;",
                limit_name("five_hour"),
                limit_name("seven_day"),
                limit_name("seven_day_overage_included"),
                n = JS_NAME,
            ),
        ),
        (
            "model limit names",
            format!(
                "seven_day_opus:\"{} limit\",seven_day_sonnet:\"{} limit\"",
                limit_name("seven_day_opus"),
                limit_name("seven_day_sonnet")
            ),
        ),
        (
            "limits join",
            "if(m===void 0)return\"limits\";return d.length===0?`${m} limit`:`${d.join(\", \")} \
             and ${m} limits`}"
                .to_string(),
        ),
    ]
    .into_iter()
    .map(|(name, text)| (name, vec![text]))
    .collect();
    const REPLACE: &str = ".replace(/ limit$/,\"\")";
    let call = |strip: &str| format!("{strip});break;case\"seven_day_sonnet\":");
    templates.extend([
        (
            "model limit names' strip",
            vec![call(REPLACE), call(",\" limit\")")],
        ),
        (
            "model limit names' strip helper",
            vec![
                REPLACE.to_string(),
                "{return t.endsWith(n)?t.slice(0,t.length-n.length):t}".to_string(),
            ],
        ),
    ]);
    templates
}

/// The names of the `templates` that `bytes` hold whole ([`holds`]) in none of their spellings.
pub(crate) fn missing_templates(
    bytes: &[u8],
    templates: &[(&'static str, Vec<String>)],
) -> Vec<&'static str> {
    templates
        .iter()
        .filter(|(_, spellings)| !spellings.iter().any(|t| holds(bytes, t)))
        .map(|(name, _)| *name)
        .collect()
}

/// A rollout key no Codex build or rollout holds: the key tests' negative control.
pub(crate) const PLANTED_ROLLOUT_KEY: &[Anchor] = &[Anchor {
    id: "rollout.planted",
    text: "turn_context_planted_by_no_build",
    since: "0.163.0",
    kind: AnchorKind::Literal,
}];
