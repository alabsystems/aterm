// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-libc-oracle harvest` — widen a cell's `union.json` list from the
//! compiler.
//!
//! The item list was derived, not from a source scan, but from what a workspace
//! check of that cell could not resolve, iterated to a fixed point: build with
//! the shim, read every name the errors say is missing from `libc`, keep only
//! those the REFERENCE exports on that cell, add them, regenerate, repeat. The
//! reference filter is what makes the loose matching safe: a name libc lacks
//! can never become an obligation. The list only ever grows.

use std::collections::{BTreeMap, BTreeSet};

use crate::closure::PRIMS;

/// Whether `c` continues an identifier.
fn ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// The identifier at the start of `text`, if one starts there.
fn ident(text: &str) -> Option<&str> {
    let first = text.chars().next()?;
    if !(first.is_ascii_alphabetic() || first == '_') {
        return None;
    }
    let end = text.find(|c: char| !ident_char(c)).unwrap_or(text.len());
    Some(&text[..end])
}

/// The identifier quoted at the start of `text` (after its opening backtick),
/// when the backtick closes right after it.
fn quoted(text: &str) -> Option<&str> {
    let name = ident(text)?;
    text[name.len()..].starts_with('`').then_some(name)
}

/// Every name a compiler error line says is missing, once each.
///
/// * ``cannot find <what> `X` `` (value, type, function, struct, macro, trait,
///   constant), the words before it any lowercase list;
/// * ``no `X` in``;
/// * ``unresolved import `path::X` `` (the path lowercase);
/// * `` `libc::X` `` or `` `c::X` `` anywhere.
#[must_use]
pub fn missing_names(line: &str) -> BTreeSet<&str> {
    const KINDS: [&str; 7] = [
        "value ",
        "type ",
        "function ",
        "struct ",
        "macro ",
        "trait ",
        "constant ",
    ];
    let mut out = BTreeSet::new();
    for (at, _) in line.match_indices("cannot find ") {
        let rest = &line[at + "cannot find ".len()..];
        let words_end = rest
            .find(|c: char| !(c.is_ascii_lowercase() || c == ',' || c == ' '))
            .unwrap_or(rest.len());
        let words = &rest[..words_end];
        if KINDS.iter().any(|k| words.ends_with(k)) && rest[words_end..].starts_with('`') {
            out.extend(quoted(&rest[words_end + 1..]));
        }
    }
    for (at, _) in line.match_indices("no `") {
        let rest = &line[at + "no `".len()..];
        if let Some(name) = quoted(rest).filter(|n| rest[n.len()..].starts_with("` in")) {
            out.insert(name);
        }
    }
    for prefix in ["unresolved import `", "unresolved imports `"] {
        for (at, _) in line.match_indices(prefix) {
            let rest = &line[at + prefix.len()..];
            let Some(close) = rest.find('`') else {
                continue;
            };
            let path = &rest[..close];
            let (module, name) = path.rsplit_once("::").unwrap_or(("", path));
            let lowercase_path = module
                .chars()
                .all(|c| c.is_ascii_lowercase() || c == '_' || c == ':');
            if lowercase_path && ident(name).is_some_and(|n| n.len() == name.len()) {
                out.insert(name);
            }
        }
    }
    for prefix in ["`c::", "`libc::"] {
        for (at, _) in line.match_indices(prefix) {
            out.extend(quoted(&line[at + prefix.len()..]));
        }
    }
    out
}

/// The path segment after `marker` in `line`, if `marker` occurs.
fn segment_after<'l>(line: &'l str, marker: &str) -> Option<&'l str> {
    let (_, rest) = line.split_once(marker)?;
    let end = rest.find('/')?;
    let name = &rest[..end];
    let valid = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '+' | '-'));
    valid.then_some(name)
}

/// Who a line's error is in: a registry crate, a first-party member, or the
/// line's own prefix.
#[must_use]
pub fn crate_of(line: &str) -> String {
    if let Some(name) = line
        .split_once("registry/src/")
        .and_then(|(_, rest)| segment_after(rest, "/"))
    {
        return name.to_string();
    }
    for tree in ["crates/", "apps/", "tools/", "vendor/"] {
        let member = line
            .match_indices(tree)
            .filter(|(at, _)| *at == 0 || matches!(line.as_bytes()[at - 1], b'/' | b' '))
            .find_map(|(at, _)| segment_after(&line[at..], tree));
        if let Some(name) = member {
            return format!("FIRST-PARTY {name}");
        }
    }
    line.split(':')
        .next()
        .unwrap_or_default()
        .chars()
        .take(60)
        .collect()
}

/// What one log said a cell is missing.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Harvest {
    /// Crate -> the reference names its errors miss.
    pub by_crate: BTreeMap<String, BTreeSet<String>>,
    /// Error lines that named nothing the reference exports.
    pub unattributed: Vec<String>,
}

