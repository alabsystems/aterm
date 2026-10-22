// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The token guards: a banned name or literal in a scoped set of files.

use super::{Bound, Corpus, Finding, Line, Rule, SourceFile, Tree, after_ws, has, hits, is_word};

/// Product source: everything the guards call the tree, vendored code aside.
pub(super) const PRODUCT: &[&str] = &["crates/", "apps/"];

/// This crate's own source, which spells every name a rule that reads
/// literals bans (its tables and its planted cases).
pub(super) const CENSUS_CRATE: &str = "crates/aterm-census/";

// engine-platform: docs/ATERM_DESIGN.md §2.7, the headless engine.

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
    // The modules themselves, whatever is imported or called from them
    // (`use std::thread::{Builder, spawn};`, `std::thread::scope`); a word, so
    // `std::thread_local!` is not one.
    ("std::thread", Bound::Word, "a thread"),
    ("std::net", Bound::Word, "a socket"),
    ("#[link", Bound::Word, "a native library"),
];

/// The `std` modules a grouped import (`use std::{fs, net::TcpStream};`) may
/// not name in an engine crate, which the path needles above miss: the group
/// splits `std::` from the module.
const ENGINE_STD_MODULES: &[(&str, &str)] = &[
    ("fs", "the filesystem"),
    ("net", "a socket"),
    ("thread", "a thread"),
];

/// The platform frameworks, read on the text view: a framework is linked or
/// opened by a name in a literal (`link(name = "AppKit")`, a
/// `/System/Library/Frameworks/Metal.framework` path).
const ENGINE_FRAMEWORKS: &[(&str, Bound)] = &[
    ("CoreText", Bound::Sub),
    ("Metal", Bound::Word),
    ("AppKit", Bound::Sub),
];

