// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! An item of a Rust file (a function, constant, static, enum, struct, union, type alias or
//! trait) found by its path, and its tokens: what a mirror proof copied, read so a reformat or a
//! comment edit leaves it the same and any change of behaviour does not.
//!
//! The tokens are Rust's own, not characters: a multi-character operator (`&&`, `->`, `..=`,
//! `>>`) is one token, so `a && b` and `a & &b` differ; a literal is one token with its
//! contents; a lifetime is one token. Comments and whitespace are not tokens. Two spellings
//! rustfmt is free to choose between are read as one: a trailing comma (`(a, b,)` is `(a, b)`,
//! but the one-tuple `(x,)` keeps its comma, since `(x)` is not a tuple) and a visibility
//! (`pub(crate)` is dropped, so a harness's `pub` copy and the code's `pub(super)` agree).
//!
//! An item reads from its outer attributes (`#[cfg(...)]`, `#[repr(u8)]`, derives: they change
//! what compiles) to its end, with the head of every `impl`, `mod` and `trait` around it, so an
//! item `#[cfg]`'d out or moved under another `impl` is not the item proved. An item inside test
//! code (`#[cfg(test)]`, `mod tests`) or a `macro_rules!` is never the one a path names.

use crate::lex::{SpanKind, lex, spans};

/// The keywords that open an item a path can name.
const ITEM_KEYWORDS: [&str; 8] = [
    "fn", "const", "static", "enum", "struct", "union", "type", "trait",
];

/// Multi-character punctuation, longest first.
const PUNCT: [&str; 24] = [
    "<<=", ">>=", "...", "..=", "::", "->", "=>", "==", "!=", "<=", ">=", "&&", "||", "+=", "-=",
    "*=", "/=", "%=", "^=", "&=", "|=", "<<", ">>", "..",
];

/// The words before a `(` that make it a tuple, not a call's argument list.
const NOT_CALLEES: [&str; 18] = [
    "as", "break", "return", "in", "let", "mut", "match", "if", "while", "else", "move", "yield",
    "ref", "for", "loop", "unsafe", "where", "dyn",
];

/// A file's tokens over its comment-blanked text.
struct Tokens {
    text: String,
    spans: Vec<(usize, usize)>,
    /// For each bracket token, its partner's index.
    pairs: Vec<Option<usize>>,
}

impl Tokens {
    fn of(src: &str) -> Self {
        let text = lex(src).text;
        let literals: Vec<(usize, usize)> = spans(src)
            .into_iter()
            .filter(|s| s.kind != SpanKind::Comment)
            .map(|s| (s.start, s.end))
            .collect();
        let b = text.as_bytes();
        let mut out = Vec::new();
        let mut lit_at = 0;
        let mut i = 0;
        while i < b.len() {
            while lit_at < literals.len() && literals[lit_at].0 < i {
                lit_at += 1;
            }
            if lit_at < literals.len() && literals[lit_at].0 == i {
                let end = literals[lit_at].1;
                out.push((i, end));
                i = end;
                lit_at += 1;
                continue;
            }
            let c = b[i];
            if c.is_ascii_whitespace() {
                i += 1;
                continue;
            }
            let start = i;
            if is_word(c) {
                while i < b.len() && is_word(b[i]) {
                    i += 1;
                }
            } else if c == b'\'' && b.get(i + 1).is_some_and(|&n| is_word(n)) {
                i += 1;
                while i < b.len() && is_word(b[i]) {
                    i += 1;
                }
            } else if let Some(p) = PUNCT.iter().find(|p| text[i..].starts_with(**p)) {
                i += p.len();
            } else {
                i += text[i..].chars().next().map_or(1, char::len_utf8);
            }
            out.push((start, i));
        }
        let mut tokens = Self {
            text,
            spans: out,
            pairs: Vec::new(),
        };
        tokens.pairs = tokens.pair_brackets();
        tokens
    }

    fn len(&self) -> usize {
        self.spans.len()
    }

