// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! A Rust source file read as token trees: what a build without `cfg(test)` compiles of it, its
//! code ([`Source::code`]) apart from its comments ([`Source::comments`], and the one that holds a
//! byte, [`Source::comment_at`]), and each place in it that can bring another file into a build
//! ([`Source::mounts`], [`Source::includes`]).
//!
//! The census lexer ([`crate::lex::spans`]) finds the comments and literals; the rest of the file
//! is split into identifiers, lifetimes, numbers, punctuation and the delimiters that nest them, so
//! a brace inside a string, a comment or a char literal never opens or closes anything.
//!
//! An item is test-only when one of its own outer attributes is `#[cfg(P)]` with `P` false whenever
//! `test` is ([`crate::wasm_cfg::when_test_disabled`]). rustc evaluates every `cfg` and `cfg_attr`
//! of an item before any attribute macro on it runs, and a `cfg_attr` can only add attributes, so
//! nothing else on the item keeps it in a build without tests. Items are read at the top of the
//! file and inside the inline modules, `impl` and `trait` blocks, each only where every attribute
//! on it, outer and inner, is inert ([`inert`]): an attribute macro on an enclosing item receives
//! its body before the `cfg`s inside it are evaluated, and can keep what they name. Function bodies, macro bodies and every other
//! group are opaque, so what they hold always ships. An item ends at the first `;` or `{…}` its
//! kind can end at, never later: where that is earlier than the item's real end (a `{ N }` const
//! argument in a test function's signature), the rest of the item reads as shipped.
//!
//! A comment is carried ([`Comment::carried`]) where a macro, or another front end, may read it, as
//! a doc comment's `#[doc]` token or through a span's source text: inside a macro invocation's
//! arguments, a `macro_rules!` or `macro` body, or an attribute's brackets; within an item whose
//! attributes carry it ([`carrying`]: a `derive`, an attribute this reader cannot vouch for, or a
//! `doc = …`, each also under `cfg_attr`), from its first attribute to an end at or past its real
//! one ([`item_extent`]); within an item macro invocation, attributes included; within a group the
//! item reader did not read whose tokens hold a carrying attribute at any depth (a local
//! `#[derive]` item in a function body); within an item led by a word the item reader cannot name,
//! and a `{…}` group the word `clean` precedes, since another front end may read either's text (a
//! Trust `clean { … }` island is Lean, where a span that lexes as a Rust comment is code); in the
//! rest of a level once the item reader cannot end an item there; and anywhere in a file whose own
//! inner attributes carry.
//!
//! What this reader does not see: what a proc macro or a build script generates, which is assumed
//! not to declare a module from a source file.

use std::collections::{BTreeMap, BTreeSet};

use crate::lex::{self, SpanKind};
use crate::wasm_cfg::{Truth, when_test_disabled};

/// One file, read.
#[derive(Debug)]
pub struct Source {
    text: String,
    trees: Vec<Tree>,
    line_starts: Vec<usize>,
    /// The byte ranges of the test-only items, in order.
    test: Vec<(usize, usize)>,
    /// The file's own inner attributes include a test-only `cfg`: no build without tests reads it.
    whole: bool,
    /// The byte ranges a macro may read ([`Comment::carried`]), sorted and disjoint.
    carriers: Vec<(usize, usize)>,
    test_modules: Vec<TestModule>,
    mounts: Vec<Mount>,
    includes: Vec<Include>,
    strings: Vec<(Option<String>, usize)>,
}

/// A token a build without `cfg(test)` compiles: its text (punctuation one character at a time) and
/// where it starts, its 1-based line and the text before it on that line, since `line!`, `column!`
/// and a panic's location read that place (`column!` in characters, a location in display
/// columns), and equal text before it means an equal column however it is counted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shipped<'a> {
    pub text: &'a str,
    pub line: usize,
    pub before: &'a str,
}

/// A comment a build without `cfg(test)` reads, plain or doc ([`Source::comments`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Comment<'a> {
    /// Its text and place, as a [`Shipped`] token's.
    pub at: Shipped<'a>,
    /// How many of the file's code tokens ([`Source::code`]) come before it: where it sits among
    /// them.
    pub gap: usize,
    /// A macro, or another front end, may read it (the module's carriers).
    pub carried: bool,
    /// It is a doc comment (`///` but not `////`, `//!`, `/** */` but not `/***` or `/**/`, and
    /// `/*! */`): a `#[doc]` attribute, which the member's own lint and doctests read. A plain
    /// comment is no token at all.
    pub doc: bool,
}

/// A file's code and comments outside its test-only items, in order ([`Source::code`],
/// [`Source::comments`]), with each comment's byte range.
#[derive(Default)]
struct Split<'a> {
    code: Vec<Shipped<'a>>,
    comments: Vec<Comment<'a>>,
    /// The byte range of each of `comments`, in its order.
    spans: Vec<(usize, usize)>,
}

/// A `mod <name>;` declaration: a module from another file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mount {
    /// The module's name, or `None` where a macro names it (`mod $name;`).
    pub name: Option<String>,
    pub path: MountPath,
    /// It sits in code only a test build compiles ([`Source::test_only`]): a build without tests
    /// never reads the file it names.
    pub test: bool,
    /// It is at the top of its file, outside any inline module (whose name the directory of a
    /// default file takes): only a declaration this reader places can say so.
    pub top: bool,
    /// A macro may read the module it declares: one of its own attributes carries
    /// ([`carrying`]), or this reader did not place it (a declaration a macro's body or an
    /// attribute macro's item holds is one).
    pub carried: bool,
    /// The byte offset of its `mod`.
    pub at: usize,
}

/// A module whose own attributes make it test-only, written with its body inline
/// (`#[cfg(test)] mod tests { … }`): a build without tests strips it, but only after reading its
/// whole body from this file, so an edit to it changes what such a build reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestModule {
    /// The module's name, as its file would be named (`r#type` is `type`).
    pub name: String,
    /// The enclosing inline modules' names, outermost first; empty at the top of the file.
    pub within: Vec<String>,
    /// One of them carries `#[path]` (outer or inner), which moves the folder its children's files
    /// are found in away from the one `within` names.
    pub pathed: bool,
    /// Its test `cfg` is an inner attribute (`mod tests { #![cfg(test)] … }`), so its `mod` line
    /// is not gated: out of line, a build without tests would still open its file to read it.
    pub inner_gate: bool,
    /// An outer attribute on it is no inert built-in, so a macro may read its body as written.
    pub rewritten: bool,
    /// The item's byte range: its first outer attribute or doc comment through its closing brace.
    pub item: (usize, usize),
    /// The byte offset where the item starts after its attributes (its visibility or `mod`).
    pub head: usize,
    /// The byte offset of the body's opening brace.
    pub open: usize,
    /// The body's byte range, strictly inside its braces.
    pub body: (usize, usize),
}

