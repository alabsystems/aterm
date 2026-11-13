// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Moving a file's inline test modules out of line: what `targo --unverified verify census --fix`
//! writes for inline-test-module's findings ([`crate::Unread`]).
//!
//! `#[cfg(test)] mod tests { … }` in `src/ops.rs` becomes `#[cfg(test)] #[path = "ops_tests.rs"]
//! mod tests;`, and `src/ops_tests.rs` holds the body (`ops_own_tests.rs` where that name is
//! taken). Outside any inline module a `#[path]` is relative to the declaring file's folder, so
//! the new file sits beside the old one, and every relative `include!`-family path in the body,
//! which is relative to the file it is written in, still names the same file.
//!
//! Two kinds of module take the file Rust's own lookup finds instead, in the folder an inline
//! module's children already resolve in: one inside a shipped inline module (`mod a {
//! #[cfg(test)] mod tests { … } }` in `src/ops.rs` moves to `src/ops/a/tests.rs`), and one whose
//! body mounts files of its own (`mod tests { mod deep; }` moves to `src/ops/tests.rs`, whose
//! `mod deep;` is `src/ops/tests/deep.rs` as before; with a `#[path]` child, to
//! `src/ops/tests/mod.rs`, the only file whose `#[path]`s are relative to that folder, as an
//! inline module's are). Such a file is in another folder, so a body that includes a file by a
//! relative path stays.
//!
//! What would read differently stays where it is, with why ([`Refusal`]): a module that carries
//! `#[path]` already or a `path` in an inner attribute, whose container carries `#[path]`, with an
//! attribute a macro may read or an inner test gate, a nested or mounting module in a file
//! reached both owning its folder and not, every new file taken or one Cargo would build as a
//! target of its own, a file a generator writes, and a file whose own `#![cfg]` makes all of it
//! test-only (its `mod` line takes the `cfg` instead). What a moved body reads of its own place
//! (`file!`, `line!`, a caller `Location`) names its new file: the plan lists those modules
//! ([`Plan::located`]), whose tests say whether that matters.
//!
//! Each plan is held to the code it came from before anything is written: the file's shipped
//! tokens are unchanged, each moved body's code tokens are its new file's (a string's escaped line
//! breaks read as Rust reads them), and no moved module is left inline; otherwise nothing of the
//! file moves.

use std::collections::BTreeSet;
use std::fmt;

use crate::items::{IncludeArg, Mount, MountPath, Source, TestModule};
use crate::lex::{SpanKind, spans};

/// What moving one file's inline test modules out of line writes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Plan {
    /// The file's new text, when any module moves.
    pub parent: Option<String>,
    /// Each new file, by repo-relative path, with its text.
    pub files: Vec<(String, String)>,
    /// Each module that stays, by name (empty for the whole file), and why.
    pub refused: Vec<(String, Refusal)>,
    /// Each moved module whose body reads its own place (`file!`, `line!`, `column!`, a caller
    /// or panic `Location`), which now names its new file: its tests say whether it still holds.
    pub located: Vec<String>,
}

/// Why a module stays where it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The file does not read as token trees.
    Unreadable,
    /// The file's own `#![cfg]` makes all of it test-only: its `mod` line takes the `cfg`.
    Whole,
    /// A generator writes the file.
    Generated,
    /// The module carries `#[path]` already.
    Pathed,
    /// An attribute on the module is one a macro may read, and would read `mod tests;` instead.
    Rewritten,
    /// Its test `cfg` is an inner attribute, so its `mod` line would not be gated.
    InnerGate,
    /// An enclosing inline module carries `#[path]`, so its children's folder is not its name.
    ContainerPathed,
    /// The file is reached both owning its folder and not, so a child's folder differs.
    Ownership,
    /// Its body includes a file by a relative path, and its new file is in another folder.
    RelativeInclude,
    /// Every file it may take, or a file that would make Rust's lookup ambiguous, exists.
    Taken(String),
    /// Every file it may take is one Cargo would build as a target of its own (`src/bin/*.rs`,
    /// `tests/*.rs`, …).
    Target(String),
    /// Moved, it would not read the same.
    Differs(String),
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreadable => f.write_str("it does not read as token trees"),
            Self::Whole => f.write_str(
                "its own `#![cfg]` makes all of it test-only: gate the `mod` line that mounts it \
                 with the same `cfg`",
            ),
            Self::Generated => {
                f.write_str("a generator writes this file: move it in the generator")
            }
            Self::Pathed => f.write_str("it carries `#[path]` already: move it by hand"),
            Self::Rewritten => f.write_str(
                "an attribute on it is one a macro may read, which out of line would read \
                 `mod tests;`: move it by hand",
            ),
            Self::InnerGate => f.write_str(
                "its test `cfg` is inside it, so its `mod` line is not gated: put the `cfg` on the \
                 `mod` line, then move it",
            ),
            Self::ContainerPathed => f.write_str(
                "an enclosing inline module carries `#[path]`, so its children's folder is not its \
                 name: move it by hand",
            ),
            Self::Ownership => f.write_str(
                "the file is reached both owning its folder and not, so a child's folder differs \
                 between builds: move it by hand",
            ),
            Self::RelativeInclude => f.write_str(
                "its body includes a file by a relative path, which its new file's folder would \
                 change: move it by hand",
            ),
            Self::Taken(rel) => write!(f, "`{rel}` exists: move it by hand"),
            Self::Target(rel) => write!(
                f,
                "`{rel}` would be a target Cargo builds on its own: move it by hand"
            ),
            Self::Differs(why) => {
                write!(
                    f,
                    "moved, it would not read the same ({why}): move it by hand"
                )
            }
        }
    }
}

