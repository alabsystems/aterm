// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! generated-seal: a generated file changes only when its generator reruns.
//!
//! `.gitattributes` names the generated files (`linguist-generated`, which GitHub also folds
//! in a diff and `git check-attr` reports), and each one carries its generator's seal
//! ([`crate::seal`]): a line naming the generator and a digest of the file as the generator
//! wrote it. A hand edit, such as a comment prune, breaks the seal; the next run of the
//! generator would revert it, and in the meantime the edit can cut something the file means
//! (the libc oracle's sibling marker, a cell's `!! … ABSENT` lines). The rule needs no
//! generator to run, so it holds a generator that takes a network or a cross toolchain too.
//!
//! Three findings: a declared file without a seal or whose seal no longer holds; a
//! `linguist-generated` row that names no file (a moved output); and a Rust file that seals
//! itself but is not declared, so a prune that took its whole header would go unseen.

use std::borrow::Cow;
use std::collections::BTreeMap;

use super::{Finding, Rule, Tree};
use crate::seal::{self, HEAD_LINES, MARK, Seal};

pub(super) const GENERATED_SEAL: Rule = Rule {
    id: "generated-seal",
    why: "a generated file is changed by rerunning its generator, never by hand: the next run \
          reverts a hand edit (a comment prune), and the edit can cut what the file means; \
          every file .gitattributes marks linguist-generated carries its generator's seal, \
          and the seal holds",
    scope: &[],
    check: generated_seal,
};

/// The file every declaration is read from.
const ATTRIBUTES: &str = ".gitattributes";

/// The attribute that declares a file generated.
const ATTRIBUTE: &str = "linguist-generated";

fn generated_seal(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    let rows = text(tree, ATTRIBUTES)
        .map(|t| declarations(&t))
        .unwrap_or_default();
    let mut declared = vec![false; tree.corpus.paths.len()];
    for row in &rows {
        let mut named = false;
        for (at, rel) in tree.corpus.paths.iter().enumerate() {
            if matches(&row.pattern, rel) {
                declared[at] = row.set;
                named = true;
            }
        }
        if row.set && !named {
            out.push(Finding {
                rule: GENERATED_SEAL.id,
                path: ATTRIBUTES.to_string(),
                line: row.line,
                detail: format!(
                    "`{}` marks no file {ATTRIBUTE}: the generator moved its output, so point \
                     the row at where it writes now",
                    row.pattern
                ),
            });
        }
    }
    let held: BTreeMap<&str, &str> = tree
        .corpus
        .under(&[""])
        .map(|f| (f.rel.as_str(), f.raw.as_str()))
        .collect();
    for (rel, declared) in tree.corpus.paths.iter().zip(declared) {
        let rust = rel.ends_with(".rs") && !rel.starts_with("vendor/");
        if !declared && !rust {
            continue;
        }
        let body = match held.get(rel.as_str()) {
            Some(raw) => Cow::Borrowed(*raw),
            None => match text(tree, rel) {
                Some(body) => body,
                None => continue,
            },
        };
        let finding = |line: usize, detail: String| Finding {
            rule: GENERATED_SEAL.id,
            path: rel.clone(),
            line,
            detail,
        };
        match seal::check(&body) {
            Seal::Absent if declared => out.push(finding(
                1,
                format!(
                    "{ATTRIBUTES} marks it {ATTRIBUTE}, but no `{MARK} … seal` line sits in \
                     its first {HEAD_LINES} lines: its generator writes one \
                     (aterm_census::seal::line) and stamps it (seal::stamp)"
                ),
            )),
            Seal::Absent => {}
            Seal::Broken { line, sealed, now } => out.push(finding(
                line,
                format!(
                    "edited since its generator wrote it (sealed {sealed}, now {now}): rerun \
                     the generator this line names, or revert the edit"
                ),
            )),
            Seal::Holds { line } if !declared => out.push(finding(
                line,
                format!(
                    "seals itself, but {ATTRIBUTES} does not mark it {ATTRIBUTE}: declare it, \
                     so a prune of its whole header is seen"
                ),
            )),
            Seal::Holds { .. } => {}
        }
    }
}