/// Where a [`Mount`]'s file is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MountPath {
    /// `<name>.rs` or `<name>/mod.rs`, beside the declaring module.
    Default,
    /// `#[path = "…"]`.
    Literal(String),
    /// A path this reader cannot settle: a `cfg_attr` that may set one, two of them, a value that is
    /// no plain string, or a declaration it cannot place.
    Unsettled,
}

/// An `include!`, `include_str!` or `include_bytes!`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Include {
    pub kind: IncludeKind,
    pub arg: IncludeArg,
    /// It sits in a macro's body, so its path is relative to wherever the macro is invoked.
    pub in_macro: bool,
    /// The byte offset of its name.
    pub at: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IncludeKind {
    /// `include!`: the file's tokens.
    Tokens,
    /// `include_str!`.
    Text,
    /// `include_bytes!`.
    Bytes,
}

/// What an [`Include`] reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IncludeArg {
    /// A path relative to the including file's directory.
    Relative(String),
    /// `concat!(env!("CARGO_MANIFEST_DIR"), "…")`: what follows the package's directory.
    ManifestDir(String),
    /// `concat!(env!("OUT_DIR"), …)`: what the build script wrote.
    OutDir,
    /// Anything else: a macro variable, another `env!`, a computed string.
    Unknown,
}

impl Source {
    /// Read `text`, or `None` where it does not lex: an unterminated literal or comment, or a
    /// delimiter that does not close.
    #[must_use]
    pub fn read(text: String) -> Option<Self> {
        let trees = build(&text, tokenize(&text)?)?;
        let line_starts = std::iter::once(0)
            .chain(text.match_indices('\n').map(|(at, _)| at + 1))
            .collect();
        let mut source = Self {
            text,
            trees,
            line_starts,
            test: Vec::new(),
            whole: false,
            carriers: Vec::new(),
            test_modules: Vec::new(),
            mounts: Vec::new(),
            includes: Vec::new(),
            strings: Vec::new(),
        };
        let mut found = Found::default();
        let (inner, from) = inner_attributes(&source.text, &source.trees);
        source.whole = inner.iter().any(|attr| test_cfg(&source.text, attr));
        if !source.whole && inner.iter().all(|attr| inert(&source.text, attr)) {
            items(
                &source.text,
                &source.trees,
                from,
                Level::Top,
                &mut Vec::new(),
                &mut found,
            );
        }
        if inner.iter().any(|attr| carrying(&source.text, attr)) {
            found.carriers.push((0, source.text.len()));
        }
        let placed = std::mem::take(&mut found.placed);
        scan(
            &source.text,
            &source.trees,
            false,
            Carriers::Open,
            &placed,
            &mut found,
        );
        found.test.sort_unstable();
        source.test = found.test;
        source.carriers = disjoint(found.carriers);
        for mount in &mut found.mounts {
            mount.test = source.test_only(mount.at);
        }
        source.test_modules = found.test_modules;
        source.mounts = found.mounts;
        source.includes = found.includes;
        let path_strings = found.path_strings;
        source.strings = found
            .strings
            .into_iter()
            .filter(|(_, at)| !path_strings.contains(at))
            .collect();
        Some(source)
    }

    /// Whether the byte at `at` is in code only a test build compiles: a test-only item, or a
    /// file whose own attributes make all of it test-only.
    #[must_use]
    pub fn test_only(&self, at: usize) -> bool {
        self.whole
            || self
                .test
                .iter()
                .any(|&(start, end)| (start..end).contains(&at))
    }

    /// Whether the file's own inner attributes make all of it test-only (`#![cfg(test)]`): a build
    /// without tests still opens it to read them, unless the declaration that mounts it is gated.
    #[must_use]
    pub const fn whole(&self) -> bool {
        self.whole
    }

    /// The test-only modules written inline ([`TestModule`]), in order, outermost only: one
    /// inside another test-only item is that item's.
    #[must_use]
    pub fn test_modules(&self) -> &[TestModule] {
        &self.test_modules
    }

    /// The text of every token that starts in `start..end`, comments and doc comments left out,
    /// in order: two ranges hold the same code where these are equal.
    #[must_use]
    pub fn code_in(&self, start: usize, end: usize) -> Vec<&str> {
        let mut out = Vec::new();
        self.code_of(&self.trees, start, end, &mut out);
        out
    }

    fn code_of<'a>(&'a self, trees: &[Tree], start: usize, end: usize, out: &mut Vec<&'a str>) {
        for tree in trees {
            if tree.end() <= start || tree.start() >= end {
                continue;
            }
            match tree {
                Tree::Leaf(tok) => {
                    if !matches!(tok.kind, Kind::Comment | Kind::OuterDoc | Kind::InnerDoc)
                        && (start..end).contains(&tok.start)
                    {
                        out.push(&self.text[tok.start..tok.end]);
                    }
                }
                Tree::Group {
                    open,
                    close,
                    children,
                } => {
                    if (start..end).contains(&open.start) {
                        out.push(&self.text[open.start..open.end]);
                    }
                    self.code_of(children, start, end, out);
                    if (start..end).contains(&close.start) {
                        out.push(&self.text[close.start..close.end]);
                    }
                }
            }
        }
    }

    /// Every token outside the test-only items but the comments, in order, each in its place: two
    /// versions of a file whose code is equal compile to the same thing without tests, apart from
    /// what their comments make ([`Source::comments`]): doc strings, and whatever a macro that
    /// reads a comment makes of it ([`Comment::carried`]).
    #[must_use]
    pub fn code(&self) -> Vec<Shipped<'_>> {
        self.split().0
    }

    /// Every comment, plain or doc, outside the test-only items, in order.
    #[must_use]
    pub fn comments(&self) -> Vec<Comment<'_>> {
        self.split().1
    }

    /// The comment of [`Source::comments`] whose text holds the byte at `at`, or `None` where that
    /// byte is code, a literal, between tokens, or in a test-only item.
    #[must_use]
    pub fn comment_at(&self, at: usize) -> Option<Comment<'_>> {
        let mut out = Split::default();
        if !self.whole {
            self.flatten(&self.trees, &mut out);
        }
        let index = out
            .spans
            .iter()
            .position(|&(start, end)| (start..end).contains(&at))?;
        Some(out.comments[index])
    }

    /// Whether a macro, or another front end, may read the byte at `at` (the module's carriers).
    #[must_use]
    pub fn carried(&self, at: usize) -> bool {
        let after = self.carriers.partition_point(|&(start, _)| start <= at);
        after > 0 && at < self.carriers[after - 1].1
    }

    fn split(&self) -> (Vec<Shipped<'_>>, Vec<Comment<'_>>) {
        let mut out = Split::default();
        if !self.whole {
            self.flatten(&self.trees, &mut out);
        }
        (out.code, out.comments)
    }

    fn flatten<'a>(&'a self, trees: &'a [Tree], out: &mut Split<'a>) {
        for tree in trees {
            match tree {
                Tree::Leaf(tok) => self.push(tok, out),
                Tree::Group {
                    open,
                    close,
                    children,
                } => {
                    self.push(open, out);
                    self.flatten(children, out);
                    self.push(close, out);
                }
            }
        }
    }

    fn push<'a>(&'a self, tok: &Tok, out: &mut Split<'a>) {
        if self.test_only(tok.start) {
            return;
        }
        let line = self.line_of(tok.start);
        let at = Shipped {
            text: &self.text[tok.start..tok.end],
            line,
            before: &self.text[self.line_starts[line - 1]..tok.start],
        };
        if matches!(tok.kind, Kind::Comment | Kind::OuterDoc | Kind::InnerDoc) {
            out.comments.push(Comment {
                at,
                gap: out.code.len(),
                carried: self.carried(tok.start),
                doc: tok.kind != Kind::Comment,
            });
            out.spans.push((tok.start, tok.end));
        } else {
            out.code.push(at);
        }
    }

    /// The 1-based line holding the byte at `at`.
    #[must_use]
    pub fn line_of(&self, at: usize) -> usize {
        self.line_starts.partition_point(|&start| start <= at)
    }

    /// Every `mod <name>;` in the file, at any depth, macro bodies included.
    #[must_use]
    pub fn mounts(&self) -> &[Mount] {
        &self.mounts
    }

    /// Every `include!`-family invocation in the file, at any depth.
    #[must_use]
    pub fn includes(&self) -> &[Include] {
        &self.includes
    }

    /// Every string literal's value (`None` where an escape is not one Rust has), with its byte
    /// offset, but a `#[path = "…"]`'s and those an include's arguments hold, which
    /// [`Source::mounts`] and [`Source::includes`] read.
    pub fn strings(&self) -> impl Iterator<Item = (Option<&str>, usize)> {
        self.strings
            .iter()
            .map(|(value, at)| (value.as_deref(), *at))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Ident,
    Lifetime,
    Str,
    Char,
    Number,
    Punct,
    Comment,
    OuterDoc,
    InnerDoc,
    Open,
    Close,
}

