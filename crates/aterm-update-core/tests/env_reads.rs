// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! NO ENVIRONMENT ALTERNATIVES IN A SHIPPED BINARY — the owner's rule, mechanised.
//!
//! 2026-09-22, verbatim: *"audit all these env flags and bullshit. I really don't like
//! them. I want the one true best batteries included default on path. Delete
//! alternatives. in the future, we could add settings, but NOT ENV VARS those are for
//! development."* Phase 4 of `docs/DESIGN-atpkg-vendor-direct-updates-2026-09-22.md`
//! deleted the update system's user-facing knobs (`ATPKG_DISABLE`, `ATPKG_TOKEN`,
//! `ATERM_NO_AUTO_UPDATE`, `ATERM_UPDATE_OWNER`, the runtime `GITHUB_TOKEN`/`GH_TOKEN`
//! rungs, …) and moved every development seam behind `aterm_types::dev_seam!`, which a
//! release build compiles to `None`. This test is what keeps it that way: it scans the
//! source of every first-party crate the shipped `aterm` binary links (the dependency
//! closure of `crates/aterm`, read from the manifests — dev- and build-dependencies
//! excluded) for environment READS of `ATERM_*` / `ATPKG_*` / `GITHUB_TOKEN` /
//! `GH_TOKEN`, and fails on any name outside an explicit allow-list:
//!
//! * [`INTERNAL_PROTOCOL`] — a parent handing its own child something (fds, nonces,
//!   markers, handoff directories). Not a flag: nothing a person sets.
//! * [`BUILD_STAMPS`] — `env!`/`option_env!` values the build derives from committed
//!   metadata or the release cutter sets. Not read at run time at all.
//! * [`DEV_SEAMS`] — read ONLY through `aterm_types::dev_seam!`; a plain `env::var` of
//!   one of these names is a failure, because that read would ship.
//! * [`OUT_OF_SCOPE`] — modes and presentation/diagnostic knobs outside the update
//!   system (`ATERM_HEADLESS`, `ATERM_LOG`, the renderer and font seams, …), listed so
//!   the ratchet holds: a NEW name read anywhere in shipped code fails here until
//!   somebody decides, in this file, which of the four it is.
//!
//! [`RETIRED`] names the deleted knobs. None may be read by any first-party source — the
//! doctor's presence-only detector ([`RETIRED_DETECTOR`]), which tells a person that an
//! export of one no longer does anything, excepted — and none may reappear on an
//! allow-list.
//!
//! "Read" is PRESUMED (2026-09-23 review). Every mention of a family name in release
//! code — a string literal that is exactly the name, or a use of a `const` holding one —
//! counts as a run-time read unless it is provably not one: an argument (at any depth) of
//! a writer (`Command::env`/`env_remove`/`envs`, `env::set`/`unset`, `set_var`,
//! `remove_var`), a `use`, the `const NAME: &str = "…"` declaration itself, an element of
//! a [`WRITE_TABLES`] table (the child-environment deny list), the doctor's
//! [`RETIRED_DETECTOR`], or a listed [`MENTIONS`] entry. A reader call around it at any
//! depth decides the kind: `dev_seam!` (a seam), `env!`/`option_env!` (a stamp), anything
//! in an `env` module or with `env` in its name (run time). The first version counted a
//! name only as a recognised reader's DIRECT first argument, and six shapes of a
//! retired-knob read passed it (`aterm_log::env::read`, a `for` loop over names,
//! `OsStr::new` inside the reader, a helper without `env` in its name, a `vars()`
//! comparison, `&*CONST`); presumption also found three real reads it had missed
//! (`aterm-verify`'s `var_path`). A retired name inside any string's CONTENT, or in a
//! script file a shipped crate carries under `src/` (the shell integration), fails too:
//! a shell reads the environment where no Rust call is there to see it.
//! `#[cfg(test)]` items, test-only modules and `tests/`, `benches/`, `examples/` and
//! build scripts are not release code and are skipped. Comments never count: the scan
//! runs over a lexed copy of each file with comments and string contents masked.
//!
//! It replaces `veto_flag_rule.rs`, which pinned HOW four boolean update knobs were read
//! (`env_flag_engaged`): three of the four no longer exist, and the fourth
//! (`ATERM_DEBUG_SEAMLESS_REEXEC`) is a development seam read through `dev_seam!` and
//! `env_flag_engaged` both (`crates/aterm-gui/src/app_update_screen.rs`).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Parent → child protocol and markers: set by aterm itself for a process it starts.
const INTERNAL_PROTOCOL: &[&str] = &[
    // The front door → session handoffs and the `aterm --no-reroute` marker.
    "ATERM_AGENTS_DIR",
    "ATERM_REROUTE_DIR",
    "__ATERM_REROUTE_PASSTHROUGH",
    // atpkg's shell hook → the shell integration, and a pass → its own children.
    "ATPKG_AGENTS",
    "ATPKG_SPAWNER_PID",
    // Every child of the window, and the session identity it hands a shell.
    "ATERM_CHILD",
    "ATERM_SESSION_ID",
    "ATERM_PARENT_SESSION_ID",
    "ATERM_LAUNCH_NONCE",
    "ATERM_EDGE_READ",
    "ATERM_EDGE_SIGNAL",
    "ATERM_EDGE_TOKENS",
    "ATERM_EDGE_WRITE",
    // The updater's re-exec and the seamless handoff's fd/nonce protocol.
    "ATERM_UPDATE_REEXEC",
    "ATERM_UPDATED_FROM",
    "ATERM_UPDATE_EXPECTED_BUILD",
    "ATERM_UPDATE_EXPECTED_COMMIT",
    "ATERM_UPDATE_EXPECTED_DMG_SHA256",
    "ATERM_SEAMLESS_FDS",
    "ATERM_SEAMLESS_LAYOUT",
    "ATERM_SEAMLESS_MANIFEST",
    "ATERM_SEAMLESS_NONCE",
    "ATERM_SEAMLESS_TARGET",
    "ATERM_HANDOFF_CLAIM",
    "ATERM_HANDOFF_COMMIT_FD",
    "ATERM_HANDOFF_PARENT_BIRTH",
    "ATERM_HANDOFF_PARENT_PID",
    "ATERM_HANDOFF_PROOF_TERM",
    "ATERM_HANDOFF_READY_FD",
    "ATERM_HANDOFF_RENDEZVOUS",
    "ATERM_HANDOFF_CONTROL_SOCKET_IDENTITY",
    // The window/session → the shell integration it injects (its directory, the
    // zsh ZDOTDIR hand-back, the per-shell nonce, the loaded guard, WSL's cwd), and the
    // controller-spawn observation hint.
    "ATERM_SHELL_INTEGRATION_DIR",
    "ATERM_SHELL_INTEGRATION_INSTALLED",
    "ATERM_SHELL_NONCE",
    "ATERM_ORIGINAL_ZDOTDIR",
    "ATERM_UNSET_ZDOTDIR",
    "ATERM_WSL_CWD",
    "ATERM_OBSERVE_SESSION_ID",
    // The harness's state directory, and the drive/mux children.
    "ATERM_HARNESS_STATE",
    "ATERM_DRIVE_READY",
    "ATERM_MUX",
    "ATERM_MUX_BASE",
    "ATERM_MUX_NOTICE",
    "ATERM_MUX_OUTER_SESSION_ID",
    "ATERM_FABRIC_COMMAND",
];

