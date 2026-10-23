// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `ty_model!`: parses a bounded state machine and expands it to an `::aterm_spec::derive::Model` literal.

use std::collections::HashSet;

use aterm_macro_support::{Cursor, Error, is_punct, join, lit, q};
use proc_macro::{Delimiter, Group, Ident, Literal, Spacing, Span, TokenStream, TokenTree};

/// Reads `input` and writes the model literal, or the first error.
pub(crate) fn expand(input: TokenStream) -> Result<TokenStream, Error> {
    let def = ModelDef::parse(input)?;
    def.expand()
}

const RESERVED: &[&str] = &[
    "_", "abstract", "as", "async", "await", "become", "box", "break", "const", "continue",
    "crate", "do", "dyn", "else", "enum", "extern", "false", "final", "fn", "for", "if", "impl",
    "in", "let", "loop", "macro", "match", "mod", "move", "mut", "override", "priv", "pub", "ref",
    "return", "Self", "self", "static", "struct", "super", "trait", "true", "try", "type",
    "typeof", "unsafe", "unsized", "use", "virtual", "where", "while", "yield",
];

const PATH_WORDS: &[&str] = &["self", "Self", "super", "crate"];

fn is_reserved(ident: &Ident) -> bool {
    RESERVED.contains(&ident.to_string().as_str())
}

/// Takes a name: an identifier that is not a reserved word.
pub(crate) fn expect_name(c: &mut Cursor, what: &str) -> Result<Ident, Error> {
    match c.peek() {
        Some(TokenTree::Ident(i)) if !is_reserved(i) => c.expect_ident(what),
        _ => Err(Error::new(c.span(), format!("expected {what}"))),
    }
}

fn expect_int(c: &mut Cursor) -> Result<i64, Error> {
    let start = c.span();
    let minus = c.eat_punct('-').is_some();
    let (literal, span) = match c.next() {
        Some(TokenTree::Literal(l)) => {
            let span = l.span();
            (l, span)
        }
        _ => return Err(Error::new(start, "expected integer literal")),
    };
    let Some(mut int) = lit::int_lit(&literal) else {
        return Err(Error::new(start, "expected integer literal"));
    };
    if minus {
        int.digits.insert(0, '-');
    }
    int.to_i64().map_err(|e| Error::spanned(start, span, e))
}

struct ActionDef {
    name: Ident,
    guard: Option<Node>,
    updates: Vec<(Ident, Node)>,
}

