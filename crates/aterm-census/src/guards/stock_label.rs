// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! stock-label: code that runs on a stock Rust toolchain carries its label.
//!
//! AGENTS.md allows stock Rust only where Trust cannot do the job, labelled `STOCK EXCEPTION`
//! or `PUBLIC BOUNDARY`, and no lint names that rule. Three shapes are stock by construction:
//!
//! - a cargo-fuzz target (cargo-fuzz and its sanitizer runtime are stock nightly's);
//! - shipped code that selects a stock channel by a literal: rustup's `"+nightly"` override,
//!   `"--toolchain=stable"` or `"RUSTUP_TOOLCHAIN=stable"` as one argument; `"RUSTUP_TOOLCHAIN"`,
//!   rustup's `"--toolchain"` or its `"run"` followed by a channel (`"stable"`), in a list or
//!   in the next `.arg(..)` of a chain; a channel handed straight to a call whose path names a
//!   toolchain or rustup (`rustup::Toolchain::resolve(rustup, "stable")`); or a channel bound
//!   as the default of a name that names one (`let mut stock = "stable".to_string();`);
//! - a Cargo target that has no work on Trust's host. Trust ships its host's std alone
//!   ([`TRUST_HOST`]), so wherever Linux or Windows code is compiled, stock Rust compiles it:
//!   the Linux and Windows lanes' guests (`aterm-lab`'s `linux_guest.rs`, `windows_guest.rs`)
//!   and `verify cells`. Those lanes carry the label where they select the stock compiler; a
//!   target with no work on Trust's host carries its own, because nothing Trust builds would.
//!   It is a target root gated whole (`#![cfg(target_os = "linux")]`), or a binary, test,
//!   bench or example whose `fn main` only reports on Trust's host ([`reports_only`]). The
//!   target roots are the ones Cargo builds: what each package's manifest declares, and what
//!   Cargo discovers beside it ([`target_roots`]). A library module or a test-suite member
//!   gated to Linux is not one: the target it belongs to is one Trust builds.
//!
//! A remedy that names a stock command to a person runs nothing. Each file of these shapes
//! carries a label in a comment; a label only inside a string (an error message) labels no code.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

use super::rules::PRODUCT;
use super::{Finding, Rule, SourceFile, Tree, is_word, joined};

pub(super) const STOCK_LABEL: Rule = Rule {
    id: "stock-label",
    why: "code that runs on a stock Rust toolchain (a cargo-fuzz target, a stock channel picked \
          by a literal, a Cargo target that has no work on Trust's host) carries a `STOCK \
          EXCEPTION` or `PUBLIC BOUNDARY` comment saying why Trust cannot do the job (AGENTS.md)",
    scope: &[],
    check: stock_label,
};

/// What the one target Trust ships a std for (aarch64-apple-darwin, the host its releases
/// carry) answers to the `cfg` names that fix a target (`trustc --print cfg` there). A
/// predicate these make false holds only on a target Trust has no std for; `unix` and
/// `windows` are `target_family`'s.
const TRUST_HOST: &[(&str, &str)] = &[
    ("target_os", "macos"),
    ("target_family", "unix"),
    ("target_arch", "aarch64"),
    ("target_vendor", "apple"),
    ("target_env", ""),
    ("target_abi", ""),
    ("target_pointer_width", "64"),
    ("target_endian", "little"),
];

/// The labels AGENTS.md names.
const LABELS: &[&str] = &["STOCK EXCEPTION", "PUBLIC BOUNDARY"];

/// The stock channels rustup selects by name.
const CHANNELS: &[&str] = &["stable", "beta", "nightly"];

/// The literals whose next argument names the toolchain: rustup's environment variable, its
/// flag and its `run` subcommand.
const TOOLCHAIN_KEYS: &[&str] = &["\"RUSTUP_TOOLCHAIN\"", "\"--toolchain\"", "\"run\""];

/// What opens a literal that names the toolchain whole, a channel after it: rustup's `+`
/// override, and its flag and environment variable joined to their value by `=`.
const OVERRIDES: &[&str] = &["+", "--toolchain=", "RUSTUP_TOOLCHAIN="];

/// The calls that do no work: conversions and `exit`. A name that opens with a capital, a
/// tuple struct's or a variant's constructor, does none either.
const INERT_CALLS: &[&str] = &["into", "from", "to_string", "to_owned", "exit"];

