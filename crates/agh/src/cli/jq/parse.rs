// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The jq parser: design §8.4's grammar and precedence, from `|` (lowest) through `,`, `//`, `or`,
//! `and`, the comparisons and `+`/`-` to the postfix steps.

use aterm_json::Value;

use super::lex::{CmpOp, Piece, Spanned, Tok};
use super::{MAX_DEPTH, find_function, refused, refused_name, syntax, too_deep, unknown_name};
use crate::error::Result;

/// `+` or `-`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ArithOp {
    /// `+`.
    Add,
    /// `-`.
    Sub,
}

/// One piece of a string expression.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum StrPiece {
    /// Literal text.
    Lit(String),
    /// `\(pipeline)`.
    Interp(Ast),
}

/// The tree.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Ast {
    /// `.`.
    Identity,
    /// A literal value.
    Literal(Value),
    /// A string, interpolated.
    Str(Vec<StrPiece>),
    /// `$name`.
    Var(String),
    /// `target[key]` (and `.key`, `."key"`): the key runs on the input, not the target. The flag
    /// is a `?` right after the step: a target the key cannot index is skipped.
    Index(Box<Ast>, Box<Ast>, bool),
    /// `target[lo:hi]`, and its `?` flag, as `Index`'s.
    Slice(Box<Ast>, Option<Box<Ast>>, Option<Box<Ast>>, bool),
    /// `target[]`, and its `?` flag (jq's `EACH_OPT`): a target that cannot be iterated is skipped.
    Iterate(Box<Ast>, bool),
    /// `term?` after any term but a step: the term's errors end its stream quietly.
    Try(Box<Ast>),
    /// `a | b`.
    Pipe(Box<Ast>, Box<Ast>),
    /// `a, b`.
    Comma(Box<Ast>, Box<Ast>),
    /// `a // b`.
    Alt(Box<Ast>, Box<Ast>),
    /// `a or b`.
    Or(Box<Ast>, Box<Ast>),
    /// `a and b`.
    And(Box<Ast>, Box<Ast>),
    /// A comparison.
    Cmp(CmpOp, Box<Ast>, Box<Ast>),
    /// `+` or `-`.
    Arith(ArithOp, Box<Ast>, Box<Ast>),
    /// `[e]`, or `[]`.
    Array(Option<Box<Ast>>),
    /// `{k: v, …}`: each key and value expression.
    Object(Vec<(Ast, Ast)>),
    /// A function and its arguments.
    Call(super::Func, Vec<Ast>),
    /// `@name`.
    Format(String),
    /// `source as $name | body`.
    Bind(Box<Ast>, String, Box<Ast>),
}

/// A tree and its depth: a leaf is 1, a node one more than its deepest child.
type Node = (Ast, usize);

/// Parse a whole program's tokens; `end` is the program's length, for the error past its end.
pub(super) fn parse(tokens: &[Spanned], end: usize) -> Result<Ast> {
    parse_nested(tokens, end, 0).map(|(ast, _)| ast)
}

/// Parse tokens that sit `nest` levels inside the program (an interpolation's).
fn parse_nested(tokens: &[Spanned], end: usize, nest: usize) -> Result<Node> {
    let mut parser = Parser {
        tokens,
        pos: 0,
        end,
        nest,
    };
    let node = parser.pipeline()?;
    match parser.tokens.get(parser.pos) {
        None => Ok(node),
        Some(s) => Err(syntax(&format!("unexpected {}", describe(&s.tok)), s.at)),
    }
}

/// A token, as an error names it.
fn describe(tok: &Tok) -> String {
    match tok {
        Tok::Dot => ".".into(),
        Tok::Field(n) => format!(".{n}"),
        Tok::LBrack => "[".into(),
        Tok::RBrack => "]".into(),
        Tok::LBrace => "{".into(),
        Tok::RBrace => "}".into(),
        Tok::LParen => "(".into(),
        Tok::RParen => ")".into(),
        Tok::Pipe => "|".into(),
        Tok::Comma => ",".into(),
        Tok::Colon => ":".into(),
        Tok::Semi => ";".into(),
        Tok::Question => "?".into(),
        Tok::Alt => "//".into(),
        Tok::Cmp(op) => match op {
            CmpOp::Eq => "==",
            CmpOp::Ne => "!=",
            CmpOp::Lt => "<",
            CmpOp::Le => "<=",
            CmpOp::Gt => ">",
            CmpOp::Ge => ">=",
        }
        .into(),
        Tok::Plus => "+".into(),
        Tok::Minus => "-".into(),
        Tok::Var(n) => format!("${n}"),
        Tok::Format(n) => format!("@{n}"),
        Tok::Name(n) => n.clone(),
        Tok::Num(v) => v.to_string(),
        Tok::Str(_) => "a string".into(),
    }
}

