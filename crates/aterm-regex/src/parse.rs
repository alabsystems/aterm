// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pattern text → [`Ast`].
//!
//! The parser is a single flat loop over the pattern's code points with an
//! explicit [`Group`] stack, so no amount of `(((((…)))))` can recurse it into
//! the guard page. What *is* recursive is the [`Ast`] it produces — and its
//! `Drop` with it — so every node records its own depth and construction fails
//! past [`MAX_NESTING_DEPTH`]. That is the same 250 the `regex` crate defaults
//! its own nest limit to.
//!
//! ## What the syntax is, and where it stops
//!
//! Everything the tree ships or accepts from a user: literals, `.`, character
//! classes with ranges/negation/escapes/`\d\w\s`/POSIX names, the perl classes,
//! the assertions `^ $ \A \z \b \B \< \>`, groups (capturing, `(?:`, named) with
//! inline flags `i m s x U`, alternation, and the quantifiers `* + ? {n} {n,}
//! {n,m}` in greedy and lazy forms.
//!
//! Four things `regex` accepts are refused here, each with a message that says
//! so. They are refusals, never silent reinterpretations, which is the whole
//! point — a regex engine that quietly matches something *else* is worse than
//! one that declines:
//!
//! * `\p{…}` / `\pL` Unicode property classes.
//! * Character-class set operations — `[a&&b]`, `[a--b]`, `[a~~b]` — and the
//!   nested `[…]` that go with them. A naive reading of `[a--b]` is "a, -, b",
//!   which is exactly the silent mismatch this refusal exists to prevent. An
//!   escaped `\-`, `\&` or `\~` is always a literal and always fine.
//! * `(?-u)` byte-oriented mode: this engine matches over `&str` by code point.
//! * `(?R)` CRLF mode.
//!
//! No backreferences and no look-around either — but those are not divergences,
//! because `regex` does not have them: a Thompson NFA is a faithful replacement
//! for it, not a reduced one.

use crate::Error;
use crate::unicode;

/// Maximum nesting depth of the parsed AST. Matches the `regex` crate's default
/// nest limit. Bounds both parse-time work and the recursive `Drop` of the tree.
pub(crate) const MAX_NESTING_DEPTH: usize = 250;

/// A zero-width assertion, evaluated against the code points either side of a
/// position in the *whole* haystack.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Assertion {
    /// `\A`, and `^` outside multi-line mode.
    StartText,
    /// `\z`, and `$` outside multi-line mode.
    EndText,
    /// `^` in multi-line mode.
    StartLine,
    /// `$` in multi-line mode.
    EndLine,
    /// `\b`.
    WordBoundary,
    /// `\B`.
    NotWordBoundary,
    /// `\<`.
    WordStart,
    /// `\>`.
    WordEnd,
}

/// One of the perl classes, carried symbolically so `\w` never has to be
/// expanded into the ~800 ranges of `Alphabetic | M | Nd | Pc | Join_Control`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PerlClass {
    Digit,
    NotDigit,
    Space,
    NotSpace,
    Word,
    NotWord,
}

impl PerlClass {
    fn matches(self, c: char) -> bool {
        match self {
            Self::Digit => unicode::is_digit(c),
            Self::NotDigit => !unicode::is_digit(c),
            Self::Space => unicode::is_space(c),
            Self::NotSpace => !unicode::is_space(c),
            Self::Word => unicode::is_word(c),
            Self::NotWord => !unicode::is_word(c),
        }
    }
}

/// A set of code points: explicit ranges, plus any perl classes named inside it,
/// plus one negation applied to the union of the two.
///
/// Keeping the perl classes symbolic is what makes `[^\s<>…]` cheap, and
/// splitting negation out is what makes `(?i)[^k]` *correct*: case folding
/// happens to the ranges first and the complement is taken afterwards, which is
/// the order the `regex` crate uses (`(?i)[^k]` must reject KELVIN SIGN).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ClassSet {
    /// Sorted, disjoint, inclusive ranges.
    ranges: Vec<(char, char)>,
    perls: Vec<PerlClass>,
    negated: bool,
}

impl ClassSet {
    /// A class holding exactly the code points of one inclusive range.
    pub(crate) fn range(lo: char, hi: char) -> Self {
        Self { ranges: vec![(lo, hi)], perls: Vec::new(), negated: false }
    }

    /// The class every code point belongs to (`(?s).`).
    pub(crate) fn any() -> Self {
        Self::range('\0', char::MAX)
    }

    /// `.` outside `(?s)`: everything except the line terminator.
    pub(crate) fn any_except_newline() -> Self {
        let mut set = Self { ranges: vec![('\n', '\n')], perls: Vec::new(), negated: true };
        set.normalize();
        set
    }

    /// One perl class on its own (`\d`, `\W`, …).
    fn perl(p: PerlClass) -> Self {
        Self { ranges: Vec::new(), perls: vec![p], negated: false }
    }

    /// Does `c` belong to this class?
    #[inline]
    pub(crate) fn matches(&self, c: char) -> bool {
        self.negated != self.contains_unnegated(c)
    }

    #[inline]
    fn contains_unnegated(&self, c: char) -> bool {
        let hit = self
            .ranges
            .binary_search_by(|&(lo, hi)| {
                if c < lo {
                    core::cmp::Ordering::Greater
                } else if c > hi {
                    core::cmp::Ordering::Less
                } else {
                    core::cmp::Ordering::Equal
                }
            })
            .is_ok();
        hit || self.perls.iter().any(|p| p.matches(c))
    }

