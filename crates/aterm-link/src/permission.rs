// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **The permission-request policy** — what aterm decides on the human's
//! behalf when Claude Code is about to put an approval box on the screen, and
//! what it refuses to decide.
//!
//! ## Why this exists (measured 2026-09-21, Claude Code 2.1.278, aterm 0.90.0)
//!
//! A session running with bypass permissions on — the human's explicit "do not
//! ask me" — still stops on one class of prompt: the vendor's critical-path
//! removal circuit breaker, which "cannot be auto-allowed by permission rules".
//! In the measured incident a workflow subagent ran
//! `S=…; for pair in …; do set -- $pair; rm -rf $S/$1; …` and the box
//! `Dangerous rm operation on possibly-empty variable path: $S/$1` sat under
//! the composer of a tab the human was not looking at, for as long as it took
//! them to notice. aterm's screen profile read that session as `phase=running`,
//! `level=quiet`, `attention=-`: nothing in the harness knew.
//!
//! The vendor documents the way out: in every mode that asks, a
//! `PermissionRequest` hook "can answer the prompt the way it answers any
//! other", and its `allow` may carry an `updatedInput`. So the hook does what
//! the box itself asks the human to do by hand — *"rewrite its `$S` as
//! `"${S:?}"`"* — and answers, but only when it can also SHOW that no value
//! the variables can take on this line reaches a critical path.
//!
//! ## The rule
//!
//! aterm DECIDES exactly one thing, and only in `bypassPermissions`: a `Bash`
//! command every one of whose `rm`/`rmdir` targets
//!
//! 1. is guardable — every `$NAME`, `${NAME}`, `$1` in it is rewritten
//!    `${NAME:?}`, so the shell stops with an error instead of removing from
//!    `/` when the variable is unset or empty (the hazard the box names), and
//! 2. RESOLVES — every variable in it has a finite set of values this check
//!    can read: same-line literal assignments (`S=/private/tmp/x`), `for NAME
//!    in <literal words>`, `set -- <literal words>` or `set -- $NAME` over
//!    such a loop (the incident's shape), `NAME=$(mktemp …)` (a fresh
//!    directory), the `cwd` the vendor reports for `$PWD`, and otherwise the
//!    value the hook's own environment holds (the vendor runs a hook with its
//!    own environment); and every combination of those values, rendered into
//!    the target, is a path outside the critical classes: not `/`, not a
//!    top-level directory, not the home directory, not the working directory
//!    or one of its parents — checked after `..`, `.`, `//`, a trailing glob
//!    and the symlinks on the path are resolved,
//!
//! is allowed with the guarded command as its input. The rewrite removes the
//! empty case; the resolution removes every other case the variables can
//! produce. Nothing about intent is judged.
//!
//! Everything else is ESCALATED and never decided: a literal critical path, a
//! variable bound on the line to one (`S=/usr; rm -rf "$S"`), a variable the
//! check cannot resolve (bound by `read`, `printf -v`, `${S:=…}`, a sourced
//! file, an array, a function; or unset everywhere it can look), a value that
//! would split or glob, a substitution or special parameter in a target, a
//! modifier other than `:?`, a relative literal target after a `cd`, any
//! glob other than a trailing `*`, a brace expansion, `--no-preserve-root`, a
//! removal run by another program (`xargs rm`, `find -delete`, `sh -c`,
//! `eval`, a quoted `rm`), a here-document, a command with no removal at all
//! (the prompt then has another cause — an ask rule, a read outside the
//! working directories, a cross-session safeguard), and any prompt in a mode
//! that asks by design (`default`, `acceptEdits`, `plan`), where the human
//! chose to review. Escalation is the typed `attention` meta the menu bar
//! badges and `ls`/`status` report, so the prompt is visible from every tab
//! and to every peer agent, and the box itself is left for the human (or a
//! supervising agent) exactly as before.
//!
//! The tie breaks toward NOT deciding. A guarded command that would still be
//! dangerous is a defect here; a command escalated that could have been
//! guarded is a prompt the human sees — the state the harness was in before.
//! An adversarial review on 2026-09-21 found five ways the first version of
//! this file allowed a command that removes a critical path (a same-line
//! binding, `$HOME//`, `~/../../usr`, `/usr/{,}`, a `\`-newline); each is a
//! test below.

use std::collections::BTreeMap;

/// What the hook decides for one permission request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Allow, with the command rewritten so no removal target can expand to
    /// the filesystem root. `rewrites` lists each `(from, to)` pair.
    Guarded {
        command: String,
        rewrites: Vec<(String, String)>,
    },
    /// No decision: leave the prompt to the human, and say why in a few words.
    Escalate { why: String },
}

/// The one permission mode in which aterm decides anything.
pub const DECIDING_MODE: &str = "bypassPermissions";

/// Decide one `PermissionRequest`.
///
/// `cwd` is the vendor's `cwd` field: the working directory the command runs
/// in, the value of `$PWD` before any `cd` on the line, and the directory a
/// relative target is joined to.
#[must_use]
pub fn decide(permission_mode: &str, tool_name: &str, command: &str, cwd: &str) -> Verdict {
    if permission_mode != DECIDING_MODE {
        return Verdict::Escalate {
            why: format!("the session asks by design ({permission_mode})"),
        };
    }
    if tool_name != "Bash" {
        return Verdict::Escalate {
            why: format!("a {tool_name} prompt is the human's"),
        };
    }
    match guard_removals(command, cwd) {
        Ok(Some(guarded)) => Verdict::Guarded {
            command: guarded.command,
            rewrites: guarded.rewrites,
        },
        Ok(None) => Verdict::Escalate {
            why: "no removal target to guard; the prompt has another cause".to_string(),
        },
        Err(why) => Verdict::Escalate {
            why: why.to_string(),
        },
    }
}

/// A command with its removal targets guarded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Guarded {
    pub command: String,
    pub rewrites: Vec<(String, String)>,
}

/// Guard every removal target in `command`, reading variables from this
/// process's environment where the line does not bind them.
///
/// `Ok(Some)` with the rewritten command when at least one expansion was
/// guarded and every target resolved outside the critical classes; `Ok(None)`
/// when there is nothing to guard (no removal, or none with a variable in it —
/// and so nothing to allow); `Err` naming what could not be shown safe.
///
/// # Errors
///
/// A short reason, in the words the attention meta carries.
pub fn guard_removals(command: &str, cwd: &str) -> Result<Option<Guarded>, &'static str> {
    guard_removals_in(command, cwd, &|name| std::env::var(name).ok())
}

/// [`guard_removals`] with the environment injected, so a test is
/// deterministic.
fn guard_removals_in(
    command: &str,
    cwd: &str,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<Option<Guarded>, &'static str> {
    let mut lx = Lexer {
        src: command.as_bytes(),
        at: 0,
        depth: 0,
        cmds: Vec::new(),
    };
    lx.list(None)?;
    let cmds = lx.cmds;
    refuse_hidden_removals(command, &cmds)?;
    let scope = Scope::collect(command, &cmds);
    let home = env("HOME");

    let mut saw_cd = false;
    let mut spans: Vec<(usize, usize, String)> = Vec::new();
    for cmd in &cmds {
        let Some(head_at) = simple_head(command, &cmd.words) else {
            continue;
        };
        let base = base_name(cmd.words[head_at].text(command));
        if matches!(base, "cd" | "pushd" | "popd") {
            saw_cd = true;
            continue;
        }
        if base != "rm" && base != "rmdir" {
            continue;
        }
        let mut after_dashdash = false;
        let mut skip_next = false;
        for w in &cmd.words[head_at + 1..] {
            let text = w.text(command);
            if skip_next {
                skip_next = false;
                continue;
            }
            if !after_dashdash {
                if text == "--" {
                    after_dashdash = true;
                    continue;
                }
                if text.starts_with('-') {
                    if text.contains("no-preserve-root") {
                        return Err("an rm that disables its own root protection");
                    }
                    continue;
                }
            }
            match redirection(text) {
                Redirection::Operator => {
                    skip_next = true;
                    continue;
                }
                Redirection::Attached => continue,
                Redirection::Plain => {}
            }
            if w.has_subst {
                return Err("a substitution inside a removal target");
            }
            let pieces = pieces(text)?;
            check_target(&pieces, &scope, cwd, saw_cd, home.as_deref(), env)?;
            for p in &pieces {
                if let Piece::Exp {
                    name,
                    start,
                    end,
                    tail,
                    ..
                } = p
                {
                    if tail.as_deref().is_none_or(str::is_empty) {
                        spans.push((w.start + start, w.start + end, format!("${{{name}:?}}")));
                    }
                }
            }
        }
    }
    if spans.is_empty() {
        return Ok(None);
    }
    spans.sort_by_key(|s| s.0);
    let mut out = String::with_capacity(command.len() + 8 * spans.len());
    let mut rewrites = Vec::new();
    let mut last = 0;
    for (start, end, to) in spans {
        if start < last {
            // Two spans overlapping is a lexer defect, not a shell; refuse
            // rather than emit a command nobody wrote.
            return Err("this check could not rewrite the command cleanly");
        }
        out.push_str(&command[last..start]);
        rewrites.push((command[start..end].to_string(), to.clone()));
        out.push_str(&to);
        last = end;
    }
    out.push_str(&command[last..]);
    Ok(Some(Guarded {
        command: out,
        rewrites,
    }))
}