struct ModelDef {
    name: Ident,
    consts: Vec<(Ident, i64)>,
    /// Each `scenario`: constant overrides the real code also runs under.
    scenarios: Vec<Vec<(Ident, i64)>>,
    vars: Vec<(Ident, i64)>,
    actions: Vec<ActionDef>,
    /// `(name, expr, kind)`: `invariant` a design claim, `bound` the box the
    /// checkers walk.
    invariants: Vec<(Ident, Node, &'static str)>,
}

impl ModelDef {
    fn parse(input: TokenStream) -> Result<Self, Error> {
        let mut top = Cursor::new(input, Span::call_site());
        let name = expect_name(&mut top, "identifier")?;
        let Some(group) = top.eat_group(Delimiter::Brace) else {
            return Err(Error::new(top.span(), "expected curly braces"));
        };
        if !top.is_empty() {
            return Err(Error::new(top.span(), "unexpected token"));
        }
        let mut body = Cursor::of_group(&group);
        let mut def = ModelDef {
            name,
            consts: Vec::new(),
            scenarios: Vec::new(),
            vars: Vec::new(),
            actions: Vec::new(),
            invariants: Vec::new(),
        };
        while !body.is_empty() {
            if body.eat_keyword("const").is_some() {
                let id = expect_name(&mut body, "identifier")?;
                body.expect_punct('=', "`=`")?;
                let value = expect_int(&mut body)?;
                body.expect_punct(';', "`;`")?;
                def.consts.push((id, value));
                continue;
            }
            let kw = expect_name(&mut body, "identifier")?;
            match kw.to_string().as_str() {
                "scenario" => {
                    let mut overrides = Vec::new();
                    loop {
                        let id = expect_name(&mut body, "identifier")?;
                        body.expect_punct('=', "`=`")?;
                        overrides.push((id, expect_int(&mut body)?));
                        if body.peek_punct(';') {
                            break;
                        }
                        body.expect_punct(',', "`,`")?;
                    }
                    body.expect_punct(';', "`;`")?;
                    def.scenarios.push(overrides);
                }
                "var" => {
                    let id = expect_name(&mut body, "identifier")?;
                    body.expect_punct('=', "`=`")?;
                    let value = expect_int(&mut body)?;
                    body.expect_punct(';', "`;`")?;
                    def.vars.push((id, value));
                }
                "action" => {
                    let name = expect_name(&mut body, "identifier")?;
                    let guard = match body.peek() {
                        Some(TokenTree::Ident(w)) if !is_reserved(w) => {
                            if w.to_string() != "when" {
                                return Err(Error::new(w.span(), "expected `when` or `{`"));
                            }
                            body.next();
                            let Some(g) = body.eat_group(Delimiter::Parenthesis) else {
                                return Err(Error::new(body.span(), "expected parentheses"));
                            };
                            let mut inner = Cursor::of_group(&g);
                            let guard = expr(&mut inner, true)?;
                            ensure_empty(&mut inner, "unexpected token")?;
                            Some(guard)
                        }
                        _ => None,
                    };
                    let Some(block) = body.eat_group(Delimiter::Brace) else {
                        return Err(Error::new(body.span(), "expected curly braces"));
                    };
                    let mut ab = Cursor::of_group(&block);
                    let mut updates = Vec::new();
                    while !ab.is_empty() {
                        let lhs = expect_name(&mut ab, "identifier")?;
                        ab.expect_punct('=', "`=`")?;
                        let rhs = expr(&mut ab, true)?;
                        ab.expect_punct(';', "`;`")?;
                        updates.push((lhs, rhs));
                    }
                    def.actions.push(ActionDef {
                        name,
                        guard,
                        updates,
                    });
                }
                word @ ("invariant" | "bound") => {
                    let kind = if word == "invariant" {
                        "Claim"
                    } else {
                        "Bound"
                    };
                    let name = expect_name(&mut body, "identifier")?;
                    body.expect_punct(':', "`:`")?;
                    let e = expr(&mut body, true)?;
                    body.expect_punct(';', "`;`")?;
                    def.invariants.push((name, e, kind));
                }
                other => {
                    return Err(Error::new(
                        kw.span(),
                        format!(
                            "expected const/scenario/var/action/invariant/bound, \
                             found `{other}`"
                        ),
                    ));
                }
            }
        }
        Ok(def)
    }

    fn expand(&self) -> Result<TokenStream, Error> {
        let const_names: HashSet<String> =
            self.consts.iter().map(|(id, _)| id.to_string()).collect();
        let name_str = str_lit(&self.name);

        let consts: Vec<TokenStream> = self
            .consts
            .iter()
            .map(|(id, v)| q!("(#s, #v)", s = str_lit(id), v = Literal::i64_suffixed(*v)))
            .collect();
        let mut scenarios = Vec::new();
        for overrides in &self.scenarios {
            let mut pairs = Vec::new();
            for (id, v) in overrides {
                let s = id.to_string();
                if s == "Buggy" || !const_names.contains(&s) {
                    return Err(Error::new(
                        id.span(),
                        format!(
                            "a scenario sets a declared constant other than `Buggy`; `{s}` is \
                             not one"
                        ),
                    ));
                }
                pairs.push(q!(
                    "(#s, #v)",
                    s = str_lit(id),
                    v = Literal::i64_suffixed(*v)
                ));
            }
            scenarios.push(q!("&[#pairs]", pairs = join(&pairs, ',', false)));
        }
        let vars: Vec<TokenStream> = self
            .vars
            .iter()
            .map(|(id, v)| {
                q!(
                    "::aterm_spec::derive::StateVar { name: #s, init: #v }",
                    s = str_lit(id),
                    v = Literal::i64_suffixed(*v)
                )
            })
            .collect();

        let mut actions = Vec::new();
        for a in &self.actions {
            let guard = match &a.guard {
                Some(g) => q!("Some(#t)", t = g.to_builder(&const_names)),
                None => q!("None"),
            };
            let updates: Vec<TokenStream> = a
                .updates
                .iter()
                .map(|(lhs, rhs)| {
                    q!(
                        "::aterm_spec::derive::Update { var: #s, expr: #t }",
                        s = str_lit(lhs),
                        t = rhs.to_builder(&const_names)
                    )
                })
                .collect();
            actions.push(q!(
                "::aterm_spec::derive::Action { name: #an, guard: #guard, updates: vec![#ups] }",
                an = str_lit(&a.name),
                guard,
                ups = join(&updates, ',', false)
            ));
        }

        let invariants: Vec<TokenStream> = self
            .invariants
            .iter()
            .map(|(id, e, kind)| {
                q!(
                    "::aterm_spec::derive::Invariant {
                        name: #s,
                        expr: #t,
                        kind: ::aterm_spec::derive::InvariantKind:: #kind,
                    }",
                    s = str_lit(id),
                    t = e.to_builder(&const_names),
                    kind = Ident::new(kind, Span::call_site())
                )
            })
            .collect();

