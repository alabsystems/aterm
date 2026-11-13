// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The token guards: a banned name or literal in a scoped set of files.

use super::{
    Bound, Corpus, Finding, Line, Rule, SourceFile, Tree, after_ws, has, hits, is_word,
    named_for_tests,
};

/// Product source: everything the guards call the tree, vendored code aside.
pub(super) const PRODUCT: &[&str] = &["crates/", "apps/"];

/// This crate's own source, which spells every name a literal-reading rule bans (its tables and
/// its planted cases).
pub(super) const CENSUS_CRATE: &str = "crates/aterm-census/";

/// The engine crates: no platform API in their default build.
const ENGINE_CRATES: &[&str] = &[
    "crates/aterm-parser/",
    "crates/aterm-grid/",
    "crates/aterm-scrollback/",
    "crates/aterm-core/",
    "crates/aterm-buffer/",
    "crates/aterm-cap/",
    "crates/aterm-effects/",
    "crates/aterm-messages/",
    "crates/aterm-slog/",
    "crates/aterm-restore/",
    "crates/aterm-aos/",
    "crates/aterm-aos-pane/",
    "crates/amacs/",
];

/// What an engine crate's shipped code may not name, and what each is.
const ENGINE_BANNED: &[(&str, Bound, &str)] = &[
    ("std::fs", Bound::Sub, "the filesystem"),
    ("TcpStream", Bound::Sub, "a socket"),
    ("UdpSocket", Bound::Sub, "a socket"),
    ("UnixStream", Bound::Sub, "a socket"),
    ("UnixListener", Bound::Sub, "a socket"),
    ("TcpListener", Bound::Sub, "a socket"),
    ("RawFd", Bound::Sub, "a raw fd"),
    ("as_raw_fd", Bound::Sub, "a raw fd"),
    ("from_raw_fd", Bound::Sub, "a raw fd"),
    ("AsRawFd", Bound::Sub, "a raw fd"),
    ("libc", Bound::Word, "libc"),
    ("thread::spawn", Bound::Sub, "a thread"),
    ("thread::Builder", Bound::Sub, "a thread"),
    ("std::thread", Bound::Word, "a thread"),
    ("std::net", Bound::Word, "a socket"),
    ("#[link", Bound::Word, "a native library"),
];

/// The `std` modules a grouped import (`use std::{fs, net::TcpStream};`) may not name in an
/// engine crate.
const ENGINE_STD_MODULES: &[(&str, &str)] = &[
    ("fs", "the filesystem"),
    ("net", "a socket"),
    ("thread", "a thread"),
];

/// Platform frameworks read on the text view (`link(name = "AppKit")` or framework path
/// literals).
const ENGINE_FRAMEWORKS: &[(&str, Bound)] = &[
    ("CoreText", Bound::Sub),
    ("Metal", Bound::Word),
    ("AppKit", Bound::Sub),
];

/// aterm-effects' replay pool for its unit tests and its integration suite, which its root mounts
/// under `cfg(any(test, debug_assertions))`: a debug build holds it, so [`TestCode::holds`] reads
/// it as shipped, and engine-platform leaves it out by this one name.
const ENGINE_SUITE_POOL: &str = "crates/aterm-effects/src/test_pool.rs";

/// Is `rel` an engine crate's file that engine-platform judges: not a build script, an example,
/// aterm-effects' `file_feed.rs` or [`ENGINE_SUITE_POOL`]? The rule leaves test code out as
/// every guard does ([`TestCode::holds`]), so a shipped file whose name opens with `test` is
/// judged.
fn engine_shipped(rel: &str) -> bool {
    if !ENGINE_CRATES.iter().any(|c| rel.starts_with(c)) {
        return false;
    }
    if rel.ends_with("/build.rs") || rel.contains("/examples/") {
        return false;
    }
    rel != "crates/aterm-effects/src/file_feed.rs" && rel != ENGINE_SUITE_POOL
}

/// Every file a test-only `cfg` loads: an out-of-line `mod` or a literal `include!` inside an
/// item whose `cfg` proves it test-only, at any depth (`#[cfg(test)] mod rig;`,
/// `#[cfg(test)] mod tests { include!("tests_hooks.rs"); }`, `mod imp { #[cfg(test)] mod deep;
/// }`); every file such a file loads in turn (`#[path = "x_more_tests.rs"] mod more;` in it);
/// and, for each, the directory of its stem (`rig/` beside `rig.rs`, or `rig/mod.rs`'s own).
fn declared_test_modules(corpus: &Corpus, prefixes: &[&str]) -> Declared {
    let mut out = Declared::default();
    let push = |out: &mut Declared, file: String| {
        let own = file
            .strip_suffix("/mod.rs")
            .or_else(|| file.strip_suffix(".rs"))
            .unwrap_or(&file)
            .to_string();
        out.0.insert(format!("{own}/"));
        out.0.insert(file);
    };
    for parent in corpus.under(prefixes) {
        if !parent.mentions(&["mod", "include"]) {
            continue;
        }
        for load in parent.loads().iter().filter(|l| l.gated) {
            if let Some(file) = load.files.iter().find(|f| corpus.get(f).is_some()) {
                push(&mut out, file.clone());
            }
        }
    }
    loop {
        let before = out.0.len();
        for parent in corpus.under(prefixes) {
            if !out.holds(&parent.rel) || !parent.mentions(&["mod", "include"]) {
                continue;
            }
            for load in parent.loads() {
                if let Some(file) = load.files.iter().find(|f| corpus.get(f).is_some())
                    && !out.holds(file)
                {
                    push(&mut out, file.clone());
                }
            }
        }
        if out.0.len() == before {
            return out;
        }
    }
}

/// The files and directories (each `/`-ended) [`declared_test_modules`] names.
#[derive(Default)]
struct Declared(std::collections::BTreeSet<String>);

impl Declared {
    /// Is `rel` one of the files, or under one of the directories? Each directory `rel` lies
    /// under is one of its own `/`-ended prefixes, so those are looked up, not every entry
    /// read.
    fn holds(&self, rel: &str) -> bool {
        self.0.contains(rel)
            || rel
                .match_indices('/')
                .any(|(at, _)| self.0.contains(&rel[..=at]))
    }
}

/// Which files of the corpus are test code, and which ship whatever their names or their parents
/// say. Every guard that leaves a file out because it is test code asks [`TestCode::holds`]
/// ([`Corpus::test_code`]); beside it, engine-platform leaves out one piece of test support by
/// its name ([`ENGINE_SUITE_POOL`]). A guard that asks another question walks its own way:
/// seam-caller follows what a release build compiles.
pub(super) struct TestCode {
    /// The files and directories [`declared_test_modules`] names.
    declared: Declared,
    /// What a shipped line of a shipped file loads ([`Corpus::shipped_loads`]).
    shipped: std::collections::BTreeSet<String>,
}

impl TestCode {
    /// The test code of the whole corpus: a file its parent declares test-only
    /// ([`declared_test_modules`]) or one [`named_for_tests`] is test code, which ships only once
    /// a shipped line loads it, and what it loads ships only then ([`Corpus::shipped_loads`]). So
    /// a test target's `mod common;`, or its `#[path]` to a `_tests.rs`, ships nothing; nor does
    /// a bench's or an example's.
    pub(super) fn read(corpus: &Corpus) -> Self {
        let declared = declared_test_modules(corpus, PRODUCT);
        let shipped = corpus.shipped_loads(PRODUCT, &|rel| declared.holds(rel));
        Self { declared, shipped }
    }

    /// Is `rel` test code by its parent's say (a file a test-only `mod` or `include!` loads, at
    /// any depth, or one in such a file's stem directory: [`declared_test_modules`]) or by its
    /// path ([`named_for_tests`]), that nothing ships? A file that a shipped `mod` or `include!`
    /// of a shipped file also loads ships, and is read.
    pub(super) fn holds(&self, rel: &str) -> bool {
        (self.declared.holds(rel) || named_for_tests(rel)) && !self.shipped.contains(rel)
    }
}

pub(super) const ENGINE_PLATFORM: Rule = Rule {
    id: "engine-platform",
    why: "the engine crates build headless for every host (docs/ATERM_DESIGN.md §2.7): their \
          default build names no filesystem, socket, raw fd, libc, platform framework, thread \
          or native link; the host does that and hands the engine values",
    scope: ENGINE_CRATES,
    check: engine_platform,
};

fn engine_platform(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    let test_code = tree.corpus.test_code();
    let needles: Vec<&str> = ENGINE_BANNED
        .iter()
        .map(|b| b.0)
        .chain(ENGINE_FRAMEWORKS.iter().map(|f| f.0))
        .chain(["link", "std"])
        .collect();
    for file in tree.corpus.under(ENGINE_CRATES) {
        if !engine_shipped(&file.rel) || test_code.holds(&file.rel) || !file.mentions(&needles) {
            continue;
        }
        let shipped: std::collections::BTreeSet<usize> =
            file.shipped_lines().map(|l| l.no).collect();
        for (no, module) in grouped_std_modules(&file.lexed().code) {
            let Some((_, what)) = ENGINE_STD_MODULES.iter().find(|(m, _)| *m == module) else {
                continue;
            };
            if shipped.contains(&no) {
                out.push(file.finding(
                    ENGINE_PLATFORM.id,
                    no,
                    format!("imports `std::{module}` in a group ({what})"),
                ));
            }
        }
        for line in file.shipped_lines() {
            for (token, bound, what) in ENGINE_BANNED {
                if has(line.code, token, *bound) {
                    out.push(file.finding(
                        ENGINE_PLATFORM.id,
                        line.no,
                        format!("names `{token}` ({what})"),
                    ));
                }
            }
            for (framework, bound) in ENGINE_FRAMEWORKS {
                if has(line.text, framework, *bound) {
                    out.push(file.finding(
                        ENGINE_PLATFORM.id,
                        line.no,
                        format!("names `{framework}` (a platform framework)"),
                    ));
                }
            }
            if !has(line.code, "#[link", Bound::Word) && links_by_name(line.code) {
                out.push(file.finding(
                    ENGINE_PLATFORM.id,
                    line.no,
                    "links a native library (`link(name = …)`)".to_string(),
                ));
            }
        }
    }
}

