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

#[cfg(test)]
#[path = "graph_tests.rs"]
mod tests;