/// The header a new file takes when its parent's head names no licence.
const HEADER: &str = "// Copyright 2026 Andrew Yates\n// SPDX-License-Identifier: Apache-2.0\n\
                      // Author: Andrew Yates\n";

/// The plan for the file at repo-relative `rel`, of the package whose folder is `package`, whose
/// text is `text`. `owns_dir` holds, for each target that compiles it with tests, whether it owns
/// its folder there (a crate root, a `mod.rs`, an included or `#[path]`-mounted file), as
/// [`crate::Unread`] does; `exists` says whether a repo-relative path is a file or folder.
#[must_use]
pub fn plan(
    rel: &str,
    package: &str,
    owns_dir: &BTreeSet<bool>,
    text: &str,
    exists: &dyn Fn(&str) -> bool,
) -> Plan {
    let mut out = Plan::default();
    let Some(source) = Source::read(text.to_string()) else {
        out.refused.push((String::new(), Refusal::Unreadable));
        return out;
    };
    if source.whole() {
        out.refused.push((String::new(), Refusal::Whole));
    }
    if !matches!(crate::seal::check(text), crate::seal::Seal::Absent) {
        for module in source.test_modules() {
            out.refused.push((module.name.clone(), Refusal::Generated));
        }
        return out;
    }
    let (dir, file) = rel.rsplit_once('/').unwrap_or(("", rel));
    let stem = file.strip_suffix(".rs").unwrap_or(file);
    let header = header(text);
    let strings: Vec<(usize, usize, bool)> = spans(text)
        .into_iter()
        .filter(|span| span.kind == SpanKind::Str)
        .map(|span| (span.start, span.end, raw(&text[span.start..])))
        .collect();
    let mut planned: BTreeSet<String> = BTreeSet::new();
    let mut moved: Vec<(&TestModule, Option<String>, String)> = Vec::new();
    let mut bodies = Vec::new();
    for module in source.test_modules() {
        let indent = indent_at(text, module.item.0);
        let body = dedent(text, module.body, indent.len() + 4, &strings);
        let candidates = match place(&source, module, &body, dir, stem, owns_dir) {
            Ok(candidates) => candidates,
            Err(why) => {
                out.refused.push((module.name.clone(), why));
                continue;
            }
        };
        let taken = |rel: &str| exists(rel) || planned.contains(rel);
        let target = |rel: &str| cargo_builds(package, rel);
        let free = candidates.iter().find(|c| {
            !target(&c.target) && !taken(&c.target) && !c.rival.as_deref().is_some_and(taken)
        });
        let Some(free) = free else {
            let first = &candidates[0];
            let why = if candidates.iter().all(|c| target(&c.target)) {
                Refusal::Target(first.target.clone())
            } else {
                Refusal::Taken(
                    first
                        .rival
                        .clone()
                        .filter(|rival| taken(rival))
                        .unwrap_or_else(|| first.target.clone()),
                )
            };
            out.refused.push((module.name.clone(), why));
            continue;
        };
        planned.extend(free.rival.clone());
        planned.insert(free.target.clone());
        if reads_place(&source.code_in(module.body.0, module.body.1)) {
            out.located.push(module.name.clone());
        }
        moved.push((module, free.attr.clone(), free.target.clone()));
        bodies.push(body);
    }
    if moved.is_empty() {
        return out;
    }
    let mut parent = text.to_string();
    let mut files = Vec::new();
    for ((module, attr, target), body) in moved.iter().zip(&bodies).rev() {
        let indent = indent_at(text, module.item.0);
        let mut declaration = text[module.item.0..module.head].to_string();
        if let Some(attr) = attr {
            declaration.push_str(&format!("#[path = \"{attr}\"]\n{indent}"));
        }
        declaration.push_str(text[module.head..module.open].trim_end());
        declaration.push(';');
        parent.replace_range(module.item.0..module.item.1, &declaration);
        files.push((target.clone(), format!("{header}\n{}\n", body.trim_end())));
    }
    files.reverse();
    if let Err(why) = holds(&source, &parent, &moved, &files) {
        out.located.clear();
        for (module, ..) in &moved {
            out.refused
                .push((module.name.clone(), Refusal::Differs(why.clone())));
        }
        return out;
    }
    out.parent = Some(parent);
    out.files = files;
    out
}