/// Every first segment of a `std::{…}` group in a file's code with its 1-based line number.
fn grouped_std_modules(code: &str) -> Vec<(usize, &str)> {
    let b = code.as_bytes();
    let mut out = Vec::new();
    for at in hits(code, "std", Bound::Word) {
        let Some(open) = after_ws(&code[at + 3..], "::").and_then(|r| after_ws(r, "{")) else {
            continue;
        };
        let mut i = code.len() - open.len();
        let mut depth = 1usize;
        let mut item_start = i;
        while i < b.len() && depth > 0 {
            match b[i] {
                b'{' => depth += 1,
                b'}' => depth -= 1,
                _ => {}
            }
            if depth == 0 || (depth == 1 && b[i] == b',') {
                let item = &code[item_start..i];
                let lead = item.len() - item.trim_start().len();
                let head: &str = item
                    .trim_start()
                    .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                    .next()
                    .unwrap_or("");
                if !head.is_empty() {
                    let line = code[..item_start + lead].matches('\n').count() + 1;
                    out.push((line, head));
                }
                item_start = i + 1;
            }
            i += 1;
        }
    }
    out
}

/// Matches a `link(name = …)` attribute however it is reached.
fn links_by_name(code: &str) -> bool {
    hits(code, "link", Bound::Word).any(|at| {
        after_ws(&code[at + 4..], "(")
            .and_then(|r| after_ws(r, "name"))
            .and_then(|r| after_ws(r, "="))
            .is_some()
    })
}

const RENDERERS: &[&str] = &["crates/aterm-render/src/", "crates/aterm-gpu/src/"];

pub(super) const RENDERER_TERMINAL: Rule = Rule {
    id: "renderer-terminal",
    why: "aterm-render and aterm-gpu read the engine's RenderInput snapshot; naming \
          aterm_core::Terminal (or borrowing one) reaches back into the engine, which the A-3 \
          boundary moved out of the renderers",
    scope: RENDERERS,
    check: renderer_terminal,
};

fn renderer_terminal(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    for file in tree.corpus.under(RENDERERS) {
        if tree.corpus.test_code().holds(&file.rel) || !file.mentions(&["Terminal"]) {
            continue;
        }
        for line in file.shipped_lines() {
            if let Some(what) = names_terminal(line.code) {
                out.push(file.finding(RENDERER_TERMINAL.id, line.no, what.to_string()));
            }
        }
    }
}

/// `aterm_core::…Terminal`, or `Terminal` as a borrowed, annotated or returned type
/// (`&Terminal`, `: Terminal`, `-> Terminal`).
fn names_terminal(code: &str) -> Option<&'static str> {
    let b = code.as_bytes();
    for at in hits(code, "Terminal", Bound::Sub) {
        if b.get(at + 8).is_some_and(|&c| is_word(c)) {
            continue;
        }
        let before =
            code[..at].trim_end_matches(|c: char| c.is_ascii_alphabetic() || "_:".contains(c));
        let run = &code[before.len()..at];
        if run.starts_with("aterm_core::") {
            return Some("names aterm_core::Terminal");
        }
        let lead = code[..at].trim_end_matches(' ');
        if code[..at].ends_with('&') || lead.ends_with(':') || lead.ends_with("->") {
            return Some("takes or returns a Terminal");
        }
    }
    None
}

pub(super) const DEV_READ: Rule = Rule {
    id: "dev-read",
    why: "a device stream need not end: `fs::read`/`read_to_string` of a /dev path allocates \
          without bound (twice a kernel watchdog panic); open it and `read_exact`/`take`, or \
          mint entropy through aterm_uds::rand",
    scope: &[],
    check: dev_read,
};

fn dev_read(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    for file in tree.corpus.under(PRODUCT) {
        if !file.mentions(&["/dev/"]) {
            continue;
        }
        for line in file.lines() {
            if reads_a_device(line) {
                out.push(file.finding(
                    DEV_READ.id,
                    line.no,
                    "reads a /dev path to EOF".to_string(),
                ));
            }
        }
    }
}

/// Matches `fs::read(…)` or `fs::read_to_string(…)` whose first argument is a `/dev/…`
/// literal.
fn reads_a_device(line: Line<'_>) -> bool {
    hits(line.code, "fs::read", Bound::Sub).any(|at| {
        let mut rest = &line.code[at + "fs::read".len()..];
        rest = rest.strip_prefix("_to_string").unwrap_or(rest);
        let Some(arg) = after_ws(rest, "(") else {
            return false;
        };
        let arg = arg.trim_start_matches([' ', '\t']);
        if !arg.starts_with('"') {
            return false;
        }
        let quote = line.code.len() - arg.len();
        line.text
            .get(quote + 1..)
            .is_some_and(|lit| lit.starts_with("/dev/"))
    })
}

/// Query APIs whose first call opens a SkyLight connection and triggers WindowServer's
/// main-thread preflight.
const INPUT_PROBES: &[&str] = &[
    "modifierFlags_class",
    "NSEvent modifierFlags",
    "NSEvent::modifierFlags",
    "CGEventSourceFlagsState",
    "CGEventSourceSecondsSince",
    "SLSInputModifierKeyState",
];

pub(super) const INPUT_PROBE: Rule = Rule {
    id: "input-probe",
    why: "AppKit's class modifier flags and the CoreGraphics/SkyLight event-source queries open \
          a WindowServer connection whose TCC preflight runs on WindowServer's main thread, \
          which a large target dir made outrun its watchdog; read the kernel \
          (keymap::hid_lock_state, platform::hid_idle) through an injected App field",
    scope: &[],
    check: input_probe,
};

fn input_probe(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    for file in tree.corpus.under(PRODUCT) {
        if file.rel.starts_with(CENSUS_CRATE) || !file.mentions(INPUT_PROBES) {
            continue;
        }
        for line in file.lines() {
            for probe in INPUT_PROBES {
                if line.text.contains(probe) {
                    out.push(file.finding(INPUT_PROBE.id, line.no, format!("names `{probe}`")));
                }
            }
        }
    }
}

/// The crates that take protected paths resolved from consent: aterm-gui and its `aterm-gui-*`
/// libraries, aterm-cli, `amacs-*` and aterm-diskwalk.
const PROTECTED_SCOPE: &[&str] = &[
    "crates/aterm-gui/src/",
    "crates/aterm-gui-",
    "crates/aterm-cli/src/",
    "crates/amacs-",
    "crates/aterm-diskwalk/src/",
];
const PROTECTED_HOME_DIRS: &[&str] = &[
    "Documents",
    "Desktop",
    "Downloads",
    "Pictures",
    "Movies",
    "Music",
];

pub(super) const PROTECTED_FOLDER: Rule = Rule {
    id: "protected-folder",
    why: "touching ~/Documents, ~/Desktop, ~/Downloads, /Volumes or Library/Containers|\
          CloudStorage raises a TCC modal that holds the calling syscall until a person \
          answers; aterm-gui, aterm-cli and aterm-diskwalk take those paths resolved from \
          aterm_containment::consent, never as literals",
    scope: PROTECTED_SCOPE,
    check: protected_folder,
};

fn protected_folder(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    let needles: Vec<&str> = PROTECTED_HOME_DIRS
        .iter()
        .copied()
        .chain(["Volumes", "Containers", "CloudStorage"])
        .collect();
    // Test code by its path (`_tests.rs`, a `tests` directory) or by its parent's say (the file a
    // `#[cfg(test)] mod tests;` loads) is skipped unless a shipped line loads it too.
    let test_code = tree.corpus.test_code();
    for file in tree.corpus.under(PROTECTED_SCOPE) {
        if test_code.holds(&file.rel) || !file.mentions(&needles) {
            continue;
        }
        for line in file.shipped_lines() {
            if let Some(what) = protected_path(line.text) {
                out.push(file.finding(PROTECTED_FOLDER.id, line.no, format!("names `{what}`")));
            }
        }
    }
}

/// The protected-folder shape `text` holds (`~/<home dir>`, `/Volumes`,
/// `Library/Containers|CloudStorage`, a bare `Containers`/`CloudStorage` component, or
/// `join("Documents")`).
fn protected_path(text: &str) -> Option<String> {
    for dir in PROTECTED_HOME_DIRS {
        let path = format!("~/{dir}");
        if text.contains(&path) {
            return Some(path);
        }
    }
    for shape in ["/Volumes", "Library/Containers", "Library/CloudStorage"] {
        // `/System/Volumes/<volume>` is where macOS mounts its own volumes (the
        // firmlinks point into `/System/Volumes/Data`), not the protected
        // `/Volumes`; that folder spelled on the data volume
        // (`/System/Volumes/Data/Volumes`) still counts.
        if text
            .match_indices(shape)
            .any(|(at, _)| shape != "/Volumes" || !text[..at].ends_with("/System"))
        {
            return Some(shape.to_string());
        }
    }
    for word in ["Containers", "CloudStorage"] {
        if has(text, word, Bound::Word) {
            return Some(word.to_string());
        }
    }
    for (at, _) in text.match_indices("join(") {
        let arg = text[at + 5..].trim_start_matches([' ', '\t']);
        let Some(arg) = arg.strip_prefix('"') else {
            continue;
        };
        let name = arg.split('"').next().unwrap_or("");
        if PROTECTED_HOME_DIRS.contains(&name)
            || ["Volumes", "Containers", "CloudStorage"].contains(&name)
        {
            return Some(format!("join(\"{name}\")"));
        }
    }
    None
}

const RUNLOOP_TIMERS: &[&str] = &[
    "CFRunLoopTimerCreate",
    "scheduledTimerWithTimeInterval",
    "timerWithTimeInterval",
    "initWithFireDate",
];

pub(super) const RUNLOOP_TIMER: Rule = Rule {
    id: "runloop-timer",
    why: "a late repeating CFRunLoopTimer is walked one interval at a time under the run-loop \
          lock (winit's 0.1 µs waker turned a long stop into minutes of main-thread spin); \
          aterm_objc::wake_timer is the one timer, so a second one is argued for there",
    scope: WINDOW_SOURCE,
    check: runloop_timer,
};

/// Rust that runs a run loop, build scripts and dev targets aside: product source, the absorbed
/// winit among it. Its caller leaves test code out ([`TestCode::holds`]).
fn runloop_scope(rel: &str) -> bool {
    let shipped =
        !(rel.ends_with("/build.rs") || rel.contains("/examples/") || rel.contains("/benches/"));
    shipped && rel != "crates/aterm-objc/src/wake_timer.rs" && !rel.starts_with(CENSUS_CRATE)
}

/// The absorbed winit (`crates/aterm-window`) run-loop scope.
const WINDOW_SOURCE: &[&str] = &["crates/aterm-window/src/"];