/// Compile-time values (`env!` / `option_env!`) the build derives or the cutter sets.
const BUILD_STAMPS: &[&str] = &[
    "ATERM_ATPKG_INDEX_OWNER",
    "ATERM_BINARY_TARGET",
    "ATERM_BUILD_NUMBER",
    "ATERM_BUILD_PROFILE",
    "ATERM_BUILD_TIME",
    "ATERM_COMPILER_COMMIT",
    "ATERM_COMPILER_FLAVOR",
    "ATERM_COMPILER_HOST",
    "ATERM_COMPILER_TRUST_VERSION",
    "ATERM_COMPILER_VERSION_LINE",
    "ATERM_DEFAULT_OWNER",
    "ATERM_DEFAULT_REPO",
    "ATERM_DEV_COMMITS",
    "ATERM_GIT_COMMIT",
    "ATERM_GIT_COMMIT_FULL",
    "ATERM_GIT_DIRTY",
    "ATERM_PUBLISH_OWNER",
    "ATERM_PUBLISH_REPO",
    "ATERM_RELEASE_BUILD",
    "ATERM_TRUST_VERIFY",
    "ATERM_UPDATE_PIN_SHA256",
];

/// Development seams: compiled only under `cfg(any(debug_assertions, feature =
/// "dev-seams"))` through `aterm_types::dev_seam!`, which a shipped binary reads as
/// `None`. The release cutter never enables the feature (`aterm-release`'s buildplan,
/// `the_release_build_enables_no_development_seam`).
const DEV_SEAMS: &[&str] = &[
    "ATPKG_REGISTRY",
    "ATPKG_STAGE_DISK_REVERIFY",
    "ATERM_UPDATE_ROOT",
    "ATERM_DEBUG_SEAMLESS_REEXEC",
    "ATERM_DEBUG_RELAUNCH_NUDGE",
    "ATERM_DEBUG_STATUS_BARS",
    // The strain row's fake saturated reading (strain_host.rs `debug_load`), for
    // captures and demos.
    "ATERM_DEBUG_STRAIN",
    "ATERM_HANDOFF_READY_TIMEOUT_MS",
    "ATERM_HANDOFF_PROOF_TIMEOUT_MS",
    "ATERM_SESSION_MODEL",
    // The fabric push lane's step hold (subscribe.rs `push_held`): the bridge race
    // test in aterm-link parks the lane between adoption and drain to force the
    // interleaving deterministically.
    "ATERM_TEST_PUSH_HOLD",
];

/// Outside Phase 4's scope — not the update system. Modes (`ATERM_HEADLESS`), the log
/// filter (`ATERM_LOG`), launch geometry and renderer/font/GPU seams, the control-socket
/// selectors, the fabric and link fault seams, and the verify gate's own knobs (it is
/// linked for `aterm help rust`). Each is its own audit's to keep or delete; listing
/// them here is what makes a NEW name fail instead of slipping in beside them.
const OUT_OF_SCOPE: &[&str] = &[
    "ATERM_HEADLESS",
    "ATERM_LOG",
    "ATERM_VERBOSE",
    "ATERM_CONTAINMENT_MODE",
    "ATERM_CONTROL_SOCK",
    "ATERM_NO_CONTROL_SOCK",
    "ATERM_CONTROL_TOKEN",
    "ATERM_CTL",
    "ATERM_STATE_HOME",
    "ATERM_SHELL",
    "ATERM_EXEC",
    "ATERM_TERM_PROGRAM",
    "ATERM_AI_HINT",
    "ATERM_ALT_ARCHIVE",
    "ATERM_BIN",
    "ATERM_COLUMNS",
    "ATERM_LINES",
    "ATERM_CPU",
    "ATERM_GPU",
    "ATERM_GPU_ADAPTER",
    "ATERM_GPU_BACKEND",
    "ATERM_GPU_FRAME_LATENCY",
    "ATERM_GPU_MEMBLOCK",
    "ATERM_GPU_POWER",
    "ATERM_GPU_PRESENT_MODE",
    "ATERM_METAL",
    "ATERM_METAL_INJECT_LOSS",
    "ATERM_FONT",
    "ATERM_FONT_HINTING",
    "ATERM_FONT_PX",
    "ATERM_FONT_SUBPIXEL",
    "ATERM_EMOJI_FONT",
    "ATERM_FALLBACK_FONT",
    "ATERM_SYMBOL_FONT",
    "ATERM_RASTERIZER",
    "ATERM_STEM_GAMMA",
    "ATERM_NO_PROCEDURAL_GLYPHS",
    "ATERM_FORCE_HC_CHROME",
    "ATERM_FORCE_SCALE",
    "ATERM_HEADROOM_PX",
    "ATERM_TAB_STRIP_ROWS",
    "ATERM_WINDOWING_BEHAVIOR",
    "ATERM_NO_COLORSPACE_MATCH",
    "ATERM_NO_DARK_CHROME",
    "ATERM_NO_FULLSIZE_CONTENT",
    "ATERM_NO_PATH_REFRESH",
    "ATERM_NO_SHELL_INTEGRATION",
    "ATERM_LATENCY_TRACE",
    "ATERM_TRACE_BOOST",
    "ATERM_TRACE_LATENCY",
    "ATERM_TRACE_SPAWN",
    "ATERM_PTY_IDLE_POLL_US",
    "ATERM_SNAPSHOT_PATH",
    "ATERM_WATCHDOG",
    "ATERM_OPERATOR",
    "ATERM_NO_OPERATOR",
    "ATERM_OPERATOR_PROFILE",
    "ATERM_NET_CERT",
    "ATERM_NET_KEY",
    "ATERM_NET_LISTEN",
    "ATERM_FABRIC_FAIL_MINT_AT",
    "ATERM_FABRIC_FLEET",
    "ATERM_FABRIC_HOME",
    "ATERM_FABRIC_TRACE",
    "ATERM_LINK_FAULT",
    "ATERM_LINK_NOTIFY_FAULT",
    "ATERM_SKIP_GUI_SMOKE",
    "ATERM_VERIFY_BASE",
    "ATERM_VERIFY_ROOT",
    "ATERM_VERIFY_STAGE_TIMEOUT",
    "ATERM_VERIFY_TEST_THREADS",
    // Read through `aterm-verify`'s `var_path` helper, which the first version of this
    // gate could not see (it knew readers by name); the presumed-read scan found them.
    "ATERM_VERIFY_LOG",
    "ATERM_VERIFY_SNAPSHOT",
    "ATERM_VERIFY_TIMINGS",
    // The gate's own fixture tests move its machine lock off the per-user one
    // (`snapshot::MACHINE_LOCK_DIR_ENV`), so a gate never waits on its own test stage.
    "ATERM_VERIFY_MACHINE_LOCK_DIR",
    // A gate started by the one holding the machine runs inside its hold
    // (`snapshot::MACHINE_HOLDER_ENV`) instead of waiting on its own ancestor.
    "ATERM_VERIFY_MACHINE_HOLDER",
    // The verify gate → the build it runs (its own provenance stamps).
    "ATERM_BUILD_GIT_COMMIT",
    "ATERM_BUILD_GIT_COMMIT_FULL",
    "ATERM_BUILD_DEV_COMMITS",
];