/// The macros that do no work, each named by its last segment (`tracing::warn!` is `warn`):
/// these of std's, each of which reports, stops, or builds a value (not `include!`, which
/// splices code in, or `thread_local!`, which declares an item), and the five level reporters
/// (`error!` to `trace!`) that `log` and `tracing` define, and aterm's `aterm-log` and
/// `aterm-tracing` after them. A call in a macro's arguments is read in its turn ([`works`]),
/// so `assert!(serve())` does work.
const INERT_MACROS: &[&str] = &[
    "eprintln",
    "println",
    "eprint",
    "print",
    "write",
    "writeln",
    "format",
    "format_args",
    "dbg",
    "panic",
    "unreachable",
    "todo",
    "unimplemented",
    "assert",
    "assert_eq",
    "assert_ne",
    "debug_assert",
    "debug_assert_eq",
    "debug_assert_ne",
    "compile_error",
    "ready",
    "vec",
    "concat",
    "stringify",
    "include_str",
    "include_bytes",
    "env",
    "option_env",
    "cfg",
    "matches",
    "is_x86_feature_detected",
    "is_aarch64_feature_detected",
    "addr_of",
    "addr_of_mut",
    "offset_of",
    "pin",
    "file",
    "line",
    "column",
    "module_path",
    "error",
    "warn",
    "info",
    "debug",
    "trace",
];

/// The keywords a `(` can follow, which call nothing.
const KEYWORDS: &[&str] = &[
    "if", "while", "for", "match", "return", "let", "in", "as", "mut", "ref", "move", "else",
    "break",
];

/// The kind of a Cargo target: `lib`, `bin`, `test`, `bench` or `example`.
type Kind = &'static str;

fn stock_label(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    let roots = target_roots(tree, out);
    let test_code = tree.corpus.test_code();
    for file in tree.corpus.under(PRODUCT) {
        let found = if file.rel.contains("/fuzz/fuzz_targets/") {
            Some((0, "a cargo-fuzz target runs on stock nightly".to_string()))
        } else if let Some(gate) = roots
            .get(&file.rel)
            .and_then(|kinds| foreign_target(file, kinds))
        {
            Some(gate)
        } else if test_code.holds(&file.rel) || !file.mentions(CHANNELS) {
            None
        } else {
            selector(file).map(|(no, seen)| (no, format!("selects a stock toolchain: `{seen}`")))
        };
        if let Some((no, seen)) = found.filter(|_| !labelled(file)) {
            out.push(file.finding(
                STOCK_LABEL.id,
                no,
                format!("{seen}, and no comment labels it `STOCK EXCEPTION` or `PUBLIC BOUNDARY`"),
            ));
        }
    }
}

/// The root file of every Cargo target of a package the corpus holds a manifest for (each
/// under [`PRODUCT`]), with the kinds of target it roots: each one its manifest's target
/// tables name (by `path`, else where Cargo looks for that `name`), and each one Cargo
/// discovers beside the manifest (`src/lib.rs`, `src/main.rs`, `src/bin/`, `tests/`,
/// `benches/`, `examples/`) unless the manifest turns that kind's discovery off. So a member of
/// a test suite under `autotests = false` (declared by the suite's `mod x;`) is no root, and
/// neither is a file of a `tests/` directory inside `src/`. A manifest this cannot read is a
/// finding: the targets it declares would go unread.
fn target_roots(tree: &Tree<'_>, out: &mut Vec<Finding>) -> BTreeMap<String, BTreeSet<Kind>> {
    let mut roots: BTreeMap<String, BTreeSet<Kind>> = BTreeMap::new();
    let mut add = |rel: String, kind: Kind| {
        roots.entry(rel).or_default().insert(kind);
    };
    for (rel, text) in &tree.corpus.manifests {
        let targets = match crate::scan_set::manifest_targets(text, rel) {
            Ok(targets) if targets.package => targets,
            Ok(_) => continue,
            Err(why) => {
                out.push(Finding {
                    rule: STOCK_LABEL.id,
                    path: rel.clone(),
                    line: 0,
                    detail: format!("unreadable manifest, so its targets go unread: {why}"),
                });
                continue;
            }
        };
        let dir = rel.strip_suffix("/Cargo.toml").unwrap_or_default();
        for (kind, name, path) in &targets.declared {
            match path {
                Some(path) => add(joined(dir, path), kind),
                None => {
                    let own = name.is_some() && *name == targets.name;
                    for at in where_cargo_looks(kind, name.as_deref(), own) {
                        add(format!("{dir}/{at}"), kind);
                    }
                }
            }
        }
        let prefix = format!("{dir}/");
        let prefixes = [prefix.as_str()];
        for file in tree.corpus.under(&prefixes) {
            let mut it = file.rel[prefix.len()..].split('/');
            let kind = match (it.next(), it.next(), it.next(), it.next(), it.next()) {
                (Some("src"), Some("lib.rs"), None, None, None) => "lib",
                (Some("src"), Some("main.rs"), None, None, None)
                | (Some("src"), Some("bin"), Some(_), None, None)
                | (Some("src"), Some("bin"), Some(_), Some("main.rs"), None) => "bin",
                (Some("tests"), Some(_), None, None, None)
                | (Some("tests"), Some(_), Some("main.rs"), None, None) => "test",
                (Some("benches"), Some(_), None, None, None)
                | (Some("benches"), Some(_), Some("main.rs"), None, None) => "bench",
                (Some("examples"), Some(_), None, None, None)
                | (Some("examples"), Some(_), Some("main.rs"), None, None) => "example",
                _ => continue,
            };
            if !targets.undiscovered.contains(&kind) {
                add(file.rel.clone(), kind);
            }
        }
    }
    roots
}