/// The last path segment of a command word (`/bin/rm` → `rm`).
fn base_name(word: &str) -> &str {
    word.rsplit('/').next().unwrap_or(word)
}

/// Programs that run a STRING as shell code, which the vendor re-parses and
/// this check does not: a removal inside one could be the box's cause.
const STRING_SHELLS: &[&str] = &[
    "sh", "bash", "zsh", "dash", "ksh", "mksh", "fish", "csh", "tcsh",
];

/// Refuse a line with a removal this check cannot see into — one run by
/// another program or re-parsed from a string. The vendor's box may have been
/// raised for exactly that removal, so allowing the line on the strength of
/// the removals that ARE visible would answer a box nobody read.
///
/// # Errors
///
/// What was found.
fn refuse_hidden_removals(src: &str, cmds: &[Cmd]) -> Result<(), &'static str> {
    for cmd in cmds {
        let head_at = simple_head(src, &cmd.words);
        let words: Vec<&str> = cmd.words.iter().map(|w| w.text(src)).collect();
        if let Some(h) = head_at {
            let base = base_name(words[h]);
            if matches!(base, "eval" | "alias" | "function" | "trap" | "exec") {
                return Err("a command this check cannot see into (eval, alias, a function)");
            }
            if STRING_SHELLS.contains(&base)
                && words[h + 1..]
                    .iter()
                    .any(|a| a.starts_with('-') && !a.starts_with("--") && a.contains('c'))
            {
                return Err("a shell running a command string this check does not read");
            }
            if base == "find"
                && words[h + 1..]
                    .iter()
                    .any(|a| matches!(*a, "-delete" | "-exec" | "-execdir" | "-ok" | "-okdir"))
            {
                return Err("a removal find would run");
            }
        }
        for (k, w) in words.iter().enumerate() {
            if Some(k) == head_at {
                continue;
            }
            // `xargs rm`, `git rm`, `timeout 5s rm`, `parallel rm` — an `rm`
            // word that is not this command's head is run by something else.
            if matches!(base_name(w), "rm" | "rmdir") {
                return Err("a removal run by another program");
            }
            // A quoted `rm` — `ssh host 'rm -rf …'` — is a string some
            // program may run.
            if w.contains(['\'', '"']) {
                if let Ok(ps) = pieces(w) {
                    let text: String = ps
                        .iter()
                        .filter_map(|p| match p {
                            Piece::Lit { text, quoted: true } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join(" ");
                    if text
                        .split(|c: char| c.is_whitespace() || ";|&()".contains(c))
                        .any(|t| matches!(base_name(t), "rm" | "rmdir"))
                    {
                        return Err("a removal inside a quoted string");
                    }
                }
            }
        }
    }
    Ok(())
}

/// How a word in an `rm` argument list relates to redirection.
enum Redirection {
    /// A bare operator (`>`, `2>`, `&>>`): its target is the NEXT word.
    Operator,
    /// An operator with its target attached (`>/dev/null`, `2>&1`).
    Attached,
    /// Not a redirection.
    Plain,
}

fn redirection(word: &str) -> Redirection {
    let w = word.trim_start_matches(|c: char| c.is_ascii_digit());
    if matches!(
        w,
        ">" | ">>" | "<" | ">&" | "<&" | "&>" | "&>>" | ">|" | "<>"
    ) {
        Redirection::Operator
    } else if w.starts_with('>') || w.starts_with('<') || w.starts_with("&>") {
        Redirection::Attached
    } else {
        Redirection::Plain
    }
}

/// Words the shell reads before a simple command's head, and never as it.
const RESERVED: &[&str] = &[
    "{", "}", "!", "do", "then", "else", "elif", "fi", "done", "esac", "if", "while", "until",
    "time",
];

/// Wrappers whose FIRST non-option argument is the real head.
const WRAPPERS: &[&str] = &[
    "command",
    "nohup",
    "nice",
    "ionice",
    "sudo",
    "env",
    "builtin",
    "caffeinate",
    "noglob",
];

/// The index of the simple command's head word, past leading reserved words,
/// `NAME=value` assignments and wrapper programs (and their options, and an
/// option's bare numeric value). `None` for a command that is only those.
fn simple_head(src: &str, words: &[Word]) -> Option<usize> {
    let mut i = 0;
    loop {
        let w = words.get(i)?.text(src);
        if RESERVED.contains(&w) || assignment(w).is_some() {
            i += 1;
            continue;
        }
        if WRAPPERS.contains(&base_name(w)) {
            i += 1;
            while let Some(next) = words.get(i).map(|w| w.text(src)) {
                if next.starts_with('-')
                    || assignment(next).is_some()
                    || (!next.is_empty() && next.bytes().all(|b| b.is_ascii_digit()))
                {
                    i += 1;
                } else {
                    break;
                }
            }
            continue;
        }
        return Some(i);
    }
}

/// Whether `s` is a shell identifier.
fn is_name(s: &str) -> bool {
    let mut b = s.bytes();
    b.next()
        .is_some_and(|c| c == b'_' || c.is_ascii_alphabetic())
        && b.all(|c| c == b'_' || c.is_ascii_alphanumeric())
}

/// `NAME=value` → `(NAME, value)`, for a word that is an assignment.
fn assignment(word: &str) -> Option<(&str, &str)> {
    let eq = word.find('=')?;
    let name = &word[..eq];
    is_name(name).then(|| (name, &word[eq + 1..]))
}

// ---------------------------------------------------------------------------
// Pieces: a word, de-quoted, as literal text and parameter expansions
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum Piece {
    /// Literal text, quote-removed; `quoted` when it came from inside quotes
    /// or an escape (no glob, no split, no tilde).
    Lit { text: String, quoted: bool },
    /// `$NAME`, `${NAME}`, `${NAME…}`, `$1` — its byte span in the WORD.
    Exp {
        name: String,
        start: usize,
        end: usize,
        /// `None` for the bare form, `Some("")` for `${NAME}`, else the text
        /// after the name inside the braces (`:?`, `:-x`, …).
        tail: Option<String>,
        quoted: bool,
    },
}

fn push_lit(out: &mut Vec<Piece>, ch: &str, quoted: bool) {
    if let Some(Piece::Lit { text, quoted: q }) = out.last_mut() {
        if *q == quoted {
            text.push_str(ch);
            return;
        }
    }
    out.push(Piece::Lit {
        text: ch.to_string(),
        quoted,
    });
}

/// Read one word into [`Piece`]s the way the shell would before globbing:
/// quotes removed, escapes applied (a `\`-newline dropped, as the shell drops
/// it), parameter expansions found with their spans.
///
/// # Errors
///
/// A form this check does not read: an unterminated quote or expansion, a
/// substitution, `$'…'`/`$"…"`, a special parameter, an indirect `${!x}`, an
/// unquoted redirection or zsh `=cmd` inside the word.
fn pieces(word: &str) -> Result<Vec<Piece>, &'static str> {
    let b = word.as_bytes();
    let mut out: Vec<Piece> = Vec::new();
    let mut i = 0;
    let mut in_single = false;
    let mut in_double = false;
    let char_at = |i: usize| word[i..].chars().next().map_or(1, char::len_utf8);
    while i < b.len() {
        let c = b[i];
        if in_single {
            if c == b'\'' {
                in_single = false;
                i += 1;
            } else {
                let n = char_at(i);
                push_lit(&mut out, &word[i..i + n], true);
                i += n;
            }
            continue;
        }
        match c {
            b'\'' if !in_double => {
                in_single = true;
                i += 1;
            }
            b'"' => {
                in_double = !in_double;
                i += 1;
            }
            b'\\' => {
                let Some(&n) = b.get(i + 1) else {
                    i += 1;
                    continue;
                };
                if n == b'\n' {
                    // A line continuation: the shell deletes the pair.
                    i += 2;
                } else if in_double && !matches!(n, b'$' | b'`' | b'"' | b'\\') {
                    push_lit(&mut out, "\\", true);
                    i += 1;
                } else {
                    let len = char_at(i + 1);
                    push_lit(&mut out, &word[i + 1..i + 1 + len], true);
                    i += 1 + len;
                }
            }
            b'`' => return Err("a substitution inside a removal target"),
            b'$' => {
                let start = i;
                i += 1;
                let Some(&n) = b.get(i) else {
                    push_lit(&mut out, "$", in_double);
                    continue;
                };
                if n == b'{' {
                    let close = word[i..]
                        .find('}')
                        .ok_or("an unterminated parameter expansion")?;
                    let inner = &word[i + 1..i + close];
                    let name_len = inner
                        .bytes()
                        .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_')
                        .count();
                    let name = &inner[..name_len];
                    let numeric = !name.is_empty() && name.bytes().all(|b| b.is_ascii_digit());
                    if !(is_name(name) || numeric) {
                        return Err("a parameter expansion this check cannot guard");
                    }
                    let tail = inner[name_len..].to_string();
                    i += close + 1;
                    out.push(Piece::Exp {
                        name: name.to_string(),
                        start,
                        end: i,
                        tail: Some(tail),
                        quoted: in_double,
                    });
                } else if n.is_ascii_alphabetic() || n == b'_' {
                    let len = word[i..]
                        .bytes()
                        .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_')
                        .count();
                    let name = word[i..i + len].to_string();
                    i += len;
                    out.push(Piece::Exp {
                        name,
                        start,
                        end: i,
                        tail: None,
                        quoted: in_double,
                    });
                } else if n.is_ascii_digit() {
                    i += 1;
                    out.push(Piece::Exp {
                        name: (n as char).to_string(),
                        start,
                        end: i,
                        tail: None,
                        quoted: in_double,
                    });
                } else if n == b'(' {
                    return Err("a substitution inside a removal target");
                } else if n == b'\'' || n == b'"' {
                    return Err("a $'…' string this check does not read");
                } else {
                    // `$@ $* $# $? $$ $! $-`.
                    return Err("a special parameter in a removal target");
                }
            }
            b'<' | b'>' if !in_double => {
                return Err("a redirection inside a removal target");
            }
            _ => {
                let n = char_at(i);
                push_lit(&mut out, &word[i..i + n], in_double);
                i += n;
            }
        }
    }
    if in_single || in_double {
        return Err("an unterminated quote in the command");
    }
    if let Some(Piece::Lit {
        text,
        quoted: false,
    }) = out.first()
    {
        if text.starts_with('=') {
            return Err("a zsh =command expansion this check does not read");
        }
    }
    Ok(out)
}

