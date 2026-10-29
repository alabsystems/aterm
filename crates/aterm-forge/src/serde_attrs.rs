// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `forge serde-attrs`: inventory of every `#[serde(...)]` attribute key and form in the workspace.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;

const VALUE_FORMS: &[&str] = &["rename_all"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Ident,
    Str,
    Lit,
    Punct,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Tok {
    kind: Kind,
    text: String,
    line: usize,
}

impl Tok {
    fn is(&self, kind: Kind, text: &str) -> bool {
        self.kind == kind && self.text == text
    }
}

const fn ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

const fn ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn lex(src: &str) -> Vec<Tok> {
    let b: Vec<char> = src.chars().collect();
    let n = b.len();
    let mut out = Vec::new();
    let (mut i, mut line) = (0usize, 1usize);
    let starts = |at: usize, s: &str| {
        s.chars()
            .enumerate()
            .all(|(k, c)| b.get(at + k) == Some(&c))
    };
    let push = |out: &mut Vec<Tok>, kind, text: String, line| out.push(Tok { kind, text, line });
    while i < n {
        let c = b[i];
        if c == '\n' {
            line += 1;
            i += 1;
        } else if c.is_whitespace() {
            i += 1;
        } else if starts(i, "//") {
            i = (i..n).find(|&j| b[j] == '\n').unwrap_or(n);
        } else if starts(i, "/*") {
            let mut depth = 1;
            i += 2;
            while i < n && depth > 0 {
                if starts(i, "/*") {
                    depth += 1;
                    i += 2;
                } else if starts(i, "*/") {
                    depth -= 1;
                    i += 2;
                } else {
                    line += usize::from(b[i] == '\n');
                    i += 1;
                }
            }
        } else if let Some((raw, hashes, open)) = string_start(&b, i) {
            let start = open + 1;
            let (text_end, end) = if raw {
                let close: Vec<char> = std::iter::once('"')
                    .chain(std::iter::repeat_n('#', hashes))
                    .collect();
                let at = (start..n)
                    .find(|&j| b[j..].starts_with(&close))
                    .unwrap_or(n);
                (at, (at + close.len()).min(n))
            } else {
                let mut j = start;
                while j < n && b[j] != '"' {
                    j += if b[j] == '\\' { 2 } else { 1 };
                }
                (j.min(n), (j + 1).min(n))
            };
            let text: String = b[start..text_end].iter().collect();
            push(&mut out, Kind::Str, text, line);
            line += b[i..end].iter().filter(|&&c| c == '\n').count();
            i = end;
        } else if c == '\'' {
            let end = char_end(&b, i)
                .or_else(|| lifetime_end(&b, i))
                .unwrap_or(i + 1);
            push(&mut out, Kind::Lit, b[i..end].iter().collect(), line);
            i = end;
        } else if ident_start(c) {
            let end = (i..n).find(|&j| !ident_char(b[j])).unwrap_or(n);
            push(&mut out, Kind::Ident, b[i..end].iter().collect(), line);
            i = end;
        } else if c.is_ascii_digit() {
            let end = (i..n)
                .find(|&j| !(b[j].is_ascii_alphanumeric() || b[j] == '_' || b[j] == '.'))
                .unwrap_or(n);
            push(&mut out, Kind::Lit, b[i..end].iter().collect(), line);
            i = end;
        } else {
            push(&mut out, Kind::Punct, c.to_string(), line);
            i += 1;
        }
    }
    out
}

fn string_start(b: &[char], i: usize) -> Option<(bool, usize, usize)> {
    for prefix in ["br", "cr", "r", "b", "c", ""] {
        let p: Vec<char> = prefix.chars().collect();
        if !b[i..].starts_with(&p) {
            continue;
        }
        let at = i + p.len();
        let hashes = b[at..].iter().take_while(|&&c| c == '#').count();
        let raw = prefix.contains('r');
        if b.get(at + hashes) == Some(&'"') {
            return (hashes == 0 || raw).then_some((raw, hashes, at + hashes));
        }
    }
    None
}

fn char_end(b: &[char], i: usize) -> Option<usize> {
    let at = i + 1;
    let first = *b.get(at)?;
    let mut bodies = Vec::new();
    if first == '\\' {
        let next = b.get(at + 1).copied();
        if next == Some('x')
            && b.len() > at + 3
            && b[at + 2..at + 4].iter().all(char::is_ascii_hexdigit)
        {
            bodies.push(4);
        }
        if next == Some('u') && b.get(at + 2) == Some(&'{') {
            let digits = b[at + 3..]
                .iter()
                .take_while(|c| c.is_ascii_hexdigit())
                .count();
            if digits > 0 && b.get(at + 3 + digits) == Some(&'}') {
                bodies.push(digits + 4);
            }
        }
        if next.is_some_and(|c| c != '\n') {
            bodies.push(2);
        }
    } else if first != '\'' {
        bodies.push(1);
    }
    bodies
        .into_iter()
        .find(|&body| b.get(at + body) == Some(&'\''))
        .map(|body| at + body + 1)
}

fn lifetime_end(b: &[char], i: usize) -> Option<usize> {
    if !ident_start(*b.get(i + 1)?) {
        return None;
    }
    Some(
        (i + 1..b.len())
            .find(|&j| !ident_char(b[j]))
            .unwrap_or(b.len()),
    )
}

fn group_end(toks: &[Tok], i: usize) -> usize {
    let close = |t: &str| match t {
        "(" => Some(")"),
        "[" => Some("]"),
        "{" => Some("}"),
        _ => None,
    };
    let mut stack: Vec<&str> = close(&toks[i].text).into_iter().collect();
    let mut i = i + 1;
    while let Some(want) = stack.last().copied() {
        let Some(t) = toks.get(i) else {
            return toks.len();
        };
        if t.kind == Kind::Punct {
            if let Some(c) = close(&t.text) {
                stack.push(c);
            } else if t.text == want {
                stack.pop();
            }
        }
        i += 1;
    }
    i
}

fn split_commas(toks: &[Tok]) -> Vec<&[Tok]> {
    let (mut items, mut start, mut depth) = (Vec::new(), 0usize, 0i32);
    for (k, t) in toks.iter().enumerate() {
        if t.kind == Kind::Punct {
            match t.text.as_str() {
                "(" | "[" | "{" => depth += 1,
                ")" | "]" | "}" => depth -= 1,
                "," if depth == 0 => {
                    if k > start {
                        items.push(&toks[start..k]);
                    }
                    start = k + 1;
                }
                _ => {}
            }
        }
    }
    if toks.len() > start {
        items.push(&toks[start..]);
    }
    items
}

fn call_args<'a>(attr: &'a [Tok], name: &str) -> Option<&'a [Tok]> {
    if attr.first()?.kind != Kind::Ident || attr[0].text != name || attr.get(1)?.text != "(" {
        return None;
    }
    let end = group_end(attr, 1);
    let close = if end > 2 && attr.get(end - 1).is_some_and(|t| t.is(Kind::Punct, ")")) {
        end - 1
    } else {
        end
    };
    Some(&attr[2..close.max(2)])
}