/// Where Cargo looks, beside the manifest, for the root of a target of `kind` declared with
/// `name` and no `path`; `own` when that name is the package's, which a binary's
/// `src/main.rs` carries.
pub(super) fn where_cargo_looks(kind: &str, name: Option<&str>, own: bool) -> Vec<String> {
    let dir = match kind {
        "bin" => "src/bin",
        "test" => "tests",
        "bench" => "benches",
        "example" => "examples",
        _ => return vec!["src/lib.rs".to_string()],
    };
    let mut out: Vec<String> = name
        .into_iter()
        .flat_map(|name| [format!("{dir}/{name}.rs"), format!("{dir}/{name}/main.rs")])
        .collect();
    if kind == "bin" && own {
        out.push("src/main.rs".to_string());
    }
    out
}

/// The first shipped place that selects a stock channel, as its line and what selects it. The
/// views are read whole, so a call rustfmt wrapped reads as one.
fn selector(file: &SourceFile) -> Option<(usize, String)> {
    let code = file.shipped_code();
    let text = text_beside(file);
    // A literal opens at `at`: its quote is code, not a literal's blanked contents.
    let opens = |at: usize| code.as_bytes().get(at) == Some(&b'"');
    let mut seen: Vec<(usize, String)> = Vec::new();
    for prefix in OVERRIDES {
        for (at, _) in text.match_indices(&format!("\"{prefix}")) {
            if opens(at)
                && let Some(lit) = channel_literal(&text, at, prefix)
            {
                seen.push((at, lit.to_string()));
            }
        }
    }
    for key in TOOLCHAIN_KEYS {
        for (at, _) in text.match_indices(key) {
            let rest = text[at + key.len()..].trim_start();
            let listed = rest.strip_prefix(',');
            let chained = rest
                .strip_prefix(')')
                .and_then(|r| r.trim_start().strip_prefix(".arg("));
            let value = listed
                .or(chained)
                .map(|v| text.len() - v.trim_start().len());
            if opens(at)
                && let Some(lit) = value.and_then(|v| channel_literal(&text, v, ""))
            {
                seen.push(match listed {
                    Some(_) => (at, format!("{key}, {lit}")),
                    None => (at, format!(".arg({key}).arg({lit})")),
                });
            }
        }
    }
    for (at, _) in text.match_indices('"') {
        let Some(lit) = channel_literal(&text, at, "").filter(|_| opens(at)) else {
            continue;
        };
        if let Some(path) = enclosing_call(&code, at).filter(|path| names_toolchain(path)) {
            seen.push((at, format!("{path}(.., {lit})")));
        } else if let Some(name) = default_of(&code, at..at + lit.len()) {
            seen.push((at, format!("{name} {lit}")));
        }
    }
    let (at, what) = seen.into_iter().min_by_key(|(at, _)| *at)?;
    Some((code[..at].matches('\n').count() + 1, what))
}

/// The literal whose quote is at `at` in this TEXT view when it is a stock channel whole
/// (`"stable"`, `"nightly-2026-01-01"`; not `"stablex"` or `"runs stable"`), after `prefix`
/// (rustup's `"+nightly"` for `+`).
fn channel_literal<'a>(text: &'a str, at: usize, prefix: &str) -> Option<&'a str> {
    let name = text[at..].strip_prefix('"')?.strip_prefix(prefix)?;
    let channel = CHANNELS.iter().find(|channel| {
        name.strip_prefix(**channel)
            .is_some_and(|rest| rest.starts_with(['"', '-']))
    })?;
    let close = text.len() - name.len() + channel.len() + name[channel.len()..].find('"')?;
    Some(&text[at..=close])
}

