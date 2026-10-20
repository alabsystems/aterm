// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The door guards: an authority the tree reaches through exactly one door,
//! where no type, visibility or clippy.toml row can say so (a public fn's
//! callers, a local `extern` declaration, a literal's shape).

use std::collections::BTreeSet;

use super::graph::attribute;
use super::rules::{CENSUS_CRATE, PRODUCT, test_path};
use super::{Bound, Finding, Line, Rule, SourceFile, Tree, calls, has, hits, is_word};

/// Product source this crate does not own: the census spells every name its
/// rules ban, in its tables and its planted cases.
fn product<'a>(tree: &Tree<'a>) -> impl Iterator<Item = &'a SourceFile> {
    tree.corpus
        .under(PRODUCT)
        .filter(|f| !f.rel.starts_with(CENSUS_CRATE))
}

// ---------------------------------------------------------------------------
// devtools-door: docs/DESIGN-macos-debugger-access-2026-09-29.md §5.5.
// ---------------------------------------------------------------------------

/// Where the change runner, the tool's path and its argv builders are defined.
const DEVTOOLS_HOME: &str = "crates/aterm-containment/src/consent.rs";
/// The file of the one function that may run a change.
const DEVTOOLS_FLOW: &str = "crates/aterm-gui/src/debugger_access.rs";
/// That function: the live arm that runs a change for the owner's yes in aterm's
/// alert. Both ways the yes is given reach it through the App's one start
/// (`App::run_debugger_change`): the owner's press in Settings ▸ Security, and the
/// alert aterm raises once per Mac at first launch (owner ruling O29).
const DEVTOOLS_WORKER: &str = "spawn_devtools_worker";
/// The runner: public, so aterm-gui can reach it.
const DEVTOOLS_RUNNER: &str = "run_devtools_change";
const DEVTOOLS_PATH: &str = "/usr/sbin/DevToolsSecurity";
/// The constant consent.rs defines the path as.
const DEVTOOLS_PATH_CONST: &str = "DEVTOOLSSECURITY_PATH";
/// The flags a printed or probed path may carry: neither changes anything.
const DEVTOOLS_QUIET_FLAGS: &[&str] = &["-status", "-disable"];
/// What names the tool where a process is built: the tool, its path constant,
/// the revert line (copy for a human, never spawned), an argv builder
/// (`devtools_<x>_command`) and the change type.
const DEVTOOLS_TOOL_WORDS: &[&str] = &[
    "DEVTOOLSSECURITY_PATH",
    "DEBUGGER_ACCESS_REVERT",
    "DebuggerChange",
];
/// What builds or runs a process on a line: a `Command`, the spawn door, an
/// argv, `posix_spawn`, and consent.rs's own runners.
const SPAWN_CALLS: &[&str] = &[
    "arg",
    "args",
    "posix_spawn",
    "posix_spawnp",
    "execv",
    "execve",
    "execvp",
    "execl",
    "execle",
    "execlp",
    "run_bounded",
    "run_bounded_exit",
    "run_to_exit_keeping_heads",
];
/// The authorization-database and privileged-helper APIs, by name.
const AUTHZ_NAMES: &[&str] = &[
    "AuthorizationRightSet",
    "AuthorizationCopyRights",
    "SMJobBless",
    "daemonServiceWithPlistName",
];
/// The requests for administrator rights a literal carries.
const AUTHZ_LITERALS: &[&str] = &["authorizationdb write", "with administrator privileges"];

pub(super) const DEVTOOLS_DOOR: Rule = Rule {
    id: "devtools-door",
    why: "DevToolsSecurity changes the debug right for the whole Mac and ENABLES when run with no \
          flag: no shipped line builds a process naming it, its path constant and argv live in \
          consent.rs's builders alone, only the owner's yes in aterm's alert — their press, or \
          the once-per-Mac launch ask — runs `run_devtools_change`, through aterm-gui's one \
          worker `spawn_devtools_worker`, no shipped line holds the bare tool path, and aterm \
          calls no authorization-database or privileged-helper API",
    scope: &[DEVTOOLS_HOME, DEVTOOLS_FLOW],
    check: devtools_door,
};