fn runloop_timer(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    let test_code = tree.corpus.test_code();
    for file in tree.corpus.under(PRODUCT) {
        if !runloop_scope(&file.rel) || test_code.holds(&file.rel) || !file.mentions(RUNLOOP_TIMERS)
        {
            continue;
        }
        for line in file.shipped_lines() {
            if let Some(timer) = RUNLOOP_TIMERS.iter().find(|t| line.text.contains(**t)) {
                out.push(file.finding(RUNLOOP_TIMER.id, line.no, format!("names `{timer}`")));
            }
        }
    }
}

/// The keeper and the one crate of its own it links.
const KEEPER_SOURCE: &[&str] = &[
    "crates/aterm-keeper/src/",
    "crates/aterm-keeper/tests/",
    "crates/aterm-uds/src/",
];
const GUI_FRAMEWORKS: &[&str] = &[
    "AppKit",
    "CoreGraphics",
    "Cocoa",
    "Metal",
    "QuartzCore",
    "SkyLight",
];
const GUI_NAMES: &[&str] = &[
    "objc_msgSend",
    "NSApplication",
    "aterm_window::",
    "winit::",
    "CGEvent",
    "NSWorkspace",
];

pub(super) const KEEPER_GUI: Rule = Rule {
    id: "keeper-gui",
    why: "the PTY keeper outlives the window because it is no WindowServer client: its own \
          code and UDS layer name no GUI framework/API or raw Objective-C message send; \
          platform observations belong behind their dependency boundary",
    scope: KEEPER_SOURCE,
    check: keeper_gui,
};

fn keeper_gui(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    for file in tree.corpus.under(KEEPER_SOURCE) {
        for line in file.lines() {
            if let Some(what) = links_gui(line.text) {
                out.push(file.finding(KEEPER_GUI.id, line.no, what));
            }
        }
    }
}

/// A GUI framework named in a link attribute (`name = "AppKit"`), or a GUI API by name.
fn links_gui(text: &str) -> Option<String> {
    linked_gui_framework(text).or_else(|| {
        GUI_NAMES
            .iter()
            .find(|n| text.contains(**n))
            .map(|n| format!("names `{n}`"))
    })
}

/// GUI evidence in an inspected dependency; raw Foundation runtime sends alone are not GUI.
pub(super) fn links_gui_dependency(text: &str) -> Option<String> {
    linked_gui_framework(text).or_else(|| {
        GUI_NAMES
            .iter()
            .filter(|&&n| n != "objc_msgSend")
            .find(|n| text.contains(**n))
            .map(|n| format!("names `{n}`"))
    })
}

fn linked_gui_framework(text: &str) -> Option<String> {
    for name in GUI_FRAMEWORKS {
        if text.contains(&format!("{name}.framework"))
            || text.contains(&format!("framework={name}"))
        {
            return Some(format!("names the `{name}` framework"));
        }
    }
    for (at, _) in text.match_indices("name") {
        if at > 0 && is_word(text.as_bytes()[at - 1]) {
            continue;
        }
        let Some(value) = after_ws(&text[at + 4..], "=").and_then(|v| after_ws(v, "\"")) else {
            continue;
        };
        let name = value.split('"').next().unwrap_or("");
        if GUI_FRAMEWORKS.contains(&name) {
            return Some(format!("links the `{name}` framework"));
        }
    }
    None
}

/// The keeper's own source.
const KEEPER_OWN: &[&str] = &["crates/aterm-keeper/src/"];

/// Memory-mapping calls by name (`mmap`, `munmap`, `mprotect`).
const KEEPER_MAPS: &[(&str, Bound)] = &[
    ("mmap", Bound::Word),
    ("munmap", Bound::Word),
    ("mprotect", Bound::Word),
];

pub(super) const KEEPER_NO_MMAP: Rule = Rule {
    id: "keeper-no-mmap",
    why: "the keeper holds each session's crash-restore region beside its master and never maps \
          it: mapped, every session's screen and scrollback would sit in the keeper's memory and \
          jetsam's ledger, against its rule that it never touches PTY bytes (keeper decision \
          10), on which the region's privacy argument rests",
    scope: KEEPER_OWN,
    check: keeper_no_mmap,
};

fn keeper_no_mmap(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    for file in tree.corpus.under(KEEPER_OWN) {
        if tree.corpus.test_code().holds(&file.rel) {
            continue;
        }
        for line in file.shipped_lines() {
            for (token, bound) in KEEPER_MAPS {
                if has(line.code, token, *bound) {
                    out.push(file.finding(
                        KEEPER_NO_MMAP.id,
                        line.no,
                        format!("names `{token}`: the keeper maps no region"),
                    ));
                }
            }
        }
    }
}

/// The crash restore's own source: the region's layout, records, writer and recovery.
const RESTORE_OWN: &[&str] = &["crates/aterm-restore/src/"];
/// The crash restore's crate, as a closure names it.
const RESTORE_CRATE: &str = "crates/aterm-restore";
/// Crates that carry bytes off the host.
const OFF_HOST_CRATES: &[&str] = &[
    "crates/aterm-link",
    "crates/aterm-net",
    "crates/aterm-mesh",
    "crates/aterm-http",
    "crates/aterm-gui-fabric",
];

/// Socket types, syscalls by name, and off-host crates.
const SOCKET_APIS: &[(&str, Bound)] = &[
    ("std::net", Bound::Sub),
    ("UnixStream", Bound::Word),
    ("UnixListener", Bound::Word),
    ("UnixDatagram", Bound::Word),
    ("TcpStream", Bound::Word),
    ("TcpListener", Bound::Word),
    ("UdpSocket", Bound::Word),
    ("CtlStream", Bound::Word),
    ("CtlListener", Bound::Word),
    ("socket", Bound::Word),
    ("sendmsg", Bound::Word),
    ("recvmsg", Bound::Word),
    ("sendto", Bound::Word),
    ("recvfrom", Bound::Word),
    ("aterm_link", Bound::Word),
];

pub(super) const RESTORE_STAYS_LOCAL: Rule = Rule {
    id: "restore-stays-local",
    why: "a crash-restore region is the window's memory (screens, scrollback, and the nonce a \
          shell prints in its marks) and never leaves the host: no crate that carries bytes off \
          it (aterm-link, aterm-net, aterm-http, aterm-gui-fabric) may have aterm-restore in its \
          closure, and aterm-restore's own source names no socket (the plan's privacy claim 5; \
          status reports counts only, and no control verb reads a region)",
    scope: RESTORE_OWN,
    check: restore_stays_local,
};

fn restore_stays_local(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    use crate::scan_set::member_reach;
    for file in tree.corpus.under(RESTORE_OWN) {
        if tree.corpus.test_code().holds(&file.rel) {
            continue;
        }
        for line in file.shipped_lines() {
            for (token, bound) in SOCKET_APIS {
                if has(line.code, token, *bound) {
                    out.push(file.finding(
                        RESTORE_STAYS_LOCAL.id,
                        line.no,
                        format!("names `{token}`: the crash restore reaches no socket"),
                    ));
                }
            }
        }
    }
    for dir in OFF_HOST_CRATES {
        let manifest = format!("{dir}/Cargo.toml");
        if !tree.root.join(&manifest).is_file() {
            continue;
        }
        match member_reach(tree.root, dir) {
            Ok(reach) => {
                if let Some(chain) = reach.get(RESTORE_CRATE) {
                    out.push(Finding {
                        rule: RESTORE_STAYS_LOCAL.id,
                        path: manifest,
                        line: 0,
                        detail: format!(
                            "aterm-restore is in {dir}'s closure (through {}): a crash-restore \
                             region would be one dependency from the fabric",
                            chain.join(" -> ")
                        ),
                    });
                }
            }
            Err(why) => out.push(Finding {
                rule: RESTORE_STAYS_LOCAL.id,
                path: manifest,
                line: 0,
                detail: format!("{dir}'s closure cannot be derived: {why}"),
            }),
        }
    }
}

#[cfg(test)]
#[path = "keeper_gui_tests.rs"]
mod keeper_gui_tests;

/// The keeper's crate: the root of the process closure.
const KEEPER_CRATE: &str = "crates/aterm-keeper";

/// Is `dep` (a dependency name) a GUI crate from outside the workspace?
fn is_external_gui(dep: &str) -> bool {
    matches!(dep, "winit" | "core-graphics" | "cocoa" | "wgpu") || dep.starts_with("objc")
}

pub(super) const KEEPER_GUI_DEPS: Rule = Rule {
    id: "keeper-gui-deps",
    why: "declared GUI frameworks/APIs in the keeper's resolved dependency closure enter its process: \
          inspect library/build sources rather than crate names, and conservatively reject \
          opaque external GUI packages reached through Cargo.lock",
    scope: &[],
    check: keeper_gui_deps,
};

/// One step of the keeper's closure walk: a workspace crate or a `Cargo.lock` package.
enum Hop {
    Member(String),
    Locked(usize),
}