#[derive(Clone, Copy, Debug)]
struct Tok {
    kind: Kind,
    start: usize,
    end: usize,
}

#[derive(Debug)]
enum Tree {
    Leaf(Tok),
    Group {
        open: Tok,
        close: Tok,
        children: Vec<Tree>,
    },
}

impl Tree {
    const fn start(&self) -> usize {
        match self {
            Self::Leaf(tok) | Self::Group { open: tok, .. } => tok.start,
        }
    }

    const fn end(&self) -> usize {
        match self {
            Self::Leaf(tok) | Self::Group { close: tok, .. } => tok.end,
        }
    }
}

const fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b >= 0x80
}

const fn is_ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

fn ident_end(b: &[u8], mut i: usize) -> usize {
    while b.get(i).copied().is_some_and(is_ident) {
        i += 1;
    }
    i
}

/// A number's end: its digits, suffix and a `.` that a digit follows (`1.5`, never `1..2`).
fn number_end(b: &[u8], mut i: usize) -> usize {
    while let Some(&c) = b.get(i) {
        let fraction = c == b'.'
            && b.get(i + 1).is_some_and(u8::is_ascii_digit)
            && b.get(i.wrapping_sub(1)) != Some(&b'.');
        if c.is_ascii_alphanumeric() || c == b'_' || fraction {
            i += 1;
        } else {
            break;
        }
    }
    i
}

/// The file's tokens, or `None` where a literal or comment does not end, or one starts inside a
/// token read here (which would mean the two readings disagree).
fn tokenize(src: &str) -> Option<Vec<Tok>> {
    let b = src.as_bytes();
    let mut spans = lex::spans(src).into_iter().peekable();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if let Some(span) = spans.peek().copied() {
            if span.start < i {
                return None;
            }
            if span.start == i {
                spans.next();
                out.push(Tok {
                    kind: literal_kind(src, &span)?,
                    start: i,
                    end: span.end,
                });
                i = span.end;
                continue;
            }
        }
        let c = b[i];
        let start = i;
        let kind = if c.is_ascii_whitespace() {
            i += 1;
            continue;
        } else if is_ident_start(c) {
            i = ident_end(b, i);
            if &b[start..i] == b"r"
                && b.get(i) == Some(&b'#')
                && b.get(i + 1).copied().is_some_and(is_ident_start)
            {
                i = ident_end(b, i + 1);
            }
            Kind::Ident
        } else if c.is_ascii_digit() {
            i = number_end(b, i);
            Kind::Number
        } else if c == b'\'' && b.get(i + 1).copied().is_some_and(is_ident_start) {
            i = ident_end(b, i + 1);
            Kind::Lifetime
        } else {
            i += 1;
            match c {
                b'(' | b'[' | b'{' => Kind::Open,
                b')' | b']' | b'}' => Kind::Close,
                _ => Kind::Punct,
            }
        };
        if spans.peek().is_some_and(|span| span.start < i) {
            return None;
        }
        out.push(Tok {
            kind,
            start,
            end: i,
        });
    }
    Some(out)
}

fn literal_kind(src: &str, span: &lex::Span) -> Option<Kind> {
    let text = &src[span.start..span.end];
    match span.kind {
        SpanKind::Str => span.quotes.map(|_| Kind::Str),
        SpanKind::Char => Some(Kind::Char),
        SpanKind::Comment if text.starts_with("/*") && !closes(text) => None,
        SpanKind::Comment => Some(
            if (text.starts_with("///") && !text.starts_with("////"))
                || (text.starts_with("/**") && !text.starts_with("/***") && text != "/**/")
            {
                Kind::OuterDoc
            } else if text.starts_with("//!") || text.starts_with("/*!") {
                Kind::InnerDoc
            } else {
                Kind::Comment
            },
        ),
    }
}

/// Whether a block comment's nesting closes exactly at its last byte.
fn closes(comment: &str) -> bool {
    let b = comment.as_bytes();
    let mut depth = 0usize;
    let mut j = 0;
    while j < b.len() {
        match (b[j], b.get(j + 1)) {
            (b'/', Some(b'*')) => {
                depth += 1;
                j += 2;
            }
            (b'*', Some(b'/')) => {
                let Some(less) = depth.checked_sub(1) else {
                    return false;
                };
                depth = less;
                j += 2;
                if depth == 0 {
                    return j == b.len();
                }
            }
            _ => j += 1,
        }
    }
    false
}