    fn tok(&self, i: usize) -> &str {
        self.spans
            .get(i)
            .map_or("", |&(start, end)| &self.text[start..end])
    }

    fn pair_brackets(&self) -> Vec<Option<usize>> {
        let mut pairs = vec![None; self.len()];
        let mut stack: Vec<(usize, &str)> = Vec::new();
        for i in 0..self.len() {
            let closer = match self.tok(i) {
                "(" | "[" | "{" => {
                    stack.push((i, self.tok(i)));
                    continue;
                }
                ")" => "(",
                "]" => "[",
                "}" => "{",
                _ => continue,
            };
            if stack.last().is_some_and(|&(_, open)| open == closer)
                && let Some((open, _)) = stack.pop()
            {
                pairs[open] = Some(i);
                pairs[i] = Some(open);
            }
        }
        pairs
    }

    /// The tokens `from..=to`, one space apart, with the trailing commas and visibilities a
    /// formatter may add or a harness may drop left out.
    fn normal(&self, from: usize, to: usize) -> Vec<&str> {
        let mut out = Vec::new();
        let mut i = from;
        while i <= to && i < self.len() {
            let t = self.tok(i);
            if t == "pub" {
                if self.tok(i + 1) == "("
                    && matches!(self.tok(i + 2), "crate" | "super" | "self" | "in")
                    && let Some(close) = self.pairs[i + 1]
                {
                    i = close + 1;
                } else {
                    i += 1;
                }
                continue;
            }
            if t == ","
                && i < to
                && matches!(self.tok(i + 1), ")" | "]" | "}" | ">")
                && !self.one_tuple(i)
            {
                i += 1;
                continue;
            }
            out.push(t);
            i += 1;
        }
        out
    }

    /// Is the comma at `comma`, just before a `)`, the one comma of a one-tuple?
    fn one_tuple(&self, comma: usize) -> bool {
        if self.tok(comma + 1) != ")" {
            return false;
        }
        let Some(open) = self.pairs[comma + 1] else {
            return false;
        };
        let mut commas = 0;
        let mut j = open + 1;
        while j < comma + 1 {
            match self.tok(j) {
                "(" | "[" | "{" => j = self.pairs[j].unwrap_or(j),
                "," => commas += 1,
                _ => {}
            }
            j += 1;
        }
        let before = open.checked_sub(1).map_or("", |p| self.tok(p));
        let callee = matches!(before, ")" | "]" | ">" | "!")
            || (before.bytes().next().is_some_and(is_word_start) && !NOT_CALLEES.contains(&before));
        commas == 1 && !callee
    }
}

const fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

const fn is_word_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

/// A `{ … }` of the file: its token range, its head's range (from the last `;`, `{` or `}`
/// before it), what it names (an `impl`'s self type and trait, a `mod`'s or `trait`'s name),
/// and whether it opens test code or a macro.
struct Block {
    open: usize,
    close: usize,
    head: usize,
    names: Vec<String>,
    /// An `impl` or `trait` block: an item in one is named only through it.
    owner: bool,
    shut: bool,
}

fn blocks(t: &Tokens) -> Vec<Block> {
    let mut out = Vec::new();
    let mut head = 0;
    for i in 0..t.len() {
        match t.tok(i) {
            "{" => {
                let (names, owner, shut) = header(t, head, i);
                out.push(Block {
                    open: i,
                    close: t.pairs[i].unwrap_or(t.len()),
                    head,
                    names,
                    owner,
                    shut,
                });
                head = i + 1;
            }
            "}" | ";" => head = i + 1,
            _ => {}
        }
    }
    out
}

