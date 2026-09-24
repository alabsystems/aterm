// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Is the `rm` behind Claude Code's "Dangerous rm operation on
//! possibly-empty variable path" box safe to wave through? The resolver the
//! approval policy's `rm-breaker` rule asks ([`resolve_rm_line`]).
//!
//! **Why a resolver of its own.** The vendor draws that box in a
//! bypass-permissions session because an `rm` operand holds a `$VAR` it cannot
//! prove non-empty. In bypass the rest of the line would run unasked anyway,
//! so the one question is where each `rm` operand POINTS. `harness::rm_policy`
//! answers a different question (is every other segment a read?) and abstains
//! on every `$` by design, so it cannot answer this box; its critical-path
//! deny table and its flag table are reused here (`rm_policy::denied`,
//! `rm_policy::is_known_flag`).
//!
//! **What resolves.** The line is read left to right as straight-line shell,
//! with a variable table filled by literal assignments only:
//! `NAME=/abs/lit`, `NAME=$OTHER/lit` (OTHER already resolved),
//! `NAME=$(mktemp -d …)` (a fresh directory under `$TMPDIR`, or under the
//! literal template's directory), and `$TMPDIR` as the scope's. An assignment counts only where it surely
//! runs: first in its `&&`/`||` list and neither piped nor backgrounded
//! (`test -d x || S=/y` leaves `S` unknown; so does `S=/y rm …`, whose
//! assignment the same command's expansion never sees).
//!
//! **What escalates** — everything else, by construction: a loop or any other
//! compound command, `set`/`shift`/`eval`/`read`/`local` and the other
//! builtins that change a variable, a subshell or group, a backtick, a
//! here-document, `$'…'`, a `${…}` form other than `${NAME}` and
//! `${NAME:?…}`, a positional parameter, an unassigned or unknown variable,
//! `$HOME` and `~`, a relative operand and `$PWD`, an `rm` word anywhere but
//! at a command head (`xargs rm`, `sh -c 'rm …'`) — in any letter case,
//! since the default macOS volume finds `/bin/rm` for `Rm` — an unknown `rm`
//! flag, a redirect on the `rm` other than `/dev/null` or a descriptor.
//!
//! **Why no relative operand and no `$PWD`.** The only working directory
//! the supervisor can read is the session's (its `meta cwd=`, the shell's
//! OSC 7): where Claude Code was LAUNCHED. The Bash tool keeps a working
//! directory of its own that the worker moves between calls, so a relative
//! operand or `$PWD` resolved against the launch directory can point
//! anywhere (lane B's review, 2026-09-23). The launch directory is still
//! used for what it is — the `<cwd>/target*` root and the guard that no
//! operand is it or an ancestor of it.
//!
//! **Where an operand may point.** Strictly inside a [`ScratchRoot`] (never
//! the root itself), with no glob at or above the first component under the
//! root, no `..`, no `.git`, not the cwd or an ancestor of it, and past every
//! default `rm_policy` deny pattern (the filesystem root, the home directory
//! and its ancestors, `/Users//<x>`, a glob in the first component). An operand
//! built on a `$(mktemp …)` variable may not glob at all: were `mktemp` to
//! fail, the variable is empty and `"$D"/*` is `/*`.
//!
//! Lexical, like `rm_policy`: nothing touches the filesystem, so a symlink
//! inside a scratch root is not followed.

use std::collections::HashMap;
use std::path::Path;

use crate::harness::rm_policy::{self, RmPolicy, has_glob, is_known_flag};
use crate::supervise::classify::glob_match;

/// The stand-in for the unique name `mktemp` makes: a component no deny rule
/// and no literal root can match, rendered as `<mktemp>` in a target.
const MKTEMP_NAME: &str = "\u{1}mktemp";

/// One component of a [`ScratchRoot`] pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RootComp {
    Lit(String),
    /// A `*`/`?` glob over ONE component (`target*`, `*`).
    Glob(String),
}

/// A directory pattern: the scratch roots an `rm` operand must sit strictly
/// inside, and the trust roots a folder-trust dialog's path must sit in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScratchRoot {
    comps: Vec<RootComp>,
    label: String,
}