        let body = q!(
            "fn __aterm_spec_model() -> ::aterm_spec::derive::Model {
                ::aterm_spec::derive::Model {
                    name: #name_str,
                    consts: vec![#consts],
                    vars: vec![#vars],
                    fn_vars: vec![],
                    actions: vec![#actions],
                    invariants: vec![#invariants],
                }
            }
            ::aterm_spec::registry::submit! {
                ::aterm_spec::xref::RegisteredModel => ::aterm_spec::xref::RegisteredModel {
                    build: __aterm_spec_model,
                    scenarios: &[#scenarios],
                    location: ::core::concat!(::core::file!(), \":\", ::core::line!()),
                }
            }
            __aterm_spec_model()",
            name_str,
            consts = join(&consts, ',', false),
            vars = join(&vars, ',', false),
            actions = join(&actions, ',', false),
            invariants = join(&invariants, ',', false),
            scenarios = join(&scenarios, ',', false)
        );
        Ok(TokenTree::Group(Group::new(Delimiter::Brace, body)).into())
    }
}

fn str_lit(ident: &Ident) -> Literal {
    Literal::string(&ident.to_string())
}

fn ensure_empty(c: &mut Cursor, message: &str) -> Result<(), Error> {
    match c.peek_raw() {
        None => Ok(()),
        Some(tt) => Err(Error::new(tt.span(), message)),
    }
}

struct Node {
    expr: Expr,
    first: Span,
    last: Span,
}

enum Expr {
    Int(i64),
    Name(String),
    /// Kept apart from its inside only so `(a <= b) == c` does not read as a
    /// chained comparison; it translates to its inside.
    Paren(Box<Node>),
    Binary(Builder, Box<Node>, Box<Node>),
    If(Box<Node>, Box<Node>, Box<Node>),
}

/// The `aterm_spec::derive` builder a supported operator maps to.
#[derive(Clone, Copy)]
enum Builder {
    Add,
    Sub,
    Gt,
    Le,
    Eq,
    And,
    Or,
}

impl Builder {
    const fn path(self) -> &'static str {
        match self {
            Builder::Add => "add",
            Builder::Sub => "sub",
            Builder::Gt => "gt",
            Builder::Le => "le",
            Builder::Eq => "eq",
            Builder::And => "and_",
            Builder::Or => "or_",
        }
    }
}

impl Node {
    fn to_builder(&self, consts: &HashSet<String>) -> TokenStream {
        match &self.expr {
            Expr::Int(v) => q!(
                "::aterm_spec::derive::int(#v)",
                v = Literal::i64_suffixed(*v)
            ),
            Expr::Name(id) => {
                let s = Literal::string(id);
                if consts.contains(id) {
                    q!("::aterm_spec::derive::cst(#s)", s)
                } else {
                    q!("::aterm_spec::derive::var(#s)", s)
                }
            }
            Expr::Paren(inner) => inner.to_builder(consts),
            Expr::Binary(f, l, r) => q!(
                "::aterm_spec::derive:: #f(#l, #r)",
                f = Ident::new(f.path(), Span::call_site()),
                l = l.to_builder(consts),
                r = r.to_builder(consts)
            ),
            Expr::If(c, t, f) => q!(
                "::aterm_spec::derive::if_(#c, #t, #f)",
                c = c.to_builder(consts),
                t = t.to_builder(consts),
                f = f.to_builder(consts)
            ),
        }
    }

    fn unsupported(&self) -> Error {
        Error::spanned(self.first, self.last, UNSUPPORTED)
    }
}

const UNSUPPORTED: &str = "unsupported expression in ty_model!";

/// Rust's binary operators, the compound assignments included: what the
/// precedence climber has to see to group the supported ones correctly.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Op {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    And,
    Or,
    BitXor,
    BitAnd,
    BitOr,
    Shl,
    Shr,
    Eq,
    Lt,
    Le,
    Ne,
    Ge,
    Gt,
    /// `+=`, `<<=` and the rest.
    Compound,
}

/// Binding strength, loosest first (the Reference's table, as syn 2 orders
/// it).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Prec {
    Min,
    Assign,
    Range,
    Or,
    And,
    Compare,
    BitOr,
    BitXor,
    BitAnd,
    Shift,
    Sum,
    Product,
    Cast,
}

impl Op {
    const fn prec(self) -> Prec {
        match self {
            Op::Add | Op::Sub => Prec::Sum,
            Op::Mul | Op::Div | Op::Rem => Prec::Product,
            Op::And => Prec::And,
            Op::Or => Prec::Or,
            Op::BitXor => Prec::BitXor,
            Op::BitAnd => Prec::BitAnd,
            Op::BitOr => Prec::BitOr,
            Op::Shl | Op::Shr => Prec::Shift,
            Op::Eq | Op::Lt | Op::Le | Op::Ne | Op::Ge | Op::Gt => Prec::Compare,
            Op::Compound => Prec::Assign,
        }
    }