/// The word's value when it has no expansion in it: quotes removed, escapes
/// applied. `None` when it has one (or cannot be read).
fn literal_word(word: &str) -> Option<String> {
    let ps = pieces(word).ok()?;
    let mut out = String::new();
    for p in ps {
        match p {
            Piece::Lit { text, .. } => out.push_str(&text),
            Piece::Exp { .. } => return None,
        }
    }
    Some(out)
}

/// Whether an assignment's value is a fresh temporary directory:
/// `$(mktemp …)` or `` `mktemp …` ``, optionally double-quoted, with nothing
/// else in the substitution. `mktemp` creates what it names, so removing it —
/// or anything under it — removes nothing that existed before.
fn is_fresh(value: &str) -> bool {
    let v = value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .unwrap_or(value);
    let inner = v
        .strip_prefix("$(")
        .and_then(|v| v.strip_suffix(')'))
        .or_else(|| v.strip_prefix('`').and_then(|v| v.strip_suffix('`')));
    inner.is_some_and(|inner| {
        let inner = inner.trim();
        (inner == "mktemp" || inner.starts_with("mktemp "))
            && !inner.contains(['$', '`', ';', '|', '&', '(', ')', '<', '>', '\n'])
    })
}

// ---------------------------------------------------------------------------
// The scope: what each name can be, on this line
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum Val {
    Lit(String),
    /// A directory `mktemp` created on this line.
    Fresh,
}

#[derive(Debug, Default)]
enum Positional {
    /// Never set on this line (the Bash tool's shell has none).
    #[default]
    Unset,
    /// Every `set --` list the line can reach.
    Known(Vec<Vec<String>>),
    /// Set by something this check does not read.
    Unknown,
}

#[derive(Debug, Default)]
struct Scope {
    /// Name → every value the line can bind it to; `None` = unknowable.
    binds: BTreeMap<String, Option<Vec<Val>>>,
    positional: Positional,
    /// A construct that can bind ANY name (`source`, `.`, a `declare` with
    /// options, `set -a`): nothing is resolved from the environment.
    opaque: bool,
}

impl Scope {
    fn unknown(&mut self, name: &str) {
        self.binds.insert(name.to_string(), None);
    }

    fn bind(&mut self, name: &str, val: Option<Val>) {
        match (self.binds.get_mut(name), val) {
            (Some(None), _) => {}
            (_, None) => self.unknown(name),
            (Some(Some(vals)), Some(v)) => {
                if !vals.contains(&v) {
                    vals.push(v);
                }
            }
            (None, Some(v)) => {
                self.binds.insert(name.to_string(), Some(vec![v]));
            }
        }
    }

    fn add_positional(&mut self, list: Option<Vec<String>>) {
        match (&mut self.positional, list) {
            (Positional::Unknown, _) => {}
            (p, None) => *p = Positional::Unknown,
            (Positional::Known(lists), Some(l)) => lists.push(l),
            (p @ Positional::Unset, Some(l)) => *p = Positional::Known(vec![l]),
        }
    }