/// A file a moved module may take: its path, the `#[path]` its declaration then carries (none
/// where Rust's own lookup finds it), and the other file whose presence would make a lookup of
/// that name ambiguous (`x.rs` beside `x/mod.rs`), which must not exist either.
struct Candidate {
    target: String,
    attr: Option<String>,
    rival: Option<String>,
}

/// The files `module` may move to, in preference order, or why it stays. `body` is its body as
/// its new file would hold it.
fn place(
    source: &Source,
    module: &TestModule,
    body: &str,
    dir: &str,
    stem: &str,
    owns_dir: &BTreeSet<bool>,
) -> Result<Vec<Candidate>, Refusal> {
    if module.inner_gate {
        return Err(Refusal::InnerGate);
    }
    if module.rewritten {
        return Err(Refusal::Rewritten);
    }
    // Out of line, a module's own inner attributes head a file, where rustc reads a `path`
    // among them against another folder.
    let code = source.code_in(module.body.0, module.body.1);
    if source.code_in(module.item.0, module.head).contains(&"path") || inner_path(&code) {
        return Err(Refusal::Pathed);
    }
    let inside = module.body.0..module.body.1;
    let mounts: Vec<&Mount> = source
        .mounts()
        .iter()
        .filter(|mount| inside.contains(&mount.at))
        .collect();
    // A `#[path]` file of `name` beside `folder`, where a default lookup of the same name in
    // another file would also accept `<name>/mod.rs`.
    let at = |folder: &str, name: String| {
        let base = name.strip_suffix(".rs").unwrap_or(&name);
        Candidate {
            target: joined(&[folder, &name]),
            rival: Some(joined(&[folder, base, "mod.rs"])),
            attr: Some(name),
        }
    };
    if module.within.is_empty() && mounts.is_empty() {
        // Last, a folder named for the file, where `src/bin/<stem>.rs`'s module goes: no name
        // beside a binary's root is one Cargo would not build as a binary of its own.
        let name = sibling(stem, &module.name);
        let second = own(&name);
        let third = format!("{stem}/{name}");
        return Ok(vec![at(dir, name), at(dir, second), at(dir, third)]);
    }
    if module.pathed {
        return Err(Refusal::ContainerPathed);
    }
    let [owns] = owns_dir.iter().copied().collect::<Vec<_>>()[..] else {
        return Err(Refusal::Ownership);
    };
    if source.includes().iter().any(|include| {
        inside.contains(&include.at)
            && !matches!(include.arg, IncludeArg::ManifestDir(_) | IncludeArg::OutDir)
    }) {
        return Err(Refusal::RelativeInclude);
    }
    let mut parts: Vec<&str> = vec![dir];
    if !owns {
        parts.push(stem);
    }
    parts.extend(module.within.iter().map(String::as_str));
    let folder = joined(&parts);
    let base = joined(&[&folder, &module.name]);
    let (flat, nested) = (format!("{base}.rs"), format!("{base}/mod.rs"));
    let found = |target: String, rival: String| Candidate {
        target,
        attr: None,
        rival: Some(rival),
    };
    if !mounts.is_empty() {
        // Mounts in test-only code are no item this reader places; at the top of the new file,
        // where they will be, they are. A `mod.rs` holds its children exactly as the inline
        // module did (both own the folder named for the module); `<name>.rs` holds only children
        // found by name at its top, since a `#[path]` in it, a mount's or an enclosing inline
        // module's, is relative to the folder beside it.
        let by_name = Source::read(body.to_string()).is_some_and(|new| {
            new.mounts()
                .iter()
                .all(|mount| mount.top && mount.path == MountPath::Default)
        });
        Ok(if by_name {
            vec![found(flat.clone(), nested.clone()), found(nested, flat)]
        } else {
            vec![found(nested, flat)]
        })
    } else if module.name == "tests" || module.name.ends_with("_tests") {
        Ok(vec![
            found(flat, nested),
            at(&folder, own(&format!("{}.rs", module.name))),
        ])
    } else {
        let name = format!("{}_tests.rs", module.name);
        let second = own(&name);
        Ok(vec![at(&folder, name), at(&folder, second)])
    }
}

