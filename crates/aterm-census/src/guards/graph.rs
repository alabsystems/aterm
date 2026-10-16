// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The call-graph guards: which function a line belongs to, whether it is a
//! test's, and what a function reaches through calls of its own file.
//!
//! Lexical and per file, by name: a call through another file's function, a
//! function pointer or selector dispatch is not followed. Each rule says what
//! that leaves to the runtime censuses.

use std::collections::{BTreeMap, VecDeque};

use super::{Bound, Finding, Line, Rule, SourceFile, Tree, calls, is_word};

/// One line of code, attributed to the innermost `fn` whose body holds it (or
/// whose signature it is), and whether that is a test's.
pub(super) struct FnLine<'a> {
    pub(super) line: Line<'a>,
    pub(super) func: Option<&'a str>,
    pub(super) in_test: bool,
}

/// The first `fn <name>` a line of code defines.
fn fn_name(code: &str) -> Option<&str> {
    super::hits(code, "fn", Bound::Word).find_map(|at| fn_name_at(&code[at..]))
}

/// The name a `fn` keyword at the start of `s` defines (`fn(` is a type).
fn fn_name_at(s: &str) -> Option<&str> {
    let rest = s.strip_prefix("fn")?;
    let name = rest.trim_start_matches([' ', '\t']);
    if name.len() == rest.len() {
        return None;
    }
    let end = name.bytes().take_while(|&b| is_word(b)).count();
    (end > 0 && !name.as_bytes()[0].is_ascii_digit()).then(|| &name[..end])
}

/// Every line of `file`, attributed by brace depth: a `fn` line opens a frame
/// at its first `{` (a `;` first ends a body-less declaration), and a frame
/// closes when the depth falls back to where it opened. A `#[test]` marks the
/// next `fn`, and every line inside a test frame is a test's.
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

/// `sel!(<selector>)`, spaces allowed, in a line of code.
fn sends(code: &str, selector: &str) -> bool {
    code.match_indices("sel!")
        .any(|(at, _)| sel_at(&code[at..], selector))
}

/// Does `code` start with `sel!(<selector>)`?
fn sel_at(code: &str, selector: &str) -> bool {
    code.strip_prefix("sel!")
        .and_then(|r| super::after_ws(r, "("))
        .and_then(|r| super::after_ws(r, selector))
        .and_then(|r| super::after_ws(r, ")"))
        .is_some()
}

/// `class(c"<name>")`: the code view shows the call and where its literal
/// sits (the `c` prefix blanked with the contents), the text view the name.
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

/// The breadth-first reach over `edges` from `starts`: each reached name and
/// the name it was reached from (`None` for a start).
fn reach<'a>(
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

/// `start`, then each name `links` leads to, until one leads nowhere.
fn walk<'a>(links: &BTreeMap<&'a str, Option<&'a str>>, start: &'a str) -> Vec<&'a str> {
    let mut path = vec![start];
    let mut at = start;
    while let Some(Some(next)) = links.get(at) {
        at = next;
        path.push(at);
    }
    path
}

// ---------------------------------------------------------------------------
// test-appkit-door: the AppKit calls measured opening a WindowServer
// connection from a test binary.
// ---------------------------------------------------------------------------

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

/// The door a line of code opens, by name.
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

/// Every test line of `file` that opens a door or calls a function of the
/// file that reaches one.
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
    // Toward the door: each function that reaches one, and the next hop.
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

// ---------------------------------------------------------------------------
// shared-application: +[NSApplication sharedApplication] CREATES the
// application object, and with it a WindowServer connection.
// ---------------------------------------------------------------------------