impl ScratchRoot {
    /// Exactly this absolute directory. `None` for a relative path or one with
    /// a `..`.
    pub fn dir(path: &Path) -> Option<Self> {
        let comps = abs_components(&path.to_string_lossy())?;
        Some(Self {
            label: path.to_string_lossy().trim_end_matches('/').to_string(),
            comps: comps.into_iter().map(RootComp::Lit).collect(),
        })
    }

    /// Any direct child of this directory (`/tmp/<x>/`).
    pub fn children_of(path: &Path) -> Option<Self> {
        Self::glob_under(path, "*")
    }

    /// Any direct child of `path` whose name matches `glob` (`<cwd>/target*`).
    pub fn glob_under(path: &Path, glob: &str) -> Option<Self> {
        let mut root = Self::dir(path)?;
        root.comps.push(RootComp::Glob(glob.to_string()));
        root.label = format!("{}/{glob}", root.label);
        Some(root)
    }

    /// The pattern as written (`/private/tmp/*`), for a reason or a ledger row.
    pub fn label(&self) -> &str {
        &self.label
    }

    fn depth(&self) -> usize {
        self.comps.len()
    }

    /// Whether `comps` begins with this pattern.
    fn prefixes(&self, comps: &[String]) -> bool {
        comps.len() >= self.comps.len()
            && self.comps.iter().zip(comps).all(|(p, c)| match p {
                RootComp::Lit(l) => l == c,
                RootComp::Glob(g) => glob_match(g, c),
            })
    }

    /// Whether the absolute path `comps` is this root or inside it (a trust
    /// root's test).
    pub fn holds(&self, comps: &[String]) -> bool {
        self.prefixes(comps)
    }

    /// Whether `comps` is STRICTLY inside this root with no glob in any
    /// component down to the first one under it: `rm -rf <root>/*` would
    /// empty the root, `rm -rf <root>/x/*` stays inside `x`.
    fn holds_strictly_unglobbed(&self, comps: &[String]) -> bool {
        comps.len() > self.depth()
            && self.prefixes(comps)
            && !comps[..=self.depth()].iter().any(|c| has_glob(c))
    }
}

/// What an `rm` operand is resolved against.
#[derive(Debug, Clone, Copy)]
pub struct RmScope<'a> {
    /// The session's working directory (absolute).
    pub cwd: &'a Path,
    /// The owner's home directory, for the deny table.
    pub home: Option<&'a Path>,
    /// The worker's `$TMPDIR`, for `$TMPDIR` and `$(mktemp -d)`; `None` leaves
    /// both unresolved.
    pub tmpdir: Option<&'a Path>,
    /// Where an operand may point.
    pub roots: &'a [ScratchRoot],
}

/// Resolve every `rm` on `cmd`: `Ok` with each operand's resolved path when
/// every one points strictly inside a scratch root (module header), else
/// `Err` naming the first thing that did not resolve or did not qualify.
pub fn resolve_rm_line(cmd: &str, scope: &RmScope<'_>) -> Result<Vec<String>, String> {
    let toks = lex(cmd)?;
    let cmds = commands(toks)?;
    let cwd = abs_components(&scope.cwd.to_string_lossy())
        .ok_or_else(|| "the session cwd is not an absolute path".to_string())?;
    let mut ev = Eval {
        scope,
        cwd,
        env: HashMap::new(),
        targets: Vec::new(),
    };
    for cmd in &cmds {
        ev.command(cmd)?;
    }
    if ev.targets.is_empty() {
        return Err("no rm at a command head on this line".to_string());
    }
    Ok(ev.targets)
}

// ---------------------------------------------------------------------------
// Lexing: words made of literal, variable and substitution parts
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum Part {
    Lit {
        text: String,
        quoted: bool,
    },
    Var {
        name: String,
        quoted: bool,
    },
    /// `$(…)`, its inner text.
    Subst(String),
}

type Word = Vec<Part>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Start,
    Semi,
    Newline,
    And,
    Or,
    Pipe,
    Amp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    Word(Word),
    Op(Op),
    /// A redirect operator; `Some(target)` when the target is glued to it
    /// (`>&1`, `2>&-`), else the next word is its target.
    Redir(String, Option<String>),
}

struct Lexer {
    cs: Vec<char>,
    i: usize,
    toks: Vec<Tok>,
    word: Word,
    in_word: bool,
}

impl Lexer {
    fn peek(&self, k: usize) -> Option<char> {
        self.cs.get(self.i + k).copied()
    }