    /// Everything the line binds, over-approximated: every binding counts,
    /// wherever and in whatever order it appears, and anything this check
    /// does not read makes its name unknowable.
    fn collect(src: &str, cmds: &[Cmd]) -> Self {
        let mut scope = Self::default();
        // `set -- $NAME` is resolved once every binding of NAME is known.
        let mut set_from: Vec<(String, bool)> = Vec::new();
        for cmd in cmds {
            let words: Vec<&str> = cmd.words.iter().map(|w| w.text(src)).collect();
            let head_at = simple_head(src, &cmd.words);
            let head = head_at.map(|h| base_name(words[h]));
            let declares = matches!(
                head,
                Some("export" | "declare" | "typeset" | "local" | "readonly")
            );
            let declare_opts = declares
                && words[head_at.unwrap_or(0) + 1..]
                    .iter()
                    .any(|w| w.starts_with('-') || w.starts_with('+'));
            let for_name = (head == Some("for")).then(|| head_at.unwrap_or(0) + 1);
            for (k, w) in words.iter().enumerate() {
                if let Some((name, value)) = assignment(w) {
                    if declare_opts {
                        scope.unknown(name);
                    } else if is_fresh(value) {
                        scope.bind(name, Some(Val::Fresh));
                    } else {
                        scope.bind(name, literal_word(value).map(Val::Lit));
                    }
                    continue;
                }
                // `NAME+=…`, `NAME[i]=…`.
                if let Some(pos) = w.find(['+', '[']) {
                    if is_name(&w[..pos]) && w[pos..].contains('=') {
                        scope.unknown(&w[..pos]);
                    }
                }
                // `${NAME:=…}` / `${NAME=…}` assign as they expand.
                let mut rest = *w;
                while let Some(at) = rest.find("${") {
                    rest = &rest[at + 2..];
                    let len = rest
                        .bytes()
                        .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_')
                        .count();
                    let after = &rest[len..];
                    if len > 0 && (after.starts_with(":=") || after.starts_with('=')) {
                        scope.unknown(&rest[..len]);
                    }
                }
                // A bare identifier anywhere but a head or a `for` name may
                // be bound by the program it is handed to (`read S`,
                // `printf -v S`, `getopts o S`, `unset S`).
                if is_name(w) && Some(k) != head_at && Some(k) != for_name {
                    scope.unknown(w);
                }
            }
            match head {
                Some("for") => {
                    let h = head_at.unwrap_or(0);
                    let Some(name) = words.get(h + 1).copied().filter(|n| is_name(n)) else {
                        continue;
                    };
                    if words.get(h + 2) == Some(&"in") {
                        for w in &words[h + 3..] {
                            scope.bind(name, literal_word(w).map(Val::Lit));
                        }
                    } else {
                        scope.unknown(name);
                    }
                }
                Some("set") => {
                    let args = &words[head_at.unwrap_or(0) + 1..];
                    if args.iter().any(|a| *a == "-a" || *a == "-o") {
                        scope.opaque = true;
                    }
                    let list: Option<&[&str]> =
                        if let Some(k) = args.iter().position(|a| *a == "--") {
                            Some(&args[k + 1..])
                        } else if args
                            .first()
                            .is_some_and(|a| !a.starts_with('-') && !a.starts_with('+'))
                        {
                            Some(args)
                        } else {
                            None
                        };
                    if let Some(list) = list {
                        let literal: Option<Vec<String>> =
                            list.iter().map(|w| literal_word(w)).collect();
                        if let Some(l) = literal {
                            scope.add_positional(Some(l));
                        } else if let [one] = list {
                            match pieces(one).ok().as_deref() {
                                Some(
                                    [Piece::Exp {
                                        name, tail, quoted, ..
                                    }],
                                ) if tail.as_deref().is_none_or(str::is_empty) => {
                                    set_from.push((name.clone(), *quoted));
                                }
                                _ => scope.add_positional(None),
                            }
                        } else {
                            scope.add_positional(None);
                        }
                    }
                }
                Some("shift") => scope.add_positional(None),
                Some("source" | ".") => scope.opaque = true,
                _ => {}
            }
            if declare_opts {
                scope.opaque = true;
            }
        }
        for (name, quoted) in set_from {
            match scope.binds.get(&name).cloned() {
                Some(Some(vals)) => {
                    for v in vals {
                        match v {
                            Val::Lit(s) if quoted => scope.add_positional(Some(vec![s])),
                            Val::Lit(s) => scope.add_positional(Some(
                                s.split_whitespace().map(str::to_string).collect(),
                            )),
                            Val::Fresh => scope.add_positional(None),
                        }
                    }
                }
                _ => scope.add_positional(None),
            }
        }
        scope
    }

    /// Every value `name` can take in a removal target: `None` is unset (the
    /// guard stops the command there). The environment is read only for a
    /// name the line does not make unknowable, and `$PWD` is the vendor's
    /// `cwd` until a `cd`.
    ///
    /// # Errors
    ///
    /// The name cannot be resolved.
    fn resolve(
        &self,
        name: &str,
        cwd: &str,
        saw_cd: bool,
        env: &dyn Fn(&str) -> Option<String>,
    ) -> Result<Vec<Option<Val>>, &'static str> {
        if name.bytes().all(|b| b.is_ascii_digit()) {
            let n: usize = name
                .parse()
                .map_err(|_| "a positional parameter this check cannot resolve")?;
            if n == 0 {
                return Err("a positional parameter this check cannot resolve");
            }
            return match &self.positional {
                Positional::Known(lists) => Ok(lists
                    .iter()
                    .map(|l| l.get(n - 1).cloned().map(Val::Lit))
                    .collect()),
                _ => Err("a positional parameter this check cannot resolve"),
            };
        }
        if self.opaque {
            return Err("a variable a sourced file or declaration may set");
        }
        let from_env = || -> Option<String> {
            if name == "PWD" {
                (!saw_cd && !cwd.is_empty()).then(|| cwd.to_string())
            } else if name == "OLDPWD" {
                None
            } else {
                env(name)
            }
        };
        match self.binds.get(name) {
            Some(None) => Err("a variable this check cannot resolve"),
            Some(Some(vals)) => {
                let mut out: Vec<Option<Val>> = vals.iter().cloned().map(Some).collect();
                // A prefix assignment does not reach its own command's
                // expansions, and an assignment may follow its use: the value
                // from before the line is possible too — or none at all.
                out.push(from_env().map(Val::Lit));
                Ok(out)
            }
            None => match from_env() {
                Some(v) => Ok(vec![Some(Val::Lit(v))]),
                None if name == "PWD" => Err("$PWD after a cd"),
                None => Err("a variable this check cannot resolve"),
            },
        }
    }
}

// ---------------------------------------------------------------------------
// One target: every value it can render to, outside the critical classes
// ---------------------------------------------------------------------------

/// Variables the vendor treats as system directories: as the WHOLE target
/// (alone, or with only a separator, glob, `.` or `..` after it) they are
/// refused even guarded, because `${HOME:?}` is still the home directory.
const SYSTEM_VARS: &[&str] = &[
    "HOME",
    "USERPROFILE",
    "HOMEPATH",
    "HOMEDRIVE",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_CACHE_HOME",
    "XDG_STATE_HOME",
    "APPDATA",
    "LOCALAPPDATA",
    "SYSTEMDRIVE",
    "SYSTEMROOT",
    "WINDIR",
    "PROGRAMFILES",
    "PROGRAMW6432",
    "PROGRAMDATA",
    "ALLUSERSPROFILE",
    "PUBLIC",
    "HOMESHARE",
    "XDG_RUNTIME_DIR",
    "PWD",
    "OLDPWD",
    "TMPDIR",
    "TMP",
    "TEMP",
];

/// More combinations than this is not a line anyone wrote by hand.
const MAX_COMBINATIONS: usize = 256;

/// Check one removal target: every expansion guardable, every combination of
/// the values its expansions can take rendered and shown outside the critical
/// classes.
///
/// # Errors
///
/// What made it unsafe or unreadable.
fn check_target(
    pieces: &[Piece],
    scope: &Scope,
    cwd: &str,
    saw_cd: bool,
    home: Option<&str>,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<(), &'static str> {
    if pieces.is_empty() {
        return Err("an empty removal target");
    }
    let tilde = matches!(
        pieces.first(),
        Some(Piece::Lit { text, quoted: false }) if text.starts_with('~')
    );
    // Each expansion's guard, the system-variable rule, and its values.
    let mut choices: Vec<Vec<Option<Val>>> = Vec::new();
    for (k, p) in pieces.iter().enumerate() {
        let Piece::Exp { name, tail, .. } = p else {
            continue;
        };
        match tail.as_deref() {
            None | Some("") => {}
            Some(t) if t.starts_with(":?") => {}
            Some(_) => return Err("a parameter expansion with a modifier this check cannot guard"),
        }
        if SYSTEM_VARS.contains(&name.as_str()) && k == 0 && names_the_whole(&pieces[1..]) {
            return Err("a home or system directory variable as the whole target");
        }
        choices.push(scope.resolve(name, cwd, saw_cd, env)?);
    }
    let total = choices
        .iter()
        .try_fold(1usize, |acc, c| acc.checked_mul(c.len().max(1)))
        .unwrap_or(usize::MAX);
    if total > MAX_COMBINATIONS {
        return Err("more values than this check will enumerate");
    }
    let mut index = vec![0usize; choices.len()];
    loop {
        render_and_check(pieces, &choices, &index, tilde, cwd, saw_cd, home)?;
        // Next combination.
        let mut k = 0;
        loop {
            if k == index.len() {
                return Ok(());
            }
            index[k] += 1;
            if index[k] < choices[k].len() {
                break;
            }
            index[k] = 0;
            k += 1;
        }
    }
}

/// Whether what follows a leading expansion leaves it naming the whole
/// target: nothing, separators, globs, `.`/`..`, or a first component that is
/// not a plain name.
fn names_the_whole(rest: &[Piece]) -> bool {
    let mut text = String::new();
    for p in rest {
        match p {
            Piece::Lit { text: t, .. } => text.push_str(t),
            Piece::Exp { .. } => return false,
        }
    }
    let comps: Vec<&str> = text.split('/').filter(|c| !c.is_empty()).collect();
    let first_is_name = comps
        .first()
        .is_some_and(|c| *c != "." && *c != ".." && !c.contains(['*', '?', '[', '{']));
    !first_is_name || comps.contains(&"..")
}