/// The keywords: names that are neither functions nor refused.
const KEYWORDS: [&str; 6] = ["as", "and", "or", "true", "false", "null"];

/// The parser's state.
struct Parser<'a> {
    tokens: &'a [Spanned],
    pos: usize,
    end: usize,
    /// How deep the parser's own recursion is.
    nest: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Tok> {
        self.tokens.get(self.pos).map(|s| &s.tok)
    }

    fn peek_at(&self, ahead: usize) -> Option<&Tok> {
        self.tokens.get(self.pos + ahead).map(|s| &s.tok)
    }

    /// The offset of the next token, or the program's end.
    fn at(&self) -> usize {
        self.tokens.get(self.pos).map_or(self.end, |s| s.at)
    }

    fn is_name(&self, name: &str) -> bool {
        matches!(self.peek(), Some(Tok::Name(n)) if n == name)
    }

    /// The error for whatever stands next.
    fn unexpected(&self) -> crate::error::Error {
        match self.tokens.get(self.pos) {
            Some(s) => syntax(&format!("unexpected {}", describe(&s.tok)), s.at),
            None => syntax("unexpected end", self.end),
        }
    }

    fn expect(&mut self, tok: &Tok) -> Result<()> {
        if self.peek() == Some(tok) {
            self.pos += 1;
            Ok(())
        } else {
            Err(self.unexpected())
        }
    }

    /// One level deeper in the parser's recursion; an error past `MAX_DEPTH`. A failed parse
    /// is dropped whole, so only a successful one needs `leave`.
    fn enter(&mut self) -> Result<()> {
        if self.nest >= MAX_DEPTH {
            return Err(too_deep(self.at()));
        }
        self.nest += 1;
        Ok(())
    }

    const fn leave(&mut self) {
        self.nest -= 1;
    }

    /// The depth of a node whose deepest child is `child` deep; an error past `MAX_DEPTH`.
    fn over(&self, child: usize) -> Result<usize> {
        if child >= MAX_DEPTH {
            return Err(too_deep(self.at()));
        }
        Ok(child + 1)
    }

    /// `expr ("|" pipeline)?`.
    fn pipeline(&mut self) -> Result<Node> {
        self.enter()?;
        let (lhs, dl) = self.comma()?;
        let node = if self.peek() == Some(&Tok::Pipe) {
            self.pos += 1;
            let (rhs, dr) = self.pipeline()?;
            let depth = self.over(dl.max(dr))?;
            (Ast::Pipe(Box::new(lhs), Box::new(rhs)), depth)
        } else {
            (lhs, dl)
        };
        self.leave();
        Ok(node)
    }

    /// `alt ("," alt)*`.
    fn comma(&mut self) -> Result<Node> {
        let (mut lhs, mut depth) = self.alt()?;
        while self.peek() == Some(&Tok::Comma) {
            self.pos += 1;
            let (rhs, dr) = self.alt()?;
            depth = self.over(depth.max(dr))?;
            lhs = Ast::Comma(Box::new(lhs), Box::new(rhs));
        }
        Ok((lhs, depth))
    }

    /// `or ("//" alt)?`: right-associative, as jq's is.
    fn alt(&mut self) -> Result<Node> {
        let (lhs, dl) = self.or()?;
        if self.peek() == Some(&Tok::Alt) {
            self.pos += 1;
            self.enter()?;
            let (rhs, dr) = self.alt()?;
            self.leave();
            let depth = self.over(dl.max(dr))?;
            return Ok((Ast::Alt(Box::new(lhs), Box::new(rhs)), depth));
        }
        Ok((lhs, dl))
    }

    /// `and ("or" and)*`.
    fn or(&mut self) -> Result<Node> {
        let (mut lhs, mut depth) = self.and()?;
        while self.is_name("or") {
            self.pos += 1;
            let (rhs, dr) = self.and()?;
            depth = self.over(depth.max(dr))?;
            lhs = Ast::Or(Box::new(lhs), Box::new(rhs));
        }
        Ok((lhs, depth))
    }

    /// `cmp ("and" cmp)*`.
    fn and(&mut self) -> Result<Node> {
        let (mut lhs, mut depth) = self.cmp()?;
        while self.is_name("and") {
            self.pos += 1;
            let (rhs, dr) = self.cmp()?;
            depth = self.over(depth.max(dr))?;
            lhs = Ast::And(Box::new(lhs), Box::new(rhs));
        }
        Ok((lhs, depth))
    }

    /// `add (op add)?`: comparisons do not chain.
    fn cmp(&mut self) -> Result<Node> {
        let (lhs, dl) = self.add()?;
        if let Some(Tok::Cmp(op)) = self.peek() {
            let op = *op;
            self.pos += 1;
            let (rhs, dr) = self.add()?;
            let depth = self.over(dl.max(dr))?;
            return Ok((Ast::Cmp(op, Box::new(lhs), Box::new(rhs)), depth));
        }
        Ok((lhs, dl))
    }

    /// `postfix (("+" | "-") postfix)*`.
    fn add(&mut self) -> Result<Node> {
        let (mut lhs, mut depth) = self.postfix()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Plus) => ArithOp::Add,
                Some(Tok::Minus) => ArithOp::Sub,
                _ => return Ok((lhs, depth)),
            };
            self.pos += 1;
            let (rhs, dr) = self.postfix()?;
            depth = self.over(depth.max(dr))?;
            lhs = Ast::Arith(op, Box::new(lhs), Box::new(rhs));
        }
    }

    /// A primary and its postfix steps: `.name`, `."name"`, `[e]`, `[]`, `[a:b]` and `?`;
    /// then, when `as` follows, the binding `term as $NAME | pipeline`.
    fn postfix(&mut self) -> Result<Node> {
        let mut step = match self.peek() {
            Some(Tok::Field(_)) => true,
            Some(Tok::Dot) => matches!(self.peek_at(1), Some(Tok::Str(_))),
            _ => false,
        };
        let (mut term, mut depth) = self.primary()?;
        loop {
            match self.peek() {
                Some(Tok::Field(name)) => {
                    let key = Ast::Literal(Value::String(name.clone()));
                    self.pos += 1;
                    depth = self.over(depth)?;
                    term = Ast::Index(Box::new(term), Box::new(key), false);
                    step = true;
                }
                Some(Tok::Dot) => match self.peek_at(1) {
                    Some(Tok::Str(pieces)) => {
                        let (key, dk) = self.string(pieces)?;
                        self.pos += 2;
                        depth = self.over(depth.max(dk))?;
                        term = Ast::Index(Box::new(term), Box::new(key), false);
                        step = true;
                    }
                    Some(Tok::LBrack) => {
                        self.pos += 1;
                        (term, depth) = self.bracket(term, depth)?;
                        step = true;
                    }
                    _ => return Err(self.unexpected()),
                },
                Some(Tok::LBrack) => {
                    (term, depth) = self.bracket(term, depth)?;
                    step = true;
                }
                Some(Tok::Question) => {
                    self.pos += 1;
                    term = match term {
                        Ast::Index(target, key, false) if step => Ast::Index(target, key, true),
                        Ast::Slice(target, lo, hi, false) if step => {
                            Ast::Slice(target, lo, hi, true)
                        }
                        Ast::Iterate(target, false) if step => Ast::Iterate(target, true),
                        other => {
                            depth = self.over(depth)?;
                            Ast::Try(Box::new(other))
                        }
                    };
                    step = false;
                }
                _ if self.is_name("as") => return self.bind(term, depth),
                _ => return Ok((term, depth)),
            }
        }
    }

    /// `as` at `pos`, after `term`: `$NAME | pipeline`, or a refused destructuring.
    fn bind(&mut self, term: Ast, depth: usize) -> Result<Node> {
        self.pos += 1;
        let name = match self.peek() {
            Some(Tok::Var(name)) => name.clone(),
            Some(Tok::LBrack) => return Err(refused("as [")),
            Some(Tok::LBrace) => return Err(refused("as {")),
            _ => return Err(self.unexpected()),
        };
        self.pos += 1;
        self.expect(&Tok::Pipe)?;
        let (body, db) = self.pipeline()?;
        let depth = self.over(depth.max(db))?;
        Ok((Ast::Bind(Box::new(term), name, Box::new(body)), depth))
    }

    /// `[` at `pos`: `[]`, `[e]`, `[a:b]`, `[:b]` or `[a:]`.
    fn bracket(&mut self, term: Ast, depth: usize) -> Result<Node> {
        self.expect(&Tok::LBrack)?;
        if self.peek() == Some(&Tok::RBrack) {
            self.pos += 1;
            let depth = self.over(depth)?;
            return Ok((Ast::Iterate(Box::new(term), false), depth));
        }
        let lo = if self.peek() == Some(&Tok::Colon) {
            None
        } else {
            Some(self.pipeline()?)
        };
        let dlo = lo.as_ref().map_or(0, |(_, d)| *d);
        if self.peek() == Some(&Tok::Colon) {
            self.pos += 1;
            let hi = if self.peek() == Some(&Tok::RBrack) {
                None
            } else {
                Some(self.pipeline()?)
            };
            let dhi = hi.as_ref().map_or(0, |(_, d)| *d);
            self.expect(&Tok::RBrack)?;
            let depth = self.over(depth.max(dlo).max(dhi))?;
            let boxed = |bound: Option<Node>| bound.map(|(ast, _)| Box::new(ast));
            return Ok((
                Ast::Slice(Box::new(term), boxed(lo), boxed(hi), false),
                depth,
            ));
        }
        self.expect(&Tok::RBrack)?;
        match lo {
            Some((key, dk)) => {
                let depth = self.over(depth.max(dk))?;
                Ok((Ast::Index(Box::new(term), Box::new(key), false), depth))
            }
            None => Err(self.unexpected()),
        }
    }

    /// A string literal's pieces, each interpolation parsed whole, one level deeper.
    fn string(&self, pieces: &[Piece]) -> Result<Node> {
        let mut out = Vec::with_capacity(pieces.len());
        let mut deepest = 0;
        for piece in pieces {
            out.push(match piece {
                Piece::Lit(text) => StrPiece::Lit(text.clone()),
                Piece::Interp(tokens) => {
                    if self.nest >= MAX_DEPTH {
                        return Err(too_deep(self.at()));
                    }
                    let end = tokens.last().map_or(self.end, |s| s.at);
                    let (ast, depth) = parse_nested(tokens, end, self.nest + 1)?;
                    deepest = deepest.max(depth);
                    StrPiece::Interp(ast)
                }
            });
        }
        if let [StrPiece::Lit(text)] = out.as_slice() {
            return Ok((Ast::Literal(Value::String(text.clone())), 1));
        }
        Ok((Ast::Str(out), self.over(deepest)?))
    }

    fn primary(&mut self) -> Result<Node> {
        let Some(tok) = self.peek().cloned() else {
            return Err(self.unexpected());
        };
        let at = self.at();
        self.pos += 1;
        match tok {
            Tok::Dot => {
                if let Some(Tok::Str(pieces)) = self.peek() {
                    let (key, dk) = self.string(pieces)?;
                    self.pos += 1;
                    let depth = self.over(dk)?;
                    return Ok((
                        Ast::Index(Box::new(Ast::Identity), Box::new(key), false),
                        depth,
                    ));
                }
                Ok((Ast::Identity, 1))
            }
            Tok::Field(name) => Ok((
                Ast::Index(
                    Box::new(Ast::Identity),
                    Box::new(Ast::Literal(Value::String(name))),
                    false,
                ),
                2,
            )),
            Tok::Var(name) => Ok((Ast::Var(name), 1)),
            Tok::Num(value) => Ok((Ast::Literal(value), 1)),
            Tok::Str(pieces) => self.string(&pieces),
            Tok::Format(name) => Ok((Ast::Format(name), 1)),
            Tok::LParen => {
                let inner = self.pipeline()?;
                self.expect(&Tok::RParen)?;
                Ok(inner)
            }
            Tok::LBrack => {
                if self.peek() == Some(&Tok::RBrack) {
                    self.pos += 1;
                    return Ok((Ast::Array(None), 1));
                }
                let (inner, di) = self.pipeline()?;
                self.expect(&Tok::RBrack)?;
                let depth = self.over(di)?;
                Ok((Ast::Array(Some(Box::new(inner))), depth))
            }
            Tok::LBrace => {
                self.enter()?;
                let node = self.object()?;
                self.leave();
                Ok(node)
            }
            Tok::Name(name) => self.name(&name, at),
            _ => {
                self.pos -= 1;
                Err(self.unexpected())
            }
        }
    }

    /// A name where a value stands: a literal, a function call, or a refusal.
    fn name(&mut self, name: &str, at: usize) -> Result<Node> {
        match name {
            "true" => return Ok((Ast::Literal(Value::Bool(true)), 1)),
            "false" => return Ok((Ast::Literal(Value::Bool(false)), 1)),
            "null" => return Ok((Ast::Literal(Value::Null), 1)),
            _ => {}
        }
        if let Some(e) = refused_name(name) {
            return Err(e);
        }
        if KEYWORDS.contains(&name) {
            return Err(syntax(&format!("unexpected {name}"), at));
        }
        let Some(func) = find_function(name) else {
            return Err(unknown_name(name));
        };
        let mut args = Vec::new();
        let mut deepest = 0;
        if self.peek() == Some(&Tok::LParen) {
            self.pos += 1;
            loop {
                let (arg, depth) = self.pipeline()?;
                deepest = deepest.max(depth);
                args.push(arg);
                if self.peek() != Some(&Tok::Semi) {
                    break;
                }
                self.pos += 1;
            }
            self.expect(&Tok::RParen)?;
        }
        if !func.arities.contains(&args.len()) {
            return Err(syntax(
                &format!("{name}/{} is not a function", args.len()),
                at,
            ));
        }
        Ok((Ast::Call(func.func, args), self.over(deepest)?))
    }

    /// `{` already read: entries up to `}`.
    fn object(&mut self) -> Result<Node> {
        let mut entries = Vec::new();
        let mut deepest = 0;
        if self.peek() == Some(&Tok::RBrace) {
            self.pos += 1;
            return Ok((Ast::Object(entries), 1));
        }
        loop {
            let Some(tok) = self.peek().cloned() else {
                return Err(self.unexpected());
            };
            self.pos += 1;
            let ((key, dk), shorthand) = match tok {
                Tok::Name(name) => {
                    let key = Ast::Literal(Value::String(name));
                    (
                        (key.clone(), 1),
                        (Ast::Index(Box::new(Ast::Identity), Box::new(key), false), 2),
                    )
                }
                Tok::Var(name) => (
                    (Ast::Literal(Value::String(name.clone())), 1),
                    (Ast::Var(name), 1),
                ),
                Tok::Str(pieces) => {
                    let (key, dk) = self.string(&pieces)?;
                    let index = Ast::Index(Box::new(Ast::Identity), Box::new(key.clone()), false);
                    ((key, dk), (index, self.over(dk)?))
                }
                Tok::LParen => {
                    let key = self.pipeline()?;
                    self.expect(&Tok::RParen)?;
                    if self.peek() != Some(&Tok::Colon) {
                        return Err(self.unexpected());
                    }
                    (key, (Ast::Identity, 1))
                }
                _ => {
                    self.pos -= 1;
                    return Err(self.unexpected());
                }
            };
            let (value, dv) = if self.peek() == Some(&Tok::Colon) {
                if matches!(
                    self.tokens.get(self.pos - 1).map(|s| &s.tok),
                    Some(Tok::Var(_))
                ) {
                    return Err(self.unexpected());
                }
                self.pos += 1;
                self.alt()?
            } else {
                shorthand
            };
            deepest = deepest.max(dk).max(dv);
            entries.push((key, value));
            match self.peek() {
                Some(Tok::Comma) => self.pos += 1,
                Some(Tok::RBrace) => {
                    self.pos += 1;
                    return Ok((Ast::Object(entries), self.over(deepest)?));
                }
                _ => return Err(self.unexpected()),
            }
        }
    }
}