fn keeper_gui_deps(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    use crate::scan_set::{
        derive_process_scan_set, external_dependency_names, member_patch_targets, read_lock,
    };
    let finding = |path: String, detail: String| Finding {
        rule: KEEPER_GUI_DEPS.id,
        path,
        line: 0,
        detail,
    };
    let keeper = format!("{KEEPER_CRATE}/Cargo.toml");
    let closure = |dir: &str| {
        derive_process_scan_set(tree.root, &[dir]).map(|set| {
            set.scan_dirs
                .iter()
                .map(|d| d.strip_suffix("/src").unwrap_or(d).to_string())
                .collect::<Vec<_>>()
        })
    };
    let (members, lock, patched) = match (
        closure(KEEPER_CRATE),
        read_lock(tree.root),
        member_patch_targets(tree.root),
    ) {
        (Ok(m), Ok(l), Ok(p)) => (m, l, p),
        (m, l, p) => {
            for why in [m.err(), l.err(), p.err()].into_iter().flatten() {
                out.push(finding(
                    keeper.clone(),
                    format!("the keeper's process closure cannot be derived: {why}"),
                ));
            }
            return;
        }
    };
    let mut seen_members: std::collections::BTreeSet<String> = members.iter().cloned().collect();
    let mut seen_locked = vec![false; lock.len()];
    let mut queue: std::collections::VecDeque<(Hop, String, Vec<String>)> = members
        .into_iter()
        .map(|dir| {
            let at = format!("{dir}/Cargo.toml");
            (Hop::Member(dir), at, Vec::new())
        })
        .collect();
    while let Some((hop, at, chain)) = queue.pop_front() {
        let through = |name: &str| {
            let mut c = chain.clone();
            c.push(name.to_string());
            c
        };
        let mut enqueue_name = |name: &str, version: Option<&str>, chain: Vec<String>| {
            let mut found = false;
            for (i, p) in lock.iter().enumerate() {
                if p.name == name && version.is_none_or(|v| p.version == v) {
                    found = true;
                    if !std::mem::replace(&mut seen_locked[i], true) {
                        queue.push_back((Hop::Locked(i), at.clone(), chain.clone()));
                    }
                }
            }
            found
        };
        match hop {
            Hop::Member(dir) => {
                out.extend(super::keeper_native::inspect(tree, &dir));
                match external_dependency_names(tree.root, &dir) {
                    Ok(deps) => {
                        for dep in deps {
                            if is_external_gui(&dep) {
                                out.push(finding(
                                    at.clone(),
                                    format!("depends on `{dep}`, inside the keeper's process"),
                                ));
                            } else if !enqueue_name(&dep, None, through(&dep)) {
                                out.push(finding(
                                    at.clone(),
                                    format!(
                                        "depends on `{dep}`, which Cargo.lock does not hold \
                                         (a stale lock), so its closure is unread"
                                    ),
                                ));
                            }
                        }
                    }
                    Err(why) => out.push(finding(
                        format!("{dir}/Cargo.toml"),
                        format!("unreadable manifest: {why}"),
                    )),
                }
            }
            Hop::Locked(i) => {
                let p = &lock[i];
                if is_external_gui(&p.name) && !chain.is_empty() {
                    out.push(finding(
                        at.clone(),
                        format!(
                            "reaches `{}` through {}, inside the keeper's process",
                            p.name,
                            chain.join(" -> ")
                        ),
                    ));
                }
                if p.source.is_none()
                    && let Some(path) = patched.get(&p.name)
                {
                    match closure(path) {
                        Ok(dirs) => {
                            for dir in dirs {
                                if seen_members.insert(dir.clone()) {
                                    queue.push_back((Hop::Member(dir), at.clone(), chain.clone()));
                                }
                            }
                        }
                        Err(why) => out.push(finding(
                            format!("{path}/Cargo.toml"),
                            format!("the closure of `{}` cannot be derived: {why}", p.name),
                        )),
                    }
                    continue;
                }
                for (dep, version) in &p.deps {
                    enqueue_name(dep, version.as_deref(), through(dep));
                }
            }
        }
    }
}

/// The crate that owns child construction, and the alias's home.
const UDS_CRATE: &str = "crates/aterm-uds/";

pub(super) const STD_COMMAND: Rule = Rule {
    id: "std-command",
    why: "aterm_uds::spawnfd::StdCommand is std::process::Command under another name, and \
          clippy's disallowed_types row does not see through an alias: a command built from it \
          hands its child every descriptor not yet close-on-exec (clippy.toml, ONE CHILD \
          CONSTRUCTOR). Outside aterm-uds the alias only types a command aterm_uds::spawnfd \
          built; it is never constructed, renamed or re-aliased",
    scope: &[UDS_CRATE],
    check: std_command,
};

fn std_command(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    for file in tree.corpus.under(PRODUCT) {
        if file.rel.starts_with(UDS_CRATE) || !file.mentions(&["StdCommand"]) {
            continue;
        }
        for line in file.lines() {
            if let Some(what) = std_command_door(line.code) {
                out.push(file.finding(STD_COMMAND.id, line.no, what.to_string()));
            }
        }
    }
}

/// The door one line of code opens on the alias (`StdCommand::new`, `StdCommand as …`, or
/// `type … = StdCommand`).
fn std_command_door(code: &str) -> Option<&'static str> {
    for at in hits(code, "StdCommand", Bound::Word) {
        let rest = &code[at + "StdCommand".len()..];
        if after_ws(rest, "::").is_some_and(|r| {
            r.trim_start_matches([' ', '\t'])
                .strip_prefix("new")
                .is_some_and(|t| !t.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_'))
        }) {
            return Some("constructs a command from the StdCommand alias (`StdCommand::new`)");
        }
        if rest
            .trim_start_matches([' ', '\t'])
            .strip_prefix("as")
            .is_some_and(|t| t.starts_with([' ', '\t']))
        {
            return Some("renames the StdCommand alias (`StdCommand as …`)");
        }
        if has(&code[..at], "type", Bound::Word) && path_prefix_trimmed(&code[..at]).ends_with('=')
        {
            return Some("re-aliases StdCommand (`type … = StdCommand`)");
        }
    }
    None
}

/// `s` without trailing `a::b::` path segments and whitespace.
fn path_prefix_trimmed(s: &str) -> &str {
    let mut s = s.trim_end();
    while let Some(r) = s.strip_suffix("::") {
        s = r
            .trim_end()
            .trim_end_matches(|c: char| c.is_ascii_alphanumeric() || c == '_')
            .trim_end();
    }
    s
}

pub(super) const FLOCK_DOOR: Rule = Rule {
    id: "flock-door",
    why: "an flock lives on the open file description, so a lock let go by dropping its File \
          stays held in every child forked meanwhile; clippy.toml fences each take it can name \
          to a guard that releases by LOCK_UN, and these doors name no path it can fence",
    scope: &[],
    check: flock_door,
};

/// `libc`'s own crate declares the ABI the lint names.
const LIBC_CRATE: &str = "crates/aterm-libc/";

fn flock_door(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    for file in tree.corpus.under(PRODUCT) {
        if file.rel.starts_with(LIBC_CRATE)
            || !file.mentions(&["flock", "O_EXLOCK", "O_SHLOCK", "F_OFD_SETLK", "syscall"])
        {
            continue;
        }
        for line in file.lines() {
            if let Some(door) = flock_line(line) {
                out.push(file.finding(FLOCK_DOOR.id, line.no, door.to_string()));
            }
        }
        for no in syscalls_by_number(&file.lexed().code) {
            out.push(file.finding(
                FLOCK_DOOR.id,
                no,
                "a raw syscall by number, whose call the lint cannot see".to_string(),
            ));
        }
    }
}