/// The name a channel literal (at `lit` in this CODE) is bound to as its default, with the
/// binding's sign (`stock =`, `stock:`), when that name names a toolchain ([`names_stock`]):
/// the literal is the whole value of a `let`, of an assignment that opens a statement, or of a
/// struct literal's field, read past a conversion (`"stable".to_string()`, `.into()`,
/// `.to_owned()`, `String::from(..)`), or the fallback of an `Option` (`.unwrap_or("stable")`,
/// `.unwrap_or_else(|| "stable".into())`); the value ends at a `;`, a `,` or a `}`. A `name =`
/// after a comma opens no statement: it is a macro's named argument
/// (`println!("{stock}", stock = "stable")`), which binds nothing.
fn default_of(code: &str, lit: Range<usize>) -> Option<String> {
    let b = code.as_bytes();
    let mut depth = 0usize;
    // Where the binding opens, and whether a comma stands before it.
    let (mut start, mut listed) = (0, false);
    for i in (0..lit.start).rev() {
        match b[i] {
            b'}' | b';' | b',' | b'{' if depth == 0 => {
                (start, listed) = (i + 1, b[i] == b',');
                break;
            }
            b')' | b']' | b'}' => depth += 1,
            b'(' | b'[' | b'{' if depth > 0 => depth -= 1,
            _ => {}
        }
    }
    let head = code[start..lit.start].trim_start();
    let (is_let, head) = match head.strip_prefix("let ") {
        Some(rest) => (true, rest.trim_start()),
        None => (false, head),
    };
    let head = head.strip_prefix("mut ").map_or(head, str::trim_start);
    let name = &head[..head.bytes().take_while(|&c| is_word(c)).count()];
    let rest = head[name.len()..].trim_start();
    let (sign, value) = match rest.strip_prefix(':').filter(|r| !r.starts_with(':')) {
        Some(typed) if is_let => (" =", typed.split_once('=')?.1),
        Some(field) => (":", field),
        None if !listed => (" =", rest.strip_prefix('=')?),
        None => return None,
    };
    let value: String = value.split_whitespace().collect();
    let close = match value.as_str() {
        "" => "",
        "String::from(" => ")",
        v if v.ends_with(".unwrap_or(") || v.ends_with(".unwrap_or_else(||") => ")",
        _ => return None,
    };
    let after = code[lit.end..].trim_start();
    let after = [".to_string()", ".to_owned()", ".into()"]
        .iter()
        .find_map(|conversion| after.strip_prefix(conversion))
        .unwrap_or(after);
    let ends = after
        .trim_start()
        .strip_prefix(close)
        .is_some_and(|r| r.trim_start().starts_with([';', ',', '}']));
    (ends && names_stock(name)).then(|| format!("{name}{sign}"))
}

fn contains_ignore_ascii_case(hay: &str, needle: &[u8]) -> bool {
    hay.as_bytes()
        .windows(needle.len())
        .any(|w| w.eq_ignore_ascii_case(needle))
}

/// Does this name (a binding's, a field's) name a toolchain (`stock`, `toolchain`,
/// `rustup_channel`)?
fn names_stock(name: &str) -> bool {
    contains_ignore_ascii_case(name, b"stock") || names_toolchain(name)
}

/// The path of the call whose argument list holds byte `at` directly
/// (`crate::rustup::Toolchain::resolve` for `crate::rustup::Toolchain::resolve(rustup, "stable")`),
/// or `None` when `at` sits in an array, a block, a macro's arguments or no call.
fn enclosing_call(code: &str, at: usize) -> Option<&str> {
    let b = code.as_bytes();
    let mut depth = 0usize;
    for i in (0..at).rev() {
        match b[i] {
            b')' | b']' | b'}' => depth += 1,
            b'(' if depth == 0 => {
                let head = code[..i].trim_end();
                let start = head
                    .bytes()
                    .rposition(|c| !(is_word(c) || c == b':' || c == b'.'))
                    .map_or(0, |p| p + 1);
                return Some(&head[start..]).filter(|path| !path.is_empty());
            }
            b'[' | b'{' | b';' if depth == 0 => return None,
            b'(' | b'[' | b'{' => depth -= 1,
            _ => {}
        }
    }
    None
}

