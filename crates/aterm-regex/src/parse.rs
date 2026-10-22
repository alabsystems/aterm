// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pattern text → [`Ast`].
//!
//! A single flat loop over the pattern's code points with an explicit [`Group`] stack,
//! so no `(((((…)))))` can recurse it into the guard page. The [`Ast`] it produces, and
//! its `Drop`, are recursive, so every node records its depth and parsing fails past
//! [`MAX_NESTING_DEPTH`].
//!
//! ## Syntax
//!
//! Literals, `.`, character classes (ranges, negation, escapes, `\d\w\s`, POSIX names),
//! the assertions `^ $ \A \z \b \B \< \>`, groups (capturing, `(?:`, named) with inline
//! flags `i m s x U`, alternation, and the quantifiers `* + ? {n} {n,} {n,m}`, greedy and
//! lazy. The constructs `regex` accepts and this engine refuses are listed in the crate
//! docs; each is an [`Error::Syntax`] naming it, never a reinterpretation (a naive reading
//! of `[a--b]` is "a, -, b"). Backreferences and look-around are refused, as in `regex`.

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
    #[inline]
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
        Self {
            ranges: vec![(lo, hi)],
            perls: Vec::new(),
            negated: false,
        }
    }

    /// The class every code point belongs to (`(?s).`).
    pub(crate) fn any() -> Self {
        Self::range('\0', char::MAX)
    }

    /// `.` outside `(?s)`: everything except the line terminator.
    pub(crate) fn any_except_newline() -> Self {
        let mut set = Self {
            ranges: vec![('\n', '\n')],
            perls: Vec::new(),
            negated: true,
        };
        set.normalize();
        set
    }

    /// One perl class on its own (`\d`, `\W`, …).
    fn perl(p: PerlClass) -> Self {
        Self {
            ranges: Vec::new(),
            perls: vec![p],
            negated: false,
        }
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
                // Coalesce overlapping and adjacent ranges: `[a-mn-z]` is one range.
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

    /// Mark, in `out`, every leading UTF-8 byte of a code point this class can match. A
    /// continuation byte (`0x80..=0xBF`) is never marked, so the prefilter's byte-at-a-time
    /// walk can only stop on a boundary. The non-ASCII side is marked conservatively, which
    /// is safe: the prefilter skips only positions no candidate could match.
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
    #[inline]
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

    /// Add one POSIX item (`[:lower:]` or `[:^lower:]`) to this class.
    ///
    /// The item's own `^` is applied after folding, as a bracketed class's is (`regex`'s
    /// `unicode_fold_and_negate`). The other order is wrong: the complement of `[a-z]`
    /// holds `A-Z`, `ſ` and the KELVIN SIGN, whose orbits cover `a-z`, so folding it
    /// yields every code point and `(?i)[[:^lower:]]` matches letters. Folding the whole
    /// class again when it closes is idempotent: a complement taken after folding holds no
    /// member of any orbit it excluded.
    fn add_posix(&mut self, table: &[(char, char)], negated: bool, case_insensitive: bool) {
        let mut item = Self {
            ranges: table.to_vec(),
            perls: Vec::new(),
            negated: false,
        };
        if case_insensitive {
            item.case_fold();
        } else {
            item.normalize();
        }
        if negated {
            item.ranges = complement(&item.ranges);
        }
        self.ranges.append(&mut item.ranges);
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
    Repeat {
        node: Box<Ast>,
        min: u32,
        max: Option<u32>,
        greedy: bool,
    },
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
    /// Capture names, tracked only to reject a duplicate as `regex` does; captures are
    /// otherwise ignored.
    group_names: Vec<String>,
    /// A flag directive (`(?i)`) is the last thing parsed in this branch. It is an item
    /// in `regex`'s grammar, so a quantifier after it has nothing to repeat and must not
    /// reach back to the item before it. Set by the directive, cleared by every push.
    after_flag_directive: bool,
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
        after_flag_directive: false,
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

    #[inline]
    fn peek(&self) -> Option<char> {
        self.pat.get(self.pos).copied()
    }

    #[inline]
    fn peek_at(&self, n: usize) -> Option<char> {
        self.pat.get(self.pos + n).copied()
    }

    #[inline]
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

    #[inline]
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
        self.after_flag_directive = false;
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
        let mut set = ClassSet {
            ranges: orbit,
            perls: Vec::new(),
            negated: false,
        };
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
        if self.after_flag_directive {
            return self.err("repetition operator missing expression");
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
            Ast::Repeat {
                node: Box::new(node),
                min,
                max,
                greedy,
            },
            depth + 1,
        ));
        Ok(())
    }

    /// `{n}`, `{n,}`, `{n,m}`.
    ///
    /// An unescaped `{` always opens one, as in `regex` (`a{b}` and `a{,3}` are errors
    /// there too). A literal brace is `\{`; a lone `}` is a literal.
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
                            self.after_flag_directive = true;
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
        // In `x` mode `[ ^a]` is `[^a]`.
        self.skip_ignorable();
        let negated = self.peek() == Some('^');
        if negated {
            self.pos += 1;
        }
        let mut set = ClassSet {
            ranges: Vec::new(),
            perls: Vec::new(),
            negated,
        };
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
                if let Some((table, negated)) = self.parse_posix_class()? {
                    set.add_posix(table, negated, self.flags.case_insensitive);
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
            // A `--` right after an item is set difference, not a range ending in `-`.
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
                    return self.err("invalid character class range, the start must be <= the end");
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

    /// `[:alpha:]` and friends — ASCII-only, exactly as in the `regex` crate —
    /// as the item's positive table and whether it is negated (`[:^alpha:]`).
    /// Returns `Ok(None)` when the bracket does not open a *known* POSIX name,
    /// which is the only case the caller may treat as an error.
    fn parse_posix_class(&mut self) -> Result<Option<(Ranges, bool)>, Error> {
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
        let Some(table) = posix_class(&name) else {
            return Ok(None);
        };
        self.pos = i + 2;
        Ok(Some((table, negated)))
    }
}