/// Nest the tokens by their delimiters, or `None` where one does not close as it opened.
fn build(src: &str, toks: Vec<Tok>) -> Option<Vec<Tree>> {
    let b = src.as_bytes();
    let mut stack: Vec<(Tok, Vec<Tree>)> = Vec::new();
    let mut top: Vec<Tree> = Vec::new();
    for tok in toks {
        match tok.kind {
            Kind::Open => stack.push((tok, std::mem::take(&mut top))),
            Kind::Close => {
                let (open, parent) = stack.pop()?;
                let pair = (b[open.start], b[tok.start]);
                if !matches!(pair, (b'(', b')') | (b'[', b']') | (b'{', b'}')) {
                    return None;
                }
                let children = std::mem::replace(&mut top, parent);
                top.push(Tree::Group {
                    open,
                    close: tok,
                    children,
                });
            }
            _ => top.push(Tree::Leaf(tok)),
        }
    }
    stack.is_empty().then_some(top)
}

/// The indices of `trees` that are not plain comments: what the item grammar reads.
fn significant(trees: &[Tree]) -> Vec<usize> {
    trees
        .iter()
        .enumerate()
        .filter(|(_, tree)| {
            !matches!(
                tree,
                Tree::Leaf(Tok {
                    kind: Kind::Comment,
                    ..
                })
            )
        })
        .map(|(i, _)| i)
        .collect()
}

/// An identifier's text as written: `r#mod` is no keyword.
fn word<'a>(src: &'a str, tree: &Tree) -> Option<&'a str> {
    match tree {
        Tree::Leaf(tok) if tok.kind == Kind::Ident => Some(&src[tok.start..tok.end]),
        _ => None,
    }
}

/// A module's name: what its file is named after (`r#type` is `type.rs`).
fn module_name(name: &str) -> String {
    name.strip_prefix("r#").unwrap_or(name).to_string()
}

fn is_punct(src: &str, tree: &Tree, ch: u8) -> bool {
    matches!(tree, Tree::Leaf(tok) if tok.kind == Kind::Punct && src.as_bytes()[tok.start] == ch)
}

fn delimiter(src: &str, tree: &Tree) -> Option<u8> {
    match tree {
        Tree::Group { open, .. } => Some(src.as_bytes()[open.start]),
        Tree::Leaf(_) => None,
    }
}

fn kind_of(tree: &Tree) -> Option<Kind> {
    match tree {
        Tree::Leaf(tok) => Some(tok.kind),
        Tree::Group { .. } => None,
    }
}

/// An attribute's content: what its brackets hold, or nothing for a doc comment.
type Attr<'t> = &'t [Tree];

/// The inner attributes that open `trees` (`#![…]`, `//!`), and the index in [`significant`]
/// order where what follows them starts.
fn inner_attributes<'t>(src: &str, trees: &'t [Tree]) -> (Vec<Attr<'t>>, usize) {
    let sig = significant(trees);
    let at = |k: usize| sig.get(k).map(|&i| &trees[i]);
    let mut attrs = Vec::new();
    let mut p = 0;
    loop {
        if at(p).and_then(kind_of) == Some(Kind::InnerDoc) {
            attrs.push(&[][..]);
            p += 1;
        } else if at(p).is_some_and(|t| is_punct(src, t, b'#'))
            && at(p + 1).is_some_and(|t| is_punct(src, t, b'!'))
            && let Some(Tree::Group { children, .. }) = at(p + 2)
            && delimiter(src, &trees[sig[p + 2]]) == Some(b'[')
        {
            attrs.push(children);
            p += 3;
        } else {
            return (attrs, p);
        }
    }
}

/// Whether an attribute is `cfg(P)` with `P` false whenever `test` is.
fn test_cfg(src: &str, attr: Attr<'_>) -> bool {
    let sig = significant(attr);
    let [name, args] = sig.as_slice() else {
        return false;
    };
    if word(src, &attr[*name]) != Some("cfg") || delimiter(src, &attr[*args]) != Some(b'(') {
        return false;
    }
    let Tree::Group { open, close, .. } = &attr[*args] else {
        return false;
    };
    when_test_disabled(&src[open.end..close.start]) == Truth::False
}

/// Built-in attributes that change no token of the item they sit on.
const INERT: &[&str] = &[
    "allow",
    "automatically_derived",
    "cfg",
    "cold",
    "crate_name",
    "crate_type",
    "debugger_visualizer",
    "deny",
    "deprecated",
    "derive",
    "doc",
    "expect",
    "export_name",
    "feature",
    "forbid",
    "ignore",
    "inline",
    "link",
    "link_name",
    "link_section",
    "macro_export",
    "macro_use",
    "must_use",
    "no_implicit_prelude",
    "no_main",
    "no_mangle",
    "no_std",
    "non_exhaustive",
    "path",
    "recursion_limit",
    "register_tool",
    "repr",
    "should_panic",
    "target_feature",
    "test",
    "track_caller",
    "type_length_limit",
    "used",
    "warn",
    "windows_subsystem",
];

/// Tool namespaces whose attributes no macro reads (`trust` is registered as a tool by the
/// workspace's crate roots).
const INERT_TOOLS: &[&str] = &["clippy", "diagnostic", "rustdoc", "rustfmt", "trust"];

/// Whether an attribute is one rustc keeps the item as written under: a built-in that rewrites
/// nothing ([`INERT`]), a tool attribute ([`INERT_TOOLS`]), `unsafe(…)` of one, or a `cfg_attr`
/// whose every attribute is. A doc comment is inert.
fn inert(src: &str, attr: Attr<'_>) -> bool {
    let sig = significant(attr);
    let Some(&first) = sig.first() else {
        return true;
    };
    let Some(name) = word(src, &attr[first]) else {
        return false;
    };
    if sig.len() >= 3 && is_punct(src, &attr[sig[1]], b':') && is_punct(src, &attr[sig[2]], b':') {
        return INERT_TOOLS.contains(&name);
    }
    let args = match sig.get(1).map(|&i| &attr[i]) {
        Some(tree @ Tree::Group { children, .. })
            if sig.len() == 2 && delimiter(src, tree) == Some(b'(') =>
        {
            Some(children.as_slice())
        }
        _ => None,
    };
    match (name, args) {
        ("cfg_attr", Some(args)) => {
            let parts = comma_parts(src, args);
            parts.len() >= 2 && parts[1..].iter().all(|part| inert(src, part))
        }
        ("unsafe", Some(args)) => inert(src, args),
        ("cfg_attr" | "unsafe", None) => false,
        _ => INERT.contains(&name),
    }
}

