// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Is the `rm` behind Claude Code's "Dangerous rm operation on possibly-empty variable path" box safe to wave through? The resolver the approval policy's `rm-breaker` rule asks ([`resolve_rm_line`]).

use std::collections::{HashMap, HashSet};
use std::path::Path;

use super::probe::{OnDisk, PolicyProbe};
use crate::supervise::classify::{GLOB_CHARS, TEST_BINARY, TEST_UNARY, glob_match, numeric_format};

/// The stand-in for the unique name `mktemp` makes: a component no deny rule
/// and no literal root can match, rendered as `<mktemp>` in a target.
const MKTEMP_NAME: &str = "\u{1}mktemp";

/// The value of a `$(mktemp -d …)` whose directory is made where the line
/// does not say — under the Bash tool's `$TMPDIR`, or relative to its
/// working directory — rendered `<mktemp -d>`.
const FRESH_DIR: &str = "\u{2}mktemp-d";

/// zsh's history modifiers (`zshexpn`, "Modifiers"): after `$NAME:` one of
/// these reshapes the value; any other character leaves `:` as text.
const ZSH_MODIFIERS: &str = "aAcefFghlpPqQrsStuwWx&";

/// A path as a reason or a target shows it: the `mktemp` stand-ins named.
fn rendered(path: &str) -> String {
    path.replace(MKTEMP_NAME, "<mktemp>")
        .replace(FRESH_DIR, "<mktemp -d>")
}

/// Names the shell itself sets, ties or reads (module docs): a line that
/// assigns one is not followed, whatever it assigns, since the value read
/// back is not the one written (`$_`, `PWD` after a `cd`, `SECONDS`, zsh's
/// read-only `status`) or the assignment changes what the rest of the line
/// runs or how it splits (`PATH`/`path`, `IFS`, `argv`).
const SHELL_NAMES: &[&str] = &[
    "_",
    "PWD",
    "OLDPWD",
    "HOME",
    "IFS",
    "PATH",
    "path",
    "CDPATH",
    "cdpath",
    "FPATH",
    "fpath",
    "MANPATH",
    "manpath",
    "MAILPATH",
    "mailpath",
    "MODULE_PATH",
    "module_path",
    "argv",
    "ARGC",
    "status",
    "pipestatus",
    "PIPESTATUS",
    "RANDOM",
    "SRANDOM",
    "SECONDS",
    "EPOCHSECONDS",
    "EPOCHREALTIME",
    "LINENO",
    "HISTCMD",
    "PPID",
    "UID",
    "EUID",
    "GID",
    "EGID",
    "USERNAME",
    "ERRNO",
    "TTYIDLE",
    "SHLVL",
    "REPLY",
    "reply",
    "MATCH",
    "match",
    "MBEGIN",
    "MEND",
    "mbegin",
    "mend",
    "OPTARG",
    "OPTIND",
    "GLOBIGNORE",
    "ENV",
    "ZDOTDIR",
    "SHELLOPTS",
    "BASHOPTS",
    "BASHPID",
    "GROUPS",
    "FUNCNAME",
    "DIRSTACK",
    "dirstack",
    "options",
    "parameters",
    "commands",
    "functions",
    "aliases",
    "builtins",
    "modules",
    "nameddirs",
    "userdirs",
    "psvar",
    "PSVAR",
    "histchars",
    "signals",
    "NULLCMD",
    "READNULLCMD",
    "POSIXLY_CORRECT",
    "PS4",
    "TMPPREFIX",
    "COLUMNS",
    "LINES",
    "FUNCNEST",
    "HISTCHARS",
    "HISTSIZE",
    "SAVEHIST",
    "KEYBOARD_HACK",
    "KEYTIMEOUT",
    "LISTMAX",
    "MAILCHECK",
    "TRY_BLOCK_ERROR",
    "TRY_BLOCK_INTERRUPT",
    "dis_aliases",
    "dis_builtins",
    "dis_functions",
    "dis_functions_source",
    "dis_galiases",
    "dis_patchars",
    "dis_reswords",
    "dis_saliases",
    "funcfiletrace",
    "funcsourcetrace",
    "funcstack",
    "functions_source",
    "functrace",
    "galiases",
    "history",
    "historywords",
    "jobdirs",
    "jobstates",
    "jobtexts",
    "keymaps",
    "patchars",
    "reswords",
    "saliases",
    "termcap",
    "terminfo",
    "usergroups",
    "widgets",
    "zsh_eval_context",
    "zsh_scheduled_events",
];

/// Whether the shell itself sets, ties or reads `name` ([`SHELL_NAMES`]).
fn shell_name(name: &str) -> bool {
    SHELL_NAMES.contains(&name)
        || ["BASH_", "ZSH_", "COMP_"]
            .iter()
            .any(|p| name.starts_with(p))
}

/// One component of a [`ScratchRoot`] pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RootComp {
    Lit(String),
    /// A `*`/`?` glob over one component (`target*`, `*`).
    Glob(String),
}

/// A directory pattern: the scratch roots an `rm` operand must sit strictly inside, and the trust roots a folder-trust dialog's path must sit in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScratchRoot {
    comps: Vec<RootComp>,
    label: String,
}

impl ScratchRoot {
    /// Exactly this absolute directory.
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

    /// The pattern as written (`/private/tmp/*`): how a policy probe's request on a harness tape names the root (`super::probe`), and what tests read to check a root's shape.
    pub(crate) fn label(&self) -> &str {
        &self.label
    }

    fn depth(&self) -> usize {
        self.comps.len()
    }

    /// This root where the disk leads it: its literal directories resolved
    /// (`probe`'s [`PolicyProbe::rm_physical`]; `/tmp/*` is `/private/tmp/*`
    /// on macOS), its glob kept.
    fn on_disk(
        &self,
        cwd: &[String],
        cwd_real: Option<&[String]>,
        probe: &dyn PolicyProbe,
    ) -> Option<Self> {
        let lits: Vec<String> = self
            .comps
            .iter()
            .map_while(|c| match c {
                RootComp::Lit(l) => Some(l.clone()),
                RootComp::Glob(_) => None,
            })
            .collect();
        let real = match cwd_real {
            Some(real) if lits == cwd && lits.len() < self.comps.len() => real.to_vec(),
            _ => {
                probe
                    .rm_physical(&lits)
                    .ok()
                    .filter(|p| !p.foreign_link)?
                    .comps
            }
        };
        let mut comps: Vec<RootComp> = real.into_iter().map(RootComp::Lit).collect();
        comps.extend(self.comps[lits.len()..].iter().cloned());
        Some(Self {
            comps,
            label: self.label.clone(),
        })
    }

    /// Whether `comps` begins with this pattern.
    fn prefixes(&self, comps: &[String]) -> bool {
        comps.len() >= self.comps.len()
            && self.comps.iter().zip(comps).all(|(p, c)| match p {
                RootComp::Lit(l) => l == c,
                RootComp::Glob(g) => glob_match(g, c),
            })
    }

    /// Whether the absolute path `comps` is this root or inside it (a trust root's test).
    pub fn holds(&self, comps: &[String]) -> bool {
        self.prefixes(comps)
    }