/// The windowed-only functions that may send it, by file name and the
/// innermost `fn` holding the send, each for the reason given.
const APP_SITES: &[(&str, &str)] = &[
    // Only through `app_menu_bar::reader(headless)`, which a headless App never gets.
    ("app_menu_bar.rs", "read_app_menu_bar"),
    // `AppRt::install_menu`, at window creation.
    ("menu.rs", "install"),
    // `validateMenuItem:` only, which AppKit sends for items of the bar `install` built.
    ("menu.rs", "stamp_full_screen_title"),
    // `WindowEvent::KeyboardInput` only.
    ("platform.rs", "current_event_queue_age_ns"),
    // An update handoff's Commit while an aterm window had focus; headless starts no updater.
    ("app_launch_successor.rs", "activate_running"),
    // After resolving a live AppKit window number; headless refuses before it.
    ("hwkey.rs", "post"),
    // The objc_toolbar_drive example's body.
    ("toolbar_drive.rs", "run"),
    // The objc_devtools_helper_drive example's body, which makes the process
    // Prohibited (never activated, no window shown) before anything else.
    ("devtools_helper_drive.rs", "run"),
    // The development pre-dial seam, on a successor only a window's updater launches.
    (
        "handoff_rendezvous.rs",
        "debug_check_in_with_launch_services",
    ),
    // `OsAppearanceSource::Platform` only (window attach and the KVO it registers);
    // a headless or test App holds the inert source.
];

pub(super) const SHARED_APPLICATION: Rule = Rule {
    id: "shared-application",
    why: "+[NSApplication sharedApplication] creates the application object when none exists, \
          as in a headless process, and its -init checks in with LaunchServices and opens a \
          WindowServer connection; aterm-gui sends it only in the windowed-only functions this \
          rule lists",
    scope: GUI_SOURCE,
    check: shared_application,
};

/// The source the rule reads: aterm-gui's.
const GUI_SOURCE: &[&str] = &["crates/aterm-gui/src/"];

fn shared_application(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    for file in tree.corpus.under(GUI_SOURCE) {
        if !file.mentions(&["sharedApplication"]) {
            continue;
        }
        for (no, func) in shared_application_sends(&file.lexed().code) {
            let allowed = func.is_some_and(|f| APP_SITES.contains(&(file.name(), f)));
            if !allowed {
                out.push(file.finding(
                    SHARED_APPLICATION.id,
                    no,
                    format!(
                        "sends +[NSApplication sharedApplication] in `{}`, which is on no \
                         windowed-only list",
                        func.unwrap_or("-")
                    ),
                ));
            }
        }
    }
}

/// Every `sel!(sharedApplication)` in a file's code: its line and the
/// innermost `fn` whose body holds it. A frame opens at the `{` its `fn` is
/// waiting for (a `;` at the signature's own depth, outside brackets, ends a
/// body-less one) and closes with its matching `}`, so a nested `fn` is its
/// own frame and a send after it closes is the outer one's.
fn shared_application_sends(code: &str) -> Vec<(usize, Option<&str>)> {
    let b = code.as_bytes();
    let mut out = Vec::new();
    let mut frames: Vec<(&str, i64)> = Vec::new();
    let mut pending: Option<(&str, i64, i64)> = None;
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
            pending = Some((name, depth, brackets));
            i += 2 + gap + name.len();
            continue;
        } else if c == b's' && boundary && sel_at(&code[i..], "sharedApplication") {
            out.push((line, frames.last().map(|f| f.0)));
        } else {
            match c {
                b'{' => {
                    depth += 1;
                    if let Some((name, _, _)) = pending.take() {
                        frames.push((name, depth));
                    }
                }
                b'}' => {
                    if frames.last().is_some_and(|f| f.1 == depth) {
                        frames.pop();
                    }
                    depth -= 1;
                }
                b'[' => brackets += 1,
                b']' => brackets -= 1,
                b';' if pending.is_some_and(|(_, d, k)| d == depth && k == brackets) => {
                    pending = None;
                }
                _ => {}
            }
        }
        i += 1;
    }
    out
}

// ---------------------------------------------------------------------------
// watch-hands: the upgrade's watch has no hands (design record 2026-09-28,
// "No upgrade stuck forever", §3.2 C4).
// ---------------------------------------------------------------------------