    /// Sort and coalesce the ranges so [`matches`](Self::matches) can binary
    /// search them.
    fn normalize(&mut self) {
        self.ranges.sort_unstable();
        let mut merged: Vec<(char, char)> = Vec::with_capacity(self.ranges.len());
        for &(lo, hi) in &self.ranges {
            match merged.last_mut() {
                // Coalesce overlapping *and* adjacent ranges: `[a-mn-z]` is one
                // range, and `range_tables_are_well_formed` relies on it.
                Some(last) if lo as u32 <= last.1 as u32 + 1 => {
                    if hi > last.1 {
                        last.1 = hi;
                    }
                }
                _ => merged.push((lo, hi)),
            }
        }
        self.ranges = merged;
        self.perls.dedup();
    }

    /// Mark, in `out`, every *leading* UTF-8 byte of a code point this class can
    /// match. Continuation bytes (`0x80..=0xBF`) are never marked, which is what
    /// lets the prefilter walk the haystack a byte at a time: it may step into
    /// the middle of a code point while skipping, but it can never stop there.
    ///
    /// Over-marking is always safe — the prefilter only ever skips a position no
    /// candidate could match — so the non-ASCII side is answered conservatively.
    pub(crate) fn mark_lead_bytes(&self, out: &mut [bool; 256]) {
        for b in 0u8..0x80 {
            if self.matches(b as char) {
                out[b as usize] = true;
            }
        }
        let non_ascii = self.negated
            || !self.perls.is_empty()
            || self.ranges.last().is_some_and(|&(_, hi)| hi as u32 > 0x7f);
        if non_ascii {
            for b in 0xc2..=0xf4u8 {
                out[b as usize] = true;
            }
        }
    }

    /// Heap bytes this class costs, charged against the compile-time ceiling
    /// alongside the instruction that indexes it.
    pub(crate) fn byte_size(&self) -> usize {
        size_of::<Self>()
            + self.ranges.len() * size_of::<(char, char)>()
            + self.perls.len() * size_of::<PerlClass>()
    }

    /// Close the explicit ranges under simple case folding. The perl classes
    /// need no work: `\w`, `\d` and `\s` are already closed under it.
    fn case_fold(&mut self) {
        let mut extra = Vec::new();
        for &(lo, hi) in &self.ranges {
            unicode::fold_range(lo, hi, &mut extra);
        }
        self.ranges.append(&mut extra);
        self.normalize();
    }
}

/// The parsed pattern.
#[derive(Clone, Debug)]
pub(crate) enum Ast {
    /// Matches the empty string. `()`, `a{0}`, an empty alternation branch.
    Empty,
    /// Exactly one code point.
    Literal(char),
    /// One code point drawn from a set.
    Class(ClassSet),
    /// Zero width.
    Assert(Assertion),
    Concat(Vec<Ast>),
    /// Alternation, in preference order: earlier branches win ties.
    Alt(Vec<Ast>),
    Repeat { node: Box<Ast>, min: u32, max: Option<u32>, greedy: bool },
}

/// The inline flags in force at a point in the pattern.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Flags {
    /// `i`
    pub(crate) case_insensitive: bool,
    /// `m`
    pub(crate) multi_line: bool,
    /// `s`
    pub(crate) dot_matches_new_line: bool,
    /// `x`
    pub(crate) ignore_whitespace: bool,
    /// `U`
    pub(crate) swap_greed: bool,
}

/// One open `(…)`: the branches closed by `|` so far, the branch being built,
/// and the flags to restore when the group closes.
struct Group {
    alts: Vec<Ast>,
    concat: Vec<(Ast, usize)>,
    saved_flags: Flags,
    open_at: usize,
}

struct Parser<'a> {
    pat: &'a [char],
    /// Rendered form of the pattern, for the error caret.
    source: &'a str,
    pos: usize,
    flags: Flags,
    stack: Vec<Group>,
    alts: Vec<Ast>,
    concat: Vec<(Ast, usize)>,
    /// Capture groups are parsed and then ignored — nothing in the tree reads a
    /// capture — but their names are still tracked, because a duplicate name is
    /// an error in the `regex` crate and accepting one here would be a
    /// divergence for no gain.
    group_names: Vec<String>,
}

/// Parse `pattern` under the builder's starting `flags`.
///
/// # Errors
/// Returns [`Error::Syntax`] with a rendered, caret-annotated message for any
/// malformed or unsupported construct.
pub(crate) fn parse(pattern: &str, flags: Flags) -> Result<Ast, Error> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut p = Parser {
        pat: &chars,
        source: pattern,
        pos: 0,
        flags,
        stack: Vec::new(),
        alts: Vec::new(),
        concat: Vec::new(),
        group_names: Vec::new(),
    };
    p.run()
}

