// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The token guards: a banned name or literal in a scoped set of files.

use super::{Bound, Finding, Line, Rule, SourceFile, Tree, after_ws, has, hits, is_word};

/// Product source: everything the guards call the tree, vendored code aside.
pub(super) const PRODUCT: &[&str] = &["crates/", "apps/"];

/// This crate's own source, which spells every name a rule that reads
/// literals bans (its tables and its planted cases).
pub(super) const CENSUS_CRATE: &str = "crates/aterm-census/";

// ---------------------------------------------------------------------------
// engine-platform: docs/ATERM_DESIGN.md §2.7, the headless engine.
// ---------------------------------------------------------------------------

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
    ("#[link", Bound::Word, "a native library"),
];

/// The platform frameworks, read on the TEXT view: a framework is linked or
/// opened by a name in a literal (`link(name = "AppKit")`, a
/// `/System/Library/Frameworks/Metal.framework` path).
const ENGINE_FRAMEWORKS: &[(&str, Bound)] = &[
    ("CoreText", Bound::Sub),
    ("Metal", Bound::Word),
    ("AppKit", Bound::Sub),
];

/// Is `rel` an engine crate's shipped default build? Test code (any path
/// naming a test), build scripts, examples, the scrollback disk tier (the
/// `disk-tier` feature) and the effects pack reader are not.
fn engine_shipped(rel: &str) -> bool {
    if !ENGINE_CRATES.iter().any(|c| rel.starts_with(c)) {
        return false;
    }
    if test_path(rel) || rel.ends_with("/build.rs") || rel.contains("/examples/") {
        return false;
    }
    if rel == "crates/aterm-effects/src/file_feed.rs" {
        return false;
    }
    !rel.strip_prefix("crates/aterm-scrollback/src/")
        .and_then(|name| name.strip_suffix(".rs"))
        .is_some_and(is_disk_tier_stem)
}

/// Does a directory or file of `rel` name test code: a `_`-separated word of
/// its name that starts with `test` (`tests/`, `test_support.rs`,
/// `page_tests/`, `style_test_helpers.rs`, `testing.rs`)?
pub(super) fn test_path(rel: &str) -> bool {
    rel.split('/').any(|part| {
        part.trim_end_matches(".rs")
            .split('_')
            .any(|word| word.starts_with("test"))
    })
}

/// `mmap`, `storage` or `disk`, alone or followed by `_…`/`.…`.
fn is_disk_tier_stem(stem: &str) -> bool {
    ["mmap", "storage", "disk"].iter().any(|head| {
        stem.strip_prefix(head).is_some_and(|rest| {
            rest.is_empty()
                || (rest.starts_with(['_', '.'])
                    && rest[1..]
                        .bytes()
                        .all(|b| b.is_ascii_alphabetic() || b == b'_'))
        })
    })
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
    for file in tree.corpus.under(ENGINE_CRATES) {
        if !engine_shipped(&file.rel) {
            continue;
        }
        let needles: Vec<&str> = ENGINE_BANNED
            .iter()
            .map(|b| b.0)
            .chain(ENGINE_FRAMEWORKS.iter().map(|f| f.0))
            .chain(["link"])
            .collect();
        if !file.mentions(&needles) {
            continue;
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

/// `link(name =`, spaces allowed: a link attribute however it is reached
/// (`#[cfg_attr(…, link(name = "m"))]`, or its line inside a wrapped one).
fn links_by_name(code: &str) -> bool {
    hits(code, "link", Bound::Word).any(|at| {
        after_ws(&code[at + 4..], "(")
            .and_then(|r| after_ws(r, "name"))
            .and_then(|r| after_ws(r, "="))
            .is_some()
    })
}

// ---------------------------------------------------------------------------
// renderer-terminal: the renderers consume RenderInput, never Terminal.
// ---------------------------------------------------------------------------

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
        if test_path(&file.rel) || !file.mentions(&["Terminal"]) {
            continue;
        }
        for line in file.shipped_lines() {
            if let Some(what) = names_terminal(line.code) {
                out.push(file.finding(RENDERER_TERMINAL.id, line.no, what.to_string()));
            }
        }
    }
}

/// `aterm_core::…Terminal`, or `Terminal` as a borrowed, annotated or returned
/// type (`&Terminal`, `: Terminal`, `-> Terminal`).
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

// ---------------------------------------------------------------------------
// dev-read: a device read to EOF.
// ---------------------------------------------------------------------------

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

/// `fs::read(` or `fs::read_to_string(` whose first argument is a `/dev/…`
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

// ---------------------------------------------------------------------------
// input-probe: the WindowServer-backed input queries.
// ---------------------------------------------------------------------------

/// The query APIs whose first call opens a SkyLight connection and makes
/// WindowServer preflight the calling binary on its main thread.
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

// ---------------------------------------------------------------------------
// protected-folder: TCC consent.
// ---------------------------------------------------------------------------

const PROTECTED_SCOPE: &[&str] = &["crates/aterm-gui/src/", "crates/aterm-cli/src/"];
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
          answers; aterm-gui and aterm-cli take those paths resolved from \
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
    for file in tree.corpus.under(PROTECTED_SCOPE) {
        if file.rel.ends_with("_tests.rs") || !file.mentions(&needles) {
            continue;
        }
        for line in file.shipped_lines() {
            if let Some(what) = protected_path(line.text) {
                out.push(file.finding(PROTECTED_FOLDER.id, line.no, format!("names `{what}`")));
            }
        }
    }
}

