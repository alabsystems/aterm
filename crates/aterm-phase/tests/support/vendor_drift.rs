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

/// Which of `needles` occur in `hay`, scanned in a single pass.
pub(crate) fn found_in(hay: &[u8], needles: &[&[u8]]) -> Vec<bool> {
    const HEAD: usize = 3;
    let mut found: Vec<bool> = needles.iter().map(|n| n.is_empty()).collect();
    let mut heads = vec![0u16; 1 << (8 * HEAD)];
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut left = 0usize;
    for (i, needle) in needles.iter().enumerate() {
        if needle.is_empty() {
            continue;
        }
        if needle.len() < HEAD {
            found[i] = hay.windows(needle.len()).any(|w| w == *needle);
            continue;
        }
        let h = needle[..HEAD]
            .iter()
            .fold(0, |h, &b| (h << 8) | usize::from(b));
        if heads[h] == 0 {
            groups.push(Vec::new());
            heads[h] = u16::try_from(groups.len()).expect("fewer than 65,536 heads");
        }
        groups[usize::from(heads[h]) - 1].push(i);
        left += 1;
    }
    let mut window = 0usize;
    for (end, &b) in hay.iter().enumerate() {
        if left == 0 {
            break;
        }
        window = ((window << 8) | usize::from(b)) & ((1 << (8 * HEAD)) - 1);
        let group = heads[window];
        if group == 0 || end + 1 < HEAD {
            continue;
        }
        let at = &hay[end + 1 - HEAD..];
        for &i in &groups[usize::from(group) - 1] {
            if !found[i] && at.starts_with(needles[i]) {
                found[i] = true;
                left -= 1;
            }
        }
    }
    found
}

/// The literal anchors of `table` missing from `bytes`, each part looked for as the screen spells it
/// and as a bundle's source does ([`js_escaped`]).
pub(crate) fn missing(bytes: &[u8], table: &[Anchor]) -> Vec<&'static str> {
    let literal: Vec<&Anchor> = table
        .iter()
        .filter(|a| a.kind == AnchorKind::Literal)
        .collect();
    let parts: Vec<&str> = literal.iter().flat_map(|a| a.text.split(" · ")).collect();
    let escaped: Vec<String> = parts.iter().map(|part| js_escaped(part)).collect();
    let needles: Vec<&[u8]> = parts
        .iter()
        .map(|part| part.as_bytes())
        .chain(escaped.iter().map(String::as_bytes))
        .collect();
    let found = found_in(bytes, &needles);
    let (plain, spelled) = found.split_at(parts.len());
    let mut held = plain.iter().zip(spelled).map(|(a, b)| *a || *b);
    literal
        .into_iter()
        .filter(|a| {
            a.text
                .split(" · ")
                .filter(|_| !held.next().expect("an answer per part"))
                .count()
                > 0
        })
        .map(|a| a.id)
        .collect()
}

/// How a bundled program's source spells `text`: a Latin-1 character as JavaScript's `\xHH`, and one
/// past it as `\uXXXX`, one per UTF-16 unit. Claude Code's `Opened changes in … ⧉` title is held this
/// way: the glyph's UTF-8 bytes are nowhere in its bundle.
pub(crate) fn js_escaped(text: &str) -> String {
    text.chars()
        .map(|c| match u32::from(c) {
            0..=0x7f => c.to_string(),
            latin1 @ 0x80..=0xff => format!("\\x{latin1:02X}"),
            _ => c
                .encode_utf16(&mut [0; 2])
                .iter()
                .map(|unit| format!("\\u{unit:04X}"))
                .collect(),
        })
        .collect()
}

/// The file ranges of a thin 64-bit Mach-O's sections of nothing but instructions. `None` for any
/// other file, and for one whose load commands or code sections do not fit where the file says.
fn code_ranges(bytes: &[u8]) -> Option<Vec<std::ops::Range<usize>>> {
    const MH_MAGIC_64: u32 = 0xfeed_facf;
    const LC_SEGMENT_64: u32 = 0x19;
    const S_ATTR_PURE_INSTRUCTIONS: u32 = 0x8000_0000;
    const HEADER: usize = 32;
    const COMMAND: usize = 8;
    const SEGMENT: usize = 72;
    const SECTION: usize = 80;
    let u32_at = |at: usize| Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?));
    let u64_at = |at: usize| Some(u64::from_le_bytes(bytes.get(at..at + 8)?.try_into().ok()?));
    if u32_at(0)? != MH_MAGIC_64 {
        return None;
    }
    let commands_end = HEADER.checked_add(usize::try_from(u32_at(20)?).ok()?)?;
    if commands_end > bytes.len() {
        return None;
    }
    let mut code = Vec::new();
    let mut command = HEADER;
    for _ in 0..u32_at(16)? {
        let size = usize::try_from(u32_at(command + 4)?).ok()?;
        let next = command.checked_add(size)?;
        if size < COMMAND || next > commands_end {
            return None;
        }
        if u32_at(command)? == LC_SEGMENT_64 {
            let sections = usize::try_from(u32_at(command + 64)?).ok()?;
            if SEGMENT.checked_add(sections.checked_mul(SECTION)?)? > size {
                return None;
            }
            for index in 0..sections {
                let section = command + SEGMENT + index * SECTION;
                if u32_at(section + 64)? & S_ATTR_PURE_INSTRUCTIONS != 0 {
                    let start = usize::try_from(u32_at(section + 48)?).ok()?;
                    let length = usize::try_from(u64_at(section + 40)?).ok()?;
                    let range = start..start.checked_add(length)?;
                    bytes.get(range.clone())?;
                    code.push(range);
                }
            }
        }
        command = next;
    }
    Some(code)
}

/// Zeroes a thin 64-bit Mach-O's instructions, so that a search of `bytes` reads what the program
/// holds as data. A short anchor's bytes occur by chance in 100 MB of code: `⧉`'s three were inside
/// ARM64 instructions of Claude Code 2.1.316 and 2.1.317, whose text never held them, so its anchor
/// passed until 2.1.318's code moved. Any other file is left whole, its code still searched, and so
/// is one [`code_ranges`] cannot follow: a range it misread would turn a held anchor into a missing
/// one.
pub(crate) fn blank_code(bytes: &mut [u8]) {
    for range in code_ranges(bytes).unwrap_or_default() {
        bytes[range].fill(0);
    }
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
    let (ide_diff, mark) = (
        anchor("box.ide_diff"),
        js_escaped(anchor("box.ide_diff.mark")),
    );
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
        (
            "IDE diff title",
            vec![format!(
                "{ide_diff}${{{JS_NAME}({JS_NAME}.ideName??\"IDE\")}} {mark}"
            )],
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