/// The knobs Phase 4 deleted (2026-09-23). Read by nothing, allow-listed nowhere.
const RETIRED: &[&str] = &[
    "ATPKG_DISABLE",
    "ATPKG_TOKEN",
    "ATPKG_ACCOUNT",
    "ATPKG_INDEX_REPO",
    "ATPKG_REFUSE_TRACKED_INSTALL",
    "ATPKG_ALLOW_TRACKED_INSTALL",
    "ATPKG_UPDATE_INTERVAL_SECS",
    "ATPKG_AGENTS_EVERYWHERE",
    "ATERM_NO_AUTO_UPDATE",
    "ATERM_NO_AUTO_APPLY",
    "ATERM_NO_SEAMLESS_UPDATE",
    "ATERM_UPDATE_OWNER",
    "ATERM_UPDATE_REPO",
    "ATERM_UPDATE_TOKEN",
    "ATERM_UPDATE_INTERVAL_SECS",
    "ATERM_REROUTE_QUIET",
    "ATERM_REROUTE_STRICT",
    "ATERM_Z3_IS_ORACLE",
    "ATERM_NO_REROUTE",
    "ATERM_NO_HARNESS",
    "GITHUB_TOKEN",
    "GH_TOKEN",
];

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .canonicalize()
        .expect("workspace root")
}

fn is_family(name: &str) -> bool {
    let tail_ok = |rest: &str| {
        !rest.is_empty()
            && rest
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
    };
    // `__ATERM_…` is the spelling of an internal marker a shell reads
    // (`atpkg::reroute::PASSTHROUGH_ENV`): still the family, still gated.
    let name = name.strip_prefix("__").unwrap_or(name);
    name.strip_prefix("ATERM_").is_some_and(tail_ok)
        || name.strip_prefix("ATPKG_").is_some_and(tail_ok)
        || name == "GITHUB_TOKEN"
        || name == "GH_TOKEN"
}

// ---------------------------------------------------------------------------
// The shipped crate closure, from the manifests.
// ---------------------------------------------------------------------------