    fn lit(&mut self, c: char, quoted: bool) {
        self.in_word = true;
        if let Some(Part::Lit { text, quoted: q }) = self.word.last_mut()
            && *q == quoted
        {
            text.push(c);
            return;
        }
        self.word.push(Part::Lit {
            text: c.to_string(),
            quoted,
        });
    }

    fn end_word(&mut self) {
        if self.in_word {
            self.toks.push(Tok::Word(std::mem::take(&mut self.word)));
            self.in_word = false;
        }
    }

    fn op(&mut self, op: Op, len: usize) {
        self.end_word();
        self.toks.push(Tok::Op(op));
        self.i += len;
    }

    fn run(&mut self) -> Result<(), String> {
        while let Some(c) = self.peek(0) {
            match c {
                ' ' | '\t' => {
                    self.end_word();
                    self.i += 1;
                }
                '\n' => self.op(Op::Newline, 1),
                ';' if self.peek(1) == Some(';') => return Err("a `;;` (case arm)".to_string()),
                ';' => self.op(Op::Semi, 1),
                '&' if self.peek(1) == Some('&') => self.op(Op::And, 2),
                '&' if self.peek(1) == Some('>') => self.redirect()?,
                '&' => self.op(Op::Amp, 1),
                '|' if self.peek(1) == Some('|') => self.op(Op::Or, 2),
                '|' if self.peek(1) == Some('&') => return Err("a `|&` pipe".to_string()),
                '|' => self.op(Op::Pipe, 1),
                '(' | ')' => return Err("a subshell or group".to_string()),
                '<' | '>' => self.redirect()?,
                '#' if !self.in_word => {
                    while self.peek(0).is_some_and(|d| d != '\n') {
                        self.i += 1;
                    }
                }
                '\'' => {
                    self.i += 1;
                    let mut any = false;
                    loop {
                        match self.peek(0) {
                            None => return Err("an unterminated ' quote".to_string()),
                            Some('\'') => break,
                            Some(d) => {
                                self.lit(d, true);
                                any = true;
                                self.i += 1;
                            }
                        }
                    }
                    self.i += 1;
                    if !any {
                        self.word.push(Part::Lit {
                            text: String::new(),
                            quoted: true,
                        });
                        self.in_word = true;
                    }
                }
                '"' => self.double_quoted()?,
                '\\' => match self.peek(1) {
                    Some('\n') => self.i += 2,
                    Some(n) => {
                        self.lit(n, true);
                        self.i += 2;
                    }
                    None => return Err("a trailing backslash".to_string()),
                },
                '$' => self.dollar(false)?,
                '`' => return Err("a backtick substitution".to_string()),
                _ => {
                    self.lit(c, false);
                    self.i += 1;
                }
            }
        }
        self.end_word();
        Ok(())
    }

    fn double_quoted(&mut self) -> Result<(), String> {
        self.i += 1;
        self.in_word = true;
        let before = self.word.len();
        loop {
            match self.peek(0) {
                None => return Err("an unterminated \" quote".to_string()),
                Some('"') => {
                    self.i += 1;
                    break;
                }
                Some('\\') => match self.peek(1) {
                    Some('\n') => self.i += 2,
                    Some(n) if matches!(n, '$' | '`' | '"' | '\\') => {
                        self.lit(n, true);
                        self.i += 2;
                    }
                    Some(n) => {
                        self.lit('\\', true);
                        self.lit(n, true);
                        self.i += 2;
                    }
                    None => return Err("an unterminated \" quote".to_string()),
                },
                Some('$') => self.dollar(true)?,
                Some('`') => return Err("a backtick substitution".to_string()),
                Some(d) => {
                    self.lit(d, true);
                    self.i += 1;
                }
            }
        }
        if self.word.len() == before {
            self.word.push(Part::Lit {
                text: String::new(),
                quoted: true,
            });
        }
        Ok(())
    }