/// The flock door one line of code opens, if any.
fn flock_line(line: Line<'_>) -> Option<&'static str> {
    let code = line.code;
    let fn_flock = hits(code, "fn", Bound::Word).any(|at| {
        let rest = &code[at + 2..];
        let name = rest.trim_start_matches([' ', '\t']);
        name.len() < rest.len()
            && name
                .strip_prefix("flock")
                .is_some_and(|r| r.trim_start_matches([' ', '\t']).starts_with(['(', '<']))
    });
    if fn_flock {
        return Some("a private `fn flock` declaration");
    }
    let linked = hits(code, "link_name", Bound::Word).any(|at| {
        let Some(value) = after_ws(&code[at + 9..], "=") else {
            return false;
        };
        let quote = code.len() - value.trim_start_matches([' ', '\t']).len();
        line.text
            .get(quote..)
            .is_some_and(|lit| lit.starts_with("\"flock\""))
    });
    if linked {
        return Some("an `flock` bound under another name (link_name)");
    }
    if has(code, "O_EXLOCK", Bound::Word) || has(code, "O_SHLOCK", Bound::Word) {
        return Some("an open that takes the lock (O_EXLOCK/O_SHLOCK)");
    }
    if has(code, "SYS_flock", Bound::Word) {
        return Some("a raw `SYS_flock` syscall");
    }
    if has(code, "F_OFD_SETLK", Bound::Word) || has(code, "F_OFD_SETLKW", Bound::Word) {
        return Some("an open-file-description lock (F_OFD_SETLK/F_OFD_SETLKW)");
    }
    None
}

/// The 1-based lines of `syscall(<digit>…)` calls in a file's code, spanning lines or not.
fn syscalls_by_number(code: &str) -> Vec<usize> {
    let mut out = Vec::new();
    for at in hits(code, "syscall", Bound::Sub) {
        if at > 0 && is_word(code.as_bytes()[at - 1]) {
            continue;
        }
        let rest = code[at + 7..].trim_start();
        let Some(arg) = rest.strip_prefix('(') else {
            continue;
        };
        if arg.trim_start().starts_with(|c: char| c.is_ascii_digit()) {
            out.push(code[..at].matches('\n').count() + 1);
        }
    }
    out
}

/// The supervisor's source directory (including `policy/`).
const SUPERVISE: &[&str] = &["crates/aterm-agent/src/supervise/"];

/// Supervisor files where a direct clock read is allowed.
const SUPERVISE_CLOCK_HOMES: &[&str] = &[
    "crates/aterm-agent/src/supervise/fx.rs",
    "crates/aterm-agent/src/supervise/tape/",
    "crates/aterm-agent/src/supervise/transport.rs",
    "crates/aterm-agent/src/supervise/mail.rs",
];

/// How a clock name must sit in a line to be a read.
#[derive(Clone, Copy)]
pub(super) enum ClockShape {
    /// Anywhere (a word): `Instant::now`, `thread::sleep`, called or passed.
    Named,
    /// A free reader: called (`unix_ms(…)`) or named by a path (`limit::unix_now`, passed as a
    /// fn item).
    Reader,
    /// A method: called on a receiver (`t.elapsed()`) or named by a path (`Instant::elapsed`),
    /// never a field of the same name.
    Method,
    /// A free function: called bare (`read_small(…)`) or by a path (`limit::offset_at(…)`,
    /// `footer::shell_cwds`).
    Free,
    /// The head of a path that is called: `fs::read(…)`, `std::fs::OpenOptions::new()`,
    /// `File::open(…)`.
    PathCall,
}

/// Each direct clock read or sleep, its shape, and what it is.
const SUPERVISE_CLOCKS: &[(&str, ClockShape, &str)] = &[
    ("Instant::now", ClockShape::Named, "the monotonic clock"),
    ("SystemTime::now", ClockShape::Named, "the wall clock"),
    ("thread::sleep", ClockShape::Named, "a sleep"),
    (
        "thread::sleep_until",
        ClockShape::Named,
        "a sleep (`thread::sleep_until`)",
    ),
    (
        "thread::sleep_ms",
        ClockShape::Named,
        "a sleep (`thread::sleep_ms`)",
    ),
    (
        "thread::park_timeout",
        ClockShape::Named,
        "a sleep (`thread::park_timeout`)",
    ),
    (
        "elapsed",
        ClockShape::Method,
        "the monotonic clock (`.elapsed()`)",
    ),
    (
        "unix_now",
        ClockShape::Reader,
        "the wall clock (`limit::unix_now`)",
    ),
    (
        "unix_now_ms",
        ClockShape::Reader,
        "the wall clock (`limit::unix_now_ms`)",
    ),
    (
        "unix_ms",
        ClockShape::Reader,
        "the wall clock (`journal::unix_ms`)",
    ),
    (
        "unix_secs",
        ClockShape::Reader,
        "the wall clock (`aterm_time::unix_secs`)",
    ),
    (
        "unix_millis",
        ClockShape::Reader,
        "the wall clock (`aterm_time::unix_millis`)",
    ),
    (
        "since_epoch",
        ClockShape::Reader,
        "the wall clock (`aterm_time::since_epoch`)",
    ),
];

/// The phase-2 reads: process id, cross-thread channel receives, and path-based approval ledger
/// reads and cuts recorded by `Fx`.
const SUPERVISE_READS: &[(&str, ClockShape, &str)] = &[
    (
        "process::id",
        ClockShape::Named,
        "this process's id (`std::process::id`)",
    ),
    (
        "try_recv",
        ClockShape::Method,
        "a delivery from another thread (`.try_recv()`)",
    ),
    (
        "recv_timeout",
        ClockShape::Method,
        "a delivery from another thread (`.recv_timeout()`)",
    ),
    (
        "approvals::typed_at",
        ClockShape::Named,
        "the approval ledger by its path (`approvals::typed_at`)",
    ),
    (
        "approvals::typed_texts",
        ClockShape::Named,
        "the approval ledger by its path (`approvals::typed_texts`)",
    ),
    (
        "approvals::usage_attempt_at",
        ClockShape::Named,
        "the approval ledger by its path (`approvals::usage_attempt_at`)",
    ),
    (
        "approvals::account_moves",
        ClockShape::Named,
        "the approval ledger by its path (`approvals::account_moves`)",
    ),
    (
        "approvals::open_account_move",
        ClockShape::Named,
        "the approval ledger by its path (`approvals::open_account_move`)",
    ),
    (
        "approvals::open_wind_down",
        ClockShape::Named,
        "the approval ledger by its path (`approvals::open_wind_down`)",
    ),
    (
        "approvals::open_wall",
        ClockShape::Named,
        "the approval ledger by its path (`approvals::open_wall`)",
    ),
    (
        "approvals::bound",
        ClockShape::Named,
        "a cut of a per-session file (`approvals::bound`)",
    ),
];

/// Functions where a phase-2 read is allowed, by file, name and the read.
const SUPERVISE_READ_HOMES: &[(&str, &str, &str)] =
    &[("claim.rs", "default_holder", "process::id")];

/// Functions that wrap the system wall clock.
const SUPERVISE_READER_HOMES: &[(&str, &str)] = &[
    ("journal.rs", "unix_ms"),
    ("limit.rs", "unix_now"),
    ("limit.rs", "unix_now_ms"),
];

/// The phase-3 looks: filesystem, process, kernel, timezone, and environment queries routed
/// through `PolicyProbe` or `Fx`.
const SUPERVISE_LOOKS: &[(&str, ClockShape, &str)] = &[
    ("fs", ClockShape::PathCall, "the filesystem (`std::fs`)"),
    ("File", ClockShape::PathCall, "a file opened (`File::open`)"),
    (
        "read_dir",
        ClockShape::Named,
        "a folder's listing (`read_dir`)",
    ),
    (
        "canonicalize",
        ClockShape::Named,
        "where a path leads (`canonicalize`)",
    ),
    (
        "symlink_metadata",
        ClockShape::Named,
        "a link on the disk (`symlink_metadata`)",
    ),
    (
        "read_link",
        ClockShape::Named,
        "a link's target (`read_link`)",
    ),
    (
        "OpenOptions",
        ClockShape::Named,
        "a file opened (`OpenOptions`)",
    ),
    (
        "metadata",
        ClockShape::Method,
        "a file's metadata (`.metadata()`)",
    ),
    ("exists", ClockShape::Method, "the disk (`.exists()`)"),
    ("is_dir", ClockShape::Method, "the disk (`.is_dir()`)"),
    ("is_file", ClockShape::Method, "the disk (`.is_file()`)"),
    (
        "spawnfd::command",
        ClockShape::Named,
        "a child process (`spawnfd::command`)",
    ),
    (
        "process_args",
        ClockShape::Named,
        "a process's environment and argv (`process_args`)",
    ),
    (
        "aterm_sysprobe",
        ClockShape::Named,
        "the kernel (`aterm_sysprobe`)",
    ),
    (
        "env::var",
        ClockShape::Named,
        "this process's environment (`std::env::var`)",
    ),
    (
        "account_home",
        ClockShape::Named,
        "the passwd database (`account_home`)",
    ),
    (
        "tz_offset_s",
        ClockShape::Free,
        "the zone (`limit::tz_offset_s`)",
    ),
    (
        "offset_at",
        ClockShape::Free,
        "a zone's offset (`limit::offset_at`)",
    ),
    (
        "zone_offset_at",
        ClockShape::Free,
        "a zone's offset (`limit::zone_offset_at`)",
    ),
    (
        "local_offset_at",
        ClockShape::Free,
        "the local zone's offset (`limit::local_offset_at`)",
    ),
    (
        "self.zone_offset",
        ClockShape::Named,
        "the session's zone reader (`Session::zone_offset`)",
    ),
    (
        "self.local_offset_at",
        ClockShape::Named,
        "the session's local-offset reader (`Session::local_offset_at`)",
    ),
    (
        "read_small",
        ClockShape::Free,
        "a file (`footer::read_small`)",
    ),
    (
        "open_regular",
        ClockShape::Free,
        "a file (`footer::open_regular`)",
    ),
    (
        "shell_cwds",
        ClockShape::Free,
        "Claude Code's transcripts (`footer::shell_cwds`)",
    ),
    (
        "under_file_provider_domain",
        ClockShape::Free,
        "the disk (`footer::under_file_provider_domain`)",
    ),
    (
        "LiveProbe",
        ClockShape::Named,
        "the machine through the live probe (`LiveProbe`), whose looks no tape holds",
    ),
];

/// Supervisor files where every machine look is allowed.
const SUPERVISE_LOOK_FILES: &[&str] = &[
    "crates/aterm-agent/src/supervise/policy/probe_live.rs",
    "crates/aterm-agent/src/supervise/policy/probe.rs",
];

/// Functions where a machine look is allowed, by supervisor-relative file, name, and reason.
const SUPERVISE_LOOK_HOMES: &[(&str, &str, &str)] = &[
    ("policy/git_config.rs", "git", GIT_VIEW),
    ("policy/git_config.rs", "view_at", GIT_VIEW),
    ("policy/git_config.rs", "is_executable", GIT_VIEW),
    ("policy/git_config.rs", "claude_dir", WORKER_LIVE),
    ("policy/git_config.rs", "claude_dir_at", INJECTED),
    ("policy/claude_settings.rs", "managed_preferences", SETTINGS),
    ("policy/claude_settings.rs", "main_worktree", SETTINGS),
    ("policy/claude_settings.rs", "drop_ins", SETTINGS),
    ("policy/claude_settings.rs", "opens", SETTINGS),
    ("policy/claude_settings.rs", "read_settings_in", SETTINGS),
    ("policy/shell_startup.rs", "read", SHELL),
    ("policy/shell_startup.rs", "snapshots", SHELL),
    ("policy/shell_startup.rs", "hook_env_scripts", SHELL),
    ("policy/shell_startup.rs", "zshenv", SHELL),
    ("limit.rs", "tz_offset_s", ZONE),
    ("limit.rs", "zone_offset_at", ZONE),
    ("limit.rs", "local_offset_at", ZONE),
    ("limit.rs", "read_offset", ZONE),
    ("limit.rs", "offset_in", INJECTED),
    ("limit.rs", "reset_in_zone", INJECTED),
    ("run.rs", "zone_lookup", FIELD_DOOR),
    ("run.rs", "local_offset_then", FIELD_DOOR),
    ("run.rs", "from_setup", FIELD_DOOR),
    ("run.rs", "append_note_at", OUTPUT),
    ("journal.rs", "open", OUTPUT),
    ("approvals.rs", "path_under", BY_PATH),
    ("approvals.rs", "typed_texts", BY_PATH),
    ("turn_end_loop.rs", "standing_rules", BY_PATH),
    ("journal.rs", "read_journal", BY_PATH),
    ("config.rs", "from_path", BY_PATH),
];

const GIT_VIEW: &str = "the live body of `probe.git_view`";
const WORKER_LIVE: &str = "read inside the live bodies of the worker's probes";
const INJECTED: &str = "the reader is an injected parameter of that name";
const SETTINGS: &str = "the live body of `probe.settings_env`/`owner_asks`/`shell_startup`";
const SHELL: &str = "the live body of `probe.shell_startup`";
const ZONE: &str = "a zone reader, asked through `Fx`'s derived doors";
const FIELD_DOOR: &str = "an injected reader's one door, or its default named";
const OUTPUT: &str = "an output a door already records";
const BY_PATH: &str = "read by its path for the host or the CLI, never by the loop";

/// Reasons in [`SUPERVISE_LOOK_HOMES`] that designate a derived door's live body.
const LIVE_BODY: &[&str] = &[GIT_VIEW, WORKER_LIVE, SETTINGS, SHELL, ZONE];

/// One `fn` of a live body ([`live_readers`]) with its file, module, name, and call shape.
struct LiveReader<'t> {
    rel: &'t str,
    module: &'t str,
    name: &'t str,
    /// It takes `self`: called `.name(…)` or by a type's path.
    method: bool,
    /// Visible outside its module (`pub`, `pub(crate)`, …).
    public: bool,
}

