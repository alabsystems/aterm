// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Conservative dependency-qualified calls for the lazy-init walk.

use super::{DependencyScopes, WordSet, method_fn, owned_fn, source_crate};
use std::collections::{BTreeMap, BTreeSet};

type Exports = BTreeMap<String, BTreeSet<String>>;

/// A proved Cargo alias, unshadowed in this source file, can select a known free export.
pub(super) struct CallScope<'a> {
    aliases: BTreeMap<String, String>,
    exports: &'a Exports,
}

impl<'a> CallScope<'a> {
    /// `code` is `text`'s lines, each [`crate::strip_line_comment`] of it.
    pub(super) fn new(
        file: &str,
        text: &str,
        code: &[&str],
        scopes: &DependencyScopes,
        exports: &'a Exports,
    ) -> Self {
        let candidates: Vec<(&String, &String)> = source_crate(file)
            .and_then(|dir| scopes.get(dir))
            .into_iter()
            .flat_map(|aliases| aliases.iter())
            .filter_map(|(alias, targets)| {
                let target = targets.first()?;
                (targets.len() == 1 && !target.is_empty()).then_some((alias, target))
            })
            .collect();
        if candidates.is_empty() {
            return Self {
                aliases: BTreeMap::new(),
                exports,
            };
        }
        let words = WordSet::new(
            candidates
                .iter()
                .map(|(alias, _)| alias.as_str())
                .filter(|alias| word(alias)),
        );
        let shadows = Shadows::of(code, &words);
        let aliases = candidates
            .into_iter()
            .filter(|(alias, _)| shadows.unshadowed(text, alias))
            .map(|(alias, target)| (alias.clone(), target.clone()))
            .collect();
        Self { aliases, exports }
    }

    /// Dot dispatch stays in the method domain, including when its receiver type
    /// is unknown. Unproved associated/module paths retain both domains.
    fn targets(&self, before_open: &str) -> Vec<String> {
        let Some(before) = direct_callee(before_open) else {
            return Vec::new();
        };
        let Some(name) = crate::ident_ending_at(before, before.len()) else {
            return Vec::new();
        };
        let head = before[..before.len() - name.len()].trim_end();
        if head.ends_with('.') && !head.ends_with("..") {
            return vec![method_fn(name)];
        }
        let Some(prefix) = head.strip_suffix("::") else {
            return vec![name.to_owned()];
        };
        let prefix = prefix.trim_end();
        let Some(alias) = crate::ident_ending_at(prefix, prefix.len()) else {
            return vec![name.to_owned(), method_fn(name)];
        };
        let earlier = prefix[..prefix.len() - alias.len()].trim_end();
        if alias == "Self" && !earlier.ends_with([':', '.', '$']) {
            return vec![method_fn(name)];
        }
        if !earlier.ends_with([':', '.', '$'])
            && let Some(dir) = self.aliases.get(alias)
            && self
                .exports
                .get(dir)
                .is_some_and(|names| names.contains(name))
        {
            return vec![owned_fn(dir, name)];
        }
        vec![name.to_owned(), method_fn(name)]
    }

    pub(super) fn dependency(&self, alias: &str) -> Option<&str> {
        self.aliases.get(alias).map(String::as_str)
    }

    pub(super) fn callees(&self, body: &[String]) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        self.line_callees(&body.join("\n"), &mut out);
        out
    }

    /// Direct call targets in lexical code, including multiline path grouping.
    pub(super) fn line_callees(&self, line: &str, out: &mut BTreeSet<String>) {
        for (at, byte) in line.bytes().enumerate() {
            if byte == b'(' {
                let before = &line[..at];
                // The declaration names an item, not a free-function call from
                // its body. Real calls later on the same line remain edges.
                let trimmed = before.trim_end();
                if crate::ident_ending_at(trimmed, trimmed.len()).is_some_and(|name| {
                    let head = trimmed[..trimmed.len() - name.len()].trim_end();
                    crate::ident_ending_at(head, head.len()) == Some("fn")
                }) {
                    continue;
                }
                out.extend(self.targets(before));
            }
        }
    }

    pub(super) fn reference(&self, text: &str) -> Vec<String> {
        self.targets(text)
    }
}

