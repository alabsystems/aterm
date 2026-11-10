// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Conservative dependency-qualified calls for the lazy-init walk.

use super::{DependencyScopes, WordSet, owned_fn, source_crate};
use std::borrow::Cow;
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

    /// Only a two-segment external free call is narrowed. Everything unproved stays by-name.
    fn target<'l>(&self, before_open: &'l str) -> Option<Cow<'l, str>> {
        let before = before_open.trim_end();
        let name = crate::ident_ending_at(before, before.len())?;
        let bare = || Cow::Borrowed(name);
        let Some(prefix) = before[..before.len() - name.len()]
            .trim_end()
            .strip_suffix("::")
        else {
            return Some(bare());
        };
        let prefix = prefix.trim_end();
        let Some(alias) = crate::ident_ending_at(prefix, prefix.len()) else {
            return Some(bare());
        };
        let earlier = prefix[..prefix.len() - alias.len()].trim_end();
        if earlier.ends_with([':', '.', '$']) {
            return Some(bare());
        }
        let Some(dir) = self.aliases.get(alias) else {
            return Some(bare());
        };
        Some(
            if self
                .exports
                .get(dir)
                .is_some_and(|names| names.contains(name))
            {
                Cow::Owned(owned_fn(dir, name))
            } else {
                bare()
            },
        )
    }

    pub(super) fn callees(&self, body: &[String]) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        for line in body {
            self.line_callees(line, &mut out);
        }
        out
    }

    /// Every `<ident>(` target in one line, added to `out`.
    pub(super) fn line_callees(&self, line: &str, out: &mut BTreeSet<String>) {
        for (at, byte) in line.bytes().enumerate() {
            if byte == b'('
                && let Some(target) = self.target(&line[..at])
                && !out.contains(&*target)
            {
                out.insert(target.into_owned());
            }
        }
    }

    pub(super) fn reference(&self, text: &str) -> Option<String> {
        self.target(text).map(Cow::into_owned)
    }
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
mod tests {
    use super::super::run_lazy_init_census;

    const PURE: &str = "pub fn read(_cache: &std::cell::OnceCell<bool>) -> bool { false }\n";
    const REENTER: &str = "pub fn read(cache: &std::cell::OnceCell<bool>) -> bool {\n    *cache.get_or_init(|| true)\n}\n";

    fn check(
        label: &str,
        prelude: &str,
        expression: &str,
        provider: &str,
        extra: &[(&str, &str)],
        red: bool,
    ) {
        let mut files = vec![
            ("Cargo.toml".into(), "[workspace]\nmembers = [\"crates/aterm-gui\", \"crates/parser-impl\"]\n".into()),
            ("crates/aterm-gui/Cargo.toml".into(), "[package]\nname = \"aterm-gui\"\n[dependencies]\nparser = { package = \"parser-implementation\", path = \"../parser-impl\" }\n".into()),
            ("crates/parser-impl/Cargo.toml".into(), "[package]\nname = \"parser-implementation\"\n".into()),
            ("crates/aterm-gui/src/lib.rs".into(), format!("{prelude}\nstruct Cache {{\n    cache: std::cell::OnceCell<bool>,\n}}\nimpl Cache {{\n    fn read(&self) -> bool {{\n        *self.cache.get_or_init({expression})\n    }}\n}}\n")),
            ("crates/parser-impl/src/lib.rs".into(), provider.into()),
        ];
        files.extend(
            extra
                .iter()
                .map(|(path, body)| ((*path).into(), (*body).into())),
        );
        let tree =
            crate::scan_set::test_fixtures::synth_tree("lazy-qualified-controls", label, &files);
        let out = run_lazy_init_census(&tree);
        assert_eq!(out.ok, !red, "{label}: {}", out.log);
    }

    #[test]
    fn qualified_calls_preserve_real_and_conservative_reentry() {
        for (label, prelude, expression) in [
            ("crate", "", "|| crate::read(self)"),
            ("self", "", "|| self::read(self)"),
            ("Self", "", "|| Self::read(self)"),
            ("unknown", "", "|| unknown::read(self)"),
            (
                "module",
                "mod parser {\n    pub fn read(cache: &super::Cache) -> bool { cache.read() }\n}",
                "|| parser::read(self)",
            ),
            (
                "import",
                "use crate::local as parser;\nmod local {\n    pub fn read(cache: &super::Cache) -> bool { cache.read() }\n}",
                "|| parser::read(self)",
            ),
            (
                "type",
                "type parser = Local;\nstruct Local;\nimpl Local {\n    fn read(cache: &Cache) -> bool { cache.read() }\n}",
                "|| parser::read(self)",
            ),
            (
                "fn-generic",
                "fn forward<parser: Capture>(cache: &Cache) -> bool {\n    parser::read(cache)\n}\ntrait Capture {}",
                "|| parser::read(self)",
            ),
            (
                "impl-generic",
                "struct Forward<T>(T);\nimpl<parser: Capture> Forward<parser> {\n    fn call(cache: &Cache) -> bool { parser::read(cache) }\n}\ntrait Capture {}",
                "|| parser::read(self)",
            ),
            (
                "mixed-local-external",
                "",
                "|| { parser::read(&self.cache); self.read() }",
            ),
            ("deep-path", "", "|| parser::nested::read(self)"),
        ] {
            check(label, prelude, expression, PURE, &[], true);
        }
    }