/// Every function of a derived door's live body across the [`SUPERVISE_LOOK_HOMES`] files.
fn live_readers<'t>(tree: &'t Tree<'_>) -> Vec<LiveReader<'t>> {
    let mut files: Vec<(&SourceFile, Vec<&'static str>)> = Vec::new();
    for &(home, name, why) in SUPERVISE_LOOK_HOMES {
        if !LIVE_BODY.contains(&why) {
            continue;
        }
        let rel = format!("{}{home}", SUPERVISE[0]);
        let Some(file) = tree.corpus.get(&rel) else {
            continue;
        };
        match files.iter_mut().find(|(f, _)| f.rel == rel) {
            Some((_, homes)) => homes.push(name),
            None => files.push((file, vec![name])),
        }
    }
    let graphs: Vec<_> = files
        .iter()
        .map(|(file, homes)| {
            let injected = |func: &str| {
                SUPERVISE_LOOK_HOMES.iter().any(|&(home, name, why)| {
                    why == INJECTED && name == func && file.rel.ends_with(home)
                })
            };
            let attributed = super::graph::attribute(file);
            let mut callers: std::collections::BTreeMap<&str, Vec<&str>> =
                std::collections::BTreeMap::new();
            let mut lines: Vec<(&str, &str)> = Vec::new();
            for l in &attributed {
                let Some(func) = l.func.filter(|f| !l.in_test && !injected(f)) else {
                    continue;
                };
                lines.push((func, l.line.code));
                for callee in super::calls(l.line.code) {
                    if callee != func {
                        callers.entry(callee).or_default().push(func);
                    }
                }
            }
            (*file, homes, callers, lines, attributed)
        })
        .collect();
    let mut readers: Vec<LiveReader<'t>> = Vec::new();
    loop {
        let mut grown = Vec::new();
        for (file, homes, callers, lines, attributed) in &graphs {
            let across = lines
                .iter()
                .filter(|(_, code)| live_call(code, &file.rel, &readers).is_some())
                .map(|(func, _)| *func);
            for name in super::graph::reach(homes.iter().copied().chain(across), callers).keys() {
                let known = |r: &LiveReader<'_>| r.rel == file.rel && r.name == *name;
                if readers.iter().chain(&grown).any(known) {
                    continue;
                }
                let Some((method, public)) = signature(attributed, name) else {
                    continue;
                };
                let module = file.name().strip_suffix(".rs").unwrap_or(file.name());
                grown.push(LiveReader {
                    rel: &file.rel,
                    module,
                    name,
                    method,
                    public,
                });
            }
        }
        if grown.is_empty() {
            return readers;
        }
        readers.extend(grown);
    }
}

/// Whether the shipped `fn name` of `attributed` takes `self`, and whether it is visible outside
/// its module.
fn signature(attributed: &[super::graph::FnLine<'_>], name: &str) -> Option<(bool, bool)> {
    let start = attributed.iter().position(|l| {
        !l.in_test
            && l.func == Some(name)
            && hits(l.line.code, "fn", Bound::Word).any(|at| {
                after_ws(&l.line.code[at + 2..], name)
                    .is_some_and(|rest| rest.bytes().next().is_none_or(|b| !is_word(b)))
            })
    })?;
    let head = attributed[start].line.code;
    let public = has(&head[..head.find("fn").unwrap_or(0)], "pub", Bound::Word);
    let mut sig = String::new();
    for l in &attributed[start..] {
        sig.push_str(l.line.code);
        sig.push(' ');
        if l.line.code.contains('{') || l.line.code.trim_end().ends_with(';') {
            break;
        }
    }
    Some((has(&sig, "self", Bound::Word), public))
}

/// Finds a live-body reader from another file that `code` in supervisor file `rel` calls.
fn live_call<'r, 't>(
    code: &str,
    rel: &str,
    readers: &'r [LiveReader<'t>],
) -> Option<&'r LiveReader<'t>> {
    readers.iter().find(|r| {
        // Either call spells the name: a line that does not is left unread.
        if r.rel == rel || !r.public || !code.contains(r.name) {
            return false;
        }
        if r.method {
            return method_call(code, r.name);
        }
        module_path(code, r)
    })
}

/// Whether `code` calls the method `name`: `.name(…)` or by a type's path.
fn method_call(code: &str, name: &str) -> bool {
    hits(code, name, Bound::Word).any(|at| {
        let before = code[..at].trim_end_matches([' ', '\t']);
        let after = code[at + name.len()..].trim_start_matches([' ', '\t']);
        before.ends_with("::") || (after.starts_with('(') && before.ends_with('.'))
    })
}

/// Whether `code` names the free live body `r` by its module's path.
fn module_path(code: &str, r: &LiveReader<'_>) -> bool {
    hits(code, &format!("{}::{}", r.module, r.name), Bound::Word)
        .next()
        .is_some()
}

/// The imports of a file, and the code outside their actual spans.
pub(super) struct UseStatements {
    /// Each statement, joined across its lines, with its first line's number.
    pub(super) statements: Vec<(usize, String)>,
    /// Only lines containing imports, with those spans blanked in place.
    pub(super) executable: std::collections::BTreeMap<usize, String>,
}

/// Each `use` statement of `file`'s shipped code.
pub(super) fn use_statements(file: &SourceFile) -> UseStatements {
    let mut out = UseStatements {
        statements: Vec::new(),
        executable: std::collections::BTreeMap::new(),
    };
    let mut open = false;
    for line in file.shipped_lines() {
        let mut rest = line.code;
        let mut start = 0;
        loop {
            if !open {
                let Some(offset) = next_use_item(rest) else {
                    break;
                };
                start += offset;
                rest = &rest[offset..];
                out.statements.push((line.no, String::new()));
            }
            let end = rest.find(';').map(|at| at + 1);
            let len = end.unwrap_or(rest.len());
            if let Some((_, text)) = out.statements.last_mut() {
                text.push_str(&rest[..len]);
                text.push(' ');
            }
            let code = out
                .executable
                .entry(line.no)
                .or_insert_with(|| line.code.to_string());
            code.replace_range(start..start + len, &" ".repeat(len));
            open = end.is_none();
            if open {
                break;
            }
            start += len;
            rest = &rest[len..];
        }
    }
    out
}

/// Byte offset of the next `use` item in `code` (including any `pub` visibility prefix), when
/// `use` stands at the start of `code` or after `{`, `}`, `;` or `]`.
fn next_use_item(code: &str) -> Option<usize> {
    for at in hits(code, "use", Bound::Word) {
        if !code[at + 3..].starts_with(|c: char| c.is_ascii_whitespace() || c == ':') {
            continue;
        }
        let boundary = code[..at].rfind(['{', '}', ';', ']']).map_or(0, |p| p + 1);
        let prefix = code[boundary..at].trim();
        if prefix.is_empty() || is_pub_vis(prefix) {
            let item_start =
                boundary + (code[boundary..at].len() - code[boundary..at].trim_start().len());
            return Some(item_start);
        }
    }
    None
}

fn is_pub_vis(s: &str) -> bool {
    let Some(rest) = s.strip_prefix("pub") else {
        return false;
    };
    let rest = rest.trim_start();
    rest.is_empty() || (rest.starts_with('(') && rest.ends_with(')'))
}

/// One name a `use` statement — or a `type` alias ([`type_alias`]) — binds.
pub(super) struct UseLeaf<'s> {
    /// Its whole path, the group prefixes above it joined.
    pub(super) path: Vec<&'s str>,
    /// The name it is bound to when renamed (`as Mono`); `as _` binds nothing a call can name.
    pub(super) alias: Option<&'s str>,
    /// A glob (`std::thread::*`): `path` is the module's, and every item of it is bound bare.
    pub(super) glob: bool,
}

/// The names one `use` statement binds, each with its whole path.
pub(super) fn use_leaves(stmt: &str) -> Vec<UseLeaf<'_>> {
    let mut out = Vec::new();
    if let Some(at) = hits(stmt, "use", Bound::Word).next() {
        let tree = stmt[at + 3..].split(';').next().unwrap_or("");
        use_tree(tree, &[], &mut out);
    }
    out
}

/// Expands one `use` tree below `prefix` into `out`.
fn use_tree<'s>(tree: &'s str, prefix: &[&'s str], out: &mut Vec<UseLeaf<'s>>) {
    let tree = tree.trim();
    if let Some(open) = tree.find('{') {
        let mut path = prefix.to_vec();
        path.extend(segments(&tree[..open]));
        let close = tree.rfind('}').filter(|&c| c > open).unwrap_or(tree.len());
        for member in top_level_members(&tree[open + 1..close]) {
            use_tree(member, &path, out);
        }
        return;
    }
    let (body, alias) = match hits(tree, "as", Bound::Word).next() {
        Some(at) => (
            &tree[..at],
            Some(tree[at + 2..].trim()).filter(|a| !a.is_empty()),
        ),
        None => (tree, None),
    };
    let mut path = prefix.to_vec();
    let mut glob = false;
    for segment in segments(body) {
        match segment {
            "*" => glob = true,
            "self" if !path.is_empty() => {}
            s => path.push(s),
        }
    }
    if !path.is_empty() {
        out.push(UseLeaf { path, alias, glob });
    }
}

/// A path's segments (`std :: thread` and `::std::thread` alike).
fn segments(path: &str) -> impl Iterator<Item = &str> {
    path.split("::").map(str::trim).filter(|s| !s.is_empty())
}

/// A group's members: split at its own commas, never a nested group's.
fn top_level_members(group: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut depth, mut start) = (0usize, 0usize);
    for (i, c) in group.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                out.push(&group[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&group[start..]);
    out
}

/// A `type` alias of a plain path, read as the import it amounts to
/// (`type Mono = std::time::Instant;` binds `Instant` as `Mono`, as a `use … as Mono` does).
pub(super) fn type_alias(code: &str) -> Option<UseLeaf<'_>> {
    let at = hits(code, "type", Bound::Word).next()?;
    let head = code[..at]
        .rsplit(['{', '}', ';', ']'])
        .next()
        .unwrap_or(&code[..at])
        .trim();
    if !(head.is_empty() || is_pub_vis(head)) {
        return None;
    }
    let rest = code[at + 4..].trim_start();
    let len = rest.bytes().take_while(|&b| is_word(b)).count();
    let (rhs, _) = after_ws(&rest[len..], "=")?.trim().split_once(';')?;
    let rhs = rhs.trim();
    if len == 0 || rhs.is_empty() || !rhs.bytes().all(|b| is_word(b) || b == b':') {
        return None;
    }
    Some(UseLeaf {
        path: segments(rhs).collect(),
        alias: Some(&rest[..len]),
        glob: false,
    })
}

/// Whether an imported leaf binds a clock read, sleep or phase-2 read out of the line view's
/// sight.
fn imported_read(
    leaf: &UseLeaf<'_>,
    by_path: &std::collections::BTreeSet<&str>,
) -> Option<(&'static str, String)> {
    if leaf.alias == Some("_") {
        return None;
    }
    let shown = leaf.path.join("::");
    let renamed = |name: &str| leaf.alias.is_some_and(|a| a != name);
    let as_written = leaf
        .alias
        .map_or_else(|| shown.clone(), |a| format!("{shown} as {a}"));
    for &(needle, shape, what) in SUPERVISE_CLOCKS.iter().chain(SUPERVISE_READS) {
        if matches!(shape, ClockShape::Method) {
            continue;
        }
        let parts: Vec<&str> = needle.split("::").collect();
        let ends = |k: usize| leaf.path.ends_with(&parts[..k]);
        if leaf.glob {
            if parts.len() > 1 && ends(parts.len() - 1) {
                return Some((
                    needle,
                    format!(
                        "imports `{shown}::*`, so {what} is named bare past the session's `Fx`"
                    ),
                ));
            }
            continue;
        }
        if ends(parts.len()) {
            let unseen = parts.len() > 1 || matches!(shape, ClockShape::Reader);
            if unseen || renamed(parts[parts.len() - 1]) {
                return Some((
                    needle,
                    format!(
                        "binds {what} to a bare name (`{as_written}`), past the session's `Fx` \
                         and out of the guard's sight: name it by its path"
                    ),
                ));
            }
            continue;
        }
        if let Some(k) = (1..parts.len()).find(|&k| ends(k) && renamed(parts[k - 1])) {
            return Some((
                needle,
                format!(
                    "renames `{}` (`{as_written}`), so {what} is read past the session's `Fx` \
                     under a name the guard cannot see",
                    parts[k - 1]
                ),
            ));
        }
    }
    match leaf.path.as_slice() {
        [.., "approvals", reader] if !leaf.glob && by_path.contains(reader) => Some((
            "approvals::",
            format!(
                "imports the approval ledger's reader by its path (`{as_written}`) to be called \
                 bare, past the session's `Fx`: read the text through `Fx::read_text` and hand \
                 it to `approvals::{reader}_in`"
            ),
        )),
        _ => None,
    }
}