    const fn builder(self) -> Option<Builder> {
        match self {
            Op::Add => Some(Builder::Add),
            Op::Sub => Some(Builder::Sub),
            Op::Gt => Some(Builder::Gt),
            Op::Le => Some(Builder::Le),
            Op::Eq => Some(Builder::Eq),
            Op::And => Some(Builder::And),
            Op::Or => Some(Builder::Or),
            _ => None,
        }
    }
}

const OPS: &[(&str, Op)] = &[
    ("<<=", Op::Compound),
    (">>=", Op::Compound),
    ("+=", Op::Compound),
    ("-=", Op::Compound),
    ("*=", Op::Compound),
    ("/=", Op::Compound),
    ("%=", Op::Compound),
    ("^=", Op::Compound),
    ("&=", Op::Compound),
    ("|=", Op::Compound),
    ("&&", Op::And),
    ("||", Op::Or),
    ("<<", Op::Shl),
    (">>", Op::Shr),
    ("==", Op::Eq),
    ("<=", Op::Le),
    ("!=", Op::Ne),
    (">=", Op::Ge),
    ("+", Op::Add),
    ("-", Op::Sub),
    ("*", Op::Mul),
    ("/", Op::Div),
    ("%", Op::Rem),
    ("^", Op::BitXor),
    ("&", Op::BitAnd),
    ("|", Op::BitOr),
    ("<", Op::Lt),
    (">", Op::Gt),
];

fn peek_spelled(c: &Cursor, op: &str) -> bool {
    let len = op.chars().count();
    op.chars().enumerate().all(|(i, ch)| {
        matches!(c.peek_raw_nth(i), Some(TokenTree::Punct(p))
            if p.as_char() == ch && (i + 1 == len || p.spacing() == Spacing::Joint))
    })
}

fn peek_binop(c: &Cursor) -> Option<(Op, usize)> {
    OPS.iter()
        .find(|(spelling, _)| peek_spelled(c, spelling))
        .map(|(spelling, op)| (*op, spelling.len()))
}

fn peek_raw_ident(c: &Cursor, word: &str) -> bool {
    matches!(c.peek_raw(), Some(TokenTree::Ident(i)) if i.to_string() == word)
}

fn peek_raw_group(c: &Cursor, delimiter: Delimiter) -> Option<&Group> {
    match c.peek_raw() {
        Some(TokenTree::Group(g)) if g.delimiter() == delimiter => Some(g),
        _ => None,
    }
}

fn peek_prec(c: &Cursor) -> Prec {
    if let Some((op, _)) = peek_binop(c) {
        op.prec()
    } else if peek_spelled(c, "=") && !peek_spelled(c, "=>") {
        Prec::Assign
    } else if peek_spelled(c, "..") {
        Prec::Range
    } else if peek_raw_ident(c, "as") {
        Prec::Cast
    } else {
        Prec::Min
    }
}

fn expr(c: &mut Cursor, allow_struct: bool) -> Result<Node, Error> {
    let lhs = unary(c, allow_struct)?;
    climb(c, lhs, allow_struct, Prec::Min)
}

fn climb(c: &mut Cursor, mut lhs: Node, allow_struct: bool, base: Prec) -> Result<Node, Error> {
    loop {
        if let Some((op, len)) = peek_binop(c) {
            let prec = op.prec();
            if prec < base {
                break;
            }
            let at = c.peek_raw().map_or_else(|| c.end_span(), TokenTree::span);
            if prec == Prec::Compare
                && matches!(&lhs.expr, Expr::Binary(f, ..) if matches!(f, Builder::Gt | Builder::Le | Builder::Eq))
            {
                return Err(Error::new(at, "comparison operators cannot be chained"));
            }
            for _ in 0..len {
                c.next_raw();
            }
            let rhs = rhs(c, allow_struct, prec)?;
            let (first, last) = (lhs.first, rhs.last);
            let Some(f) = op.builder() else {
                return Err(Error::spanned(
                    first,
                    last,
                    "unsupported operator (use + - > <= == && ||)",
                ));
            };
            lhs = Node {
                expr: Expr::Binary(f, Box::new(lhs), Box::new(rhs)),
                first,
                last,
            };
        } else if (base <= Prec::Assign && peek_spelled(c, "=") && !peek_spelled(c, "=>"))
            || (base <= Prec::Range && peek_spelled(c, ".."))
            || (base <= Prec::Cast && peek_raw_ident(c, "as"))
        {
            let at = c.peek_raw().map_or(lhs.last, TokenTree::span);
            return Err(Error::spanned(lhs.first, at, UNSUPPORTED));
        } else {
            break;
        }
    }
    Ok(lhs)
}

fn rhs(c: &mut Cursor, allow_struct: bool, prec: Prec) -> Result<Node, Error> {
    let mut node = unary(c, allow_struct)?;
    loop {
        let next = peek_prec(c);
        if next > prec || (next == prec && prec == Prec::Assign) {
            node = climb(c, node, allow_struct, next)?;
        } else {
            break;
        }
    }
    Ok(node)
}

fn unary(c: &mut Cursor, allow_struct: bool) -> Result<Node, Error> {
    if let Some(TokenTree::Punct(p)) = c.peek_raw()
        && matches!(p.as_char(), '&' | '*' | '!' | '-')
    {
        return Err(Error::new(p.span(), UNSUPPORTED));
    }
    let node = atom(c, allow_struct)?;
    postfix(c, node)
}

fn postfix(c: &mut Cursor, node: Node) -> Result<Node, Error> {
    let call_or_index = peek_raw_group(c, Delimiter::Parenthesis).is_some()
        || peek_raw_group(c, Delimiter::Bracket).is_some();
    let field = peek_spelled(c, ".") && !peek_spelled(c, "..");
    if call_or_index || field || peek_spelled(c, "?") {
        let at = c.peek_raw().map_or(node.last, TokenTree::span);
        return Err(Error::spanned(node.first, at, UNSUPPORTED));
    }
    Ok(node)
}

fn atom(c: &mut Cursor, allow_struct: bool) -> Result<Node, Error> {
    let Some(tt) = c.next_raw() else {
        return Err(Error::new(c.end_span(), "expected an expression"));
    };
    let span = tt.span();
    let leaf = |expr| Node {
        expr,
        first: span,
        last: span,
    };
    match tt {
        TokenTree::Literal(l) => match lit::int_lit(&l) {
            Some(int) => int
                .to_i64()
                .map(|v| leaf(Expr::Int(v)))
                .map_err(|e| Error::new(span, e)),
            None => Err(Error::new(span, UNSUPPORTED)),
        },
        TokenTree::Ident(i) => {
            let word = i.to_string();
            if word == "if" {
                return if_expr(c, span);
            }
            if is_reserved(&i) && !PATH_WORDS.contains(&word.as_str()) {
                return Err(Error::new(span, UNSUPPORTED));
            }
            if peek_spelled(c, "::") {
                return Err(Error::new(span, "expected a simple identifier"));
            }
            let is_macro = peek_spelled(c, "!") && !peek_spelled(c, "!=");
            let is_struct = allow_struct && peek_raw_group(c, Delimiter::Brace).is_some();
            if is_macro || is_struct {
                return Err(Error::new(span, UNSUPPORTED));
            }
            Ok(leaf(Expr::Name(word)))
        }
        TokenTree::Group(g) if g.delimiter() == Delimiter::Parenthesis => {
            let mut inner = Cursor::of_group(&g);
            if inner.peek_raw().is_none() {
                return Err(Error::new(span, UNSUPPORTED));
            }
            let e = expr(&mut inner, true)?;
            match inner.peek_raw() {
                None => Ok(Node {
                    expr: Expr::Paren(Box::new(e)),
                    first: g.span_open(),
                    last: g.span_close(),
                }),
                Some(t) if is_punct(t, ',') => Err(Error::new(span, UNSUPPORTED)),
                Some(t) => Err(Error::new(t.span(), "expected `,`")),
            }
        }
        _ => Err(Error::new(span, UNSUPPORTED)),
    }
}

fn if_expr(c: &mut Cursor, if_span: Span) -> Result<Node, Error> {
    let cond = expr(c, false)?;
    let Some(then_group) = peek_raw_group(c, Delimiter::Brace).cloned() else {
        return Err(Error::new(
            c.peek_raw().map_or_else(|| c.end_span(), TokenTree::span),
            "expected curly braces",
        ));
    };
    c.next_raw();
    let then = block_tail(&then_group)?;
    if !peek_raw_ident(c, "else") {
        return Err(Error::spanned(
            if_span,
            then_group.span_close(),
            "`if` must have an `else` branch",
        ));
    }
    c.next_raw();
    let otherwise = if peek_raw_ident(c, "if") {
        let at = c.next_raw().map_or(if_span, |t| t.span());
        if_expr(c, at)?
    } else if let Some(group) = peek_raw_group(c, Delimiter::Brace).cloned() {
        c.next_raw();
        block_tail(&group)?
    } else {
        return Err(Error::new(
            c.peek_raw().map_or_else(|| c.end_span(), TokenTree::span),
            "expected `if` or curly braces",
        ));
    };
    let last = otherwise.last;
    Ok(Node {
        expr: Expr::If(Box::new(cond), Box::new(then), Box::new(otherwise)),
        first: if_span,
        last,
    })
}

fn block_tail(group: &Group) -> Result<Node, Error> {
    let single = || {
        Error::spanned(
            group.span_open(),
            group.span_close(),
            "if/else branch must be a single expression",
        )
    };
    let mut c = Cursor::of_group(group);
    if c.peek_raw().is_none() {
        return Err(single());
    }
    let node = if peek_raw_ident(&c, "if") {
        let at = c.next_raw().map_or_else(|| group.span_open(), |t| t.span());
        let node = if_expr(&mut c, at)?;
        if peek_spelled(&c, "?") || (peek_spelled(&c, ".") && !peek_spelled(&c, "..")) {
            return Err(node.unsupported());
        }
        node
    } else {
        expr(&mut c, true)?
    };
    if c.peek_raw().is_some() {
        return Err(single());
    }
    Ok(node)
}