fn load_manifest(path: &Path) -> aterm_toml::Table {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    aterm_toml::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// The first-party crate directories `crate_dir` depends on for its BUILT artifact:
/// `[dependencies]` and every `[target.*.dependencies]`, optional ones included (a
/// feature may pull them in), dev- and build-dependencies excluded.
fn first_party_deps(
    root: &Path,
    workspace_deps: &aterm_toml::Table,
    crate_dir: &Path,
) -> Vec<PathBuf> {
    let manifest = load_manifest(&crate_dir.join("Cargo.toml"));
    let mut tables: Vec<&aterm_toml::Table> = Vec::new();
    if let Some(t) = manifest.get("dependencies").and_then(|v| v.as_table()) {
        tables.push(t);
    }
    if let Some(targets) = manifest.get("target").and_then(|v| v.as_table()) {
        for spec in targets.values().filter_map(|v| v.as_table()) {
            if let Some(t) = spec.get("dependencies").and_then(|v| v.as_table()) {
                tables.push(t);
            }
        }
    }
    let mut out = Vec::new();
    for table in tables {
        for (name, spec) in table.iter() {
            let Some(spec) = spec.as_table() else {
                continue;
            };
            let path = if spec.get("workspace").and_then(|v| v.as_bool()) == Some(true) {
                let key = spec.get("package").and_then(|v| v.as_str()).unwrap_or(name);
                workspace_deps
                    .get(key)
                    .or_else(|| workspace_deps.get(name.as_str()))
                    .and_then(|v| v.as_table())
                    .and_then(|t| t.get("path"))
                    .and_then(|v| v.as_str())
                    .map(|p| root.join(p))
            } else {
                spec.get("path")
                    .and_then(|v| v.as_str())
                    .map(|p| crate_dir.join(p))
            };
            if let Some(path) = path.and_then(|p| p.canonicalize().ok()) {
                out.push(path);
            }
        }
    }
    out
}

fn shipped_crates(root: &Path) -> BTreeSet<PathBuf> {
    let workspace = load_manifest(&root.join("Cargo.toml"));
    let workspace_deps = workspace
        .get("workspace")
        .and_then(|v| v.as_table())
        .and_then(|w| w.get("dependencies"))
        .and_then(|v| v.as_table())
        .cloned()
        .unwrap_or_else(aterm_toml::Table::new);
    let mut seen = BTreeSet::new();
    let mut stack = vec![
        root.join("crates/aterm")
            .canonicalize()
            .expect("crates/aterm"),
    ];
    while let Some(dir) = stack.pop() {
        if seen.insert(dir.clone()) {
            stack.extend(first_party_deps(root, &workspace_deps, &dir));
        }
    }
    seen
}

fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

// ---------------------------------------------------------------------------
// A small Rust lexer: comments and string contents masked, literals recorded.
// ---------------------------------------------------------------------------

struct Lexed {
    /// The source with every comment and every string/char literal's CONTENT replaced
    /// by spaces (newlines kept), so brace matching and call detection never see them.
    code: Vec<u8>,
    /// Every string literal: (content start, content end, content).
    strings: Vec<(usize, usize, String)>,
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn lex(src: &str) -> Lexed {
    let bytes = src.as_bytes();
    let mut code = bytes.to_vec();
    let mut strings = Vec::new();
    let blank = |code: &mut Vec<u8>, from: usize, to: usize| {
        for b in &mut code[from..to] {
            if *b != b'\n' {
                *b = b' ';
            }
        }
    };
    let mut i = 0;
    let n = bytes.len();
    while i < n {
        let prev_ident = i > 0 && is_ident_byte(bytes[i - 1]);
        if bytes[i..].starts_with(b"//") {
            let end = bytes[i..]
                .iter()
                .position(|&b| b == b'\n')
                .map_or(n, |p| i + p);
            blank(&mut code, i, end);
            i = end;
            continue;
        }
        if bytes[i..].starts_with(b"/*") {
            let mut depth = 1;
            let mut j = i + 2;
            while j < n && depth > 0 {
                if bytes[j..].starts_with(b"/*") {
                    depth += 1;
                    j += 2;
                } else if bytes[j..].starts_with(b"*/") {
                    depth -= 1;
                    j += 2;
                } else {
                    j += 1;
                }
            }
            blank(&mut code, i, j);
            i = j;
            continue;
        }
        // Raw strings: r"…", r#"…"#, br"…".
        if !prev_ident && (bytes[i] == b'r' || bytes[i..].starts_with(b"br")) {
            let mut j = i + if bytes[i] == b'b' { 2 } else { 1 };
            let mut hashes = 0;
            while j < n && bytes[j] == b'#' {
                hashes += 1;
                j += 1;
            }
            if j < n && bytes[j] == b'"' {
                let start = j + 1;
                let mut close = b"\"".to_vec();
                close.extend(std::iter::repeat_n(b'#', hashes));
                let end = bytes[start..]
                    .windows(close.len())
                    .position(|w| w == close.as_slice())
                    .map_or(n, |p| start + p);
                strings.push((start, end, src[start..end].to_string()));
                blank(&mut code, start, end);
                i = (end + close.len()).min(n);
                continue;
            }
        }
        if bytes[i] == b'"' || (!prev_ident && bytes[i..].starts_with(b"b\"")) {
            let start = i + if bytes[i] == b'"' { 1 } else { 2 };
            let mut j = start;
            while j < n && bytes[j] != b'"' {
                j += if bytes[j] == b'\\' { 2 } else { 1 };
            }
            let end = j.min(n);
            strings.push((start, end, src[start..end].to_string()));
            blank(&mut code, start, end);
            i = end + 1;
            continue;
        }
        if bytes[i] == b'\'' {
            // A char literal ('x', '\n', '\u{..}') — never a lifetime ('a followed by
            // an ident byte and no closing quote).
            if i + 3 < n && bytes[i + 1] == b'\\' {
                // An escape: the closing quote comes after the escaped byte ('\'',
                // '\n', '\u{1F600}').
                if let Some(p) = bytes[i + 3..].iter().take(12).position(|&b| b == b'\'') {
                    let close = i + 3 + p;
                    blank(&mut code, i + 1, close);
                    i = close + 1;
                    continue;
                }
            } else if let Some(c) = src[i + 1..].chars().next() {
                let w = c.len_utf8();
                if i + 1 + w < n && bytes[i + 1 + w] == b'\'' {
                    blank(&mut code, i + 1, i + 1 + w);
                    i += 2 + w;
                    continue;
                }
            }
        }
        i += 1;
    }
    Lexed { code, strings }
}

/// The byte ranges of `#[cfg(test)]` / `#[cfg(all(test, …))]` items in `code`, and the
/// file names of test-only modules declared out of line (`mod x;`, with or without a
/// `#[path = "…"]`) or pulled in with `include!` from inside a test item.
fn test_regions(src: &str, code: &[u8], file: &Path) -> (Vec<(usize, usize)>, Vec<PathBuf>) {
    let text = String::from_utf8_lossy(code);
    let dir = file.parent().expect("a file has a directory");
    let is_test_cfg = |attr: &str| {
        let squeezed: String = attr.chars().filter(|c| !c.is_whitespace()).collect();
        squeezed.starts_with("#[cfg(test)") || squeezed.starts_with("#[cfg(all(test")
    };
    let mut ranges = Vec::new();
    let mut files = Vec::new();
    // Every attributed item: walk each `#[` run, and when the run carries a test cfg,
    // mark the item it decorates.
    let mut i = 0;
    let bytes = code;
    while let Some(p) = text[i..].find("#[") {
        let start = i + p;
        // The run of attributes starting here.
        let mut j = start;
        let mut attrs: Vec<(usize, usize)> = Vec::new();
        loop {
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            if !text[j..].starts_with("#[") {
                break;
            }
            let mut depth = 0i32;
            let mut k = j + 1;
            while k < bytes.len() {
                match bytes[k] {
                    b'[' => depth += 1,
                    b']' => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
                k += 1;
            }
            attrs.push((j, k + 1));
            j = k + 1;
        }
        let attr_text = |(a, b): (usize, usize)| &src[a..b.min(src.len())];
        if attrs
            .iter()
            .any(|&r| is_test_cfg(&text[r.0..r.1.min(text.len())]))
        {
            // The item: up to its `;` (an out-of-line module) or its `{…}` body.
            let semi = text[j..].find(';').map(|p| j + p);
            let brace = text[j..].find('{').map(|p| j + p);
            // A `;` ends the item only for the item kinds that end that way; a `fn`
            // whose signature holds `[u8; 4]` still ends at its body's `}`.
            let head_kind = text[j..]
                .trim_start()
                .trim_start_matches("pub(crate)")
                .trim_start_matches("pub")
                .trim_start();
            let semi_item = ["mod ", "use ", "extern ", "type ", "const ", "static "]
                .iter()
                .any(|k| head_kind.starts_with(k));
            match (semi, brace) {
                (Some(s), b) if semi_item && b.is_none_or(|b| s < b) => {
                    let head = &text[j..s];
                    if let Some(name) = head.split_whitespace().skip_while(|w| *w != "mod").nth(1) {
                        let path_attr = attrs.iter().find_map(|&r| {
                            let a = attr_text(r);
                            a.contains("path")
                                .then(|| a.split('"').nth(1).map(str::to_string))?
                        });
                        if let Some(p) = path_attr {
                            files.push(dir.join(p));
                        }
                        files.push(dir.join(format!("{name}.rs")));
                        files.push(dir.join(name).join("mod.rs"));
                        if let Some(stem) = file.file_stem().and_then(|s| s.to_str())
                            && !["mod", "lib", "main"].contains(&stem)
                        {
                            files.push(dir.join(stem).join(format!("{name}.rs")));
                        }
                    }
                    ranges.push((start, s));
                }
                (_, Some(b)) => {
                    let mut depth = 0i32;
                    let mut k = b;
                    while k < bytes.len() {
                        match bytes[k] {
                            b'{' => depth += 1,
                            b'}' => {
                                depth -= 1;
                                if depth == 0 {
                                    break;
                                }
                            }
                            _ => {}
                        }
                        k += 1;
                    }
                    ranges.push((start, k));
                    // `include!("x.rs")` inside a test item makes x.rs test-only.
                    let body = &src[b..k.min(src.len())];
                    let mut from = 0;
                    while let Some(q) = body[from..].find("include!(\"") {
                        let s = from + q + "include!(\"".len();
                        if let Some(e) = body[s..].find('"') {
                            files.push(dir.join(&body[s..s + e]));
                        }
                        from = s;
                    }
                }
                _ => {}
            }
            i = j.max(start + 2);
        } else {
            i = j.max(start + 2);
        }
    }
    (ranges, files)
}

// ---------------------------------------------------------------------------
// Reads.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum ReadKind {
    /// `dev_seam!(…)` — compiled out of a shipped binary.
    Seam,
    /// `env!` / `option_env!` — a build stamp.
    Build,
    /// A run-time read: a reader call (`var`, `var_os`, `env::take`, `env::read`, an
    /// `…env…` helper) — or ANY mention the scanner cannot prove is not one (below).
    Runtime,
}

/// What the call whose argument list opens at `open` does with a name inside it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CallRole {
    /// A reader: the name is read.
    Read(ReadKind),
    /// A writer: `Command::env`/`env_remove`/`envs`, `env::set`/`unset`, `set_var`,
    /// `remove_var` — the name is set or cleared for a child, never read.
    Write,
    /// Anything else (`OsStr::new`, `.push`, `matches!`, a label): look further out.
    Neither,
}

/// The call whose argument list opens at `open` (the index of its `(`), classified.
fn call_role(code: &[u8], open: usize) -> CallRole {
    let text = String::from_utf8_lossy(&code[..open]);
    let trimmed = text.trim_end();
    // The path before `(`, `!` included for macros.
    let start = trimmed
        .char_indices()
        .rev()
        .find(|&(_, c)| !(c.is_ascii_alphanumeric() || c == '_' || c == ':' || c == '!'))
        .map_or(0, |(i, c)| i + c.len_utf8());
    let path = &trimmed[start..];
    let method = trimmed[..start].ends_with('.');
    if path.is_empty() {
        return CallRole::Neither;
    }
    let last = path.rsplit("::").next().unwrap_or(path);
    if method {
        // `Command::env(..)` and friends set a CHILD's environment.
        return match last {
            "env" | "env_remove" | "envs" => CallRole::Write,
            _ => CallRole::Neither,
        };
    }
    match last {
        "dev_seam!" => CallRole::Read(ReadKind::Seam),
        "env!" | "option_env!" => CallRole::Read(ReadKind::Build),
        "set_var" | "remove_var" => CallRole::Write,
        "set" | "unset" | "env_remove" | "env_clear" if path.contains("env::") => CallRole::Write,
        "var" | "var_os" => CallRole::Read(ReadKind::Runtime),
        // Every other function of an `env` module reads: `std::env::…`,
        // `aterm_log::env::take` / `read` (the workspace's lock-scoped reader).
        _ if path.contains("env::") => CallRole::Read(ReadKind::Runtime),
        other if other.to_ascii_lowercase().contains("env") && !other.ends_with('!') => {
            CallRole::Read(ReadKind::Runtime)
        }
        _ => CallRole::Neither,
    }
}

/// Tables whose `&str` elements are names STRIPPED from a child's environment, never
/// read: a family name listed in one is not a read of it.
const WRITE_TABLES: &[&str] = &["ENV_DENY_VARS"];

/// The one place a RETIRED name may be named in shipped code: the doctor's detector,
/// which reads PRESENCE only (`var_os(name).is_some()`), never acts on it, and tells a
/// person their export no longer does anything and which setting does
/// (`crates/atpkg/src/doctor.rs`, `RETIRED_OPT_OUTS`).
const RETIRED_DETECTOR: (&str, &str) = ("crates/atpkg/src/doctor.rs", "RETIRED_OPT_OUTS");

/// Mentions the scanner would presume to be reads that are not — each named, file and
/// name, with the reason. A new one is a decision made HERE, and one no longer found
/// fails as stale.
const MENTIONS: &[(&str, &str, &str)] = &[
    (
        "crates/aterm-gui/src/app_update_screen.rs",
        "ATERM_DEBUG_RELAUNCH_NUDGE",
        "the apply posture's LABEL for the seam; the read is `dev_seam!` in lib.rs",
    ),
    (
        "crates/aterm-gui/src/clipboard_x11.rs",
        "ATERM_CLIPBOARD_RECV",
        "an X11 atom name, not an environment variable",
    ),
];

/// Where a name at `at` sits, as far as reading it goes.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Context {
    /// Read, by a reader call or by presumption.
    Read(ReadKind),
    /// Set or cleared for a child, listed in a [`WRITE_TABLES`] table, or imported.
    NotRead,
    /// The value of `const NAME: &str = "…"` (its uses are scanned instead).
    Declaration,
    /// An element of the named `const`/`static` table (for [`RETIRED_DETECTOR`]).
    InTable(String),
}