/// What the head `from..to` names, whether its block is an `impl` or `trait`, and whether it
/// is test code or a macro's.
fn header(t: &Tokens, from: usize, to: usize) -> (Vec<String>, bool, bool) {
    let toks: Vec<&str> = (from..to).map(|i| t.tok(i)).collect();
    let mut shut = toks
        .windows(2)
        .any(|w| w == ["mod", "tests"] || w == ["macro_rules", "!"]);
    for (at, tok) in toks.iter().enumerate() {
        if *tok == "cfg" && toks.get(at + 1) == Some(&"(") {
            let group: Vec<&str> = toks[at + 1..]
                .iter()
                .copied()
                .take_while(|x| *x != "]")
                .collect();
            if group.contains(&"test") && !group.contains(&"not") {
                shut = true;
            }
        }
    }
    let mut names = Vec::new();
    let opener = toks.iter().position(|x| {
        matches!(
            *x,
            "fn" | "mod" | "trait" | "impl" | "struct" | "enum" | "union"
        )
    });
    let kind = opener.map_or("", |at| toks[at]);
    if let Some(at) = opener.filter(|_| matches!(kind, "mod" | "trait")) {
        if let Some(name) = toks.get(at + 1) {
            names.push((*name).to_string());
        }
    } else if let Some(at) = opener.filter(|_| kind == "impl") {
        let mut rest = &toks[at + 1..];
        if rest.first() == Some(&"<") {
            rest = &rest[angle_end(rest)..];
        }
        let rest = &rest[..rest
            .iter()
            .position(|x| *x == "where")
            .unwrap_or(rest.len())];
        match split_for(rest) {
            Some((tr, ty)) => {
                names.push(type_name(ty));
                names.push(type_name(tr));
            }
            None => names.push(type_name(rest)),
        }
    }
    (names, matches!(kind, "impl" | "trait"), shut)
}

/// The index just past the `<…>` `toks` opens with.
fn angle_end(toks: &[&str]) -> usize {
    let mut depth = 0i32;
    for (i, t) in toks.iter().enumerate() {
        depth += angle(t);
        if depth <= 0 {
            return i + 1;
        }
    }
    toks.len()
}

/// How a token moves the depth of `<…>`.
fn angle(t: &str) -> i32 {
    match t {
        "<" => 1,
        ">" => -1,
        ">>" => -2,
        _ => 0,
    }
}

/// `toks` split at its `for` outside `<…>`, into trait and self type.
fn split_for<'a, 'b>(toks: &'a [&'b str]) -> Option<(&'a [&'b str], &'a [&'b str])> {
    let mut depth = 0i32;
    for (i, t) in toks.iter().enumerate() {
        depth += angle(t);
        if depth == 0 && *t == "for" && toks.get(i + 1) != Some(&"<") {
            return Some((&toks[..i], &toks[i + 1..]));
        }
    }
    None
}

/// A type's last path segment outside its generics and brackets: `&'a mut a::B<C>` is `B`.
fn type_name(toks: &[&str]) -> String {
    let mut depth = 0i32;
    let mut name = "";
    for t in toks {
        match *t {
            "(" | "[" => depth += 1,
            ")" | "]" => depth -= 1,
            _ => depth += angle(t),
        }
        if depth == 0
            && t.bytes().next().is_some_and(is_word_start)
            && !matches!(*t, "mut" | "dyn" | "const")
        {
            name = t;
        }
    }
    name.to_string()
}

/// `path`'s qualifiers and its name: `<A as B<C>>::f` is (`[A, B]`, `f`), `m::T::f` is
/// (`[m, T]`, `f`).
fn split_path(path: &str) -> (Vec<String>, &str) {
    let (head, name) = path.rsplit_once("::").unwrap_or(("", path));
    let mut quals = Vec::new();
    if let Some(inner) = head.strip_prefix('<').and_then(|h| h.strip_suffix('>')) {
        let (ty, tr) = inner.split_once(" as ").unwrap_or((inner, ""));
        for part in [ty, tr] {
            let toks = Tokens::of(part);
            let words: Vec<&str> = (0..toks.len()).map(|i| toks.tok(i)).collect();
            if !words.is_empty() {
                quals.push(type_name(&words));
            }
        }
    } else {
        quals.extend(
            head.split("::")
                .filter(|q| !q.is_empty())
                .map(ToString::to_string),
        );
    }
    (quals, name)
}

