// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared vendor-file readers and matchers for deterministic fixtures and the explicit vendor-drift
//! bench.

use crate::anchors::*;

/// Whether `needle` occurs in `hay`.
pub(crate) fn contains_bytes(hay: &[u8], needle: &[u8]) -> bool {
    find_bytes(hay, needle, 0).is_some()
}

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

/// Private-use placeholder in a template matching any single JavaScript identifier.
pub(crate) const JS_NAME: char = '\u{E000}';

fn js_name_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

fn js_name_len(bytes: &[u8]) -> usize {
    match bytes.first() {
        Some(&b) if js_name_char(b) && !b.is_ascii_digit() => {
            1 + bytes[1..].iter().take_while(|&&b| js_name_char(b)).count()
        }
        _ => 0,
    }
}

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

/// Whether `bytes` contain `template` with each [`JS_NAME`] slot matched to a full identifier.
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

/// Where each word of `text` stands: a whole JavaScript name
/// ([`js_name_len`]) that opens with an ASCII letter (a minifier's name, a
/// key, a drawn word).
pub(crate) fn words(text: &str) -> Vec<std::ops::Range<usize>> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        if !bytes[at].is_ascii_alphabetic() || (at > 0 && js_name_char(bytes[at - 1])) {
            at += 1;
            continue;
        }
        let end = at + js_name_len(&bytes[at..]);
        out.push(at..end);
        at = end;
    }
    out
}

/// `text` with one word changed, once for each of its [`words`]: the word
/// gets a `Q` after it.
pub(crate) fn reworded(text: &str) -> Vec<String> {
    words(text)
        .into_iter()
        .map(|w| format!("{}Q{}", &text[..w.end], &text[w.end..]))
        .collect()
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

/// Limit-reset templates matched against vendor bundles.
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
    let spent =
        |done: &dyn Fn(ResetKind) -> &'static str| switch(&|kind| done(kind).replace('·', "\\xB7"));
    let before_2_1_312 = |kind: ResetKind| match kind {
        ResetKind::Session => crate::reset::SESSION_DONE_BEFORE_2_1_312,
        _ => kind.done(),
    };
    let names = |five_hour: &str| {
        format!(
            "case\"five_hour\":{n}.add(\"{five_hour}\");break;case\"seven_day\":{n}.add(\"{}\");\
             break;case\"seven_day_overage_included\":{n}.add({n}.givesBackWeekly?\"Fable\":\"{}\");\
             break;",
            limit_name("seven_day"),
            limit_name("seven_day_overage_included"),
            n = JS_NAME,
        )
    };
    templates.extend([
        (
            "spent heads",
            vec![spent(&before_2_1_312), spent(&ResetKind::done)],
        ),
        (
            "once-a-week row",
            vec![
                crate::reset::WEEKLY_ROW_BEFORE_2_1_312.to_string(),
                anchor("reset.weekly_row").to_string(),
            ],
        ),
        (
            "limit names",
            vec![
                names(crate::reset::FIVE_HOUR_LIMIT_BEFORE_2_1_312),
                names(limit_name("five_hour")),
            ],
        ),
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

/// The templates a composed anchor is drawn by, each spelling one build's: `effort.auto`, a
/// literal before 2.1.312, which 2.1.312 draws as `effort.auto_head` and `${E1()}`, the word
/// `"auto"` only while the flag beside it is compiled off
/// (`function Re(){return!1}function E1(){return Re()?"adaptive":"auto"}`).
pub(crate) fn composed_templates() -> Vec<(&'static str, Vec<String>)> {
    let (whole, head) = (anchor("effort.auto"), anchor("effort.auto_head"));
    let word = whole
        .strip_prefix(head)
        .expect("the auto notice opens with its head");
    vec![
        (
            "effort auto notice",
            vec![whole.to_string(), format!("{head}${{{JS_NAME}()}}")],
        ),
        (
            "effort auto word",
            vec![
                whole.to_string(),
                format!(
                    "{{return!1}}function {JS_NAME}(){{return {JS_NAME}()?\"adaptive\":\"{word}\"}}"
                ),
            ],
        ),
    ]
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