/// Whether an inner attribute in `code`, the module's own or a nested inline module's, names a
/// `path` at any depth (`#![path = …]`, `#![cfg_attr(unix, path = …)]`).
fn inner_path(code: &[&str]) -> bool {
    code.windows(3).enumerate().any(|(at, w)| {
        let mut depth = 0usize;
        w == ["#", "!", "["]
            && code[at + 2..]
                .iter()
                .take_while(|&&token| {
                    match token {
                        "[" => depth += 1,
                        "]" => depth = depth.saturating_sub(1),
                        _ => {}
                    }
                    depth > 0
                })
                .any(|&token| token == "path")
    })
}

/// Whether `code` reads its own place: `file!()`, `line!()`, `column!()`, a `Location` (a caller's
/// or a panic's), or a `#[track_caller]` function, which reports where it was called from.
fn reads_place(code: &[&str]) -> bool {
    code.windows(3)
        .any(|w| matches!(w[0], "file" | "line" | "column") && w[1..] == ["!", "("])
        || code
            .iter()
            .any(|&token| matches!(token, "Location" | "track_caller"))
}

/// Whether Cargo would build the file at repo-relative `rel` as a target of the package whose
/// folder is `package` on its own: a crate root it finds by layout.
fn cargo_builds(package: &str, rel: &str) -> bool {
    let Some(sub) = rel
        .strip_prefix(package)
        .and_then(|sub| sub.strip_prefix('/'))
    else {
        return false;
    };
    let parts: Vec<&str> = sub.split('/').collect();
    match parts[..] {
        ["build.rs"] | ["src", "lib.rs" | "main.rs"] => true,
        ["src", "bin", file] | ["tests" | "benches" | "examples", file] => file.ends_with(".rs"),
        ["src", "bin", _, "main.rs"] | ["tests" | "benches" | "examples", _, "main.rs"] => true,
        _ => false,
    }
}

/// The name a moved module's file takes where `name` is taken: `_own` before its `_tests.rs`.
fn own(name: &str) -> String {
    let base = name.strip_suffix(".rs").unwrap_or(name);
    match base.strip_suffix("_tests") {
        Some(base) => format!("{base}_own_tests.rs"),
        None if base == "tests" => "own_tests.rs".to_string(),
        None => format!("{base}_own_tests.rs"),
    }
}

/// The file a test module of a file named `<stem>.rs` moves to: `<stem>_tests.rs` for `tests`,
/// `<stem>_<name>.rs` for a name that ends `_tests`, else `<stem>_<name>_tests.rs`.
#[must_use]
pub fn sibling(stem: &str, name: &str) -> String {
    if name == "tests" {
        format!("{stem}_tests.rs")
    } else if name.ends_with("_tests") {
        format!("{stem}_{name}.rs")
    } else {
        format!("{stem}_{name}_tests.rs")
    }
}