    #[test]
    fn only_known_public_free_exports_narrow_calls_and_function_references() {
        for (label, provider, red) in [
            ("public", PURE, false),
            ("borrowed-cell", REENTER, true),
            ("private", "fn read() -> bool { false }\n", true),
            ("missing", "pub fn other() -> bool { false }\n", true),
            (
                "foreign-reexport",
                "pub use elsewhere::read;\nfn read() -> bool { false }\n",
                true,
            ),
            ("glob-reexport", "pub use elsewhere::*;\n", true),
        ] {
            check(
                label,
                "",
                "|| parser::read(&self.cache)",
                provider,
                &[],
                red,
            );
        }
        for (label, provider, red) in [
            ("reference-pure", "pub fn read() -> bool { false }\n", false),
            (
                "reference-reenters",
                "pub fn read() -> bool {\n    let cache: &std::cell::OnceCell<bool> = unreachable!();\n    *cache.get_or_init(|| true)\n}\n",
                true,
            ),
        ] {
            check(label, "", "parser::read", provider, &[], red);
        }
        for (label, implementation, red) in [
            ("local-export-pure", PURE, false),
            ("local-export-borrowed", REENTER, true),
        ] {
            check(
                label,
                "",
                "|| parser::read(&self.cache)",
                "mod parsing;\npub use parsing::read;\n",
                &[("crates/parser-impl/src/parsing.rs", implementation)],
                red,
            );
        }
    }

    /// Each way a file shadows an alias, judged by the one-pass [`super::Shadows`] (and, for
    /// an alias that is not one word, by the full scan it falls back to) exactly as the full
    /// scan [`super::unshadowed`] judges it.
    #[test]
    fn every_shadowing_shape_is_judged_per_alias() {
        for (text, alias, unshadowed) in [
            ("fn f() { parser::read(); }\n", "parser", true),
            ("fn f() { parser :: read(); }\n", "parser", true),
            ("fn f() { other::read(); }\n", "parser", true),
            (
                "// parser\nfn f() { parser::read(); } // parser alone\n",
                "parser",
                true,
            ),
            ("fn f() { parsers::read(); parser_x(); }\n", "parser", true),
            ("fn f() { éparser::read(); parseré(); }\n", "parser", true),
            ("use crate::local as parser;\n", "parser", false),
            ("fn f() { x::parser::read(); }\n", "parser", false),
            (
                "macro_rules! m { () => { $parser::read() }; }\n",
                "parser",
                false,
            ),
            ("fn f() { parser é::read(); }\n", "parser", false),
            (
                "use parser::{self, Read};\nfn f() { parser::read(); }\n",
                "parser",
                false,
            ),
            (
                "use parser::{\n    self,\n};\nfn f() { parser::read(); }\n",
                "parser",
                false,
            ),
            (
                "use other::{self};\nfn f() { parser::read(); }\n",
                "parser",
                true,
            ),
            (
                "use other::*;\nfn f() { parser::read(); }\n",
                "parser",
                false,
            ),
            (
                "fn f() { parser::read(); }\nuse other::thing",
                "parser",
                false,
            ),
            ("fn f() { a-b::read(); }\n", "a-b", true),
            ("fn f() { let x = a-b; }\n", "a-b", false),
            ("use a-b::{self};\n", "a-b", false),
        ] {
            let code: Vec<&str> = text.lines().map(crate::strip_line_comment).collect();
            let words = super::WordSet::new([alias].into_iter().filter(|a| super::word(a)));
            let shadows = super::Shadows::of(&code, &words);
            assert_eq!(
                shadows.unshadowed(text, alias),
                unshadowed,
                "{alias:?} in {text:?}"
            );
            assert_eq!(
                super::unshadowed(text, alias),
                unshadowed,
                "{alias:?} in {text:?}, by the full scan"
            );
        }
    }

    #[test]
    fn a_renamed_borrowed_cell_cannot_be_proved_pure() {
        check(
            "renamed-borrow",
            "",
            "|| parser::read(&self.cache)",
            "pub fn read(borrowed: &std::cell::OnceCell<bool>) -> bool {\n    *borrowed.get_or_init(|| true)\n}\n",
            &[],
            true,
        );
    }

    #[test]
    fn a_same_file_helper_cannot_hide_a_borrowed_cell() {
        check(
            "helper-borrow",
            "",
            "|| parser::read(&self.cache)",
            "pub fn read(borrowed: &std::cell::OnceCell<bool>) -> bool {\n    fill(borrowed)\n}\nfn fill(borrowed: &std::cell::OnceCell<bool>) -> bool {\n    *borrowed.get_or_init(|| true)\n}\n",
            &[],
            true,
        );
    }

    #[test]
    fn a_comment_cannot_prove_a_macro_generated_export() {
        check(
            "comment-export",
            "",
            "|| parser::read(&self.cache)",
            "/*\npub fn read() -> bool { false }\n*/\nmake_reader!();\n",
            &[],
            true,
        );
    }

    #[test]
    fn a_multiline_string_cannot_prove_a_macro_generated_export() {
        check(
            "string-export",
            "",
            "|| parser::read(&self.cache)",
            "const GUIDE: &str = \"\npub fn read() -> bool { false }\n\";\nmake_reader!();\n",
            &[],
            true,
        );
    }
}