/// Is `rel` an engine crate's shipped default build? Test code (any path
/// naming a test, or a module its parent declares test-only:
/// [`declared_test_modules`]), build scripts, examples and the effects pack
/// reader are not.
fn engine_shipped(rel: &str) -> bool {
    if !ENGINE_CRATES.iter().any(|c| rel.starts_with(c)) {
        return false;
    }
    if test_path(rel) || rel.ends_with("/build.rs") || rel.contains("/examples/") {
        return false;
    }
    rel != "crates/aterm-effects/src/file_feed.rs"
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

/// The files that are test code by their parent's say, whatever their names:
/// every out-of-line module a file declares at its top level inside an item
/// whose `cfg` proves it test-only (`#[cfg(test)] mod rig;`, its `#[path]`
/// followed), and everything in that module's own directory. Each entry is a
/// file, or a directory prefix ending in `/`. A declaration nested in an
/// inline module, or one whose file the corpus does not hold, names nothing,
/// so its file stays shipped and read.
fn declared_test_modules(corpus: &Corpus, prefixes: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    for parent in corpus.under(prefixes) {
        if !parent.mentions(&["mod "]) {
            continue;
        }
        let shipped: std::collections::BTreeSet<usize> =
            parent.shipped_lines().map(|l| l.no).collect();
        let lines: Vec<Line<'_>> = parent.lines().collect();
        for (at, line) in lines.iter().enumerate() {
            if shipped.contains(&line.no) || line.code.starts_with([' ', '\t']) {
                continue;
            }
            let Some(name) = out_of_line_mod(line.code) else {
                continue;
            };
            let attrs = lines[..at]
                .iter()
                .rev()
                .take_while(|l| l.code.trim_start().starts_with("#["));
            let path = attrs.filter_map(|l| path_attr(l.text)).next();
            if let Some(file) = module_file(corpus, &parent.rel, name, path) {
                let own = file
                    .strip_suffix("/mod.rs")
                    .or_else(|| file.strip_suffix(".rs"))
                    .unwrap_or(&file);
                out.push(format!("{own}/"));
                out.push(file);
            }
        }
    }
    out
}

/// The module a body-less `mod` item declares (`pub(crate) mod rig;` gives
/// `rig`), when `code` is one.
fn out_of_line_mod(code: &str) -> Option<&str> {
    let mut rest = code.trim();
    if let Some(after) = rest.strip_prefix("pub") {
        rest = after.trim_start();
        if rest.starts_with('(') {
            rest = rest.split_once(')')?.1.trim_start();
        }
    }
    let name = rest
        .strip_prefix("mod ")?
        .trim_start()
        .strip_suffix(';')?
        .trim_end();
    (!name.is_empty() && name.bytes().all(is_word)).then_some(name)
}

/// The path a `#[path = "…"]` attribute line names (the TEXT view keeps it).
fn path_attr(text: &str) -> Option<&str> {
    let rest = text
        .trim()
        .strip_prefix("#[path")?
        .trim_start()
        .strip_prefix('=')?;
    rest.trim_start()
        .strip_prefix('"')?
        .split_once('"')
        .map(|(p, _)| p)
}

/// The corpus file `mod name;` in `parent` loads: `path` against the parent's
/// directory, else `name.rs` or `name/mod.rs` in the parent's module directory
/// (its own directory for a `lib.rs`, `main.rs` or `mod.rs`, else the one its
/// stem names).
fn module_file(corpus: &Corpus, parent: &str, name: &str, path: Option<&str>) -> Option<String> {
    let (dir, file) = parent.rsplit_once('/')?;
    let candidates = match path {
        Some(path) => vec![normal_path(&format!("{dir}/{path}"))?],
        None => {
            let base = if matches!(file, "lib.rs" | "main.rs" | "mod.rs") {
                dir.to_string()
            } else {
                format!("{dir}/{}", file.strip_suffix(".rs")?)
            };
            vec![format!("{base}/{name}.rs"), format!("{base}/{name}/mod.rs")]
        }
    };
    candidates.into_iter().find(|c| corpus.get(c).is_some())
}

/// `rel` with its `.` and `..` segments resolved; `None` when `..` climbs out.
fn normal_path(rel: &str) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    for part in rel.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            _ => parts.push(part),
        }
    }
    Some(parts.join("/"))
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
    let test_modules = declared_test_modules(tree.corpus, ENGINE_CRATES);
    let declared_test = |rel: &str| {
        test_modules
            .iter()
            .any(|t| rel == t || (t.ends_with('/') && rel.starts_with(t.as_str())))
    };
    for file in tree.corpus.under(ENGINE_CRATES) {
        if !engine_shipped(&file.rel) || declared_test(&file.rel) {
            continue;
        }
        let needles: Vec<&str> = ENGINE_BANNED
            .iter()
            .map(|b| b.0)
            .chain(ENGINE_FRAMEWORKS.iter().map(|f| f.0))
            .chain(["link", "std"])
            .collect();
        if !file.mentions(&needles) {
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

/// Every first segment of a `std::{…}` group in a file's code: its 1-based
/// line and the segment (`use std::{fs, io::{self, Read}};` gives `fs` and
/// `io`), however the group wraps. Nested groups below the first level are
/// paths inside a module already named.
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

// renderer-terminal: the renderers consume RenderInput, never Terminal.

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

// dev-read: a device read to EOF.

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

// input-probe: the WindowServer-backed input queries.

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

// protected-folder: TCC consent.

/// aterm-gui's source (the window crate's and its split-out libraries',
/// `crates/aterm-gui-*`), aterm-cli's, amacs's host crates (`crates/amacs-*`:
/// its file admission and its terminal program), which run in those two
/// processes, and the disk walker's, which stats every path it is handed and
/// so must take its deny set from consent too. The `amacs` engine names no
/// path.
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

// runloop-timer: one run-loop timer.

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
const WINDOW_SOURCE: &[&str] = &["crates/aterm-window/src/"];

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

// keeper-gui, keeper-gui-deps: the PTY keeper is no GUI process
// (docs/DESIGN-pty-keeper-2026-09-26.md §5.6).

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
    why: "the PTY keeper outlives the window because it is no WindowServer client: linking \
          AppKit, CoreGraphics or aterm-window, or sending an Objective-C message, would put every \
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

// keeper-no-mmap: the keeper holds crash-restore regions and never maps one
// (the crash-restore plan §5).

/// The keeper's own source. aterm-uds, which it links, maps regions for the windows (the
/// journal's writer and its restorer), code the keeper never calls.
const KEEPER_OWN: &[&str] = &["crates/aterm-keeper/src/"];

/// The mapping calls by NAME: what a local `extern` declaration in the keeper (whose OS half
/// declares its own calls, `src/sys.rs`) would spell, which no clippy path can name. aterm-uds's
/// region API (`Region::create`, `Region::map_readonly`, the writable test twin) and libc's calls
/// are `crates/aterm-keeper/clippy.toml` rows instead: resolved by type, they catch any import
/// (a grouped `use`, an alias), where a text scan for their spelling did not.
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
        if test_path(&file.rel) {
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

// restore-stays-local: a crash-restore region never leaves the host (the
// crash-restore plan §5, privacy claim 5).

/// The crash restore's own source: the region's layout, records, writer and recovery.
const RESTORE_OWN: &[&str] = &["crates/aterm-restore/src/"];
/// The crash restore's crate, as a closure names it.
const RESTORE_CRATE: &str = "crates/aterm-restore";
/// The crates that carry bytes off the host: the fabric's link, the fleet's TLS proof,
/// listener and relay, the HTTP stream, and the window's fabric endpoint. `aterm-gui`, which
/// links the restore, also runs the fleet's listener: there no closure can keep a region
/// local, and what does is that no control verb reads one (status reports counts).
const OFF_HOST_CRATES: &[&str] = &[
    "crates/aterm-link",
    "crates/aterm-net",
    "crates/aterm-http",
    "crates/aterm-gui-fabric",
];

/// What reaches a socket: std's and aterm-uds's socket types, the calls by name, and
/// the off-host crates themselves.
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
        if test_path(&file.rel) {
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

/// The keeper's crate: the root of the process closure.
const KEEPER_CRATE: &str = "crates/aterm-keeper";
/// Workspace crates that are the GUI.
const GUI_CRATES: &[&str] = &[
    "crates/aterm-gui",
    "crates/aterm-window",
    "crates/aterm-objc",
    "crates/aterm-gpu",
    "crates/aterm-render",
    "crates/aterm-effects",
];

/// Is `dep` (a dependency name) a GUI crate from outside the workspace? One of
/// the workspace's is a node of the closure, found as such.
fn is_external_gui(dep: &str) -> bool {
    matches!(dep, "winit" | "core-graphics" | "cocoa" | "wgpu") || dep.starts_with("objc")
}

pub(super) const KEEPER_GUI_DEPS: Rule = Rule {
    id: "keeper-gui-deps",
    why: "a GUI crate anywhere in the keeper's process closure links its frameworks into the \
          keeper (the closure is derived from the manifests as cargo resolves it, and its \
          external packages from Cargo.lock, transitively)",
    scope: &[],
    check: keeper_gui_deps,
};

/// One step of the keeper's closure walk: a workspace crate, read by its
/// manifest (normal edges only), or a `Cargo.lock` package, read by its lock
/// entry; each with the chain that reached it.
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
    // Breadth-first from the keeper's own closure, each node once, so a GUI
    // package is reported with its shortest chain, at the manifest of the
    // keeper-closure crate it is reached from.
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
                if GUI_CRATES.contains(&dir.as_str()) {
                    let detail = if chain.is_empty() {
                        format!("`{dir}` is in the keeper's process closure")
                    } else {
                        format!(
                            "`{dir}` is in the keeper's process closure, through {}",
                            chain.join(" -> ")
                        )
                    };
                    out.push(finding(at.clone(), detail));
                }
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
                // A workspace member's lock entry lists its dev-dependencies
                // too: read it by its manifest, as the closure does.
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

// std-command: a child built from the alias clippy cannot see through.

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

/// The door one line of code opens on the alias: `StdCommand::new`, a rename
/// (`StdCommand as …`) or a re-alias (`type … = StdCommand`, its path spelled
/// or not).
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

/// `s` without the `a::b::` path (spaces allowed, a leading `::` too) and
/// the spaces that end it.
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

// flock-door: an flock the clippy.toml lint cannot name.

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

// supervise-fx: the supervising loop reads its clocks and sleeps through its
// one door (`crates/aterm-agent/src/supervise/fx.rs`), and looks at the
// machine only through a derived door (the policy probe, or `Fx`'s own).

/// The supervisor's source: the loop and everything it calls in its own
/// module (the policy under `policy/` included).
const SUPERVISE: &[&str] = &["crates/aterm-agent/src/supervise/"];

/// The supervisor files a direct clock read belongs in: the door itself and
/// the tape behind it; the transport, which sits BELOW the loop's `Ctl` door
/// (its socket timeouts are the request's, and a tape holds the request's
/// reply, not them); and the mail lane, its own thread with its own injected
/// clock (`mail::task_with_clock`), which a tape refuses outright.
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
    /// A free reader: called (`unix_ms(`) or named by a path (`limit::unix_now`,
    /// passed as a fn item) — never a local or a field of the same name
    /// (`Due::unix_ms`, `settle.unix_ms`), nor a `fn` line of that name (a
    /// definition: its body is read line by line). The reader's own `fn`, in
    /// its own file, is the wall clock's home ([`reader_homes`]).
    Reader,
    /// A method: called on a receiver (`t.elapsed()`) or named by a path
    /// (`Instant::elapsed`), never a field of the same name.
    Method,
    /// A free function: called bare (`read_small(`) or by a path
    /// (`limit::offset_at(`, `footer::shell_cwds`, also passed as a fn item
    /// when pathed) — never a method of the same name (the probe's own
    /// `self.fx.zone_offset(…)`, `ctx.probe.shell_cwds(…)`), a field
    /// (`l.tz_offset_s`), or the name's own `fn` line.
    Free,
    /// The head of a path that is CALLED: `fs::read(…)`,
    /// `std::fs::OpenOptions::new()`, `File::open(…)` — never the path named
    /// as a type (`Option<std::fs::File>`), where nothing is looked at.
    PathCall,
}

/// Each direct clock read or sleep, its shape, and what it is. Read on the
/// CODE view, so a comment or a string naming one is no finding.
///
/// A line names each by its path, so an IMPORT that binds one under a name
/// the line view cannot see is read on its own ([`use_leaves`],
/// [`imported_read`]): a group (`use std::thread::{self, sleep};`, then a
/// bare `sleep(d)`), a rename (`Instant as Mono`, `std::thread as t`), a glob
/// (`std::thread::*`), a reader imported to be passed bare
/// (`journal::{json_str, unix_ms}`, then `unwrap_or_else(unix_ms)`), and a
/// `type` alias of a clock's type. What a lexical guard still cannot see is
/// said where the rule is ([`SUPERVISE_FX`]).
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

/// PHASE 2 (the recorder, the replay plan's T4): the other values the loop
/// observes that its door records — this process's id (the claim's holder,
/// resolved once into a setup), a delivery handed over by another thread (the
/// mail lane's: `Fx::try_recv_mail`, `Fx::recv_mail_timeout`), and the
/// approval ledger's readers and its bound,
/// which read or cut a file by its path (the loop reads the ledger ONCE through
/// `Fx::read_text` and hands the text to the `approvals::*_in` forms; it cuts
/// through `Fx::bound`). Read on the code view, in the same shapes. Every
/// other reader of the ledger by path is named by its convention, beside its
/// `_in` form ([`ledger_by_path`]), so one added later needs no row here.
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
        "approvals::open_model_switch",
        ClockShape::Named,
        "the approval ledger by its path (`approvals::open_model_switch`)",
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
        "approvals::relaunch_waiting",
        ClockShape::Named,
        "the approval ledger by its path (`approvals::relaunch_waiting`)",
    ),
    (
        "approvals::bound",
        ClockShape::Named,
        "a cut of a per-session file (`approvals::bound`)",
    ),
];

/// The `fn`s a phase-2 read belongs in, by file (under the supervisor), name
/// and needle: the holder's resolution (`claim::default_holder`, read where a
/// setup is made, so a replay names the recorded holder and never this
/// process). A home is its own `fn` in its own file, for its own read: a `fn`
/// of the same name elsewhere is none, as phase 3's homes are not.
const SUPERVISE_READ_HOMES: &[(&str, &str, &str)] =
    &[("claim.rs", "default_holder", "process::id")];

/// The wall readers' own `fn`s, by file (under the supervisor) and name:
/// `limit::unix_now{,_ms}` and `journal::unix_ms` wrap the system's wall
/// clock — that is what each is for, and the crate's callers outside the
/// supervisor read them too — so each body is the WALL clock's home, and the
/// rule is about their callers. A home for the wall read alone
/// ([`wall_read`]): a sleep or a monotonic read in a reader's body is a read
/// past the door like any other, and a `fn` of a reader's name in another
/// file (a method `unix_ms` on some type) is no home at all — a home by name
/// alone excused whatever such a body read.
const SUPERVISE_READER_HOMES: &[(&str, &str)] = &[
    ("journal.rs", "unix_ms"),
    ("limit.rs", "unix_now"),
    ("limit.rs", "unix_now_ms"),
];

/// PHASE 3 (the policy probe, the replay plan's T5): the looks at the
/// machine itself — the filesystem, a process, the kernel, the zone, the
/// environment — which the approval policy and the loop make only through a
/// derived door, so a harness tape holds what each concluded
/// (`policy::probe::PolicyProbe`, handed out by `Fx::policy_probe`; the
/// loop's own `Fx::tz_offset`, `Fx::zone_offset`, `Fx::local_offset_at`).
/// The session's injected readers (`self.zone_offset`, `self.local_offset_at`) are
/// named too: each is read by one Session helper that asks the door; and so is
/// the live probe (`LiveProbe`), which only the door hands out. Besides these
/// names, a call of a door's LIVE BODY from above the door is named
/// ([`live_readers`]): `shell_startup::read`, `WorkerEnv::raw_git_view` and
/// the rest look at the machine inside their homes, out of this list's sight.
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

/// The supervisor files every look belongs in: the policy probe's live arm,
/// where the policy's looks were moved unchanged, and the probe's door itself
/// (`policy/probe.rs`), which names the live probe it wraps. `Fx`, which hands
/// the live probe out when nothing records, is a clock home, outside the rule.
const SUPERVISE_LOOK_FILES: &[&str] = &[
    "crates/aterm-agent/src/supervise/policy/probe_live.rs",
    "crates/aterm-agent/src/supervise/policy/probe.rs",
];

/// The `fn`s a phase-3 look belongs in, by file (under the supervisor) and
/// name, each with why: the LIVE BODIES of the derived doors, which run
/// below their door so a tape holds only what they concluded; the readers the
/// window's host shares; the outputs a door already records (the journal's
/// file, the notes line, the ledger's bound); and the setup read once before
/// the loop runs.
///
/// A row says where a look is MADE. That a live body's callers are below its
/// door — the probe's live arm, `Fx`'s doors — is checked, not claimed: every
/// `fn` of a live body's file that reaches one of its homes (through the
/// file's own calls, or another home file's live body) is part of it, and a
/// call of one from any other supervisor file is a finding
/// ([`live_readers`]).
const SUPERVISE_LOOK_HOMES: &[(&str, &str, &str)] = &[
    // The live body of `probe.git_view` (the probe's live arm calls
    // `WorkerEnv::raw_git_view`): git spawned as the worker runs it.
    ("policy/git_config.rs", "git", GIT_VIEW),
    ("policy/git_config.rs", "view_at", GIT_VIEW),
    ("policy/git_config.rs", "is_executable", GIT_VIEW),
    // The worker's Claude Code directory, read inside the live bodies of
    // `probe.shell_cwds`, `probe.shell_startup`, `probe.owner_asks` and
    // `probe.settings_env`; the passwd reader is `claude_dir_at`'s parameter.
    ("policy/git_config.rs", "claude_dir", WORKER_LIVE),
    ("policy/git_config.rs", "claude_dir_at", INJECTED),
    // The live bodies of `probe.settings_env`, `probe.owner_asks` and
    // `probe.shell_startup` (Claude Code's settings files), shared with the
    // window's host.
    ("policy/claude_settings.rs", "managed_preferences", SETTINGS),
    ("policy/claude_settings.rs", "main_worktree", SETTINGS),
    ("policy/claude_settings.rs", "drop_ins", SETTINGS),
    ("policy/claude_settings.rs", "opens", SETTINGS),
    ("policy/claude_settings.rs", "read_settings_in", SETTINGS),
    // The live body of `probe.shell_startup`: the shell's snapshots and rc
    // files.
    ("policy/shell_startup.rs", "read", SHELL),
    ("policy/shell_startup.rs", "snapshots", SHELL),
    ("policy/shell_startup.rs", "hook_env_scripts", SHELL),
    ("policy/shell_startup.rs", "zshenv", SHELL),
    // The zone readers themselves, which `Fx`'s derived doors ask
    // (`probe.tz_offset`, `probe.zone_offset`, `probe.local_offset_at`) and
    // the window's host calls; and the placement helpers whose injected
    // reader is a parameter of that name.
    ("limit.rs", "tz_offset_s", ZONE),
    ("limit.rs", "zone_offset_at", ZONE),
    ("limit.rs", "local_offset_at", ZONE),
    ("limit.rs", "date_offset", ZONE),
    ("limit.rs", "zone_known", ZONE),
    ("limit.rs", "offset_in", INJECTED),
    ("limit.rs", "reset_in_zone", INJECTED),
    // The one reader of each injected `fn` field, through its door; and the
    // constructor naming the shipped readers as the fields' defaults.
    ("run.rs", "zone_lookup", FIELD_DOOR),
    ("run.rs", "local_offset_then", FIELD_DOOR),
    ("run.rs", "from_setup", FIELD_DOOR),
    // Outputs a door records: the notes line written inside the `notes`
    // door's live call, the journal's own file (`Fx::emit` holds each row).
    ("run.rs", "append_note_at", OUTPUT),
    ("journal.rs", "open", OUTPUT),
    // Files read by their path for the window's host and the CLI, never by
    // the loop, which reads each once through `Fx::read_text` (phase 2 names
    // a loop call of the ledger's): the ledger's directory and its reader
    // that has no `_in` form (every by-path reader beside an `_in` form is a
    // home by that convention: `ledger_by_path`), the standing rules, a
    // journal `aterm drive ledger` replays, the owner's policy file (a tape
    // holds the policy itself, in its header).
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

/// The reasons a [`SUPERVISE_LOOK_HOMES`] row gives for a derived door's LIVE
/// BODY: code that runs below its door, so that a tape holds what it
/// concluded and a replay answers the door without running it. A live body is
/// reached only from below the door — the probe's live arm, `Fx`'s doors, or
/// another `fn` of a live body — and phase 3 names a call of one from
/// anywhere else ([`live_readers`]): the look it makes would reach no tape
/// while every look INSIDE it stays a home. The other reasons are no live
/// body: an injected reader's parameter, a door's own field, an output a door
/// records, a file the loop never reads by its path.
const LIVE_BODY: &[&str] = &[GIT_VIEW, WORKER_LIVE, SETTINGS, SHELL, ZONE];

/// One `fn` of a live body ([`live_readers`]): the home file it is in, its
/// module's name, its own, and how it can be called, read off its signature.
struct LiveReader<'t> {
    rel: &'t str,
    module: &'t str,
    name: &'t str,
    /// It takes `self`: called `.name(…)` or by a type's path.
    method: bool,
    /// Visible outside its module (`pub`, `pub(crate)`, …): a call from
    /// another file is the guard's to see; a private one's is the compiler's.
    public: bool,
}

/// THE LIVE BODIES, read from the code rather than listed: in each file that
/// holds a live body's home ([`SUPERVISE_LOOK_HOMES`] rows whose reason is in
/// [`LIVE_BODY`]), every `fn` that reaches a home through the file's own
/// calls (`super::graph::reach`, by name, as `test-appkit-door` reads a
/// file), or calls another home file's live body, is part of that live body
/// — its entry included (`shell_startup::read`, `claude_settings::owner_asks`,
/// `WorkerEnv::raw_git_view`) — until nothing more is
/// reached. So the homes table lists only where a look is MADE; what leads to
/// one is found, and a reader added later is guarded where it is called the
/// moment it reaches a home. How each is called comes from its signature.
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
            // A fn whose reader is an injected parameter named as a home is
            // (`INJECTED`: `offset_in`'s `zone_offset_at`) calls the
            // parameter, never the home: its lines are no edge.
            let injected = |func: &str| {
                SUPERVISE_LOOK_HOMES.iter().any(|&(home, name, why)| {
                    why == INJECTED && name == func && file.rel.ends_with(home)
                })
            };
            let mut callers: std::collections::BTreeMap<&str, Vec<&str>> =
                std::collections::BTreeMap::new();
            let mut lines: Vec<(&str, &str)> = Vec::new();
            for l in super::graph::attribute(file) {
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
            (*file, homes, callers, lines)
        })
        .collect();
    let mut readers: Vec<LiveReader<'t>> = Vec::new();
    loop {
        let mut grown = Vec::new();
        for (file, homes, callers, lines) in &graphs {
            let across = lines
                .iter()
                .filter(|(_, code)| live_call(code, &file.rel, &readers).is_some())
                .map(|(func, _)| *func);
            for name in super::graph::reach(homes.iter().copied().chain(across), callers).keys() {
                let known = |r: &LiveReader<'_>| r.rel == file.rel && r.name == *name;
                if readers.iter().chain(&grown).any(known) {
                    continue;
                }
                let Some((method, public)) = signature(file, name) else {
                    continue; // a name called here that is no `fn` of this file
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

/// Whether the shipped `fn name` of `file` takes `self`, and whether it is
/// visible outside its module: read from its signature, the `fn` line to the
/// line that opens its body (or ends a declaration). `None` when the file
/// defines no such `fn`.
fn signature(file: &SourceFile, name: &str) -> Option<(bool, bool)> {
    let attributed = super::graph::attribute(file);
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

/// The live body of ANOTHER file one line of code in the supervisor file
/// `rel` calls, if any ([`live_readers`]): a method by `.name(…)` or a type's
/// path (`WorkerEnv::raw_git_view`), a free `fn` by its module's path
/// (`shell_startup::read`, `super::limit::zone_offset_at`). A call in the live
/// body's own file makes its caller part of the live body; a private `fn` is
/// the compiler's outside its file.
fn live_call<'r, 't>(
    code: &str,
    rel: &str,
    readers: &'r [LiveReader<'t>],
) -> Option<&'r LiveReader<'t>> {
    readers.iter().find(|r| {
        if r.rel == rel || !r.public {
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
    /// Executable statements after their semicolons keep their byte positions.
    pub(super) executable: std::collections::BTreeMap<usize, String>,
}

/// Each `use` statement of `file`'s shipped code: a live body imported into a
/// file outside the door is called there bare, which [`live_call`] cannot
/// tell from a local of the same name, so the import is what is named
/// ([`live_import`]); and so is a clock bound under a name the lines cannot
/// see ([`use_leaves`]). Only its span through its semicolon is an import:
/// `use std::thread; thread::sleep(d);` still calls a clock door on that line.
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
                if !starts_use(rest) {
                    break;
                }
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

/// One name a `use` statement — or a `type` alias ([`type_alias`]) — binds.
pub(super) struct UseLeaf<'s> {
    /// Its whole path, the group prefixes above it joined: `std::thread::sleep`
    /// from `use std::{thread::{self, sleep}};`, and `std::thread` from that
    /// `self`.
    pub(super) path: Vec<&'s str>,
    /// The name it is bound to when renamed (`as Mono`); `as _` binds nothing
    /// a call can name.
    pub(super) alias: Option<&'s str>,
    /// A glob (`std::thread::*`): `path` is the module's, and every item of it
    /// is bound bare.
    pub(super) glob: bool,
}

/// THE NAMES ONE `use` STATEMENT BINDS, each with its whole path: the line
/// view sees `thread::sleep` in `use std::thread::sleep;` but not in
/// `use std::thread::{self, sleep};`, nor `Instant::now` in `Mono::now()`
/// after `use std::time::Instant as Mono;` — a group splits the path from its
/// item, a rename hides it — so the statement is read as the tree it is,
/// however it wraps.
pub(super) fn use_leaves(stmt: &str) -> Vec<UseLeaf<'_>> {
    let mut out = Vec::new();
    if let Some(at) = hits(stmt, "use", Bound::Word).next() {
        let tree = stmt[at + 3..].split(';').next().unwrap_or("");
        use_tree(tree, &[], &mut out);
    }
    out
}

/// One use tree below the path `prefix`: a group (`head::{a, b::{c}}`) is
/// each of its members below `prefix::head`; a leaf is a path, a glob or
/// `self`, renamed or not.
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
            // `{self}` is the module the group is in.
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
/// (`type Mono = std::time::Instant;` binds `Instant` as `Mono`, as
/// `use std::time::Instant as Mono;` does). An alias of anything but a path
/// (`fn() -> Instant`, a tuple, a generic) binds no clock to call.
pub(super) fn type_alias(code: &str) -> Option<UseLeaf<'_>> {
    let at = hits(code, "type", Bound::Word).next()?;
    let head = code[..at].trim();
    if !(head.is_empty() || head.starts_with("pub")) {
        return None;
    }
    let rest = code[at + 4..].trim_start();
    let len = rest.bytes().take_while(|&b| is_word(b)).count();
    let rhs = after_ws(&rest[len..], "=")?
        .trim()
        .strip_suffix(';')?
        .trim();
    if len == 0 || rhs.is_empty() || !rhs.bytes().all(|b| is_word(b) || b == b':') {
        return None;
    }
    Some(UseLeaf {
        path: segments(rhs).collect(),
        alias: Some(&rest[..len]),
        glob: false,
    })
}

/// THE CLOCK READ, SLEEP OR PHASE-2 READ ONE IMPORTED NAME BINDS where the
/// line view cannot see it, if any: the needle, and the finding's detail.
/// For a needle `a::b` (a [`SUPERVISE_CLOCKS`] or [`SUPERVISE_READS`] name):
/// - the whole of it imported (`std::thread::sleep`, `std::process::id`,
///   `approvals::typed_at`), renamed or not: named bare from then on, never
///   as `a::b`;
/// - a one-word wall reader imported (`journal::unix_ms`): a bare CALL is
///   seen, but the reader passed bare as a fn item (`unwrap_or_else(unix_ms)`)
///   is not, so the import is what is named; any other one-word name only
///   when renamed;
/// - a head of it renamed (`std::thread as t`, `Instant as Mono`,
///   `approvals as ledger`): `t::sleep`, `Mono::now`;
/// - its module globbed in (`std::thread::*`): `sleep` bare.
///
/// A method (`.elapsed()`, `.try_recv()`) is not imported. A reader of the
/// ledger by its path ([`ledger_by_path`]) imported is named as phase 2's
/// call is. `as _` binds nothing to call, and an import under its own name
/// (`use std::thread;`, `use std::time::Instant;`) keeps the path the lines
/// are read for.
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

/// The free live body of another file a `use` statement imports, if any: by
/// its module's path (`use super::shell_startup::read;`) or in a group of that
/// module (`shell_startup::{ShellStartup, read}`, across lines).
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

/// The rule. WHAT IT READS, AND WHERE ITS SIGHT ENDS: the supervisor's own
/// shipped files, line by line on the code view, and each import there as the
/// names it binds ([`use_leaves`]) — so a clock read is found by its path, in
/// a group, renamed, globbed in (`std::thread::*`) or aliased as a type. It
/// is lexical, and does not see: a macro that expands to a read; a glob of a
/// wall reader's module (`use aterm_time::*;`, then a reader passed bare as a
/// fn item — a bare call is still seen); a qualified path (`<Instant>::now()`);
/// and a clock read inside a helper OUTSIDE the supervisor's files that the
/// loop calls (the harness's), which is put behind a door where one is found
/// — the retired Codex lane's process-age reader (`started_ms`) was moved
/// into a derived door's live body (`Fx::codex_records`) for that reason.
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

/// A shipped supervisor file outside the clock's homes.
fn supervise_fx_scope(rel: &str) -> bool {
    rel.starts_with(SUPERVISE[0])
        && !test_path(rel)
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
    // Each segment too: an import that binds a clock under a name the line
    // view cannot see names a segment of it (`use std::thread::{self,
    // sleep};` names `thread`, `Instant as Mono` names `Instant`).
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
        if !supervise_fx_scope(&file.rel) || !file.mentions(&needles) {
            continue;
        }
        let homes = reader_homes(file);
        let read_homes = read_homes(file);
        // A clock read on line `no` is at home: a wall read in a wall
        // reader's own body, or a phase-2 read in its own home `fn`.
        let at_home = |no: usize, needle: &str| {
            (homes.contains(&no) && wall_read(needle)) || read_homes.get(&no) == Some(&needle)
        };
        // Imports are read whole, as the names they bind ([`use_leaves`]);
        // only their spans are blanked in the executable view below.
        let uses = use_statements(file);
        let mut look_homes = look_homes(file);
        if file.rel == SUPERVISE_LEDGER {
            look_homes.extend(
                super::graph::attribute(file)
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
            // In a live body's own file, a fn that calls one IS one
            // ([`live_readers`]): the call is named where that fn is called.
            if starts_use(code) || live_file {
                continue;
            }
            if let Some(r) = live_call(code, &file.rel, &readers) {
                out.push(file.finding(SUPERVISE_FX.id, line.no, live_bypass(r, "calls")));
            }
        }
        // Each import, as the names it binds: one that binds a clock read, a
        // sleep or a phase-2 read under a name the lines cannot see is named
        // at its first line, where the name is bound.
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

/// A phase-3 finding's detail for a live body `r` called or imported above
/// its door.
fn live_bypass(r: &LiveReader<'_>, how: &str) -> String {
    format!(
        "{how} {}, the live body of a derived door, above its door: its look reaches no \
         tape — ask the door (`ctx.probe`, `Fx::policy_probe`, `Fx`'s own), or make this `fn` a \
         home of that live body",
        live_name(r)
    )
}

/// The approval ledger's module: its readers by path are found by the
/// convention every one of them keeps ([`ledger_by_path`]).
const SUPERVISE_LEDGER: &str = "crates/aterm-agent/src/supervise/approvals.rs";

/// The ledger's BY-PATH READERS: each `fn X` of its module that has an
/// `fn X_in` beside it — `X(path, …)` is `X_in` over the file read by its
/// path, for the window's host and the CLI, and the loop reads the file once
/// through `Fx::read_text` and hands the text to `X_in`. Found by the
/// convention rather than listed, so a reader added beside its `_in` form is
/// a phase-3 home in the module (where it reads the file) and a phase-2
/// finding wherever the loop calls it, with no list to keep in step.
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

/// The ledger reader by path ([`ledger_by_path`]) one line of code calls
/// through its module's path (`approvals::open_wall(…)`, or passes as a fn
/// item), if any — never its `_in` form, a name the path only begins.
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

/// The lines of a file that belong to a phase-3 look's home `fn`
/// ([`SUPERVISE_LOOK_HOMES`]: this file's, by name).
fn look_homes(file: &SourceFile) -> std::collections::BTreeSet<usize> {
    let rel = file.rel.strip_prefix(SUPERVISE[0]).unwrap_or(&file.rel);
    super::graph::attribute(file)
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

/// The lines of a file that belong to a phase-2 read's home `fn` in its own
/// file ([`SUPERVISE_READ_HOMES`]), each with the needle that home is for.
fn read_homes(file: &SourceFile) -> std::collections::BTreeMap<usize, &'static str> {
    let rel = file.rel.strip_prefix(SUPERVISE[0]).unwrap_or(&file.rel);
    super::graph::attribute(file)
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

/// The lines of a file that belong to a wall reader's own `fn` in its own
/// file ([`SUPERVISE_READER_HOMES`]), signature and body: the wall clock's
/// home, for a [`wall_read`] alone.
fn reader_homes(file: &SourceFile) -> std::collections::BTreeSet<usize> {
    let rel = file.rel.strip_prefix(SUPERVISE[0]).unwrap_or(&file.rel);
    super::graph::attribute(file)
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

/// Whether the [`SUPERVISE_CLOCKS`] needle `needle` reads the WALL clock (the
/// system's time, `SystemTime::now` or a one-word wall reader), which is all a
/// wall reader's own body is the home of: never a sleep, nor the monotonic
/// clock.
fn wall_read(needle: &str) -> bool {
    needle == "SystemTime::now"
        || SUPERVISE_CLOCKS
            .iter()
            .any(|(name, shape, _)| matches!(shape, ClockShape::Reader) && *name == needle)
}

/// The direct clock read, sleep or phase-2 read one line of code makes, if
/// any, and the name that hit: the first word hit of a [`SUPERVISE_CLOCKS`]
/// or [`SUPERVISE_READS`] name that sits in its [`ClockShape`]. The door's own
/// `self.fx.sleep(…)` is no hit: `sleep` counts only as `thread::sleep`; nor
/// is `self.fx.try_recv_mail(…)` or an `approvals::typed_at_in(…)`: a name is
/// a word, never the start of a longer one.
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

/// The look at the machine one line of code makes past a derived door, if
/// any ([`SUPERVISE_LOOKS`], in their [`ClockShape`]s), and the name that hit.
/// An import is no look (`use crate::harness::footer::read_small;`): the
/// calls it makes possible are each a finding where they are made.
fn supervise_look(code: &str) -> Option<(&'static str, &'static str)> {
    if starts_use(code) {
        return None;
    }
    first_hit(code, SUPERVISE_LOOKS.iter(), |_| true)
}

/// Whether a line of code begins a `use` statement.
fn starts_use(code: &str) -> bool {
    let head = code.trim_start();
    ["use ", "pub use ", "pub(crate) use ", "pub(super) use "]
        .iter()
        .any(|u| head.starts_with(u))
}

/// The first included name that sits in `code` in its shape. An allowed
/// home read cannot hide another name later on the same line.
pub(super) fn first_hit<'a>(
    code: &str,
    names: impl Iterator<Item = &'a (&'static str, ClockShape, &'static str)>,
    include: impl Fn(&str) -> bool,
) -> Option<(&'static str, &'static str)> {
    for (needle, shape, what) in names {
        for at in hits(code, needle, Bound::Word) {
            let before = code[..at].trim_end_matches([' ', '\t']);
            let after = code[at + needle.len()..].trim_start_matches([' ', '\t']);
            let pathed = before.ends_with("::");
            let called = after.starts_with('(');
            let read = match shape {
                ClockShape::Named => true,
                // A `fn` of the reader's name is a definition, not a read: its
                // body is what is read, line by line.
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

/// Whether `rest`, what follows a [`ClockShape::PathCall`] head, is one or
/// more `::segment`s and then a call: `::read(`, `::OpenOptions::new()`.
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

// store-current: a store `current` path is resolved before anything runs from
// it.

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

// licence-header: every .rs carries its SPDX and copyright lines.

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
                ("crates/aterm-scrollback/src/storage.rs", src),
                ("crates/aterm-effects/src/file_feed.rs", src),
                ("crates/aterm-gui/src/lib.rs", src),
                ("crates/aterm-aos/src/log.rs", src),
                ("crates/aterm-aos/tests/toy.rs", src),
            ],
        );
        assert_eq!(
            spots(&found),
            [
                "crates/aterm-aos/src/log.rs:1",
                "crates/aterm-aos/src/log.rs:2",
                "crates/aterm-aos/src/log.rs:2",
                "crates/aterm-aos/src/log.rs:4",
                "crates/aterm-aos/src/log.rs:5",
                "crates/aterm-grid/src/lib.rs:1",
                "crates/aterm-grid/src/lib.rs:2",
                "crates/aterm-grid/src/lib.rs:2",
                "crates/aterm-grid/src/lib.rs:4",
                "crates/aterm-grid/src/lib.rs:5",
                "crates/aterm-scrollback/src/storage.rs:1",
                "crates/aterm-scrollback/src/storage.rs:2",
                "crates/aterm-scrollback/src/storage.rs:2",
                "crates/aterm-scrollback/src/storage.rs:4",
                "crates/aterm-scrollback/src/storage.rs:5",
            ]
        );
    }

    /// The thread, socket and filesystem shapes the path needles miss: a
    /// `thread::Builder`, a module named inside a grouped `std` import however
    /// it wraps, and a module's own items imported or called by path.
    #[test]
    fn a_thread_builder_and_a_grouped_std_import_are_found() {
        let src = "use std::{fs, io::{self, Read}};\n\
                   use std::{\n    collections::HashMap,\n    net::TcpStream as Tcp,\n    thread,\n};\n\
                   fn f() { let _ = std::thread::Builder::new(); }\n\
                   use std::{io, sync::Arc};\n\
                   // use std::{fs};\n\
                   #[cfg(test)]\nmod tests {\n    use std::{fs, thread};\n}\n\
                   use std::thread::{Builder, spawn};\n\
                   fn g() { std::thread::scope(|s| { s.spawn(|| ()); }); }\n\
                   use std::net::*;\n\
                   std::thread_local! { static N: u8 = 0; }\n";
        let found = run(&ENGINE_PLATFORM, &[("crates/aterm-grid/src/lib.rs", src)]);
        assert_eq!(
            found
                .iter()
                .map(|f| format!("{}:{}", f.line, f.detail))
                .collect::<Vec<_>>(),
            [
                "1:imports `std::fs` in a group (the filesystem)",
                "4:imports `std::net` in a group (a socket)",
                "4:names `TcpStream` (a socket)",
                "5:imports `std::thread` in a group (a thread)",
                "7:names `thread::Builder` (a thread)",
                "7:names `std::thread` (a thread)",
                "14:names `std::thread` (a thread)",
                "15:names `std::thread` (a thread)",
                "16:names `std::net` (a socket)",
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

    /// A module its parent declares test-only is test code whatever its name
    /// (and so is what lies in its own directory); a shipped declaration, one
    /// nested in an inline module, or a missing file exempts nothing.
    #[test]
    fn a_module_the_parent_declares_test_only_is_test_code() {
        let threaded = "fn rig() { std::thread::scope(|s| { s.spawn(|| ()); }); }\n";
        let lib = "pub mod writer;\n#[cfg(test)]\nmod rig;\n\
                   #[cfg(all(test, unix))]\n#[path = \"../fixtures/deep_rig.rs\"]\nmod deep;\n\
                   #[cfg(test)]\nmod tests {\n    mod inner;\n}\n#[cfg(test)]\nmod gone;\n";
        let found = run(
            &ENGINE_PLATFORM,
            &[
                ("crates/aterm-restore/src/lib.rs", lib),
                ("crates/aterm-restore/src/rig.rs", threaded),
                ("crates/aterm-restore/src/rig/part.rs", threaded),
                ("crates/aterm-restore/fixtures/deep_rig.rs", threaded),
                ("crates/aterm-restore/src/writer.rs", threaded),
                ("crates/aterm-restore/src/inner.rs", threaded),
            ],
        );
        assert_eq!(
            spots(&found),
            [
                "crates/aterm-restore/src/inner.rs:1",
                "crates/aterm-restore/src/writer.rs:1",
            ],
            "{found:?}"
        );
        assert_eq!(out_of_line_mod("pub(crate) mod rig;"), Some("rig"));
        assert_eq!(out_of_line_mod("mod rig {"), None);
        assert_eq!(path_attr("#[path = \"a/b.rs\"]"), Some("a/b.rs"));
        assert_eq!(
            normal_path("crates/x/src/../t/./y.rs").as_deref(),
            Some("crates/x/t/y.rs")
        );
        assert_eq!(normal_path("../y.rs"), None);
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
    fn a_protected_folder_literal_is_found_in_gui_cli_and_amacs_host_shipped_code() {
        let src = "let a = home.join(\"Documents\");\nlet b = \"~/Downloads/x\";\n\
                   let c = Path::new(\"/Volumes/Ext\");\nlet d = \"Library/CloudStorage\";\n\
                   // ~/Desktop in prose\nlet e = DocumentsView::new();\n\
                   const DATA: &str = \"/System/Volumes/Data\";\n\
                   let f = \"/System/Volumes/Data/Volumes/Ext\";\n\
                   #[cfg(test)]\nmod tests {\n    const P: &str = \"~/Documents\";\n}\n";
        let found = run(
            &PROTECTED_FOLDER,
            &[
                ("crates/aterm-gui/src/lib.rs", src),
                ("crates/aterm-gui/src/lib_tests.rs", src),
                ("crates/aterm-containment/src/consent.rs", src),
                ("crates/amacs-files/src/host.rs", src),
                ("crates/amacs/src/store.rs", src),
            ],
        );
        assert_eq!(
            spots(&found),
            [
                "crates/amacs-files/src/host.rs:1",
                "crates/amacs-files/src/host.rs:2",
                "crates/amacs-files/src/host.rs:3",
                "crates/amacs-files/src/host.rs:4",
                "crates/amacs-files/src/host.rs:8",
                "crates/aterm-gui/src/lib.rs:1",
                "crates/aterm-gui/src/lib.rs:2",
                "crates/aterm-gui/src/lib.rs:3",
                "crates/aterm-gui/src/lib.rs:4",
                "crates/aterm-gui/src/lib.rs:8",
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
    fn the_keeper_maps_no_region() {
        // Line 2 is the region API, clippy's to catch by path (crates/aterm-keeper/clippy.toml);
        // lines 4 and 6 name a mapping call no path could, a libc call and a local declaration.
        let src = "fn hold(fd: OwnedFd) { let len = fd_size(fd.as_fd()); }\n\
                   let r = aterm_uds::shm::Region::map_readonly(fd.as_fd(), len);\n\
                   // a comment may say mmap\n\
                   unsafe { libc::mmap(p, n, 1, 1, fd, 0) };\n\
                   let s = \"mmap in a literal\";\n\
                   unsafe extern \"C\" { fn munmap(addr: *mut u8, len: usize) -> i32; }\n\
                   #[cfg(test)]\nfn t() { let _ = mmap(16384); }\n";
        let found = run(
            &KEEPER_NO_MMAP,
            &[
                ("crates/aterm-keeper/src/server.rs", src),
                ("crates/aterm-keeper/src/core_companion_tests.rs", src),
                ("crates/aterm-keeper/tests/kernel_custody.rs", src),
                ("crates/aterm-uds/src/shm.rs", src),
            ],
        );
        assert_eq!(
            spots(&found),
            [
                "crates/aterm-keeper/src/server.rs:4",
                "crates/aterm-keeper/src/server.rs:6",
            ]
        );
    }

    #[test]
    fn the_crash_restore_names_no_socket() {
        let src = "use std::net::TcpStream;\n\
                   // a comment may say socket\n\
                   let s = \"a UnixStream in a literal\";\n\
                   fn f(s: aterm_uds::CtlStream) {}\n\
                   let n = sockets_seen + 1;\n\
                   #[cfg(test)]\nfn t() { let _ = std::os::unix::net::UnixStream::pair(); }\n";
        let found = run(
            &RESTORE_STAYS_LOCAL,
            &[
                ("crates/aterm-restore/src/recover.rs", src),
                ("crates/aterm-restore/src/recover_tests.rs", src),
                ("crates/aterm-restore/tests/engine_conformance.rs", src),
                ("crates/aterm-gui/src/crash_restore.rs", src),
            ],
        );
        assert_eq!(
            spots(&found),
            [
                "crates/aterm-restore/src/recover.rs:1",
                "crates/aterm-restore/src/recover.rs:1",
                "crates/aterm-restore/src/recover.rs:4",
            ]
        );
    }

    #[test]
    fn no_off_host_crate_has_the_crash_restore_in_its_closure() {
        use crate::scan_set::test_fixtures::{disk_tree, workspace_manifests};
        let closure_of = |off_host: (&str, &str), session_deps: &str, label: &str| {
            let mut files = workspace_manifests(&[
                off_host,
                ("aterm-session", session_deps),
                ("aterm-uds", ""),
                (
                    "aterm-restore",
                    "[dependencies]\naterm-uds = { workspace = true }\n",
                ),
            ]);
            for dir in [
                "aterm-gui",
                "aterm-types",
                off_host.0,
                "aterm-session",
                "aterm-uds",
                "aterm-restore",
            ] {
                files.push((format!("crates/{dir}/src/lib.rs"), String::new()));
            }
            files.push(("Cargo.lock".to_string(), "version = 4\n".to_string()));
            let root = disk_tree("aterm-census-guards", label, &files);
            let corpus = super::super::Corpus::of(&[]);
            let tree = Tree {
                root: &root,
                corpus: &corpus,
            };
            let mut found = Vec::new();
            restore_stays_local(&tree, &mut found);
            let _ = std::fs::remove_dir_all(&root);
            found
        };
        let closure = |link_deps: &str, session_deps: &str, label: &str| {
            closure_of(("aterm-link", link_deps), session_deps, label)
        };
        // Every crate that carries bytes off the host is held to it, the fleet's TLS crate,
        // the HTTP stream and the window's fabric endpoint as the fabric's link is.
        for off_host in ["aterm-net", "aterm-http", "aterm-gui-fabric"] {
            let found = closure_of(
                (
                    off_host,
                    "[dependencies]\naterm-restore = { workspace = true }\n",
                ),
                "",
                &format!("restore-local-{off_host}"),
            );
            assert_eq!(found.len(), 1, "{off_host}: {found:?}");
            assert_eq!(found[0].path, format!("crates/{off_host}/Cargo.toml"));
            let found = closure_of(
                (
                    off_host,
                    "[dependencies]\naterm-uds = { workspace = true }\n",
                ),
                "",
                &format!("restore-local-{off_host}-clean"),
            );
            assert!(found.is_empty(), "{off_host}: {found:?}");
        }
        let link_via_session = "[dependencies]\naterm-uds = { workspace = true }\naterm-session = { workspace = true }\n";
        let found = closure(link_via_session, "", "restore-local-clean");
        assert!(found.is_empty(), "{found:?}");
        let found = closure(
            "[dependencies]\naterm-restore = { workspace = true }\n",
            "",
            "restore-local-link",
        );
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].path, "crates/aterm-link/Cargo.toml");
        assert!(found[0].detail.contains("aterm-restore is in"), "{found:?}");
        // Through a crate between them: the closure is walked, not the direct edges.
        let found = closure(
            link_via_session,
            "[target.'cfg(unix)'.dependencies]\naterm-restore = { workspace = true }\n",
            "restore-local-session",
        );
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].path, "crates/aterm-link/Cargo.toml");
        assert!(
            found[0]
                .detail
                .contains("through aterm-session -> aterm-restore"),
            "{found:?}"
        );
    }

    #[test]
    fn the_keepers_closure_holds_no_gui_crate() {
        use crate::scan_set::test_fixtures::{disk_tree, workspace_manifests};
        let mut files = workspace_manifests(&[
            (
                "aterm-keeper",
                "[dependencies]\naterm-uds = { workspace = true }\n",
            ),
            ("aterm-uds", "[dependencies]\nobjc-sys = \"0.3\"\n"),
        ]);
        for dir in ["aterm-gui", "aterm-types", "aterm-keeper", "aterm-uds"] {
            files.push((format!("crates/{dir}/src/lib.rs"), String::new()));
        }
        files.push(("Cargo.lock".to_string(), "version = 4\n".to_string()));
        let root = disk_tree("aterm-census-guards", "keeper-deps", &files);
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
        assert!(found[0].detail.contains("`objc-sys`"), "{found:?}");

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
        files.push(("Cargo.lock".to_string(), "version = 4\n".to_string()));
        let root = disk_tree("aterm-census-guards", "keeper-gui-crate", &files);
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
        files.push(("Cargo.lock".to_string(), "version = 4\n".to_string()));
        let root = disk_tree("aterm-census-guards", "keeper-renamed-gui", &files);
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

    /// A GUI package two registry hops below the keeper's closure is found
    /// through Cargo.lock, with its chain; a member reached through the lock is
    /// read by its manifest, so its dev-dependencies (which its lock entry
    /// lists) are not followed; a dependency the lock lacks is a finding.
    #[test]
    fn the_keepers_closure_is_walked_through_cargo_lock() {
        use crate::scan_set::test_fixtures::{disk_tree, workspace_manifests};
        let lock = "version = 4\n\n\
                    [[package]]\nname = \"aterm-uds\"\nversion = \"0.1.0\"\n\
                    dependencies = [\n \"helper\",\n]\n\n\
                    [[package]]\nname = \"helper\"\nversion = \"1.0.0\"\n\
                    source = \"registry+https://github.com/rust-lang/crates.io-index\"\n\
                    dependencies = [\n \"log\",\n \"middle\",\n]\n\n\
                    [[package]]\nname = \"middle\"\nversion = \"0.2.0\"\n\
                    source = \"registry+https://github.com/rust-lang/crates.io-index\"\n\
                    dependencies = [\n \"objc2-foundation 0.3.1\",\n]\n\n\
                    [[package]]\nname = \"objc2-foundation\"\nversion = \"0.3.1\"\n\
                    source = \"registry+https://github.com/rust-lang/crates.io-index\"\n\n\
                    [[package]]\nname = \"objc2-foundation\"\nversion = \"0.2.2\"\n\
                    source = \"registry+https://github.com/rust-lang/crates.io-index\"\n\n\
                    [[package]]\nname = \"log\"\nversion = \"0.4.0\"\n\
                    dependencies = [\n \"winit\",\n]\n\n\
                    [[package]]\nname = \"winit\"\nversion = \"0.30.0\"\n\
                    source = \"registry+https://github.com/rust-lang/crates.io-index\"\n";
        let tree_with = |name: &str, uds_deps: &str| {
            let mut files = workspace_manifests(&[
                (
                    "aterm-keeper",
                    "[dependencies]\naterm-uds = { workspace = true }\n",
                ),
                ("aterm-uds", uds_deps),
                ("aterm-log", "[dev-dependencies]\nwinit = \"0.30\"\n"),
            ]);
            for (path, contents) in files.iter_mut() {
                if path == "Cargo.toml" {
                    contents.push_str("log = { path = \"crates/aterm-log\" }\n");
                }
            }
            for dir in [
                "aterm-gui",
                "aterm-types",
                "aterm-keeper",
                "aterm-uds",
                "aterm-log",
            ] {
                files.push((format!("crates/{dir}/src/lib.rs"), String::new()));
            }
            files.push(("Cargo.lock".to_string(), lock.to_string()));
            disk_tree("aterm-census-guards", name, &files)
        };
        let run_on = |root: &std::path::Path| {
            let corpus = super::super::Corpus::of(&[]);
            let tree = Tree {
                root,
                corpus: &corpus,
            };
            let mut found = Vec::new();
            keeper_gui_deps(&tree, &mut found);
            let _ = std::fs::remove_dir_all(root);
            found
        };
        let found = run_on(&tree_with(
            "keeper-lock",
            "[dependencies]\nhelper = \"1\"\n",
        ));
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].path, "crates/aterm-uds/Cargo.toml");
        assert!(
            found[0].detail.contains(
                "reaches `objc2-foundation` through helper -> middle -> objc2-foundation"
            ),
            "{found:?}"
        );
        let found = run_on(&tree_with(
            "keeper-lock-stale",
            "[dependencies]\nnot-locked = \"1\"\n",
        ));
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            found[0]
                .detail
                .contains("`not-locked`, which Cargo.lock does not hold"),
            "{found:?}"
        );
    }

    #[test]
    fn the_std_command_alias_is_built_renamed_or_re_aliased_only_in_aterm_uds() {
        let src = "use aterm_uds::spawnfd::StdCommand;\n\
                   fn a(c: &mut StdCommand) -> &mut StdCommand { c }\n\
                   let c = StdCommand::new(\"ls\");\n\
                   let d = aterm_uds::spawnfd::StdCommand :: new (\"ls\");\n\
                   use aterm_uds::spawnfd::StdCommand as Cmd;\n\
                   type Cmd2 = StdCommand;\n\
                   type Cmd3 = ::aterm_uds :: spawnfd::StdCommand;\n\
                   // StdCommand::new in prose\n\
                   let s = \"StdCommand::new\";\n\
                   let e = StdCommand::new_with(x);\n\
                   #[cfg(test)]\nmod tests {\n    fn t() { let _ = StdCommand::new(\"x\"); }\n}\n";
        let found = run(
            &STD_COMMAND,
            &[
                ("crates/aterm-gui/src/x.rs", src),
                ("crates/aterm-uds/src/spawnfd.rs", src),
            ],
        );
        assert_eq!(
            spots(&found),
            [
                "crates/aterm-gui/src/x.rs:3",
                "crates/aterm-gui/src/x.rs:4",
                "crates/aterm-gui/src/x.rs:5",
                "crates/aterm-gui/src/x.rs:6",
                "crates/aterm-gui/src/x.rs:7",
                "crates/aterm-gui/src/x.rs:13",
            ],
            "{found:?}"
        );
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

    /// Each way the supervisor's code can read a clock or sleep past its
    /// door, with the line it fires on.
    #[test]
    fn every_clock_read_past_the_supervisors_door_is_found() {
        let cases: &[(&str, &str, usize)] = &[
            ("a monotonic read", "let now = Instant::now();\n", 1),
            (
                "a qualified one",
                "fn f() {\n    let t = std::time::Instant::now();\n}\n",
                2,
            ),
            (
                "the monotonic clock passed as a fn item",
                "let at = *since.get_or_insert_with(Instant::now);\n",
                1,
            ),
            ("an elapsed", "if at.elapsed() < step {\n}\n", 1),
            (
                "an elapsed on its own chained line",
                "let ms = since\n    .elapsed()\n    .as_millis();\n",
                2,
            ),
            (
                "an elapsed by path",
                "let d = marks.map(Instant::elapsed);\n",
                1,
            ),
            ("a wall read", "let w = std::time::SystemTime::now();\n", 1),
            ("limit's wall reader", "let s = limit::unix_now();\n", 1),
            (
                "limit's wall reader passed as a fn item",
                "self.clock.map_or_else(limit::unix_now_ms, |s| s * 1000)\n",
                1,
            ),
            (
                "the journal's wall reader",
                "let age = unix_ms().saturating_sub(taken);\n",
                1,
            ),
            (
                "aterm_time's wall reader",
                "let secs = aterm_time::unix_secs().unwrap_or(0);\n",
                1,
            ),
            ("a sleep", "std::thread::sleep(BACKOFF);\n", 1),
            ("a sleep imported", "use std::thread::sleep;\n", 1),
            (
                "a sleep after an own-name import",
                "fn f() {\n    use std::thread; thread::sleep(D);\n}\n",
                2,
            ),
            (
                "a monotonic read after a multiline import closes",
                "fn f() {\n    use std::{\n        thread,\n    }; let _ = Instant::now();\n}\n",
                4,
            ),
            (
                "a renamed sleep in the second import on one line",
                "use std::fmt; use std::thread::sleep as nap;\n",
                1,
            ),
            (
                "a process read after an own-name import",
                "fn f() {\n    use std::process; let _ = process::id();\n}\n",
                2,
            ),
            (
                "a sleep until an instant",
                "std::thread::sleep_until(at);\n",
                1,
            ),
            ("a park with a timeout", "thread::park_timeout(d);\n", 1),
            // A name brought in by an import the line view cannot follow: a
            // group, a rename, a glob, a type alias. Each is named at its
            // statement's first line, where the name is bound.
            (
                "a sleep imported in a group with its module, then called bare",
                "use std::thread::{self, sleep};\nfn f() {\n    sleep(BUSY_SINK_BACKOFF);\n}\n",
                1,
            ),
            (
                "a sleep imported alone in braces",
                "use std::thread::{sleep};\n",
                1,
            ),
            (
                "a sleep renamed in a nested group across lines",
                "use std::{\n    thread::{sleep as nap, spawn},\n    time::Duration,\n};\n",
                1,
            ),
            (
                "the thread module renamed",
                "use std::thread as t;\nfn f() {\n    t::sleep(D);\n}\n",
                1,
            ),
            ("the thread module globbed in", "use std::thread::*;\n", 1),
            (
                "the monotonic clock renamed",
                "use std::time::Instant as Mono;\nfn f() {\n    let t = Mono::now();\n}\n",
                1,
            ),
            (
                "the monotonic clock renamed in a group",
                "use std::time::{Duration, Instant as Mono};\n",
                1,
            ),
            (
                "the wall clock's type renamed",
                "use std::time::SystemTime as Wall;\n",
                1,
            ),
            (
                "the monotonic clock aliased as a type",
                "pub(crate) type Mono = std::time::Instant;\n",
                1,
            ),
            (
                "the journal's wall reader imported in a group, passed bare",
                "use super::journal::{json_str, unix_ms};\nfn f() {\n    \
                 let t = x.unwrap_or_else(unix_ms);\n}\n",
                1,
            ),
            (
                "aterm_time's wall reader renamed",
                "use aterm_time::{unix_millis as now_ms};\n",
                1,
            ),
            // Phase 2: the recorder's other doors.
            (
                "this process's id",
                "let holder = format!(\"x:{}\", std::process::id());\n",
                1,
            ),
            (
                "a delivery taken",
                "match m.rx.try_recv() {\n    _ => {}\n}\n",
                1,
            ),
            (
                "a delivery awaited",
                "let got = m.rx.recv_timeout(slice);\n",
                1,
            ),
            (
                "the ledger read by its path",
                "let typed = approvals::typed_at(path, sid);\n",
                1,
            ),
            (
                "another reader of the ledger by its path",
                "open_wall = approvals::open_wall(path, sid);\n",
                1,
            ),
            (
                "the relaunch's wait read off the ledger by its path",
                "let w = approvals::relaunch_waiting(path, sid);\n",
                1,
            ),
            ("the ledger cut", "approvals::bound(path);\n", 1),
            (
                "this process's id imported bare",
                "use std::process::{self, id};\n",
                1,
            ),
            (
                "a reader of the ledger by its path imported bare",
                "use super::approvals::{Ledger, typed_at};\n",
                1,
            ),
            (
                "the ledger's module renamed",
                "use super::approvals as ledger;\n",
                1,
            ),
        ];
        for (why, src, line) in cases {
            let found = run(
                &SUPERVISE_FX,
                &[("crates/aterm-agent/src/supervise/run.rs", src)],
            );
            assert_eq!(
                spots(&found),
                [format!("crates/aterm-agent/src/supervise/run.rs:{line}")],
                "MISS — {why}:\n{src}"
            );
        }
    }

    /// The door's own reads, a value or a field of a reader's name, prose,
    /// test code, a reader's own definition, the clock's home files and code
    /// outside the supervisor are silent.
    #[test]
    fn the_door_and_the_clocks_homes_are_silent() {
        let src = "let now = self.fx.mono();\nlet d = self.fx.since(at);\nself.fx.sleep(pause);\n\
                   let w = fx.wall_ms() + self.fx.stamp_ms();\n\
                   // Instant::now() and t.elapsed() in prose\n\
                   let s = \"thread::sleep(1)\";\nlet unix_ms = d.unix_ms;\nsettle.unix_ms = unix_ms;\n\
                   fn wall_reset_at(unix_ms: i64, elapsed: Duration) {}\nlet x = r.elapsed;\n\
                   #[cfg(test)]\nmod tests {\n    fn t() { let _ = Instant::now(); std::thread::sleep(D); }\n}\n";
        // The wall readers' own definitions, each in its own file.
        let limit = "pub fn unix_now() -> i64 {\n    \
                     aterm_time::unix_secs().map_or(0, |s| s as i64)\n}\n\
                     pub fn unix_now_ms() -> i64 {\n    \
                     aterm_time::unix_millis().map_or(0, |m| m as i64)\n}\n";
        let journal = "pub fn unix_ms() -> i64 {\n    \
                       aterm_time::unix_millis().map_or(0, |m| m as i64)\n}\n";
        let found = run(
            &SUPERVISE_FX,
            &[
                ("crates/aterm-agent/src/supervise/run.rs", src),
                ("crates/aterm-agent/src/supervise/limit.rs", limit),
                ("crates/aterm-agent/src/supervise/journal.rs", journal),
            ],
        );
        assert!(found.is_empty(), "{:?}", spots(&found));
        // Phase 2's doors, their homes, and names a door's are the start of.
        let doors = "match self.fx.try_recv_mail(&m.rx) {\n    _ => {}\n}\n\
                     let got = self.fx.recv_mail_timeout(&m.rx, slice);\n\
                     let typed = approvals::typed_at_in(&body, sid);\n\
                     let w = approvals::open_wall_in(&body, sid);\nself.fx.bound(path);\n\
                     let r = approvals::relaunch_waiting_in(body, sid);\n";
        let holder = "pub(super) fn default_holder() -> String {\n    \
                      format!(\"aterm-supervise:{}\", std::process::id())\n}\n";
        let found = run(
            &SUPERVISE_FX,
            &[
                ("crates/aterm-agent/src/supervise/run.rs", doors),
                ("crates/aterm-agent/src/supervise/claim.rs", holder),
            ],
        );
        assert!(found.is_empty(), "{:?}", spots(&found));
        // A home is a home for its own read alone.
        let elsewhere = "fn output_within() {\n    let _ = std::process::id();\n}\n\
                         fn default_holder() {\n    let _ = rx.try_recv();\n}\n";
        assert_eq!(
            spots(&run(
                &SUPERVISE_FX,
                &[("crates/aterm-agent/src/supervise/run.rs", elsewhere)]
            )),
            [
                "crates/aterm-agent/src/supervise/run.rs:2",
                "crates/aterm-agent/src/supervise/run.rs:5",
            ]
        );
        let bypass =
            "let now = Instant::now();\nstd::thread::sleep(D);\nlet w = limit::unix_now();\n";
        assert!(
            run(
                &SUPERVISE_FX,
                &[
                    ("crates/aterm-agent/src/supervise/fx.rs", bypass),
                    ("crates/aterm-agent/src/supervise/tape/codec.rs", bypass),
                    ("crates/aterm-agent/src/supervise/transport.rs", bypass),
                    ("crates/aterm-agent/src/supervise/mail.rs", bypass),
                    ("crates/aterm-agent/src/supervise/run_tests.rs", bypass),
                    ("crates/aterm-agent/src/harness/upgrade_drive.rs", bypass),
                    ("crates/aterm-gui/src/harness_host.rs", bypass),
                ]
            )
            .is_empty()
        );
        // The supervisor's policy is the supervisor's: in scope.
        assert_eq!(
            spots(&run(
                &SUPERVISE_FX,
                &[(
                    "crates/aterm-agent/src/supervise/policy/turn_end.rs",
                    bypass
                )]
            )),
            [
                "crates/aterm-agent/src/supervise/policy/turn_end.rs:1",
                "crates/aterm-agent/src/supervise/policy/turn_end.rs:2",
                "crates/aterm-agent/src/supervise/policy/turn_end.rs:3",
            ]
        );
    }

    /// A WALL READER'S HOME IS ITS OWN `fn`, IN ITS OWN FILE, FOR THE WALL
    /// READ ALONE: `limit::unix_now{,_ms}` and `journal::unix_ms` read the
    /// system's wall clock in their bodies, and nothing else there is
    /// excused — a sleep or a monotonic read in a reader's body is a read past
    /// the door — and a `fn` of a reader's name anywhere else (a method of
    /// that name on some type) is no home at all, whatever it reads.
    #[test]
    fn a_wall_readers_home_is_its_own_fn_in_its_own_file_for_the_wall_alone() {
        let limit = "pub fn unix_now() -> i64 {\n    \
                     aterm_time::unix_secs().map_or(0, |s| s as i64)\n}\n\
                     pub fn unix_now_ms() -> i64 {\n    std::thread::sleep(D);\n    \
                     let _ = std::time::Instant::now();\n    \
                     aterm_time::unix_millis().map_or(0, |m| m as i64)\n}\n";
        let journal = "pub fn unix_ms() -> i64 {\n    \
                       let _ = std::time::SystemTime::now();\n    \
                       aterm_time::unix_millis().map_or(0, |m| m as i64)\n}\n";
        let elsewhere = "impl Waiter {\n    fn unix_ms(&self) -> i64 {\n        \
                         std::thread::sleep(Duration::from_secs(5));\n        \
                         let _ = std::time::Instant::now();\n        \
                         aterm_time::unix_millis().map_or(0, |m| m as i64)\n    }\n}\n";
        assert_eq!(
            spots(&run(
                &SUPERVISE_FX,
                &[
                    ("crates/aterm-agent/src/supervise/limit.rs", limit),
                    ("crates/aterm-agent/src/supervise/journal.rs", journal),
                    ("crates/aterm-agent/src/supervise/probe2.rs", elsewhere),
                    // The journal's reader by name, in the limit's file.
                    ("crates/aterm-agent/src/supervise/policy/limit.rs", journal),
                ]
            )),
            [
                "crates/aterm-agent/src/supervise/limit.rs:5",
                "crates/aterm-agent/src/supervise/limit.rs:6",
                "crates/aterm-agent/src/supervise/policy/limit.rs:2",
                "crates/aterm-agent/src/supervise/policy/limit.rs:3",
                "crates/aterm-agent/src/supervise/probe2.rs:3",
                "crates/aterm-agent/src/supervise/probe2.rs:4",
                "crates/aterm-agent/src/supervise/probe2.rs:5",
            ]
        );
        // The holder's resolution is a home in the claim's file alone, and
        // for this process's id alone.
        let holder = "pub(super) fn default_holder() -> String {\n    \
                      format!(\"aterm-supervise:{}\", std::process::id())\n}\n\
                      fn default_holder_too() {\n    let _ = std::process::id();\n}\n";
        let wrong = "fn default_holder() {\n    let _ = rx.try_recv();\n}\n";
        assert_eq!(
            spots(&run(
                &SUPERVISE_FX,
                &[
                    ("crates/aterm-agent/src/supervise/claim.rs", holder),
                    ("crates/aterm-agent/src/supervise/run.rs", holder),
                    ("crates/aterm-agent/src/supervise/escalate.rs", wrong),
                ]
            )),
            [
                "crates/aterm-agent/src/supervise/claim.rs:5",
                "crates/aterm-agent/src/supervise/escalate.rs:2",
                "crates/aterm-agent/src/supervise/run.rs:2",
                "crates/aterm-agent/src/supervise/run.rs:5",
            ]
        );
        // An allowed read on a line excuses only itself, never a second
        // clock or delivery on that line, in either kind of home.
        let mixed_wall = "fn unix_now() {\n    \
                          let _ = SystemTime::now(); thread::sleep(D);\n    \
                          let _ = SystemTime::now(); let _ = process::id();\n    \
                          let _ = aterm_time::unix_secs(); let _ = rx.recv_timeout(D);\n    \
                          let _ = SystemTime::now(); let _ = aterm_time::unix_secs();\n}\n";
        let mixed_holder = "fn default_holder() {\n    \
                            let _ = process::id(); let _ = rx.try_recv();\n}\n";
        assert_eq!(
            spots(&run(
                &SUPERVISE_FX,
                &[
                    ("crates/aterm-agent/src/supervise/limit.rs", mixed_wall),
                    ("crates/aterm-agent/src/supervise/claim.rs", mixed_holder),
                ]
            )),
            [
                "crates/aterm-agent/src/supervise/claim.rs:2",
                "crates/aterm-agent/src/supervise/limit.rs:2",
                "crates/aterm-agent/src/supervise/limit.rs:3",
                "crates/aterm-agent/src/supervise/limit.rs:4",
            ]
        );
    }

    /// AN IMPORT THAT KEEPS A CLOCK'S NAME IS SILENT: the module or the type
    /// brought in under its own name (`use std::thread;`, `Instant`,
    /// `SystemTime`, the ledger's module and its `_in` readers), a group of
    /// them, `self` in a group, `as _` (which binds nothing to call), a
    /// rename of something no clock is named through, a type alias of a
    /// type that is not a clock, and an import in test code.
    #[test]
    fn an_import_that_keeps_a_clocks_name_is_silent() {
        let src = "use std::thread;\nuse std::time::{Duration, Instant};\n\
                   use std::time::SystemTime;\nuse std::fmt::Write as _;\n\
                   use std::thread::sleep as _;\n\
                   use super::approvals::{self, Ledger, typed_at_in};\n\
                   use super::journal::{Journal, json_str, unix_ms_of};\n\
                   use std::{\n    process,\n    thread::{self, spawn},\n};\n\
                   use aterm_phase::prompt::fixtures as f;\n\
                   use std::os::unix::fs::OpenOptionsExt;\n\
                   type Clocked = fn() -> Instant;\ntype Mark = (Instant, u64);\n\
                   pub(crate) type Stamp = super::policy::sleeps::SleepStamp;\n\
                   #[cfg(test)]\nmod tests {\n    use std::thread::{self, sleep};\n    \
                   use std::time::Instant as Mono;\n}\n";
        let found = run(
            &SUPERVISE_FX,
            &[("crates/aterm-agent/src/supervise/run.rs", src)],
        );
        assert!(found.is_empty(), "{:?}", spots(&found));
    }

    /// PHASE 3: each look at the machine made past a derived door is a
    /// finding, in each shape it is written.
    #[test]
    fn a_look_at_the_machine_past_a_derived_door_is_named() {
        let cases = [
            ("a folder listed", "let e = std::fs::read_dir(&dir)?;\n"),
            ("a file read through fs", "let b = fs::read(&p)?;\n"),
            ("a file opened by its type", "let f = File::open(&p)?;\n"),
            (
                "a file opened with options",
                "let f = std::fs::OpenOptions::new().append(true).open(p)?;\n",
            ),
            (
                "a path resolved",
                "let r = std::fs::canonicalize(p).ok();\n",
            ),
            ("a path resolved as a method", "let r = p.canonicalize();\n"),
            (
                "a link examined",
                "let m = std::fs::symlink_metadata(&p);\n",
            ),
            ("a link read", "let to = std::fs::read_link(&p);\n"),
            ("a metadata read", "let m = p.metadata()?;\n"),
            ("an existence asked", "if p.exists() {\n}\n"),
            ("a directory asked", "if p.is_dir() {\n}\n"),
            ("a file asked", "if p.is_file() {\n}\n"),
            (
                "a child spawned",
                "let mut c = aterm_uds::spawnfd::command(\"lsof\");\n",
            ),
            (
                "a process read through the kernel",
                "let a = atpkg::caller_shell::process_args(pid);\n",
            ),
            (
                "the kernel asked",
                "let cwd = aterm_sysprobe::process_cwd(pid);\n",
            ),
            (
                "this process's environment",
                "let p = std::env::var(\"PATH\");\n",
            ),
            (
                "the passwd database",
                "let h = atpkg::platform::account_home();\n",
            ),
            ("the zone read", "let o = limit::tz_offset_s();\n"),
            ("a zone's offset", "let o = limit::offset_at(Some(z), t);\n"),
            (
                "a named zone's offset",
                "let o = super::limit::zone_offset_at(z, t);\n",
            ),
            (
                "the local zone's offset passed as a fn item",
                "let at = limit::reset_at_with(&s, n, l, &limit::local_offset_at);\n",
            ),
            (
                "the injected zone reader read past its door",
                "let at = limit::reset_at_with(&s, n, l, &self.zone_offset);\n",
            ),
            (
                "the injected local reader called",
                "let o = (self.local_offset_at)(at);\n",
            ),
            ("a small file read bare", "let t = read_small(&p, 4096);\n"),
            (
                "a file opened by the footer's reader",
                "let f = crate::harness::footer::open_regular(&p)?;\n",
            ),
            (
                "the transcripts read",
                "ctx.shell_cwds = crate::harness::footer::shell_cwds(&claude, cwd);\n",
            ),
        ];
        for (why, src) in cases {
            let found = run(
                &SUPERVISE_FX,
                &[("crates/aterm-agent/src/supervise/approval_loop.rs", src)],
            );
            assert_eq!(
                spots(&found),
                ["crates/aterm-agent/src/supervise/approval_loop.rs:1"],
                "MISS — {why}:\n{src}"
            );
        }
    }

    /// PHASE 3's doors, homes and look-alikes are silent: the probe's and
    /// `Fx`'s own methods, a field or a type of a look's name, an import, a
    /// `fn` line, a home `fn` in its own file, and the probe's live arm.
    #[test]
    fn the_derived_doors_and_their_homes_are_silent() {
        let doors = "ctx.shell_cwds = ctx.probe.shell_cwds(w, cwd);\n\
                     let real = ctx.probe.real_components(comps);\n\
                     let o = self.fx.zone_offset(live, z, t);\n\
                     let o = self.fx.local_offset_at(live, t);\n\
                     let o = self.fx.tz_offset();\n\
                     let s = stamp(l.now_ms, l.tz_offset_s, true);\n\
                     file: Option<std::fs::File>,\n\
                     use crate::harness::footer::read_small;\n\
                     use std::fs;\n\
                     fn shell_cwds(&self, worker: &WorkerEnv, cwd: &Path) -> Vec<PathBuf>;\n\
                     let started_ms = thread.is_none();\n\
                     zone_offset: limit::zone_offset_at,\n\
                     // std::fs::read_dir(p) in prose\n\
                     let s = \"p.exists()\";\n\
                     #[cfg(test)]\nmod tests {\n    fn t() { let _ = std::fs::read(p); }\n}\n";
        assert_eq!(
            spots(&run(
                &SUPERVISE_FX,
                &[("crates/aterm-agent/src/supervise/approval_loop.rs", doors)]
            )),
            // The constructor's default, a pathed fn item outside its home `fn`.
            ["crates/aterm-agent/src/supervise/approval_loop.rs:12"]
        );
        // A home `fn` in its own file is a home; the same `fn` name in another
        // file is not, and a look in another `fn` of the home's file is not.
        let homes = "fn read(worker: &WorkerEnv) {\n    let t = read_small(p, 9);\n}\n\
                     fn parse(text: &str) {\n    let t = read_small(p, 9);\n}\n";
        assert_eq!(
            spots(&run(
                &SUPERVISE_FX,
                &[
                    (
                        "crates/aterm-agent/src/supervise/policy/shell_startup.rs",
                        homes
                    ),
                    ("crates/aterm-agent/src/supervise/policy/question.rs", homes),
                ]
            )),
            [
                "crates/aterm-agent/src/supervise/policy/question.rs:2",
                "crates/aterm-agent/src/supervise/policy/question.rs:5",
                "crates/aterm-agent/src/supervise/policy/shell_startup.rs:5",
            ]
        );
        // The probe's live arm is where the policy's looks live.
        let live = "let e = std::fs::read_dir(&dir);\n\
                    let a = atpkg::caller_shell::process_args(group);\n\
                    let r = std::fs::canonicalize(p);\n";
        assert!(
            run(
                &SUPERVISE_FX,
                &[(
                    "crates/aterm-agent/src/supervise/policy/probe_live.rs",
                    live
                )]
            )
            .is_empty()
        );
        // A clock read in a phase-3 home is still a clock read.
        let clock = "fn read(worker: &WorkerEnv) {\n    let n = Instant::now();\n}\n";
        assert_eq!(
            spots(&run(
                &SUPERVISE_FX,
                &[(
                    "crates/aterm-agent/src/supervise/policy/shell_startup.rs",
                    clock
                )]
            )),
            ["crates/aterm-agent/src/supervise/policy/shell_startup.rs:2"]
        );
    }

    /// Stand-ins for the files that hold the policy's live bodies: each home
    /// `fn` of [`SUPERVISE_LOOK_HOMES`] makes its look, and the entries the
    /// probe's live arm calls reach a home only through their file's own calls
    /// (or another home file's), as the real ones do.
    fn live_body_files() -> Vec<(&'static str, &'static str)> {
        vec![
            (
                "crates/aterm-agent/src/supervise/policy/shell_startup.rs",
                "pub fn read(worker: &WorkerEnv, launch: &Path) -> Result<S, String> {\n    \
                 let s = snapshots(&claude)?;\n}\n\
                 fn snapshots(claude: &Path) -> Result<Vec<PathBuf>, String> {\n    \
                 let e = std::fs::read_dir(claude);\n}\n\
                 pub fn tool_kinds(worker: &WorkerEnv) -> Vec<Kind> {\n    Vec::new()\n}\n",
            ),
            (
                "crates/aterm-agent/src/supervise/policy/claude_settings.rs",
                "pub fn owner_asks(worker: &WorkerEnv, launch: &Path) -> Vec<OwnerAsk> {\n    \
                 for (s, v) in parsed(worker, launch) {}\n}\n\
                 pub(crate) fn parsed(worker: &WorkerEnv, launch: &Path) -> Vec<Doc> {\n    \
                 let t = read_settings_in(None, p);\n}\n\
                 fn read_settings_in(fence: Option<&Path>, path: &Path) -> Option<String> {\n    \
                 crate::harness::footer::read_small(path, 9)\n}\n\
                 pub fn rule_tool(rule: &str) -> Option<&str> {\n    None\n}\n",
            ),
            (
                "crates/aterm-agent/src/supervise/policy/git_config.rs",
                "impl WorkerEnv {\n    pub fn raw_git_view(\n        &self,\n        dir: &Path,\n    \
                 ) -> Result<GitView, String> {\n        self.view_at(dir)\n    }\n    \
                 fn view_at(&self, dir: &Path) -> Result<GitView, String> {\n        \
                 let o = aterm_uds::spawnfd::command(\"git\");\n    }\n    \
                 pub fn git_settings_env(&self, launch: &Path) -> Result<(), String> {\n        \
                 self.settings_env_where(launch)\n    }\n    \
                 pub fn settings_env_where(&self, launch: &Path) -> Result<(), String> {\n        \
                 for d in super::claude_settings::parsed(self, launch) {}\n    }\n    \
                 pub fn var(&self, key: &str) -> Option<&str> {\n        None\n    }\n}\n",
            ),
            (
                "crates/aterm-agent/src/supervise/limit.rs",
                "pub fn zone_offset_at(zone: &str, unix: i64) -> Option<i64> {\n    \
                 let t = std::fs::read(zone);\n}\n\
                 fn offset_in(zone_offset_at: &dyn Fn(&str) -> Option<i64>) -> i64 {\n    \
                 zone_offset_at(\"z\").unwrap_or(0)\n}\n\
                 pub fn reset_at_with(zone_offset_at: &dyn Fn(&str) -> Option<i64>) -> i64 {\n    \
                 offset_in(zone_offset_at)\n}\n\
                 fn date_offset(zone: &str, unix: i64) -> Option<i64> {\n    \
                 let t = std::fs::read(zone);\n}\n\
                 pub fn rogue_reset(unix: i64) -> i64 {\n    \
                 date_offset(\"z\", unix).unwrap_or(0)\n}\n",
            ),
        ]
    }

    /// A PROBE'S LIVE BODY CALLED FROM ABOVE ITS DOOR IS NAMED: each call the
    /// policy made before T5 — the shell's startup, the owner's asks, the git
    /// view and the settings' `env` read off the worker — made again from the
    /// loop or the policy, and the live probe itself named
    /// there, is a finding, as is a live body imported to be called bare (on
    /// one line, or in a group across lines). Control: the footer's transcript
    /// reader, a phase-3 look by name, is found among them.
    #[test]
    fn a_probes_live_body_called_above_its_door_is_named() {
        let lp = "let s = crate::supervise::policy::shell_startup::read(w, cwd);\n\
                  let a = crate::supervise::policy::claude_settings::owner_asks(w, cwd);\n\
                  let w = crate::supervise::policy::LiveProbe.worker_proc(group, &sid);\n\
                  ctx.shell_cwds = crate::harness::footer::shell_cwds(&claude, cwd);\n";
        let policy = "worker.git_settings_env(&ctx.cwd)?;\n\
                      let v = worker.raw_git_view(dir, &|_| false)?;\n\
                      let t = LiveProbe.hidden_twin(comps)?;\n\
                      let d = super::claude_settings::parsed(worker, launch);\n";
        let te = "let r = super::limit::rogue_reset(now);\n";
        let imports = "use super::policy::shell_startup::read;\n\
                       use super::policy::{\n    \
                       claude_settings::{OwnerAsk, owner_asks},\n    \
                       probe::PolicyProbe,\n};\n";
        let mut files = live_body_files();
        files.extend([
            ("crates/aterm-agent/src/supervise/approval_loop.rs", lp),
            (
                "crates/aterm-agent/src/supervise/policy/approval.rs",
                policy,
            ),
            ("crates/aterm-agent/src/supervise/turn_end_loop.rs", te),
            ("crates/aterm-agent/src/supervise/run.rs", imports),
        ]);
        assert_eq!(
            spots(&run(&SUPERVISE_FX, &files)),
            [
                "crates/aterm-agent/src/supervise/approval_loop.rs:1",
                "crates/aterm-agent/src/supervise/approval_loop.rs:2",
                "crates/aterm-agent/src/supervise/approval_loop.rs:3",
                "crates/aterm-agent/src/supervise/approval_loop.rs:4",
                "crates/aterm-agent/src/supervise/policy/approval.rs:1",
                "crates/aterm-agent/src/supervise/policy/approval.rs:2",
                "crates/aterm-agent/src/supervise/policy/approval.rs:3",
                "crates/aterm-agent/src/supervise/policy/approval.rs:4",
                "crates/aterm-agent/src/supervise/run.rs:1",
                "crates/aterm-agent/src/supervise/run.rs:2",
                "crates/aterm-agent/src/supervise/turn_end_loop.rs:1",
            ]
        );
    }

    /// THE PROBE, ITS LIVE BODIES' OWN FILES AND THEIR PURE NEIGHBOURS ARE
    /// SILENT: a call of the probe (a method of the same name as a live body
    /// included), a pure `fn` of a live body's file, a fn that hands an
    /// injected reader on (`INJECTED`: its call of the home's name is its
    /// parameter), a type imported from a live body's module, and the chain
    /// inside the live bodies' files — an entry, the `fn`s it reaches, and a
    /// call into another home file's live body from one of them. The probe's
    /// door and live arm name the live probe.
    #[test]
    fn the_probe_and_its_live_bodies_own_files_are_silent() {
        let policy = "ctx.probe.settings_env(worker, &ctx.cwd)?;\n\
                      let v = ctx.probe.git_view(worker, dir, &writes)?;\n\
                      let s = ctx.probe.shell_startup(w, cwd);\n\
                      let a = ctx.probe.owner_asks(w, cwd);\n\
                      let k = super::shell_startup::tool_kinds(&w);\n\
                      let r = super::claude_settings::rule_tool(rule);\n\
                      let e = worker.var(\"HOME\");\n\
                      let p = self.fx.policy_probe();\n\
                      let at = super::limit::reset_at_with(&|z| self.fx.zone_offset(z));\n\
                      use super::shell_startup::ShellStartup;\n\
                      fn read(&self) -> u8 {\n    0\n}\n";
        let door = "pub struct LiveProbe;\n\
                    #[derive(Clone)]\npub(crate) struct TapedProbe {\n    live: LiveProbe,\n}\n";
        let arm = "impl PolicyProbe for LiveProbe {\n    \
                   fn shell_startup(&self, w: &WorkerEnv, l: &Path) -> R {\n        \
                   super::shell_startup::read(w, l)\n    }\n    \
                   fn settings_env(&self, w: &WorkerEnv, l: &Path) -> Result<(), String> {\n        \
                   w.git_settings_env(l)\n    }\n}\n";
        let mut files = live_body_files();
        files.extend([
            (
                "crates/aterm-agent/src/supervise/policy/approval.rs",
                policy,
            ),
            ("crates/aterm-agent/src/supervise/policy/probe.rs", door),
            ("crates/aterm-agent/src/supervise/policy/probe_live.rs", arm),
        ]);
        let found = run(&SUPERVISE_FX, &files);
        assert!(found.is_empty(), "{:?}", spots(&found));
    }

    /// THE LEDGER'S READERS BY PATH ARE FOUND BY THEIR CONVENTION: a reader
    /// beside its `_in` form reads the file in the ledger's module unnamed
    /// (phase 3's home there) and is named wherever the loop calls it (phase
    /// 2), while a look in another `fn` of the module, and the `_in` form
    /// called from the loop, are what they are.
    #[test]
    fn the_ledgers_readers_by_path_are_found_by_their_convention() {
        let ledger = "pub fn awaited_try_in(body: &str) -> u8 {\n    0\n}\n\
                      pub fn awaited_try(path: &Path) -> u8 {\n    \
                      awaited_try_in(&std::fs::read_to_string(path).unwrap_or_default())\n}\n\
                      pub fn rogue(path: &Path) {\n    let _ = std::fs::read(path);\n}\n";
        let lp = "let t = approvals::awaited_try(path, sid);\n\
                  let u = approvals::awaited_try_in(&body, sid);\n\
                  let v = super::approvals::awaited_try(path, sid);\n\
                  use super::approvals::{awaited_try_in, awaited_try};\n\
                  use super::approvals::awaited_try_in;\n";
        assert_eq!(
            spots(&run(
                &SUPERVISE_FX,
                &[
                    ("crates/aterm-agent/src/supervise/approvals.rs", ledger),
                    ("crates/aterm-agent/src/supervise/turn_end_loop.rs", lp),
                ]
            )),
            [
                "crates/aterm-agent/src/supervise/approvals.rs:8",
                "crates/aterm-agent/src/supervise/turn_end_loop.rs:1",
                "crates/aterm-agent/src/supervise/turn_end_loop.rs:3",
                // Imported bare in a group, beside its `_in` form; the `_in`
                // form imported alone is silent.
                "crates/aterm-agent/src/supervise/turn_end_loop.rs:4",
            ]
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