    /// At a `$`: a variable, `${NAME}` / `${NAME:?…}`, or `$(…)`.
    fn dollar(&mut self, quoted: bool) -> Result<(), String> {
        self.in_word = true;
        match self.peek(1) {
            Some('{') => {
                let start = self.i + 2;
                let Some(len) = self.cs[start..].iter().position(|&d| d == '}') else {
                    return Err("an unterminated ${".to_string());
                };
                let inner: String = self.cs[start..start + len].iter().collect();
                self.i = start + len + 1;
                let name_len = inner
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .count();
                let (name, rest) = inner.split_at(name_len);
                let plain = rest.is_empty()
                    || ((rest.starts_with(":?") || rest.starts_with('?'))
                        && !rest.contains(['$', '`', '\'', '"', '\\', '{']));
                if !valid_name(name) || !plain {
                    return Err(format!("a ${{{inner}}} form this resolver does not follow"));
                }
                self.word.push(Part::Var {
                    name: name.to_string(),
                    quoted,
                });
            }
            Some('(') if self.peek(2) == Some('(') => {
                return Err("an arithmetic expansion".to_string());
            }
            Some('(') => {
                let start = self.i + 2;
                let mut depth = 0usize;
                let mut k = start;
                let end = loop {
                    match self.cs.get(k) {
                        None => return Err("an unterminated $(".to_string()),
                        Some('(') => depth += 1,
                        Some(')') if depth == 0 => break k,
                        Some(')') => depth -= 1,
                        Some('\'' | '"' | '`' | '\\') => {
                            return Err("a quote inside a $( … )".to_string());
                        }
                        _ => {}
                    }
                    k += 1;
                };
                self.word
                    .push(Part::Subst(self.cs[start..end].iter().collect()));
                self.i = end + 1;
            }
            Some('\'' | '"') if !quoted => {
                return Err("$'…' or $\"…\" quoting".to_string());
            }
            Some(c) if c.is_ascii_alphabetic() || c == '_' => {
                let start = self.i + 1;
                let len = self.cs[start..]
                    .iter()
                    .take_while(|c| c.is_ascii_alphanumeric() || **c == '_')
                    .count();
                self.word.push(Part::Var {
                    name: self.cs[start..start + len].iter().collect(),
                    quoted,
                });
                self.i = start + len;
            }
            Some(c) if c.is_ascii_digit() || "@*#?$!-".contains(c) => {
                self.word.push(Part::Var {
                    name: c.to_string(),
                    quoted,
                });
                self.i += 2;
            }
            _ => {
                self.lit('$', quoted);
                self.i += 1;
            }
        }
        Ok(())
    }

    /// `[n]>`, `>>`, `>|`, `&>`, `>&n`, `<`: the operator, and its target when
    /// glued to a `&`. A here-document or process substitution is refused.
    fn redirect(&mut self) -> Result<(), String> {
        let mut op = String::new();
        // A descriptor number just typed is the operator's, not a word.
        if let Some(Part::Lit {
            text,
            quoted: false,
        }) = self.word.last()
            && self.word.len() == 1
            && !text.is_empty()
            && text.chars().all(|c| c.is_ascii_digit())
        {
            op.push_str(text);
            self.word.clear();
            self.in_word = false;
        }
        self.end_word();
        if self.peek(0) == Some('&') {
            op.push('&');
            self.i += 1;
        }
        let first = self.peek(0).unwrap_or('>');
        if first == '<' && matches!(self.peek(1), Some('<')) {
            return Err("a here-document".to_string());
        }
        if matches!(self.peek(1), Some('(')) {
            return Err("a process substitution".to_string());
        }
        op.push(first);
        self.i += 1;
        while let Some(d) = self.peek(0).filter(|d| matches!(d, '>' | '|')) {
            op.push(d);
            self.i += 1;
        }
        if self.peek(0) == Some('&') {
            self.i += 1;
            let mut target = String::from("&");
            while let Some(d) = self.peek(0).filter(|d| d.is_ascii_digit() || *d == '-') {
                target.push(d);
                self.i += 1;
            }
            self.toks.push(Tok::Redir(op, Some(target)));
        } else {
            self.toks.push(Tok::Redir(op, None));
        }
        Ok(())
    }
}

fn lex(src: &str) -> Result<Vec<Tok>, String> {
    let mut lx = Lexer {
        cs: src.chars().collect(),
        i: 0,
        toks: Vec::new(),
        word: Vec::new(),
        in_word: false,
    };
    lx.run()?;
    Ok(lx.toks)
}

// ---------------------------------------------------------------------------
// Simple commands, in order, with the operators on either side
// ---------------------------------------------------------------------------

struct Cmd {
    words: Vec<Word>,
    redirs: Vec<(String, Option<Word>, Option<String>)>,
    prev: Op,
    next: Op,
}

