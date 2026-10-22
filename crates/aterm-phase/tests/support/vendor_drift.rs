// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared vendor-file readers and matchers for deterministic fixtures and the
//! explicit vendor-drift bench. No vendor executable is started.

use crate::anchors::*;

/// Whether `needle` occurs in `hay` (Boyer-Moore-Horspool: a 225 MB binary
/// is scanned in a fraction of a second per needle, where a window-by-
/// window compare took most of a minute for the table).
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

/// The version a `--version` line names: a dotted one (`2.1.286 (Claude
/// Code)`, `codex-cli 0.130.0`, agy's bare `1.4.0`), or Muse Code's with
/// its revision, `x.y.z-R<n>.<s>` — `Muse Code 1.4.9 (1.4.9-R6297.1)`
/// names both, and the revision is the build its anchors were read on.
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

/// THE LIMIT RESET'S TEMPLATES, WHOLE, as the 2.1.304 bundle spells them
/// — a `·` as its `\xB7` escape, a value as its `{placeholder}` — each
/// built from the rows the readers match by (`crate::reset`) and named
/// by what it is: the grant row, the confirmation's title, yes, body
/// and detail, the early lines' tail, the answers a reader tells apart
/// (the spent line, the still-walled one, the kept line's head, the
/// lifted and login lines), the three switches of the bundle's that say
/// which kind each word is — `X()`'s `{kind}`, `ce()`'s count noun for
/// the five-hour kind, `Mt()`'s `{done}` (`crate::reset::ResetKind`
/// reads each word as that kind) — the two rules the readers read the
/// kinds' words back by: `ce()`'s count grammar (`no …s`, `1 …`, `n …s`)
/// and `Kg`'s `{Kind}` (the first letter upper-cased); and `Yn`'s
/// `{limits}`, the confirmation's body, which `reset_limit_name` and
/// `limits_of` read back: its limit names, the model limits' names it
/// strips ` limit` from (`jie`), and its `X limit` / `a, b and c limits`
/// join.
///
/// [`missing`] looks for each side of a `·` on its own, and the kinds'
/// names, the confirmation's two joins (`now`, `left`, `use by`) and the
/// heads `Use a`, `Yes, use my` and `kept` are words a build that
/// rewords the reset still carries elsewhere. A template is ONE literal
/// of the vendor's table (a switch, one function's body), so a reworded
/// one is missing — and what a rewording costs is every row, every
/// confirmation or every answer of the reset read as none of its own:
/// refused, the wall's one attempt with it, or a spent reset recorded as
/// failed.
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
    // A switch over the kinds' ids in the bundle's own order, each
    // returning `word(kind)`.
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
        // `ce()`'s grammar of `{kindResets}`, which `ResetKind::resets_of`
        // reads back (`no <noun>s`, `1 <noun>`, `<n> <noun>s`), and `Kg`'s
        // `{Kind}`, the kind's first letter upper-cased, which the kept
        // and still-walled lines are read by (`ResetKind::capital`):
        // byte for byte, the bundle's minified names and all, so a build
        // that rewords either, or only renames them, is named.
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
        // `Yn`'s `{limits}`: each window's name as `reset_limit_name`
        // reads it back (a Fable give-back's own `Fable` aside), the
        // model limits' names (`jie`'s, less ` limit`), and the join
        // `limits_of` splits.
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
    // `Yn`'s strip of ` limit` off the model limits' names, in either of the
    // bundle's spellings: 2.1.306 and older strip it at the call,
    // `.replace(/ limit$/,"")`; 2.1.307 calls a suffix helper,
    // `Iy(<name>," limit")`, whose body strips exactly that suffix. Each row
    // holds in a build that holds one of its spellings: the call row by the
    // strip at the call or the helper's argument there, the helper row by the
    // strip at the call or the helper's body (the helper's name is the
    // minifier's, so the two rows are not tied by it).
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

/// The names of the `templates` that `bytes` hold whole in none of their
/// spellings.
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

/// A rollout key no Codex build or rollout holds: the key tests'
/// negative control.
pub(crate) const PLANTED_ROLLOUT_KEY: &[Anchor] = &[Anchor {
    id: "rollout.planted",
    text: "turn_context_planted_by_no_build",
    since: "0.163.0",
    kind: AnchorKind::Literal,
}];