fn joined(parts: &[&str]) -> String {
    parts
        .iter()
        .filter(|part| !part.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join("/")
}

/// `text`'s head, the plain `//` lines it opens with, which carry its licence and what aterm
/// changed in upstream code it derives from, or [`HEADER`] where they name no licence.
fn header(text: &str) -> String {
    let lines: Vec<&str> = text
        .lines()
        .take_while(|line| {
            line.starts_with("//") && !line.starts_with("///") && !line.starts_with("//!")
        })
        .collect();
    if lines
        .iter()
        .any(|line| line.contains("SPDX-License-Identifier:"))
    {
        lines.iter().map(|line| format!("{line}\n")).collect()
    } else {
        HEADER.to_string()
    }
}

/// The whitespace that opens the line holding byte `at`.
fn indent_at(text: &str, at: usize) -> &str {
    let start = text[..at].rfind('\n').map_or(0, |n| n + 1);
    let line = &text[start..];
    &line[..line.len() - line.trim_start_matches([' ', '\t']).len()]
}

/// Whether the string literal that starts `text` is raw (`r"…"`, `br#"…"#`, `cr"…"`).
fn raw(text: &str) -> bool {
    let text = text.trim_start_matches(['b', 'c']);
    text.starts_with('r')
}

/// `text[body]` with up to `by` leading spaces taken off each line, but a line that starts
/// inside a string literal, whose text that would change, unless the line before ends in an
/// escaped line break of a cooked string, after which Rust skips the leading whitespace anyway.
/// The first line (what follows the opening brace on its line) and leading blank lines go.
fn dedent(text: &str, body: (usize, usize), by: usize, strings: &[(usize, usize, bool)]) -> String {
    let mut out = String::new();
    let mut at = body.0;
    let mut started = false;
    for line in text[body.0..body.1].split_inclusive('\n') {
        let start = at;
        at += line.len();
        if !started {
            if line.trim().is_empty() {
                continue;
            }
            started = true;
        }
        let inside = strings
            .iter()
            .find(|&&(s, e, _)| s < start && start < e)
            .copied();
        let keep = match inside {
            None => false,
            Some((_, _, true)) => true,
            Some((_, _, false)) => !escaped_break(&text[..start]),
        };
        if keep {
            out.push_str(line);
            continue;
        }
        let spaces = line.len() - line.trim_start_matches(' ').len();
        out.push_str(&line[spaces.min(by)..]);
    }
    out
}

/// Whether `before` ends with a backslash that escapes the line break after it.
fn escaped_break(before: &str) -> bool {
    let line = before.strip_suffix('\n').unwrap_or(before);
    let line = line.strip_suffix('\r').unwrap_or(line);
    (line.len() - line.trim_end_matches('\\').len()) % 2 == 1
}

/// `token` as Rust reads it where it is a cooked string: each escaped line break, with the
/// whitespace that follows it, is no part of its value.
fn read_as(token: &str) -> String {
    let cooked = !raw(token) && token.trim_start_matches(['b', 'c']).starts_with('"');
    if !cooked {
        return token.to_string();
    }
    let mut out = String::with_capacity(token.len());
    let mut chars = token.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            Some('\n' | '\r') => {
                while chars
                    .next_if(|c| matches!(c, ' ' | '\t' | '\n' | '\r'))
                    .is_some()
                {}
            }
            Some(&next) => {
                out.push(c);
                out.push(next);
                chars.next();
            }
            None => out.push(c),
        }
    }
    out
}

fn code(tokens: Vec<&str>) -> Vec<String> {
    tokens.into_iter().map(read_as).collect()
}

/// Whether the planned texts read as the code they came from (see the module's docs).
fn holds(
    before: &Source,
    parent: &str,
    moved: &[(&TestModule, Option<String>, String)],
    files: &[(String, String)],
) -> Result<(), String> {
    let after = Source::read(parent.to_string()).ok_or("the file no longer reads")?;
    // The code a build without tests compiles, and each comment with where it sits among it (a
    // doc string, or text a macro may read): together they are that build's whole text.
    let shipped = |source: &Source| -> (Vec<String>, Vec<(String, usize)>) {
        let code = source
            .code()
            .iter()
            .map(|token| read_as(token.text))
            .collect();
        let comments = source
            .comments()
            .iter()
            .map(|comment| (read_as(comment.at.text), comment.gap))
            .collect();
        (code, comments)
    };
    if shipped(before) != shipped(&after) {
        return Err("its shipped code changes".to_string());
    }
    if after.test_modules().len() + moved.len() != before.test_modules().len() {
        return Err("a moved module is still inline".to_string());
    }
    let mounted: Vec<&MountPath> = after
        .mounts()
        .iter()
        .filter(|mount| mount.test)
        .map(|mount| &mount.path)
        .collect();
    for ((module, attr, _), (rel, text)) in moved.iter().zip(files) {
        let expected = attr
            .as_ref()
            .map_or(MountPath::Default, |attr| MountPath::Literal(attr.clone()));
        if !mounted.contains(&&expected) {
            return Err(format!("no test-only `mod` line mounts `{rel}`"));
        }
        let new = Source::read(text.clone()).ok_or_else(|| format!("`{rel}` does not read"))?;
        if code(new.code_in(0, text.len())) != code(before.code_in(module.body.0, module.body.1)) {
            return Err(format!("`{rel}` holds different code"));
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "outline_tests.rs"]
mod tests;