/// Parentheses are transparent only around a complete direct path. A call's
/// argument list, tuple, cast, or computed result does not become a path.
fn direct_callee(text: &str) -> Option<&str> {
    let text = text.trim_end();
    if !text.ends_with(')') {
        return Some(text);
    }
    let mut depth = 0usize;
    let mut opening = None;
    for (at, byte) in text.bytes().enumerate().rev() {
        match byte {
            b')' => depth += 1,
            b'(' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    opening = Some(at);
                    break;
                }
            }
            _ => {}
        }
    }
    let at = opening?;
    let head = text[..at].trim_end();
    if callable_head(head) {
        return None;
    }
    let mut inner = text[at + 1..text.len() - 1].trim();
    while inner.starts_with('(') && inner.ends_with(')') {
        inner = &inner[1..inner.len() - 1];
        inner = inner.trim();
    }
    let mut rest = inner.strip_prefix("::").unwrap_or(inner).trim_start();
    loop {
        let end = rest
            .bytes()
            .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_')
            .count();
        let name = &rest[..end];
        if name.is_empty() || name.as_bytes()[0].is_ascii_digit() {
            return None;
        }
        rest = rest[end..].trim_start();
        if rest.is_empty() {
            return Some(inner);
        }
        rest = rest.strip_prefix("::")?.trim_start();
    }
}

/// A preceding callable path or computed value makes this pair an argument
/// list, rather than transparent grouping. Expression keywords and a labeled
/// `break` introduce values; unary `!` does not turn either into a macro path.
fn callable_head(mut head: &str) -> bool {
    while let Some(prefix) = head.strip_suffix('!') {
        head = prefix.trim_end();
    }
    if head.ends_with([')', ']', '?']) {
        return true;
    }
    // This is only argument-list exclusion, not a new generic-call target.
    // A comparison `>` alone does not establish a callable prefix.
    if head.ends_with('>')
        && head.match_indices('<').any(|(at, _)| {
            head[..at].trim_end().ends_with("::")
                && crate::lock_order::matching_angle(&head[at..]) == Some(head.len() - at - 1)
        })
    {
        return true;
    }
    let Some(word) = crate::ident_ending_at(head, head.len()) else {
        return false;
    };
    let before = head[..head.len() - word.len()].trim_end();
    if matches!(
        word,
        "return" | "break" | "yield" | "if" | "while" | "match" | "in"
    ) && !before.ends_with([':', '.', '#'])
    {
        return false;
    }
    if let Some(before_label) = before.strip_suffix('\'') {
        let before_label = before_label.trim_end();
        if crate::ident_ending_at(before_label, before_label.len()) == Some("break") {
            return false;
        }
    }
    true
}

fn ident(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// Every char is one the whole-word test in [`unshadowed`] treats as part of a word.
fn word(alias: &str) -> bool {
    !alias.is_empty() && alias.chars().all(|c| c.is_alphanumeric() || c == '_')
}

/// What [`unshadowed`] reads from one file, read in one pass for all of its aliases.
///
/// A glob or unterminated `use` shadows every alias. For a [`word`] alias, a whole-word
/// occurrence is exactly a maximal run of word chars equal to it (no earlier occurrence can
/// overlap a run's start, because the char before the run is not one of the alias's chars),
/// so one tokenization answers every alias's mention test. Any other alias is judged by
/// [`unshadowed`] itself.
struct Shadows<'t> {
    /// A terminated `use` holds `*`, or the file ends inside a `use`.
    every: bool,
    /// The terminated `use` statements that name `self`, as [`unshadowed`] accumulates them.
    self_uses: Vec<String>,
    /// [`word`] aliases with a whole-word occurrence that is not a `name::` path head, or that
    /// a `:` or `$` precedes.
    misused: BTreeSet<&'t str>,
}