/// One `.gitattributes` row that sets or unsets [`ATTRIBUTE`].
struct Declaration {
    pattern: String,
    set: bool,
    /// 1-based.
    line: usize,
}

/// The rows of `attributes` that name [`ATTRIBUTE`], in file order (a later row wins).
fn declarations(attributes: &str) -> Vec<Declaration> {
    let mut out = Vec::new();
    for (index, row) in attributes.lines().enumerate() {
        let mut words = row.split_whitespace();
        let Some(pattern) = words.next().filter(|p| !p.starts_with('#')) else {
            continue;
        };
        for word in words {
            let set = match word {
                ATTRIBUTE => true,
                _ if word.strip_prefix(ATTRIBUTE) == Some("=true") => true,
                _ if word.strip_prefix(['-', '!']) == Some(ATTRIBUTE) => false,
                _ if word.strip_prefix(ATTRIBUTE) == Some("=false") => false,
                _ => continue,
            };
            out.push(Declaration {
                pattern: pattern.to_string(),
                set,
                line: index + 1,
            });
        }
    }
    out
}

/// Does the `.gitattributes` pattern `pattern` match the root-relative path `rel`? A pattern
/// with no `/` matches the last component at any depth; one with a `/` matches from the root,
/// where `**` spans any number of components and `*` and `?` stay within one.
fn matches(pattern: &str, rel: &str) -> bool {
    let pattern = pattern.strip_prefix('/').unwrap_or(pattern);
    if !pattern.contains('/') {
        return rel
            .rsplit('/')
            .next()
            .is_some_and(|name| component(pattern, name));
    }
    let want: Vec<&str> = pattern.split('/').collect();
    let have: Vec<&str> = rel.split('/').collect();
    components(&want, &have)
}

fn components(want: &[&str], have: &[&str]) -> bool {
    match (want.split_first(), have.split_first()) {
        (None, None) => true,
        (Some((&"**", rest)), _) => {
            components(rest, have)
                || have
                    .split_first()
                    .is_some_and(|(_, tail)| components(want, tail))
        }
        (Some((w, rest)), Some((h, tail))) => component(w, h) && components(rest, tail),
        _ => false,
    }
}

/// `*` and `?` glob match of one path component.
fn component(want: &str, have: &str) -> bool {
    fn go(want: &[u8], have: &[u8]) -> bool {
        match want.split_first() {
            None => have.is_empty(),
            Some((b'*', rest)) => (0..=have.len()).any(|skip| go(rest, &have[skip..])),
            Some((b'?', rest)) => !have.is_empty() && go(rest, &have[1..]),
            Some((w, rest)) => have.first() == Some(w) && go(rest, &have[1..]),
        }
    }
    go(want.as_bytes(), have.as_bytes())
}

/// The text at root-relative `rel`: the corpus's copy when it holds one, else the file's.
fn text<'t>(tree: &'t Tree<'_>, rel: &str) -> Option<Cow<'t, str>> {
    if let Some(file) = tree.corpus.get(rel) {
        return Some(Cow::Borrowed(file.raw.as_str()));
    }
    if let Some(script) = tree.corpus.scripts.iter().find(|s| s.rel == rel) {
        return Some(Cow::Borrowed(script.raw.as_str()));
    }
    std::fs::read_to_string(tree.root.join(rel))
        .ok()
        .map(Cow::Owned)
}

#[cfg(test)]
mod tests {
    use super::super::tests::{run, spots};
    use super::*;

    const ATTRS: &str = "*.rs text eol=lf\n\
                         # the libc cells\n\
                         libc-oracle/conformance/src/cell_*.rs linguist-generated\n\
                         crates/a/src/tables/** linguist-generated=true\n\
                         crates/a/src/tables/hand.rs -linguist-generated\n";