impl Parser<'_> {
    fn err<T>(&self, msg: &str) -> Result<T, Error> {
        Err(self.error(msg))
    }

    /// Render an error the way the `regex` crate does: the pattern, a caret
    /// under the offending position, then the reason. The call sites surface
    /// this string to users verbatim (`SearchOptionsError::InvalidRegex`).
    fn error(&self, msg: &str) -> Error {
        let caret = " ".repeat(self.pos.min(self.pat.len()));
        Error::Syntax(format!(
            "regex parse error:\n    {}\n    {caret}^\nerror: {msg}",
            self.source
        ))
    }

    fn peek(&self) -> Option<char> {
        self.pat.get(self.pos).copied()
    }

    fn peek_at(&self, n: usize) -> Option<char> {
        self.pat.get(self.pos + n).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek();
        if c.is_some() {
            self.pos += 1;
        }
        c
    }

    /// In `x` mode, whitespace is insignificant and `#` runs to end of line.
    fn skip_ignorable(&mut self) {
        if !self.flags.ignore_whitespace {
            return;
        }
        while let Some(c) = self.peek() {
            if c.is_whitespace() {
                self.pos += 1;
            } else if c == '#' {
                while let Some(c) = self.peek() {
                    self.pos += 1;
                    if c == '\n' {
                        break;
                    }
                }
            } else {
                break;
            }
        }
    }

    fn depth(&self) -> usize {
        self.stack.len()
    }

    fn push(&mut self, ast: Ast, depth: usize) -> Result<(), Error> {
        if depth + self.depth() > MAX_NESTING_DEPTH {
            return self.err(&format!(
                "pattern nests more than {MAX_NESTING_DEPTH} levels deep"
            ));
        }
        self.concat.push((ast, depth));
        Ok(())
    }

    fn run(&mut self) -> Result<Ast, Error> {
        loop {
            self.skip_ignorable();
            let Some(c) = self.peek() else { break };
            match c {
                '(' => self.open_group()?,
                ')' => self.close_group()?,
                '|' => {
                    self.pos += 1;
                    let branch = take_concat(&mut self.concat);
                    self.alts.push(branch);
                }
                '*' | '+' | '?' => {
                    self.pos += 1;
                    let (min, max) = match c {
                        '*' => (0, None),
                        '+' => (1, None),
                        _ => (0, Some(1)),
                    };
                    // `*`, `+` and `?` take their laziness marker only when it
                    // is adjacent: in `x` mode `a? ?` is `(a?)?`, not `a??`.
                    self.apply_repeat(min, max, false)?;
                }
                '{' => {
                    let (min, max) = self.parse_counted()?;
                    // A counted repetition is the exception — `a{1,2} ?` *is*
                    // lazy. Asymmetric, and the `regex` crate's asymmetry.
                    self.apply_repeat(min, max, true)?;
                }
                '[' => {
                    let mut class = self.parse_class()?;
                    if self.flags.case_insensitive {
                        class.case_fold();
                    }
                    self.push(Ast::Class(class), 0)?;
                }
                '.' => {
                    self.pos += 1;
                    let class = if self.flags.dot_matches_new_line {
                        ClassSet::any()
                    } else {
                        ClassSet::any_except_newline()
                    };
                    self.push(Ast::Class(class), 0)?;
                }
                '^' => {
                    self.pos += 1;
                    let a = if self.flags.multi_line {
                        Assertion::StartLine
                    } else {
                        Assertion::StartText
                    };
                    self.push(Ast::Assert(a), 0)?;
                }
                '$' => {
                    self.pos += 1;
                    let a = if self.flags.multi_line {
                        Assertion::EndLine
                    } else {
                        Assertion::EndText
                    };
                    self.push(Ast::Assert(a), 0)?;
                }
                '\\' => {
                    let node = self.parse_escape()?;
                    self.push(node, 0)?;
                }
                _ => {
                    self.pos += 1;
                    self.push(self.literal(c), 0)?;
                }
            }
        }
        if let Some(g) = self.stack.last() {
            self.pos = g.open_at;
            return self.err("unclosed group");
        }
        let branch = take_concat(&mut self.concat);
        self.alts.push(branch);
        Ok(finish_alts(core::mem::take(&mut self.alts)))
    }

    /// A single literal code point, expanded into its fold orbit under `(?i)`.
    fn literal(&self, c: char) -> Ast {
        if !self.flags.case_insensitive {
            return Ast::Literal(c);
        }
        let mut orbit: Vec<(char, char)> = unicode::fold_orbit(c).map(|f| (f, f)).collect();
        if orbit.len() == 1 {
            return Ast::Literal(c);
        }
        orbit.sort_unstable();
        let mut set = ClassSet { ranges: orbit, perls: Vec::new(), negated: false };
        set.normalize();
        Ast::Class(set)
    }

    /// Attach a quantifier to the last item of the current branch.
    ///
    /// `space_before_lazy` says whether `x` mode may separate the operator from
    /// its `?` laziness marker. It may after `{n,m}` and may not after `*`, `+`
    /// or `?` — see the two call sites.
    fn apply_repeat(
        &mut self,
        min: u32,
        max: Option<u32>,
        space_before_lazy: bool,
    ) -> Result<(), Error> {
        let mut greedy = !self.flags.swap_greed;
        if space_before_lazy {
            self.skip_ignorable();
        }
        if self.peek() == Some('?') {
            self.pos += 1;
            greedy = !greedy;
        }
        let Some((node, depth)) = self.concat.pop() else {
            return self.err("repetition operator missing expression");
        };
        if depth + 1 + self.depth() > MAX_NESTING_DEPTH {
            return self.err(&format!(
                "pattern nests more than {MAX_NESTING_DEPTH} levels deep"
            ));
        }
        self.concat.push((
            Ast::Repeat { node: Box::new(node), min, max, greedy },
            depth + 1,
        ));
        Ok(())
    }

    /// `{n}`, `{n,}`, `{n,m}`.
    ///
    /// An unescaped `{` always opens one. That is the `regex` crate's rule too —
    /// `a{b}` and `a{,3}` are errors there, not literals — and a divergence here
    /// would mean accepting a pattern the oracle rejects. A literal brace is
    /// `\{`; a `}` on its own is already a literal, in both engines.
    fn parse_counted(&mut self) -> Result<(u32, Option<u32>), Error> {
        debug_assert_eq!(self.peek(), Some('{'));
        let open = self.pos;
        self.pos += 1;
        self.skip_spaces();
        if self.peek().is_none() {
            self.pos = open;
            return self.err("unclosed counted repetition");
        }
        if !self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.pos = open;
            return self.err("repetition quantifier expects a valid decimal");
        }
        let min = self.parse_decimal()?;
        self.skip_spaces();
        let max = match self.peek() {
            Some('}') => Some(min),
            Some(',') => {
                self.pos += 1;
                self.skip_spaces();
                if self.peek() == Some('}') {
                    None
                } else if self.peek().is_some_and(|c| c.is_ascii_digit()) {
                    Some(self.parse_decimal()?)
                } else {
                    self.pos = open;
                    return self.err("repetition quantifier expects a valid decimal");
                }
            }
            _ => {
                self.pos = open;
                return self.err("unclosed counted repetition");
            }
        };
        self.skip_spaces();
        if self.peek() != Some('}') {
            self.pos = open;
            return self.err("unclosed counted repetition");
        }
        self.pos += 1;
        if let Some(m) = max
            && m < min
        {
            self.pos = open;
            return self.err("invalid repetition count range, the start must be <= the end");
        }
        Ok((min, max))
    }

    /// Whitespace inside `{…}` is insignificant even outside `x` mode, matching
    /// the `regex` crate (`a{ 2 }` compiles there).
    fn skip_spaces(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.pos += 1;
        }
    }

    fn parse_decimal(&mut self) -> Result<u32, Error> {
        let start = self.pos;
        let mut n: u32 = 0;
        while let Some(c) = self.peek()
            && let Some(d) = c.to_digit(10)
        {
            self.pos += 1;
            n = match n.checked_mul(10).and_then(|n| n.checked_add(d)) {
                Some(n) => n,
                None => {
                    self.pos = start;
                    return self.err("decimal literal is too large");
                }
            };
        }
        Ok(n)
    }

    fn open_group(&mut self) -> Result<(), Error> {
        let open_at = self.pos;
        self.pos += 1; // '('
        // `x` mode may separate the paren from the `?` that qualifies it:
        // `( ?i)` is `(?i)`.
        self.skip_ignorable();
        let mut flags = self.flags;
        if self.peek() == Some('?') {
            self.pos += 1;
            match self.peek() {
                None => {
                    self.pos = open_at;
                    return self.err("unclosed group");
                }
                Some(':') => {
                    self.pos += 1;
                }
                Some('=') | Some('!') => {
                    return self.err(
                        "look-around, including look-ahead and look-behind, is not supported",
                    );
                }
                Some('<') if matches!(self.peek_at(1), Some('=') | Some('!')) => {
                    return self.err(
                        "look-around, including look-ahead and look-behind, is not supported",
                    );
                }
                Some('<') => {
                    self.pos += 1;
                    self.parse_group_name()?;
                }
                Some('P') => {
                    self.pos += 1;
                    if self.peek() != Some('<') {
                        return self.err("unrecognized flag");
                    }
                    self.pos += 1;
                    self.parse_group_name()?;
                }
                Some(_) => {
                    // A flag directive: `(?flags)` changes the rest of the
                    // enclosing group in place; `(?flags:…)` opens a new one.
                    flags = self.parse_flags()?;
                    match self.bump() {
                        Some(')') => {
                            self.flags = flags;
                            return Ok(());
                        }
                        Some(':') => {}
                        _ => {
                            self.pos = open_at;
                            return self.err("unclosed group");
                        }
                    }
                }
            }
        }
        if self.stack.len() + 1 > MAX_NESTING_DEPTH {
            return self.err(&format!(
                "pattern nests more than {MAX_NESTING_DEPTH} levels deep"
            ));
        }
        self.stack.push(Group {
            alts: core::mem::take(&mut self.alts),
            concat: core::mem::take(&mut self.concat),
            saved_flags: self.flags,
            open_at,
        });
        self.flags = flags;
        Ok(())
    }

    /// `<name>`. The charset is the `regex` crate's: a name starts with a letter
    /// or `_` and continues with letters, digits, `_`, `.`, `[` or `]`.
    fn parse_group_name(&mut self) -> Result<(), Error> {
        let from = self.pos;
        loop {
            let at = self.pos;
            match self.bump() {
                Some('>') => break,
                Some(c) => {
                    let ok = if at == from {
                        c == '_' || c.is_alphabetic()
                    } else {
                        c == '_' || c == '.' || c == '[' || c == ']' || c.is_alphanumeric()
                    };
                    if !ok {
                        self.pos = at;
                        return self.err("invalid capture group character");
                    }
                }
                None => return self.err("unclosed group name"),
            }
        }
        let name: String = self.pat[from..self.pos - 1].iter().collect();
        if name.is_empty() {
            return self.err("empty capture group name");
        }
        if self.group_names.contains(&name) {
            return self.err("duplicate capture group name");
        }
        self.group_names.push(name);
        Ok(())
    }

    fn close_group(&mut self) -> Result<(), Error> {
        let Some(group) = self.stack.pop() else {
            return self.err("unopened group");
        };
        self.pos += 1; // ')'
        let branch = take_concat(&mut self.concat);
        self.alts.push(branch);
        let inner = finish_alts(core::mem::take(&mut self.alts));
        let depth = ast_depth(&inner) + 1;
        self.alts = group.alts;
        self.concat = group.concat;
        self.flags = group.saved_flags;
        self.push(inner, depth)
    }

    /// `i m s x U` and `u`, optionally after a `-` that turns them off.
    fn parse_flags(&mut self) -> Result<Flags, Error> {
        let mut flags = self.flags;
        let mut negating = false;
        let mut seen: Vec<char> = Vec::new();
        loop {
            let Some(c) = self.peek() else {
                return self.err("expected flag but got end of regex");
            };
            match c {
                ':' | ')' => {
                    if seen.is_empty() {
                        return self.err("expected flag but got a group terminator");
                    }
                    return Ok(flags);
                }
                '-' => {
                    if negating {
                        return self.err("unrecognized flag");
                    }
                    negating = true;
                    self.pos += 1;
                }
                'u' if negating => {
                    return self.err(
                        "byte-oriented matching `(?-u)` is not supported: this engine matches \
                         `&str` by code point",
                    );
                }
                'R' => {
                    return self.err("CRLF mode `(?R)` is not supported");
                }
                'i' | 'm' | 's' | 'x' | 'U' | 'u' => {
                    if seen.contains(&c) {
                        return self.err("duplicate flag");
                    }
                    seen.push(c);
                    self.pos += 1;
                    let on = !negating;
                    match c {
                        'i' => flags.case_insensitive = on,
                        'm' => flags.multi_line = on,
                        's' => flags.dot_matches_new_line = on,
                        'x' => flags.ignore_whitespace = on,
                        'U' => flags.swap_greed = on,
                        // `u` is Unicode mode, which is the only mode there is.
                        _ => {}
                    }
                }
                _ => return self.err("unrecognized flag"),
            }
        }
    }

    /// An escape outside a character class: a class shorthand, an assertion, or
    /// a single code point.
    fn parse_escape(&mut self) -> Result<Ast, Error> {
        let start = self.pos;
        self.pos += 1; // '\'
        let Some(c) = self.peek() else {
            self.pos = start;
            return self.err("incomplete escape sequence, reached end of pattern prematurely");
        };
        let assertion = match c {
            'b' => Some(Assertion::WordBoundary),
            'B' => Some(Assertion::NotWordBoundary),
            'A' => Some(Assertion::StartText),
            'z' => Some(Assertion::EndText),
            '<' => Some(Assertion::WordStart),
            '>' => Some(Assertion::WordEnd),
            _ => None,
        };
        if let Some(a) = assertion {
            self.pos += 1;
            return Ok(Ast::Assert(a));
        }
        if let Some(p) = perl_class(c) {
            self.pos += 1;
            return Ok(Ast::Class(ClassSet::perl(p)));
        }
        let ch = self.parse_escape_char()?;
        Ok(self.literal(ch))
    }

    /// The code-point-valued escapes, shared by the class parser.
    fn parse_escape_char(&mut self) -> Result<char, Error> {
        let start = self.pos;
        let Some(c) = self.bump() else {
            self.pos = start;
            return self.err("incomplete escape sequence, reached end of pattern prematurely");
        };
        match c {
            'a' => Ok('\u{7}'),
            'f' => Ok('\u{c}'),
            't' => Ok('\t'),
            'n' => Ok('\n'),
            'r' => Ok('\r'),
            'v' => Ok('\u{b}'),
            'x' => self.parse_hex(2),
            'u' => self.parse_hex(4),
            'U' => self.parse_hex(8),
            '0'..='9' => {
                self.pos = start.saturating_sub(1);
                self.err("backreferences are not supported")
            }
            'p' | 'P' => {
                self.pos = start.saturating_sub(1);
                self.err(
                    "Unicode property classes `\\p{…}` are not supported; name the code points \
                     with a character class instead",
                )
            }
            c if !c.is_alphanumeric() => Ok(c),
            _ => {
                self.pos = start.saturating_sub(1);
                self.err("unrecognized escape sequence")
            }
        }
    }

    /// `\xNN`, or the braced `\x{…}` form shared by `\u` and `\U`.
    fn parse_hex(&mut self, width: usize) -> Result<char, Error> {
        let start = self.pos;
        let (digits, end) = if self.peek() == Some('{') {
            self.pos += 1;
            let from = self.pos;
            while self.peek().is_some_and(|c| c != '}') {
                self.pos += 1;
            }
            if self.peek() != Some('}') {
                self.pos = start;
                return self.err("unclosed hexadecimal literal");
            }
            let d: String = self.pat[from..self.pos].iter().collect();
            self.pos += 1;
            (d, self.pos)
        } else {
            let from = self.pos;
            for _ in 0..width {
                if !self.peek().is_some_and(|c| c.is_ascii_hexdigit()) {
                    self.pos = start;
                    return self.err("invalid hexadecimal digit");
                }
                self.pos += 1;
            }
            (self.pat[from..self.pos].iter().collect(), self.pos)
        };
        if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_hexdigit()) {
            self.pos = start;
            return self.err("invalid hexadecimal digit");
        }
        let cp = u32::from_str_radix(&digits, 16)
            .ok()
            .filter(|&n| n <= char::MAX as u32);
        match cp.and_then(char::from_u32) {
            Some(c) => {
                self.pos = end;
                Ok(c)
            }
            None => {
                self.pos = start;
                self.err("hexadecimal literal is not a Unicode scalar value")
            }
        }
    }

    /// `[…]`, including `[^…]`, ranges, escapes, perl shorthands and POSIX
    /// names. Refuses the set-operation syntax rather than misreading it.
    fn parse_class(&mut self) -> Result<ClassSet, Error> {
        let open = self.pos;
        self.pos += 1; // '['
        // Likewise for the negation: in `x` mode `[ ^a]` is `[^a]`.
        self.skip_ignorable();
        let negated = self.peek() == Some('^');
        if negated {
            self.pos += 1;
        }
        let mut set = ClassSet { ranges: Vec::new(), perls: Vec::new(), negated };
        let mut first = true;
        loop {
            if self.flags.ignore_whitespace {
                self.skip_ignorable();
            }
            let Some(c) = self.peek() else {
                self.pos = open;
                return self.err("unclosed character class");
            };
            if c == ']' && !first {
                self.pos += 1;
                set.normalize();
                return Ok(set);
            }
            first = false;
            // Set operations would silently become "a, -, b" under a naive
            // reading, so they are refused outright.
            if matches!(c, '&' | '-' | '~') && self.peek_at(1) == Some(c) {
                return self.err(&format!(
                    "character-class set operations (`{c}{c}`) are not supported; escape the \
                     operator (`\\{c}`) to match it literally"
                ));
            }
            if c == '[' {
                if let Some(posix) = self.parse_posix_class()? {
                    set.ranges.extend_from_slice(posix);
                    self.reject_shorthand_range()?;
                    continue;
                }
                return self.err(
                    "nested character classes are not supported; escape the bracket (`\\[`) to \
                     match it literally",
                );
            }
            // An item: either a shorthand (never a range endpoint) or one code
            // point (which may open a range).
            let lo = if c == '\\' {
                self.pos += 1;
                let Some(e) = self.peek() else {
                    self.pos = open;
                    return self.err("unclosed character class");
                };
                if let Some(p) = perl_class(e) {
                    self.pos += 1;
                    set.perls.push(p);
                    self.reject_shorthand_range()?;
                    continue;
                }
                if matches!(e, 'b' | 'B' | 'A' | 'z' | '<' | '>') {
                    self.pos -= 1;
                    return self.err("invalid escape sequence found in character class");
                }
                self.parse_escape_char()?
            } else {
                self.pos += 1;
                c
            };
            if self.flags.ignore_whitespace {
                self.skip_ignorable();
            }
            // A `--` right after an item is the set-difference operator, not a
            // range whose end happens to be `-`. Refused for the same reason as
            // the other two: a naive reading silently means something else.
            if self.peek() == Some('-') && self.peek_at(1) == Some('-') {
                return self.err(
                    "character-class set operations (`--`) are not supported; escape the \
                     operator (`\\-`) to match it literally",
                );
            }
            // `-` is a range operator unless it closes the class.
            if self.peek() == Some('-') && self.peek_at(1) != Some(']') {
                let dash = self.pos;
                self.pos += 1;
                if self.flags.ignore_whitespace {
                    self.skip_ignorable();
                }
                let Some(h) = self.peek() else {
                    self.pos = open;
                    return self.err("unclosed character class");
                };
                let hi = if h == '\\' {
                    self.pos += 1;
                    if self.peek().is_some_and(|e| perl_class(e).is_some()) {
                        self.pos = dash;
                        return self.err("invalid range boundary, must be a literal");
                    }
                    self.parse_escape_char()?
                } else if h == '[' {
                    // `[a-[:digit:]]` is a bad range boundary; `[a-[b]]` is the
                    // nested-class syntax. Say which, so the message is
                    // actionable.
                    if self.parse_posix_class()?.is_some() {
                        self.pos = dash;
                        return self.err("invalid range boundary, must be a literal");
                    }
                    self.pos = dash;
                    return self.err(
                        "nested character classes are not supported; escape the bracket \
                         (`\\[`) to match it literally",
                    );
                } else {
                    self.pos += 1;
                    h
                };
                if hi < lo {
                    self.pos = dash;
                    return self.err(
                        "invalid character class range, the start must be <= the end",
                    );
                }
                set.ranges.push((lo, hi));
            } else {
                set.ranges.push((lo, lo));
            }
        }
    }

    /// A multi-code-point shorthand (`\d`, `[:alpha:]`) cannot be the low end of
    /// a range: `[\s-\d]` is an error in the `regex` crate, not the three-item
    /// class a naive parser would read it as.
    fn reject_shorthand_range(&mut self) -> Result<(), Error> {
        if self.peek() == Some('-') && self.peek_at(1) != Some(']') {
            return self.err("invalid range boundary, must be a literal");
        }
        Ok(())
    }

    /// `[:alpha:]` and friends — ASCII-only, exactly as in the `regex` crate.
    /// Returns `Ok(None)` when the bracket does not open a *known* POSIX name,
    /// which is the only case the caller may treat as an error.
    fn parse_posix_class(&mut self) -> Result<Option<Ranges>, Error> {
        if self.peek_at(1) != Some(':') {
            return Ok(None);
        }
        let mut i = self.pos + 2;
        let negated = self.pat.get(i) == Some(&'^');
        if negated {
            i += 1;
        }
        let from = i;
        while self.pat.get(i).is_some_and(|c| c.is_ascii_alphabetic()) {
            i += 1;
        }
        if self.pat.get(i) != Some(&':') || self.pat.get(i + 1) != Some(&']') {
            return Ok(None);
        }
        let name: String = self.pat[from..i].iter().collect();
        let Some(ranges) = posix_class(&name, negated) else {
            return Ok(None);
        };
        self.pos = i + 2;
        Ok(Some(ranges))
    }
}