impl<'t> Shadows<'t> {
    /// `lines` is the file's lines, each [`crate::strip_line_comment`] of it.
    fn of(lines: &[&'t str], words: &WordSet<'_>) -> Self {
        let mut out = Self {
            every: false,
            self_uses: Vec::new(),
            misused: BTreeSet::new(),
        };
        let mut use_text = String::new();
        for &code in lines {
            if !use_text.is_empty() || code.contains("use ") {
                use_text.push_str(code);
                use_text.push(' ');
                if use_text.contains(';') {
                    if use_text.contains('*') {
                        out.every = true;
                        return out;
                    }
                    if super::mentions_ident(&use_text, "self") {
                        out.self_uses.push(std::mem::take(&mut use_text));
                    } else {
                        use_text.clear();
                    }
                }
            }
            if !words.is_empty() {
                misused_words(code, words, &mut out.misused);
            }
        }
        out.every = !use_text.is_empty();
        out
    }

    fn unshadowed(&self, text: &str, alias: &str) -> bool {
        if self.every {
            return false;
        }
        if !word(alias) {
            return unshadowed(text, alias);
        }
        !self.misused.contains(alias)
            && !self
                .self_uses
                .iter()
                .any(|statement| super::mentions_ident(statement, alias))
    }
}

/// Each maximal run of word chars in `code` that is one of `words` and is not a `name::`
/// path head, or that a `:` or `$` precedes, joins `misused`.
fn misused_words<'t>(code: &'t str, words: &WordSet<'_>, misused: &mut BTreeSet<&'t str>) {
    let mut start = None;
    let mut at = 0;
    loop {
        // The char at `at` (past the end, none), read byte-wise while it is ASCII.
        let (in_word, width) = match code.as_bytes().get(at) {
            None => (false, 0),
            Some(&byte) if byte.is_ascii() => (byte.is_ascii_alphanumeric() || byte == b'_', 1),
            Some(_) => code[at..]
                .chars()
                .next()
                .map_or((false, 1), |c| (c.is_alphanumeric(), c.len_utf8())),
        };
        match (start, in_word) {
            (None, true) => start = Some(at),
            (Some(from), false) => {
                start = None;
                let run = &code[from..at];
                if words.contains(run)
                    && !misused.contains(run)
                    && (!code[at..].trim_start().starts_with("::")
                        || code[..from].trim_end().ends_with([':', '$']))
                {
                    misused.insert(run);
                }
            }
            _ => {}
        }
        if width == 0 {
            return;
        }
        at += width;
    }
}

/// File-wide uncertainty deliberately prevents narrowing: declarations, imports, fn/impl
/// generics, and globs may shadow an extern-prelude name. This is not a compiler resolver.
fn unshadowed(text: &str, alias: &str) -> bool {
    let mut use_text = String::new();
    for raw in text.lines() {
        let code = crate::strip_line_comment(raw);
        if !use_text.is_empty() || code.contains("use ") {
            use_text.push_str(code);
            use_text.push(' ');
            if use_text.contains(';') {
                if use_text.contains('*')
                    || (super::mentions_ident(&use_text, alias)
                        && super::mentions_ident(&use_text, "self"))
                {
                    return false;
                }
                use_text.clear();
            }
        }
        for (at, _) in code.match_indices(alias) {
            let left = &code[..at];
            let right = &code[at + alias.len()..];
            if left.ends_with(|c: char| c.is_alphanumeric() || c == '_')
                || right.starts_with(|c: char| c.is_alphanumeric() || c == '_')
            {
                continue;
            }
            if !right.trim_start().starts_with("::") || left.trim_end().ends_with([':', '$']) {
                return false;
            }
        }
    }
    use_text.is_empty()
}

#[derive(Default)]
struct RootItems {
    free: BTreeSet<String>,
    modules: BTreeSet<String>,
    exports: Vec<String>,
}