fn devtools_door(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    let mut needles = vec![DEVTOOLS_RUNNER, "DevToolsSecurity", "devtools_"];
    needles.extend(DEVTOOLS_TOOL_WORDS);
    needles.extend(AUTHZ_NAMES);
    needles.extend(AUTHZ_LITERALS);
    let definition = format!("fn {DEVTOOLS_RUNNER}");
    for file in product(tree) {
        if test_path(&file.rel) || !file.mentions(&needles) {
            continue;
        }
        let home = file.rel == DEVTOOLS_HOME;
        let shipped: BTreeSet<usize> = file.shipped_lines().map(|l| l.no).collect();
        for at in attribute(file) {
            let line = at.line;
            if at.in_test || !shipped.contains(&line.no) {
                continue;
            }
            let func = at.func.unwrap_or("-");
            let worker = file.rel == DEVTOOLS_FLOW && func == DEVTOOLS_WORKER;
            let why = if let Some(name) =
                AUTHZ_NAMES.iter().find(|n| has(line.code, n, Bound::Word))
            {
                Some(format!("names `{name}`"))
            } else if let Some(lit) = AUTHZ_LITERALS.iter().find(|l| line.text.contains(*l)) {
                Some(format!("asks for `{lit}`"))
            } else if builds_a_process(line.code) && names_the_tool(line.text) {
                Some("builds a process from DevToolsSecurity".to_string())
            } else if has(line.code, DEVTOOLS_RUNNER, Bound::Word)
                && !has(line.code, &definition, Bound::Word)
                && !worker
            {
                Some(format!(
                    "names `{DEVTOOLS_RUNNER}` in `{func}`, not the gesture worker"
                ))
            } else if has(line.code, DEVTOOLS_PATH_CONST, Bound::Word)
                && !(home && path_const_allowed(line, func))
            {
                Some(if home {
                    format!(
                        "`{DEVTOOLS_PATH_CONST}` in `{func}`, outside its definition and its argv \
                         builders"
                    )
                } else {
                    format!("`{DEVTOOLS_PATH_CONST}` outside the consent module")
                })
            } else {
                bare_path(
                    line.text,
                    home && has(line.code, DEVTOOLS_PATH_CONST, Bound::Word),
                )
            };
            if let Some(why) = why {
                out.push(file.finding(DEVTOOLS_DOOR.id, line.no, why));
            }
        }
    }
}

/// Does a line of code build or run a process?
fn builds_a_process(code: &str) -> bool {
    has(code, "Command::new", Bound::Sub)
        || calls(code).iter().any(|c| SPAWN_CALLS.contains(c))
        || code.contains("spawnfd::command")
}

/// Does a line's text name the tool (its name in a literal counts)?
fn names_the_tool(text: &str) -> bool {
    text.contains("DevToolsSecurity")
        || DEVTOOLS_TOOL_WORDS
            .iter()
            .any(|w| has(text, w, Bound::Word))
        || hits(text, "devtools_", Bound::Sub).any(|at| {
            (at == 0 || !is_word(text.as_bytes()[at - 1]))
                && text[at + "devtools_".len()..]
                    .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                    .next()
                    .is_some_and(|rest| {
                        rest.strip_suffix("_command").is_some_and(|x| {
                            !x.is_empty() && x.bytes().all(|b| b.is_ascii_lowercase())
                        })
                    })
        })
}

/// In consent.rs, may this line name the path constant? Its definition, the
/// probe's `[PATH, "-status"]`, and `[PATH, "-enable"]` / `[PATH, "-disable"]`
/// inside `devtools_enable_command` / `devtools_disable_command` alone.
fn path_const_allowed(line: Line<'_>, func: &str) -> bool {
    if has(
        line.code,
        &format!("const {DEVTOOLS_PATH_CONST}"),
        Bound::Word,
    ) {
        return true;
    }
    match path_argv_flag(line.text) {
        Some("status") => true,
        Some(flag @ ("enable" | "disable")) => func == format!("devtools_{flag}_command"),
        _ => false,
    }
}

/// The flag of a `[DEVTOOLSSECURITY_PATH, "-<flag>"]` argv on a line, if one.
fn path_argv_flag(text: &str) -> Option<&str> {
    hits(text, DEVTOOLS_PATH_CONST, Bound::Word).find_map(|at| {
        text[..at]
            .trim_end_matches([' ', '\t'])
            .ends_with('[')
            .then_some(())?;
        let rest = text[at + DEVTOOLS_PATH_CONST.len()..].trim_start_matches([' ', '\t']);
        let rest = rest.strip_prefix(',')?.trim_start_matches([' ', '\t']);
        let rest = rest.strip_prefix("\"-")?;
        let (flag, rest) = rest.split_once('"')?;
        rest.trim_start_matches([' ', '\t'])
            .starts_with(']')
            .then_some(flag)
    })
}