/// Does a segment of this call path name a toolchain or rustup (`Toolchain`, `rust_toolchain`,
/// `rustup`, `rustup_run`)?
fn names_toolchain(path: &str) -> bool {
    path.split([':', '.']).any(|segment| {
        contains_ignore_ascii_case(segment, b"toolchain")
            || contains_ignore_ascii_case(segment, b"rustup")
    })
}

/// The TEXT view, line for line and byte for byte beside [`SourceFile::shipped_code`]. A
/// literal on a test-only line is no literal there: its quote is blank in that CODE.
fn text_beside(file: &SourceFile) -> String {
    let mut text = String::with_capacity(file.raw.len());
    for line in file.lines() {
        text.push_str(line.text);
        text.push('\n');
    }
    text
}

/// What makes a target root (of `kinds`) stock by construction, as its line and a description:
/// a gate on the whole file that holds only off Trust's host, else, for a target that runs a
/// `fn main` (not a library), a `fn main` that only reports on Trust's host ([`reports_only`]).
fn foreign_target(file: &SourceFile, kinds: &BTreeSet<Kind>) -> Option<(usize, String)> {
    if !file.mentions(&["cfg("]) {
        return None;
    }
    let lexed = file.lexed();
    let code = lexed.code.as_str();
    let attrs = attributes(code, &lexed.text);
    if let Some((attr, pred)) = attrs
        .iter()
        .filter(|a| a.file)
        .find_map(|a| foreign(a).map(|p| (a, p)))
    {
        return Some((
            attr.no,
            format!(
                "a Cargo target that builds only for `{}`, which Trust ships no std for",
                tidy(pred)
            ),
        ));
    }
    if kinds.iter().all(|kind| *kind == "lib") {
        return None;
    }
    let (attr, pred) = reports_only(code, &attrs)?;
    Some((
        attr.no,
        format!(
            "a Cargo target whose `fn main` only reports on Trust's host: its work is gated to \
             `{}`, which Trust ships no std for",
            tidy(pred)
        ),
    ))
}

/// The predicate of a `cfg` attribute that holds only off Trust's host.
fn foreign(attr: &Attr) -> Option<&str> {
    attr.cfg().filter(|pred| on_trust(pred) == Some(false))
}

/// Does the target's `fn main` only report on Trust's host? Some code in or on a top-level
/// `fn main` is gated to a target Trust has no std for, and no `fn main`, that code (a whole
/// `fn main` when the gate stands on it, or opens its body as an inner `#![cfg(..)]`) and every
/// attribute left out, does work ([`works`]). The answer is the first such gate and its
/// predicate. A `#[cfg(not(windows))]` arm is no gate here: what decides is whether the arm
/// Trust's host compiles is a stub.
fn reports_only<'a>(code: &str, attrs: &'a [Attr]) -> Option<(&'a Attr, &'a str)> {
    let mains = mains(code, attrs);
    let gated: Vec<(&Attr, &str, Range<usize>)> = attrs
        .iter()
        .filter_map(|a| {
            let main = mains.iter().find(|m| m.span.contains(&a.span.start))?;
            let range = if !a.inner {
                a.span.start..item_end(code, a.span.end)
            } else if opener(code, a.span.start) == main.body.start.checked_sub(1) {
                main.span.clone()
            } else {
                return None;
            };
            Some((a, foreign(a)?, range))
        })
        .collect();
    let &(first, pred, _) = gated.first()?;
    let blank: Vec<Range<usize>> = gated
        .iter()
        .map(|(_, _, range)| range.clone())
        .chain(attrs.iter().map(|a| a.span.clone()))
        .collect();
    let host_work = mains.iter().any(|m| {
        // Byte for byte, so the gates' places below stay true of what is kept. Each range's
        // ends sit on ASCII (`#`, a bracket, `;`, `,`) or the file's end, so every character
        // is kept or blanked whole.
        let mut kept = code.as_bytes()[m.body.clone()].to_vec();
        for range in &blank {
            for at in range.start.max(m.body.start)..range.end.min(m.body.end) {
                kept[at - m.body.start] = b' ';
            }
        }
        let gates: Vec<Range<usize>> = gated
            .iter()
            .filter(|(_, _, range)| m.body.contains(&range.start))
            .map(|(_, _, range)| range.start - m.body.start..range.end - m.body.start)
            .collect();
        works(&String::from_utf8_lossy(&kept), &gates)
    });
    (!host_work).then_some((first, pred))
}