/// Classify the name at `at` (a literal's opening quote, or a const identifier) by what
/// encloses it in `code` (comments and strings masked).
///
/// PRESUMED READ. The first version of this gate counted a name only as the DIRECT first
/// argument of a reader it recognised, and a review (2026-09-23) passed six reads of
/// retired knobs through it: `aterm_log::env::read("…")`, `var_os(OsStr::new("…"))`,
/// `for n in ["…"] { var_os(n) }`, a helper `knob("…")`, `vars().any(|(k, _)| k == "…")`
/// and `var_os(&*QUIET)`. Now every mention is a read UNLESS it is provably not one:
/// inside a writer's arguments, a `use`, a `const NAME: &str` declaration, a
/// [`WRITE_TABLES`] table, or a listed [`MENTIONS`] entry.
fn classify(code: &[u8], at: usize) -> Context {
    // Walk out through the unmatched openers around `at`, innermost first, to the
    // statement's edge: an unmatched `{`, or a `;`/`}` at depth 0.
    let mut depth = 0usize;
    let mut outermost = at;
    let mut edge = 0usize;
    let mut i = at;
    while i > 0 {
        i -= 1;
        match code[i] {
            b')' | b']' => depth += 1,
            b'}' if depth > 0 => depth += 1,
            b'}' | b';' if depth == 0 => {
                edge = i + 1;
                break;
            }
            b'{' if depth == 0 => {
                edge = i + 1;
                break;
            }
            b'(' | b'[' | b'{' if depth > 0 => depth -= 1,
            b'(' => {
                outermost = i;
                match call_role(code, i) {
                    CallRole::Read(kind) => return Context::Read(kind),
                    CallRole::Write => return Context::NotRead,
                    CallRole::Neither => {}
                }
            }
            b'[' => outermost = i,
            _ => {}
        }
    }
    // The statement's head, attributes and visibility stripped.
    let head = String::from_utf8_lossy(&code[edge..outermost]).into_owned();
    let mut head = head.trim();
    while head.starts_with("#[") {
        let Some(close) = head.find(']') else { break };
        head = head[close + 1..].trim_start();
    }
    let head = head
        .trim_start_matches("pub(crate)")
        .trim_start_matches("pub")
        .trim_start();
    if head.starts_with("use ") {
        return Context::NotRead;
    }
    for kind in ["const ", "static "] {
        if let Some(rest) = head.strip_prefix(kind) {
            let rest = rest.trim_start().trim_start_matches("mut ").trim_start();
            let name: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            if outermost == at && rest.contains("&str") {
                return Context::Declaration;
            }
            if WRITE_TABLES.contains(&name.as_str()) {
                return Context::NotRead;
            }
            return Context::InTable(name);
        }
    }
    Context::Read(ReadKind::Runtime)
}

