// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Lexical per-file call-graph guards.

use std::collections::{BTreeMap, VecDeque};

use super::{Bound, Finding, Line, Rule, SourceFile, Tree, calls, is_word};

/// One line of code, attributed to the innermost `fn` whose body holds it (or whose signature it
/// is), and whether that is a test's.
pub(super) struct FnLine<'a> {
    pub(super) line: Line<'a>,
    pub(super) func: Option<&'a str>,
    pub(super) in_test: bool,
}

pub(super) fn fn_name(code: &str) -> Option<&str> {
    super::hits(code, "fn", Bound::Word).find_map(|at| fn_name_at(&code[at..]))
}

pub(super) fn fn_name_at(s: &str) -> Option<&str> {
    let rest = s.strip_prefix("fn")?;
    let name = rest.trim_start_matches([' ', '\t']);
    if name.len() == rest.len() {
        return None;
    }
    let end = name.bytes().take_while(|&b| is_word(b)).count();
    (end > 0 && !name.as_bytes()[0].is_ascii_digit()).then(|| &name[..end])
}

/// Every line of `file`, attributed by brace depth.
pub(super) fn attribute(file: &SourceFile) -> Vec<FnLine<'_>> {
    struct Frame<'a> {
        name: &'a str,
        depth: i64,
        test: bool,
    }
    let mut out = Vec::new();
    let mut frames: Vec<Frame<'_>> = Vec::new();
    let mut depth = 0i64;
    let mut pending: Option<(&str, bool)> = None;
    let mut test_attr = false;
    for line in file.lines() {
        let code = line.code;
        if code.contains("#[test]") {
            test_attr = true;
        }
        if let Some(name) = fn_name(code) {
            pending = Some((name, test_attr));
            test_attr = false;
        }
        let in_test = pending.is_some_and(|(_, t)| t) || frames.iter().any(|f| f.test);
        let func = pending
            .map(|(name, _)| name)
            .or_else(|| frames.last().map(|f| f.name));
        out.push(FnLine {
            line,
            func,
            in_test,
        });
        let opens = code.matches('{').count() as i64;
        let closes = code.matches('}').count() as i64;
        if let Some((name, test)) = pending {
            if opens > 0 {
                frames.push(Frame { name, depth, test });
                pending = None;
            } else if code.trim_end().ends_with(';') {
                pending = None;
            }
        }
        depth += opens - closes;
        while frames.last().is_some_and(|f| depth <= f.depth) {
            frames.pop();
        }
    }
    out
}

fn sends(code: &str, selector: &str) -> bool {
    code.match_indices("sel!")
        .any(|(at, _)| sel_at(&code[at..], selector))
}

fn sel_at(code: &str, selector: &str) -> bool {
    code.strip_prefix("sel!")
        .and_then(|r| super::after_ws(r, "("))
        .and_then(|r| super::after_ws(r, selector))
        .and_then(|r| super::after_ws(r, ")"))
        .is_some()
}

fn builds_class(line: Line<'_>, name: &str) -> bool {
    line.code.match_indices("class").any(|(at, _)| {
        let Some(arg) = super::after_ws(&line.code[at + 5..], "(") else {
            return false;
        };
        let arg = arg.trim_start_matches([' ', '\t']);
        if !arg.starts_with('"') {
            return false;
        }
        let quote = line.code.len() - arg.len();
        line.text.get(quote + 1..).is_some_and(|lit| {
            lit.strip_prefix(name)
                .is_some_and(|rest| rest.starts_with('"'))
        })
    })
}

/// The breadth-first reach over `edges` from `starts`.
pub(super) fn reach<'a>(
    starts: impl IntoIterator<Item = &'a str>,
    edges: &BTreeMap<&'a str, Vec<&'a str>>,
) -> BTreeMap<&'a str, Option<&'a str>> {
    let mut from: BTreeMap<&str, Option<&str>> = BTreeMap::new();
    let mut queue = VecDeque::new();
    for start in starts {
        if from.insert(start, None).is_none() {
            queue.push_back(start);
        }
    }
    while let Some(at) = queue.pop_front() {
        for &next in edges.get(at).map(Vec::as_slice).unwrap_or(&[]) {
            if !from.contains_key(next) {
                from.insert(next, Some(at));
                queue.push_back(next);
            }
        }
    }
    from
}