/// The watch's roots: each file and the function every line it reaches is
/// fenced from.
const WATCH_ROOTS: &[(&str, &str)] = &[
    ("crates/aterm-agent/src/harness/upgrade_watch.rs", "watch"),
    ("crates/aterm-gui/src/harness_host.rs", "watch_upgrades"),
];
/// The product's `Acts::watch`: a function whose whole job is one call.
const WATCH_ONLY: (&str, &str) = (
    "crates/aterm-gui/src/harness_host.rs",
    "watch_off_any_point",
);
/// What `watch_off_any_point` may call: `Some`, the watch, and a `default`.
fn watch_only_allows(call: &str) -> bool {
    call == "Some"
        || call == "upgrade_drive::watch"
        || call == "default"
        || call.strip_suffix("::default").is_some_and(|path| {
            path.split("::").all(|seg| {
                seg.bytes()
                    .next()
                    .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
                    && seg.bytes().all(is_word)
            })
        })
}

/// The control-verb table the input verbs are read from.
const VERB_TABLE: &str = "crates/aterm-types/src/control_verbs.rs";

/// The calls that type, signal, kill or relaunch.
fn is_hand(call: &str) -> bool {
    matches!(
        call,
        "turn_fenced"
            | "turn"
            | "type_line"
            | "type_text"
            | "terminate"
            | "restart"
            | "relaunch"
            | "carry_on"
            | "signal"
            | "kill"
            | "paste"
            | "send_line"
    ) || call.strip_prefix("kill_").is_some_and(|r| !r.is_empty())
}

/// A point's acts, reached through the act seam: `(acts.step)(…)`.
const POINT_ACTS: &[&str] = &[
    "step",
    "notice",
    "resume",
    "restart",
    "relaunch",
    "carry",
    "carry_in_flight",
    "relaunch_restored",
];

pub(super) const WATCH_HANDS: Rule = Rule {
    id: "watch-hands",
    why: "the upgrade's watch runs off any point, on a timer, which is safe only because it can \
          do nothing to the session: it and every function of its file it reaches never type, \
          send keys, paste, signal, kill or relaunch, take a point's act or name an input verb \
          in a request; the product's Acts::watch calls upgrade_drive::watch and nothing else",
    scope: &[],
    check: watch_hands,
};

fn watch_hands(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    let verbs = match tree
        .corpus
        .get(VERB_TABLE)
        .map(|f| input_verbs(&f.lexed().text))
    {
        Some(verbs) if verbs.len() >= 20 => verbs,
        found => {
            out.push(Finding {
                rule: WATCH_HANDS.id,
                path: VERB_TABLE.to_string(),
                line: 0,
                detail: format!(
                    "read {} input verbs off the verb table, too few to be it",
                    found.map_or(0, |v| v.len())
                ),
            });
            return;
        }
    };
    for (path, root) in WATCH_ROOTS {
        match tree.corpus.get(path) {
            Some(file) => out.extend(watch_reach_findings(file, root, &verbs)),
            None => out.push(no_root(path, root)),
        }
    }
    let (path, func) = WATCH_ONLY;
    match tree.corpus.get(path) {
        Some(file) => out.extend(watch_only_findings(file, func, watch_only_allows)),
        None => out.push(no_root(path, func)),
    }
}

fn no_root(path: &str, root: &str) -> Finding {
    Finding {
        rule: WATCH_HANDS.id,
        path: path.to_string(),
        line: 0,
        detail: format!("no fn {root} in this file, so the rule reads nothing"),
    }
}

/// Every control verb whose op-class is not `Read`, by its first word: each
/// `v("<name>", <OpClass>, …)` row of the verb table's text view.
fn input_verbs(text: &str) -> Vec<String> {
    let b = text.as_bytes();
    let mut verbs = Vec::new();
    for (at, _) in text.match_indices("v(") {
        if at > 0 && (b[at - 1].is_ascii_alphabetic() || b[at - 1] == b'_') {
            continue;
        }
        let rest = text[at + 2..].trim_start();
        let Some(rest) = rest.strip_prefix('"') else {
            continue;
        };
        let Some((name, rest)) = rest.split_once('"') else {
            continue;
        };
        let Some(rest) = rest.strip_prefix(',') else {
            continue;
        };
        let rest = rest.trim_start();
        let class_len = rest.bytes().take_while(u8::is_ascii_alphabetic).count();
        let class = &rest[..class_len];
        if name.is_empty() || class.is_empty() || !rest[class_len..].starts_with(',') {
            continue;
        }
        if class != "Read" {
            verbs.push(name.split(' ').next().unwrap_or(name).to_string());
        }
    }
    verbs.sort();
    verbs.dedup();
    verbs
}