#[derive(Default)]
struct Scan {
    /// name → (kind, "file:line") for every read found.
    reads: BTreeMap<String, BTreeSet<(ReadKind, String)>>,
    /// (file, name) of every [`MENTIONS`] entry found (and so not counted as a read).
    mentions: BTreeSet<(String, String)>,
    /// (file, name) of every retired name found in the [`RETIRED_DETECTOR`] table.
    detected: BTreeSet<(String, String)>,
    /// "file:line: NAME" of a retired name inside a string's CONTENT (a message or an
    /// embedded script) or a non-Rust script file.
    retired_in_text: Vec<String>,
}

/// Every `const NAME: &str = "VALUE";` in `src` whose value is a family name.
fn family_consts(src: &str, into: &mut BTreeMap<String, BTreeSet<String>>) {
    for line in src.lines() {
        let line = line.trim();
        let Some(rest) = line
            .strip_prefix("pub(crate) const ")
            .or_else(|| line.strip_prefix("pub const "))
            .or_else(|| line.strip_prefix("const "))
        else {
            continue;
        };
        let Some((name, value)) = rest.split_once(':') else {
            continue;
        };
        // The value must BE the literal (`= "NAME";`): `const BUILD: &str =
        // env!("ATERM_BUILD_NUMBER");` holds the stamp's VALUE, not the name.
        let Some((_, init)) = value.split_once('=') else {
            continue;
        };
        let Some(value) = init
            .trim_start()
            .strip_prefix('"')
            .and_then(|v| v.split('"').next())
        else {
            continue;
        };
        if is_family(value) && rest.contains("str") {
            into.entry(name.trim().to_string())
                .or_default()
                .insert(value.to_string());
        }
    }
}

/// Whether `name` appears in `text` as a whole word (not part of a longer name).
fn names_word(text: &str, name: &str) -> bool {
    let bytes = text.as_bytes();
    let mut from = 0;
    while let Some(p) = text[from..].find(name) {
        let a = from + p;
        let b = a + name.len();
        let before_ok = a == 0 || !is_ident_byte(bytes[a - 1]);
        let after_ok = b >= bytes.len() || !is_ident_byte(bytes[b]);
        if before_ok && after_ok {
            return true;
        }
        from = a + 1;
    }
    false
}

fn scan_file(
    root: &Path,
    file: &Path,
    consts: &BTreeMap<String, BTreeSet<String>>,
    test_files: &BTreeSet<PathBuf>,
    scan: &mut Scan,
) {
    if test_files.contains(file) {
        return;
    }
    let Ok(src) = std::fs::read_to_string(file) else {
        return;
    };
    let lexed = lex(&src);
    let (ranges, _) = test_regions(&src, &lexed.code, file);
    let in_test = |at: usize| ranges.iter().any(|&(a, b)| a <= at && at <= b);
    let rel = file
        .strip_prefix(root)
        .unwrap_or(file)
        .display()
        .to_string();
    let shown = |at: usize| format!("{rel}:{}", src[..at].matches('\n').count() + 1);
    let record = |scan: &mut Scan, name: &str, at: usize| {
        let context = classify(&lexed.code, at);
        if MENTIONS.iter().any(|(f, n, _)| *n == name && rel == *f)
            && context == Context::Read(ReadKind::Runtime)
        {
            scan.mentions.insert((rel.clone(), name.to_string()));
            return;
        }
        let kind = match context {
            Context::Read(kind) => kind,
            Context::NotRead | Context::Declaration => return,
            Context::InTable(table) => {
                if (rel.as_str(), table.as_str()) == RETIRED_DETECTOR && RETIRED.contains(&name) {
                    scan.detected.insert((rel.clone(), name.to_string()));
                    return;
                }
                ReadKind::Runtime
            }
        };
        scan.reads
            .entry(name.to_string())
            .or_default()
            .insert((kind, shown(at)));
    };
    // Literals: a string whose whole content is a family name.
    for (start, _end, content) in &lexed.strings {
        if in_test(*start) {
            continue;
        }
        if is_family(content) {
            // The literal's opening quote (or `b` of a byte string) is before `start`.
            record(scan, content, start.saturating_sub(1));
        } else {
            for name in RETIRED {
                if names_word(content, name) {
                    scan.retired_in_text
                        .push(format!("{}: {name}", shown(*start)));
                }
            }
        }
    }
    // Consts: every use of a `const` holding a family name, by identifier.
    let code = &lexed.code;
    let mut i = 0;
    while i < code.len() {
        if !is_ident_byte(code[i]) || (i > 0 && is_ident_byte(code[i - 1])) {
            i += 1;
            continue;
        }
        let mut j = i;
        while j < code.len() && is_ident_byte(code[j]) {
            j += 1;
        }
        let ident = String::from_utf8_lossy(&code[i..j]);
        if let Some(values) = consts.get(ident.as_ref())
            && !in_test(i)
        {
            let declared_here = String::from_utf8_lossy(&code[i.saturating_sub(16)..i])
                .trim_end()
                .ends_with("const");
            if !declared_here {
                for value in values {
                    record(scan, value, i);
                }
            }
        }
        i = j;
    }
}

fn scan_crates(root: &Path, crates: &BTreeSet<PathBuf>) -> Scan {
    let mut files = Vec::new();
    for dir in crates {
        rust_sources(&dir.join("src"), &mut files);
    }
    let consts = release_consts(root);
    let mut test_files = BTreeSet::new();
    for file in &files {
        if let Ok(src) = std::fs::read_to_string(file) {
            let lexed = lex(&src);
            let (_, mods) = test_regions(&src, &lexed.code, file);
            for m in mods {
                if let Ok(canon) = m.canonicalize() {
                    test_files.insert(canon);
                }
            }
        }
    }
    let mut scan = Scan::default();
    for file in &files {
        let canon = file.canonicalize().unwrap_or_else(|_| file.clone());
        scan_file(root, &canon, &consts, &test_files, &mut scan);
    }
    // EMBEDDED SCRIPTS: the shell integration and anything else a crate ships as a
    // script file under `src/` read the environment in their own language, so a
    // retired name in one is a knob come back — named, whatever the syntax.
    for dir in crates {
        let mut scripts = Vec::new();
        script_sources(&dir.join("src"), &mut scripts);
        for script in scripts {
            let Ok(text) = std::fs::read_to_string(&script) else {
                continue;
            };
            for (n, line) in text.lines().enumerate() {
                for name in RETIRED {
                    if names_word(line, name) {
                        scan.retired_in_text.push(format!(
                            "{}:{}: {name}",
                            script.strip_prefix(root).unwrap_or(&script).display(),
                            n + 1
                        ));
                    }
                }
            }
        }
    }
    scan
}