fn walk<'a>(links: &BTreeMap<&'a str, Option<&'a str>>, start: &'a str) -> Vec<&'a str> {
    let mut path = vec![start];
    let mut at = start;
    while let Some(Some(next)) = links.get(at) {
        at = next;
        path.push(at);
    }
    path
}

pub(super) const TEST_APPKIT_DOOR: Rule = Rule {
    id: "test-appkit-door",
    why: "a unit test that opens a WindowServer connection makes WindowServer's main thread \
          preflight the test binary, which a large target dir made outrun its watchdog; \
          +[NSMenuItem separatorItem], -setToolTip: and an NSToolbarItem build are the measured \
          doors, so no #[test] (ignored or not) reaches one through its file's calls — a test \
          that needs WindowServer is an integration lane against a real aterm",
    scope: &[],
    check: test_appkit_door,
};

const DOOR_NEEDLES: &[&str] = &["separatorItem", "setToolTip:", "NSToolbarItem"];

fn appkit_door(line: Line<'_>) -> Option<&'static str> {
    if sends(line.code, "separatorItem") {
        Some("+[NSMenuItem separatorItem]")
    } else if sends(line.code, "setToolTip:") {
        Some("-setToolTip:")
    } else if builds_class(line, "NSToolbarItem") {
        Some("NSToolbarItem construction")
    } else {
        None
    }
}

fn test_appkit_door(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    for file in tree.corpus.under(&["crates/", "apps/"]) {
        if file.mentions(DOOR_NEEDLES) {
            out.extend(appkit_door_findings(file));
        }
    }
}

fn appkit_door_findings(file: &SourceFile) -> Vec<Finding> {
    let lines = attribute(file);
    let mut doors: BTreeMap<&str, &str> = BTreeMap::new();
    let mut door_order: Vec<&str> = Vec::new();
    let mut callers: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for l in &lines {
        let Some(func) = l.func else { continue };
        if let Some(door) = appkit_door(l.line)
            && !doors.contains_key(func)
        {
            doors.insert(func, door);
            door_order.push(func);
        }
        for callee in calls(l.line.code) {
            if callee != func {
                callers.entry(callee).or_default().push(func);
            }
        }
    }
    let hop = reach(door_order.iter().copied(), &callers);
    let chain = |f| {
        let mut path = walk(&hop, f);
        let door = path
            .last()
            .and_then(|g| doors.get(g))
            .copied()
            .unwrap_or("?");
        path.push(door);
        path.join(" -> ")
    };
    let mut out = Vec::new();
    for l in lines.iter().filter(|l| l.in_test) {
        let why = "a test reaches an AppKit call that opens a WindowServer connection";
        if let Some(door) = appkit_door(l.line) {
            out.push(file.finding(
                TEST_APPKIT_DOOR.id,
                l.line.no,
                format!("{why}: {door}, directly"),
            ));
            continue;
        }
        if let Some(callee) = calls(l.line.code)
            .into_iter()
            .find(|c| hop.contains_key(c) && Some(*c) != l.func)
        {
            out.push(file.finding(
                TEST_APPKIT_DOOR.id,
                l.line.no,
                format!("{why}: {}", chain(callee)),
            ));
        }
    }
    out
}

const WINDOWED_TOKEN: &str = "Windowed";

pub(super) const SHARED_APPLICATION: Rule = Rule {
    id: "shared-application",
    why: "+[NSApplication sharedApplication] creates the application object when none exists, \
          as in a headless process, and its -init checks in with LaunchServices and opens a \
          WindowServer connection; aterm-gui sends it only in a function that holds the \
          `Windowed` proof a windowed launch mints (takes it, or answers without it first)",
    scope: GUI_SOURCE,
    check: shared_application,
};

const GUI_SOURCE: &[&str] = &["crates/aterm-gui/src/", "crates/aterm-gui-"];

fn shared_application(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    for file in tree.corpus.under(GUI_SOURCE) {
        if !file.mentions(&["sharedApplication"]) {
            continue;
        }
        for send in shared_application_sends(&file.lexed().code) {
            if send.windowed {
                continue;
            }
            out.push(file.finding(
                SHARED_APPLICATION.id,
                send.line,
                format!(
                    "sends +[NSApplication sharedApplication] in `{}`, which does not hold the \
                     `{WINDOWED_TOKEN}` proof",
                    send.func.unwrap_or("-")
                ),
            ));
        }
    }
}

#[derive(Debug)]
struct AppSend<'a> {
    line: usize,
    func: Option<&'a str>,
    windowed: bool,
}