/// Whether a macro may read the item an attribute sits on, the attribute and its doc comments
/// included: it is not [`inert`] (an attribute macro), or it is a `derive` or a `doc = …` (whose
/// value a macro may compute), alone, under `cfg_attr` or under `unsafe(…)`. A doc comment and
/// the list form `doc(…)` do not carry.
fn carrying(src: &str, attr: Attr<'_>) -> bool {
    if !inert(src, attr) {
        return true;
    }
    let sig = significant(attr);
    let Some(name) = sig.first().and_then(|&i| word(src, &attr[i])) else {
        return false;
    };
    let args = match sig.as_slice() {
        [_, group] if delimiter(src, &attr[*group]) == Some(b'(') => match &attr[*group] {
            Tree::Group { children, .. } => Some(children.as_slice()),
            Tree::Leaf(_) => None,
        },
        _ => None,
    };
    match (name, args) {
        ("derive", _) => true,
        ("doc", _) => sig.get(1).is_some_and(|&i| is_punct(src, &attr[i], b'=')),
        ("cfg_attr", Some(args)) => comma_parts(src, args)
            .get(1..)
            .is_some_and(|parts| parts.iter().any(|part| carrying(src, part))),
        ("unsafe", Some(args)) => carrying(src, args),
        _ => false,
    }
}

/// Whether `trees` holds, at any depth, an attribute (`#[…]`, `#![…]`) that carries
/// ([`carrying`]).
fn holds_carrying(src: &str, trees: &[Tree]) -> bool {
    let sig = significant(trees);
    let punct_at = |k: usize, ch: u8| sig.get(k).is_some_and(|&i| is_punct(src, &trees[i], ch));
    sig.iter().enumerate().any(|(k, &i)| match &trees[i] {
        Tree::Group { children, .. } => {
            let attr = delimiter(src, &trees[i]) == Some(b'[')
                && (punct_at(k.wrapping_sub(1), b'#')
                    || (punct_at(k.wrapping_sub(1), b'!') && punct_at(k.wrapping_sub(2), b'#')));
            (attr && carrying(src, children)) || holds_carrying(src, children)
        }
        Tree::Leaf(_) => false,
    })
}

/// `ranges` sorted, with those that overlap or touch merged.
fn disjoint(mut ranges: Vec<(usize, usize)>) -> Vec<(usize, usize)> {
    ranges.sort_unstable();
    let mut out: Vec<(usize, usize)> = Vec::with_capacity(ranges.len());
    for (start, end) in ranges {
        match out.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => out.push((start, end)),
        }
    }
    out
}

/// `trees` split at its top-level commas, a trailing empty part dropped.
fn comma_parts<'t>(src: &str, trees: &'t [Tree]) -> Vec<&'t [Tree]> {
    let mut parts: Vec<&[Tree]> = trees.split(|tree| is_punct(src, tree, b',')).collect();
    if parts.len() > 1
        && parts
            .last()
            .is_some_and(|part| significant(part).is_empty())
    {
        parts.pop();
    }
    parts
}

/// What a module declaration's attributes say its file is.
fn mount_path(src: &str, attrs: &[Attr<'_>]) -> MountPath {
    let mut found = MountPath::Default;
    for attr in attrs {
        let sig = significant(attr);
        let named_path = sig
            .first()
            .is_some_and(|&i| word(src, &attr[i]) == Some("path"));
        if named_path {
            let value = match sig.as_slice() {
                [_, eq, value] if is_punct(src, &attr[*eq], b'=') => {
                    string_value(src, &attr[*value])
                }
                _ => None,
            };
            found = match (found, value) {
                (MountPath::Default, Some(value)) => MountPath::Literal(value),
                _ => return MountPath::Unsettled,
            };
        } else if mentions(src, attr, "path") {
            return MountPath::Unsettled;
        }
    }
    found
}

/// Whether `trees` holds the identifier `name` at any depth.
fn mentions(src: &str, trees: &[Tree], name: &str) -> bool {
    trees.iter().any(|tree| match tree {
        Tree::Leaf(_) => word(src, tree) == Some(name),
        Tree::Group { children, .. } => mentions(src, children, name),
    })
}

/// A string literal's value, or `None` for any other tree or an escape this reader does not
/// decode.
fn string_value(src: &str, tree: &Tree) -> Option<String> {
    match tree {
        Tree::Leaf(tok) if tok.kind == Kind::Str => decode(&src[tok.start..tok.end]),
        _ => None,
    }
}

fn decode(literal: &str) -> Option<String> {
    let body = literal.trim_start_matches(['b', 'c']);
    if let Some(raw) = body.strip_prefix('r') {
        let hashes = raw.len() - raw.trim_start_matches('#').len();
        let fence = "#".repeat(hashes);
        return raw[hashes..]
            .strip_prefix('"')?
            .strip_suffix(fence.as_str())?
            .strip_suffix('"')
            .map(str::to_string);
    }
    let inner = body.strip_prefix('"')?.strip_suffix('"')?;
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next()? {
            'n' => out.push('\n'),
            't' => out.push('\t'),
            'r' => out.push('\r'),
            '0' => out.push('\0'),
            '\\' => out.push('\\'),
            '"' => out.push('"'),
            '\'' => out.push('\''),
            'x' => {
                let hex: String = chars.by_ref().take(2).collect();
                out.push(char::from(u8::from_str_radix(&hex, 16).ok()?));
            }
            'u' => {
                let rest = chars.as_str().strip_prefix('{')?;
                let (hex, after) = rest.split_once('}')?;
                out.push(char::from_u32(
                    u32::from_str_radix(&hex.replace('_', ""), 16).ok()?,
                )?);
                chars = after.chars();
            }
            '\n' => chars = chars.as_str().trim_start().chars(),
            _ => return None,
        }
    }
    Some(out)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    Fn,
    ModFile,
    ModInline,
    Struct,
    Trait,
    Impl,
    Semi,
    ExternBlock,
    Macro,
    Unknown,
}

/// What [`items`] and [`scan`] find.
#[derive(Default)]
struct Found {
    test: Vec<(usize, usize)>,
    test_modules: Vec<TestModule>,
    /// The `mod` offsets of the declarations [`items`] placed.
    placed: BTreeSet<usize>,
    mounts: Vec<Mount>,
    includes: Vec<Include>,
    strings: Vec<(Option<String>, usize)>,
    /// The offsets of the `#[path = "…"]` literals and those an include's arguments hold.
    path_strings: BTreeSet<usize>,
    /// The byte ranges a macro may read.
    carriers: Vec<(usize, usize)>,
    /// The opening offsets of the groups whose items [`items`] read.
    read: BTreeSet<usize>,
}

/// Where a level of items [`items`] reads is.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Level {
    /// The top of the file.
    Top,
    /// An inline module's body.
    Inline,
    /// An `impl` or `trait` body, where no module is declared.
    Body,
}