/// The input verb a request string on this line names: `"@… <verb>"` with the
/// verb followed by a space, a `{` or the string's end.
fn request_verb<'v>(text: &str, verbs: &'v [String]) -> Option<&'v str> {
    for (at, _) in text.match_indices("\"@") {
        let body = &text[at + 2..];
        let (body, closed) = match body.find('"') {
            Some(end) => (&body[..end], true),
            None => (body, false),
        };
        for verb in verbs {
            let found = body.match_indices(verb.as_str()).any(|(v, _)| {
                let spaced = v > 0 && body.as_bytes()[v - 1].is_ascii_whitespace();
                let after = body.as_bytes().get(v + verb.len());
                let ends = match after {
                    Some(&c) => c.is_ascii_whitespace() || c == b'{',
                    None => closed,
                };
                spaced && ends
            });
            if found {
                return Some(verb);
            }
        }
    }
    None
}

/// `.<act>)(`, a point's act reached through its seam.
fn point_act(code: &str) -> Option<&'static str> {
    POINT_ACTS.iter().copied().find(|act| {
        code.match_indices(&format!(".{act})"))
            .any(|(at, _)| super::after_ws(&code[at + act.len() + 2..], "(").is_some())
    })
}

/// Every hand the functions `root` reaches in `file` hold.
fn watch_reach_findings(file: &SourceFile, root: &str, verbs: &[String]) -> Vec<Finding> {
    let lines = attribute(file);
    let mut defined: BTreeMap<&str, ()> = BTreeMap::new();
    let mut edges: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for l in lines.iter().filter(|l| !l.in_test) {
        let Some(func) = l.func else { continue };
        defined.insert(func, ());
        for callee in calls(l.line.code) {
            if callee != func {
                edges.entry(func).or_default().push(callee);
            }
        }
    }
    if !defined.contains_key(root) {
        return vec![no_root(&file.rel, root)];
    }
    for callees in edges.values_mut() {
        callees.retain(|c| defined.contains_key(c));
    }
    let from = reach([root], &edges);
    let chain = |f| {
        let mut path = walk(&from, f);
        path.reverse();
        path.join(" -> ")
    };
    let mut out = Vec::new();
    for l in lines.iter().filter(|l| !l.in_test) {
        let Some(func) = l.func.filter(|f| from.contains_key(f)) else {
            continue;
        };
        let detail = if let Some(verb) = request_verb(l.line.text, verbs) {
            format!(
                "the watch names an input verb in a request: {} [{verb}]",
                chain(func)
            )
        } else if let Some(act) = point_act(l.line.code) {
            format!(
                "the watch runs the session's {act} callback: {} -> ({act})",
                chain(func)
            )
        } else if let Some(hand) = calls(l.line.code).into_iter().find(|c| is_hand(c)) {
            format!(
                "the watch types, signals or relaunches: {} -> {hand}",
                chain(func)
            )
        } else {
            continue;
        };
        out.push(file.finding(WATCH_HANDS.id, l.line.no, detail));
    }
    out
}

/// Every free or path call (never a method) `func` makes that `allowed`
/// refuses: the allow-list form, for a function whose whole job is one call.
fn watch_only_findings(file: &SourceFile, func: &str, allowed: fn(&str) -> bool) -> Vec<Finding> {
    let mut seen = false;
    let mut out = Vec::new();
    for l in attribute(file) {
        if l.in_test || l.func != Some(func) {
            continue;
        }
        seen = true;
        for call in path_calls(l.line.code) {
            if call != func && !allowed(call) {
                out.push(file.finding(
                    WATCH_HANDS.id,
                    l.line.no,
                    format!("the watch calls past its allow-list: {func} -> {call}"),
                ));
            }
        }
    }
    if !seen {
        out.push(no_root(&file.rel, func));
    }
    out
}