fn shared_application_sends(code: &str) -> Vec<AppSend<'_>> {
    struct Frame<'a> {
        func: Option<&'a str>,
        depth: i64,
        impl_of_proof: bool,
        held_from: Option<i64>,
    }
    let b = code.as_bytes();
    let mut out = Vec::new();
    let mut frames: Vec<Frame<'_>> = Vec::new();
    let mut pending: Option<(Option<&str>, i64, i64, usize)> = None;
    let (mut depth, mut brackets, mut line) = (0i64, 0i64, 1usize);
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        let boundary = i == 0 || !is_word(b[i - 1]);
        if c == b'\n' {
            line += 1;
        } else if c == b'f'
            && boundary
            && let Some(name) = fn_name_at(&code[i..])
        {
            let gap = code[i + 2..].len() - code[i + 2..].trim_start_matches([' ', '\t']).len();
            pending = Some((Some(name), depth, brackets, i));
            i += 2 + gap + name.len();
            continue;
        } else if c == b'i'
            && boundary
            && pending.is_none()
            && code[i..]
                .strip_prefix("impl")
                .is_some_and(|r| r.starts_with([' ', '<', '\n']))
        {
            pending = Some((None, depth, brackets, i));
            i += 4;
            continue;
        } else if c == b'W' && boundary && pending.is_none() && answers_without_proof(code, i) {
            if let Some(frame) = frames.iter_mut().rev().find(|f| f.func.is_some()) {
                frame.held_from = Some(frame.held_from.map_or(depth, |d| d.min(depth)));
            }
            i += WINDOWED_TOKEN.len();
            continue;
        } else if c == b's' && boundary && sel_at(&code[i..], "sharedApplication") {
            let inner = frames.iter().rev().find(|f| f.func.is_some());
            out.push(AppSend {
                line,
                func: inner.and_then(|f| f.func),
                windowed: inner.is_some_and(|f| f.held_from.is_some()),
            });
        } else {
            match c {
                b'{' => {
                    depth += 1;
                    if let Some((func, _, _, start)) = pending.take() {
                        let header = &code[start..i];
                        let frame = if func.is_some() {
                            let in_proof_impl = frames
                                .last()
                                .is_some_and(|f| f.func.is_none() && f.impl_of_proof);
                            Frame {
                                func,
                                depth,
                                impl_of_proof: false,
                                held_from: takes_proof(header, in_proof_impl).then_some(depth),
                            }
                        } else {
                            Frame {
                                func,
                                depth,
                                impl_of_proof: impl_of_proof(header),
                                held_from: None,
                            }
                        };
                        frames.push(frame);
                    }
                }
                b'}' => {
                    for f in &mut frames {
                        if f.held_from == Some(depth) {
                            f.held_from = None;
                        }
                    }
                    if frames.last().is_some_and(|f| f.depth == depth) {
                        frames.pop();
                    }
                    depth -= 1;
                }
                b'[' => brackets += 1,
                b']' => brackets -= 1,
                b';' if pending.is_some_and(|(_, d, k, _)| d == depth && k == brackets) => {
                    pending = None;
                }
                _ => {}
            }
        }
        i += 1;
    }
    out
}

fn answers_without_proof(code: &str, at: usize) -> bool {
    let Some(rest) = code[at..]
        .strip_prefix(WINDOWED_TOKEN)
        .and_then(|r| r.strip_prefix("::get()"))
    else {
        return false;
    };
    let rest = rest.trim_start();
    if rest.starts_with('?') {
        return true;
    }
    let start = code[..at].rfind([';', '{', '}']).map_or(0, |p| p + 1);
    let lead = path_tail_trimmed(&code[start..at]).trim();
    if word_at(rest, "else") {
        return lead.starts_with("let Some(") && lead.ends_with('=');
    }
    rest.strip_prefix(".is_none()")
        .map(str::trim_start)
        .and_then(|r| r.strip_prefix('{'))
        .is_some_and(|block| lead == "if" && word_at(block.trim_start(), "return"))
}