/// Finds a free live-body reader from another file imported by `stmt`.
fn live_import<'r, 't>(
    stmt: &str,
    rel: &str,
    readers: &'r [LiveReader<'t>],
) -> Option<&'r LiveReader<'t>> {
    readers.iter().find(|r| {
        if r.method || !r.public || r.rel == rel {
            return false;
        }
        module_path(stmt, r)
            || hits(stmt, r.module, Bound::Word).any(|at| {
                let Some(group) = stmt[at + r.module.len()..].strip_prefix("::{") else {
                    return false;
                };
                let mut depth = 1usize;
                let end = group
                    .char_indices()
                    .find_map(|(i, c)| {
                        match c {
                            '{' => depth += 1,
                            '}' => depth -= 1,
                            _ => {}
                        }
                        (depth == 0).then_some(i)
                    })
                    .unwrap_or(group.len());
                has(&group[..end], r.name, Bound::Word)
            })
    })
}

/// How a phase-3 finding names the live body `r`.
fn live_name(r: &LiveReader<'_>) -> String {
    if r.method {
        format!("`.{}()` ({}.rs)", r.name, r.module)
    } else {
        format!("`{}::{}`", r.module, r.name)
    }
}

/// The rule.
pub(super) const SUPERVISE_FX: Rule = Rule {
    id: "supervise-fx",
    why: "a harness tape holds every value the supervising loop observed, in order, so a clock \
          read or a sleep that skips the session's `Fx` (supervise/fx.rs) is a hole a recorder \
          misses and a replay answers from the real clock: read it through `self.fx` (`mono`, \
          `since`, `wall_ms`, `wall_s`, `stamp_ms`, `sleep`), or take `now` from the caller — \
          and name it by its path, never imported bare, renamed or globbed in, where the guard \
          cannot follow it; \
          and so is this process's id, a delivery from another thread and the approval ledger \
          by its path: through `Fx` (`try_recv_mail`, `recv_mail_timeout`, `read_text` and an \
          `approvals::*_in` reader, `bound`), or resolved once into the setup; and so \
          is a look at the machine itself — the filesystem, a child process, the kernel, the \
          zone, this process's environment — which goes through a derived door that tapes \
          what it concluded: the approval policy's probe (`ctx.probe`, `Fx::policy_probe`, \
          its live bodies in policy/probe_live.rs) or `Fx`'s own (`tz_offset`, `zone_offset`, \
          `local_offset_at`) — never the live probe named, nor a door's live body \
          (`shell_startup::read`, `WorkerEnv::raw_git_view`, …) called or imported from above \
          its door",
    scope: SUPERVISE,
    check: supervise_fx,
};

/// A supervisor file outside the clock's homes. Its caller leaves test code out
/// ([`TestCode::holds`]).
fn supervise_fx_scope(rel: &str) -> bool {
    rel.starts_with(SUPERVISE[0])
        && !SUPERVISE_CLOCK_HOMES
            .iter()
            .any(|home| rel.starts_with(home))
}

fn supervise_fx(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    let mut needles: Vec<&str> = SUPERVISE_CLOCKS
        .iter()
        .chain(SUPERVISE_READS)
        .chain(SUPERVISE_LOOKS)
        .map(|c| c.0)
        .collect();
    needles.extend(
        SUPERVISE_CLOCKS
            .iter()
            .chain(SUPERVISE_READS)
            .flat_map(|c| c.0.split("::")),
    );
    needles.push("approvals::");
    let by_path = ledger_by_path(tree);
    let readers = live_readers(tree);
    needles.extend(readers.iter().map(|r| r.name));
    for file in tree.corpus.under(SUPERVISE) {
        if !supervise_fx_scope(&file.rel)
            || tree.corpus.test_code().holds(&file.rel)
            || !file.mentions(&needles)
        {
            continue;
        }
        let attributed = super::graph::attribute(file);
        let rel = file.rel.strip_prefix(SUPERVISE[0]).unwrap_or(&file.rel);
        let homes = reader_homes(rel, &attributed);
        let read_homes = read_homes(rel, &attributed);
        let at_home = |no: usize, needle: &str| {
            (homes.contains(&no) && wall_read(needle)) || read_homes.get(&no) == Some(&needle)
        };
        let uses = use_statements(file);
        let mut look_homes = look_homes(rel, &attributed);
        if file.rel == SUPERVISE_LEDGER {
            look_homes.extend(
                attributed
                    .iter()
                    .filter(|l| l.func.is_some_and(|f| by_path.contains(f)))
                    .map(|l| l.line.no),
            );
        }
        let looks_home = SUPERVISE_LOOK_FILES.contains(&file.rel.as_str());
        let live_file = readers.iter().any(|r| r.rel == file.rel);
        for line in file.shipped_lines() {
            let code = uses
                .executable
                .get(&line.no)
                .map_or(line.code, String::as_str);
            let read = supervise_clock(code, |needle| !at_home(line.no, needle))
                .map(|(needle, what)| (needle, format!("reads {what} past the session's `Fx`")))
                .or_else(|| {
                    type_alias(code)
                        .and_then(|leaf| imported_read(&leaf, &by_path))
                        .filter(|(needle, _)| !at_home(line.no, needle))
                });
            if let Some((_, detail)) = read {
                out.push(file.finding(SUPERVISE_FX.id, line.no, detail));
                continue;
            }
            if let Some(reader) = ledger_read(code, &by_path) {
                out.push(file.finding(
                    SUPERVISE_FX.id,
                    line.no,
                    format!(
                        "reads the approval ledger by its path (`approvals::{reader}`) past the \
                         session's `Fx`: read the text through `Fx::read_text` and hand it to \
                         `approvals::{reader}_in`"
                    ),
                ));
                continue;
            }
            if looks_home || look_homes.contains(&line.no) {
                continue;
            }
            if let Some((_, what)) = supervise_look(code) {
                out.push(file.finding(
                    SUPERVISE_FX.id,
                    line.no,
                    format!("looks at {what} past a derived door (a policy probe or `Fx`)"),
                ));
                continue;
            }
            if starts_use(code) || live_file {
                continue;
            }
            if let Some(r) = live_call(code, &file.rel, &readers) {
                out.push(file.finding(SUPERVISE_FX.id, line.no, live_bypass(r, "calls")));
            }
        }
        for (no, stmt) in &uses.statements {
            let bound = use_leaves(stmt).iter().find_map(|leaf| {
                imported_read(leaf, &by_path).filter(|(needle, _)| !at_home(*no, needle))
            });
            if let Some((_, detail)) = bound {
                out.push(file.finding(SUPERVISE_FX.id, *no, detail));
            }
        }
        if looks_home {
            continue;
        }
        for (no, stmt) in &uses.statements {
            if let Some(r) = live_import(stmt, &file.rel, &readers) {
                out.push(file.finding(SUPERVISE_FX.id, *no, live_bypass(r, "imports")));
            }
        }
    }
}

/// Detail for a live body `r` called or imported above its door.
fn live_bypass(r: &LiveReader<'_>, how: &str) -> String {
    format!(
        "{how} {}, the live body of a derived door, above its door: its look reaches no \
         tape — ask the door (`ctx.probe`, `Fx::policy_probe`, `Fx`'s own), or make this `fn` a \
         home of that live body",
        live_name(r)
    )
}

/// The approval ledger's module, whose by-path readers follow [`ledger_by_path`].
const SUPERVISE_LEDGER: &str = "crates/aterm-agent/src/supervise/approvals.rs";

/// Ledger `fn X` items paired with `fn X_in` so the loop reads through `Fx::read_text` once.
fn ledger_by_path<'t>(tree: &'t Tree<'_>) -> std::collections::BTreeSet<&'t str> {
    let Some(file) = tree.corpus.get(SUPERVISE_LEDGER) else {
        return std::collections::BTreeSet::new();
    };
    let names: std::collections::BTreeSet<&str> = super::graph::attribute(file)
        .iter()
        .filter(|l| !l.in_test)
        .filter_map(|l| l.func)
        .collect();
    names
        .iter()
        .filter(|n| names.contains(format!("{n}_in").as_str()))
        .copied()
        .collect()
}

/// The [`ledger_by_path`] reader that `code` references via `approvals::`, excluding `_in` forms.
fn ledger_read<'a>(code: &str, by_path: &std::collections::BTreeSet<&'a str>) -> Option<&'a str> {
    for at in hits(code, "approvals::", Bound::Sub) {
        if at > 0 && is_word(code.as_bytes()[at - 1]) {
            continue;
        }
        let rest = &code[at + "approvals::".len()..];
        let len = rest.bytes().take_while(|&b| is_word(b)).count();
        if let Some(reader) = by_path.get(&rest[..len]) {
            return Some(reader);
        }
    }
    None
}