/// Every free or path call on a line (`name(`, `a::b(`), methods (`.name(`)
/// left out.
fn path_calls(code: &str) -> Vec<&str> {
    let b = code.as_bytes();
    let mut out = Vec::new();
    for (open, _) in code.match_indices('(') {
        let mut end = open;
        while end > 0 && matches!(b[end - 1], b' ' | b'\t') {
            end -= 1;
        }
        let mut start = end;
        while start > 0 && (is_word(b[start - 1]) || b[start - 1] == b':') {
            start -= 1;
        }
        let path = code[start..end].trim_start_matches(':');
        let starts_ok = path
            .bytes()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == b'_');
        let method = start > 0 && b[start - 1] == b'.';
        if starts_ok && !method && start == end - path.len() {
            out.push(path);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::tests::run;
    use super::*;

    const HEADER: &str = "// SPDX-License-Identifier: Apache-2.0\n";

    /// The door finding for one file played as aterm-gui source.
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

    /// Each way a test reaches a door, with the chain its finding prints.
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

    /// The application object's sends, by file and innermost function.
    fn app_sends(files: &[(&str, &str)]) -> Vec<Finding> {
        run(&SHARED_APPLICATION, files)
    }

    const SEND: &str =
        "unsafe { appkit::send_id(class(c\"NSApplication\").as_id(), sel!(sharedApplication)) }";

    #[test]
    fn a_send_outside_the_windowed_functions_is_found() {
        let cases: &[(&str, &str, String, &str)] = &[
            (
                "an allowlisted name in another file",
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
                "a send after a nested allowlisted name",
                "crates/aterm-gui/src/app_menu_bar.rs",
                format!(
                    "impl App {{\n    fn read_native_chrome(&self) -> Vec<String> {{\n        fn read_app_menu_bar() -> usize {{\n            0\n        }}\n        let app =\n            {SEND};\n        vec![]\n    }}\n}}\n"
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
    fn sends_in_the_windowed_functions_prose_and_strings_are_silent() {
        let menu = format!(
            "mod macos {{\n    pub(crate) fn install(p: &Proxy) -> Option<Menu> {{\n        let main = build(p)?;\n        unsafe {{\n            let app = {SEND};\n        }}\n        Some(Menu)\n    }}\n}}\n"
        );
        let successor = format!(
            "pub(crate) fn activate_running(pid: i32) -> bool {{\n    unsafe {{\n        let shared: unsafe extern \"C-unwind\" fn(Id, Sel) -> Id = aterm_objc::msg();\n        let app = {SEND};\n        pid > 1\n    }}\n}}\n"
        );
        let hwkey = format!(
            "pub(crate) fn post(spec: &[u8; 4], n: i64) -> Result<u32, String> {{\n    let app = {SEND};\n    Ok(0)\n}}\n"
        );
        let prose = "impl App {\n    /// Never `sel!(sharedApplication)` here.\n    fn read_native_chrome(&self) -> Vec<String> {\n        // sel!(sharedApplication) in a trailing note\n        let why = \"sel!(sharedApplication) CREATES NSApp\";\n        vec![why.to_string()]\n    }\n}\n";
        let found = app_sends(&[
            ("crates/aterm-gui/src/menu.rs", &menu),
            ("crates/aterm-gui/src/app_launch_successor.rs", &successor),
            ("crates/aterm-gui/src/hwkey.rs", &hwkey),
            ("crates/aterm-gui/src/app_introspect.rs", prose),
        ]);
        assert!(found.is_empty(), "CRIES WOLF: {found:?}");
    }

    fn verbs() -> Vec<String> {
        [
            "key",
            "send",
            "paste",
            "paste-bin",
            "hwkey",
            "turn",
            "signal",
        ]
        .iter()
        .map(|v| (*v).to_string())
        .collect()
    }

    /// Each hand the watch can reach, with the chain its finding prints.
    #[test]
    fn every_hand_the_watch_reaches_is_found_with_its_chain() {
        let cases: &[(&str, &str, &str, &str)] = &[
            (
                "a brace in a string ends no fn",
                "watch",
                "pub fn watch(opts: &Opts, tab: &str) -> Watched {\n    let line = format!(\"watch }} {tab}\");\n    let closing = \"}\";\n    restart(opts, tab);\n    Watched::default()\n}\n",
                "the watch types, signals or relaunches: watch -> restart",
            ),
            (
                "a helper two hops down types",
                "watch",
                "fn nudge(c: &mut Client, tab: &str) {\n    let _ = super::type_line(c, tab, \"keep going\");\n}\n\nfn write_back(opts: &Opts, tab: &str) {\n    if let Ok(mut c) = connect(opts, tab) {\n        nudge(&mut c, tab);\n    }\n}\n\npub fn watch(opts: &Opts, tab: &str) -> Watched {\n    write_back(opts, tab);\n    Watched::default()\n}\n",
                "the watch types, signals or relaunches: watch -> write_back -> nudge -> type_line",
            ),
            (
                "a method call kills",
                "watch",
                "impl Stuck {\n    fn reap(&self) {\n        if self.waited > BUDGET {\n            unsafe { libc::kill(self.pid, libc::SIGTERM) };\n        }\n    }\n}\n\npub fn watch(opts: &Opts, tab: &str) -> Watched {\n    let stuck = Stuck::read(opts, tab);\n    stuck.reap();\n    Watched::default()\n}\n",
                "the watch types, signals or relaunches: watch -> reap -> kill",
            ),
            (
                "a request names an input verb",
                "watch",
                "fn poke(c: &mut Client, tab: &str) {\n    let _ = c.request_line(&format!(\"@{tab} key enter\"));\n}\n\npub fn watch(opts: &Opts, tab: &str) -> Watched {\n    poke(&mut c, tab);\n    Watched::default()\n}\n",
                "the watch names an input verb in a request: watch -> poke [key]",
            ),
            (
                "a request pastes bytes",
                "watch",
                "fn put(c: &mut Client, tab: &str, b: &[u8]) {\n    let _ = c.request_line(&format!(\"@{tab} paste-bin {}\", b.len()));\n}\n\npub fn watch(opts: &Opts, tab: &str) -> Watched {\n    put(&mut c, tab, b\"/exit\\r\");\n    Watched::default()\n}\n",
                "watch -> put [paste-bin]",
            ),
            (
                "a request presses a hardware key",
                "watch",
                "fn press(c: &mut Client, tab: &str) {\n    let _ = c.request_line(&format!(\"@{tab} hwkey enter\"));\n}\n\npub fn watch(opts: &Opts, tab: &str) -> Watched {\n    press(&mut c, tab);\n    Watched::default()\n}\n",
                "watch -> press [hwkey]",
            ),
            (
                "the host takes a point's step",
                "watch_upgrades",
                "fn watch_tab(sid: &str, acts: &Acts) {\n    let watched = (acts.watch)(sid, 0, &Watcher::default());\n    if !watched.wrote() {\n        let _ = (acts.step)(sid, 0);\n    }\n}\n\nfn watch_upgrades(due: &[String], hooks: &Hooks) -> Option<Instant> {\n    for sid in due {\n        watch_tab(sid, &hooks.acts);\n    }\n    None\n}\n",
                "the watch runs the session's step callback: watch_upgrades -> watch_tab -> (step)",
            ),
            (
                "the watch types a turn",
                "watch",
                "pub fn watch(opts: &Opts, tab: &str) -> Watched {\n    turn_fenced(&mut c, tab, \"please wrap up\", g);\n    Watched::default()\n}\n",
                "the watch types, signals or relaunches: watch -> turn_fenced",
            ),
            (
                "no root anchors on nothing",
                "watch",
                "pub fn look_off_any_point(opts: &Opts, tab: &str) -> Watched {\n    Watched::default()\n}\n",
                "no fn watch in this file",
            ),
        ];
        for (why, root, src, expect) in cases {
            let file = SourceFile::new("crates/a/src/w.rs".to_string(), (*src).to_string());
            let found = watch_reach_findings(&file, root, &verbs());
            assert_eq!(found.len(), 1, "MISS or extra — {why}: {found:?}\n{src}");
            assert!(
                found[0].detail.contains(expect),
                "{why}: {:?}",
                found[0].detail
            );
        }
    }

    #[test]
    fn what_the_watch_never_reaches_tests_and_prose_are_silent() {
        let src = "// The watch never calls turn_fenced(…) or restart(…).\n\n\
                   /// A doc line naming `type_line(c, tab, \"x\")` is prose.\n\
                   pub fn watch(opts: &Opts, tab: &str) -> Watched {\n    let dry = Opts {\n        dry_run: true,\n        ..opts.clone()\n    };\n    let r = super::step(&dry);\n    let note = \"the watch never types a turn or a restart\";\n    let _ = c.request_line(&format!(\"@{tab} status\"));\n    write_back(opts, tab, &r, note);\n    Watched::default()\n}\n\n\
                   fn write_back(opts: &Opts, tab: &str, r: &Report, note: &str) {\n    let mut st = load(opts, &r.session)?;\n    save_at(opts, &r.session, &st, 0);\n}\n\n\
                   fn announce_at_a_point(c: &mut Client, tab: &str) {\n    turn_fenced(c, tab, \"the notice\", None);\n    let _ = c.request_line(&format!(\"@{tab} key enter\"));\n}\n\n\
                   #[cfg(test)]\nmod tests {\n    #[test]\n    fn a_test_may_type() {\n        turn(&mut c, \"s-t\", \"hello\");\n        watch(&opts, \"s-t\", &how);\n    }\n}\n";
        let file = SourceFile::new("crates/a/src/w.rs".to_string(), src.to_string());
        let found = watch_reach_findings(&file, "watch", &verbs());
        assert!(found.is_empty(), "CRIES WOLF: {found:?}");
    }

    #[test]
    fn the_product_watch_calls_nothing_past_its_allow_list() {
        let visit = "fn watch_off_any_point(\n    opts: Option<upgrade_drive::Opts>,\n    sid: &str,\n) -> upgrade_drive::Watched {\n    let Some(o) = opts else {\n        return upgrade_drive::Watched::default();\n    };\n    let _ = upgrade_drive::step(&o);\n    upgrade_drive::watch(&o, sid, how)\n}\n";
        let file = SourceFile::new("crates/a/src/h.rs".to_string(), visit.to_string());
        let found = watch_only_findings(&file, "watch_off_any_point", watch_only_allows);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            found[0]
                .detail
                .ends_with("watch_off_any_point -> upgrade_drive::step")
        );

        let shipped = "fn watch_off_any_point(\n    opts: Option<upgrade_drive::Opts>,\n) -> upgrade_drive::Watched {\n    let Some(o) = opts else {\n        return upgrade_drive::Watched {\n            skipped: Some(\"no-home\".to_string()),\n            ..upgrade_drive::Watched::default()\n        };\n    };\n    upgrade_drive::watch(&o, sid, how)\n}\n";
        let file = SourceFile::new("crates/a/src/h.rs".to_string(), shipped.to_string());
        let found = watch_only_findings(&file, "watch_off_any_point", watch_only_allows);
        assert!(found.is_empty(), "CRIES WOLF: {found:?}");
    }

    #[test]
    fn input_verbs_are_every_non_read_verb_of_the_table() {
        let table = "const VERBS: &[Verb] = &[\n    v(\"status\", Read, \"…\"),\n    v(\"key\", Input,\n      \"…\"),\n    v(\"inbox seen\", Write, \"…\"),\n    // v(\"ghost\", Input, \"…\"),\n];\n";
        assert_eq!(input_verbs(&crate::lex::lex(table).text), ["inbox", "key"]);
    }

    #[test]
    fn the_watch_with_too_few_verbs_is_a_finding_not_a_pass() {
        let found = run(&WATCH_HANDS, &[(VERB_TABLE, "v(\"key\", Input, \"x\"),\n")]);
        assert_eq!(found.len(), 1);
        assert!(found[0].detail.contains("too few"), "{found:?}");
    }
}