fn serde_items(attr: &[Tok]) -> Vec<(&[Tok], bool)> {
    if let Some(args) = call_args(attr, "serde") {
        return split_commas(args).into_iter().map(|i| (i, false)).collect();
    }
    if let Some(args) = call_args(attr, "cfg_attr") {
        return split_commas(args)
            .into_iter()
            .skip(1)
            .flat_map(|arg| serde_items(arg).into_iter().map(|(i, _)| (i, true)))
            .collect();
    }
    Vec::new()
}

fn derives(attr: &[Tok]) -> Vec<(String, bool)> {
    if let Some(args) = call_args(attr, "derive") {
        return split_commas(args)
            .into_iter()
            .filter_map(|item| {
                let last = &item.last()?.text;
                (last == "Serialize" || last == "Deserialize").then(|| (last.clone(), false))
            })
            .collect();
    }
    if let Some(args) = call_args(attr, "cfg_attr") {
        return split_commas(args)
            .into_iter()
            .skip(1)
            .flat_map(|arg| derives(arg).into_iter().map(|(n, _)| (n, true)))
            .collect();
    }
    Vec::new()
}

fn form_of(item: &[Tok]) -> (String, String) {
    let key = item[0].text.clone();
    if item.len() == 1 {
        return (key, "bare".into());
    }
    if item[1].text == "=" && item.len() == 3 && item[2].kind == Kind::Str {
        if VALUE_FORMS.contains(&key.as_str()) {
            let form = format!("= \"{}\"", item[2].text);
            return (key, form);
        }
        return (key, "= <string>".into());
    }
    if let [_, open, inner @ .., close] = item
        && open.text == "("
        && close.text == ")"
    {
        let parts: Vec<String> = split_commas(inner)
            .into_iter()
            .map(|sub| {
                let (k, f) = form_of(sub);
                format!("{k} {f}")
            })
            .collect();
        return (key, format!("({})", parts.join(", ")));
    }
    let rest: Vec<&str> = item[1..].iter().map(|t| t.text.as_str()).collect();
    (key, format!("?{}", rest.join(" ")))
}