fn commands(toks: Vec<Tok>) -> Result<Vec<Cmd>, String> {
    let mut out = Vec::new();
    let mut cur = Cmd {
        words: Vec::new(),
        redirs: Vec::new(),
        prev: Op::Start,
        next: Op::Newline,
    };
    let mut it = toks.into_iter().peekable();
    while let Some(tok) = it.next() {
        match tok {
            Tok::Word(w) => cur.words.push(w),
            Tok::Redir(op, Some(target)) => cur.redirs.push((op, None, Some(target))),
            Tok::Redir(op, None) => match it.next() {
                Some(Tok::Word(w)) => cur.redirs.push((op, Some(w), None)),
                _ => return Err(format!("a `{op}` with no target")),
            },
            Tok::Op(op) => {
                let empty = cur.words.is_empty() && cur.redirs.is_empty();
                if empty && op != Op::Newline && op != Op::Semi {
                    return Err("an operator with no command before it".to_string());
                }
                let prev = if empty { cur.prev } else { op };
                if !empty {
                    cur.next = op;
                    out.push(std::mem::replace(
                        &mut cur,
                        Cmd {
                            words: Vec::new(),
                            redirs: Vec::new(),
                            prev,
                            next: Op::Newline,
                        },
                    ));
                }
            }
        }
    }
    if !cur.words.is_empty() || !cur.redirs.is_empty() {
        if matches!(cur.prev, Op::And | Op::Or | Op::Pipe) && cur.words.is_empty() {
            return Err("an operator with no command after it".to_string());
        }
        out.push(cur);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Straight-line evaluation
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum Val {
    /// The value, and whether it can be EMPTY at run time (a `$(mktemp)`
    /// that failed).
    Known {
        text: String,
        maybe_empty: bool,
    },
    Unknown,
}

/// Shell words that open or close a compound command.
const KEYWORDS: &[&str] = &[
    "for", "while", "until", "select", "case", "esac", "if", "then", "elif", "else", "fi", "do",
    "done", "function", "{", "}", "!", "[[", "coproc", "time",
];

/// Builtins that can change a variable, the positional parameters, what a
/// name runs, or the line's own text.
const VAR_BUILTINS: &[&str] = &[
    "set",
    "shift",
    "unset",
    "declare",
    "typeset",
    "local",
    "readonly",
    "read",
    "mapfile",
    "readarray",
    "getopts",
    "eval",
    "source",
    ".",
    "exec",
    "alias",
    "unalias",
    "trap",
    "let",
    "enable",
    "hash",
    "builtin",
    "command",
];

struct Eval<'a> {
    scope: &'a RmScope<'a>,
    cwd: Vec<String>,
    env: HashMap<String, Val>,
    targets: Vec<String>,
}

fn valid_name(name: &str) -> bool {
    let mut cs = name.chars();
    matches!(cs.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && cs.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The word as literal text when every part is literal.
fn literal(word: &Word) -> Option<String> {
    let mut s = String::new();
    for p in word {
        match p {
            Part::Lit { text, .. } => s.push_str(text),
            _ => return None,
        }
    }
    Some(s)
}

/// `NAME=` at the head of an unquoted first part: `(NAME, the value's parts)`.
fn assignment(word: &Word) -> Option<(String, Word)> {
    let Some(Part::Lit {
        text,
        quoted: false,
    }) = word.first()
    else {
        return None;
    };
    let (name, rest) = text.split_once('=')?;
    if !valid_name(name) {
        return None;
    }
    let mut value = Vec::new();
    if !rest.is_empty() {
        value.push(Part::Lit {
            text: rest.to_string(),
            quoted: false,
        });
    }
    value.extend(word[1..].iter().cloned());
    Some((name.to_string(), value))
}

/// Whether some word or substitution on a command carries an `rm` token
/// that is not the command's own head.
fn mentions_rm(text: &str) -> bool {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '/' | '-')))
        .map(str::to_ascii_lowercase)
        .any(|w| w == "rm" || w.ends_with("/rm"))
}

/// `rm` by any letter case: the default macOS volume is case-insensitive,
/// so `Rm` and `/BIN/RM` run `/bin/rm` (measured: bash's `type Rm`).
fn is_rm_head(h: &str) -> bool {
    matches!(
        h.to_ascii_lowercase().as_str(),
        "rm" | "/bin/rm" | "/usr/bin/rm"
    )
}