/// Does this CODE (a `fn main`'s body, its gated code at `gates` left out) call anything that
/// does work? A macro that reports, stops or builds a value ([`INERT_MACROS`]), a constructor,
/// a conversion and `exit` do none; any other macro (`serve_main!()`) is work, and so is a call
/// through a tuple field (`handlers.0()`). A call handed a closure that holds gated code (a
/// harness running a bench's rows) is judged by the closure, whose calls are read in their
/// turn; one whose closures hold none (a server handed its handler) is work.
fn works(code: &str, gates: &[Range<usize>]) -> bool {
    code.match_indices(['(', '[', '{']).any(|(at, open)| {
        let head = code[..at].trim_end();
        if let Some(name) = macro_name(head) {
            return !INERT_MACROS.contains(&name);
        }
        let name = callee(head);
        !(open != "("
            || name.is_empty()
            || name.starts_with(|c: char| c.is_ascii_uppercase())
            || INERT_CALLS.contains(&name)
            || KEYWORDS.contains(&name)
            || runs_gated_closure(code, at, gates))
    })
}

/// The name of the macro whose arguments a bracket opens, from the CODE before it, trimmed
/// (`serve_main` for `aterm_p::serve_main!`); `None` when no macro's `!` ends it (`if !`, `!=`).
fn macro_name(head: &str) -> Option<&str> {
    let head = head.strip_suffix('!')?;
    let start = head.bytes().rposition(|b| !is_word(b)).map_or(0, |p| p + 1);
    Some(&head[start..]).filter(|name| !name.is_empty())
}

/// The name a `(` calls, from the CODE before it: the identifier that ends it, read past a
/// turbofish (`collect::<Vec<_>>`); empty for a macro (`eprintln!`), a tuple or a group.
fn callee(head: &str) -> &str {
    let mut head = head.trim_end();
    if let Some(open) = head.rfind("::<") {
        let generics = &head[open + 2..];
        let balanced = generics.matches('<').count() == generics.matches('>').count();
        if balanced && generics.ends_with('>') && !generics.contains(['(', ')', ';', '{', '}']) {
            head = &head[..open];
        }
    }
    let start = head.bytes().rposition(|b| !is_word(b)).map_or(0, |p| p + 1);
    &head[start..]
}

/// Does an argument of the call whose `(` is at `at` open with a closure (`|row| ..`, `|| ..`,
/// `move || ..`) and hold the start of one of `gates`?
fn runs_gated_closure(code: &str, at: usize, gates: &[Range<usize>]) -> bool {
    let (mut depth, mut start, mut end) = (0usize, at + 1, code.len());
    let mut args = Vec::new();
    for (i, b) in code.bytes().enumerate().skip(at + 1) {
        match b {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' if depth == 0 => {
                end = i;
                break;
            }
            b')' | b']' | b'}' => depth -= 1,
            b',' if depth == 0 => {
                args.push(start..i);
                start = i + 1;
            }
            _ => {}
        }
    }
    args.push(start..end);
    args.into_iter().any(|range| {
        let arg = code[range.clone()].trim_start();
        let closure = arg
            .strip_prefix("move")
            .filter(|rest| rest.starts_with([' ', '|']))
            .unwrap_or(arg)
            .trim_start()
            .starts_with('|');
        closure && gates.iter().any(|gate| range.contains(&gate.start))
    })
}

/// One attribute (`#[..]` or `#![..]`) that opens a line, read over every line rustfmt wrapped
/// it across.
struct Attr {
    /// Its bytes, `#` through `]`.
    span: Range<usize>,
    /// Its first line's number.
    no: usize,
    /// `#![..]`; `file` when it stands at column 0, so it is the file's own.
    inner: bool,
    file: bool,
    /// Its TEXT inside the brackets (`cfg(not(windows))`), each run of whitespace one space.
    body: String,
}

impl Attr {
    /// The predicate of a `cfg` attribute.
    fn cfg(&self) -> Option<&str> {
        self.body
            .strip_prefix("cfg(")?
            .strip_suffix(')')
            .map(str::trim)
    }
}

/// Every attribute that opens a line, in file order. The brackets are counted in the CODE view,
/// where a literal's contents are blank, and the body is read from the TEXT view beside it.
fn attributes(code: &str, text: &str) -> Vec<Attr> {
    let mut out = Vec::new();
    let (mut at, mut no) = (0, 1);
    while at < code.len() {
        let line = &code[at..code[at..].find('\n').map_or(code.len(), |n| at + n)];
        let indent = line.len() - line.trim_start().len();
        let inner = line[indent..].starts_with("#![");
        let mut last = at + indent;
        if inner || line[indent..].starts_with("#[") {
            let open = at + indent + if inner { 2 } else { 1 };
            let Some(close) = closing(code, open) else {
                break;
            };
            out.push(Attr {
                span: at + indent..close + 1,
                no,
                inner,
                file: inner && indent == 0,
                body: text[open + 1..close]
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" "),
            });
            last = close;
        }
        let next = code[last..].find('\n').map_or(code.len(), |n| last + n + 1);
        no += code[at..next].matches('\n').count();
        at = next;
    }
    out
}