/// Every family-valued `const` in the RELEASE code of every first-party crate
/// (`crates/*/src`, test modules and `#[cfg(test)]` items blanked): a crate may read a
/// const another defines. Test code is left out because its short fixture names
/// (`SCALE`, `ROOT`, …) would otherwise make every same-named identifier in shipped
/// code a presumed read.
fn release_consts(root: &Path) -> BTreeMap<String, BTreeSet<String>> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(root.join("crates"))
        .expect("crates/")
        .flatten()
    {
        rust_sources(&entry.path().join("src"), &mut files);
    }
    let mut test_files = BTreeSet::new();
    let mut texts = Vec::new();
    for file in &files {
        let Ok(src) = std::fs::read_to_string(file) else {
            continue;
        };
        let lexed = lex(&src);
        let (ranges, mods) = test_regions(&src, &lexed.code, file);
        test_files.extend(mods.into_iter().filter_map(|m| m.canonicalize().ok()));
        let mut release = src.into_bytes();
        let len = release.len();
        for (a, b) in ranges {
            for byte in &mut release[a.min(len)..b.min(len)] {
                if *byte != b'\n' {
                    *byte = b' ';
                }
            }
        }
        texts.push((
            file.canonicalize().unwrap_or_else(|_| file.clone()),
            release,
        ));
    }
    let mut consts = BTreeMap::new();
    for (file, text) in texts {
        if !test_files.contains(&file) {
            family_consts(&String::from_utf8_lossy(&text), &mut consts);
        }
    }
    consts
}

/// The non-Rust script files under `dir` (shell, fish, PowerShell), test data excluded.
fn script_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path
                .file_name()
                .is_some_and(|n| n != "testdata" && n != "fixtures")
            {
                script_sources(&path, out);
            }
        } else if path
            .extension()
            .is_some_and(|e| ["sh", "bash", "zsh", "fish", "ps1"].iter().any(|x| e == *x))
        {
            out.push(path);
        }
    }
}

// ---------------------------------------------------------------------------
// The gate.
// ---------------------------------------------------------------------------

#[test]
fn the_allow_lists_are_disjoint_family_names_and_retire_nothing_twice() {
    let mut seen = BTreeSet::new();
    for (list, names) in [
        ("INTERNAL_PROTOCOL", INTERNAL_PROTOCOL),
        ("BUILD_STAMPS", BUILD_STAMPS),
        ("DEV_SEAMS", DEV_SEAMS),
        ("OUT_OF_SCOPE", OUT_OF_SCOPE),
    ] {
        for name in names {
            assert!(
                is_family(name),
                "{list}: {name} is not an ATERM_/ATPKG_ name"
            );
            assert!(seen.insert(*name), "{name} is on two allow-lists");
            assert!(
                !RETIRED.contains(name),
                "{name} was deleted (2026-09-23) and may not come back on {list}"
            );
        }
    }
}