/// A full tool path with neither quiet flag after it — but the definition of
/// the path constant itself (`defines`).
fn bare_path(text: &str, defines: bool) -> Option<String> {
    hits(text, DEVTOOLS_PATH, Bound::Sub).find_map(|start| {
        let after = text[start + DEVTOOLS_PATH.len()..].trim_start_matches([' ', '\t']);
        (!defines && !DEVTOOLS_QUIET_FLAGS.iter().any(|f| after.starts_with(f))).then(|| {
            format!(
                "`{DEVTOOLS_PATH}` with no {} after it",
                DEVTOOLS_QUIET_FLAGS.join(" or ")
            )
        })
    })
}

// ---------------------------------------------------------------------------
// ffi-door: a platform call declared in one file, whose callers its privacy
// then bounds.
// ---------------------------------------------------------------------------

/// The foreign functions a local `extern` block may declare in one file only:
/// the name's prefix, that file (`None`: nowhere), and what the call opens.
const FFI_DOORS: &[(&str, Option<&str>, &str)] = &[
    (
        "MTLCreateSystemDefaultDevice",
        Some("crates/aterm-gpu/src/metal/ffi.rs"),
        "a WindowServer connection, and a switch to the discrete GPU",
    ),
    (
        "CreateProcess",
        Some("crates/aterm-pty/src/windows/ffi.rs"),
        "a Windows process, whose console only the ConPTY seam decides",
    ),
    (
        "ShellExecute",
        Some("crates/aterm-gui/src/app_mouse.rs"),
        "a Windows process through the shell, which only the URL opener starts",
    ),
    ("WinExec", None, "a Windows process with a console window"),
];

pub(super) const FFI_DOOR: Rule = Rule {
    id: "ffi-door",
    why: "a WindowServer or process-creation call no clippy path can name (a local `extern` \
          declaration) is declared in shipped code in its one file, behind the privacy that \
          keeps its callers to the one door that decides its console or its WindowServer \
          connection",
    scope: &[
        "crates/aterm-gpu/src/metal/ffi.rs",
        "crates/aterm-pty/src/windows/ffi.rs",
        "crates/aterm-gui/src/app_mouse.rs",
    ],
    check: ffi_door,
};

fn ffi_door(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    let needles: Vec<&str> = FFI_DOORS.iter().map(|(name, ..)| *name).collect();
    for file in product(tree) {
        if test_path(&file.rel) || !file.mentions(&needles) {
            continue;
        }
        for line in file.shipped_lines() {
            for name in declared(line.code) {
                for (prefix, home, opens) in FFI_DOORS {
                    if name.starts_with(prefix) && *home != Some(file.rel.as_str()) {
                        out.push(file.finding(
                            FFI_DOOR.id,
                            line.no,
                            format!(
                                "declares `{name}` ({opens}) outside {}",
                                home.unwrap_or("every file: it has no door")
                            ),
                        ));
                    }
                }
            }
        }
    }
}

/// Every name a line of code declares with `fn`.
fn declared(code: &str) -> impl Iterator<Item = &str> {
    hits(code, "fn", Bound::Word).filter_map(move |at| {
        let rest = &code[at + 2..];
        let name = rest.trim_start_matches([' ', '\t']);
        let end = name.bytes().take_while(|&b| is_word(b)).count();
        (name.len() < rest.len() && end > 0).then(|| &name[..end])
    })
}

// ---------------------------------------------------------------------------
// handoff-name: the live upgrade's handoff variables are authority, read and
// handed on only as `HandoffName`s (crates/aterm-gui/src/handoff_env.rs).
// ---------------------------------------------------------------------------

/// aterm-gui's source: the window crate's and its split-out libraries'
/// (`crates/aterm-gui-*`).
const HANDOFF_SCOPE: &[&str] = &["crates/aterm-gui/src/", "crates/aterm-gui-"];
/// The module that owns the names' environment.
const HANDOFF_HOME: &str = "crates/aterm-gui/src/handoff_env.rs";
const HANDOFF_PREFIXES: &[&str] = &["\"ATERM_SEAMLESS_", "\"ATERM_HANDOFF_"];
/// Where such a literal may stand: as a `HandoffName`, or a dev seam's name (a
/// timeout knob that vanishes from release builds, no authority).
const HANDOFF_DOORS: &[&str] = &["HandoffName::new(", "dev_seam!("];

