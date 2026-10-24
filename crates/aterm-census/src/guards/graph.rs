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
mod tests {
    use super::super::tests::run;
    use super::*;

    const HEADER: &str = "// SPDX-License-Identifier: Apache-2.0\n";

    fn doors(src: &str) -> Vec<Finding> {
        run(&TEST_APPKIT_DOOR, &[("crates/aterm-gui/src/menu.rs", src)])
    }

    const DOOR_HELPER: &str = "fn add_separator(menu: &Obj) {
    unsafe {
        let sep = appkit::send_id(class(c\"NSMenuItem\").as_id(), sel!(separatorItem));
        appkit::send_v_id(menu.id(), sel!(addItem:), sep);
    }
}
";

    #[test]
    fn every_test_that_reaches_a_door_is_found_with_its_chain() {
        let cases: &[(&str, String, &str)] = &[
            (
                "a test calls the separator helper",
                format!(
                    "{DOOR_HELPER}#[cfg(test)]\nmod tests {{\n    use super::*;\n\n    #[test]\n    fn a_new_row() {{\n        add_separator(&new_menu().expect(\"NSMenu\"));\n    }}\n}}\n"
                ),
                "add_separator -> +[NSMenuItem separatorItem]",
            ),
            (
                "a brace in a string ends no test",
                format!(
                    "{DOOR_HELPER}#[cfg(test)]\nmod tests {{\n    #[test]\n    fn a_brace() {{\n        let label = \"}}\";\n        add_separator(&new_menu().expect(label));\n    }}\n}}\n"
                ),
                "add_separator -> +[NSMenuItem separatorItem]",
            ),
            (
                "an ignored test exempts nothing",
                format!(
                    "{DOOR_HELPER}fn build_section(e: &[&str]) -> Option<Obj> {{\n    let menu = new_menu()?;\n    add_separator(&menu);\n    Some(menu)\n}}\n#[cfg(test)]\nmod tests {{\n    #[test]\n    #[ignore = \"needs a Mac\"]\n    fn the_section() {{\n        let menu = build_section(&[\"one\"]).expect(\"menu\");\n    }}\n}}\n"
                ),
                "build_section -> add_separator -> +[NSMenuItem separatorItem]",
            ),
            (
                "an ignore above the test exempts nothing",
                "fn item_for_identifier(identifier: Id) -> Id {\n    unsafe {\n        appkit::send_id_id(\n            appkit::alloc(class(c\"NSToolbarItem\")),\n            sel!(initWithItemIdentifier:),\n            identifier,\n        )\n    }\n}\n#[cfg(test)]\nmod tests {\n    #[ignore = \"builds AppKit objects\"]\n    #[test]\n    fn the_item() {\n        let item = item_for_identifier(ident);\n    }\n}\n".to_string(),
                "item_for_identifier -> NSToolbarItem construction",
            ),
            (
                "a method call reaches the door",
                "impl Chip {\n    fn set_help(&self, help: &str) {\n        unsafe { appkit::send_v_id(self.0.id(), sel!(setToolTip:), s.id()) };\n    }\n}\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn a_chip() {\n        chip.set_help(\"New Tab\");\n    }\n}\n".to_string(),
                "set_help -> -setToolTip:",
            ),
            (
                "a test builds a toolbar item",
                "#[cfg(test)]\nmod tests {\n    #[test]\n    fn an_item() {\n        let item = unsafe {\n            appkit::send_id_id(\n                appkit::alloc(class(c\"NSToolbarItem\")),\n                sel!(initWithItemIdentifier:),\n            )\n        };\n    }\n}\n".to_string(),
                "NSToolbarItem construction, directly",
            ),
            (
                "a wrapper two hops up",
                "fn add_item_mods(menu: &Obj, title: &str, mods: usize) {\n    unsafe {\n        appkit::send_v_id(item.id(), sel!(setToolTip:), tip.id());\n    }\n}\n\nfn add_item(menu: &Obj, title: &str) {\n    add_item_mods(menu, title, 0);\n}\n\nfn build_section(entries: &[&str]) -> Option<Obj> {\n    for e in entries {\n        add_item(&menu, e);\n    }\n    Some(menu)\n}\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn rows() {\n        let menu = super::build_section(&[\"one\"]).expect(\"menu\");\n    }\n}\n".to_string(),
                "build_section -> add_item -> add_item_mods -> -setToolTip:",
            ),
            (
                "a test helper reaches the door",
                "fn stamp_native_tip(item: Id, action: MenuAction) {\n    unsafe { appkit::send_v_id(item, sel!(setToolTip:), tip.id()) };\n}\n#[cfg(test)]\nmod tests {\n    fn menu_with_tips(actions: &[MenuAction]) -> Vec<Id> {\n        for a in actions {\n            stamp_native_tip(x, *a);\n        }\n        items\n    }\n\n    #[test]\n    fn the_halt_pair() {\n        let items = menu_with_tips(&[MenuAction::HoldSession]);\n    }\n}\n".to_string(),
                "menu_with_tips -> stamp_native_tip -> -setToolTip:",
            ),
            (
                "a test sends the door itself",
                "#[cfg(test)]\nmod tests {\n    #[test]\n    fn shared() {\n        let a = unsafe { appkit::send_id(class(c\"NSMenuItem\").as_id(), sel!(separatorItem)) };\n    }\n}\n".to_string(),
                "+[NSMenuItem separatorItem], directly",
            ),
            (
                "a module's name exempts nothing",
                format!(
                    "{DOOR_HELPER}#[cfg(test)]\nmod tests {{\n    mod window_server {{\n        #[test]\n        fn a_row() {{\n            add_separator(&new_menu().expect(\"NSMenu\"));\n        }}\n    }}\n}}\n"
                ),
                "add_separator -> +[NSMenuItem separatorItem]",
            ),
        ];
        for (why, src, chain) in cases {
            let found = doors(&format!("{HEADER}{src}"));
            assert_eq!(found.len(), 1, "MISS or extra — {why}: {found:?}\n{src}");
            assert!(
                found[0].detail.ends_with(chain),
                "{why}: wrong chain {:?}, wanted {chain}",
                found[0].detail
            );
        }
    }

    #[test]
    fn production_doors_and_tests_that_touch_none_are_silent() {
        let src = format!(
            "{HEADER}{DOOR_HELPER}fn new_item(title: &str) -> Option<Obj> {{\n    unsafe {{ send(class(c\"NSMenuItem\"), sel!(initWithTitle:)) }}\n}}\n\
             fn build_section(e: &[&str]) -> Option<Obj> {{\n    add_separator(&menu);\n    Some(menu)\n}}\n\
             #[cfg(test)]\nmod tests {{\n    use super::*;\n\n    // add_separator(&menu) and sel!(setToolTip:) in prose\n\
             #[test]\n    fn plain() {{\n        let item = new_item(\"row\").expect(\"item\");\n        let why = \"add_separator( would; sel!(separatorItem) too\";\n    }}\n\n\
             #[test]\n    fn three_args() {{\n        let enc = unsafe {{ method_types(cls, sel!(toolbar:itemForItemIdentifier:willBeInsertedIntoToolbar:)) }};\n        let s: unsafe extern \"C\" fn(Id, Sel) -> Id = msg();\n    }}\n}}\n"
        );
        let found = doors(&src);
        assert!(found.is_empty(), "CRIES WOLF: {found:?}");
    }

    fn app_sends(files: &[(&str, &str)]) -> Vec<Finding> {
        run(&SHARED_APPLICATION, files)
    }

    fn unheld(src: &str) -> Vec<String> {
        app_sends(&[("crates/aterm-gui/src/status_item.rs", src)])
            .into_iter()
            .map(|f| f.detail)
            .collect()
    }

    const SEND: &str =
        "unsafe { appkit::send_id(class(c\"NSApplication\").as_id(), sel!(sharedApplication)) }";

    #[test]
    fn a_send_outside_the_windowed_functions_is_found() {
        let cases: &[(&str, &str, String, &str)] = &[
            (
                "a windowed function's name, without the proof",
                "crates/aterm-gui/src/status_item.rs",
                format!(
                    "pub(crate) fn install(p: &Proxy) -> Option<Item> {{\n    let app = {SEND};\n    None\n}}\n"
                ),
                "in `install`",
            ),
            (
                "chrome reads the application directly",
                "crates/aterm-gui/src/app_introspect.rs",
                format!(
                    "impl App {{\n    pub(crate) fn read_native_chrome(&self) -> Vec<String> {{\n        fn unshown(item: Id) -> bool {{\n            unsafe {{ send_bool(item, sel!(isHidden)) }}\n        }}\n        autoreleasepool(|_| {{\n            let app = {SEND};\n        }});\n        out\n    }}\n}}\n"
                ),
                "in `read_native_chrome`",
            ),
            (
                "a send after a nested fn",
                "crates/aterm-gui/src/hwkey.rs",
                format!(
                    "impl App {{\n    fn read_native_chrome(&self) -> Vec<String> {{\n        fn post() -> usize {{\n            0\n        }}\n        let app =\n            {SEND};\n        vec![]\n    }}\n}}\n"
                ),
                "in `read_native_chrome`",
            ),
            (
                "a send inside a nested fn",
                "crates/aterm-gui/src/menu.rs",
                format!(
                    "pub(crate) fn install(p: &Proxy) -> Option<Menu> {{\n    fn sneak() -> Id {{\n        {SEND}\n    }}\n    let _ = sneak();\n    None\n}}\n"
                ),
                "in `sneak`",
            ),
        ];
        for (why, path, src, expect) in cases {
            let found = app_sends(&[(path, src)]);
            assert_eq!(found.len(), 1, "MISS — {why}: {found:?}\n{src}");
            assert!(
                found[0].detail.contains(expect),
                "{why}: {:?}",
                found[0].detail
            );
        }
    }

    #[test]
    fn prose_and_strings_are_silent() {
        let prose = "impl App {\n    /// Never `sel!(sharedApplication)` here.\n    fn read_native_chrome(&self) -> Vec<String> {\n        // sel!(sharedApplication) in a trailing note\n        let why = \"sel!(sharedApplication) CREATES NSApp\";\n        vec![why.to_string()]\n    }\n}\n";
        let found = app_sends(&[("crates/aterm-gui/src/app_introspect.rs", prose)]);
        assert!(found.is_empty(), "CRIES WOLF: {found:?}");
    }

    #[test]
    fn a_proof_taken_or_answered_for_before_the_send_holds_it() {
        let held = format!(
            "pub(crate) fn status(w: &Windowed, p: &Proxy) -> Option<Item> {{\n    let app = {SEND};\n    None\n}}\n\
             fn by_value<F: Fn(u8) -> u8>(f: F, _w: crate::platform::Windowed) {{\n    let app = {SEND};\n}}\n\
             impl Windowed {{\n    pub(crate) fn app(&self) -> Id {{\n        {SEND}\n    }}\n}}\n\
             impl Copy for Windowed {{\n    fn moved(mut self) -> Id {{\n        {SEND}\n    }}\n}}\n\
             pub(crate) fn install(p: &Proxy) -> Option<Menu> {{\n    let _windowed = crate::platform::Windowed::get()?;\n    unsafe {{\n        let app = {SEND};\n    }}\n    None\n}}\n\
             fn stamp(item: Id) {{\n    let Some(_windowed) = crate::platform::Windowed::get() else {{\n        return;\n    }};\n    let app = {SEND};\n}}\n\
             fn activate(pid: i32) -> bool {{\n    if crate::platform::Windowed::get().is_none() {{\n        return false;\n    }}\n    autoreleasepool(|_| {{\n        let app = {SEND};\n    }});\n    true\n}}\n"
        );
        assert_eq!(unheld(&held), Vec::<String>::new());
    }

    #[test]
    fn a_proof_named_but_not_answered_for_holds_nothing() {
        let cases: &[(&str, String)] = &[
            (
                "a fetch dropped on the floor",
                format!(
                    "fn dropped() {{\n    let _ = Windowed::get();\n    let app = {SEND};\n}}\n"
                ),
            ),
            (
                "an assertion",
                format!(
                    "fn asserted() {{\n    debug_assert!(crate::platform::Windowed::get().is_some());\n    let app = {SEND};\n}}\n"
                ),
            ),
            (
                "an Option parameter",
                format!("fn optional(w: Option<Windowed>) {{\n    let app = {SEND};\n}}\n"),
            ),
            (
                "a returned proof",
                format!(
                    "fn minted() -> Option<Windowed> {{\n    let app = {SEND};\n    None\n}}\n"
                ),
            ),
            (
                "an associated fn of the proof with no self",
                format!("impl Windowed {{\n    fn bare() -> Id {{\n        {SEND}\n    }}\n}}\n"),
            ),
            (
                "a self method of another type taking the proof's name in a generic",
                format!(
                    "impl From<Windowed> for Menu {{\n    fn from(&self) -> Id {{\n        {SEND}\n    }}\n}}\n"
                ),
            ),
            (
                "an answer after the send",
                format!(
                    "fn early(p: &Proxy) -> bool {{\n    let app = {SEND};\n    if crate::platform::Windowed::get().is_none() {{\n        return false;\n    }}\n    true\n}}\n"
                ),
            ),
            (
                "an answer inside a block the send is not in",
                format!(
                    "fn branch(cond: bool) -> Option<()> {{\n    if cond {{\n        let _w = Windowed::get()?;\n    }}\n    let app = {SEND};\n    None\n}}\n"
                ),
            ),
            (
                "an is_none test that goes on",
                format!(
                    "fn goes_on(a: bool) {{\n    if a && Windowed::get().is_none() {{\n        return;\n    }}\n    if Windowed::get().is_none() {{\n        log();\n    }}\n    let app = {SEND};\n}}\n"
                ),
            ),
            (
                "an irrefutable let-else",
                format!(
                    "fn bound() {{\n    let _ = Windowed::get() else {{\n        return;\n    }};\n    let app = {SEND};\n}}\n"
                ),
            ),
            (
                "a lookalike type",
                format!(
                    "fn neighbour() {{\n    let _ = WindowedLike::get()?;\n    let app = {SEND};\n}}\n"
                ),
            ),
        ];
        for (why, src) in cases {
            let found = unheld(src);
            assert_eq!(found.len(), 1, "MISS — {why}: {found:?}\n{src}");
        }
    }
}