/// Render one combination and check it.
///
/// # Errors
///
/// What made the rendered path unsafe or unreadable.
fn render_and_check(
    pieces: &[Piece],
    choices: &[Vec<Option<Val>>],
    index: &[usize],
    tilde: bool,
    cwd: &str,
    saw_cd: bool,
    home: Option<&str>,
) -> Result<(), &'static str> {
    let mut path = String::new();
    let mut fresh_first = false;
    let mut e = 0;
    for (k, p) in pieces.iter().enumerate() {
        match p {
            Piece::Lit { text, .. } => path.push_str(text),
            Piece::Exp { quoted, .. } => {
                let val = choices[e].get(index[e]).cloned().flatten();
                e += 1;
                match val {
                    // Unset or empty: `${NAME:?}` stops the shell here, so
                    // this combination removes nothing.
                    None => return Ok(()),
                    Some(Val::Lit(v)) if v.is_empty() => return Ok(()),
                    Some(Val::Fresh) => {
                        if k != 0 {
                            return Err("a fresh temporary directory inside a longer path");
                        }
                        fresh_first = true;
                    }
                    Some(Val::Lit(v)) => {
                        if v.contains(['*', '?', '[', ']', '{', '}']) {
                            return Err("a variable whose value would glob");
                        }
                        if !quoted && v.chars().any(char::is_whitespace) {
                            return Err("a variable whose value would split into several targets");
                        }
                        if v.chars().any(char::is_control) {
                            return Err("a control character in a removal target");
                        }
                        path.push_str(&v);
                    }
                }
            }
        }
    }
    if fresh_first {
        // Under a directory mktemp just made: safe while nothing climbs out.
        glob_is_trailing(&path)?;
        if path.split('/').any(|c| c == "..") {
            return Err("a path that climbs out of a fresh temporary directory");
        }
        return Ok(());
    }
    literal_is_safe(&path, cwd, saw_cd, tilde, home)
}

/// Only a trailing `*` (the directory's contents) is read; every other
/// pattern character names a set of paths this check cannot compare.
///
/// # Errors
///
/// The pattern.
fn glob_is_trailing(path: &str) -> Result<(), &'static str> {
    if path.contains(['?', '[', ']', '{', '}']) {
        return Err("a glob or brace expansion this check does not read");
    }
    if let Some(i) = path.find('*') {
        let trailing = path[i..].bytes().all(|b| b == b'*');
        let at_segment_start = i == 0 || path[..i].ends_with('/');
        if !trailing || !at_segment_start {
            return Err("a glob this check does not read");
        }
    }
    Ok(())
}

/// Whether a rendered removal target is outside every critical class.
///
/// # Errors
///
/// The class it is in, or why it cannot be read.
fn literal_is_safe(
    path: &str,
    cwd: &str,
    after_cd: bool,
    tilde: bool,
    home: Option<&str>,
) -> Result<(), &'static str> {
    if path.is_empty() {
        return Err("an empty removal target");
    }
    if path.chars().any(char::is_control) {
        return Err("a control character in a removal target");
    }
    glob_is_trailing(path)?;
    let expanded;
    let path = if tilde {
        if path == "~" || path.starts_with("~/") {
            let home = home.ok_or("a home-relative target with no HOME known")?;
            expanded = format!("{home}{}", &path[1..]);
            expanded.as_str()
        } else {
            return Err("a home-relative target this check cannot read");
        }
    } else {
        path
    };
    let absolute = if path.starts_with('/') {
        path.to_string()
    } else {
        if after_cd {
            return Err("a relative target after a cd");
        }
        if cwd.is_empty() {
            return Err("a relative target with no working directory known");
        }
        format!("{}/{}", cwd.trim_end_matches('/'), path)
    };
    let norm = normalize(strip_glob(&absolute));
    check_absolute(&norm, cwd, home)?;
    // Through the symlinks on the path, as the removal will go.
    if let Some(canon) = canonical(&norm) {
        if canon != norm {
            check_absolute(&canon, cwd, home)?;
        }
    }
    Ok(())
}

/// `/*`, `/`, `*` and `/**` at the end of a path, removed.
fn strip_glob(path: &str) -> &str {
    let mut p = path;
    loop {
        let before = p;
        p = p.trim_end_matches('*');
        if p.len() > 1 {
            p = p.trim_end_matches('/');
        }
        if p == before {
            return if p.is_empty() { "/" } else { p };
        }
    }
}

/// A lexically normalized absolute path (`.` and `..` resolved, no trailing
/// slash, `/` for the root).
fn normalize(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    if parts.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", parts.join("/"))
    }
}

/// `path` with its longest existing ancestor resolved through the filesystem
/// (symlinks), the rest appended and normalized. `None` when nothing on it
/// can be resolved.
fn canonical(path: &str) -> Option<String> {
    let mut cur = std::path::Path::new(path);
    let mut rest: Vec<&std::ffi::OsStr> = Vec::new();
    for _ in 0..128 {
        if let Ok(c) = std::fs::canonicalize(cur) {
            let mut out = c;
            for seg in rest.iter().rev() {
                out.push(seg);
            }
            return Some(normalize(&out.to_string_lossy()));
        }
        rest.push(cur.file_name()?);
        cur = cur.parent()?;
    }
    None
}

/// # Errors
///
/// The root, a top-level directory, the home directory, the working directory
/// or a parent of it (as given and through its symlinks).
fn check_absolute(norm: &str, cwd: &str, home: Option<&str>) -> Result<(), &'static str> {
    if norm == "/" {
        return Err("the filesystem root");
    }
    if norm.matches('/').count() == 1 {
        return Err("a top-level directory");
    }
    if let Some(home) = home {
        let home = normalize(home);
        if home != "/" && norm == home {
            return Err("the home directory");
        }
    }
    if !cwd.is_empty() {
        let plain = normalize(cwd);
        let resolved = canonical(&plain);
        for c in std::iter::once(plain).chain(resolved) {
            if norm == c || c.starts_with(&format!("{norm}/")) {
                return Err("the working directory or one of its parents");
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The lexer: words with byte spans, grouped into simple commands
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Word {
    start: usize,
    end: usize,
    /// The word contains a `$(…)`, `<(…)`, `>(…)` or backtick group.
    has_subst: bool,
}

impl Word {
    fn text<'a>(&self, src: &'a str) -> &'a str {
        &src[self.start..self.end]
    }
}

#[derive(Debug, Default)]
struct Cmd {
    words: Vec<Word>,
}

struct Lexer<'a> {
    src: &'a [u8],
    at: usize,
    depth: usize,
    cmds: Vec<Cmd>,
}

/// Nesting deeper than this is not a command anyone typed.
const MAX_DEPTH: usize = 32;