impl Eval<'_> {
    fn command(&mut self, cmd: &Cmd) -> Result<(), String> {
        let n_assign = cmd
            .words
            .iter()
            .take_while(|w| assignment(w).is_some())
            .count();
        let head = cmd.words.get(n_assign).map(literal);
        if let Some(Some(h)) = &head {
            if KEYWORDS.contains(&h.as_str()) {
                return Err(format!("a compound command (`{h}`)"));
            }
            if VAR_BUILTINS.contains(&h.as_str()) {
                return Err(format!("`{h}` can change what a later rm means"));
            }
        }
        // Any `rm` a word or a substitution hands to another program.
        for (k, w) in cmd.words.iter().enumerate() {
            for p in w {
                let text = match p {
                    Part::Lit { text, .. } | Part::Subst(text) => text,
                    Part::Var { .. } => continue,
                };
                let head = k == n_assign && literal(w).as_deref().is_some_and(is_rm_head);
                if !head && mentions_rm(text) {
                    return Err(format!(
                        "an rm this resolver cannot see run (in `{}`)",
                        clip(text)
                    ));
                }
            }
        }
        if n_assign == cmd.words.len() {
            // An assignment-only command. It surely runs only when it starts
            // its `&&`/`||` list and is neither piped nor backgrounded.
            let definite = matches!(cmd.prev, Op::Start | Op::Semi | Op::Newline | Op::Amp)
                && !matches!(cmd.next, Op::Pipe | Op::Amp);
            for w in &cmd.words {
                let (name, value) = assignment(w).ok_or("an assignment")?;
                let val = if definite {
                    self.value(&value)
                } else {
                    Val::Unknown
                };
                self.env.insert(name, val);
            }
            return Ok(());
        }
        let rest = &cmd.words[n_assign..];
        let Some(Some(head)) = head else {
            return Err("a command named by an expansion".to_string());
        };
        if head == "printf"
            && rest[1..]
                .iter()
                .any(|w| literal(w).as_deref() == Some("-v"))
        {
            return Err("`printf -v` assigns a variable".to_string());
        }
        if head == "export" {
            return self.export(cmd, rest);
        }
        let prefixed: Vec<String> = cmd.words[..n_assign]
            .iter()
            .filter_map(|w| assignment(w).map(|(n, _)| n))
            .collect();
        if is_rm_head(&head) {
            if !prefixed.is_empty() {
                return Err("an assignment on the rm command itself".to_string());
            }
            return self.rm(cmd, &rest[1..]);
        }
        // `NAME=v cmd`: not seen by this command's own expansions, and not
        // known after it.
        for name in prefixed {
            self.env.insert(name, Val::Unknown);
        }
        Ok(())
    }

    /// `export NAME=value` is an assignment under the same rule; `export NAME`
    /// changes nothing this resolver reads; anything else escalates.
    fn export(&mut self, cmd: &Cmd, rest: &[Word]) -> Result<(), String> {
        let definite = matches!(cmd.prev, Op::Start | Op::Semi | Op::Newline | Op::Amp)
            && !matches!(cmd.next, Op::Pipe | Op::Amp);
        for w in &rest[1..] {
            if let Some((name, value)) = assignment(w) {
                let val = if definite {
                    self.value(&value)
                } else {
                    Val::Unknown
                };
                self.env.insert(name, val);
            } else if !literal(w).is_some_and(|l| valid_name(&l)) {
                return Err("an `export` this resolver does not follow".to_string());
            }
        }
        Ok(())
    }

    /// A variable's value as this line has it.
    fn lookup(&self, name: &str) -> Val {
        if let Some(v) = self.env.get(name) {
            return v.clone();
        }
        match name {
            "TMPDIR" => match self.scope.tmpdir {
                Some(t) => Val::Known {
                    text: t.to_string_lossy().trim_end_matches('/').to_string(),
                    maybe_empty: false,
                },
                None => Val::Unknown,
            },
            _ => Val::Unknown,
        }
    }

    /// An assignment's value: no word splitting or globbing, `~` unresolved.
    fn value(&self, parts: &Word) -> Val {
        let mut text = String::new();
        let mut maybe_empty = false;
        for (k, p) in parts.iter().enumerate() {
            match p {
                Part::Lit { text: t, quoted } => {
                    if k == 0 && !quoted && t.starts_with('~') {
                        return Val::Unknown;
                    }
                    text.push_str(t);
                }
                Part::Var { name, .. } => match self.lookup(name) {
                    Val::Known {
                        text: t,
                        maybe_empty: e,
                    } => {
                        text.push_str(&t);
                        maybe_empty |= e;
                    }
                    Val::Unknown => return Val::Unknown,
                },
                Part::Subst(s) => match mktemp_dir(s, self.scope.tmpdir) {
                    Some(t) => {
                        text.push_str(&t);
                        maybe_empty = true;
                    }
                    None => return Val::Unknown,
                },
            }
        }
        Val::Known { text, maybe_empty }
    }

    fn rm(&mut self, cmd: &Cmd, args: &[Word]) -> Result<(), String> {
        for (op, word, glued) in &cmd.redirs {
            let target = match (word, glued) {
                (_, Some(g)) => g.clone(),
                (Some(w), None) => literal(w).unwrap_or_default(),
                (None, None) => String::new(),
            };
            let harmless = op.ends_with('>')
                && (target == "/dev/null"
                    || target.strip_prefix('&').is_some_and(|fd| {
                        fd == "-" || (!fd.is_empty() && fd.bytes().all(|b| b.is_ascii_digit()))
                    }));
            if !harmless {
                return Err(format!("a `{op}` redirect on the rm"));
            }
        }
        let mut after_dd = false;
        let mut operands = 0usize;
        for w in args {
            let lit = literal(w);
            if !after_dd && let Some(l) = lit.as_deref() {
                if l == "--" {
                    after_dd = true;
                    continue;
                }
                if l.starts_with('-') && l != "-" {
                    if l == "--no-preserve-root" {
                        return Err("rm --no-preserve-root".to_string());
                    }
                    if !is_known_flag(l) {
                        return Err(format!("an rm flag this resolver does not know: {l}"));
                    }
                    continue;
                }
            }
            let (text, glob, maybe_empty) = self.operand(w)?;
            if !after_dd && text.starts_with('-') {
                return Err("an rm operand that expands to a flag".to_string());
            }
            let target = self.check(&text, glob, maybe_empty)?;
            self.targets.push(target);
            operands += 1;
        }
        if operands == 0 {
            return Err("an rm with no operand".to_string());
        }
        Ok(())
    }

    /// An operand's text after expansion, whether it globs, and whether a
    /// variable in it can be empty.
    fn operand(&self, word: &Word) -> Result<(String, bool, bool), String> {
        let mut text = String::new();
        let mut glob = false;
        let mut maybe_empty = false;
        for (k, p) in word.iter().enumerate() {
            match p {
                Part::Lit { text: t, quoted } => {
                    if !quoted {
                        if k == 0 && t.starts_with('~') {
                            return Err("~ in an rm operand".to_string());
                        }
                        if t.contains(['{', '}']) {
                            return Err("brace expansion in an rm operand".to_string());
                        }
                        glob |= has_glob(t);
                    } else {
                        // A quoted `*` is a literal name; judged as a glob
                        // anyway (a tie breaks toward escalating).
                        glob |= has_glob(t);
                    }
                    text.push_str(t);
                }
                Part::Var { name, quoted } => {
                    if name == "HOME" {
                        return Err("$HOME in an rm operand".to_string());
                    }
                    if !valid_name(name) {
                        return Err(format!("${name} (a positional or special parameter)"));
                    }
                    match self.lookup(name) {
                        Val::Known {
                            text: t,
                            maybe_empty: e,
                        } => {
                            if !quoted && (t.chars().any(char::is_whitespace) || has_glob(&t)) {
                                return Err(format!(
                                    "${name} is split or globbed unquoted (its value holds a space or a glob)"
                                ));
                            }
                            glob |= has_glob(&t);
                            maybe_empty |= e;
                            text.push_str(&t);
                        }
                        Val::Unknown if name == "PWD" => {
                            return Err("$PWD (the Bash tool's working directory is not \
                                        known to the supervisor)"
                                .to_string());
                        }
                        Val::Unknown => {
                            return Err(format!("${name} is not assigned a literal on this line"));
                        }
                    }
                }
                Part::Subst(_) => {
                    return Err("a command substitution in an rm operand".to_string());
                }
            }
        }
        Ok((text, glob, maybe_empty))
    }

    /// Where the operand points, and whether it may point there.
    fn check(&self, text: &str, glob: bool, maybe_empty: bool) -> Result<String, String> {
        if text.is_empty() {
            return Err("an empty rm operand".to_string());
        }
        if maybe_empty && glob {
            return Err(
                "a glob behind a $(mktemp) variable (were mktemp to fail, it is a glob at the root)"
                    .to_string(),
            );
        }
        if !text.starts_with('/') {
            return Err(format!(
                "a relative rm operand ({}; the Bash tool's working directory is not known \
                 to the supervisor)",
                clip(text)
            ));
        }
        let mut comps = Vec::new();
        for c in text.split('/') {
            match c {
                "" | "." => {}
                ".." => return Err("a .. component in an rm operand".to_string()),
                c if c.starts_with('.') && has_glob(c) => {
                    return Err("a glob that can match .. in an rm operand".to_string());
                }
                c => comps.push(c.to_string()),
            }
        }
        let shown = join(&comps).replace(MKTEMP_NAME, "<mktemp>");
        let home = self
            .scope
            .home
            .and_then(|h| abs_components(&h.to_string_lossy()));
        for pattern in &RmPolicy::default().deny_patterns {
            if let Some(name) =
                rm_policy::denied(pattern, &comps, &shown, text, None, home.as_deref())
            {
                return Err(format!("{shown}: {name}"));
            }
        }
        // ASCII-case-insensitively: the default macOS volume is, and a
        // differently-cased spelling of the cwd is the cwd.
        if comps.len() <= self.cwd.len()
            && comps
                .iter()
                .zip(&self.cwd)
                .all(|(a, b)| a.eq_ignore_ascii_case(b))
        {
            return Err(format!("{shown} is the session cwd or an ancestor of it"));
        }
        if self
            .scope
            .roots
            .iter()
            .any(|r| r.holds_strictly_unglobbed(&comps))
        {
            Ok(shown)
        } else {
            Err(format!("{shown} is not strictly inside a scratch root"))
        }
    }
}