/// Do `quals` name the blocks enclosing an item, outermost first? Each qualifier names one
/// block, in order (one block may answer to consecutive qualifiers: an `impl Trait for Type`
/// to both of `<Type as Trait>`); a `mod` may go unnamed, an `impl` or `trait` may not.
fn qualifiers_hold(quals: &[String], enclosing: &[&Block]) -> bool {
    let mut at = 0;
    for block in enclosing {
        let before = at;
        while quals.get(at).is_some_and(|q| block.names.contains(q)) {
            at += 1;
        }
        if block.owner && at == before {
            return false;
        }
    }
    at == quals.len()
}

/// The last token of the item whose keyword `kw` is at `k` (its name at `k + 1`): a body's
/// closing brace, or the `;` that ends a declaration; `None` for a function or trait with no
/// body.
fn item_end(t: &Tokens, kw: &str, k: usize) -> Option<usize> {
    let valued = matches!(kw, "const" | "static" | "type");
    let mut depth = 0i32;
    let mut j = k + 2;
    while j < t.len() {
        let tok = t.tok(j);
        match tok {
            "(" | "[" => j = t.pairs[j]?,
            "{" if valued || depth > 0 => j = t.pairs[j]?,
            "{" => return t.pairs[j],
            ";" if valued || depth <= 0 => {
                return (!matches!(kw, "fn" | "trait")).then_some(j);
            }
            _ if !valued => depth += angle(tok),
            _ => {}
        }
        j += 1;
    }
    None
}

/// The first token of the item whose keyword is at `k`: back over its qualifiers (`pub`,
/// `pub(crate)`, `const`, `unsafe`, `extern "C"`, …) and its outer attributes.
fn item_start(t: &Tokens, k: usize) -> usize {
    let mut s = k;
    while let Some(p) = s.checked_sub(1) {
        let tok = t.tok(p);
        if matches!(
            tok,
            "pub" | "const" | "async" | "unsafe" | "extern" | "default" | "safe"
        ) || (tok.starts_with('"') && p > 0 && t.tok(p - 1) == "extern")
        {
            s = p;
        } else if tok == ")"
            && let Some(open) = t.pairs[p]
            && open > 0
            && t.tok(open - 1) == "pub"
        {
            s = open;
        } else if tok == "]"
            && let Some(open) = t.pairs[p]
            && open > 0
            && t.tok(open - 1) == "#"
        {
            s = open - 1;
        } else {
            break;
        }
    }
    s
}

/// A Rust file tokenized and block-indexed once so multiple items can be extracted without
/// re-lexing the file.
pub struct ParsedFile {
    t: Tokens,
    blocks: Vec<Block>,
}

impl ParsedFile {
    /// Tokenizes `src` and indexes its `{ … }` blocks.
    #[must_use]
    pub fn of(src: &str) -> Self {
        let t = Tokens::of(src);
        let blocks = blocks(&t);
        Self { t, blocks }
    }

    /// The tokens of the item `path` names in this file.
    ///
    /// # Errors
    /// As [`item_tokens`].
    pub fn item_tokens(&self, path: &str) -> Result<String, String> {
        let (quals, name) = split_path(path);
        let t = &self.t;
        let blocks = &self.blocks;
        let mut found = Vec::new();
        for k in 0..t.len() {
            let kw = t.tok(k);
            if !ITEM_KEYWORDS.contains(&kw) || t.tok(k + 1) != name {
                continue;
            }
            let before = k.checked_sub(1).map_or("", |p| t.tok(p));
            if matches!(kw, "const" | "static") && matches!(before, "<" | ",") {
                continue;
            }
            let enclosing: Vec<&Block> = blocks
                .iter()
                .filter(|b| b.open < k && k < b.close)
                .collect();
            if enclosing.iter().any(|b| b.shut) || !qualifiers_hold(&quals, &enclosing) {
                continue;
            }
            let Some(end) = item_end(t, kw, k) else {
                continue;
            };
            let mut out: Vec<&str> = Vec::new();
            for b in enclosing.iter().filter(|b| !b.names.is_empty()) {
                out.extend(t.normal(b.head, b.open));
            }
            out.extend(t.normal(item_start(t, k), end));
            found.push(out.join(" "));
        }
        match found.len() {
            1 => Ok(found.remove(0)),
            0 => Err(format!("no item `{name}` answers to `{path}`")),
            n => Err(format!(
                "{n} items answer to `{path}`: qualify it (`Type::{name}`, `module::{name}`)"
            )),
        }
    }
}