    /// What a generator writes: its header, its seal, a comment and a table.
    fn generated(generator: &str) -> String {
        seal::stamp(&format!(
            "// Copyright 2026 Andrew Yates\n// SPDX-License-Identifier: Apache-2.0\n{}\n\n\
             // !! `sigqueue` ABSENT on this cell.\nconst _: () = ();\n",
            seal::line("//", generator)
        ))
        .expect("the head carries a seal line")
    }

    #[test]
    fn a_declared_file_as_its_generator_wrote_it_passes() {
        let cell = generated("`aterm-libc-oracle regen`");
        let table = generated("`gen tables`");
        let found = run(
            &GENERATED_SEAL,
            &[
                (ATTRIBUTES, ATTRS),
                ("libc-oracle/conformance/src/cell_mac.rs", &cell),
                ("crates/a/src/tables/width/l1.rs", &table),
                ("crates/a/src/tables/hand.rs", "fn hand() {}\n"),
                ("crates/a/src/lib.rs", "mod tables;\n"),
            ],
        );
        assert_eq!(spots(&found), Vec::<String>::new(), "{found:?}");
    }

    #[test]
    fn a_pruned_or_unsealed_declared_file_fails() {
        let cell = generated("`aterm-libc-oracle regen`");
        let pruned = cell.replace("// !! `sigqueue` ABSENT on this cell.\n", "");
        let reheaded = cell.replace("`aterm-libc-oracle regen`", "regen");
        let headless = cell
            .lines()
            .filter(|l| !l.contains(MARK))
            .collect::<Vec<_>>()
            .join("\n");
        let found = run(
            &GENERATED_SEAL,
            &[
                (ATTRIBUTES, ATTRS),
                ("libc-oracle/conformance/src/cell_mac.rs", &pruned),
                ("libc-oracle/conformance/src/cell_macx.rs", &reheaded),
                ("libc-oracle/conformance/src/cell_linux.rs", &headless),
                ("crates/a/src/tables/width.rs", &cell),
            ],
        );
        assert_eq!(
            spots(&found),
            [
                "libc-oracle/conformance/src/cell_linux.rs:1",
                "libc-oracle/conformance/src/cell_mac.rs:3",
                "libc-oracle/conformance/src/cell_macx.rs:3",
            ],
            "{found:?}"
        );
    }

    #[test]
    fn a_row_naming_no_file_and_an_undeclared_seal_fail() {
        let table = generated("`gen tables`");
        let found = run(
            &GENERATED_SEAL,
            &[
                (ATTRIBUTES, ATTRS),
                ("crates/b/src/tables.rs", &table),
                ("vendor/x/src/lib.rs", &table),
            ],
        );
        assert_eq!(
            spots(&found),
            [
                ".gitattributes:3",
                ".gitattributes:4",
                "crates/b/src/tables.rs:3"
            ],
            "{found:?}"
        );
    }

    #[test]
    fn patterns_match_as_git_matches_them() {
        assert!(matches(
            "cell_*.rs",
            "libc-oracle/conformance/src/cell_mac.rs"
        ));
        assert!(matches("/crates/a/src/x.rs", "crates/a/src/x.rs"));
        assert!(!matches("crates/a/src/x.rs", "z/crates/a/src/x.rs"));
        assert!(matches("crates/**/gen.rs", "crates/gen.rs"));
        assert!(matches("crates/**/gen.rs", "crates/a/b/gen.rs"));
        assert!(matches("crates/a/**", "crates/a/b/c.rs"));
        assert!(!matches("crates/a/*.rs", "crates/a/b/c.rs"));
        assert!(matches("crates/a/cell_?.rs", "crates/a/cell_x.rs"));
        assert!(!matches("crates/a/cell_?.rs", "crates/a/cell_xy.rs"));
    }
}