impl Lexer<'_> {
    /// Read a command list up to `close` (or the end), pushing every simple
    /// command found — at this level and inside every group.
    fn list(&mut self, close: Option<u8>) -> Result<(), &'static str> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err("a command nested too deeply to read");
        }
        let mut cur = Cmd::default();
        loop {
            let Some(&c) = self.src.get(self.at) else {
                if close.is_some() {
                    return Err("an unterminated group in the command");
                }
                self.flush(&mut cur);
                self.depth -= 1;
                return Ok(());
            };
            match c {
                b' ' | b'\t' => self.at += 1,
                b'\n' | b';' | b'|' | b'&' => {
                    self.at += 1;
                    self.flush(&mut cur);
                }
                b')' => {
                    if close == Some(b')') {
                        self.at += 1;
                        self.flush(&mut cur);
                        self.depth -= 1;
                        return Ok(());
                    }
                    return Err("an unbalanced parenthesis in the command");
                }
                b'`' if close == Some(b'`') => {
                    self.at += 1;
                    self.flush(&mut cur);
                    self.depth -= 1;
                    return Ok(());
                }
                b'(' => {
                    // A `(` glued to a word is an array assignment, a
                    // function definition or a zsh glob qualifier — none of
                    // which this check reads. So is an empty `()`.
                    let glued = self
                        .at
                        .checked_sub(1)
                        .map(|i| self.src[i])
                        .is_some_and(|p| {
                            !matches!(p, b' ' | b'\t' | b'\n' | b';' | b'|' | b'&' | b'(')
                        });
                    if glued || self.src.get(self.at + 1) == Some(&b')') {
                        return Err("a parenthesis glued to a word (an array, a function)");
                    }
                    self.at += 1;
                    self.flush(&mut cur);
                    self.list(Some(b')'))?;
                }
                b'#' => {
                    // A comment to the end of the line: `#` here always starts
                    // a word.
                    while self.src.get(self.at).is_some_and(|&b| b != b'\n') {
                        self.at += 1;
                    }
                }
                // Everything else — a backtick, a `$(`, a `<(` included —
                // starts a word; [`Self::word`] reads the group inside it.
                _ => {
                    let w = self.word(close)?;
                    if self.src[w.start..w.end].starts_with(b"<<") {
                        return Err("a here-document this check does not read");
                    }
                    cur.words.push(w);
                }
            }
        }
    }

    fn flush(&mut self, cur: &mut Cmd) {
        if !cur.words.is_empty() {
            self.cmds.push(std::mem::take(cur));
        }
    }

    /// One word: through quotes, escapes and nested groups, to the next
    /// unquoted metacharacter. Inside a backtick group (`close` is the
    /// backtick) the closing backtick ends the word; it is the list's to
    /// consume.
    fn word(&mut self, close: Option<u8>) -> Result<Word, &'static str> {
        let start = self.at;
        let mut has_subst = false;
        let mut in_single = false;
        let mut in_double = false;
        while let Some(&c) = self.src.get(self.at) {
            if in_single {
                self.at += 1;
                if c == b'\'' {
                    in_single = false;
                }
                continue;
            }
            match c {
                b'\\' => self.at += 2,
                b'\'' if !in_double => {
                    in_single = true;
                    self.at += 1;
                }
                b'"' => {
                    in_double = !in_double;
                    self.at += 1;
                }
                b'$' if self.src.get(self.at + 1) == Some(&b'(') => {
                    self.at += 2;
                    has_subst = true;
                    self.list(Some(b')'))?;
                }
                b'`' if close == Some(b'`') && !in_double => break,
                b'`' => {
                    self.at += 1;
                    has_subst = true;
                    self.list(Some(b'`'))?;
                }
                b'<' | b'>' if !in_double && self.src.get(self.at + 1) == Some(&b'(') => {
                    self.at += 2;
                    has_subst = true;
                    self.list(Some(b')'))?;
                }
                b' ' | b'\t' | b'\n' | b';' | b'|' | b'&' | b'(' | b')' if !in_double => break,
                _ => self.at += 1,
            }
        }
        if in_single || in_double {
            return Err("an unterminated quote in the command");
        }
        let end = self.at.min(self.src.len());
        Ok(Word {
            start,
            end,
            has_subst,
        })
    }
}

// ---------------------------------------------------------------------------
// The words the escalation carries
// ---------------------------------------------------------------------------

/// The prefix of every attention aterm's hooks set — what the clear checks
/// before it unsets anything, so a human's or an operator's own attention is
/// never removed by a hook.
pub const ATTENTION_PREFIX: &str = "claude needs approval";

/// The attention meta's cap (`meta set attention`, 256 bytes), less room for
/// the endpoint's own encoding.
const ATTENTION_MAX: usize = 200;

/// The head of a command as a label: whitespace collapsed, printable ASCII
/// and spaces only, at most 72 characters. What the attention, the decision
/// log and an ask carry in place of the command.
#[must_use]
pub fn command_head(command: &str) -> String {
    command
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .filter(|c| c.is_ascii_graphic() || *c == ' ')
        .take(72)
        .collect()
}