/// The `]` that closes the `[` at byte `open` of this CODE.
fn closing(code: &str, open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (i, byte) in code.bytes().enumerate().skip(open) {
        match byte {
            b'[' => depth += 1,
            b']' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// The end of the item or statement that starts at byte `from` of this CODE, past whitespace
/// and further attributes: just past its `;` at its own depth, or past a brace that closes a
/// block at its own depth (and a `;` after it) unless `else`, `.` or `?` carries the statement
/// on or it is a `let`, which runs to its `;`; for a block's tail expression, where the
/// enclosing block closes. A `,` at its own depth ends it only where commas part items: in an
/// argument list or an array, after a match arm's `=>`, or in a struct literal's field
/// (`name: ..`, `name,`). Anywhere else the comma is a type's or a closure's (`-> Result<(), E>`,
/// `HashMap<u8, u8>`, `parse::<A, B>()`, `|a, b|`).
fn item_end(code: &str, from: usize) -> usize {
    let b = code.as_bytes();
    let mut i = from;
    loop {
        while b.get(i).is_some_and(u8::is_ascii_whitespace) {
            i += 1;
        }
        if !b[i..].starts_with(b"#[") {
            break;
        }
        i = closing(code, i + 1).map_or(b.len(), |close| close + 1);
    }
    let is_let = code[i..]
        .strip_prefix("let")
        .is_some_and(|rest| !rest.bytes().next().is_some_and(is_word));
    let listed = opener(code, i).is_some_and(|at| matches!(b[at], b'(' | b'['));
    let field = is_field(&code[i..]);
    let mut arm = false;
    let mut depth = 0usize;
    while i < b.len() {
        match b[i] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' if depth == 0 => return i,
            b')' | b']' => depth -= 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    let rest = code[i + 1..].trim_start();
                    if rest.starts_with(';') {
                        return code.len() - rest.len() + 1;
                    }
                    let carried = rest.starts_with(['.', '?'])
                        || rest
                            .strip_prefix("else")
                            .is_some_and(|r| !r.bytes().next().is_some_and(is_word));
                    if !carried && !is_let {
                        return i + 1;
                    }
                }
            }
            b';' if depth == 0 => return i + 1,
            b'=' if depth == 0 && b.get(i + 1) == Some(&b'>') => arm = true,
            b',' if depth == 0 && (listed || arm || field) => return i + 1,
            _ => {}
        }
        i += 1;
    }
    b.len()
}

/// Where the bracket (`(`, `[` or `{`) whose contents hold byte `at` of this CODE opens.
fn opener(code: &str, at: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (i, byte) in code.as_bytes()[..at].iter().enumerate().rev() {
        match byte {
            b')' | b']' | b'}' => depth += 1,
            b'(' | b'[' | b'{' if depth == 0 => return Some(i),
            b'(' | b'[' | b'{' => depth -= 1,
            _ => {}
        }
    }
    None
}

/// Does this CODE open with a struct literal's field: a name, then `:` (not a path's `::`) or
/// `,`? No statement opens so.
fn is_field(code: &str) -> bool {
    let name = code.bytes().take_while(|&c| is_word(c)).count();
    let rest = code[name..].trim_start();
    name > 0 && (rest.starts_with(',') || (rest.starts_with(':') && !rest.starts_with("::")))
}

/// The outer attributes standing on the item at byte `item`, nearest first (a comment, blank in
/// the CODE view, may sit between them).
fn chain<'a>(code: &str, attrs: &'a [Attr], item: usize) -> Vec<&'a Attr> {
    let mut out = Vec::new();
    let mut first = item;
    while let Some(attr) = attrs
        .iter()
        .find(|a| !a.inner && a.span.end == code[..first].trim_end().len())
    {
        out.push(attr);
        first = attr.span.start;
    }
    out
}

/// A top-level `fn main`.
struct Main {
    /// Its bytes, from its first attribute through its closing brace.
    span: Range<usize>,
    /// Its body's bytes, inside the braces.
    body: Range<usize>,
}