fn perl_class(c: char) -> Option<PerlClass> {
    match c {
        'd' => Some(PerlClass::Digit),
        'D' => Some(PerlClass::NotDigit),
        's' => Some(PerlClass::Space),
        'S' => Some(PerlClass::NotSpace),
        'w' => Some(PerlClass::Word),
        'W' => Some(PerlClass::NotWord),
        _ => None,
    }
}

/// A static, sorted, inclusive range list — the shape every class table takes.
type Ranges = &'static [(char, char)];

/// The POSIX bracket classes. ASCII-only — `[[:alpha:]]` does not match `é` in
/// the `regex` crate either.
fn posix_class(name: &str, negated: bool) -> Option<Ranges> {
    const ALNUM: Ranges = &[('0', '9'), ('A', 'Z'), ('a', 'z')];
    const ALPHA: Ranges = &[('A', 'Z'), ('a', 'z')];
    const ASCII: Ranges = &[('\0', '\u{7f}')];
    const BLANK: Ranges = &[('\t', '\t'), (' ', ' ')];
    const CNTRL: Ranges = &[('\0', '\u{1f}'), ('\u{7f}', '\u{7f}')];
    const DIGIT: Ranges = &[('0', '9')];
    const GRAPH: Ranges = &[('!', '~')];
    const LOWER: Ranges = &[('a', 'z')];
    const PRINT: Ranges = &[(' ', '~')];
    const PUNCT: Ranges = &[('!', '/'), (':', '@'), ('[', '`'), ('{', '~')];
    const SPACE: Ranges = &[('\t', '\r'), (' ', ' ')];
    const UPPER: Ranges = &[('A', 'Z')];
    const WORD: Ranges = &[('0', '9'), ('A', 'Z'), ('_', '_'), ('a', 'z')];
    const XDIGIT: Ranges = &[('0', '9'), ('A', 'F'), ('a', 'f')];
    // Complements, precomputed: a class is one flat range list, and the class
    // parser has no way to express "negate just this item".
    const N_ALNUM: Ranges =
        &[('\0', '/'), (':', '@'), ('[', '`'), ('{', char::MAX)];
    const N_ALPHA: Ranges = &[('\0', '@'), ('[', '`'), ('{', char::MAX)];
    const N_ASCII: Ranges = &[('\u{80}', char::MAX)];
    const N_BLANK: Ranges =
        &[('\0', '\u{8}'), ('\n', '\u{1f}'), ('!', char::MAX)];
    const N_CNTRL: Ranges = &[(' ', '~'), ('\u{80}', char::MAX)];
    const N_DIGIT: Ranges = &[('\0', '/'), (':', char::MAX)];
    const N_GRAPH: Ranges = &[('\0', ' '), ('\u{7f}', char::MAX)];
    const N_LOWER: Ranges = &[('\0', '`'), ('{', char::MAX)];
    const N_PRINT: Ranges = &[('\0', '\u{1f}'), ('\u{7f}', char::MAX)];
    const N_PUNCT: Ranges =
        &[('\0', ' '), ('0', '9'), ('A', 'Z'), ('a', 'z'), ('\u{7f}', char::MAX)];
    const N_SPACE: Ranges =
        &[('\0', '\u{8}'), ('\u{e}', '\u{1f}'), ('!', char::MAX)];
    const N_UPPER: Ranges = &[('\0', '@'), ('[', char::MAX)];
    const N_WORD: Ranges =
        &[('\0', '/'), (':', '@'), ('[', '^'), ('`', '`'), ('{', char::MAX)];
    const N_XDIGIT: Ranges =
        &[('\0', '/'), (':', '@'), ('G', '`'), ('g', char::MAX)];

    let (yes, no): (Ranges, Ranges) = match name {
        "alnum" => (ALNUM, N_ALNUM),
        "alpha" => (ALPHA, N_ALPHA),
        "ascii" => (ASCII, N_ASCII),
        "blank" => (BLANK, N_BLANK),
        "cntrl" => (CNTRL, N_CNTRL),
        "digit" => (DIGIT, N_DIGIT),
        "graph" => (GRAPH, N_GRAPH),
        "lower" => (LOWER, N_LOWER),
        "print" => (PRINT, N_PRINT),
        "punct" => (PUNCT, N_PUNCT),
        "space" => (SPACE, N_SPACE),
        "upper" => (UPPER, N_UPPER),
        "word" => (WORD, N_WORD),
        "xdigit" => (XDIGIT, N_XDIGIT),
        _ => return None,
    };
    Some(if negated { no } else { yes })
}