/// Read the items of one level, from `from` (an index in [`significant`] order): mark each
/// test-only one, place each module declaration, mark what a macro may read, and read on inside
/// the containers whose attributes are inert. `within` names the inline modules this level is in.
fn items(
    src: &str,
    trees: &[Tree],
    from: usize,
    level: Level,
    within: &mut Vec<(String, bool)>,
    found: &mut Found,
) {
    let sig = significant(trees);
    let level_end = trees.last().map_or(0, Tree::end);
    // Where the reader cannot end an item, what follows is unread: a macro may read any of it.
    let unread = |from: usize, found: &mut Found| {
        if let Some(&i) = sig.get(from) {
            found.carriers.push((trees[i].start(), level_end));
        }
    };
    let mut p = from;
    while p < sig.len() {
        let first = p;
        let mut attrs: Vec<Attr<'_>> = Vec::new();
        loop {
            match sig.get(p).map(|&i| &trees[i]) {
                Some(tree) if kind_of(tree) == Some(Kind::OuterDoc) => {
                    attrs.push(&[]);
                    p += 1;
                }
                Some(tree)
                    if is_punct(src, tree, b'#')
                        && sig
                            .get(p + 1)
                            .is_some_and(|&i| delimiter(src, &trees[i]) == Some(b'[')) =>
                {
                    if let Tree::Group { children, .. } = &trees[sig[p + 1]] {
                        attrs.push(children);
                    }
                    p += 2;
                }
                _ => break,
            }
        }
        if p >= sig.len() {
            unread(first, found);
            return;
        }
        let (end, shape, body) = item_end(src, trees, &sig, p);
        let Some(end) = end else {
            unread(first, found);
            return;
        };
        let span = (trees[sig[first]].start(), trees[sig[end]].end());
        let test = shape != Shape::Unknown && attrs.iter().any(|attr| test_cfg(src, attr));
        if test {
            found.test.push(span);
            if shape == Shape::ModInline
                && level != Level::Body
                && let Tree::Group { open, close, .. } = &trees[sig[end]]
                && let Some(name) = word(src, &trees[sig[end - 1]])
            {
                found.test_modules.push(TestModule {
                    name: module_name(name),
                    within: within.iter().map(|(name, _)| name.clone()).collect(),
                    pathed: within.iter().any(|&(_, pathed)| pathed),
                    inner_gate: false,
                    rewritten: !attrs.iter().all(|attr| inert(src, attr)),
                    item: span,
                    head: trees[sig[p]].start(),
                    open: open.start,
                    body: (open.end, close.start),
                });
            }
        }
        // From its first attribute to an end at or past its real one.
        let whole = (
            span.0,
            trees[sig[item_extent(src, trees, &sig, p).max(end)]].end(),
        );
        let carried = attrs.iter().any(|attr| carrying(src, attr));
        // An item led by a word this reader cannot name may be another front end's input, read as
        // text: a Trust `clean { … }` island is Lean to the compiler, where a span that lexes as a
        // Rust comment is code. (One led by punctuation is the rest of an item [`item_end`] ended
        // early, as at a `{ N }` const default, whose kind it named.)
        let foreign = shape == Shape::Unknown && word(src, &trees[sig[p]]).is_some();
        if shape == Shape::Macro {
            found.carriers.push(span);
        } else if carried || foreign {
            found.carriers.push(whole);
        }
        if shape == Shape::ModFile && level != Level::Body {
            let at = mod_keyword(src, trees, &sig, p, end);
            found.placed.insert(at);
            found.mounts.push(Mount {
                name: sig
                    .get(end.wrapping_sub(1))
                    .and_then(|&i| word(src, &trees[i]))
                    .map(module_name),
                path: mount_path(src, &attrs),
                test,
                top: level == Level::Top,
                carried,
                at,
            });
        }
        if !test
            && attrs.iter().all(|attr| inert(src, attr))
            && matches!(shape, Shape::ModInline | Shape::Impl | Shape::Trait)
            && let Some(group @ Tree::Group { children, .. }) = body.map(|b| &trees[sig[b]])
        {
            let (inner, next) = inner_attributes(src, children);
            if inner.iter().any(|attr| carrying(src, attr)) {
                found.carriers.push(whole);
            }
            if shape == Shape::ModInline && inner.iter().any(|attr| test_cfg(src, attr)) {
                found.test.push(span);
                if level != Level::Body
                    && let Some(b) = body
                    && let Tree::Group { open, close, .. } = &trees[sig[b]]
                    && let Some(name) = word(src, &trees[sig[end - 1]])
                {
                    found.test_modules.push(TestModule {
                        name: module_name(name),
                        within: within.iter().map(|(name, _)| name.clone()).collect(),
                        pathed: within.iter().any(|&(_, pathed)| pathed),
                        inner_gate: true,
                        rewritten: false,
                        item: span,
                        head: trees[sig[p]].start(),
                        open: open.start,
                        body: (open.end, close.start),
                    });
                }
            } else if inner.iter().all(|attr| inert(src, attr)) {
                let level = if shape == Shape::ModInline {
                    Level::Inline
                } else {
                    Level::Body
                };
                found.read.insert(group.start());
                let name = (shape == Shape::ModInline)
                    .then(|| word(src, &trees[sig[end - 1]]).map(module_name))
                    .flatten();
                let pathed = !matches!(mount_path(src, &attrs), MountPath::Default)
                    || !matches!(mount_path(src, &inner), MountPath::Default);
                within.extend(name.clone().map(|name| (name, pathed)));
                items(src, children, next, level, within, found);
                if name.is_some() {
                    within.pop();
                }
            }
        }
        p = end + 1;
    }
}

/// The offset of the `mod` of the module declaration that runs from `p` to `end`.
fn mod_keyword(src: &str, trees: &[Tree], sig: &[usize], p: usize, end: usize) -> usize {
    (p..end)
        .map(|k| &trees[sig[k]])
        .find(|tree| word(src, tree) == Some("mod"))
        .map_or_else(|| trees[sig[p]].start(), Tree::start)
}

