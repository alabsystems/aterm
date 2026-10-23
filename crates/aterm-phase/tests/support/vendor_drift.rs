// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared vendor-file readers and matchers for deterministic fixtures and the explicit vendor-drift
//! bench.

use crate::anchors::*;

/// Whether `needle` occurs in `hay`.
pub(crate) fn contains_bytes(hay: &[u8], needle: &[u8]) -> bool {
    let n = needle.len();
    if n == 0 {
        return true;
    }
    if hay.len() < n {
        return false;
    }
    let mut skip = [n; 256];
    for (i, &b) in needle[..n - 1].iter().enumerate() {
        skip[usize::from(b)] = n - 1 - i;
    }
    let mut at = 0;
    while at + n <= hay.len() {
        if &hay[at..at + n] == needle {
            return true;
        }
        at += skip[usize::from(hay[at + n - 1])];
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
        (
            "limit names",
            format!(
                "case\"five_hour\":s.add(\"{}\");break;case\"seven_day\":s.add(\"{}\");break;\
                 case\"seven_day_overage_included\":s.add(a.givesBackWeekly?\"Fable\":\"{}\");\
                 break;",
                limit_name("five_hour"),
                limit_name("seven_day"),
                limit_name("seven_day_overage_included")
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

/// The names of the `templates` that `bytes` hold whole in none of their spellings.
pub(crate) fn missing_templates(
    bytes: &[u8],
    templates: &[(&'static str, Vec<String>)],
) -> Vec<&'static str> {
    templates
        .iter()
        .filter(|(_, spellings)| !spellings.iter().any(|t| contains(bytes, t)))
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