/// Every top-level `fn main` of this CODE. Its body opens at the first `{` after its line
/// opens (no signature of a `fn main` holds one, while its return type may hold a comma:
/// `-> Result<(), String>`) and ends at the `}` that closes it.
fn mains(code: &str, attrs: &[Attr]) -> Vec<Main> {
    let mut out = Vec::new();
    let mut at = 0;
    for line in code.split_inclusive('\n') {
        let start = at;
        at += line.len();
        if !opens_main(line) {
            continue;
        }
        let Some(open) = code[start..].find('{').map(|n| start + n) else {
            continue;
        };
        let mut depth = 0usize;
        let close = (open..code.len())
            .find(|&i| match code.as_bytes()[i] {
                b'{' => {
                    depth += 1;
                    false
                }
                b'}' => {
                    depth -= 1;
                    depth == 0
                }
                _ => false,
            })
            .unwrap_or(code.len());
        out.push(Main {
            span: chain(code, attrs, start)
                .last()
                .map_or(start, |attr| attr.span.start)
                ..(close + 1).min(code.len()),
            body: open + 1..close,
        });
    }
    out
}

/// Does this CODE line open a top-level `fn main` (`fn main(`, `pub fn main(`, `pub(crate) fn
/// main(`), the qualifiers a `fn` may carry read past (`async fn main(`, `pub unsafe extern "C"
/// fn main(`)?
fn opens_main(code: &str) -> bool {
    let mut rest = match code.strip_prefix("pub") {
        Some(vis) if vis.starts_with('(') => {
            vis.split_once(')').map_or("", |(_, r)| r).trim_start()
        }
        Some(vis) if vis.starts_with(' ') => vis.trim_start(),
        Some(_) => return false,
        // At column 0: an indented `fn main` is no top-level one.
        None => code,
    };
    loop {
        let word = &rest[..rest.bytes().take_while(|&c| is_word(c)).count()];
        rest = match word {
            "const" | "async" | "unsafe" => rest[word.len()..].trim_start(),
            // The ABI is a literal, its contents blank in this CODE: `extern " "`.
            "extern" => {
                let abi = rest[word.len()..].trim_start();
                abi.strip_prefix('"').map_or(abi, |r| {
                    r.split_once('"').map_or("", |(_, r)| r).trim_start()
                })
            }
            _ => break,
        };
    }
    rest.starts_with("fn main(")
}

/// `pred` (a `cfg` predicate) on Trust's host: `Some(false)` when no build there compiles what
/// it gates, `None` when the answer turns on a name the host does not fix (`feature`, `test`,
/// `debug_assertions`).
fn on_trust(pred: &str) -> Option<bool> {
    let pred = pred.trim();
    if let Some(inner) = pred.strip_prefix("not(").and_then(|r| r.strip_suffix(')')) {
        return on_trust(inner).map(|holds| !holds);
    }
    for (head, any) in [("all(", false), ("any(", true)] {
        if let Some(inner) = pred.strip_prefix(head).and_then(|r| r.strip_suffix(')')) {
            let mut open = false;
            for child in crate::split_top_level_commas(inner) {
                match on_trust(child) {
                    Some(holds) if holds == any => return Some(any),
                    Some(_) => {}
                    None => open = true,
                }
            }
            return (!open).then_some(!any);
        }
    }
    let (name, value) = match pred.split_once('=') {
        Some((name, value)) => (
            name.trim(),
            value.trim().strip_prefix('"')?.strip_suffix('"')?,
        ),
        None if pred == "unix" || pred == "windows" => ("target_family", pred),
        None => return None,
    };
    TRUST_HOST
        .iter()
        .find(|(fixed, _)| *fixed == name)
        .map(|(_, host)| *host == value)
}

/// A predicate as rustfmt writes it on one line: no space inside a parenthesis, no trailing
/// comma.
fn tidy(pred: &str) -> String {
    pred.replace("( ", "(")
        .replace(" )", ")")
        .replace(",)", ")")
}

/// Does a comment of `file` carry a label (the text view blank where the raw file has it)?
fn labelled(file: &SourceFile) -> bool {
    if !file.mentions(LABELS) {
        return false;
    }
    file.raw.lines().zip(file.lines()).any(|(raw, line)| {
        LABELS.iter().any(|label| {
            raw.match_indices(label).any(|(at, _)| {
                line.text
                    .get(at..at + label.len())
                    .is_some_and(|seen| seen.trim().is_empty())
            })
        })
    })
}

#[cfg(test)]
#[path = "stock_label_tests.rs"]
mod tests;