/// Where the item starting at `p` (after its attributes) ends, what it is, and the body a
/// container reads on in. The end is `None` where nothing ends it.
fn item_end(
    src: &str,
    trees: &[Tree],
    sig: &[usize],
    p: usize,
) -> (Option<usize>, Shape, Option<usize>) {
    let at = |k: usize| sig.get(k).map(|&i| &trees[i]);
    let word_at = |k: usize| at(k).and_then(|t| word(src, t));
    let punct_at = |k: usize, ch: u8| at(k).is_some_and(|t| is_punct(src, t, ch));
    let delimiter_at = |k: usize| at(k).and_then(|t| delimiter(src, t));
    // The first `;` or `{…}` at this level from `k`.
    let first_end =
        |k: usize| (k..sig.len()).find(|&e| punct_at(e, b';') || delimiter_at(e) == Some(b'{'));
    let semicolon = |k: usize| (k..sig.len()).find(|&e| punct_at(e, b';'));
    let mut q = p;
    if word_at(q) == Some("pub") {
        q += 1;
        if delimiter_at(q) == Some(b'(') {
            q += 1;
        }
    }
    loop {
        match word_at(q) {
            Some("unsafe" | "async" | "default" | "auto" | "safe") => q += 1,
            Some("const")
                if matches!(
                    word_at(q + 1),
                    Some("fn" | "unsafe" | "async" | "extern" | "trait" | "impl")
                ) =>
            {
                q += 1;
            }
            Some("extern") => {
                if word_at(q + 1) == Some("crate") {
                    return (semicolon(q), Shape::Semi, None);
                }
                let r = if at(q + 1).and_then(kind_of) == Some(Kind::Str) {
                    q + 2
                } else {
                    q + 1
                };
                if delimiter_at(r) == Some(b'{') {
                    return (Some(r), Shape::ExternBlock, Some(r));
                }
                q = r;
            }
            _ => break,
        }
    }
    // A macro invocation or `macro_rules!`: a path, `!`, its arguments, and a `;` unless braced.
    let mut m = q;
    while word_at(m).is_some() && punct_at(m + 1, b':') && punct_at(m + 2, b':') {
        m += 3;
    }
    if word_at(m).is_some() && punct_at(m + 1, b'!') {
        let rules = m == q && word_at(m) == Some("macro_rules");
        let args = if rules { m + 3 } else { m + 2 };
        if rules && word_at(m + 2).is_none() {
            return (first_end(q), Shape::Unknown, None);
        }
        return match delimiter_at(args) {
            Some(b'{') => (Some(args), Shape::Macro, None),
            Some(_) if punct_at(args + 1, b';') => (Some(args + 1), Shape::Macro, None),
            _ => (first_end(q), Shape::Unknown, None),
        };
    }
    let shape = match word_at(q) {
        Some("fn") => Shape::Fn,
        Some("mod") if word_at(q + 1).is_some() && punct_at(q + 2, b';') => {
            return (Some(q + 2), Shape::ModFile, None);
        }
        Some("mod") if word_at(q + 1).is_some() && delimiter_at(q + 2) == Some(b'{') => {
            return (Some(q + 2), Shape::ModInline, Some(q + 2));
        }
        Some("struct" | "enum") => Shape::Struct,
        Some("union") if word_at(q + 1).is_some() => Shape::Struct,
        Some("trait") => Shape::Trait,
        Some("impl") => Shape::Impl,
        Some("type" | "use" | "static" | "const") => return (semicolon(q), Shape::Semi, None),
        _ => return (first_end(q), Shape::Unknown, None),
    };
    let end = first_end(q);
    let body = end
        .filter(|&e| delimiter_at(e) == Some(b'{') && matches!(shape, Shape::Trait | Shape::Impl));
    (end, shape, body)
}

/// What [`scan`] knows of the carriers in what it reads.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Carriers {
    /// Its items' carriers are [`items`]'; a group it did not read may hold a carrying attribute.
    Open,
    /// It is in a group no reader read that holds no carrying attribute: only a macro's input
    /// and a `clean { … }` island carry.
    Clean,
    /// It is in a carrier already: all of it is.
    Covered,
}

/// Where the item starting at `p` (after its attributes) may end, at or past its real end, unlike
/// [`item_end`]: the first `;` or `{…}` outside every `<…>` (a `>` of `->` or `=>` closes none),
/// or the level's last tree where there is none. A const or static whose value is a block ends
/// later, at the `;` [`item_end`] finds.
fn item_extent(src: &str, trees: &[Tree], sig: &[usize], p: usize) -> usize {
    item_terminator(src, trees, sig, p).unwrap_or_else(|| sig.len().saturating_sub(1))
}

fn item_terminator(src: &str, trees: &[Tree], sig: &[usize], p: usize) -> Option<usize> {
    let mut depth = 0usize;
    for k in p..sig.len() {
        let tree = &trees[sig[k]];
        if depth == 0 && (is_punct(src, tree, b';') || delimiter(src, tree) == Some(b'{')) {
            return Some(k);
        }
        if is_punct(src, tree, b'<') {
            depth += 1;
        } else if is_punct(src, tree, b'>') {
            let arrow = k > p
                && (is_punct(src, &trees[sig[k - 1]], b'-')
                    || is_punct(src, &trees[sig[k - 1]], b'='));
            if !arrow {
                depth = depth.saturating_sub(1);
            }
        }
    }
    None
}

/// Real body extents for caller-selected item starts. Each pair is the
/// opening byte and its exclusive header bound. This determines extent only,
/// not whether the caller's opening is a Rust item or an associated owner.
/// Const-expression token groups remain opaque while the existing item
/// terminator walks angle depth; a bodyless alias yields no body. Return the
/// header with comments, literals and group interiors masked, so their comparisons and
/// keywords cannot change the caller's lexical owner/type-parameter reading.
pub(crate) fn item_bodies_at(
    src: &str,
    starts: &[(usize, usize)],
) -> BTreeMap<usize, (String, usize)> {
    fn visit(
        src: &str,
        trees: &[Tree],
        starts: &BTreeMap<usize, usize>,
        bodies: &mut BTreeMap<usize, (String, usize)>,
    ) {
        let sig = significant(trees);
        for (p, &index) in sig.iter().enumerate() {
            let tree = &trees[index];
            if let Some(&limit) = starts.get(&tree.start()) {
                let upper = sig.partition_point(|&index| trees[index].start() < limit);
                if p < upper
                    && let Some(end) = item_terminator(src, trees, &sig[..upper], p)
                {
                    let body = &trees[sig[end]];
                    if delimiter(src, body) == Some(b'{') {
                        let start = tree.start();
                        let mut header = src.as_bytes()[start..body.start()].to_vec();
                        for part in &trees[index..sig[end]] {
                            let masked = match part {
                                Tree::Group { open, close, .. } => Some((open.end, close.start)),
                                Tree::Leaf(token)
                                    if matches!(
                                        token.kind,
                                        Kind::Comment
                                            | Kind::OuterDoc
                                            | Kind::InnerDoc
                                            | Kind::Str
                                            | Kind::Char
                                    ) =>
                                {
                                    Some((token.start, token.end))
                                }
                                _ => None,
                            };
                            if let Some((from, to)) = masked {
                                header[from - start..to - start].fill(b' ');
                            }
                        }
                        // Whole token/group boundaries are UTF-8 boundaries;
                        // replacing their bytes with ASCII preserves UTF-8.
                        let header =
                            String::from_utf8(header).expect("masked source header is UTF-8");
                        bodies.insert(start, (header, body.end()));
                    }
                }
            }
            if let Tree::Group { children, .. } = tree {
                visit(src, children, starts, bodies);
            }
        }
    }
    if starts.is_empty() {
        return BTreeMap::new();
    }
    let Some(tokens) = tokenize(src) else {
        return BTreeMap::new();
    };
    let Some(trees) = build(src, tokens) else {
        return BTreeMap::new();
    };
    let mut bodies = BTreeMap::new();
    visit(src, &trees, &starts.iter().copied().collect(), &mut bodies);
    bodies
}