fn path_tail_trimmed(s: &str) -> &str {
    let mut s = s.trim_end();
    while let Some(r) = s.strip_suffix("::") {
        s = r
            .trim_end()
            .trim_end_matches(|c: char| c.is_ascii_alphanumeric() || c == '_');
        s = s.trim_end();
    }
    s
}

fn word_at(s: &str, word: &str) -> bool {
    s.strip_prefix(word)
        .is_some_and(|r| !r.bytes().next().is_some_and(is_word))
}

fn takes_proof(header: &str, in_proof_impl: bool) -> bool {
    let Some(params) = param_list(header) else {
        return false;
    };
    top_level(params, b',')
        .into_iter()
        .enumerate()
        .any(|(n, param)| {
            (n == 0 && in_proof_impl && is_self_receiver(param))
                || top_level(param, b':')
                    .get(1)
                    .is_some_and(|ty| is_proof_type(ty))
        })
}

fn param_list(header: &str) -> Option<&str> {
    let b = header.as_bytes();
    let (mut angle, mut open) = (0i64, None);
    for (i, &c) in b.iter().enumerate() {
        match c {
            b'<' => angle += 1,
            b'>' if i == 0 || b[i - 1] != b'-' => angle -= 1,
            b'(' if angle == 0 => {
                open = Some(i + 1);
                break;
            }
            _ => {}
        }
    }
    let open = open?;
    let mut parens = 1i64;
    for (i, &c) in b.iter().enumerate().skip(open) {
        match c {
            b'(' => parens += 1,
            b')' => {
                parens -= 1;
                if parens == 0 {
                    return Some(&header[open..i]);
                }
            }
            _ => {}
        }
    }
    None
}

fn top_level(s: &str, sep: u8) -> Vec<&str> {
    let b = s.as_bytes();
    let (mut nest, mut from, mut out) = (0i64, 0, Vec::new());
    for (i, &c) in b.iter().enumerate() {
        match c {
            b'(' | b'[' | b'{' | b'<' => nest += 1,
            b'>' if i > 0 && b[i - 1] == b'-' => {}
            b')' | b']' | b'}' | b'>' => nest -= 1,
            _ if c == sep
                && nest == 0
                && !(sep == b':'
                    && (b.get(i + 1) == Some(&b':') || (i > 0 && b[i - 1] == b':'))) =>
            {
                out.push(&s[from..i]);
                from = i + 1;
            }
            _ => {}
        }
    }
    out.push(&s[from..]);
    out
}

fn is_self_receiver(param: &str) -> bool {
    let s = without_reference(param);
    s.strip_prefix("self")
        .is_some_and(|r| r.trim_start().is_empty() || r.trim_start().starts_with(':'))
}

fn without_reference(ty: &str) -> &str {
    let mut s = ty.trim_start();
    if let Some(r) = s.strip_prefix('&') {
        s = r.trim_start();
        if let Some(r) = s.strip_prefix('\'') {
            s = r
                .trim_start_matches(|c: char| c.is_ascii_alphanumeric() || c == '_')
                .trim_start();
        }
    }
    if let Some(r) = s.strip_prefix("mut")
        && r.starts_with(char::is_whitespace)
    {
        s = r.trim_start();
    }
    s.trim_end()
}

fn is_proof_type(ty: &str) -> bool {
    let t = without_reference(ty);
    !t.contains(['<', '[', '(']) && t.rsplit("::").next().map(str::trim) == Some(WINDOWED_TOKEN)
}

fn impl_of_proof(header: &str) -> bool {
    let mut h = header.strip_prefix("impl").unwrap_or(header).trim_start();
    if h.starts_with('<') {
        h = after_generics(h);
    }
    let h = super::hits(h, "where", Bound::Word)
        .next()
        .map_or(h, |at| &h[..at]);
    let self_ty = super::hits(h, "for", Bound::Word)
        .last()
        .map_or(h, |at| &h[at + 3..]);
    is_proof_type(self_ty)
}

fn after_generics(s: &str) -> &str {
    let b = s.as_bytes();
    let mut angle = 0i64;
    for (i, &c) in b.iter().enumerate() {
        match c {
            b'<' => angle += 1,
            b'>' if i > 0 && b[i - 1] == b'-' => {}
            b'>' => {
                angle -= 1;
                if angle == 0 {
                    return &s[i + 1..];
                }
            }
            _ => {}
        }
    }
    ""
}

#[cfg(test)]
#[path = "graph_tests.rs"]
mod tests;