/// The protected-folder shape `text` holds: a `~/<home dir>`, `/Volumes`,
/// `Library/Containers|CloudStorage`, a bare `Containers`/`CloudStorage`
/// component, or a basename join (`join("Documents")`).
fn protected_path(text: &str) -> Option<String> {
    for dir in PROTECTED_HOME_DIRS {
        let path = format!("~/{dir}");
        if text.contains(&path) {
            return Some(path);
        }
    }
    for shape in ["/Volumes", "Library/Containers", "Library/CloudStorage"] {
        if text.contains(shape) {
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

// ---------------------------------------------------------------------------
// runloop-timer: one run-loop timer.
// ---------------------------------------------------------------------------

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
    scope: WINIT_SOURCE,
    check: runloop_timer,
};

/// Shipped Rust that runs a run loop: product source, the absorbed winit among it.
fn runloop_scope(rel: &str) -> bool {
    let shipped = !(rel.contains("/tests/")
        || rel.ends_with("/build.rs")
        || rel.contains("/examples/")
        || rel.contains("/benches/"));
    shipped && rel != "crates/aterm-objc/src/wake_timer.rs" && !rel.starts_with(CENSUS_CRATE)
}

/// The absorbed winit (`crates/aterm-window`), whose run loop the rule exists
/// for; its scope, so a rename that empties it is reported.
const WINIT_SOURCE: &[&str] = &["crates/aterm-window/src/"];

fn runloop_timer(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    for file in tree.corpus.under(PRODUCT) {
        if !runloop_scope(&file.rel) || !file.mentions(RUNLOOP_TIMERS) {
            continue;
        }
        for line in file.shipped_lines() {
            if let Some(timer) = RUNLOOP_TIMERS.iter().find(|t| line.text.contains(**t)) {
                out.push(file.finding(RUNLOOP_TIMER.id, line.no, format!("names `{timer}`")));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// keeper-gui, keeper-gui-deps: the PTY keeper is no GUI process
// (docs/DESIGN-pty-keeper-2026-09-26.md §5.6).
// ---------------------------------------------------------------------------

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
    "winit::",
    "CGEvent",
    "NSWorkspace",
];

pub(super) const KEEPER_GUI: Rule = Rule {
    id: "keeper-gui",
    why: "the PTY keeper outlives the window because it is no WindowServer client: linking \
          AppKit, CoreGraphics or winit, or sending an Objective-C message, would put every \
          terminal it holds in WindowServer's blast radius",
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

/// A GUI framework named in a link attribute (`name = "AppKit"`), or a GUI
/// API by name.
fn links_gui(text: &str) -> Option<String> {
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
    GUI_NAMES
        .iter()
        .find(|n| text.contains(**n))
        .map(|n| format!("names `{n}`"))
}

/// The keeper's crate: the root of the process closure.
const KEEPER_CRATE: &str = "crates/aterm-keeper";
/// Workspace crates that are the GUI.
const GUI_CRATES: &[&str] = &[
    "crates/aterm-gui",
    "crates/aterm-objc",
    "crates/aterm-gpu",
    "crates/aterm-render",
    "crates/aterm-effects",
];

/// Is `dep` (a dependency name) a GUI crate from outside the workspace? One of
/// the workspace's is a node of the closure, found as such.
fn is_external_gui(dep: &str) -> bool {
    matches!(
        dep,
        "winit" | "objc" | "objc2" | "core-graphics" | "cocoa" | "wgpu"
    ) || dep.starts_with("objc2-")
}

pub(super) const KEEPER_GUI_DEPS: Rule = Rule {
    id: "keeper-gui-deps",
    why: "a GUI crate anywhere in the keeper's process closure links its frameworks into the \
          keeper (the closure is derived from the manifests as cargo resolves it)",
    scope: &[],
    check: keeper_gui_deps,
};

fn keeper_gui_deps(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    let manifest = format!("{KEEPER_CRATE}/Cargo.toml");
    let set = match crate::scan_set::derive_process_scan_set(tree.root, &[KEEPER_CRATE]) {
        Ok(set) => set,
        Err(why) => {
            out.push(Finding {
                rule: KEEPER_GUI_DEPS.id,
                path: manifest,
                line: 0,
                detail: format!("the keeper's process closure cannot be derived: {why}"),
            });
            return;
        }
    };
    for dir in &set.scan_dirs {
        let crate_dir = dir.strip_suffix("/src").unwrap_or(dir);
        let at = format!("{crate_dir}/Cargo.toml");
        if GUI_CRATES.contains(&crate_dir) {
            out.push(Finding {
                rule: KEEPER_GUI_DEPS.id,
                path: at.clone(),
                line: 0,
                detail: format!("`{crate_dir}` is in the keeper's process closure"),
            });
        }
        match crate::scan_set::normal_dependency_names(tree.root, crate_dir) {
            Ok(deps) => {
                for dep in deps.iter().filter(|d| is_external_gui(d)) {
                    out.push(Finding {
                        rule: KEEPER_GUI_DEPS.id,
                        path: at.clone(),
                        line: 0,
                        detail: format!("depends on `{dep}`, inside the keeper's process"),
                    });
                }
            }
            Err(why) => out.push(Finding {
                rule: KEEPER_GUI_DEPS.id,
                path: at.clone(),
                line: 0,
                detail: format!("unreadable manifest: {why}"),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// flock-door: an flock the clippy.toml lint cannot name.
// ---------------------------------------------------------------------------

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

/// The 1-based lines of `syscall(<digit>…` calls in a file's code, the call
/// spanning lines or not.
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

// ---------------------------------------------------------------------------
// store-current: a store `current` path is resolved before anything runs from
// it.
// ---------------------------------------------------------------------------

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

/// Shipped Rust outside atpkg, which owns the store and the link.
fn store_current_scope(rel: &str) -> bool {
    !(rel.starts_with("crates/atpkg/")
        || rel.contains("/tests/")
        || rel.ends_with("_tests.rs")
        || rel.ends_with("/build.rs")
        || rel.contains("/examples/")
        || rel.contains("/benches/"))
}

fn store_current(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    for file in tree.corpus.under(PRODUCT) {
        if !store_current_scope(&file.rel) || !file.mentions(&["\"current\"", "/current"]) {
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

/// `join("current")`, or a `join`/`Path::new`/`PathBuf::from` of a literal
/// passing through or ending in a `/current` component.
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

/// A resolve: `canonicalize`, or a `physical(` helper.
fn resolves(code: &str) -> bool {
    code.contains("canonicalize") || code.contains("physical(")
}

// ---------------------------------------------------------------------------
// licence-header: every .rs carries its SPDX and copyright lines.
// ---------------------------------------------------------------------------

/// The header region: the lines the two must sit in.
const HEADER_LINES: usize = 8;

pub(super) const LICENCE_HEADER: Rule = Rule {
    id: "licence-header",
    why: "every Rust file under crates/ and apps/ carries an `SPDX-License-Identifier:` line \
          and a copyright line in its first 8 lines (a vendored upstream keeps its own SPDX id)",
    scope: &[],
    check: licence_header,
};

fn licence_header(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    for file in tree.corpus.under(PRODUCT) {
        if let Some(missing) = header_gap(file) {
            out.push(file.finding(LICENCE_HEADER.id, 1, missing.to_string()));
        }
    }
}

/// What the file's header lacks, if anything.
fn header_gap(file: &SourceFile) -> Option<&'static str> {
    let head: Vec<&str> = file.raw.lines().take(HEADER_LINES).collect();
    let spdx = head.iter().any(|l| l.contains("SPDX-License-Identifier:"));
    let copyright = head
        .iter()
        .any(|l| l.to_ascii_lowercase().contains("copyright"));
    match (spdx, copyright) {
        (true, true) => None,
        (false, true) => Some("no SPDX-License-Identifier line in its first 8 lines"),
        (true, false) => Some("no copyright line in its first 8 lines"),
        (false, false) => Some("no SPDX-License-Identifier or copyright line in its first 8 lines"),
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{run, spots};
    use super::*;

    const HEADER: &str = "// SPDX-License-Identifier: Apache-2.0\n// Copyright 2026 Andrew Yates\n";

    #[test]
    fn the_engine_names_no_platform_in_shipped_code() {
        let src = "use std::fs::File;\nfn f() { let _ = std::thread::spawn(|| ()); }\n\
                   // std::fs in prose\nconst S: &str = \"Metal\";\n#[link(name = \"m\")]\n\
                   extern \"C\" {}\n#[cfg(test)]\nmod tests {\n    use std::fs;\n}\n";
        let found = run(
            &ENGINE_PLATFORM,
            &[
                ("crates/aterm-grid/src/lib.rs", src),
                ("crates/aterm-grid/src/grid/test_support.rs", src),
                ("crates/aterm-scrollback/src/mmap.rs", src),
                ("crates/aterm-scrollback/src/disk_tier.rs", src),
                ("crates/aterm-scrollback/src/diskless.rs", src),
                ("crates/aterm-effects/src/file_feed.rs", src),
                ("crates/aterm-gui/src/lib.rs", src),
            ],
        );
        assert_eq!(
            spots(&found),
            [
                "crates/aterm-grid/src/lib.rs:1",
                "crates/aterm-grid/src/lib.rs:2",
                "crates/aterm-grid/src/lib.rs:4",
                "crates/aterm-grid/src/lib.rs:5",
                "crates/aterm-scrollback/src/diskless.rs:1",
                "crates/aterm-scrollback/src/diskless.rs:2",
                "crates/aterm-scrollback/src/diskless.rs:4",
                "crates/aterm-scrollback/src/diskless.rs:5",
            ]
        );
    }

    #[test]
    fn an_engine_framework_named_in_a_literal_or_a_cfg_attr_link_is_found() {
        let src = "#[cfg_attr(target_os = \"macos\", link(name = \"AppKit\", kind = \"framework\"))]\n\
                   let h = open_framework(c\"/System/Library/Frameworks/Metal.framework/Metal\");\n\
                   #[cfg_attr(\n    unix,\n    link(name = \"m\")\n)]\n\
                   fn hyperlink(name: &str) -> Link { link(name) }\nstruct MetalLayer;\n";
        let found = run(&ENGINE_PLATFORM, &[("crates/aterm-core/src/lib.rs", src)]);
        assert_eq!(
            found
                .iter()
                .map(|f| format!("{}:{}", f.line, f.detail))
                .collect::<Vec<_>>(),
            [
                "1:names `AppKit` (a platform framework)",
                "1:links a native library (`link(name = …)`)",
                "2:names `Metal` (a platform framework)",
                "5:links a native library (`link(name = …)`)",
            ]
        );
    }

    #[test]
    fn a_test_path_is_named_by_a_word_not_a_substring() {
        for rel in [
            "crates/aterm-grid/src/page_tests/mod.rs",
            "crates/aterm-grid/src/style_test_helpers.rs",
            "crates/aterm-core/test_support/generated/sixel.rs",
            "crates/aterm-messages/src/strain/tests.rs",
            "crates/aterm-grid/tests/it.rs",
        ] {
            assert!(test_path(rel), "{rel}");
        }
        assert!(!test_path("crates/aterm-core/src/latest.rs"));
        assert!(!test_path("crates/aterm-core/src/attestation.rs"));
    }

    #[test]
    fn the_renderers_never_name_terminal() {
        let src = "use aterm_core::terminal::Terminal;\nfn draw(t: &Terminal) {}\n\
                   fn make() -> Terminal {}\nfn ok(f: &TerminalFrame, s: TermSize) {}\n\
                   // aterm_core::Terminal in prose\n";
        let found = run(
            &RENDERER_TERMINAL,
            &[
                ("crates/aterm-render/src/lib.rs", src),
                ("crates/aterm-render/src/raster_tests.rs", src),
            ],
        );
        assert_eq!(
            spots(&found),
            [
                "crates/aterm-render/src/lib.rs:1",
                "crates/aterm-render/src/lib.rs:2",
                "crates/aterm-render/src/lib.rs:3",
            ]
        );
    }

    #[test]
    fn a_device_read_to_eof_is_found_in_code_only() {
        let src = "let k = std::fs::read(\"/dev/urandom\")?;\n\
                   let s = fs::read_to_string( \"/dev/stdin\" );\n\
                   // fs::read(\"/dev/random\") is the incident\n\
                   let ok = fs::read(\"/etc/hosts\");\nlet m = \"fs::read(\\\"/dev/x\\\")\";\n";
        let found = run(&DEV_READ, &[("crates/x/tests/t.rs", src)]);
        assert_eq!(
            spots(&found),
            ["crates/x/tests/t.rs:1", "crates/x/tests/t.rs:2"]
        );
    }

    #[test]
    fn an_input_probe_is_found_in_code_and_strings_not_prose() {
        let src = "// CGEventSourceFlagsState opened a connection\n\
                   unsafe extern \"C\" { fn CGEventSourceSecondsSince(s: u32) -> f64; }\n\
                   let sym = dlsym(c\"SLSInputModifierKeyState\");\n\
                   let why = \"+[NSEvent modifierFlags]\";\n";
        let found = run(&INPUT_PROBE, &[("crates/aterm-gui/src/platform.rs", src)]);
        assert_eq!(
            spots(&found),
            [
                "crates/aterm-gui/src/platform.rs:2",
                "crates/aterm-gui/src/platform.rs:3",
                "crates/aterm-gui/src/platform.rs:4",
            ]
        );
    }

    #[test]
    fn a_protected_folder_literal_is_found_in_gui_and_cli_shipped_code() {
        let src = "let a = home.join(\"Documents\");\nlet b = \"~/Downloads/x\";\n\
                   let c = Path::new(\"/Volumes/Ext\");\nlet d = \"Library/CloudStorage\";\n\
                   // ~/Desktop in prose\nlet e = DocumentsView::new();\n\
                   #[cfg(test)]\nmod tests {\n    const P: &str = \"~/Documents\";\n}\n";
        let found = run(
            &PROTECTED_FOLDER,
            &[
                ("crates/aterm-gui/src/lib.rs", src),
                ("crates/aterm-gui/src/lib_tests.rs", src),
                ("crates/aterm-containment/src/consent.rs", src),
            ],
        );
        assert_eq!(
            spots(&found),
            [
                "crates/aterm-gui/src/lib.rs:1",
                "crates/aterm-gui/src/lib.rs:2",
                "crates/aterm-gui/src/lib.rs:3",
                "crates/aterm-gui/src/lib.rs:4",
            ]
        );
    }

    #[test]
    fn a_runloop_timer_lives_only_in_wake_timer() {
        let src = "let t = CFRunLoopTimerCreate(a, b);\n\
                   let n = send(cls, sel!(scheduledTimerWithTimeInterval:target:selector:userInfo:repeats:));\n";
        let found = run(
            &RUNLOOP_TIMER,
            &[
                ("crates/aterm-objc/src/wake_timer.rs", src),
                ("crates/aterm-gui/src/app.rs", src),
                ("crates/aterm-gui/tests/t.rs", src),
                ("crates/aterm-window/src/platform_impl/macos/app.rs", src),
            ],
        );
        assert_eq!(
            spots(&found),
            [
                "crates/aterm-gui/src/app.rs:1",
                "crates/aterm-gui/src/app.rs:2",
                "crates/aterm-window/src/platform_impl/macos/app.rs:1",
                "crates/aterm-window/src/platform_impl/macos/app.rs:2",
            ]
        );
    }

    #[test]
    fn the_keeper_links_no_gui_framework() {
        let src = "#[link(name = \"AppKit\", kind = \"framework\")]\nextern \"C\" {}\n\
                   #[link(name = \"Security\", kind = \"framework\")]\n\
                   // objc_msgSend in prose\nlet x = winit::event_loop::EventLoop::new();\n";
        let found = run(
            &KEEPER_GUI,
            &[
                ("crates/aterm-keeper/src/lib.rs", src),
                ("crates/aterm-gui/src/lib.rs", src),
            ],
        );
        assert_eq!(
            spots(&found),
            [
                "crates/aterm-keeper/src/lib.rs:1",
                "crates/aterm-keeper/src/lib.rs:5",
            ]
        );
    }

    #[test]
    fn the_keepers_closure_holds_no_gui_crate() {
        use crate::scan_set::test_fixtures::{synth_tree, workspace_manifests};
        let mut files = workspace_manifests(&[
            (
                "aterm-keeper",
                "[dependencies]\naterm-uds = { workspace = true }\n",
            ),
            ("aterm-uds", "[dependencies]\nobjc2 = \"0.6\"\n"),
        ]);
        for dir in ["aterm-gui", "aterm-types", "aterm-keeper", "aterm-uds"] {
            files.push((format!("crates/{dir}/src/lib.rs"), String::new()));
        }
        let root = synth_tree("aterm-census-guards", "keeper-deps", &files);
        let corpus = super::super::Corpus::of(&[]);
        let tree = Tree {
            root: &root,
            corpus: &corpus,
        };
        let mut found = Vec::new();
        keeper_gui_deps(&tree, &mut found);
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].path, "crates/aterm-uds/Cargo.toml");
        assert!(found[0].detail.contains("`objc2`"), "{found:?}");

        // A GUI crate of the workspace in the closure, through a path edge.
        let mut files = workspace_manifests(&[
            (
                "aterm-keeper",
                "[dependencies]\naterm-uds = { workspace = true }\n",
            ),
            (
                "aterm-uds",
                "[dependencies]\naterm-render = { path = \"../aterm-render\" }\n",
            ),
            ("aterm-render", ""),
        ]);
        for dir in [
            "aterm-gui",
            "aterm-types",
            "aterm-keeper",
            "aterm-uds",
            "aterm-render",
        ] {
            files.push((format!("crates/{dir}/src/lib.rs"), String::new()));
        }
        let root = synth_tree("aterm-census-guards", "keeper-gui-crate", &files);
        let tree = Tree {
            root: &root,
            corpus: &corpus,
        };
        let mut found = Vec::new();
        keeper_gui_deps(&tree, &mut found);
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].path, "crates/aterm-render/Cargo.toml");
        assert!(
            found[0].detail.contains("in the keeper's process closure"),
            "{found:?}"
        );

        // A GUI crate behind a renamed key is named by its package.
        let mut files = workspace_manifests(&[
            (
                "aterm-keeper",
                "[dependencies]\naterm-uds = { workspace = true }\n",
            ),
            (
                "aterm-uds",
                "[dependencies]\nwindowing = { package = \"winit\", version = \"0.30\" }\n",
            ),
        ]);
        for dir in ["aterm-gui", "aterm-types", "aterm-keeper", "aterm-uds"] {
            files.push((format!("crates/{dir}/src/lib.rs"), String::new()));
        }
        let root = synth_tree("aterm-census-guards", "keeper-renamed-gui", &files);
        let tree = Tree {
            root: &root,
            corpus: &corpus,
        };
        let mut found = Vec::new();
        keeper_gui_deps(&tree, &mut found);
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].path, "crates/aterm-uds/Cargo.toml");
        assert!(found[0].detail.contains("`winit`"), "{found:?}");
    }

    /// Each door the lint cannot name, with the line it fires on.
    #[test]
    fn every_flock_door_the_lint_cannot_name_is_found() {
        let cases: &[(&str, &str, usize)] = &[
            (
                "a private extern flock",
                "unsafe extern \"C\" {\n    fn flock(fd: i32, op: i32) -> i32;\n}\n",
                2,
            ),
            (
                "flock under another name",
                "unsafe extern \"C\" {\n    #[link_name = \"flock\"]\n    fn take_it(fd: i32) -> i32;\n}\n",
                2,
            ),
            (
                "an open that takes the lock",
                "let fd = libc::open(p, libc::O_RDWR | libc::O_EXLOCK, 0o600);\n",
                1,
            ),
            (
                "an OFD lock",
                "unsafe { libc::fcntl(fd, libc::F_OFD_SETLKW, &lock) };\n",
                1,
            ),
            (
                "a raw SYS_flock",
                "libc::syscall(\n    libc::SYS_flock,\n    fd,\n)\n",
                2,
            ),
            (
                "a syscall by number",
                "unsafe { libc::syscall(\n    131, fd, 6) };\n",
                1,
            ),
        ];
        for (why, src, line) in cases {
            let found = run(&FLOCK_DOOR, &[("crates/aterm-gui/src/x.rs", src)]);
            assert_eq!(
                spots(&found),
                [format!("crates/aterm-gui/src/x.rs:{line}")],
                "MISS — {why}:\n{src}"
            );
        }
    }

    #[test]
    fn doors_the_lint_sees_and_near_misses_are_silent() {
        let src = "// A private `fn flock(fd, op)` would be a finding.\n\
                   let l = unsafe { libc::flock(fd, libc::LOCK_EX) };\nlet _ = file.try_lock(); // O_EXLOCKED\n\
                   fn flock_probe() {}\nlet q = libc::fcntl(fd, libc::F_OFD_GETLK);\nlet _ = my_syscall(1);\n\
                   #[link_name = \"flockfile\"]\nlet c = libc::syscall(libc::SYS_close_range, 3);\n\
                   let s = \"fn flock(\";\n";
        assert!(run(&FLOCK_DOOR, &[("crates/aterm-gui/src/x.rs", src)]).is_empty());
        let libc = "pub const O_EXLOCK: c_int = 0x20;\npub const SYS_flock: c_int = 131;\n\
                    unsafe extern \"C\" {\n    pub fn flock(fd: c_int, op: c_int) -> c_int;\n}\n";
        assert!(
            run(
                &FLOCK_DOOR,
                &[("crates/aterm-libc/src/darwin_aarch64.rs", libc)]
            )
            .is_empty()
        );
    }

    #[test]
    fn a_store_current_path_is_resolved_within_six_lines() {
        let src = "fn b(p: &Path) -> PathBuf {\n    let c = p.join(\"store/trust/current/bin\");\n    \
                   std::fs::canonicalize(c).unwrap()\n}\n\
                   fn d(p: &Path) -> PathBuf {\n    physical(&p.join(\"current\"))\n}\n\
                   fn e(p: &Path) -> PathBuf {\n    p.join(\"currently\").join(\"a/current_x\")\n}\n\
                   // p.join(\"current\") in prose\n\
                   #[cfg(test)]\nmod tests {\n    fn t(p: &Path) { p.join(\"current\"); }\n}\n\
                   fn a(p: &Path) -> PathBuf {\n    p.join(\"current\")\n}\n\
                   fn c() -> PathBuf {\n    PathBuf::from(\"/opt/store/x/current\")\n}\n";
        let found = run(
            &STORE_CURRENT,
            &[
                ("crates/aterm-verify/src/lib.rs", src),
                ("crates/atpkg/src/store.rs", src),
                ("crates/aterm-verify/tests/t.rs", src),
                ("crates/aterm-spec/src/verify_tests.rs", src),
            ],
        );
        assert_eq!(
            spots(&found),
            [
                "crates/aterm-verify/src/lib.rs:17",
                "crates/aterm-verify/src/lib.rs:20",
            ]
        );
    }

    #[test]
    fn a_header_needs_both_lines_in_either_order() {
        let found = run(
            &LICENCE_HEADER,
            &[
                ("crates/a/src/spdx_first.rs", HEADER),
                (
                    "crates/a/src/copyright_first.rs",
                    "// Copyright 2026 Andrew Yates\n// SPDX-License-Identifier: MIT\n",
                ),
                ("crates/a/src/bare.rs", "fn main() {}\n"),
                (
                    "crates/a/src/late.rs",
                    "\n\n\n\n\n\n\n\n// SPDX-License-Identifier: Apache-2.0\n// Copyright\n",
                ),
                ("vendor/indexmap/src/lib.rs", "fn main() {}\n"),
            ],
        );
        assert_eq!(
            spots(&found),
            ["crates/a/src/bare.rs:1", "crates/a/src/late.rs:1"]
        );
    }
}