#[test]
fn shipped_code_reads_no_environment_name_outside_the_allow_list() {
    let root = workspace_root();
    let crates = shipped_crates(&root);
    // Non-vacuity: the closure is the real one.
    for must in [
        "crates/aterm",
        "crates/aterm-gui",
        "crates/atpkg",
        "crates/aterm-update",
    ] {
        assert!(
            crates.contains(&root.join(must).canonicalize().unwrap()),
            "{must} is in the shipped closure: {crates:?}"
        );
    }
    let scan = scan_crates(&root, &crates);
    let total: usize = scan.reads.values().map(BTreeSet::len).sum();
    assert!(
        total > 150,
        "found only {total} environment reads — the scanner no longer matches how the \
         tree spells one"
    );
    // Non-vacuity: known reads of every kind are found.
    let has = |name: &str, kind: ReadKind| {
        scan.reads
            .get(name)
            .is_some_and(|reads| reads.iter().any(|(k, _)| *k == kind))
    };
    assert!(has("ATERM_HEADLESS", ReadKind::Runtime));
    assert!(
        has("__ATERM_REROUTE_PASSTHROUGH", ReadKind::Runtime),
        "a const read"
    );
    assert!(
        has("ATERM_UPDATE_EXPECTED_BUILD", ReadKind::Runtime),
        "an env::take read"
    );
    assert!(has("ATPKG_REGISTRY", ReadKind::Seam));
    assert!(has("ATERM_UPDATE_ROOT", ReadKind::Seam));
    assert!(has("ATERM_DEFAULT_OWNER", ReadKind::Build));

    let mut violations = Vec::new();
    for (name, reads) in &scan.reads {
        for (kind, at) in reads {
            let ok = match kind {
                ReadKind::Seam => DEV_SEAMS.contains(&name.as_str()),
                ReadKind::Build => BUILD_STAMPS.contains(&name.as_str()),
                ReadKind::Runtime => {
                    INTERNAL_PROTOCOL.contains(&name.as_str())
                        || OUT_OF_SCOPE.contains(&name.as_str())
                }
            };
            if !ok {
                let why = if RETIRED.contains(&name.as_str()) {
                    "a RETIRED knob (2026-09-23) is read again"
                } else if DEV_SEAMS.contains(&name.as_str()) {
                    "a development seam is read outside `aterm_types::dev_seam!` — that read ships"
                } else {
                    "not on an allow-list: decide here whether it is internal protocol, a \
                     build stamp, a dev seam (then read it through `dev_seam!`) or out of \
                     scope"
                };
                violations.push(format!("{at}: {kind:?} read of ${name} — {why}"));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "shipped code reads an environment name the owner's rule does not admit \
         (\"NOT ENV VARS those are for development\"):\n{}",
        violations.join("\n")
    );

    // No stale allow-list entry: every name listed is still read somewhere, in the
    // way its list says, so the lists describe the tree and not its history.
    let mut stale = Vec::new();
    for (names, kind) in [(DEV_SEAMS, ReadKind::Seam), (BUILD_STAMPS, ReadKind::Build)] {
        for name in names {
            if !has(name, kind) {
                stale.push(format!("{name} ({kind:?})"));
            }
        }
    }
    for name in INTERNAL_PROTOCOL.iter().chain(OUT_OF_SCOPE) {
        if !has(name, ReadKind::Runtime) {
            stale.push(format!("{name} (Runtime)"));
        }
    }
    assert!(
        stale.is_empty(),
        "allow-listed but read nowhere in shipped code — delete the entry: {stale:?}"
    );

    // Every MENTIONS entry still names a mention, so the exemption list describes the
    // tree and not its history.
    let unmentioned: Vec<(&str, &str)> = MENTIONS
        .iter()
        .filter(|(file, name, _)| {
            !scan
                .mentions
                .contains(&((*file).to_string(), (*name).to_string()))
        })
        .map(|(file, name, _)| (*file, *name))
        .collect();
    assert!(
        unmentioned.is_empty(),
        "a MENTIONS entry names nothing in shipped code any more — delete it: {unmentioned:?}"
    );
    // The doctor's retired-opt-out detector is found where it is allowed, and only
    // there (non-vacuity for `RETIRED_DETECTOR`).
    assert!(
        scan.detected
            .iter()
            .any(|(file, name)| file == RETIRED_DETECTOR.0 && name == "ATERM_NO_AUTO_UPDATE"),
        "the doctor's detector table is where `RETIRED_DETECTOR` says: {:?}",
        scan.detected
    );
    // No retired name inside a string's CONTENT (a message, an embedded script body) or
    // a shipped script file: the shell integration and the reroute stub read the
    // environment in shell, where no Rust call is there to see.
    assert!(
        scan.retired_in_text.is_empty(),
        "a retired knob is named in shipped text — a script would read it, a message \
         would teach it:\n{}",
        scan.retired_in_text.join("\n")
    );
}

/// The retired knobs are read by NO first-party source at all — shipped or not, a
/// publisher tool included — so none can come back through a crate outside the
/// closure either.
#[test]
fn no_first_party_source_reads_a_retired_knob() {
    let root = workspace_root();
    let mut crates = BTreeSet::new();
    for entry in std::fs::read_dir(root.join("crates"))
        .expect("crates/")
        .flatten()
    {
        // `aterm-census` is a linter whose sources embed COPIES of shipped readers as
        // synthetic fixtures; those are inputs to its gate, not readers of anything.
        if entry.file_name() != "aterm-census" && entry.path().join("src").is_dir() {
            crates.insert(entry.path().canonicalize().unwrap());
        }
    }
    let scan = scan_crates(&root, &crates);
    let revived: Vec<String> = RETIRED
        .iter()
        .filter_map(|name| {
            scan.reads.get(*name).map(|reads| {
                format!(
                    "${name}: {}",
                    reads
                        .iter()
                        .map(|(_, at)| at.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })
        })
        .collect();
    assert!(
        revived.is_empty(),
        "a knob deleted on 2026-09-23 is read again:\n{}",
        revived.join("\n")
    );
}

/// The scanner's own contract, pinned on a fixture. A name in a comment, in a writer
/// (`.env(`, `env::set`), in a `use`, as a `const NAME: &str` declaration, in a
/// [`WRITE_TABLES`] table, or under `#[cfg(test)]` is not a read; EVERY OTHER mention is
/// — a reader call's argument at any depth, a const's use, and anything the scanner
/// cannot prove is not one.
///
/// The seven shapes a review (2026-09-23) passed through the first version, which knew a
/// read only as a recognised reader's direct first argument, are all here: the
/// workspace's `aterm_log::env::read`, a `for` loop over names, `OsStr::new` inside the
/// reader, a helper with no `env` in its name, a `vars()` comparison, a `&*` const, and a
/// plain const. Measured before the fix: only the plain const was found.
#[test]
fn the_scanner_reads_calls_not_mentions() {
    let dir = std::env::temp_dir().join(format!("aterm-env-reads-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    let file = dir.join("src/lib.rs");
    std::fs::write(
        &file,
        r##"
// std::env::var("ATERM_IN_A_COMMENT")
/* var_os("ATERM_IN_A_BLOCK") */
use crate::knobs::QUIET_BY_DEREF;
const KNOB: &str = "ATERM_BY_CONST";
const QUIET_BY_DEREF: &str = "ATERM_BY_DEREF_CONST";
pub const ENV_DENY_VARS: &[&str] = &["ATERM_IN_THE_DENY_TABLE"];
fn f(cmd: &mut std::process::Command) {
    cmd.env("ATERM_A_BUILDER_SET", "1").env_remove("ATERM_A_BUILDER_REMOVE");
    aterm_log::env::set("ATERM_A_SET", "1");
    aterm_log::env::unset("ATERM_AN_UNSET");
    let _ = std::env::var("ATERM_LITERAL_READ");
    let _ = std::env::var_os(KNOB);
    let _ = aterm_types::dev_seam!("ATERM_A_SEAM");
    let _ = env!("ATERM_A_STAMP");
    let _ = flag_env("ATERM_A_HELPER");
    let _ = r#"var("ATERM_IN_A_RAW_STRING")"#;
    let _ = '"';
    let _ = std::env::var("ATERM_AFTER_A_QUOTE_CHAR");
    // The seven shapes.
    let _ = aterm_log::env::read("ATERM_LOG_ENV_READ");
    for n in ["ATERM_IN_A_LOOP"] { let _ = std::env::var_os(n); }
    let _ = std::env::var_os(std::ffi::OsStr::new("ATERM_IN_OS_STR_NEW"));
    let _ = knob("ATERM_IN_A_HELPER_WITHOUT_ENV");
    let _ = std::env::vars().any(|(k, _)| k == "ATERM_COMPARED_IN_VARS");
    let _ = std::env::var_os(&*QUIET_BY_DEREF);
    let _ = "ATERM_JUST_A_STRING";
}
#[cfg(test)]
mod tests {
    fn t() { let _ = std::env::var("ATERM_IN_A_TEST"); }
}
"##,
    )
    .unwrap();
    let mut consts = BTreeMap::new();
    family_consts(&std::fs::read_to_string(&file).unwrap(), &mut consts);
    let mut scan = Scan::default();
    scan_file(&dir, &file, &consts, &BTreeSet::new(), &mut scan);
    let found: BTreeMap<&str, ReadKind> = scan
        .reads
        .iter()
        .map(|(name, reads)| (name.as_str(), reads.iter().next().unwrap().0))
        .collect();
    assert_eq!(
        found,
        BTreeMap::from([
            ("ATERM_AFTER_A_QUOTE_CHAR", ReadKind::Runtime),
            ("ATERM_A_HELPER", ReadKind::Runtime),
            ("ATERM_A_SEAM", ReadKind::Seam),
            ("ATERM_A_STAMP", ReadKind::Build),
            ("ATERM_BY_CONST", ReadKind::Runtime),
            ("ATERM_BY_DEREF_CONST", ReadKind::Runtime),
            ("ATERM_COMPARED_IN_VARS", ReadKind::Runtime),
            ("ATERM_IN_A_HELPER_WITHOUT_ENV", ReadKind::Runtime),
            ("ATERM_IN_A_LOOP", ReadKind::Runtime),
            ("ATERM_IN_OS_STR_NEW", ReadKind::Runtime),
            ("ATERM_JUST_A_STRING", ReadKind::Runtime),
            ("ATERM_LITERAL_READ", ReadKind::Runtime),
            ("ATERM_LOG_ENV_READ", ReadKind::Runtime),
        ])
    );
    // A retired name in a string's CONTENT (an embedded script) is caught as text.
    let script = dir.join("src/script.rs");
    std::fs::write(
        &script,
        "const BODY: &str = \"case ${ATERM_NO_REROUTE:-} in 1) exit;; esac\";\n",
    )
    .unwrap();
    let mut scan = Scan::default();
    scan_file(&dir, &script, &consts, &BTreeSet::new(), &mut scan);
    assert_eq!(
        scan.retired_in_text,
        ["src/script.rs:1: ATERM_NO_REROUTE".to_string()]
    );
    let _ = std::fs::remove_dir_all(&dir);
}