/// Line numbers in `attributed` belonging to a phase-3 look's home `fn` ([`SUPERVISE_LOOK_HOMES`]).
fn look_homes(
    rel: &str,
    attributed: &[super::graph::FnLine<'_>],
) -> std::collections::BTreeSet<usize> {
    if !SUPERVISE_LOOK_HOMES.iter().any(|(home, _, _)| *home == rel) {
        return std::collections::BTreeSet::new();
    }
    attributed
        .iter()
        .filter(|l| {
            l.func.is_some_and(|func| {
                SUPERVISE_LOOK_HOMES
                    .iter()
                    .any(|(home, name, _)| *home == rel && *name == func)
            })
        })
        .map(|l| l.line.no)
        .collect()
}

/// Line numbers in `attributed` belonging to a phase-2 read's home `fn` ([`SUPERVISE_READ_HOMES`]).
fn read_homes(
    rel: &str,
    attributed: &[super::graph::FnLine<'_>],
) -> std::collections::BTreeMap<usize, &'static str> {
    if !SUPERVISE_READ_HOMES.iter().any(|(home, _, _)| *home == rel) {
        return std::collections::BTreeMap::new();
    }
    attributed
        .iter()
        .filter_map(|l| {
            let func = l.func?;
            SUPERVISE_READ_HOMES
                .iter()
                .find(|(home, name, _)| *home == rel && *name == func)
                .map(|(_, _, needle)| (l.line.no, *needle))
        })
        .collect()
}

/// Line numbers in `attributed` belonging to a wall reader's home `fn` ([`SUPERVISE_READER_HOMES`]).
fn reader_homes(
    rel: &str,
    attributed: &[super::graph::FnLine<'_>],
) -> std::collections::BTreeSet<usize> {
    if !SUPERVISE_READER_HOMES.iter().any(|(home, _)| *home == rel) {
        return std::collections::BTreeSet::new();
    }
    attributed
        .iter()
        .filter(|l| {
            l.func.is_some_and(|f| {
                SUPERVISE_READER_HOMES
                    .iter()
                    .any(|(home, name)| *home == rel && *name == f)
            })
        })
        .map(|l| l.line.no)
        .collect()
}

/// Whether `needle` reads the wall clock (`SystemTime::now` or a [`ClockShape::Reader`] entry).
fn wall_read(needle: &str) -> bool {
    needle == "SystemTime::now"
        || SUPERVISE_CLOCKS
            .iter()
            .any(|(name, shape, _)| matches!(shape, ClockShape::Reader) && *name == needle)
}

/// First direct clock read, sleep, or phase-2 read in `code` matching its [`ClockShape`].
fn supervise_clock(
    code: &str,
    include: impl Fn(&str) -> bool,
) -> Option<(&'static str, &'static str)> {
    first_hit(
        code,
        SUPERVISE_CLOCKS.iter().chain(SUPERVISE_READS),
        include,
    )
}

/// First machine look in `code` matching [`SUPERVISE_LOOKS`] in its [`ClockShape`].
fn supervise_look(code: &str) -> Option<(&'static str, &'static str)> {
    if starts_use(code) {
        return None;
    }
    first_hit(code, SUPERVISE_LOOKS.iter(), |_| true)
}

/// Whether a line of code begins a `use` statement.
fn starts_use(code: &str) -> bool {
    next_use_item(code).is_some_and(|offset| code[..offset].trim().is_empty())
}

/// The first included name that sits in `code` in its shape.
pub(super) fn first_hit<'a>(
    code: &str,
    names: impl Iterator<Item = &'a (&'static str, ClockShape, &'static str)>,
    include: impl Fn(&str) -> bool,
) -> Option<(&'static str, &'static str)> {
    // A name the line does not spell is not searched for.
    for (needle, shape, what) in names.filter(|(needle, _, _)| code.contains(needle)) {
        for at in hits(code, needle, Bound::Word) {
            let before = code[..at].trim_end_matches([' ', '\t']);
            let after = code[at + needle.len()..].trim_start_matches([' ', '\t']);
            let pathed = before.ends_with("::");
            let called = after.starts_with('(');
            let read = match shape {
                ClockShape::Named => true,
                ClockShape::Reader => pathed || (called && !ends_with_word(before, "fn")),
                ClockShape::Method => pathed || (called && before.ends_with('.')),
                ClockShape::Free => {
                    pathed || (called && !before.ends_with('.') && !ends_with_word(before, "fn"))
                }
                ClockShape::PathCall => !before.ends_with('.') && path_called(after),
            };
            if read && include(needle) {
                return Some((needle, what));
            }
        }
    }
    None
}

/// Whether `rest` after a [`ClockShape::PathCall`] head is `::segment`s followed by a call
/// (`::read(…)`).
fn path_called(rest: &str) -> bool {
    let mut rest = rest;
    let mut segments = 0;
    while let Some(after) = rest.strip_prefix("::") {
        let len = after.bytes().take_while(|&b| is_word(b)).count();
        if len == 0 {
            return false;
        }
        rest = after[len..].trim_start_matches([' ', '\t']);
        segments += 1;
    }
    segments > 0 && rest.starts_with('(')
}

/// Whether `text` ends with the word `word` (`    fn`, never `cfn`).
fn ends_with_word(text: &str, word: &str) -> bool {
    text.strip_suffix(word)
        .is_some_and(|rest| rest.as_bytes().last().is_none_or(|&b| !is_word(b)))
}

pub(super) const STORE_CURRENT: Rule = Rule {
    id: "store-current",
    why: "`<prefix>/store/<program>/current` is a link every atpkg update re-points, and a \
          driver run by that spelling finds its compiler by the same unresolved path, so a flip \
          mid-build hands the rest of the build to another compiler (error[E0514]); resolve it \
          (`canonicalize`, or a `physical(` helper) within the next six lines",
    scope: &[],
    check: store_current,
};

/// How many lines after a `current` join its resolve may sit.
const RESOLVE_REACH: usize = 6;

/// Rust outside atpkg, which owns the store and the link, build scripts and dev targets aside.
/// Its caller leaves test code out ([`TestCode::holds`]).
fn store_current_scope(rel: &str) -> bool {
    !(rel.starts_with("crates/atpkg/")
        || rel.ends_with("/build.rs")
        || rel.contains("/examples/")
        || rel.contains("/benches/"))
}

fn store_current(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    let test_code = tree.corpus.test_code();
    for file in tree.corpus.under(PRODUCT) {
        if !store_current_scope(&file.rel)
            || test_code.holds(&file.rel)
            || !file.mentions(&["\"current\"", "/current"])
        {
            continue;
        }
        let lines: Vec<Line<'_>> = file.shipped_lines().collect();
        for (i, line) in lines.iter().enumerate() {
            let window = &lines[i..lines.len().min(i + RESOLVE_REACH + 1)];
            if joins_current(*line) && !window.iter().any(|l| resolves(l.code)) {
                out.push(file.finding(
                    STORE_CURRENT.id,
                    line.no,
                    "joins a store `current` path and does not resolve it".to_string(),
                ));
            }
        }
    }
}

/// Whether `line` joins `"current"` or constructs a path literal containing a `/current` component.
fn joins_current(line: Line<'_>) -> bool {
    ["join", "Path::new", "PathBuf::from"].iter().any(|door| {
        hits(line.code, door, Bound::Word).any(|at| {
            let Some(arg) = after_ws(&line.code[at + door.len()..], "(") else {
                return false;
            };
            let arg = arg.trim_start_matches([' ', '\t']);
            if !arg.starts_with('"') {
                return false;
            }
            let quote = line.code.len() - arg.len();
            let Some(lit) = line.text.get(quote + 1..).and_then(|t| t.split('"').next()) else {
                return false;
            };
            let bare = *door == "join"
                && lit == "current"
                && arg
                    .get(lit.len() + 2..)
                    .and_then(|r| after_ws(r, ")"))
                    .is_some();
            bare || lit
                .match_indices("/current")
                .any(|(c, _)| matches!(lit.as_bytes().get(c + 8), None | Some(b'/')))
        })
    })
}

/// Whether `code` resolves a path via `canonicalize` or a `physical(…)` helper.
fn resolves(code: &str) -> bool {
    code.contains("canonicalize") || code.contains("physical(")
}

/// The header region: the lines the two must sit in.
const HEADER_LINES: usize = 8;

pub(super) const LICENCE_HEADER: Rule = Rule {
    id: "licence-header",
    why: "every Rust file under crates/ and apps/ carries an `SPDX-License-Identifier:` line \
          and a copyright line in its first 8 lines (a vendored upstream keeps its own SPDX id), \
          and a head that says the file is derived from upstream code also says what aterm did \
          to it, the notice Apache-2.0 §4(b) asks of a modified file",
    scope: &[],
    check: licence_header,
};

/// The words a derived file's head states its change in: "modified by the aterm project",
/// "Modifications Copyright", "the rest of the file is aterm's", "unchanged".
const CHANGE_WORDS: &[&str] = &[
    "modified",
    "modifications",
    "changed",
    "rewritten",
    "added",
    "adapted",
    "ported",
    "cut ",
    "trimmed",
    "reduced",
    "verbatim",
    "is aterm's",
];

fn licence_header(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    for file in tree.corpus.under(PRODUCT) {
        if let Some(missing) = header_gap(file) {
            out.push(file.finding(LICENCE_HEADER.id, 1, missing.to_string()));
        }
    }
}

fn contains_ignore_ascii_case(haystack: &str, needle: &str) -> bool {
    let n = needle.len();
    n == 0
        || haystack
            .as_bytes()
            .windows(n)
            .any(|w| w.eq_ignore_ascii_case(needle.as_bytes()))
}

/// What the file's header lacks, if anything.
fn header_gap(file: &SourceFile) -> Option<&'static str> {
    let mut spdx = false;
    let mut copyright = false;
    for l in file.raw().lines().take(HEADER_LINES) {
        spdx |= l.contains("SPDX-License-Identifier:");
        copyright |= contains_ignore_ascii_case(l, "copyright");
    }
    match (spdx, copyright) {
        (true, true) => change_gap(file.raw()),
        (false, true) => Some("no SPDX-License-Identifier line in its first 8 lines"),
        (true, false) => Some("no copyright line in its first 8 lines"),
        (false, false) => Some("no SPDX-License-Identifier or copyright line in its first 8 lines"),
    }
}

/// A head (the plain `//` lines before the module doc or the first code) that says the file
/// is derived from upstream code and names no change. A generator writes its own output's head.
fn change_gap(raw: &str) -> Option<&'static str> {
    let head = raw.lines().map(str::trim).take_while(|l| {
        l.is_empty() || (l.starts_with("//") && !l.starts_with("//!") && !l.starts_with("///"))
    });
    if !head
        .clone()
        .any(|l| contains_ignore_ascii_case(l, "derived from"))
    {
        return None;
    }
    let generated = head.clone().any(|l| {
        contains_ignore_ascii_case(l, "@generated")
            || contains_ignore_ascii_case(l, "generated from")
    });
    let changed = head.into_iter().any(|l| {
        CHANGE_WORDS
            .iter()
            .any(|w| contains_ignore_ascii_case(l, w))
    });
    (!generated && !changed)
        .then_some("says it is derived from upstream code but not what aterm changed in it")
}

#[cfg(test)]
#[path = "rules_tests.rs"]
mod tests;