pub(super) const HANDOFF_NAME: Rule = Rule {
    id: "handoff-name",
    why: "a live-upgrade handoff variable carries the successor's authority (its descriptors, \
          nonce, rendezvous); spelled as a bare string it can be read or handed on past \
          `HandoffName`, which pairs every name with its value",
    scope: &[HANDOFF_HOME],
    check: handoff_name,
};

fn handoff_name(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    let needles: Vec<&str> = HANDOFF_PREFIXES.iter().map(|p| &p[1..]).collect();
    for file in tree.corpus.under(HANDOFF_SCOPE) {
        if file.rel == HANDOFF_HOME || test_path(&file.rel) || !file.mentions(&needles) {
            continue;
        }
        for line in file.shipped_lines() {
            for prefix in HANDOFF_PREFIXES {
                for at in hits(line.text, prefix, Bound::Sub) {
                    let before = line.code[..at].trim_end_matches([' ', '\t']);
                    if !HANDOFF_DOORS.iter().any(|d| before.ends_with(d)) {
                        out.push(file.finding(
                            HANDOFF_NAME.id,
                            line.no,
                            format!("spells `{}…` outside `HandoffName::new`", &prefix[1..]),
                        ));
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// core-signal: the argument half of the clippy.toml `abort`/`raise` rows, and
// every script's.
// ---------------------------------------------------------------------------

/// The signals whose death makes macOS write a crash report (and, for an app
/// bundle, put a "quit unexpectedly" dialog on the owner's screen).
const CORE_SIGNALS: &[&str] = &[
    "ABRT", "IOT", "SEGV", "BUS", "ILL", "FPE", "QUIT", "TRAP", "SYS", "EMT", "XCPU", "XFSZ",
];
/// Their numbers macOS and Linux agree on.
const CORE_NUMBERS: &[&str] = &["3", "4", "5", "6", "7", "8", "11"];
/// The Rust calls that send one.
const SENDERS: &[&str] = &["kill", "killpg", "raise", "pthread_kill"];
/// A word every shape [`core_shape`] matches holds: a line without one is skipped.
const CORE_WORDS: &[&str] = &[
    "abort",
    "faulthandler",
    "string_at",
    "kill",
    "raise",
    "timeout",
    "\"-",
    "'-",
];

pub(super) const CORE_SIGNAL: Rule = Rule {
    id: "core-signal",
    why: "a process killed by a core-dump signal makes macOS write a crash report, and an app \
          bundle's (Homebrew's python3 is one) puts a \"quit unexpectedly\" dialog on the \
          owner's screen; a stand-in that must die dies by SIGKILL (kill -KILL, os.kill(pid, 9), \
          os._exit(n), aterm_uds::process::kill_self_now) or by TERM where a wrapper re-raises it",
    scope: &[],
    check: core_signal,
};

fn core_signal(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    for file in product(tree) {
        if !file.mentions(&["SIG"]) && !file.mentions(CORE_WORDS) {
            continue;
        }
        for line in file.lines() {
            let sends = calls(line.code).iter().any(|c| SENDERS.contains(c));
            let named = CORE_SIGNALS
                .iter()
                .find(|sig| sends && has(line.code, &format!("SIG{sig}"), Bound::Word));
            if let Some(why) = named
                .map(|sig| format!("sends SIG{sig}"))
                .or_else(|| core_shape(line.text))
            {
                out.push(file.finding(CORE_SIGNAL.id, line.no, why));
            }
        }
    }
    for script in &tree.corpus.scripts {
        if !CORE_WORDS.iter().any(|w| script.raw.contains(w)) {
            continue;
        }
        for (no, line) in script.lines() {
            if let Some(why) = core_shape(line) {
                out.push(script.finding(CORE_SIGNAL.id, no, why));
            }
        }
    }
}

/// The first shape on `line` that ends a process by a core-dump signal, named.
/// Read over a script's lines and a Rust file's text view (a script in a Rust
/// string counts): a Python or Node abort, a private faulthandler helper, a
/// NULL `ctypes.string_at`, a kill call naming a core signal (Python also by
/// number), `kill`/`pkill`/`killall` or `timeout -s` given one as a shell
/// word, and one as an argv string (`"-ABRT"`, or `"-s", "SEGV"`).
fn core_shape(line: &str) -> Option<String> {
    if !CORE_WORDS.iter().any(|w| line.contains(w)) {
        return None;
    }
    if aborts(line) {
        return Some("an abort() call".to_string());
    }
    if line.contains("faulthandler._")
        && hits(line, "faulthandler._", Bound::Sub).any(|at| {
            let rest = &line[at + "faulthandler._".len()..];
            let name = rest
                .bytes()
                .take_while(|b| b.is_ascii_lowercase() || *b == b'_')
                .count();
            name > 0 && rest[name..].starts_with('(')
        })
    {
        return Some("a private faulthandler crash helper".to_string());
    }
    if hits(line, "ctypes.string_at(", Bound::Sub).any(|at| {
        let rest = line[at + "ctypes.string_at(".len()..].trim_start();
        rest.strip_prefix('0')
            .is_some_and(|r| r.trim_start().starts_with([',', ')']))
    }) {
        return Some("a NULL ctypes.string_at".to_string());
    }
    if let Some(why) = signal_call(line) {
        return Some(why);
    }
    if let Some(why) = shell_kill(line) {
        return Some(why);
    }
    argv_signal(line)
}

/// `SIG` (optional) and a core signal at the start of `s` — or, with
/// `numbers`, one of [`CORE_NUMBERS`] — ending at a word edge: its name.
fn core_at(s: &str, numbers: bool) -> Option<&'static str> {
    let bare = s.strip_prefix("SIG").unwrap_or(s);
    let names = CORE_SIGNALS
        .iter()
        .chain(if numbers { CORE_NUMBERS } else { &[] });
    names.copied().find(|name| {
        bare.strip_prefix(name)
            .is_some_and(|rest| rest.bytes().next().is_none_or(|b| !is_word(b)))
    })
}

/// A Python or Node abort: `os.abort(`, `__import__("os").abort(`,
/// `CDLL(…).abort(`, `process.abort(`, `from os import … abort`.
fn aborts(line: &str) -> bool {
    let call = hits(line, ".abort(", Bound::Sub).any(|at| {
        let before = &line[..at];
        let word_ends = |w: &str| {
            before
                .strip_suffix(w)
                .is_some_and(|b| b.bytes().next_back().is_none_or(|c| !is_word(c)))
        };
        let cdll = before.rfind("CDLL(").is_some_and(|open| {
            let inner = &before[open + "CDLL(".len()..];
            inner.strip_suffix(')').is_some_and(|i| !i.contains(')'))
        });
        word_ends("os")
            || word_ends("process")
            || before.ends_with("\"os\")")
            || before.ends_with("'os')")
            || cdll
    });
    call || hits(line, "from", Bound::Word).any(|at| {
        let rest = &line[at + 4..];
        let Some(rest) = spaced(rest).and_then(|r| r.strip_prefix("os")) else {
            return false;
        };
        let Some(rest) = spaced(rest).and_then(|r| r.strip_prefix("import")) else {
            return false;
        };
        let names = rest.split('#').next().unwrap_or(rest);
        has(names, "abort", Bound::Word)
    })
}

/// `s` past one or more spaces or tabs, `None` when it does not start with one.
fn spaced(s: &str) -> Option<&str> {
    let rest = s.trim_start_matches([' ', '\t']);
    (rest.len() < s.len()).then_some(rest)
}

/// A kill call naming a core signal: `os.kill`, `os.killpg`,
/// `signal.raise_signal`, `signal.pthread_kill`, `process.kill` or
/// `libc::kill`/`killpg`/`raise` with `SIG<core>` among its arguments; Python's
/// with a core signal's number as its last.
fn signal_call(line: &str) -> Option<String> {
    const BY_NAME: &[&str] = &[
        "os.kill(",
        "os.killpg(",
        "signal.raise_signal(",
        "signal.pthread_kill(",
        "process.kill(",
        "libc::kill(",
        "libc::killpg(",
        "libc::raise(",
    ];
    const BY_NUMBER: &[&str] = &["os.kill(", "os.killpg(", "signal.pthread_kill("];
    for call in BY_NAME {
        for at in hits(line, call, Bound::Sub) {
            let args = arguments(&line[at + call.len()..]);
            for sig in hits(args, "SIG", Bound::Sub) {
                if let Some(name) = core_at(&args[sig..], false) {
                    return Some(format!("sends SIG{name}"));
                }
            }
            let last = if *call == "signal.raise_signal(" {
                Some(args)
            } else if BY_NUMBER.contains(call) {
                args.rsplit_once(',').map(|(_, last)| last)
            } else {
                None
            };
            if let Some(n) = last.map(str::trim)
                && CORE_NUMBERS.contains(&n)
                && line[at + call.len() + args.len()..].starts_with(')')
            {
                return Some(format!("sends signal {n}"));
            }
        }
    }
    None
}

/// A call's arguments: up to its closing parenthesis (one nested level), a
/// `#` comment or the line's end.
fn arguments(s: &str) -> &str {
    let mut depth = 0usize;
    for (i, b) in s.bytes().enumerate() {
        match b {
            b'#' => return &s[..i],
            b'(' => depth += 1,
            b')' if depth == 0 => return &s[..i],
            b')' => depth -= 1,
            _ => {}
        }
    }
    s
}

/// `kill`, `pkill` or `killall` as a shell word given a core signal
/// (`-ABRT`, `-SIGSEGV`, `-s QUIT`, `-n 6`, `-11`), or `timeout -s <core>` /
/// `--signal=<core>`.
fn shell_kill(line: &str) -> Option<String> {
    let b = line.as_bytes();
    for at in hits(line, "kill", Bound::Sub) {
        let start = if at > 0 && b[at - 1] == b'p' {
            at - 1
        } else {
            at
        };
        if start > 0 && (is_word(b[start - 1]) || matches!(b[start - 1], b'.' | b'-')) {
            continue;
        }
        let word_end = if line[at + 4..].starts_with("all") {
            at + 7
        } else {
            at + 4
        };
        let Some(rest) = spaced(&line[word_end..]).and_then(|r| r.strip_prefix('-')) else {
            continue;
        };
        let rest = ["s", "n"]
            .iter()
            .find_map(|f| rest.strip_prefix(f).and_then(spaced))
            .unwrap_or(rest);
        if let Some(name) = core_at(rest, true) {
            return Some(format!("{} -{name}", &line[start..word_end]));
        }
    }
    for at in hits(line, "timeout", Bound::Sub) {
        let Some(rest) = spaced(&line[at + "timeout".len()..]) else {
            continue;
        };
        let flag = rest
            .strip_prefix("--signal")
            .map(|r| r.strip_prefix('=').or_else(|| spaced(r)).unwrap_or(r))
            .or_else(|| {
                rest.strip_prefix("-s")
                    .map(|r| r.trim_start_matches([' ', '\t']))
            });
        if let Some(name) = flag.and_then(|r| core_at(r, false)) {
            return Some(format!("timeout -s {name}"));
        }
    }
    None
}

/// A core signal as an argv string: `"-ABRT"` (or `'-SIGSEGV'`), or a `"-s"` /
/// `"-n"` element followed by `"<core>"`.
fn argv_signal(line: &str) -> Option<String> {
    for quote in ['"', '\''] {
        let open = format!("{quote}-");
        for at in hits(line, &open, Bound::Sub) {
            let rest = &line[at + 2..];
            if let Some(name) = core_at(rest, false) {
                let after = rest.strip_prefix("SIG").unwrap_or(rest);
                if after[name.len()..].starts_with(quote) {
                    return Some(format!("hands a kill the argument {quote}-{name}{quote}"));
                }
            }
            let Some(rest) = ["s", "n"]
                .iter()
                .find_map(|f| rest.strip_prefix(f).and_then(|r| r.strip_prefix(quote)))
            else {
                continue;
            };
            let rest = rest.trim_start_matches([' ', '\t']);
            let Some(rest) = rest
                .strip_prefix(',')
                .map(|r| r.trim_start_matches([' ', '\t']))
            else {
                continue;
            };
            if let Some(rest) = rest.strip_prefix(quote)
                && let Some(name) = core_at(rest, false)
                && rest.strip_prefix("SIG").unwrap_or(rest)[name.len()..].starts_with(quote)
            {
                return Some(format!("hands a kill the arguments -s {name}"));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::super::tests::{run, spots};
    use super::*;

    #[test]
    fn only_the_gesture_worker_runs_a_devtools_change() {
        let flow = "fn spawn_devtools_worker(job: Job) {\n    \
                    consent::run_devtools_change(job.change, gate, f);\n}\n\
                    fn elsewhere() {\n    consent::run_devtools_change(c, g, f);\n}\n\
                    #[cfg(test)]\nmod tests {\n    fn t() { run_devtools_change(c, g, f); }\n}\n";
        let home = "pub fn run_devtools_change<F>(c: C) {}\n\
                    const DEVTOOLSSECURITY_PATH: &str = \"/usr/sbin/DevToolsSecurity\";\n\
                    pub const REVERT: &str = \"/usr/sbin/DevToolsSecurity -disable\";\n\
                    const fn devtools_status_command() -> [&'static str; 2] {\n    \
                    [DEVTOOLSSECURITY_PATH, \"-status\"]\n}\n\
                    const fn devtools_enable_command() -> [&'static str; 2] {\n    \
                    [DEVTOOLSSECURITY_PATH, \"-enable\"]\n}\n\
                    impl DebuggerChange {\n    fn command(self) -> [&'static str; 2] {\n        \
                    match self {\n            Self::On => devtools_enable_command(),\n        \
                    }\n    }\n}\n\
                    fn spawn_it(change: DebuggerChange) { run_to_exit_keeping_heads(&change.command(), |_| {}) }\n\
                    fn sneaky() {\n    let argv = [DEVTOOLSSECURITY_PATH, \"-enable\"];\n}\n\
                    fn probe() { let out = run_bounded_exit(program, &args, ceiling); }\n";
        let found = run(
            &DEVTOOLS_DOOR,
            &[
                (DEVTOOLS_FLOW, flow),
                (DEVTOOLS_HOME, home),
                (
                    "crates/aterm-cli/src/lib.rs",
                    "fn a() { run_devtools_change(c, g, f); }\n\
                     fn b() { let p = \"/usr/sbin/DevToolsSecurity\"; }\n\
                     fn c() { let s = \"/usr/sbin/DevToolsSecurity -status\"; }\n\
                     fn d() { AuthorizationCopyRights(a, r, e, f, o); }\n\
                     fn e() { let s = \"do shell script x with administrator privileges\"; }\n\
                     fn f() { aterm_uds::spawnfd::command(\"DevToolsSecurity\").arg(\"-enable\"); }\n\
                     fn g() { spawnfd::command(\"sh\").args([\"-c\", \"DevToolsSecurity -enable\"]); }\n\
                     fn h() { let argv = [DEVTOOLSSECURITY_PATH, \"-enable\"]; }\n\
                     fn i() { println!(\"run DevToolsSecurity -status to see\"); }\n",
                ),
                (
                    "crates/aterm-gui/tests/t.rs",
                    "fn t() { run_devtools_change(c, g, f); }\n",
                ),
            ],
        );
        assert_eq!(
            spots(&found),
            [
                "crates/aterm-cli/src/lib.rs:1",
                "crates/aterm-cli/src/lib.rs:2",
                "crates/aterm-cli/src/lib.rs:4",
                "crates/aterm-cli/src/lib.rs:5",
                "crates/aterm-cli/src/lib.rs:6",
                "crates/aterm-cli/src/lib.rs:7",
                "crates/aterm-cli/src/lib.rs:8",
                "crates/aterm-containment/src/consent.rs:17",
                "crates/aterm-containment/src/consent.rs:19",
                "crates/aterm-gui/src/debugger_access.rs:5",
            ]
        );
    }

    #[test]
    fn a_door_ffi_name_is_declared_in_its_one_file() {
        let decl = "unsafe extern \"system\" {\n    fn CreateProcessW(a: *const u16) -> i32;\n}\n";
        let found = run(
            &FFI_DOOR,
            &[
                ("crates/aterm-pty/src/windows/ffi.rs", decl),
                ("crates/aterm-cli/src/win.rs", decl),
                (
                    "crates/aterm-gui/src/app_mouse.rs",
                    "fn open_url_windows() {\n    unsafe extern \"system\" { fn ShellExecuteW(); }\n}\n",
                ),
                (
                    "crates/aterm-gpu/src/probe.rs",
                    "unsafe extern \"C\" { fn MTLCreateSystemDefaultDevice() -> Id; }\n\
                     fn create_process_verdict() {}\nfn x() { let f = WinExec; }\n",
                ),
                (
                    "crates/aterm-gui/src/launch.rs",
                    "extern \"system\" { fn WinExec(); }\n",
                ),
                ("crates/aterm-uds/tests/windows_spawn_console.rs", decl),
                (
                    "crates/aterm-cli/src/console.rs",
                    &format!("#[cfg(test)]\nmod tests {{\n{decl}}}\n"),
                ),
            ],
        );
        assert_eq!(
            spots(&found),
            [
                "crates/aterm-cli/src/win.rs:2",
                "crates/aterm-gpu/src/probe.rs:1",
                "crates/aterm-gui/src/launch.rs:1",
            ]
        );
    }

    #[test]
    fn a_handoff_name_is_spelled_only_as_a_handoff_name() {
        let src = "pub(crate) const ENV_FDS: HandoffName = HandoffName::new(\"ATERM_SEAMLESS_FDS\");\n\
                   const PARK: HandoffName =\n    HandoffName::new(\"ATERM_HANDOFF_PARK\");\n\
                   fn t() { let ms = dev_seam!(\"ATERM_HANDOFF_READY_TIMEOUT_MS\"); }\n\
                   fn r() { std::env::var_os(\"ATERM_SEAMLESS_NONCE\"); }\n\
                   fn s() { let k = \"ATERM_HANDOFF_CLAIM\".to_string(); }\n\
                   #[cfg(test)]\nmod tests {\n    const K: &str = \"ATERM_SEAMLESS_NONCE\";\n}\n";
        let found = run(
            &HANDOFF_NAME,
            &[
                ("crates/aterm-gui/src/seamless.rs", src),
                (HANDOFF_HOME, src),
                ("crates/aterm-gui/src/seamless_tests.rs", src),
                ("crates/aterm-cli/src/lib.rs", src),
            ],
        );
        assert_eq!(
            spots(&found),
            [
                "crates/aterm-gui/src/seamless.rs:5",
                "crates/aterm-gui/src/seamless.rs:6",
            ]
        );
    }

    #[test]
    fn a_kill_never_names_a_core_signal() {
        let rust = "fn a() { unsafe { libc::kill(pid, libc::SIGABRT) }; }\n\
                    fn b() { unsafe { libc::kill(pid, libc::SIGKILL) }; }\n\
                    fn c() { Command::new(\"kill\").args([\"-SEGV\", &pid]); }\n\
                    fn d() { Command::new(\"kill\").args([\"-KILL\", &pid]); }\n\
                    fn e() { unsafe { libc::raise(sig) }; }\n\
                    fn f() { let quit = libc::SIGQUIT; handle(quit); }\n\
                    fn g() { sh(&format!(\"kill -SEGV {pid}\")); }\n\
                    fn h() { Command::new(\"kill\").args([\"-s\", \"SEGV\", pid]); }\n\
                    fn i() { Command::new(\"timeout\").args([\"-s\", \"QUIT\", \"5\", \"x\"]); }\n\
                    fn j() { Command::new(\"head\").args([\"-n\", \"5\"]); }\n\
                    fn k() { sh(\"kill -9 $pid; timeout -s KILL 5 x\"); }\n";
        let shell = "#!/bin/sh\nkill -ABRT $$\n# kill -KILL $$ is the way\npkill -s QUIT aterm\n\
                     killall -11 x\ntimeout --signal=SEGV 5 x\ngtimeout -s TRAP 5 x\n\
                     kill -TERM $pid\nskill -ABRT x\n";
        let python = "import os, signal, ctypes, faulthandler\nos.abort()\n\
                      from os import getpid, abort\nctypes.CDLL(None).abort()\n\
                      faulthandler._sigsegv()\nctypes.string_at(0)\n\
                      os.kill(pid, signal.SIGSEGV)\nos.kill(pid, 6)\nsignal.raise_signal(11)\n\
                      os.kill(pid, 9)\nos._exit(1)\nsignal.pthread_kill(t, signal.SIGQUIT)\n";
        let node =
            "process.abort();\nprocess.kill(pid, 'SIGILL');\nprocess.kill(pid, 'SIGKILL');\n";
        let found = run(
            &CORE_SIGNAL,
            &[
                ("crates/aterm-gui/tests/stand_in.rs", rust),
                ("tools/stand-in.sh", shell),
                ("tools/probe.py", python),
                ("tools/x.mjs", node),
                (
                    "crates/aterm-census/src/guards/doors.rs",
                    "fn a() { libc::kill(pid, libc::SIGABRT); }\n",
                ),
            ],
        );
        let at = |path: &str, lines: &[usize]| -> Vec<String> {
            lines.iter().map(|n| format!("{path}:{n}")).collect()
        };
        let expected: Vec<String> = [
            at("crates/aterm-gui/tests/stand_in.rs", &[1, 3, 7, 8, 9]),
            at("tools/probe.py", &[2, 3, 4, 5, 6, 7, 8, 9, 12]),
            at("tools/stand-in.sh", &[2, 4, 5, 6, 7]),
            at("tools/x.mjs", &[1, 2]),
        ]
        .concat();
        assert_eq!(spots(&found), expected, "{found:#?}");
    }
}