/// `mktemp -d [-q] [-t PREFIX]` → `<tmpdir>/<mktemp>`; `mktemp -d [-q]
/// /abs/dir/name.XXXX` → `/abs/dir/<mktemp>`. Anything else (`-p DIR`, a
/// relative template, an expansion) is not resolved.
fn mktemp_dir(subst: &str, tmpdir: Option<&Path>) -> Option<String> {
    let words: Vec<&str> = subst.split_whitespace().collect();
    if words.first() != Some(&"mktemp") {
        return None;
    }
    let mut dir_flag = false;
    let mut template: Option<&str> = None;
    let mut k = 1;
    while let Some(w) = words.get(k).copied() {
        match w {
            "-d" | "--directory" => dir_flag = true,
            "-q" | "--quiet" => {}
            "-dq" | "-qd" => dir_flag = true,
            "-t" => {
                let prefix = words.get(k + 1)?;
                if prefix.contains(['/', '$']) {
                    return None;
                }
                k += 1;
            }
            w if !w.starts_with('-') && template.is_none() => template = Some(w),
            _ => return None,
        }
        k += 1;
    }
    if !dir_flag {
        return None;
    }
    match template {
        None => {
            let t = tmpdir?.to_string_lossy().trim_end_matches('/').to_string();
            abs_components(&t)?;
            Some(format!("{t}/{MKTEMP_NAME}"))
        }
        Some(t) => {
            if t.contains('$') || !t.ends_with("XXX") {
                return None;
            }
            let (dir, _) = t.rsplit_once('/')?;
            abs_components(dir)?;
            Some(format!("{dir}/{MKTEMP_NAME}"))
        }
    }
}

/// The components of an absolute path, `.` and empty ones dropped; `None` for
/// a relative path or one with a `..`.
pub(crate) fn abs_components(path: &str) -> Option<Vec<String>> {
    let rest = path.strip_prefix('/')?;
    let mut out = Vec::new();
    for c in rest.split('/') {
        match c {
            "" | "." => {}
            ".." => return None,
            c => out.push(c.to_string()),
        }
    }
    Some(out)
}

fn join(comps: &[String]) -> String {
    format!("/{}", comps.join("/"))
}

fn clip(s: &str) -> String {
    let t: String = s.chars().take(60).collect();
    if t.len() < s.len() {
        format!("{t}…")
    } else {
        t
    }
}

#[cfg(test)]
#[path = "rm_breaker_tests.rs"]
mod tests;