impl Harvest {
    /// Read a `--message-format short` check log; `exported` is the
    /// reference's crate-root names.
    #[must_use]
    pub fn scan(log: &str, exported: &dyn Fn(&str) -> bool) -> Self {
        let mut out = Self::default();
        let known = |name: &&str| exported(name) || PRIMS.contains(name);
        for line in log
            .lines()
            .filter(|l| l.contains("error[") || l.contains("error:"))
        {
            let hit: BTreeSet<String> = missing_names(line)
                .into_iter()
                .filter(known)
                .map(String::from)
                .collect();
            if hit.is_empty() {
                out.unattributed.push(line.trim_end().to_string());
            } else {
                out.by_crate.entry(crate_of(line)).or_default().extend(hit);
            }
        }
        out
    }

    /// Every distinct name, across crates.
    #[must_use]
    pub fn names(&self) -> BTreeSet<String> {
        self.by_crate.values().flatten().cloned().collect()
    }

    /// The report: crates by how much they miss, then the unattributed lines.
    #[must_use]
    pub fn report(&self, cell: &str) -> String {
        let mut crates: Vec<(&String, usize)> =
            self.by_crate.iter().map(|(c, n)| (c, n.len())).collect();
        crates.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
        let mut out: Vec<String> = crates.iter().map(|(c, n)| format!("{c:<42} {n}")).collect();
        let names: Vec<String> = self.names().into_iter().collect();
        out.push(format!(
            "DISTINCT MISSING ({cell}): {}: {}",
            names.len(),
            names.join(" ")
        ));
        out.push(format!(
            "--- UNATTRIBUTED ERROR LINES ({}):",
            self.unattributed.len()
        ));
        out.extend(
            self.unattributed
                .iter()
                .take(25)
                .map(|l| format!("    {}", l.chars().take(220).collect::<String>())),
        );
        out.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_error_shape_yields_its_name() {
        let cases: [(&str, &[&str]); 9] = [
            (
                "error[E0425]: cannot find value `AF_INET` in crate `libc`",
                &["AF_INET"],
            ),
            (
                "error[E0412]: cannot find type `stat` in this scope",
                &["stat"],
            ),
            (
                "error[E0425]: cannot find function, tuple struct or tuple variant `f` in crate `libc`",
                &[],
            ),
            (
                "error[E0433]: cannot find struct, variant or union type `kevent` in crate `libc`",
                &["kevent"],
            ),
            (
                "error[E0432]: unresolved import `libc::getpid`",
                &["getpid"],
            ),
            ("error[E0432]: unresolved import `libc::Foo::bar`", &[]),
            ("error[E0425]: no `SIGWINCH` in the root", &["SIGWINCH"]),
            ("error: no `X` here", &[]),
            (
                "   = note: `libc::sysctl` and `c::sysctlbyname` are not found",
                &["sysctl", "sysctlbyname"],
            ),
        ];
        for (line, want) in cases {
            let want: BTreeSet<&str> = want.iter().copied().collect();
            assert_eq!(missing_names(line), want, "{line}");
        }
    }

    #[test]
    fn a_line_is_attributed_to_its_crate() {
        assert_eq!(
            crate_of(
                "/h/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/rustix-1.1.4/src/fs.rs:3:5: error"
            ),
            "rustix-1.1.4"
        );
        assert_eq!(
            crate_of("crates/aterm-gui/src/app.rs:1:1: error[E0425]"),
            "FIRST-PARTY aterm-gui"
        );
        assert_eq!(
            crate_of("/w/vendor/astream/src/x.rs:1:1: error"),
            "FIRST-PARTY astream"
        );
        assert_eq!(crate_of("error[E0425]: cannot find value"), "error[E0425]");
    }

    /// Only names the reference exports (or a primitive) count; the rest are
    /// unattributed, so a loose match never becomes an obligation.
    #[test]
    fn the_reference_filters_what_a_log_names() {
        let log = "\
crates/a/src/x.rs:1:1: error[E0425]: cannot find value `AF_INET` in crate `libc`
crates/a/src/x.rs:2:1: error[E0412]: cannot find type `c_int` in crate `libc`
crates/b/src/y.rs:1:1: error[E0425]: cannot find value `NOT_IN_LIBC` in crate `libc`
crates/b/src/y.rs:1:1: warning: unused import `libc::AF_UNIX`
";
        let harvest = Harvest::scan(log, &|name| name == "AF_INET");
        let names: Vec<String> = harvest.names().into_iter().collect();
        assert_eq!(names, ["AF_INET", "c_int"]);
        assert_eq!(
            harvest.by_crate.keys().collect::<Vec<_>>(),
            ["FIRST-PARTY a"]
        );
        assert_eq!(
            harvest.unattributed.len(),
            1,
            "NOT_IN_LIBC; a warning is no error line"
        );
        assert!(
            harvest
                .report("mac")
                .contains("DISTINCT MISSING (mac): 2: AF_INET c_int")
        );
    }
}