/// Collect, at any depth: module declarations [`items`] did not place (each one this reader cannot
/// vouch for), `include!`-family invocations, string literals, and what a macro or another front
/// end may read outside the items [`items`] read.
fn scan(
    src: &str,
    trees: &[Tree],
    in_macro: bool,
    carriers: Carriers,
    placed: &BTreeSet<usize>,
    found: &mut Found,
) {
    let sig = significant(trees);
    let at = |k: usize| sig.get(k).map(|&i| &trees[i]);
    let word_at = |k: usize| at(k).and_then(|t| word(src, t));
    let punct_at = |k: usize, ch: u8| at(k).is_some_and(|t| is_punct(src, t, ch));
    let mut macro_bodies: BTreeSet<usize> = BTreeSet::new();
    for (k, &i) in sig.iter().enumerate() {
        match &trees[i] {
            Tree::Group { children, .. } => {
                if punct_at(k.wrapping_sub(1), b'#')
                    && let [name, eq, value] = significant(children).as_slice()
                    && word(src, &children[*name]) == Some("path")
                    && is_punct(src, &children[*eq], b'=')
                {
                    found.path_strings.insert(children[*value].start());
                }
                let args = punct_at(k.wrapping_sub(1), b'!')
                    || (word_at(k.wrapping_sub(1)).is_some() && punct_at(k.wrapping_sub(2), b'!'))
                    || punct_at(k.wrapping_sub(1), b'#')
                    || (punct_at(k.wrapping_sub(1), b'!') && punct_at(k.wrapping_sub(2), b'#'));
                let body = in_macro || args || macro_bodies.contains(&k);
                let tree = &trees[i];
                // A Trust `clean { … }` island the item reader did not read, whose text the Clean
                // front end reads ([`items`]).
                let island = word_at(k.wrapping_sub(1)) == Some("clean")
                    && delimiter(src, tree) == Some(b'{');
                // A macro reads its input, and a group whose items no reader read is a macro's
                // where an attribute in it carries.
                let below = match carriers {
                    Carriers::Covered => Carriers::Covered,
                    _ if body || island => {
                        found.carriers.push((tree.start(), tree.end()));
                        Carriers::Covered
                    }
                    Carriers::Open if !found.read.contains(&tree.start()) => {
                        if holds_carrying(src, children) {
                            found.carriers.push((tree.start(), tree.end()));
                            Carriers::Covered
                        } else {
                            Carriers::Clean
                        }
                    }
                    open_or_clean => open_or_clean,
                };
                scan(src, children, body, below, placed, found);
            }
            Tree::Leaf(tok) => {
                let name = word(src, &trees[i]);
                if tok.kind == Kind::Str {
                    found
                        .strings
                        .push((decode(&src[tok.start..tok.end]), tok.start));
                }
                if name == Some("macro") && word_at(k + 1).is_some() {
                    macro_bodies.extend([k + 2, k + 3]);
                }
                if name == Some("mod") && !placed.contains(&tok.start) {
                    let named = word_at(k + 1).map(module_name);
                    let file = if punct_at(k + 1, b'$') {
                        word_at(k + 2).is_some() && punct_at(k + 3, b';')
                    } else {
                        named.is_some() && punct_at(k + 2, b';')
                    };
                    if file {
                        found.mounts.push(Mount {
                            name: if in_macro { None } else { named },
                            path: MountPath::Unsettled,
                            test: false,
                            top: false,
                            carried: true,
                            at: tok.start,
                        });
                    }
                }
                if let Some(kind @ ("include" | "include_str" | "include_bytes")) = name
                    && punct_at(k + 1, b'!')
                    && let Some(Tree::Group { children, .. }) = at(k + 2)
                {
                    literals(children, &mut found.path_strings);
                    found.includes.push(Include {
                        kind: match kind {
                            "include" => IncludeKind::Tokens,
                            "include_str" => IncludeKind::Text,
                            _ => IncludeKind::Bytes,
                        },
                        arg: include_arg(src, children),
                        in_macro,
                        at: tok.start,
                    });
                }
            }
        }
    }
}

/// The offsets of every string literal in `trees`, at any depth.
fn literals(trees: &[Tree], into: &mut BTreeSet<usize>) {
    for tree in trees {
        match tree {
            Tree::Leaf(tok) if tok.kind == Kind::Str => {
                into.insert(tok.start);
            }
            Tree::Leaf(_) => {}
            Tree::Group { children, .. } => literals(children, into),
        }
    }
}

/// What an `include!`-family invocation's arguments name.
fn include_arg(src: &str, args: &[Tree]) -> IncludeArg {
    let parts = comma_parts(src, args);
    let [part] = parts.as_slice() else {
        return IncludeArg::Unknown;
    };
    let sig = significant(part);
    match sig.as_slice() {
        [one] => string_value(src, &part[*one]).map_or(IncludeArg::Unknown, IncludeArg::Relative),
        [name, bang, group]
            if word(src, &part[*name]) == Some("concat") && is_punct(src, &part[*bang], b'!') =>
        {
            let Tree::Group { children, .. } = &part[*group] else {
                return IncludeArg::Unknown;
            };
            concat_arg(src, children)
        }
        _ => IncludeArg::Unknown,
    }
}

/// `concat!`'s arguments as a path: string literals, optionally after one `env!` of the package
/// or build-output directory.
fn concat_arg(src: &str, args: &[Tree]) -> IncludeArg {
    let mut base: Option<&str> = None;
    let mut path = String::new();
    for (n, part) in comma_parts(src, args).into_iter().enumerate() {
        let sig = significant(part);
        match sig.as_slice() {
            [one] => match string_value(src, &part[*one]) {
                Some(piece) => path.push_str(&piece),
                None => return IncludeArg::Unknown,
            },
            [name, bang, group]
                if n == 0
                    && word(src, &part[*name]) == Some("env")
                    && is_punct(src, &part[*bang], b'!') =>
            {
                let Tree::Group { children, .. } = &part[*group] else {
                    return IncludeArg::Unknown;
                };
                let var = significant(children);
                match var.as_slice() {
                    [one] => match string_value(src, &children[*one]).as_deref() {
                        Some("OUT_DIR") => base = Some("OUT_DIR"),
                        Some("CARGO_MANIFEST_DIR") => base = Some("CARGO_MANIFEST_DIR"),
                        _ => return IncludeArg::Unknown,
                    },
                    _ => return IncludeArg::Unknown,
                }
            }
            _ => return IncludeArg::Unknown,
        }
    }
    match base {
        Some("OUT_DIR") => IncludeArg::OutDir,
        Some(_) => IncludeArg::ManifestDir(path),
        None => IncludeArg::Relative(path),
    }
}

#[cfg(test)]
#[path = "items_tests.rs"]
mod tests;