/// The tokens of the item `path` names in `src` ([module docs](self)): the heads of the
/// `impl`, `mod` and `trait` blocks around it, then the item, one space apart.
///
/// `path` is `name`, or `name` behind qualifiers (`Type::name`, `module::name`,
/// `<Type as Trait>::name`), each of which must name an `impl`'s self type, its trait, a `mod`
/// or a `trait` enclosing the item, innermost last.
///
/// # Errors
/// When no item, or more than one, answers to `path` outside test code and macros.
pub fn item_tokens(src: &str, path: &str) -> Result<String, String> {
    ParsedFile::of(src).item_tokens(path)
}

/// The names of `src`'s items outside test code and macros, in file order, each once: what a
/// harness header's glob (`FIT_TOKEN_MASK_*`) is read against.
#[must_use]
pub fn item_names(src: &str) -> Vec<String> {
    let t = Tokens::of(src);
    let blocks = blocks(&t);
    let mut out: Vec<String> = Vec::new();
    for k in 0..t.len() {
        let (kw, name) = (t.tok(k), t.tok(k + 1));
        let before = k.checked_sub(1).map_or("", |p| t.tok(p));
        if !ITEM_KEYWORDS.contains(&kw)
            || !name.bytes().next().is_some_and(is_word_start)
            || ITEM_KEYWORDS.contains(&name)
            || matches!(name, "unsafe" | "extern" | "async")
            || (matches!(kw, "const" | "static") && matches!(before, "<" | ","))
            || blocks.iter().any(|b| b.open < k && k < b.close && b.shut)
            || out.iter().any(|n| n == name)
        {
            continue;
        }
        out.push(name.to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = "pub struct A;\n\
        impl A {\n    /// Doc.\n    pub const fn level(self) -> u8 {\n        match self { _ => 1, }\n    }\n}\n\
        impl From<bool> for A {\n    fn from(b: bool) -> Self { let _ = \"x  y\"; A }\n}\n\
        pub fn level(x: u8) -> u8 { x }\n\
        #[cfg(test)]\nmod tests {\n    fn level() {}\n    fn only_here() {}\n}\n\
        macro_rules! m {\n    () => { fn in_macro() {} };\n}\n";

    fn toks(src: &str, path: &str) -> String {
        item_tokens(src, path).unwrap_or_else(|e| panic!("{path}: {e}"))
    }

    #[test]
    fn an_item_is_found_by_its_qualifiers() {
        assert_eq!(
            toks(SRC, "A::level"),
            "impl A { const fn level ( self ) -> u8 { match self { _ => 1 } }"
        );
        assert_eq!(toks(SRC, "level"), "fn level ( x : u8 ) -> u8 { x }");
        assert!(item_tokens(SRC, "<A as From<bool>>::from").is_ok());
        assert!(item_tokens(SRC, "<A as Into<bool>>::from").is_err());
        assert!(item_tokens(SRC, "B::level").is_err());
        assert_eq!(toks(SRC, "A"), "struct A ;");
    }

    #[test]
    fn test_code_and_macros_hold_no_item() {
        assert!(item_tokens(SRC, "only_here").is_err());
        assert!(item_tokens(SRC, "in_macro").is_err());
        let gone = SRC.replace("pub fn level(x: u8) -> u8 { x }\n", "");
        assert!(
            item_tokens(
                &gone.replace("fn level() {}", "fn level(x: u8) -> u8 { x }"),
                "level"
            )
            .is_err(),
            "a test module's copy never stands in for a deleted function"
        );
    }

    #[test]
    fn operators_are_whole_tokens() {
        let and = "fn f(a: bool, b: bool) -> bool { a && b }";
        let amp = "fn f(a: bool, b: bool) -> bool { a & &b }";
        assert_ne!(toks(and, "f"), toks(amp, "f"));
        assert_eq!(
            toks("fn g(x: u8) -> u8 { x >>= 1; x..=2 }", "g"),
            "fn g ( x : u8 ) -> u8 { x >>= 1 ; x ..= 2 }"
        );
        assert_eq!(
            toks("fn h<'a>(x: &'a str) {}", "h"),
            "fn h < 'a > ( x : & 'a str ) { }"
        );
    }

    #[test]
    fn a_trailing_comma_goes_but_a_one_tuple_keeps_its_comma() {
        let tuple = "fn f(x: u8) -> (u8,) { (x,) }";
        let paren = "fn f(x: u8) -> (u8) { (x) }";
        assert_ne!(toks(tuple, "f"), toks(paren, "f"));
        assert_eq!(
            toks(
                "fn f(\n    x: u8,\n) -> u8 {\n    g(\n        x,\n    )\n}",
                "f"
            ),
            toks("fn f(x: u8) -> u8 { g(x) }", "f")
        );
        assert_eq!(
            toks("fn f() -> [u8; 2] { [1, 2,] }", "f"),
            toks("fn f() -> [u8; 2] { [1, 2] }", "f")
        );
        assert_ne!(
            toks("fn f() { g(a, b) }", "f"),
            toks("fn f() { g(a b) }", "f"),
            "only a comma before a closing bracket is a formatter's"
        );
    }

    #[test]
    fn a_brace_in_the_signature_does_not_open_the_body() {
        let src = "fn f() -> Foo<{ N }> { 1 }\n";
        let edited = "fn f() -> Foo<{ N }> { 2 }\n";
        assert_ne!(toks(src, "f"), toks(edited, "f"));
        assert_eq!(toks(src, "f"), "fn f ( ) -> Foo < { N } > { 1 }");
        let arr = "fn f(x: [u8; { 2 }]) { 1 }\n";
        assert_eq!(toks(arr, "f"), "fn f ( x : [ u8 ; { 2 } ] ) { 1 }");
    }

    #[test]
    fn attributes_and_the_impl_head_are_part_of_the_item() {
        let src = "impl A {\n    pub(crate) fn f(self) -> u8 { 1 }\n}\n";
        let gated = "impl A {\n    #[cfg(any())]\n    pub(crate) fn f(self) -> u8 { 1 }\n}\n";
        let moved = "impl<T> A<T> {\n    pub(crate) fn f(self) -> u8 { 1 }\n}\n";
        assert_ne!(toks(src, "A::f"), toks(gated, "A::f"));
        assert_ne!(toks(src, "A::f"), toks(moved, "A::f"));
        assert_eq!(
            toks(src, "A::f"),
            toks("impl A {\n    pub fn f(self) -> u8 { 1 }\n}\n", "A::f"),
            "visibility is not behaviour"
        );
    }

    #[test]
    fn constants_and_enums_are_items() {
        let src = "pub const MAX: usize = 512;\n#[repr(usize)]\npub enum Level {\n    Error = 1,\n    Warn = 2,\n}\nconst fn f<const N: usize>() {}\n";
        assert_eq!(toks(src, "MAX"), "const MAX : usize = 512 ;");
        assert_eq!(
            toks(src, "Level"),
            "# [ repr ( usize ) ] enum Level { Error = 1 , Warn = 2 }"
        );
        assert_ne!(
            toks(src, "Level"),
            toks(
                &src.replace("Error = 1,\n    Warn = 2", "Warn = 2,\n    Error = 1"),
                "Level"
            ),
            "the variant order is the derived order"
        );
        assert!(item_tokens(src, "N").is_err(), "a const generic is no item");
        assert_eq!(item_names(src), ["MAX", "Level", "f"]);
        assert_eq!(toks(src, "f"), "const fn f < const N : usize > ( ) { }");
    }
}