/// The attention text for an escalated request: the prefix, the tool, the
/// command's head, and the reason — bounded, one line, printable ASCII and
/// spaces only (the meta value is a label, not a transcript).
#[must_use]
pub fn attention_text(tool_name: &str, command: &str, why: &str) -> String {
    let head = command_head(command);
    let mut text = format!("{ATTENTION_PREFIX}:");
    if !tool_name.is_empty() {
        text.push(' ');
        text.push_str(tool_name);
    }
    if !head.is_empty() {
        text.push(' ');
        text.push_str(&head);
    }
    if !why.is_empty() {
        text.push_str(" — ");
        text.push_str(why);
    }
    let mut out: String = text
        .chars()
        .map(|c| if c == '\u{2014}' { '-' } else { c })
        .filter(|c| c.is_ascii_graphic() || *c == ' ')
        .collect();
    while out.len() > ATTENTION_MAX {
        out.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A working directory and a home that exist on no machine, so the
    /// symlink pass has nothing to resolve and every verdict is the rule's.
    const CWD: &str = "/Users//nobody-aterm-test/proj";
    const HOME: &str = "/Users//nobody-aterm-test";

    fn env(name: &str) -> Option<String> {
        match name {
            "HOME" => Some(HOME.to_string()),
            "TMPDIR" => Some("/var/folders/zz/aterm-test/T/".to_string()),
            _ => None,
        }
    }

    fn run(cmd: &str) -> Result<Option<Guarded>, &'static str> {
        guard_removals_in(cmd, CWD, &env)
    }

    fn guarded(cmd: &str) -> String {
        match run(cmd) {
            Ok(Some(g)) => g.command,
            other => panic!("{cmd}: expected a guarded rewrite, got {other:?}"),
        }
    }

    fn refused(cmd: &str) -> &'static str {
        match run(cmd) {
            Err(why) => why,
            other => panic!("{cmd}: expected a refusal, got {other:?}"),
        }
    }

    /// The measured incident, in its full shape — a `cd` first, `S` bound
    /// on the line, `$1` bound by `set -- $pair` over a literal `for` list:
    /// the two variables of the removal are guarded, every value they can
    /// take resolves under the scratch dir, and nothing else in the line
    /// moves.
    #[test]
    fn the_incidents_command_is_guarded_in_place() {
        let cmd = "cd /Users//nobody-aterm-test/trust && S=/private/tmp/scratch/agent && grep -h edition <(git show origin/main:Cargo.toml) | sort | uniq -c; ED=$(git show origin/main:Cargo.toml | grep -m1 edition); ED=${ED:-2021}; for pair in \"t_mb 630604f8\" \"t_sv salvage/overlay-raw-20260721\" \"t_om origin/main\"; do set -- $pair; rm -rf $S/$1; mkdir -p $S/$1; git archive $2 | tar -x -C $S/$1; done; ls $S/t_sv | head";
        let g = run(cmd).expect("no refusal").expect("a rewrite");
        assert!(
            g.command.contains("rm -rf ${S:?}/${1:?}; mkdir -p $S/$1;"),
            "{}",
            g.command
        );
        assert_eq!(
            g.rewrites,
            vec![
                ("$S".to_string(), "${S:?}".to_string()),
                ("$1".to_string(), "${1:?}".to_string())
            ]
        );
        assert_eq!(
            g.command.replace("${S:?}/${1:?}", "$S/$1"),
            cmd,
            "only the removal target changed"
        );
    }

    /// Resolved forms that are guarded and allowed.
    #[test]
    fn a_target_every_value_of_which_is_safe_is_guarded() {
        assert_eq!(
            guarded("S=/tmp/z/w; rm -rf $S/*"),
            "S=/tmp/z/w; rm -rf ${S:?}/*"
        );
        assert_eq!(
            guarded("S=/tmp/z/w; rm -rf \"$S\"/*"),
            "S=/tmp/z/w; rm -rf \"${S:?}\"/*"
        );
        assert_eq!(
            guarded("S=/tmp/z/w; rm -rf ${S}/build"),
            "S=/tmp/z/w; rm -rf ${S:?}/build"
        );
        assert_eq!(
            guarded("a=/tmp/q; b=x; rmdir $a/$b"),
            "a=/tmp/q; b=x; rmdir ${a:?}/${b:?}"
        );
        assert_eq!(
            guarded("out=/tmp/o; rm -r -- $out/x $out/y"),
            "out=/tmp/o; rm -r -- ${out:?}/x ${out:?}/y"
        );
        // A relative value, joined to the working directory.
        assert_eq!(
            guarded("for d in build dist; do rm -rf $d; done"),
            "for d in build dist; do rm -rf ${d:?}; done"
        );
        // From the environment.
        assert_eq!(guarded("rm -rf $TMPDIR/foo"), "rm -rf ${TMPDIR:?}/foo");
        assert_eq!(
            guarded("rm -rf $HOME/.cache/x"),
            "rm -rf ${HOME:?}/.cache/x"
        );
        assert_eq!(guarded("rm -rf $PWD/build"), "rm -rf ${PWD:?}/build");
        // A fresh temporary directory.
        assert_eq!(
            guarded("T=$(mktemp -d); rm -rf \"$T\""),
            "T=$(mktemp -d); rm -rf \"${T:?}\""
        );
        assert_eq!(
            guarded("T=`mktemp -d`; rm -rf $T/*"),
            "T=`mktemp -d`; rm -rf ${T:?}/*"
        );
        // An empty value in a loop: the guard stops that iteration.
        assert_eq!(
            guarded("for d in \"\" build; do rm -rf $d/*; done"),
            "for d in \"\" build; do rm -rf ${d:?}/*; done"
        );
        // Already guarded: nothing to do.
        assert_eq!(run("S=/tmp/z/w; rm -rf ${S:?}/x /tmp/y/z"), Ok(None));
    }

    /// The review's first blocking finding: a variable bound on the SAME line
    /// to a critical path.
    #[test]
    fn a_variable_bound_on_the_line_to_a_critical_path_is_refused() {
        assert_eq!(refused("S=/usr && rm -rf \"$S\""), "a top-level directory");
        assert_eq!(
            refused("for d in / /usr /etc; do rm -rf $d; done"),
            "the filesystem root"
        );
        assert_eq!(refused("set -- /usr; rm -rf $1"), "a top-level directory");
        assert_eq!(
            refused("for p in \"/usr x\"; do set -- $p; rm -rf $1; done"),
            "a top-level directory"
        );
        assert_eq!(
            refused("arr=(/usr); rm -rf $arr"),
            "a parenthesis glued to a word (an array, a function)"
        );
        assert_eq!(
            refused("S=/; rm -rf --no-preserve-root \"$S\""),
            "an rm that disables its own root protection"
        );
        assert_eq!(
            refused("S=/tmp/ok; S=/Users//nobody-aterm-test; rm -rf $S"),
            "the home directory"
        );
        assert_eq!(
            refused("S=$HOME/x/..; rm -rf $S"),
            "a variable this check cannot resolve"
        );
    }

    /// The review's second: suffixes that turn `$HOME`/`$PWD` back into a
    /// critical path.
    #[test]
    fn a_system_variable_with_a_climbing_or_globbing_suffix_is_refused() {
        let whole = "a home or system directory variable as the whole target";
        for cmd in [
            "rm -rf $HOME",
            "rm -rf $HOME//",
            "rm -rf \"$TMPDIR\"/*",
            "rm -rf $HOME/{,}",
            "rm -rf $HOME/*/",
            "rm -rf $HOME/.*",
            "rm -rf $HOME/../*",
            "rm -rf $HOME/../../usr",
            "rm -rf \"$HOME\"/?*",
            "rm -rf $PWD/../proj",
            "rm -rf $PWD//",
            "rm -rf $TMPDIR/../../../../usr",
        ] {
            assert_eq!(refused(cmd), whole, "{cmd}");
        }
        assert_eq!(
            refused("rm -rf $HOME/proj/.."),
            whole,
            "a `..` anywhere climbs"
        );
        // Not the whole target: resolved, and then refused on what it is.
        assert_eq!(
            refused("rm -rf $HOME/proj"),
            "the working directory or one of its parents"
        );
    }

    /// The review's third and fourth: `~`, globs and braces in literal
    /// targets beside a guardable variable.
    #[test]
    fn a_literal_beside_a_guardable_variable_is_checked_in_full() {
        let s = "S=/tmp/ok;";
        assert_eq!(
            refused(&format!("{s} rm -rf $S/x ~/../../usr")),
            "a top-level directory"
        );
        assert_eq!(
            refused(&format!("{s} rm -rf $S/x ~/.")),
            "the home directory"
        );
        assert_eq!(
            refused(&format!("{s} rm -rf $S/x ~/{{,}}")),
            "a glob or brace expansion this check does not read"
        );
        assert_eq!(
            refused(&format!("{s} rm -rf $S/x ~/?*")),
            "a glob or brace expansion this check does not read"
        );
        assert_eq!(
            refused(&format!("{s} rm -rf $S/x /usr/{{,}}")),
            "a glob or brace expansion this check does not read"
        );
        assert_eq!(
            refused(&format!("{s} rm -rf $S/x /usr/[a-z]*")),
            "a glob or brace expansion this check does not read"
        );
        assert_eq!(
            refused(&format!("{s} rm -rf $S/x .[!.]*")),
            "a glob or brace expansion this check does not read"
        );
        assert_eq!(
            refused(&format!("{s} rm -rf $S/x */build")),
            "a glob this check does not read"
        );
        assert_eq!(
            refused(&format!("{s} rm -rf $S/x ~other/x")),
            "a home-relative target this check cannot read"
        );
        assert_eq!(
            guarded(&format!("{s} rm -rf $S/x ~/some/dir")),
            "S=/tmp/ok; rm -rf ${S:?}/x ~/some/dir"
        );
    }

    /// The review's fifth: a `\`-newline is joined the way the shell joins it.
    #[test]
    fn a_line_continuation_is_joined_before_the_target_is_read() {
        let s = "S=/tmp/ok;";
        assert_eq!(
            refused(&format!("{s} rm -rf $S/x \\\n/usr")),
            "a top-level directory"
        );
        assert_eq!(
            refused(&format!("{s} rm -rf $S/x \\\n~")),
            "the home directory"
        );
        assert_eq!(
            refused(&format!("{s} rm -rf $S/x \"\\\n/usr\"")),
            "a top-level directory"
        );
    }

    #[test]
    fn what_cannot_be_resolved_or_read_is_refused_by_name() {
        assert_eq!(
            refused("rm -rf $UNSET_ANYWHERE/x"),
            "a variable this check cannot resolve"
        );
        assert_eq!(
            refused("rm -rf $1"),
            "a positional parameter this check cannot resolve"
        );
        assert_eq!(
            refused("read S; rm -rf $S/x"),
            "a variable this check cannot resolve"
        );
        assert_eq!(
            refused("printf -v S /usr; rm -rf $S/x"),
            "a variable this check cannot resolve"
        );
        assert_eq!(
            refused(": ${S:=/usr}; rm -rf $S/x"),
            "a variable this check cannot resolve"
        );
        assert_eq!(
            refused("S=$(pwd); rm -rf $S/x"),
            "a variable this check cannot resolve"
        );
        assert_eq!(
            refused("source ./env.sh; rm -rf $TMPDIR/x"),
            "a variable a sourced file or declaration may set"
        );
        assert_eq!(
            refused("declare -n S=T; rm -rf $S/x"),
            "a variable a sourced file or declaration may set"
        );
        assert_eq!(
            refused("S=\"/tmp/a /usr\"; rm -rf $S"),
            "a variable whose value would split into several targets"
        );
        assert_eq!(
            refused("for f in *; do rm -rf $f; done"),
            "a variable whose value would glob"
        );
        assert_eq!(
            refused("T=$(mktemp -d); rm -rf $T/../x"),
            "a path that climbs out of a fresh temporary directory"
        );
        assert_eq!(
            refused("rm -rf $@"),
            "a special parameter in a removal target"
        );
        assert_eq!(
            refused("rm -rf $(cat list)"),
            "a substitution inside a removal target"
        );
        assert_eq!(
            refused("rm -rf `pwd`/x"),
            "a substitution inside a removal target"
        );
        assert_eq!(
            refused("S=/tmp/ok; rm -rf ${S:-/tmp}/x"),
            "a parameter expansion with a modifier this check cannot guard"
        );
        assert_eq!(
            refused("S=/tmp/ok; rm -rf ${!S}"),
            "a parameter expansion this check cannot guard"
        );
        assert_eq!(
            refused("cd /tmp/a && rm -rf build"),
            "a relative target after a cd"
        );
        assert_eq!(
            refused("S=build; cd /tmp/a && rm -rf $S"),
            "a relative target after a cd"
        );
        assert_eq!(refused("cd /tmp/a && rm -rf $PWD/x"), "$PWD after a cd");
        assert_eq!(
            refused("S=/tmp/ok; rm -rf 'unterminated"),
            "an unterminated quote in the command"
        );
        assert_eq!(
            refused("S=/tmp/ok; rm -rf $S/x /usr>/dev/null"),
            "a redirection inside a removal target"
        );
        assert_eq!(
            refused("f() { rm -rf $1; }; f /usr"),
            "a parenthesis glued to a word (an array, a function)"
        );
        assert_eq!(
            refused("f () { rm -rf $1; }; f /usr"),
            "a parenthesis glued to a word (an array, a function)"
        );
        assert_eq!(
            refused("cat <<EOF\nx\nEOF"),
            "a here-document this check does not read"
        );
    }

    /// A removal this check cannot see into refuses the whole line, whatever
    /// the visible removals are.
    #[test]
    fn a_hidden_removal_refuses_the_line() {
        let s = "S=/tmp/ok; rm -rf $S/x;";
        assert_eq!(
            refused(&format!("{s} find . -name x -print0 | xargs -0 rm -rf")),
            "a removal run by another program"
        );
        assert_eq!(
            refused(&format!("{s} git rm -r y")),
            "a removal run by another program"
        );
        assert_eq!(
            refused(&format!("{s} find / -delete")),
            "a removal find would run"
        );
        assert_eq!(
            refused(&format!("{s} sh -c 'rm -rf /usr'")),
            "a shell running a command string this check does not read"
        );
        assert_eq!(
            refused(&format!("{s} bash -lc true")),
            "a shell running a command string this check does not read"
        );
        assert_eq!(
            refused(&format!("{s} eval \"$CMD\"")),
            "a command this check cannot see into (eval, alias, a function)"
        );
        assert_eq!(
            refused(&format!("{s} ssh host 'rm -rf /usr'")),
            "a removal inside a quoted string"
        );
        assert_eq!(
            refused("echo 'rm -rf $x'"),
            "a removal inside a quoted string"
        );
    }

    #[test]
    fn what_is_refused_as_a_critical_literal() {
        let s = "S=/tmp/ok; rm -rf $S/x";
        assert_eq!(refused(&format!("{s} /")), "the filesystem root");
        assert_eq!(refused(&format!("{s} /*")), "the filesystem root");
        assert_eq!(refused(&format!("{s} /usr")), "a top-level directory");
        assert_eq!(refused(&format!("{s} /usr/")), "a top-level directory");
        assert_eq!(refused(&format!("{s} /etc/*")), "a top-level directory");
        assert_eq!(refused(&format!("{s} /a/b/../*")), "a top-level directory");
        assert_eq!(refused(&format!("{s} ~")), "the home directory");
        assert_eq!(refused(&format!("{s} ~/")), "the home directory");
        assert_eq!(refused(&format!("{s} ~/*")), "the home directory");
        for t in ["/Users//nobody-aterm-test/proj", ".", "./*", "../proj"] {
            assert_eq!(
                refused(&format!("{s} {t}")),
                "the working directory or one of its parents",
                "{t}"
            );
        }
    }

    /// A guardable variable beside a safe literal is guarded; beside a
    /// critical literal the whole line is refused — one bad target is enough.
    #[test]
    fn one_critical_target_refuses_the_line() {
        assert_eq!(
            guarded("S=/tmp/ok; rm -rf $S/x /tmp/y/z"),
            "S=/tmp/ok; rm -rf ${S:?}/x /tmp/y/z"
        );
        assert_eq!(
            refused("S=/tmp/ok; rm -rf $S/x /usr"),
            "a top-level directory"
        );
    }

    /// Inside substitutions, process substitutions and subshells the removal
    /// is found and resolved like any other.
    #[test]
    fn removals_inside_groups_are_found() {
        assert_eq!(
            guarded("S=/tmp/ok; echo \"$(rm -rf $S/x; echo done)\""),
            "S=/tmp/ok; echo \"$(rm -rf ${S:?}/x; echo done)\""
        );
        assert_eq!(
            guarded("S=/tmp/ok; (cd /tmp/a && ls) ; rm -rf $S/y"),
            "S=/tmp/ok; (cd /tmp/a && ls) ; rm -rf ${S:?}/y"
        );
        assert_eq!(
            guarded("S=/tmp/ok; diff <(ls $S/a) <(ls $S/b) && rm -rf $S/c"),
            "S=/tmp/ok; diff <(ls $S/a) <(ls $S/b) && rm -rf ${S:?}/c"
        );
    }

    /// Wrappers and assignments in front of the head are seen through; a
    /// redirection is never a target.
    #[test]
    fn wrappers_assignments_and_redirections() {
        assert_eq!(
            guarded("S=/tmp/ok; env X=1 nice -n 5 rm -rf $S/x"),
            "S=/tmp/ok; env X=1 nice -n 5 rm -rf ${S:?}/x"
        );
        assert_eq!(
            guarded("S=/tmp/ok; rm -rf $S/x 2>/dev/null >$S/log"),
            "S=/tmp/ok; rm -rf ${S:?}/x 2>/dev/null >$S/log"
        );
        assert_eq!(
            guarded("S=/tmp/ok; rm -rf $S/x > /usr/log"),
            "S=/tmp/ok; rm -rf ${S:?}/x > /usr/log"
        );
        assert_eq!(
            guarded("S=/tmp/ok; sudo rm -rf $S/x"),
            "S=/tmp/ok; sudo rm -rf ${S:?}/x"
        );
        assert_eq!(
            guarded("S=/tmp/ok; rm -rf $S/x # was /usr"),
            "S=/tmp/ok; rm -rf ${S:?}/x # was /usr"
        );
    }

    /// A command with no removal at all is not the hook's to decide.
    #[test]
    fn a_line_without_a_variable_removal_is_none() {
        assert_eq!(run("git push --force"), Ok(None));
        assert_eq!(run("rm -rf /tmp/a/b"), Ok(None));
    }

    /// The symlink pass: a path that resolves onto the working directory is
    /// refused even when its spelling does not name it.
    #[cfg(unix)]
    #[test]
    fn a_symlink_onto_the_working_directory_is_refused() {
        let root = std::env::temp_dir().join(format!("aterm-perm-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let real = root.join("real");
        std::fs::create_dir_all(real.join("sub")).expect("scratch");
        let link = root.join("link");
        std::os::unix::fs::symlink(&real, &link).expect("symlink");
        let cwd = std::fs::canonicalize(&real).expect("canonical");
        let cmd = format!("S={}; rm -rf $S", link.display());
        assert_eq!(
            guard_removals_in(&cmd, &cwd.display().to_string(), &env),
            Err("the working directory or one of its parents")
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn decide_only_in_bypass_and_only_for_bash() {
        match decide("default", "Bash", "rm -rf $S/x", CWD) {
            Verdict::Escalate { why } => assert!(why.contains("asks by design"), "{why}"),
            other => panic!("{other:?}"),
        }
        match decide("bypassPermissions", "Edit", "", CWD) {
            Verdict::Escalate { why } => assert!(why.contains("Edit"), "{why}"),
            other => panic!("{other:?}"),
        }
        match decide("bypassPermissions", "Bash", "S=/tmp/ok; rm -rf $S/x", CWD) {
            Verdict::Guarded { command, rewrites } => {
                assert_eq!(command, "S=/tmp/ok; rm -rf ${S:?}/x");
                assert_eq!(rewrites.len(), 1);
            }
            other => panic!("{other:?}"),
        }
        match decide("bypassPermissions", "Bash", "git push", CWD) {
            Verdict::Escalate { why } => assert!(why.contains("another cause"), "{why}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn attention_text_is_bounded_and_printable() {
        let text = attention_text("Bash", "rm -rf   $S/$1\n\tmkdir x", "the filesystem root");
        assert_eq!(
            text,
            "claude needs approval: Bash rm -rf $S/$1 mkdir x - the filesystem root"
        );
        let long = attention_text("Bash", &"a".repeat(500), &"b".repeat(500));
        assert!(long.len() <= ATTENTION_MAX, "{}", long.len());
        assert!(long.starts_with(ATTENTION_PREFIX));
        let ctl = attention_text("Bash", "rm \u{1b}[31m$S", "");
        assert!(!ctl.contains('\u{1b}'));
    }

    #[test]
    fn normalize_resolves_dots() {
        assert_eq!(normalize("/a/b/../c/./d/"), "/a/c/d");
        assert_eq!(normalize("/../.."), "/");
        assert_eq!(strip_glob("/a/b/*"), "/a/b");
        assert_eq!(strip_glob("/a/b/"), "/a/b");
        assert_eq!(strip_glob("/*"), "/");
        assert_eq!(strip_glob("/"), "/");
    }
}