/// Collapse a branch's items into one node.
fn take_concat(concat: &mut Vec<(Ast, usize)>) -> Ast {
    let items: Vec<Ast> = core::mem::take(concat).into_iter().map(|(a, _)| a).collect();
    match items.len() {
        0 => Ast::Empty,
        1 => items.into_iter().next().unwrap_or(Ast::Empty),
        _ => Ast::Concat(items),
    }
}

/// Collapse alternation branches into one node.
fn finish_alts(mut alts: Vec<Ast>) -> Ast {
    match alts.len() {
        0 => Ast::Empty,
        1 => alts.pop().unwrap_or(Ast::Empty),
        _ => Ast::Alt(alts),
    }
}

/// Depth of an already-built subtree, computed iteratively.
fn ast_depth(ast: &Ast) -> usize {
    let mut max = 0usize;
    let mut stack = vec![(ast, 0usize)];
    while let Some((node, d)) = stack.pop() {
        max = max.max(d);
        match node {
            Ast::Empty | Ast::Literal(_) | Ast::Class(_) | Ast::Assert(_) => {}
            Ast::Concat(v) | Ast::Alt(v) => stack.extend(v.iter().map(|c| (c, d + 1))),
            Ast::Repeat { node, .. } => stack.push((node, d + 1)),
        }
    }
    max
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ast(pattern: &str) -> Ast {
        parse(pattern, Flags::default()).expect("pattern parses")
    }

    fn message(pattern: &str) -> String {
        match parse(pattern, Flags::default()) {
            Err(Error::Syntax(msg)) => msg,
            other => panic!("expected a syntax error for {pattern:?}, got {other:?}"),
        }
    }

    /// The rendered error has to be readable by whoever typed the pattern: the
    /// pattern itself, a caret under the offending position, then the reason.
    #[test]
    fn errors_quote_the_pattern_and_point_at_the_position() {
        assert_eq!(
            message("ab[cd"),
            "regex parse error:\n    ab[cd\n      ^\nerror: unclosed character class"
        );
        assert_eq!(
            message("a(b"),
            "regex parse error:\n    a(b\n     ^\nerror: unclosed group"
        );
        assert!(message("*a").contains("repetition operator missing expression"));
    }

    /// Inline flags apply to the rest of the enclosing group and stop at its
    /// close paren — `(a(?i)b)c` matches `aBc` but not `aBC`.
    #[test]
    fn inline_flags_are_scoped_to_the_enclosing_group() {
        let flags = |p: &str| match parse(p, Flags::default()) {
            Ok(a) => format!("{a:?}"),
            Err(e) => panic!("{e}"),
        };
        // `(?i)` before a literal turns it into a class; after the group closes
        // it does not.
        assert!(flags("(a(?i)b)c").contains("Class"));
        assert!(!flags("(a(?i)b)c").ends_with("Class"));
        assert!(matches!(ast("(?i:a)"), Ast::Class(_)));
        assert!(matches!(ast("(?i)(?-i)a"), Ast::Literal('a')));
    }

    /// A single literal only becomes a class when `(?i)` actually gives it an
    /// orbit — `(?i)7` stays a literal, which keeps the common path cheap.
    #[test]
    fn case_folding_only_widens_what_it_must() {
        let i = Flags { case_insensitive: true, ..Flags::default() };
        assert!(matches!(parse("7", i), Ok(Ast::Literal('7'))));
        assert!(matches!(parse("a", i), Ok(Ast::Class(_))));
    }

    /// Ranges are sorted and coalesced so membership can binary search, and
    /// adjacency counts: `[a-mn-z]` is one range, not two.
    #[test]
    fn class_ranges_are_normalized() {
        let Ast::Class(set) = ast("[z-za-mn-yA]") else { panic!("a class") };
        assert_eq!(set.ranges, vec![('A', 'A'), ('a', 'z')]);
        assert!(set.matches('q') && set.matches('A') && !set.matches('B'));
    }

    /// Negation applies to the union of ranges *and* shorthands, and applies
    /// after case folding.
    #[test]
    fn negation_covers_the_whole_class() {
        let Ast::Class(set) = ast(r"[^\d a-c]") else { panic!("a class") };
        assert!(set.matches('z') && !set.matches('5') && !set.matches(' ') && !set.matches('b'));

        let i = Flags { case_insensitive: true, ..Flags::default() };
        let Ok(Ast::Class(set)) = parse("[^k]", i) else { panic!("a class") };
        assert!(!set.matches('k') && !set.matches('K') && !set.matches('\u{212a}'));
        assert!(set.matches('z'));
    }

    /// POSIX names are ASCII-only, and an unknown name is not silently read as a
    /// nested class full of colons.
    #[test]
    fn posix_classes_are_ascii_and_closed() {
        let Ast::Class(set) = ast("[[:alpha:]]") else { panic!("a class") };
        assert!(set.matches('a') && set.matches('Z') && !set.matches('\u{e9}'));
        let Ast::Class(set) = ast("[[:^digit:]]") else { panic!("a class") };
        assert!(set.matches('x') && !set.matches('4'));
        assert!(message("[[:nosuch:]]").contains("nested character classes"));
    }

    /// The depth cap is enforced while parsing, so neither the parser nor the
    /// tree's `Drop` can be driven off the stack.
    #[test]
    fn nesting_is_capped() {
        let ok = format!("{}a{}", "(".repeat(MAX_NESTING_DEPTH - 1), ")".repeat(MAX_NESTING_DEPTH - 1));
        assert!(parse(&ok, Flags::default()).is_ok());
        let too_deep = format!(
            "{}a{}",
            "(".repeat(MAX_NESTING_DEPTH + 1),
            ")".repeat(MAX_NESTING_DEPTH + 1)
        );
        assert!(message(&too_deep).contains("nests more than"));
        // Stacked quantifiers deepen the tree without opening a group.
        assert!(message(&format!("a{}", "*".repeat(MAX_NESTING_DEPTH + 1))).contains("nests more than"));
    }

    /// Whitespace inside `{…}` is insignificant even outside `x` mode, which is
    /// what the `regex` crate does.
    #[test]
    fn counted_repetition_tolerates_spaces() {
        assert!(matches!(
            ast("a{ 2 , 3 }"),
            Ast::Repeat { min: 2, max: Some(3), .. }
        ));
        assert!(matches!(ast("a{2,}"), Ast::Repeat { min: 2, max: None, .. }));
        assert!(matches!(
            ast("a{2}?"),
            Ast::Repeat { min: 2, max: Some(2), greedy: false, .. }
        ));
    }

    /// `(?U)` swaps greediness, and an explicit `?` swaps it back.
    #[test]
    fn swap_greed_inverts_and_composes() {
        let u = Flags { swap_greed: true, ..Flags::default() };
        assert!(matches!(parse("a*", u), Ok(Ast::Repeat { greedy: false, .. })));
        assert!(matches!(parse("a*?", u), Ok(Ast::Repeat { greedy: true, .. })));
    }

    /// A class costs bytes, and those bytes are charged against the compile
    /// ceiling — so the accounting has to see them.
    #[test]
    fn classes_report_their_size() {
        let Ast::Class(small) = ast("[a]") else { panic!("a class") };
        let Ast::Class(big) = ast("[a-cx-zA-F0-9_]") else { panic!("a class") };
        assert!(big.byte_size() > small.byte_size());
        assert!(small.byte_size() >= size_of::<ClassSet>());
    }
}