#[inline]
const fn perl_class(c: char) -> Option<PerlClass> {
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

/// The POSIX bracket classes, ASCII-only as in `regex`. Positive tables only: an item's
/// `^` is applied by [`ClassSet::add_posix`] after folding.
fn posix_class(name: &str) -> Option<Ranges> {
    let table: Ranges = match name {
        "alnum" => &[('0', '9'), ('A', 'Z'), ('a', 'z')],
        "alpha" => &[('A', 'Z'), ('a', 'z')],
        "ascii" => &[('\0', '\u{7f}')],
        "blank" => &[('\t', '\t'), (' ', ' ')],
        "cntrl" => &[('\0', '\u{1f}'), ('\u{7f}', '\u{7f}')],
        "digit" => &[('0', '9')],
        "graph" => &[('!', '~')],
        "lower" => &[('a', 'z')],
        "print" => &[(' ', '~')],
        "punct" => &[('!', '/'), (':', '@'), ('[', '`'), ('{', '~')],
        "space" => &[('\t', '\r'), (' ', ' ')],
        "upper" => &[('A', 'Z')],
        "word" => &[('0', '9'), ('A', 'Z'), ('_', '_'), ('a', 'z')],
        "xdigit" => &[('0', '9'), ('A', 'F'), ('a', 'f')],
        _ => return None,
    };
    Some(table)
}

/// The code point after `c`, stepping over the surrogate gap.
#[inline]
const fn char_after(c: char) -> Option<char> {
    match c {
        '\u{d7ff}' => Some('\u{e000}'),
        _ => char::from_u32(c as u32 + 1),
    }
}

/// The code point before `c`, stepping over the surrogate gap.
#[inline]
const fn char_before(c: char) -> Option<char> {
    match c {
        '\u{e000}' => Some('\u{d7ff}'),
        _ => match (c as u32).checked_sub(1) {
            Some(cp) => char::from_u32(cp),
            None => None,
        },
    }
}

/// Every code point NOT in `ranges`, which must be sorted and disjoint (what
/// [`ClassSet::normalize`] leaves), as sorted, disjoint ranges.
fn complement(ranges: &[(char, char)]) -> Vec<(char, char)> {
    let mut out = Vec::with_capacity(ranges.len() + 1);
    let mut from = Some('\0');
    for &(lo, hi) in ranges {
        if let Some(f) = from
            && f < lo
            && let Some(to) = char_before(lo)
        {
            out.push((f, to));
        }
        from = char_after(hi);
    }
    if let Some(f) = from {
        out.push((f, char::MAX));
    }
    out
}

/// Collapse a branch's items into one node.
fn take_concat(concat: &mut Vec<(Ast, usize)>) -> Ast {
    let items: Vec<Ast> = core::mem::take(concat)
        .into_iter()
        .map(|(a, _)| a)
        .collect();
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

    /// The rendered error quotes the pattern and puts a caret under the
    /// offending position (column, not wording, is what this pins).
    #[test]
    fn errors_quote_the_pattern_and_point_at_the_position() {
        for (pattern, column) in [("ab[cd", 2usize), ("a(b", 1)] {
            let msg = message(pattern);
            let mut lines = msg.lines().skip(1);
            let quoted = lines.next().expect("the pattern line");
            let caret = lines.next().expect("the caret line");
            let indent = quoted.len() - pattern.len();
            assert_eq!(&quoted[indent..], pattern, "{msg}");
            assert_eq!(caret.find('^'), Some(indent + column), "{msg}");
        }
    }

    /// POSIX names are ASCII-only, and an unknown name is not silently read as a
    /// nested class full of colons.
    #[test]
    fn posix_classes_are_ascii_and_closed() {
        let Ast::Class(set) = ast("[[:alpha:]]") else {
            panic!("a class")
        };
        assert!(set.matches('a') && set.matches('Z') && !set.matches('\u{e9}'));
        let Ast::Class(set) = ast("[[:^digit:]]") else {
            panic!("a class")
        };
        assert!(set.matches('x') && !set.matches('4'));
        assert!(message("[[:nosuch:]]").contains("nested character classes"));
    }

    /// An item's own `^` is the exact complement of its folded table, over every code
    /// point a POSIX table or its fold reaches and the edges of the scalar-value space.
    #[test]
    fn a_negated_posix_item_is_the_complement_of_its_folded_table() {
        let names = [
            "alnum", "alpha", "ascii", "blank", "cntrl", "digit", "graph", "lower", "print",
            "punct", "space", "upper", "word", "xdigit",
        ];
        let edges = [0xd7ff, 0xe000, 0x10_ffff];
        for name in names {
            let table = posix_class(name).expect("a known name");
            for case_insensitive in [false, true] {
                let (mut yes, mut no) = (ClassSet::default(), ClassSet::default());
                yes.add_posix(table, false, case_insensitive);
                no.add_posix(table, true, case_insensitive);
                yes.normalize();
                no.normalize();
                for c in (0..=0x2200u32).chain(edges).filter_map(char::from_u32) {
                    assert_ne!(
                        yes.matches(c),
                        no.matches(c),
                        "[:{name}:] and [:^{name}:] (case-insensitive: {case_insensitive}) \
                         agree about U+{:04X}",
                        u32::from(c)
                    );
                }
            }
        }
        // Fold before complement: `(?i)[:^lower:]` excludes the exotic members of the
        // `k` and `s` orbits along with `A-Z`.
        let mut no = ClassSet::default();
        no.add_posix(posix_class("lower").expect("known"), true, true);
        for c in ['a', 'Z', '\u{17f}', '\u{212a}'] {
            assert!(!no.matches(c), "(?i)[:^lower:] must not hold {c:?}");
        }
        assert!(no.matches('1') && no.matches('\u{e9}'));
    }

    /// Whitespace inside `{…}` is insignificant even outside `x` mode, which is
    /// what the `regex` crate does.
    #[test]
    fn counted_repetition_tolerates_spaces() {
        assert!(matches!(
            ast("a{ 2 , 3 }"),
            Ast::Repeat {
                min: 2,
                max: Some(3),
                ..
            }
        ));
        assert!(matches!(
            ast("a{2,}"),
            Ast::Repeat {
                min: 2,
                max: None,
                ..
            }
        ));
        assert!(matches!(
            ast("a{2}?"),
            Ast::Repeat {
                min: 2,
                max: Some(2),
                greedy: false,
                ..
            }
        ));
    }

    /// `(?U)` swaps greediness, and an explicit `?` swaps it back.
    #[test]
    fn swap_greed_inverts_and_composes() {
        let u = Flags {
            swap_greed: true,
            ..Flags::default()
        };
        assert!(matches!(
            parse("a*", u),
            Ok(Ast::Repeat { greedy: false, .. })
        ));
        assert!(matches!(
            parse("a*?", u),
            Ok(Ast::Repeat { greedy: true, .. })
        ));
    }

    /// A class costs bytes, and those bytes are charged against the compile
    /// ceiling — so the accounting has to see them.
    #[test]
    fn classes_report_their_size() {
        let Ast::Class(small) = ast("[a]") else {
            panic!("a class")
        };
        let Ast::Class(big) = ast("[a-cx-zA-F0-9_]") else {
            panic!("a class")
        };
        assert!(big.byte_size() > small.byte_size());
        assert!(small.byte_size() >= size_of::<ClassSet>());
    }
}