fn test_path(rel: &str) -> bool {
    rel.split('/').any(|part| {
        part.strip_suffix(".rs")
            .unwrap_or(part)
            .split('_')
            .any(|w| w.starts_with("test"))
    })
}

struct Pending {
    key: String,
    form: String,
    via: bool,
    line: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Position {
    Container,
    Variant,
    Field,
}

impl Position {
    const fn name(self) -> &'static str {
        match self {
            Self::Container => "container",
            Self::Variant => "variant",
            Self::Field => "field",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Ctx {
    Enum,
    Fields,
    Other,
}

#[derive(Default)]
struct Inventory {
    rows: BTreeMap<(Position, String, String), Vec<(String, bool)>>,
    combos: BTreeMap<(Position, String), Vec<String>>,
    derive_sites: BTreeMap<(String, String), Vec<(String, bool)>>,
    manual: BTreeMap<String, Vec<String>>,
}

impl Inventory {
    fn record(&mut self, at: Position, pending: &[Pending], rel: &str) {
        let Some(first) = pending.first() else {
            return;
        };
        for p in pending {
            self.rows
                .entry((at, p.key.clone(), p.form.clone()))
                .or_default()
                .push((format!("{rel}:{}", p.line), p.via));
        }
        let keys: BTreeSet<String> = pending
            .iter()
            .map(|p| {
                if p.form == "bare" {
                    p.key.clone()
                } else {
                    format!("{} {}", p.key, p.form)
                }
            })
            .collect();
        let keys: Vec<String> = keys.into_iter().collect();
        self.combos
            .entry((at, keys.join(" + ")))
            .or_default()
            .push(format!("{rel}:{}", first.line));
    }

    fn scan(&mut self, rel: &str, src: &str) {
        let toks = lex(src);
        let n = toks.len();
        let mut ctx: Vec<Ctx> = Vec::new();
        let mut pending: Vec<Pending> = Vec::new();
        let mut pending_derive: Vec<(String, bool, usize)> = Vec::new();
        let mut last_kw: Option<&str> = None;
        let mut i = 0;
        while i < n {
            let t = &toks[i];
            let attr_open = if t.is(Kind::Punct, "#") {
                if toks.get(i + 1).is_some_and(|x| x.is(Kind::Punct, "[")) {
                    Some(i + 1)
                } else if toks.get(i + 1).is_some_and(|x| x.is(Kind::Punct, "!"))
                    && toks.get(i + 2).is_some_and(|x| x.is(Kind::Punct, "["))
                {
                    Some(i + 2)
                } else {
                    None
                }
            } else {
                None
            };
            if let Some(j) = attr_open {
                let end = group_end(&toks, j);
                let close =
                    if end > j + 1 && toks.get(end - 1).is_some_and(|t| t.is(Kind::Punct, "]")) {
                        end - 1
                    } else {
                        end
                    };
                let inner = &toks[(j + 1).min(end)..close.max(j + 1)];
                for (item, via) in serde_items(inner) {
                    let (key, form) = form_of(item);
                    pending.push(Pending {
                        key,
                        form,
                        via,
                        line: item[0].line,
                    });
                }
                pending_derive.extend(
                    derives(inner)
                        .into_iter()
                        .map(|(name, via)| (name, via, t.line)),
                );
                i = end;
                continue;
            }
            if t.is(Kind::Ident, "impl") {
                self.impl_site(&toks, i, rel);
            }
            match (t.kind, t.text.as_str()) {
                (Kind::Ident, kw @ ("struct" | "enum" | "union")) => {
                    last_kw = Some(kw);
                    if !pending.is_empty() || !pending_derive.is_empty() {
                        self.record(Position::Container, &pending, rel);
                        for (name, via, line) in pending_derive.drain(..) {
                            self.derive_sites
                                .entry((name, kw.to_string()))
                                .or_default()
                                .push((format!("{rel}:{line}"), via));
                        }
                        pending.clear();
                    }
                }
                (Kind::Punct, open @ ("{" | "(")) => {
                    let new = if last_kw == Some("struct") {
                        Ctx::Fields
                    } else if last_kw == Some("enum") && open == "{" {
                        Ctx::Enum
                    } else if ctx.last() == Some(&Ctx::Enum)
                        && i.checked_sub(1)
                            .is_some_and(|p| toks[p].kind == Kind::Ident)
                    {
                        Ctx::Fields
                    } else {
                        Ctx::Other
                    };
                    ctx.push(new);
                    last_kw = None;
                }
                (Kind::Punct, "}" | ")") => {
                    ctx.pop();
                    last_kw = None;
                }
                (Kind::Punct, ";") => {
                    last_kw = None;
                    pending_derive.clear();
                }
                (Kind::Ident, word)
                    if !pending.is_empty()
                        && !matches!(word, "pub" | "crate" | "in" | "self" | "super") =>
                {
                    let at = match ctx.last() {
                        Some(Ctx::Enum) => Some(Position::Variant),
                        Some(Ctx::Fields) => Some(Position::Field),
                        _ => None,
                    };
                    if let Some(at) = at {
                        self.record(at, &pending, rel);
                        pending.clear();
                    }
                }
                _ => {}
            }
            i += 1;
        }
    }

    fn impl_site(&mut self, toks: &[Tok], i: usize, rel: &str) {
        let n = toks.len();
        let mut k = i + 1;
        if toks.get(k).is_some_and(|t| t.text == "<") {
            let mut depth = 0i32;
            while k < n {
                depth += i32::from(toks[k].text == "<");
                depth -= i32::from(toks[k].text == ">");
                k += 1;
                if depth == 0 {
                    break;
                }
            }
        }
        let mut path: Vec<&str> = Vec::new();
        while k < n && !matches!(toks[k].text.as_str(), "for" | "{" | "<" | ";") {
            path.push(&toks[k].text);
            k += 1;
        }
        let Some((&name, prefix)) = path.split_last() else {
            return;
        };
        if (name == "Serialize" || name == "Deserialize")
            && toks
                .get(k)
                .is_some_and(|t| t.text == "<" || t.text == "for")
            && prefix
                .iter()
                .all(|t| matches!(*t, "serde" | "de" | "ser" | ":"))
        {
            self.manual
                .entry(name.to_string())
                .or_default()
                .push(format!("{rel}:{}", toks[i].line));
        }
    }

    fn render(&self, under: &str) -> String {
        let mut w = String::new();
        let _ = writeln!(
            w,
            "# serde attribute inventory: every #[serde(...)] key and form under {under}"
        );
        w.push_str("# Produced by `targo --unverified forge serde-attrs` (crates/aterm-forge). Read with\n");
        w.push_str("# docs/AUDIT-third-party-2026-10-15.md SP.5: the first-party derive accepts\n");
        w.push_str("# exactly these keys and forms and rejects any other with compile_error!.\n");
        for ((name, item), sites) in &self.derive_sites {
            let via = sites.iter().filter(|(_, v)| *v).count();
            let _ = writeln!(
                w,
                "# derive {name} on {item}: {} sites ({via} via cfg_attr)",
                sites.len()
            );
        }
        for (name, sites) in &self.manual {
            let _ = writeln!(
                w,
                "# hand-written impl {name}: {} (first {})",
                sites.len(),
                sites[0]
            );
        }
        w.push_str("position\tkey\tform\tsites\tin_tests\tvia_cfg_attr\tfirst_site\n");
        for ((at, key, form), sites) in &self.rows {
            let via = sites.iter().filter(|(_, v)| *v).count();
            let tests = sites
                .iter()
                .filter(|(site, _)| test_path(site.split(':').next().unwrap_or(site)))
                .count();
            let _ = writeln!(
                w,
                "{}\t{key}\t{form}\t{}\t{tests}\t{via}\t{}",
                at.name(),
                sites.len(),
                sites[0].0
            );
        }
        w.push_str(
            "\n# Each item's whole set of serde keys (an item with several attributes counts once).\n",
        );
        w.push_str("position\tkeys\titems\tfirst_site\n");
        for ((at, keys), sites) in &self.combos {
            let _ = writeln!(w, "{}\t{keys}\t{}\t{}", at.name(), sites.len(), sites[0]);
        }
        w
    }
}

fn rs_files(
    dir: &Path,
    skip: Option<&Path>,
    out: &mut Vec<std::path::PathBuf>,
) -> std::io::Result<()> {
    let mut files = Vec::new();
    let mut dirs = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let ty = entry.file_type()?;
        if ty.is_dir() {
            if name != "target" && skip != Some(entry.path().as_path()) {
                dirs.push(name);
            }
        } else if name.ends_with(".rs") {
            files.push(name);
        }
    }
    files.sort();
    dirs.sort();
    out.extend(files.into_iter().map(|f| dir.join(f)));
    for d in dirs {
        rs_files(&dir.join(d), skip, out)?;
    }
    Ok(())
}

/// The TSV inventory of the checkout at `root`: of `paths` (relative to `root`) when any are named,
/// else of `crates/` less `crates/aterm-serde`.
/// # Errors
/// A tree could not be walked or a file could not be read as UTF-8.
pub fn inventory(root: &Path, paths: &[std::path::PathBuf]) -> Result<String, String> {
    let implementation = root.join("crates/aterm-serde");
    let (trees, skip): (Vec<std::path::PathBuf>, Option<&Path>) = if paths.is_empty() {
        (vec![root.join("crates")], Some(implementation.as_path()))
    } else {
        (paths.iter().map(|p| root.join(p)).collect(), None)
    };
    let mut files = Vec::new();
    for tree in &trees {
        rs_files(tree, skip, &mut files)
            .map_err(|e| format!("cannot walk {}: {e}", tree.display()))?;
    }
    let under = if paths.is_empty() {
        "crates/".to_string()
    } else {
        paths
            .iter()
            .map(|p| format!("{}/", p.display().to_string().trim_end_matches('/')))
            .collect::<Vec<_>>()
            .join(" ")
    };
    let mut inv = Inventory::default();
    for path in files {
        let src = std::fs::read_to_string(&path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let rel = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        inv.scan(&rel, &src);
    }
    Ok(inv.render(&under))
}

/// The `serde-attrs` verb: the inventory on stdout.
/// # Errors
/// As [`inventory`].
pub fn run(root: &Path, paths: &[std::path::PathBuf]) -> Result<crate::Outcome, String> {
    Ok(crate::Outcome {
        ok: true,
        log: inventory(root, paths)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scanned(files: &[(&str, &str)]) -> Inventory {
        let mut inv = Inventory::default();
        for (rel, src) in files {
            inv.scan(rel, src);
        }
        inv
    }

    #[test]
    fn the_lexer_skips_comments_and_keeps_string_contents() {
        let toks = lex("a /* x /* y */ z */ // c\n r#\"q\"# 'b' 'life \"e\\\"s\" 1.5f32");
        let texts: Vec<(Kind, &str, usize)> = toks
            .iter()
            .map(|t| (t.kind, t.text.as_str(), t.line))
            .collect();
        assert_eq!(
            texts,
            vec![
                (Kind::Ident, "a", 1),
                (Kind::Str, "q", 2),
                (Kind::Lit, "'b'", 2),
                (Kind::Lit, "'life", 2),
                (Kind::Str, "e\\\"s", 2),
                (Kind::Lit, "1.5f32", 2),
            ]
        );
    }

    #[test]
    fn items_are_filed_by_where_they_sit() {
        let inv = scanned(&[(
            "crates/x/src/lib.rs",
            "#[derive(Serialize, Deserialize)]\n\
             #[serde(rename_all = \"snake_case\", deny_unknown_fields)]\n\
             pub enum E {\n\
                 #[serde(rename = \"a\")]\n\
                 A { #[serde(default)] x: u8 },\n\
                 B(#[serde(skip)] u8),\n\
             }\n\
             struct S { #[cfg_attr(test, serde(default = \"f\"))] pub f: u8 }\n\
             impl<'de> serde::Deserialize<'de> for S {}\n",
        )]);
        let keys: Vec<(Position, &str, &str)> = inv
            .rows
            .keys()
            .map(|(p, k, f)| (*p, k.as_str(), f.as_str()))
            .collect();
        assert_eq!(
            keys,
            vec![
                (Position::Container, "deny_unknown_fields", "bare"),
                (Position::Container, "rename_all", "= \"snake_case\""),
                (Position::Variant, "rename", "= <string>"),
                (Position::Field, "default", "= <string>"),
                (Position::Field, "default", "bare"),
                (Position::Field, "skip", "bare"),
            ]
        );
        assert_eq!(
            inv.rows[&(Position::Field, "default".into(), "= <string>".into())],
            vec![("crates/x/src/lib.rs:8".to_string(), true)]
        );
        assert!(inv.combos.contains_key(&(
            Position::Container,
            "deny_unknown_fields + rename_all = \"snake_case\"".to_string()
        )));
        assert_eq!(
            inv.derive_sites.keys().cloned().collect::<Vec<_>>(),
            vec![
                ("Deserialize".to_string(), "enum".to_string()),
                ("Serialize".to_string(), "enum".to_string())
            ]
        );
        assert_eq!(inv.manual["Deserialize"], vec!["crates/x/src/lib.rs:9"]);
    }

    #[test]
    fn nested_forms_and_test_paths() {
        let inv = scanned(&[
            (
                "crates/x/tests/serde_test.rs",
                "struct S { #[serde(with(a = \"b\", c))] f: u8 }",
            ),
            ("crates/x/src/broken_eof.rs", "#[serde(with("),
            ("crates/x/src/shebang_eof.rs", "#!"),
            (
                "crates/x/src/broken_item.rs",
                "#[serde(with(a) b)] struct B { f: u8 }",
            ),
        ]);
        let rendered = inv.render("crates/");
        assert!(rendered.contains(
            "field\twith\t(a = <string>, c bare)\t1\t1\t0\tcrates/x/tests/serde_test.rs:1\n"
        ));
        assert!(
            rendered
                .contains("container\twith\t?( a ) b\t1\t0\t0\tcrates/x/src/broken_item.rs:1\n")
        );
        assert!(test_path("crates/a/src/foo_tests.rs") && !test_path("crates/a/src/latest.rs"));
    }

    #[test]
    fn the_implementation_is_listed_only_when_named() {
        let root = std::env::temp_dir().join(format!("forge-serde-{}", std::process::id()));
        for (rel, src) in [
            (
                "crates/aterm-serde/src/lib.rs",
                "struct I { #[serde(skip)] f: u8 }",
            ),
            (
                "crates/x/src/lib.rs",
                "struct M { #[serde(default)] f: u8 }",
            ),
        ] {
            let path = root.join(rel);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
            std::fs::write(&path, src).expect("write");
        }
        let default = inventory(&root, &[]).expect("default tree");
        let named = inventory(&root, &["crates/aterm-serde".into()]).expect("named tree");
        let _ = std::fs::remove_dir_all(&root);
        assert!(
            default.contains("\tdefault\t") && !default.contains("\tskip\t"),
            "{default}"
        );
        assert!(default.starts_with(
            "# serde attribute inventory: every #[serde(...)] key and form under crates/\n"
        ));
        assert!(
            named.contains("\tskip\t") && !named.contains("\tdefault\t"),
            "{named}"
        );
        assert!(
            named.contains("form under crates/aterm-serde/\n"),
            "{named}"
        );
    }
}