    /// Whether `comps` is strictly inside this root with no glob in any
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
    /// Where an operand may point.
    pub roots: &'a [ScratchRoot],
    /// How the disk is looked at: where a path's symbolic links lead ([`PolicyProbe::rm_physical`]) — the approval context's probe, so a harness tape holds each answer the rule judged by.
    pub probe: &'a dyn PolicyProbe,
}

/// Resolve every `rm` on `cmd`: `Ok` with each operand's resolved path when every one points strictly inside a scratch root (module docs), else `Err` naming the first thing that did not resolve or did not qualify.
pub fn resolve_rm_line(cmd: &str, scope: &RmScope<'_>) -> Result<Vec<String>, String> {
    let toks = lex(cmd)?;
    let unquoted = unquoted_names(&toks);
    let cmds = commands(toks)?;
    let cwd = abs_components(&scope.cwd.to_string_lossy())
        .ok_or_else(|| "the session cwd is not an absolute path".to_string())?;
    let mut ev = Eval {
        scope,
        disk: Disk::of(scope, &cwd),
        cwd,
        env: HashMap::new(),
        targets: Vec::new(),
        runs: 0,
        made: Vec::new(),
        prev: Flow::default(),
        unquoted,
    };
    for cmd in &cmds {
        ev.command(cmd)?;
    }
    if ev.targets.is_empty() {
        return Err("no rm at a command head on this line".to_string());
    }
    Ok(ev.targets)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Part {
    Lit {
        text: String,
        quoted: bool,
    },
    Var {
        name: String,
        quoted: bool,
        /// Written `${NAME:?…}`: the shell exits rather than expand it empty.
        nonempty: bool,
    },
    /// `$(…)`: its inner text, and whether it stands inside double quotes.
    Subst {
        text: String,
        quoted: bool,
    },
    /// An expansion zsh computes and this resolver does not (module docs):
    /// `$=V`, `$~V`, `$^V`, `$+V`, `$V:h`, a non-ASCII name.
    Opaque(String),
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

    /// The source from `start` to the cursor as one [`Part::Opaque`].
    fn opaque(&mut self, start: usize) {
        self.in_word = true;
        self.word
            .push(Part::Opaque(self.cs[start..self.i].iter().collect()));
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

    /// At a `$`: a variable, `${NAME}` / `${NAME:?…}`, `$(…)`, or a zsh form
    /// this resolver does not compute ([`Part::Opaque`]).
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
                    nonempty: rest.starts_with(":?"),
                });
            }
            Some('(') if self.peek(2) == Some('(') => {
                return Err("an arithmetic expansion".to_string());
            }
            Some('[') => {
                return Err("a `$[…]` arithmetic expansion (it can assign: `$[S=5]`)".to_string());
            }
            Some('=' | '~' | '^' | '+') => {
                let start = self.i;
                self.i += 1;
                while self
                    .peek(0)
                    .is_some_and(|d| matches!(d, '=' | '~' | '^' | '+'))
                {
                    self.i += 1;
                }
                while self
                    .peek(0)
                    .is_some_and(|d| d.is_ascii_alphanumeric() || d == '_')
                {
                    self.i += 1;
                }
                self.opaque(start);
            }
            Some(c) if !c.is_ascii() => {
                let start = self.i;
                self.i += 1;
                while self
                    .peek(0)
                    .is_some_and(|d| !d.is_ascii() || d.is_ascii_alphanumeric() || d == '_')
                {
                    self.i += 1;
                }
                self.opaque(start);
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
                self.word.push(Part::Subst {
                    text: self.cs[start..end].iter().collect(),
                    quoted,
                });
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
                match self.cs.get(start + len) {
                    Some('[') => {
                        return Err(
                            "a zsh subscript on a bare name (`$V[…]`; its arithmetic can \
                             assign)"
                                .to_string(),
                        );
                    }
                    Some(':')
                        if self
                            .cs
                            .get(start + len + 1)
                            .is_some_and(|m| ZSH_MODIFIERS.contains(*m)) =>
                    {
                        let from = self.i;
                        self.i = start + len + 2;
                        self.opaque(from);
                        return Ok(());
                    }
                    _ => {}
                }
                self.word.push(Part::Var {
                    name: self.cs[start..start + len].iter().collect(),
                    quoted,
                    nonempty: false,
                });
                self.i = start + len;
            }
            Some(c) if c.is_ascii_digit() || "@*#?$!-".contains(c) => {
                let mut k = self.i + 2;
                if c == '#' {
                    while self
                        .cs
                        .get(k)
                        .is_some_and(|d| d.is_ascii_alphanumeric() || *d == '_')
                    {
                        k += 1;
                    }
                }
                if self.cs.get(k) == Some(&'[') {
                    return Err(
                        "a zsh subscript on a special parameter (`$#[…]`; its arithmetic \
                         can assign)"
                            .to_string(),
                    );
                }
                self.word.push(Part::Var {
                    name: c.to_string(),
                    quoted,
                    nonempty: false,
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
    /// glued to a `&`.
    fn redirect(&mut self) -> Result<(), String> {
        let mut op = String::new();
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

struct Cmd {
    words: Vec<Word>,
    redirs: Vec<(String, Option<Word>, Option<String>)>,
    prev: Op,
    next: Op,
    /// Its `&&`/`||` list ends in `&`: bash runs the whole list in a
    /// background subshell, so an assignment in it is not the shell's after
    /// it (`S=/x && true & echo "$S"` prints nothing in bash, `/x` in zsh).
    in_async_list: bool,
}

fn commands(toks: Vec<Tok>) -> Result<Vec<Cmd>, String> {
    let mut out = Vec::new();
    let mut cur = Cmd {
        words: Vec::new(),
        redirs: Vec::new(),
        prev: Op::Start,
        next: Op::Newline,
        in_async_list: false,
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
                            in_async_list: false,
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
    let mut in_async_list = false;
    for c in out.iter_mut().rev() {
        match c.next {
            Op::Amp => in_async_list = true,
            Op::Semi | Op::Newline | Op::Start => in_async_list = false,
            Op::And | Op::Or | Op::Pipe => {}
        }
        c.in_async_list = in_async_list;
    }
    Ok(out)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Val {
    Known {
        text: String,
        /// The `$(mktemp -d)` runs (numbered in line order) whose failure
        /// would empty a part of this value (module header).
        runs: Vec<u32>,
        /// The value were every one of `runs` to fail.
        failed: String,
        /// Whether the value is one run's output and nothing else, so it is
        /// empty as a whole when that run fails: what `${V:?}` guards.
        whole: bool,
    },
    Unknown,
    /// Known once, then unknown after the command it names (its head),
    /// which may have assigned it ([`Eval::forget_named`]).
    Forgotten(String),
}

/// Shell words that open or close a compound command.
const KEYWORDS: &[&str] = &[
    "for", "while", "until", "select", "case", "esac", "if", "then", "elif", "else", "fi", "do",
    "done", "function", "{", "}", "!", "[[", "coproc", "time",
];

/// Builtins that can change a variable, the positional parameters, what a
/// name runs, how a later word expands, or the line's own text: zsh's
/// `integer`/`float` evaluate their values as arithmetic (`integer T=A` reads
/// `A`'s value, which can assign), and `setopt`, `emulate`, bash's `shopt`
/// and a module `zmodload` loads change how every later word reads.
const VAR_BUILTINS: &[&str] = &[
    "set",
    "integer",
    "float",
    "setopt",
    "unsetopt",
    "emulate",
    "shopt",
    "zmodload",
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

/// What the previous simple command left for the next one's `&&`/`||`/`|`.
#[derive(Debug, Clone, Default)]
struct Flow {
    /// The runs known made whenever it ran.
    made: Vec<u32>,
    /// The runs its success proves made: its only substitution a `$(mktemp
    /// -d)` it assigned, or `test -n "$D"` of one.
    implies: Vec<u32>,
    /// Whether it began its `&&`/`||` list.
    began_list: bool,
}

/// The disk's side of the checks ("On the disk" in the module docs): the
/// session cwd, the home directory and the scratch roots, each where it
/// leads.
struct Disk {
    cwd: Result<Vec<String>, String>,
    home: Option<Vec<String>>,
    roots: Vec<ScratchRoot>,
}

impl Disk {
    fn of(scope: &RmScope<'_>, cwd: &[String]) -> Self {
        let home = scope
            .home
            .and_then(|h| abs_components(&h.to_string_lossy()));
        let physical = |comps: &[String]| scope.probe.rm_physical(comps);
        let cwd_real = physical(cwd).map(|p| p.comps);
        let roots = scope
            .roots
            .iter()
            .filter_map(|r| r.on_disk(cwd, cwd_real.as_deref().ok(), scope.probe))
            .collect();
        Self {
            cwd: cwd_real,
            home: home.map(|h| physical(&h).map_or(h, |p: OnDisk| p.comps)),
            roots,
        }
    }
}

struct Eval<'a> {
    scope: &'a RmScope<'a>,
    cwd: Vec<String>,
    disk: Disk,
    env: HashMap<String, Val>,
    targets: Vec<String>,
    /// How many `$(mktemp -d)` runs the line has read: the next one's number.
    runs: u32,
    /// The runs known made from here on, whatever runs next: a `${D:?}` a
    /// command that surely ran expanded, and `D=$(…) || exit`.
    made: Vec<u32>,
    prev: Flow,
    /// Every name the line expands somewhere not inside double quotes (a bare
    /// `$D` or `${D}`, and any `$D` inside a `$(…)` or a zsh form), which
    /// [`Self::fresh_dir`] refuses ([`unquoted_names`]).
    unquoted: HashSet<String>,
}

/// The names `toks` expand outside double quotes ([`Eval::unquoted`]): a
/// [`Part::Var`] not quoted, and every `$NAME`, `${NAME` or zsh `$=NAME`-style
/// reference inside a [`Part::Subst`] or [`Part::Opaque`], whose quoting is
/// not read.
fn unquoted_names(toks: &[Tok]) -> HashSet<String> {
    let mut out = HashSet::new();
    let scan = |out: &mut HashSet<String>, text: &str| {
        let cs: Vec<char> = text.chars().collect();
        for (i, c) in cs.iter().enumerate() {
            if *c != '$' {
                continue;
            }
            let mut k = i + 1;
            while cs
                .get(k)
                .is_some_and(|d| matches!(d, '{' | '=' | '~' | '^' | '+' | '#'))
            {
                k += 1;
            }
            let name: String = cs[k..]
                .iter()
                .take_while(|d| d.is_ascii_alphanumeric() || **d == '_')
                .collect();
            if !name.is_empty() {
                out.insert(name);
            }
        }
    };
    for tok in toks {
        let Tok::Word(w) = tok else {
            continue;
        };
        for p in w {
            match p {
                Part::Var {
                    name,
                    quoted: false,
                    ..
                } => {
                    out.insert(name.clone());
                }
                Part::Subst { text: t, .. } | Part::Opaque(t) => scan(&mut out, t),
                Part::Var { .. } | Part::Lit { .. } => {}
            }
        }
    }
    out
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

/// An assignment [`assignment`] does not read, at a command's head: an
/// append (`S+=/..`) or a subscript (`S[0]=/etc`, which is `S=/etc` in bash;
/// `S[1,-1]=/etc`, which is `S=/etc` in zsh).
fn assignment_like(word: &Word) -> Option<String> {
    let Some(Part::Lit {
        text,
        quoted: false,
    }) = word.first()
    else {
        return None;
    };
    let name_len = text
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .count();
    let (name, rest) = text.split_at(name_len);
    (valid_name(name) && (rest.starts_with("+=") || rest.starts_with('['))).then(|| clip(text, 40))
}

/// Whether an unquoted piece `t` of an assignment's value, after `before`,
/// holds a `~` or `=` the shell expands there: at the value's start or after
/// a `:` (module docs).
fn value_expands(before: &str, t: &str) -> bool {
    let mut prev = before.chars().last();
    for c in t.chars() {
        if matches!(c, '~' | '=') && matches!(prev, None | Some(':')) {
            return true;
        }
        prev = Some(c);
    }
    false
}

/// Commands that take no word as a variable's name or as arithmetic,
/// whatever it holds: a double-quoted word the resolver cannot compute under
/// one of these names nothing ([`Eval::forget_named`]; zsh's `{S}>file`
/// redirect assigns `S` under a builtin one, and is read apart).
const PLAIN_CONSUMERS: &[&str] = &[
    "echo",
    "ls",
    "du",
    "cat",
    "date",
    "wc",
    "head",
    "tail",
    "file",
    "mkdir",
    "touch",
    "pwd",
    "true",
    "false",
    ":",
    "grep",
    "rg",
    "find",
    "sort",
    "uniq",
    "cut",
    "basename",
    "dirname",
    "readlink",
    "realpath",
    "diff",
    "cmp",
    "tree",
    "which",
    "git",
    "egrep",
    "fgrep",
    "mdfind",
    "sed",
    "awk",
    "tr",
    "jq",
    "comm",
    "nl",
    "md5",
    "shasum",
    "sha256sum",
    "column",
    "paste",
    "seq",
    "strings",
    "fold",
    "expr",
    "uname",
    "whoami",
    "id",
    "sw_vers",
    "hostname",
    "df",
    "uptime",
    "sysctl",
    "ps",
    "lsof",
    "pgrep",
    "sleep",
];

/// Commands zsh evaluates a word of as arithmetic, which assigns and reads a
/// variable's value as arithmetic in turn (zsh 5.9: `printf %d "S=5"`,
/// `print -f %d "S=5"`, `[ -t "1/(S=5)" ]`, `return S=5` each set `S`; `[ 1
/// -eq S=5 ]`, `printf %s S=5`, `kill`, `ulimit`, `umask`, `wait` do not).
const ARITH_HEADS: &[&str] = &["printf", "print", "test", "[", "return"];

/// How an [`ARITH_HEADS`] command reads its words (numbered from the
/// command's head, 0), `words` as they expand and `parts` as written.
#[derive(Debug, Default)]
struct Reading {
    /// The words it evaluates as arithmetic: `test`/`[`'s word after `-t`;
    /// `printf`'s arguments after its format (past `-v NAME` and `--`) unless
    /// the format says otherwise ([`numeric_format`]; zsh takes a `-` word
    /// other than `-v` as the format), and every word after a `-vNAME` word,
    /// which bash reads as `-v NAME`; every word of `print -f` (`printf`'s own
    /// reading) and of `return`.
    arith: Vec<usize>,
    /// The first word from which a word it does not evaluate names nothing:
    /// `printf`'s format, when every word before it is `-v NAME` or `--` and
    /// every word up to it stands as written ([`as_written`]), since only `-v
    /// NAME`, an option, assigns.
    mute_from: usize,
}

fn arith_words(head: &str, words: &[String], parts: &[Word]) -> Reading {
    let none = Reading {
        arith: Vec::new(),
        mute_from: words.len(),
    };
    if !ARITH_HEADS.contains(&head) {
        return none;
    }
    match head {
        "test" | "[" => Reading {
            arith: (2..words.len()).filter(|&k| words[k - 1] == "-t").collect(),
            ..none
        },
        "printf" => {
            let mut k = 1;
            while let Some(w) = words.get(k) {
                if w == "--" {
                    k += 1;
                    break;
                }
                if !w.starts_with('-') || w == "-" {
                    break;
                }
                if w.starts_with("-v") && w != "-v" {
                    return Reading {
                        arith: (k + 1..words.len()).collect(),
                        ..none
                    };
                }
                if w != "-v" {
                    // Any other `-` word is zsh's format (`printf -%d S=5`
                    break;
                }
                k += 2;
            }
            let args: Vec<usize> = (k + 1..words.len()).collect();
            if k >= words.len() || !(1..=k).all(|j| parts.get(j).is_some_and(as_written)) {
                return Reading {
                    arith: args,
                    ..none
                };
            }
            Reading {
                arith: if numeric_format(&words[k]) {
                    args
                } else {
                    Vec::new()
                },
                mute_from: k,
            }
        }
        "print"
            if words[1..]
                .iter()
                .take_while(|w| w.as_str() != "--")
                .any(|w| w.starts_with('-') && w.contains('f')) =>
        {
            Reading {
                arith: (1..words.len()).collect(),
                ..none
            }
        }
        "return" => Reading {
            arith: (1..words.len()).collect(),
            ..none
        },
        _ => none,
    }
}

/// The words `test`/`[` read as a string or a file operand (numbered from
/// the command's head, 0), by POSIX's rules for 1, 2 and 3 arguments, every
/// operator a literal: `[ X ]`, `[ ! X ]`, `[ -n X ]`, `[ X = Y ]`, `[ !
fn test_operands(head: &str, words: &[Word]) -> Vec<usize> {
    let args = match (head, words.get(1..).unwrap_or_default()) {
        ("test", args) => args,
        ("[", [args @ .., close]) if literal(close).as_deref() == Some("]") => args,
        _ => return Vec::new(),
    };
    if args
        .iter()
        .flatten()
        .any(|p| matches!(p, Part::Var { name, .. } if name == "@"))
    {
        return Vec::new();
    }
    let is = |k: usize, set: &[&str]| {
        args.get(k)
            .and_then(literal)
            .is_some_and(|t| t != "-t" && set.contains(&t.as_str()))
    };
    let at: &[usize] = match args.len() {
        1 => &[0],
        2 if is(0, &["!"]) || is(0, TEST_UNARY) => &[1],
        3 if is(1, TEST_BINARY) => &[0, 2],
        3 if is(0, &["!"]) && is(1, TEST_UNARY) => &[2],
        _ => &[],
    };
    at.iter().map(|k| k + 1).collect()
}

/// Whether the shell passes `word` on as exactly the one word it spells:
/// quoted text, quoted variables, and unquoted text with no glob, no brace,
/// no `~` (a leading one is the home directory, and `EXTENDED_GLOB` reads
/// any other) and no leading `=` (zsh's `=cmd`).
fn as_written(word: &Word) -> bool {
    word.iter().enumerate().all(|(k, p)| match p {
        Part::Lit { quoted: true, .. } | Part::Var { quoted: true, .. } => true,
        Part::Lit {
            text,
            quoted: false,
        } => {
            !has_glob(text)
                && !text.contains(['{', '}', '~'])
                && !(k == 0 && text.starts_with(['~', '=']))
        }
        Part::Var { .. } | Part::Subst { .. } | Part::Opaque(_) => false,
    })
}

/// Whether the shell may make other words of `word`: an unquoted `{`, which
/// brace-expands (`{%d,A}` is `%d A`, `-{vS,-}` is `-vS --`), or an unquoted
/// glob, which becomes the names in the working directory (`printf %d *`
/// with a file `S=5` there sets `S` in zsh, as does `[ ^-vS ]` under
/// `EXTENDED_GLOB` beside files `-t` and `S=5`).
fn expands_apart(word: &Word) -> bool {
    word.iter().enumerate().any(|(k, p)| match p {
        Part::Lit {
            text,
            quoted: false,
        } => {
            let tilde = text.chars().skip(usize::from(k == 0)).any(|c| c == '~');
            text.contains('{') || has_glob(text) || tilde
        }
        _ => false,
    })
}

/// A value's text with the `mktemp` stand-ins blanked: they name nothing.
fn unstand(text: &str) -> String {
    text.replace(MKTEMP_NAME, " ").replace(FRESH_DIR, " ")
}

/// Whether some word or substitution on a command carries an `rm` token
/// that is not the command's own head.
fn mentions_rm(text: &str) -> bool {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '/' | '-')))
        .map(str::to_ascii_lowercase)
        .any(|w| w == "rm" || w.ends_with("/rm"))
}

/// `rm` by any letter case: the default macOS volume is case-insensitive, so
/// `Rm` and `/BIN/RM` run `/bin/rm`.
fn is_rm_head(h: &str) -> bool {
    matches!(
        h.to_ascii_lowercase().as_str(),
        "rm" | "/bin/rm" | "/usr/bin/rm"
    )
}

/// `exit` or `exit <n>`, nothing else on the command.
fn is_exit(cmd: &Cmd) -> bool {
    let words: Vec<Option<String>> = cmd.words.iter().map(literal).collect();
    match words.as_slice() {
        [Some(h)] => h == "exit",
        [Some(h), Some(n)] => h == "exit" && !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()),
        _ => false,
    }
}

/// A known value unknown after the command `head` ([`Val::Forgotten`]); an
/// unknown one keeps its own reason.
fn forget(v: &mut Val, head: &str) {
    if matches!(v, Val::Known { .. }) {
        *v = Val::Forgotten(clip(head, 20));
    }
}

/// `a` with every member of `b` it lacks.
fn union(a: &[u32], b: &[u32]) -> Vec<u32> {
    let mut out = a.to_vec();
    out.extend(b.iter().filter(|r| !a.contains(r)));
    out
}

/// One `rm` operand, expanded.
struct Operand {
    text: String,
    glob: bool,
    /// Each `$(mktemp -d)` run the operand is built on, and the variable it
    /// came through.
    runs: Vec<(u32, String)>,
    /// The operand were those runs to fail.
    failed: String,
    /// It is one variable and nothing else, whose value is one run's output:
    /// `""` (or no word at all) when that run fails, which removes nothing.
    alone: bool,
}

impl Eval<'_> {
    /// One simple command, and what it leaves for the next ([`Flow`]).
    fn command(&mut self, cmd: &Cmd) -> Result<(), String> {
        let made = match cmd.prev {
            Op::Start | Op::Semi | Op::Newline | Op::Amp | Op::Or => Vec::new(),
            Op::And => union(&self.prev.made, &self.prev.implies),
            Op::Pipe => self.prev.made.clone(),
        };
        let began_list = matches!(cmd.prev, Op::Start | Op::Semi | Op::Newline | Op::Amp);
        let surely = began_list && cmd.next != Op::Pipe && !cmd.in_async_list;
        if cmd.prev == Op::Or
            && self.prev.began_list
            && matches!(cmd.next, Op::Semi | Op::Newline)
            && is_exit(cmd)
        {
            self.made = union(&self.made, &self.prev.implies);
        }
        let (implies, guarded) = self.simple(cmd, &made, surely)?;
        if surely {
            // Its `${D:?}` expanded, or the shell exited: that `D` was not
            self.made = union(&self.made, &guarded);
        }
        self.prev = Flow {
            made,
            implies,
            began_list,
        };
        Ok(())
    }

    /// The runs the `${V:?}` among `parts` guard, `V` as the line has it
    /// now: each `V` whose value is one run's output (`Val::Known::whole`).
    fn guarded_runs<'p>(&self, parts: impl Iterator<Item = &'p Part>) -> Vec<u32> {
        let mut out = Vec::new();
        for p in parts {
            if let Part::Var {
                name,
                nonempty: true,
                ..
            } = p
                && let Val::Known {
                    runs, whole: true, ..
                } = self.lookup(name)
            {
                out = union(&out, &runs);
            }
        }
        out
    }

    /// [`Self::command`]'s reading of the command itself: `Ok` with the runs
    /// its success proves made, and the runs its `${V:?}` words guard, each
    /// read where the shell expands it: an assignment's after the ones before
    /// it on the command, every other word before the command assigns
    /// anything (`E=${D:?} D=…` guards the old `D`; `export` expands all its
    /// words first).
    fn simple(
        &mut self,
        cmd: &Cmd,
        made: &[u32],
        surely: bool,
    ) -> Result<(Vec<u32>, Vec<u32>), String> {
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
        if let Some(form) = cmd.words.get(n_assign).and_then(assignment_like) {
            return Err(format!(
                "`{form}` assigns a variable in a way this resolver does not follow"
            ));
        }
        for (k, w) in cmd.words.iter().enumerate() {
            for p in w {
                let text = match p {
                    Part::Lit { text, .. } | Part::Subst { text, .. } => text,
                    Part::Var { .. } | Part::Opaque(_) => continue,
                };
                let head = k == n_assign && literal(w).as_deref().is_some_and(is_rm_head);
                if !head && mentions_rm(text) {
                    return Err(format!(
                        "an rm this resolver cannot see run (in `{}`)",
                        clip(text, 60)
                    ));
                }
            }
        }
        let names: Vec<String> = cmd.words[..n_assign]
            .iter()
            .filter_map(|w| assignment(w).map(|(n, _)| n))
            .collect();
        if let Some(name) = names.iter().find(|n| shell_name(n)) {
            return Err(format!(
                "{name} is assigned, a name the shell itself sets or reads"
            ));
        }
        if n_assign == cmd.words.len() {
            let substs = cmd
                .words
                .iter()
                .flatten()
                .filter(|p| matches!(p, Part::Subst { .. }))
                .count();
            let mut implies = Vec::new();
            let mut guarded = Vec::new();
            for w in &cmd.words {
                let (name, value) = assignment(w).ok_or("an assignment")?;
                guarded = union(&guarded, &self.guarded_runs(value.iter()));
                let first = self.runs;
                let val = if surely {
                    self.value(&value)
                } else {
                    Val::Unknown
                };
                if substs == 1 && self.runs > first {
                    implies.push(first);
                }
                self.env.insert(name, val);
            }
            return Ok((implies, guarded));
        }
        let guarded = if names.is_empty() {
            self.guarded_runs(cmd.words.iter().flatten())
        } else {
            Vec::new()
        };
        let rest = &cmd.words[n_assign..];
        let Some(Some(head)) = head else {
            return Err("a command named by an expansion".to_string());
        };
        if head == "export" {
            let prefixed = !names.is_empty();
            self.export(rest, surely && !prefixed)?;
            for name in names {
                self.env.insert(name, Val::Unknown);
            }
            return Ok((Vec::new(), guarded));
        }
        if is_rm_head(&head) {
            if !names.is_empty() {
                return Err("an assignment on the rm command itself".to_string());
            }
            self.rm(cmd, &rest[1..], &union(made, &guarded))?;
            return Ok((Vec::new(), guarded));
        }
        for name in names {
            self.env.insert(name, Val::Unknown);
        }
        let implies = self.test_implies(&head, &rest[1..]);
        self.forget_named(cmd, &head);
        Ok((implies, guarded))
    }

    /// What a command this resolver does not model may do to the line's
    /// variables: a builtin may assign a variable named in its words (`print
    /// -v S /etc`, `print -vS`, bash `printf -vS`, `zparseopts a:=S`, `zstyle -s
    /// c s S`, `zformat -f S /etc`, `stat -A S`) or evaluate a word as
    /// arithmetic, which assigns (`[ -t "1/(S=5)" ]`, zsh `printf %d "S=5"`), so
    /// every variable a word names, as the word expands, is unknown after it.
    fn forget_named(&mut self, cmd: &Cmd, head: &str) {
        for w in &cmd.words {
            if let [
                Part::Lit {
                    text,
                    quoted: false,
                },
            ] = w.as_slice()
                && let Some(name) = text.strip_prefix('{').and_then(|t| t.strip_suffix('}'))
                && let Some(v) = self.env.get_mut(name)
            {
                forget(v, head);
            }
        }
        let plain = PLAIN_CONSUMERS.contains(&head);
        let redirs = cmd.redirs.iter().filter_map(|(_, w, _)| w.as_ref());
        let mut words: Vec<String> = Vec::new();
        let mut unknown = false;
        let mut quoted_unknown: Vec<usize> = Vec::new();
        for (k, w) in cmd.words.iter().chain(redirs).enumerate() {
            let mut text = String::new();
            for p in w {
                match p {
                    Part::Lit { text: t, .. } => text.push_str(t),
                    Part::Var { name, quoted, .. } => match self.lookup(name) {
                        Val::Known { text: t, .. } => {
                            unknown |= !quoted && has_glob(&t);
                            text.push_str(&unstand(&t));
                        }
                        Val::Unknown | Val::Forgotten(_) if *quoted => {
                            if !plain {
                                quoted_unknown.push(k);
                            }
                        }
                        Val::Unknown | Val::Forgotten(_) => unknown = true,
                    },
                    Part::Subst { quoted: true, .. } => {
                        if !plain {
                            quoted_unknown.push(k);
                        }
                    }
                    Part::Subst { quoted: false, .. } | Part::Opaque(_) => unknown = true,
                }
            }
            words.push(text);
        }
        if unknown {
            self.forget_all(head);
            return;
        }
        if plain {
            return;
        }
        let at = cmd
            .words
            .iter()
            .take_while(|w| assignment(w).is_some())
            .count();
        let reading = arith_words(head, &words[at..cmd.words.len()], &cmd.words[at..]);
        let operands = test_operands(head, &cmd.words[at..]);
        let data = |k: usize| {
            k > at
                && k < cmd.words.len()
                && (k - at > reading.mute_from || operands.contains(&(k - at)))
                && !reading.arith.contains(&(k - at))
        };
        if !quoted_unknown.iter().all(|&k| data(k)) {
            self.forget_all(head);
            return;
        }
        let moved = (at + 1..cmd.words.len()).any(|k| {
            expands_apart(&cmd.words[k])
                && !(words[k].starts_with('/') && !words[k].contains(char::is_whitespace))
                && (k - at < reading.mute_from || reading.arith.contains(&(k - at)))
        });
        if moved {
            self.forget_all(head);
            return;
        }
        for (k, t) in words.iter().enumerate() {
            if k > at && k < cmd.words.len() && reading.arith.contains(&(k - at)) {
                if self.forget_arith(t, head) {
                    return;
                }
                continue;
            }
            if k > at && k < cmd.words.len() && k - at >= reading.mute_from {
                continue;
            }
            let cluster = t.len() > 1
                && t.starts_with('-')
                && t[1..].bytes().all(|b| b.is_ascii_alphabetic());
            let path = t.contains('/')
                && !t.contains(['=', '(', '['])
                && !t.contains("++")
                && !t.contains("--");
            if path {
                continue;
            }
            let names: Vec<&str> = if cluster {
                // What is glued after the first option letter: `-vS` → `S`,
                (2..t.len()).map(|k| &t[k..]).collect()
            } else {
                t.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                    .collect()
            };
            for name in names {
                if let Some(v) = self.env.get_mut(name) {
                    forget(v, head);
                }
            }
        }
    }

    /// Every variable unknown after the command `head`.
    fn forget_all(&mut self, head: &str) {
        for v in self.env.values_mut() {
            forget(v, head);
        }
    }

    /// A word zsh evaluates as arithmetic, `text` as it expands: every name
    /// it reads is unknown after it, and zsh reads a variable's value as
    /// arithmetic in turn (`A=S=5; printf %d A` sets `S`), so so is every name
    /// that value reads, and on.
    fn forget_arith(&mut self, text: &str, head: &str) -> bool {
        let mut todo = vec![text.to_string()];
        let mut seen = HashSet::new();
        while let Some(t) = todo.pop() {
            if t.starts_with('/') {
                continue;
            }
            for name in t.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
                if !valid_name(name) || !seen.insert(name.to_string()) {
                    continue;
                }
                match self.lookup(name) {
                    Val::Known { text: v, .. } => {
                        todo.push(unstand(&v));
                        if let Some(v) = self.env.get_mut(name) {
                            forget(v, head);
                        }
                    }
                    Val::Unknown | Val::Forgotten(_) => {
                        self.forget_all(head);
                        return true;
                    }
                }
            }
        }
        false
    }

    /// `test -n "$D"`, `test -d "$D"`, `test -e "$D"` and their `[ … ]` forms,
    /// `D` one run's output: their success proves the run made.
    fn test_implies(&self, head: &str, args: &[Word]) -> Vec<u32> {
        let args = match (head, args) {
            ("test", args) => args,
            ("[", [args @ .., close]) if literal(close).as_deref() == Some("]") => args,
            _ => return Vec::new(),
        };
        let [flag, word] = args else {
            return Vec::new();
        };
        if !matches!(literal(flag).as_deref(), Some("-n" | "-d" | "-e")) {
            return Vec::new();
        }
        let mut vars = word
            .iter()
            .filter(|p| !matches!(p, Part::Lit { text, .. } if text.is_empty()));
        match (vars.next(), vars.next()) {
            (
                Some(Part::Var {
                    name, quoted: true, ..
                }),
                None,
            ) => match self.lookup(name) {
                Val::Known {
                    runs, whole: true, ..
                } => runs,
                _ => Vec::new(),
            },
            _ => Vec::new(),
        }
    }

    /// `export NAME=value` is an assignment under the same rule; `export NAME`
    /// changes nothing this resolver reads; anything else escalates.
    fn export(&mut self, rest: &[Word], surely: bool) -> Result<(), String> {
        let mut bound = Vec::new();
        for w in &rest[1..] {
            if let Some((name, value)) = assignment(w) {
                if shell_name(&name) {
                    return Err(format!(
                        "{name} is assigned, a name the shell itself sets or reads"
                    ));
                }
                let val = if surely {
                    self.value(&value)
                } else {
                    Val::Unknown
                };
                bound.push((name, val));
            } else if !literal(w).is_some_and(|l| valid_name(&l)) {
                return Err("an `export` this resolver does not follow".to_string());
            }
        }
        self.env.extend(bound);
        Ok(())
    }

    /// A variable's value as this line assigned it; a name it did not
    /// assign is not known (module header, "Variables from the environment").
    fn lookup(&self, name: &str) -> Val {
        self.env.get(name).cloned().unwrap_or(Val::Unknown)
    }

    /// An assignment's value: no word splitting or globbing; a `~` or `=`
    /// the shell expands there unresolved ([`value_expands`]).
    fn value(&mut self, parts: &Word) -> Val {
        let mut text = String::new();
        let mut failed = String::new();
        let mut runs = Vec::new();
        let mut pieces = 0usize;
        let mut whole_piece = false;
        for p in parts {
            match p {
                Part::Lit { text: t, quoted } => {
                    if !quoted && value_expands(&text, t) {
                        return Val::Unknown;
                    }
                    text.push_str(t);
                    failed.push_str(t);
                    pieces += usize::from(!t.is_empty());
                }
                Part::Var { name, nonempty, .. } => match self.lookup(name) {
                    Val::Known {
                        text: t,
                        runs: r,
                        failed: f,
                        whole: w,
                    } => {
                        pieces += 1;
                        text.push_str(&t);
                        if *nonempty && w {
                            // `${D:?}`: the shell exits rather than go on
                            failed.push_str(&t);
                        } else {
                            failed.push_str(&f);
                            runs = union(&runs, &r);
                            whole_piece = w;
                        }
                    }
                    Val::Unknown | Val::Forgotten(_) => return Val::Unknown,
                },
                Part::Subst { text: s, .. } => match mktemp_dir(s) {
                    Some(t) => {
                        pieces += 1;
                        text.push_str(&t);
                        runs.push(self.runs);
                        self.runs += 1;
                        whole_piece = true;
                    }
                    None => return Val::Unknown,
                },
                Part::Opaque(_) => return Val::Unknown,
            }
        }
        Val::Known {
            text,
            whole: pieces == 1 && whole_piece && runs.len() == 1,
            runs,
            failed,
        }
    }

    fn rm(&mut self, cmd: &Cmd, args: &[Word], made: &[u32]) -> Result<(), String> {
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
        let made = union(made, &self.made);
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
            let fresh = self.fresh_dir(w, &made);
            if let Some(Ok(target)) = fresh {
                self.targets.push(target);
                operands += 1;
                continue;
            }
            let op = self.operand(w)?;
            if !after_dd && op.text.starts_with('-') {
                return Err("an rm operand that expands to a flag".to_string());
            }
            if let Some((_, var)) = op.runs.iter().find(|(r, _)| !made.contains(r))
                && !op.alone
            {
                let failed = if op.failed.is_empty() {
                    "empty".to_string()
                } else {
                    clip(&op.failed, 60)
                };
                let fix = format!(
                    "run the rm behind `&&` after the assignment, or write \"${{{var}:?}}\""
                );
                return Err(if op.glob {
                    format!(
                        "a glob behind a $(mktemp) variable (were mktemp to fail, ${var} is \
                         empty and the operand is {failed}); {fix}"
                    )
                } else {
                    format!(
                        "${var} is a `$(mktemp -d)` that can fail, leaving it empty, and the \
                         rm then removes {failed}; {fix}"
                    )
                });
            }
            if op.text.contains(FRESH_DIR) {
                return Err(match fresh {
                    Some(Err(why)) => why,
                    _ => format!(
                        "{}: a path built on a directory `mktemp -d` makes is proven only as \
                         \"$D\" or a path under it",
                        rendered(&op.text)
                    ),
                });
            }
            let target = self.check(&op.text)?;
            self.targets.push(target);
            operands += 1;
        }
        if operands == 0 {
            return Err("an rm with no operand".to_string());
        }
        Ok(())
    }

    /// The fresh-directory rule (module docs, "A directory the line itself
    /// just made"): `Some(Ok(target))` when `word` is `"$D"` or lies lexically
    /// under it, `D` exactly one `$(mktemp -d …)` run's output, that run
    /// proven made where the rm runs (`made`, or the operand's own `${D:?}`)
    /// unless the operand is `"$D"` alone, the operand quoted and, for a path
    /// under `$D`, every use of `$D` on the line double-quoted, and the rule
    /// not withheld ([`RmScope::roots`]); `Some(Err(why))` when `word` starts
    /// with such a `D` and one of those fails; `None` when it does not start
    /// with one.
    fn fresh_dir(&self, word: &Word, made: &[u32]) -> Option<Result<String, String>> {
        let (
            Part::Var {
                name,
                quoted,
                nonempty,
            },
            rest,
        ) = word.split_first()?
        else {
            return None;
        };
        let Val::Known {
            text,
            runs,
            whole: true,
            ..
        } = self.lookup(name)
        else {
            return None;
        };
        let run = *runs.first()?;
        let refused = |why: &str| {
            Some(Err(format!(
                "${name} is a directory `mktemp -d` makes, proven only as \"${name}\" or a path \
                 under it: {why}"
            )))
        };
        if self.scope.roots.is_empty() {
            return refused("the rm rule is withheld (no scratch root), so nothing is proven");
        }
        let unquoted = || {
            refused(&format!(
                "every use of ${name} on the line must be double-quoted (its directory's name \
                 may hold a space or a glob)"
            ))
        };
        if !quoted {
            return unquoted();
        }
        let mut under = String::new();
        for p in rest {
            let Part::Lit { text: t, quoted } = p else {
                return refused("another expansion follows it");
            };
            if has_glob(t) || (!quoted && t.contains(['{', '}'])) {
                return refused("a glob or a brace follows it");
            }
            under.push_str(t);
        }
        if !under.is_empty() && self.unquoted.contains(name.as_str()) {
            return unquoted();
        }
        if !(under.is_empty() || under.starts_with('/')) {
            return refused(
                "text glued to its name names a sibling of the directory, not a path in it",
            );
        }
        if under.split('/').any(|c| c == "..") {
            return refused("a .. under it climbs out of it");
        }
        if !under.is_empty() && !nonempty && !made.contains(&run) {
            return refused(&format!(
                "the rm may run with the `mktemp` failed and ${name} empty (run it behind `&&` \
                 after the assignment, or write \"${{{name}:?}}\")"
            ));
        }
        Some(Ok(rendered(&format!("{text}{under}"))))
    }

    /// An operand after expansion ([`Operand`]).
    fn operand(&self, word: &Word) -> Result<Operand, String> {
        let mut op = Operand {
            text: String::new(),
            glob: false,
            runs: Vec::new(),
            failed: String::new(),
            alone: false,
        };
        let mut pieces = 0usize;
        let mut whole_var = false;
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
                    }
                    op.glob |= has_glob(t);
                    op.text.push_str(t);
                    op.failed.push_str(t);
                    pieces += usize::from(!t.is_empty());
                }
                Part::Var {
                    name,
                    quoted,
                    nonempty,
                } => {
                    if name == "HOME" {
                        return Err("$HOME in an rm operand".to_string());
                    }
                    if !valid_name(name) || name == "_" {
                        return Err(format!("${name} (a positional or special parameter)"));
                    }
                    match self.lookup(name) {
                        Val::Known {
                            text: t,
                            runs,
                            failed,
                            whole,
                        } => {
                            if !quoted && (t.chars().any(char::is_whitespace) || has_glob(&t)) {
                                return Err(format!(
                                    "${name} is split or globbed unquoted (its value holds a space or a glob)"
                                ));
                            }
                            pieces += 1;
                            op.glob |= has_glob(&t);
                            op.text.push_str(&t);
                            if *nonempty && whole {
                                op.failed.push_str(&t);
                            } else {
                                op.failed.push_str(&failed);
                                op.runs.extend(runs.into_iter().map(|r| (r, name.clone())));
                                whole_var = whole;
                            }
                        }
                        Val::Forgotten(by) => {
                            return Err(format!(
                                "${name} is not known after `{by}`, which may assign it (it \
                                 names or evaluates it, or holds a word this resolver cannot \
                                 read)"
                            ));
                        }
                        Val::Unknown if name == "PWD" => {
                            return Err("$PWD (the Bash tool's working directory is not \
                                        known to the supervisor)"
                                .to_string());
                        }
                        Val::Unknown if self.env.contains_key(name) => {
                            return Err(format!("${name} is not assigned a literal on this line"));
                        }
                        Val::Unknown => {
                            return Err(format!(
                                "${name} is not assigned on this line (a value from the \
                                 environment is not proven)"
                            ));
                        }
                    }
                }
                Part::Subst { .. } => {
                    return Err("a command substitution in an rm operand".to_string());
                }
                Part::Opaque(src) => {
                    return Err(format!(
                        "`{}` in an rm operand, an expansion zsh computes that this resolver \
                         does not",
                        clip(src, 20)
                    ));
                }
            }
        }
        op.alone = pieces == 1 && whole_var;
        Ok(op)
    }

    /// Where the operand points, and whether it may point there: as written,
    /// then on the disk ([`Self::on_disk`]).
    fn check(&self, text: &str) -> Result<String, String> {
        if text.is_empty() {
            return Err("an empty rm operand".to_string());
        }
        if !text.starts_with('/') {
            return Err(format!(
                "a relative rm operand ({}; the Bash tool's working directory is not known \
                 to the supervisor)",
                clip(text, 60)
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
        let shown = rendered(&join(&comps));
        let home = self
            .scope
            .home
            .and_then(|h| abs_components(&h.to_string_lossy()));
        inside(&comps, &shown, &self.cwd, home.as_deref(), self.scope.roots)?;
        let follow_last = text
            .rsplit('/')
            .next()
            .is_some_and(|last| last.is_empty() || last == ".");
        self.on_disk(&comps, follow_last, &shown)?;
        Ok(shown)
    }

    /// The operand's directories resolved on the disk ([`PolicyProbe::rm_physical`]),
    /// and the path they lead to checked again, against the cwd, the home
    /// directory and the roots as the disk has them ([`Disk`]).
    fn on_disk(&self, comps: &[String], follow_last: bool, shown: &str) -> Result<(), String> {
        let dirs = if follow_last {
            comps.len()
        } else {
            comps.len().saturating_sub(1)
        };
        if let Some(c) = comps[..dirs].iter().find(|c| has_glob(c)) {
            return Err(format!(
                "{shown}: a glob in one of its directories ({}) can match a symbolic link",
                clip(c, 30)
            ));
        }
        let real = self
            .scope
            .probe
            .rm_physical(&comps[..dirs])
            .map_err(|e| format!("{shown}: {e}"))?;
        let mut real = real.comps;
        real.extend(comps[dirs..].iter().cloned());
        let led = if real == comps {
            format!("{shown} (on the disk)")
        } else {
            format!(
                "{shown} leads through a symbolic link to {}",
                rendered(&join(&real))
            )
        };
        let cwd = self
            .disk
            .cwd
            .as_deref()
            .map_err(|e| format!("{shown}: the session cwd on the disk: {e}"))?;
        inside(
            &real,
            &led,
            cwd,
            self.disk.home.as_deref(),
            &self.disk.roots,
        )
    }
}

/// Whether `comps` is past every [`critical_path`] (with `home`), is not
/// `cwd` or an ancestor of it, and is strictly inside one of `roots`;
/// `shown` names it in the reason.
fn inside(
    comps: &[String],
    shown: &str,
    cwd: &[String],
    home: Option<&[String]>,
    roots: &[ScratchRoot],
) -> Result<(), String> {
    if let Some(name) = critical_path(comps, home) {
        return Err(format!("{shown}: {name}"));
    }
    if comps.len() <= cwd.len()
        && comps
            .iter()
            .zip(cwd)
            .all(|(a, b)| a.eq_ignore_ascii_case(b))
    {
        return Err(format!("{shown} is the session cwd or an ancestor of it"));
    }
    if roots.iter().any(|r| r.holds_strictly_unglobbed(comps)) {
        Ok(())
    } else {
        Err(format!("{shown} is not strictly inside a scratch root"))
    }
}

/// `mktemp -d [-q] [-t PREFIX]` → [`FRESH_DIR`] (under the Bash tool's
/// `$TMPDIR`, which is never proven: module docs); `mktemp -d [-q]
/// /abs/dir/name.XXXX` → `/abs/dir/<mktemp>`; a relative template →
/// [`FRESH_DIR`] (under the Bash tool's working directory).
fn mktemp_dir(subst: &str) -> Option<String> {
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
        None => Some(FRESH_DIR.to_string()),
        Some(t) => {
            if t.contains(['$', '`', '~', '*', '?', '[']) || !t.ends_with("XXX") {
                return None;
            }
            match t.rsplit_once('/').filter(|_| t.starts_with('/')) {
                Some((dir, _)) => {
                    abs_components(dir)?;
                    Some(format!("{dir}/{MKTEMP_NAME}"))
                }
                None => Some(FRESH_DIR.to_string()),
            }
        }
    }
}

/// The components of an absolute path, `.` and empty ones dropped; `None` for a relative path or one with a `..`.
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

/// BSD and GNU `rm` flags that only change how the named operands are
/// removed.
fn is_known_flag(word: &str) -> bool {
    if let Some(long) = word.strip_prefix("--") {
        return matches!(
            long,
            "force"
                | "recursive"
                | "dir"
                | "verbose"
                | "one-file-system"
                | "preserve-root"
                | "preserve-root=all"
                | "interactive"
                | "interactive=never"
                | "interactive=once"
                | "interactive=always"
        );
    }
    match word.strip_prefix('-') {
        Some(cluster) if !cluster.is_empty() => cluster.chars().all(|c| "dfiIPrRvx".contains(c)),
        _ => false,
    }
}

/// Whether `s` holds a character by which zsh globs a word into more names
/// ([`GLOB_CHARS`]: `*`, `?`, `[`, and `EXTENDED_GLOB`'s `^`, `#` and `(`;
/// `rm -rf /tmp/w/^x` removes every name there but `x`).
fn has_glob(s: &str) -> bool {
    s.contains(GLOB_CHARS)
}

/// The paths no operand may name whatever root holds it, compared
/// ASCII-case-insensitively (as the default macOS volume is): `Some(its
/// name)` for the filesystem root, the home directory, an ancestor of it or a
/// glob directly inside it, `/Users` or `/home` or a direct child of either
/// (somebody's home), a glob in the first component (`/*`, `/u*/x`), and any
/// `.git` component.
fn critical_path(comps: &[String], home: Option<&[String]>) -> Option<&'static str> {
    let prefix_of = |a: &[String], b: &[String]| {
        a.len() <= b.len() && a.iter().zip(b).all(|(x, y)| x.eq_ignore_ascii_case(y))
    };
    if comps.is_empty() {
        Some("filesystem root")
    } else if home.is_some_and(|h| {
        prefix_of(comps, h)
            || (comps.len() == h.len() + 1
                && prefix_of(h, comps)
                && comps.last().is_some_and(|c| has_glob(c)))
    }) {
        Some("home")
    } else if comps.len() <= 2
        && (comps[0].eq_ignore_ascii_case("Users") || comps[0].eq_ignore_ascii_case("home"))
    {
        Some("/Users")
    } else if has_glob(&comps[0]) {
        Some("glob at root")
    } else if comps.iter().any(|c| c.eq_ignore_ascii_case(".git")) {
        Some("component .git")
    } else {
        None
    }
}

fn join(comps: &[String]) -> String {
    format!("/{}", comps.join("/"))
}

/// `s` cut to its first `n` characters, `…` where it was cut: the policy's one clip (an operand quoted in a reason, 60; a decline's reason, its quoted invocation and a refusal's label, [`super::approval`]).
pub(super) fn clip(s: &str, n: usize) -> String {
    let t: String = s.chars().take(n).collect();
    if t.len() < s.len() {
        format!("{t}…")
    } else {
        t
    }
}

#[cfg(test)]
#[path = "rm_breaker_tests.rs"]
mod tests;