/// Prove only ordinary formatted root declarations. A blocking surface anywhere in the
/// file keeps conservative reach: a parameter or a same-file helper may name the caller's
/// cell differently. Comments, multiline literals and custom paths remain unproved too.
fn root_items(text: &str) -> Option<RootItems> {
    if text.contains("r#\"")
        || text.contains("r\"")
        || text.contains("#[path")
        || text.contains("/*")
        || super::BLOCKING_METHODS
            .iter()
            .any(|method| super::mentions_ident(text, method))
    {
        return None;
    }
    let mut out = RootItems::default();
    let mut depth = 0i32;
    let mut export = String::new();
    for raw in text.lines() {
        let line = crate::lock_order::mask_literals(crate::strip_line_comment(raw));
        if line.bytes().filter(|byte| *byte == b'"').count() % 2 != 0 {
            return None;
        }
        if depth == 0
            && line.starts_with("pub ")
            && let Some((_, name)) = crate::parse_fn_def(&line)
        {
            out.free.insert(name);
        }
        if depth == 0 {
            let module = line
                .strip_prefix("pub mod ")
                .or_else(|| line.strip_prefix("mod "));
            if let Some(name) = module
                .and_then(|s| s.trim().strip_suffix(';'))
                .filter(|s| ident(s))
            {
                out.modules.insert(name.to_owned());
            }
        }
        if !export.is_empty() || (depth == 0 && line.starts_with("pub use ")) {
            export.push_str(&line);
            export.push(' ');
            if export.contains(';') {
                out.exports.push(std::mem::take(&mut export));
            }
        }
        depth += line.matches('{').count() as i32 - line.matches('}').count() as i32;
    }
    (depth == 0 && export.is_empty()).then_some(out)
}

/// Public free functions at the default library root or explicitly re-exported from one
/// declared same-crate module. Unsupported exports fall back to the old conservative walk.
pub(super) fn known_free_exports(sources: &[(String, String)]) -> Exports {
    let files: BTreeMap<&str, &str> = sources
        .iter()
        .map(|(p, s)| (p.as_str(), s.as_str()))
        .collect();
    let mut exports = BTreeMap::new();
    for (file, text) in sources {
        let Some(dir) = file.strip_suffix("/src/lib.rs") else {
            continue;
        };
        let Some(root) = root_items(text) else {
            continue;
        };
        let mut known = root.free;
        let mut uncertain = BTreeSet::new();
        let mut glob = false;
        for statement in root.exports {
            if statement.contains('*') {
                glob = true;
                break;
            }
            let body = statement
                .trim()
                .strip_prefix("pub use ")
                .unwrap_or("")
                .trim_end_matches(';')
                .trim();
            let Some((module, leaves)) = body.split_once("::") else {
                continue;
            };
            let leaves = leaves.trim().trim_start_matches('{').trim_end_matches('}');
            let module = module.trim();
            let target = [
                format!("{dir}/src/{module}.rs"),
                format!("{dir}/src/{module}/mod.rs"),
            ];
            let candidates: Vec<_> = target
                .iter()
                .filter_map(|path| files.get(path.as_str()))
                .collect();
            let local = if root.modules.contains(module) && candidates.len() == 1 {
                root_items(candidates[0]).map(|items| items.free)
            } else {
                None
            };
            for leaf in leaves.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                let named = leaf.split(" as ").last().unwrap_or(leaf).trim();
                if !ident(named) {
                    glob = true;
                    break;
                }
                if leaf == named && local.as_ref().is_some_and(|fns| fns.contains(named)) {
                    known.insert(named.to_owned());
                } else {
                    uncertain.insert(named.to_owned());
                }
            }
        }
        if !glob {
            known.retain(|name| !uncertain.contains(name));
            exports.insert(dir.to_owned(), known);
        }
    }
    exports
}

#[cfg(test)]
#[path = "lazy_init_calls_tests.rs"]
mod tests;
