// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Is this shell command line READ-ONLY? The one judgment a supervisor makes
//! hundreds of times a session, so it lives here as a pure function with the
//! corpus that shaped it pinned as tests. Every rule below was needed against a
//! real Claude Code worker. A pre-pass ([`prelex`]) first reads the line the
//! way the shell does where the segment readers would not: a `#` comment ends
//! at its newline, a here-document body is data up to its terminator, `$'…'`
//! and `${…}` are followed or refused, and an unterminated quote is refused.
//! Then the segment-head check is a POSITIVE filter (an unknown program is not
//! read-only, and a program named by a path outside `/bin` and `/usr/bin` is
//! unknown), and quoted strings are dropped BEFORE it runs so a `>` inside a
//! `git --format` string is not a redirect — except that the PROGRAMS a line
//! hands to awk and sed are read back from the raw words, since `system(…)`
//! and `s///w file` live inside those quotes. A program word is judged only
//! where it runs, at a segment head: `grep -rn open src` is a read, `open
//! src` is not. A wrapper (`xargs`, `env`, `timeout`) is seen through to the
//! command it runs, and `&` ends a segment like `;`, so a backgrounded command
//! is a head too. The flags by which a read tool runs a program or writes a
//! file (`git -c`, `git --output`, `rg --pre`, `sort --compress-program`,
//! `printf -v`, `less +…`, `date -s`) and the assignments that change what a
//! later program is (`PATH=`, `GIT_*=`, `HOME=`) are refused wherever they sit.
//!
//! This judges ONE reading of a line. A screen-read command whose rows were
//! joined with spaces is a different line from the one the shell runs (a
//! newline ends a segment, a space does not), so a caller reading a box
//! classifies BOTH readings and approves only when both are read-only
//! (`supervise::policy::approval`). False negatives (a read handed to the
//! manager) cost a human a glance; a false positive would auto-approve a write,
//! so every tie breaks toward "not read-only".

/// The verdict on one command line: `read_only`, and `reason` naming the rule
/// that decided it (the token or segment head), so a supervisor's notes say WHY
/// a command was handed to the manager.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    /// `true` when every segment is a known read-only program and no danger
    /// token appears anywhere.
    pub read_only: bool,
    /// The rule that decided it: `"every segment read-only"`, or the offending
    /// token / segment head (`"rm"`, `"git push"`, `"redirect > b.txt"`).
    pub reason: String,
}

impl Verdict {
    fn no(reason: impl Into<String>) -> Self {
        Self {
            read_only: false,
            reason: reason.into(),
        }
    }
}

/// The python scripts a worker may run as reads when no `--allow-python` glob
/// is given: none. A script is a program of the worker's choosing, and a name
/// is no evidence of what it does (`scripts/purge_report.py --delete-all`
/// matched the owner's campaign globs that used to sit here). A project that
/// wants its generators approved names them with `--allow-python`, e.g.
/// `scripts/*standing*.py`.
pub const DEFAULT_PYTHON_ALLOW: &[&str] = &[];

/// Programs a segment may START with and still be a read. Shell keywords are
/// here because a `for`/`if` segment's head is the keyword; the real command
/// after a `do`/`then` is checked too (see [`segment_head`]). `let` and
/// `local` are not: both evaluate arithmetic (`local -i`), and the shell's
/// arithmetic expands an array subscript, so `let 'x=a[$(touch M)]'` runs
/// the `touch` from inside a quote this reader treats as opaque (measured
/// under zsh and bash 3.2, lane B's review of 2026-09-23).
const READ_ONLY: &[&str] = &[
    "git",
    "ls",
    "cat",
    "head",
    "tail",
    "grep",
    "egrep",
    "fgrep",
    "rg",
    "find",
    "mdfind",
    "wc",
    "sed",
    "awk",
    "sort",
    "uniq",
    "cut",
    "tr",
    "echo",
    "printf",
    "stat",
    "file",
    "which",
    "type",
    "df",
    "du",
    "uptime",
    "ps",
    "hostname",
    "sysctl",
    "date",
    "pwd",
    "true",
    "test",
    "[",
    "[[",
    "for",
    "do",
    "done",
    "if",
    "then",
    "else",
    "elif",
    "fi",
    "while",
    "jq",
    "diff",
    "comm",
    "basename",
    "dirname",
    "realpath",
    "readlink",
    "xargs",
    "env",
    "nl",
    "less",
    "more",
    "tree",
    "md5",
    "shasum",
    "sha256sum",
    "column",
    "paste",
    "seq",
    "expr",
    "export",
    "cd",
    "tmutil",
    "pgrep",
    "sleep",
    "strings",
    "lsof",
    "fold",
    "uname",
    "whoami",
    "id",
    "sw_vers",
];

/// Keyword heads that merely PREFIX the command a segment really runs: `do rm x`
/// is `rm x`. Stripped before the head check so the keyword cannot launder it.
const KEYWORD_PREFIX: &[&str] = &[
    "do", "then", "else", "if", "elif", "while", "until", "!", "{",
];

/// The directories a program may be named from by path and still be the program
/// its basename says: `/bin/ls` is `ls`, `/tmp/evil/ls` is not.
const SYSTEM_BIN_DIRS: &[&str] = &["/bin/", "/usr/bin/", "/sbin/", "/usr/sbin/"];

/// Variables whose assignment changes what a LATER program runs or reads:
/// the search path, the shell's own start-up and splitting, the pagers and
/// editors a read tool may exec, the config roots git and rg read. `PATH` is
/// allowed when every entry is a [`SYSTEM_BIN_DIRS`] directory.
const HAZARD_VARS: &[&str] = &[
    "PATH",
    "IFS",
    "BASH_ENV",
    "ENV",
    "PROMPT_COMMAND",
    "PS4",
    "SHELLOPTS",
    "BASHOPTS",
    "PAGER",
    "MANPAGER",
    "LESS",
    "LESSOPEN",
    "LESSCLOSE",
    "EDITOR",
    "VISUAL",
    "SHELL",
    "HOME",
    "XDG_CONFIG_HOME",
    "ZDOTDIR",
    "RIPGREP_CONFIG_PATH",
    "PERL5OPT",
    "NODE_OPTIONS",
    "PYTHONSTARTUP",
    "PYTHONPATH",
];

/// Prefixes of [`HAZARD_VARS`]: the dynamic loader's and git's whole families.
const HAZARD_VAR_PREFIXES: &[&str] = &["LD_", "DYLD_", "GIT_", "BASH_FUNC_"];

/// `git <sub>` reads. `worktree` and `stash` are read-only ONLY as `list`.
const GIT_READ: &[&str] = &[
    "log",
    "show",
    "status",
    "diff",
    "branch",
    "rev-parse",
    "rev-list",
    "ls-files",
    "grep",
    "check-ignore",
    "describe",
    "cat-file",
    "for-each-ref",
    "tag",
    "remote",
    "blame",
    "shortlog",
    "config",
    "merge-base",
    "name-rev",
    "worktree",
    "stash",
];

/// `git <sub>` writes, named in the reason rather than left to the head check.
const GIT_DANGER: &[&str] = &[
    "push",
    "reset",
    "checkout",
    "switch",
    "rebase",
    "merge",
    "commit",
    "clean",
    "restore",
    "rm",
    "mv",
    "am",
    "cherry-pick",
    "revert",
    "pull",
    "fetch",
];

/// `find -exec <prog>` is a read when `<prog>` is one of these.
const FIND_EXEC_READ: &[&str] = &["du", "ls", "cat", "head", "wc", "stat", "file", "grep"];

/// `tmutil` verbs that only report.
const TMUTIL_READ: &[&str] = &["listlocalsnapshots", "latestbackup", "destinationinfo"];

/// Classify with the default python allowlist ([`DEFAULT_PYTHON_ALLOW`]).
pub fn classify_command(cmd: &str) -> Verdict {
    classify_command_with(cmd, DEFAULT_PYTHON_ALLOW)
}

/// Classify `cmd`; `python_allow` are the script-path globs `python3 <path>` may
/// run as a read (`*` matches any run of characters, `?` one).
pub fn classify_command_with<S: AsRef<str>>(cmd: &str, python_allow: &[S]) -> Verdict {
    // (0) Comments, here-document bodies, `$'…'` and `${…}`, read as the shell
    // reads them — or refused.
    let cmd = match prelex(cmd) {
        Ok(cmd) => cmd,
        Err(reason) => return Verdict::no(reason),
    };
    // (a) The worker's timeout idiom is a wrapper, not a perl program.
    let cmd = strip_alarm_idiom(&cmd);
    // (b) Quoted strings are opaque to the danger scan.
    let stripped = strip_quotes(&cmd);
    // (c)+(d) Segments of whitespace-split tokens.
    let segments = split_segments(&stripped);
    if segments.is_empty() {
        return Verdict::no("empty command");
    }

    if let Some(reason) = danger_scan(&segments) {
        return Verdict::no(reason);
    }
    // (e) The programs inside the quotes the scans above cannot see.
    if let Some(reason) = program_scan(&cmd) {
        return Verdict::no(reason);
    }
    for seg in &segments {
        if let Some(reason) = segment_head(seg, python_allow) {
            return Verdict::no(reason);
        }
    }
    Verdict {
        read_only: true,
        reason: "every segment read-only".to_string(),
    }
}

/// The REST of an rm line, judged as a read: [`classify_command_with`] over
/// every segment but the `rm` commands themselves (an `rm` head in any
/// letter case, after its assignments), the assignment-only segments and
/// the `mktemp` commands whose directory the rm resolver follows
/// (`supervise::policy::rm_breaker`). What the rm circuit-breaker rule
/// requires besides the resolver's verdict: the rule is only safe because a
/// bypass session would run the rest unasked anyway, and the bypass it
/// knows of was read off a footer the box has since replaced.
pub(crate) fn classify_except_rm<S: AsRef<str>>(cmd: &str, python_allow: &[S]) -> Verdict {
    let lexed = match prelex(cmd) {
        Ok(cmd) => cmd,
        Err(reason) => return Verdict::no(reason),
    };
    let lexed = strip_alarm_idiom(&lexed);
    let segments: Vec<Vec<String>> = split_segments(&strip_quotes(&lexed))
        .into_iter()
        .filter(|seg| {
            let head = seg.iter().find(|t| !is_assignment(t));
            match head {
                None => false,
                Some(h) => {
                    let p = program(h).to_ascii_lowercase();
                    p != "rm" && p != "mktemp"
                }
            }
        })
        .collect();
    if let Some(reason) = danger_scan(&segments) {
        return Verdict::no(reason);
    }
    if let Some(reason) = program_scan(&lexed) {
        return Verdict::no(reason);
    }
    for seg in &segments {
        if let Some(reason) = segment_head(seg, python_allow) {
            return Verdict::no(reason);
        }
    }
    Verdict {
        read_only: true,
        reason: "every segment but the rm and mktemp commands read-only".to_string(),
    }
}

/// The shell's reading of the parts of a line the segment readers below get
/// wrong, applied first: `Ok` is the line with every `#` comment and every
/// here-document body removed (their newlines kept), `$"…"` read as `"…"` and
/// a backslash-free `$'…'` as `'…'`; `Err` names the construct this reader
/// will not follow, and the line is not read-only.
///
/// Why each: a comment's apostrophe opened a quote for [`strip_quotes`] that
/// the shell never opened, hiding every later line (`git log # what's new⏎git
/// push --force` read as one read); a here-document body is data, not code,
/// but a quote inside it did the same (`cat <<EOF⏎echo '⏎EOF⏎rm -rf /`); and
/// `$'a\'b'` ends where a plain `'…'` would not. A body under an UNQUOTED
/// terminator still expands `$(…)` and backticks, so such a body is refused;
/// so is a `${…}` holding a quote or a substitution, and any quote,
/// substitution or here-document left open at the end.
pub(crate) fn prelex(src: &str) -> Result<String, String> {
    let mut lx = Prelex {
        chars: src.chars().collect(),
        i: 0,
        out: String::with_capacity(src.len()),
        heredocs: Vec::new(),
    };
    lx.run(None)?;
    if !lx.heredocs.is_empty() {
        // `cat <<EOF` with no newline after it: the shell reads the body from
        // the lines that follow, and there are none on this line.
        return Err("a here-document with no body".to_string());
    }
    Ok(lx.out)
}

/// A here-document whose body starts at the next newline.
struct Heredoc {
    delim: String,
    /// `<<-`: leading tabs are stripped from each body line and the terminator.
    strip_tabs: bool,
    /// Any quoting in the delimiter word: the body is not expanded.
    quoted: bool,
}

struct Prelex {
    chars: Vec<char>,
    i: usize,
    out: String,
    heredocs: Vec<Heredoc>,
}

impl Prelex {
    fn peek(&self, k: usize) -> Option<char> {
        self.chars.get(self.i + k).copied()
    }

    /// Whether position `i` starts a word: the shell's rule for where `#`
    /// opens a comment.
    fn at_word_start(&self) -> bool {
        self.i == 0
            || self.chars[self.i - 1].is_whitespace()
            || matches!(self.chars[self.i - 1], ';' | '&' | '|' | '(' | ')')
    }

    /// One level: the top (`stop` none), a `$(…)` (`)`), or a backtick
    /// substitution. Returns having consumed the closing character.
    fn run(&mut self, stop: Option<char>) -> Result<(), String> {
        let mut depth = 0usize;
        while let Some(c) = self.peek(0) {
            match c {
                '\\' => {
                    self.out.push(c);
                    self.i += 1;
                    if let Some(n) = self.peek(0) {
                        self.out.push(n);
                        self.i += 1;
                    }
                }
                '\'' => self.single_quoted()?,
                '"' => self.double_quoted()?,
                '$' if self.peek(1) == Some('\'') => {
                    // ANSI-C quoting: a backslash escape inside it is where a
                    // plain `'…'` reader and the shell part ways.
                    self.i += 2;
                    let start = self.i;
                    while let Some(d) = self.peek(0) {
                        if d == '\\' {
                            return Err("$'…' with a backslash escape".to_string());
                        }
                        if d == '\'' {
                            break;
                        }
                        self.i += 1;
                    }
                    if self.peek(0) != Some('\'') {
                        return Err("an unterminated $' quote".to_string());
                    }
                    let body: String = self.chars[start..self.i].iter().collect();
                    self.i += 1;
                    self.out.push('\'');
                    self.out.push_str(&body);
                    self.out.push('\'');
                }
                '$' if self.peek(1) == Some('"') => {
                    // `$"…"` is `"…"` translated: the same expansion rules.
                    self.i += 1;
                    self.double_quoted()?;
                }
                '$' if self.peek(1) == Some('{') => self.braced_parameter()?,
                '$' if self.peek(1) == Some('(') => {
                    self.out.push_str("$(");
                    self.i += 2;
                    self.run(Some(')'))?;
                    self.out.push(')');
                }
                '$' => self.bare_parameter()?,
                '`' if stop == Some('`') => {
                    self.i += 1;
                    return Ok(());
                }
                '`' => {
                    self.out.push('`');
                    self.i += 1;
                    self.run(Some('`'))?;
                    self.out.push('`');
                }
                '(' => {
                    depth += 1;
                    self.out.push(c);
                    self.i += 1;
                }
                ')' => {
                    self.i += 1;
                    if depth == 0 && stop == Some(')') {
                        return Ok(());
                    }
                    depth = depth.saturating_sub(1);
                    self.out.push(c);
                }
                '#' if self.at_word_start() => {
                    // A comment runs to the newline, which is kept.
                    while self.peek(0).is_some_and(|d| d != '\n') {
                        self.i += 1;
                    }
                }
                '<' if self.peek(1) == Some('<') && self.peek(2) == Some('<') => {
                    // A here-string: a word, not a body.
                    self.out.push_str("<<<");
                    self.i += 3;
                }
                '<' if self.peek(1) == Some('<') => self.heredoc_operator()?,
                '\n' => {
                    self.out.push('\n');
                    self.i += 1;
                    self.heredoc_bodies()?;
                }
                _ => {
                    self.out.push(c);
                    self.i += 1;
                }
            }
        }
        match stop {
            Some(')') => Err("an unterminated $(".to_string()),
            Some(_) => Err("an unterminated backtick".to_string()),
            None => Ok(()),
        }
    }

    fn single_quoted(&mut self) -> Result<(), String> {
        let start = self.i;
        self.i += 1;
        while self.peek(0).is_some_and(|d| d != '\'') {
            self.i += 1;
        }
        if self.peek(0).is_none() {
            return Err("an unterminated ' quote".to_string());
        }
        self.i += 1;
        self.out.extend(&self.chars[start..self.i]);
        Ok(())
    }

    fn double_quoted(&mut self) -> Result<(), String> {
        self.out.push('"');
        self.i += 1;
        loop {
            let Some(d) = self.peek(0) else {
                return Err("an unterminated \" quote".to_string());
            };
            match d {
                '\\' => {
                    self.out.push(d);
                    self.i += 1;
                    if let Some(n) = self.peek(0) {
                        self.out.push(n);
                        self.i += 1;
                    }
                }
                '"' => {
                    self.out.push(d);
                    self.i += 1;
                    return Ok(());
                }
                '$' if self.peek(1) == Some('(') => {
                    self.out.push_str("$(");
                    self.i += 2;
                    self.run(Some(')'))?;
                    self.out.push(')');
                }
                '$' if self.peek(1) == Some('{') => self.braced_parameter()?,
                '$' => self.bare_parameter()?,
                '`' => {
                    self.out.push('`');
                    self.i += 1;
                    self.run(Some('`'))?;
                    self.out.push('`');
                }
                _ => {
                    self.out.push(d);
                    self.i += 1;
                }
            }
        }
    }

    /// A `$` that opens neither `${`, `$(`, `$'` nor `$"`: copied, unless it
    /// opens an ARITHMETIC evaluation — `$[x]` (bash's old `$((x))`) or
    /// zsh's unbraced subscript `$name[x]` — whose subscript expansion runs a
    /// substitution held in the variable's value (`x='a[$(touch M)]'; echo
    /// $[x]`, measured).
    fn bare_parameter(&mut self) -> Result<(), String> {
        if self.peek(1) == Some('[') {
            return Err("a $[…] arithmetic expansion".to_string());
        }
        let mut k = 1;
        while self
            .peek(k)
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            k += 1;
        }
        if k > 1 && self.peek(k) == Some('[') {
            return Err("a $name[…] subscript (zsh evaluates it as arithmetic)".to_string());
        }
        self.out.push('$');
        self.i += 1;
        Ok(())
    }

    /// `${…}`: copied when it holds no quote, substitution or nested `${`,
    /// which is every form a read uses (`${VAR}`, `${VAR:-x}`, `${#VAR}`),
    /// and when every ARITHMETIC part of it is a literal ([`braced_is_literal`]):
    /// an array subscript and a substring offset or length are evaluated as
    /// arithmetic, which expands a subscript in a variable's value and runs
    /// the substitution it holds (`x='a[$(touch M)]'; echo ${a[x]}`, `${y:x}`,
    /// `${a[@]:x}`, measured under bash; `${#a[x]}` under both shells).
    fn braced_parameter(&mut self) -> Result<(), String> {
        let start = self.i;
        self.i += 2;
        while let Some(d) = self.peek(0) {
            match d {
                '}' => {
                    let body: String = self.chars[start + 2..self.i].iter().collect();
                    braced_is_literal(&body)?;
                    self.i += 1;
                    self.out.extend(&self.chars[start..self.i]);
                    return Ok(());
                }
                '\'' | '"' | '`' | '\\' | '{' => {
                    return Err(
                        "a ${…} expansion holding a quote or a nested expansion".to_string()
                    );
                }
                '$' if matches!(self.peek(1), Some('(' | '{')) => {
                    return Err("a ${…} expansion holding a substitution".to_string());
                }
                _ => self.i += 1,
            }
        }
        Err("an unterminated ${".to_string())
    }

    /// `<<[-]WORD`: the operator and its word stay in the line (the command
    /// reads its stdin from it); the body is queued for the next newline.
    fn heredoc_operator(&mut self) -> Result<(), String> {
        self.out.push_str("<<");
        self.i += 2;
        let strip_tabs = self.peek(0) == Some('-');
        if strip_tabs {
            self.out.push('-');
            self.i += 1;
        }
        while self.peek(0).is_some_and(|d| d == ' ' || d == '\t') {
            self.out.push(self.chars[self.i]);
            self.i += 1;
        }
        let mut delim = String::new();
        let mut quoted = false;
        while let Some(d) = self.peek(0) {
            if d.is_whitespace() || matches!(d, ';' | '&' | '|' | '(' | ')' | '<' | '>') {
                break;
            }
            self.out.push(d);
            self.i += 1;
            match d {
                '\'' | '"' => {
                    quoted = true;
                    while let Some(e) = self.peek(0) {
                        self.out.push(e);
                        self.i += 1;
                        if e == d {
                            break;
                        }
                        delim.push(e);
                    }
                }
                '\\' => {
                    quoted = true;
                    if let Some(e) = self.peek(0) {
                        self.out.push(e);
                        self.i += 1;
                        delim.push(e);
                    }
                }
                '$' | '`' => {
                    return Err("a here-document delimiter with an expansion".to_string());
                }
                _ => delim.push(d),
            }
        }
        if delim.is_empty() {
            return Err("a here-document with no delimiter".to_string());
        }
        self.heredocs.push(Heredoc {
            delim,
            strip_tabs,
            quoted,
        });
        Ok(())
    }

    /// At a newline: consume the queued bodies, each up to and including its
    /// terminator line, and drop them from the output. A body that is never
    /// terminated runs to the end of the line, as the shell reads it.
    fn heredoc_bodies(&mut self) -> Result<(), String> {
        for doc in std::mem::take(&mut self.heredocs) {
            loop {
                if self.i >= self.chars.len() {
                    break;
                }
                let start = self.i;
                while self.peek(0).is_some_and(|d| d != '\n') {
                    self.i += 1;
                }
                let line: String = self.chars[start..self.i].iter().collect();
                if self.peek(0) == Some('\n') {
                    self.i += 1;
                }
                let bare = if doc.strip_tabs {
                    line.trim_start_matches('\t')
                } else {
                    line.as_str()
                };
                if bare == doc.delim {
                    break;
                }
                if !doc.quoted && (line.contains("$(") || line.contains('`')) {
                    return Err(
                        "a here-document body that runs a command (unquoted delimiter)".to_string(),
                    );
                }
                if !doc.quoted {
                    body_arithmetic_is_literal(&line)?;
                }
            }
        }
        Ok(())
    }
}

/// The arithmetic parts of a `${…}` body (the text between `${` and `}`)
/// are literals: its subscript is an integer, `@` or `*`, and a substring
/// offset or length (`${y:1}`, `${y: -2:3}`) is an integer. The name is a
/// plain name, a positional or a special parameter, optionally led by `#`
/// or `!`; a form this reader does not know — zsh's `${(e)x}` flags among
/// them, which evaluate the value as a command line — is refused. The
/// default-value forms (`:-`, `:=`, `:+`, `:?`) and the pattern forms
/// (`#`, `%`, `/`) evaluate no arithmetic.
fn braced_is_literal(body: &str) -> Result<(), String> {
    let refuse = |why: &str| Err(format!("a ${{{body}}} expansion with {why}"));
    let int = |t: &str| {
        let t = t.trim();
        let t = t.strip_prefix('-').unwrap_or(t);
        !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit())
    };
    let mut rest = body;
    if let Some(r) = rest.strip_prefix(['#', '!']) {
        if r.is_empty() {
            // `${#}` and `${!}`: the special parameters themselves.
            return Ok(());
        }
        rest = r;
    }
    let name_len = if rest.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') {
        rest.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(rest.len())
    } else if rest.starts_with(|c: char| c.is_ascii_digit()) {
        rest.find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len())
    } else if rest.starts_with(['@', '*', '#', '?', '$', '!', '-']) {
        1
    } else {
        return refuse("a form this reader does not follow");
    };
    rest = &rest[name_len..];
    if let Some(r) = rest.strip_prefix('[') {
        let Some(end) = r.find(']') else {
            return refuse("an unterminated subscript");
        };
        let sub = &r[..end];
        if !(sub == "@" || sub == "*" || int(sub)) {
            return refuse("a subscript that is not a literal (evaluated as arithmetic)");
        }
        rest = &r[end + 1..];
    }
    if let Some(r) = rest.strip_prefix(':')
        && !r.starts_with(['-', '=', '+', '?'])
    {
        // `${name:offset[:length]}`: both are arithmetic.
        if !r.split(':').take(2).all(int) || r.split(':').count() > 2 {
            return refuse("a substring offset that is not a literal (evaluated as arithmetic)");
        }
    }
    Ok(())
}

/// The arithmetic an UNQUOTED here-document body expands is literal: its
/// `${…}` forms pass [`braced_is_literal`], and it holds no `$[…]` and no
/// `$name[…]` (the body of `cat <<EOF` is expanded as a double-quoted word).
fn body_arithmetic_is_literal(line: &str) -> Result<(), String> {
    let mut rest = line;
    while let Some(at) = rest.find('$') {
        let after = &rest[at + 1..];
        if after.starts_with('[') {
            return Err("a here-document body with a $[…] arithmetic expansion".to_string());
        }
        if let Some(b) = after.strip_prefix('{') {
            let Some(end) = b.find('}') else {
                return Err("an unterminated ${ in a here-document body".to_string());
            };
            braced_is_literal(&b[..end])?;
        } else {
            let n = after
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(after.len());
            if n > 0 && after[n..].starts_with('[') {
                return Err("a here-document body with a $name[…] subscript".to_string());
            }
        }
        rest = after;
    }
    Ok(())
}

/// Remove every `perl -e 'alarm N; exec @ARGV'` (either quote) wrapper — that
/// body EXACTLY (see [`is_alarm_body`]). A `perl -e` that is NOT that idiom
/// stays, and the danger scan refuses it: a body that also runs `system(…)`
/// before the `exec` is a program, however it starts.
fn strip_alarm_idiom(cmd: &str) -> String {
    let mut s = cmd.to_string();
    let mut from = 0;
    while let Some(rel) = s[from..].find("perl -e ") {
        let start = from + rel;
        let body_start = start + "perl -e ".len();
        let Some(q) = s[body_start..]
            .chars()
            .next()
            .filter(|c| *c == '\'' || *c == '"')
        else {
            from = body_start;
            continue;
        };
        let inner_start = body_start + 1;
        let Some(close_rel) = s[inner_start..].find(q) else {
            break;
        };
        let inner = &s[inner_start..inner_start + close_rel];
        if is_alarm_body(inner) {
            let mut end = inner_start + close_rel + 1;
            while s[end..].starts_with(' ') {
                end += 1;
            }
            s.replace_range(start..end, "");
            from = start;
        } else {
            from = inner_start + close_rel + 1;
        }
    }
    s
}

/// `alarm <digits> ; exec @ARGV [;]`, whitespace free between the parts, and
/// nothing else: the timeout wrapper and only the timeout wrapper.
fn is_alarm_body(body: &str) -> bool {
    let Some(rest) = body.trim().strip_prefix("alarm") else {
        return false;
    };
    let rest = rest.trim_start();
    let digits = rest.chars().take_while(char::is_ascii_digit).count();
    if digits == 0 {
        return false;
    }
    let Some(rest) = rest[digits..].trim_start().strip_prefix(';') else {
        return false;
    };
    let Some(rest) = rest.trim_start().strip_prefix("exec") else {
        return false;
    };
    let Some(rest) = rest.trim_start().strip_prefix("@ARGV") else {
        return false;
    };
    let rest = rest.trim();
    rest.is_empty() || rest == ";"
}

/// Replace every single- and double-quoted string with `""`, keeping a `$(…)`
/// or backtick substitution found INSIDE double quotes (its command still
/// runs), turning find's `\(`/`\)` into bare parens so the splitter sees the
/// group, and keeping any other backslash escape verbatim.
pub(crate) fn strip_quotes(src: &str) -> String {
    let chars: Vec<char> = src.chars().collect();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    strip_into(&chars, &mut i, &mut out, None);
    out
}

/// `stop` is the character that ends this level: `)` inside a `$(…)`, a
/// backtick inside a backtick substitution, none at the top.
fn strip_into(chars: &[char], i: &mut usize, out: &mut String, stop: Option<char>) {
    let mut depth = 0usize;
    while *i < chars.len() {
        let c = chars[*i];
        match c {
            '\\' => {
                match chars.get(*i + 1) {
                    Some(&n) if n == '(' || n == ')' => out.push(n),
                    Some(&n) => {
                        out.push('\\');
                        out.push(n);
                    }
                    None => out.push('\\'),
                }
                *i += if *i + 1 < chars.len() { 2 } else { 1 };
            }
            '\'' => {
                *i += 1;
                while *i < chars.len() && chars[*i] != '\'' {
                    *i += 1;
                }
                *i += 1;
                out.push_str("\"\"");
            }
            '"' => {
                *i += 1;
                out.push_str("\"\"");
                while *i < chars.len() {
                    let d = chars[*i];
                    if d == '\\' {
                        *i += 2;
                        continue;
                    }
                    if d == '"' {
                        *i += 1;
                        break;
                    }
                    if d == '$' && chars.get(*i + 1) == Some(&'(') {
                        out.push_str(" $(");
                        *i += 2;
                        strip_into(chars, i, out, Some(')'));
                        out.push_str(") ");
                        continue;
                    }
                    if d == '`' {
                        out.push_str(" `");
                        *i += 1;
                        strip_into(chars, i, out, Some('`'));
                        out.push_str("` ");
                        continue;
                    }
                    *i += 1;
                }
            }
            '$' if chars.get(*i + 1) == Some(&'(') => {
                out.push_str("$(");
                *i += 2;
                strip_into(chars, i, out, Some(')'));
                out.push(')');
            }
            '`' if stop == Some('`') => {
                *i += 1;
                return;
            }
            '(' => {
                depth += 1;
                out.push('(');
                *i += 1;
            }
            ')' => {
                *i += 1;
                if depth == 0 && stop == Some(')') {
                    return;
                }
                depth = depth.saturating_sub(1);
                out.push(')');
            }
            _ => {
                out.push(c);
                *i += 1;
            }
        }
    }
}

/// Split on `;`, `&`, `&&`, `||`, `|`, `$(`, `(`, `)`, backtick and newline;
/// each segment is its whitespace-split tokens. A backslash-escaped character
/// (find's `\;`) is literal, never a separator, and the `&` of a redirect
/// (`2>&1`, `&>f`, `<&3`) stays in its token — only a job-control `&` splits,
/// so the command run after `ls &` is a head of its own. A `>` or `<` glued to
/// the END of a word that is not a descriptor number starts a token of its
/// own, as the shell reads it: `rm>/dev/null -rf /` is `rm` and a redirect.
pub(crate) fn split_segments(s: &str) -> Vec<Vec<String>> {
    let mut segments = Vec::new();
    let mut cur = String::new();
    let flush = |cur: &mut String, segments: &mut Vec<Vec<String>>| {
        let toks: Vec<String> = cur.split_whitespace().map(str::to_string).collect();
        if !toks.is_empty() {
            segments.push(toks);
        }
        cur.clear();
    };
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\\' => {
                cur.push(c);
                if let Some(&n) = chars.get(i + 1) {
                    cur.push(n);
                    i += 1;
                }
            }
            ';' | '|' | '(' | ')' | '`' | '\n' => flush(&mut cur, &mut segments),
            '>' | '<' => {
                if glued_to_a_word(&cur) {
                    cur.push(' ');
                }
                cur.push(c);
            }
            '&' if chars.get(i + 1) == Some(&'&') => {
                flush(&mut cur, &mut segments);
                i += 1;
            }
            '&' => {
                let prev = i.checked_sub(1).map(|k| chars[k]);
                let next = chars.get(i + 1).copied();
                if prev == Some('>') || prev == Some('<') || next == Some('>') {
                    cur.push(c);
                } else {
                    flush(&mut cur, &mut segments);
                }
            }
            '$' if chars.get(i + 1) == Some(&'(') => {
                flush(&mut cur, &mut segments);
                i += 1;
            }
            _ => cur.push(c),
        }
        i += 1;
    }
    flush(&mut cur, &mut segments);
    segments
}

/// Whether a redirect glyph arriving now would be glued to a WORD (`rm>x`),
/// not to a descriptor (`2>x`), another glyph (`>>`, `<>`), or `&` (`&>x`).
fn glued_to_a_word(cur: &str) -> bool {
    let word = cur.rsplit(char::is_whitespace).next().unwrap_or("");
    let Some(last) = word.chars().last() else {
        return false;
    };
    !matches!(last, '>' | '<' | '&') && !word.chars().all(|c| c.is_ascii_digit())
}

/// The program name of a token: its basename, so `/bin/rm` and `rm` agree.
pub(crate) fn program(tok: &str) -> &str {
    tok.rsplit('/').next().unwrap_or(tok)
}

/// An output redirect token: `Some(target)` when `tok` redirects (`>`, `>>`,
/// `2>`, `&>`, `>|`, `>file`); `None` when the target is the NEXT token.
pub(crate) fn redirect_target(tok: &str) -> Option<Option<&str>> {
    let pos = tok.find('>')?;
    // `<>` and `<(` are not output redirects; `->`/`=>` inside a word are, in
    // shell, so they are refused (a tie breaks toward not-read-only).
    if tok[..pos].ends_with('<') {
        return None;
    }
    let rest = tok[pos..].trim_start_matches('>');
    let rest = rest.strip_prefix('|').unwrap_or(rest);
    Some((!rest.is_empty()).then_some(rest))
}

/// A redirect target that writes no file: `/dev/null`, a descriptor (`&1`), or
/// a close (`&-`). `>&word` with any other word is bash for "stdout and stderr
/// into the file `word`".
pub(crate) fn redirect_is_safe(target: &str) -> bool {
    target == "/dev/null"
        || target.strip_prefix('&').is_some_and(|fd| {
            fd == "-" || (!fd.is_empty() && fd.bytes().all(|b| b.is_ascii_digit()))
        })
}

/// Skip `git`'s global options to its subcommand: `-C <dir>`, `-c <k=v>`,
/// `--git-dir=…`, `--work-tree=…`, `--no-pager`, `-P`, and any other flag.
fn git_subcommand(seg: &[String], git_idx: usize) -> Option<(usize, &str)> {
    let mut j = git_idx + 1;
    while j < seg.len() {
        let t = seg[j].as_str();
        if t == "-C" || t == "-c" || t == "--git-dir" || t == "--work-tree" {
            j += 2;
        } else if t.starts_with('-') {
            j += 1;
        } else {
            return Some((j, t));
        }
    }
    None
}

/// The NEGATIVE filter, over every token: a redirect to a file, a `find
/// -delete` / `-exec <writer>`, `sed -i`, an inline-code interpreter, a python
/// heredoc, `xargs` feeding a writer, a git write or a git flag that runs a
/// program or writes a file, the flags by which `rg`, `sort`, `less` and
/// `printf` do the same, a clock-setting `date`, and an assignment to a
/// [`HAZARD_VARS`] variable. A program NAME is not judged here: that is the
/// head check's, so `grep -rn open src` is a read and `open src` is not.
pub(crate) fn danger_scan(segments: &[Vec<String>]) -> Option<String> {
    for seg in segments {
        for (i, tok) in seg.iter().enumerate() {
            let prog = program(tok);
            let next = seg.get(i + 1).map(String::as_str);
            let next_prog = next.map(program);
            if let Some(reason) = assignment_hazard(tok) {
                return Some(reason);
            }
            if prog == "xargs" {
                // `xargs [-0] [-n N] [-I {}] <cmd>`: name the fed command.
                let mut k = i + 1;
                while k < seg.len() && seg[k].starts_with('-') {
                    k += if seg[k] == "-n" || seg[k] == "-I" || seg[k] == "-L" || seg[k] == "-P" {
                        2
                    } else {
                        1
                    };
                }
                if let Some(fed) = seg.get(k).map(|t| program(t))
                    && matches!(fed, "rm" | "mv" | "cp")
                {
                    return Some(format!("xargs {fed}"));
                }
            }
            if tok == "." && i == 0 {
                return Some("source".to_string());
            }
            let test_head = tok == "["
                || (tok == "test" && (i == 0 || KEYWORD_PREFIX.contains(&seg[i - 1].as_str())));
            if tok == "[[" || test_head {
                // `[[`'s arithmetic comparisons evaluate their operands as
                // arithmetic, and so does `-v name[sub]` in `[[`, `[` and
                // `test`: a subscript in a variable's value runs the
                // substitution it holds (measured, bash and zsh). `[`/`test`
                // with `-eq` compare integers without evaluating (measured).
                let arith: &[&str] = if tok == "[[" {
                    &["-eq", "-ne", "-lt", "-le", "-gt", "-ge", "-v"]
                } else {
                    &["-v"]
                };
                if let Some(op) = seg[i + 1..]
                    .iter()
                    .take_while(|t| *t != "]]" && *t != "]")
                    .find(|t| arith.contains(&t.as_str()))
                {
                    return Some(format!("{tok} {op} (evaluates arithmetic)"));
                }
            }
            if tok.contains("<>") {
                // Opens the file read-write, creating it.
                return Some(format!("redirect <> {tok}"));
            }
            if tok == "-delete" {
                return Some("find -delete".to_string());
            }
            if tok.starts_with("-fprint") || tok == "-fls" {
                return Some(format!("find {tok}"));
            }
            if prog == "sort" || prog == "tree" {
                // `sort -o FILE` / `--output=FILE`, `tree -o FILE`: an output
                // file. Neither has another short flag with an `o` in it.
                for t in &seg[i + 1..] {
                    if t == "--output"
                        || t.starts_with("--output=")
                        || (t.starts_with('-') && !t.starts_with("--") && t[1..].contains('o'))
                    {
                        return Some(format!("{prog} -o"));
                    }
                }
            }
            if prog == "uniq" {
                // `uniq IN OUT`: a second positional is the output file.
                let mut positionals = 0;
                let mut k = i + 1;
                while k < seg.len() {
                    let t = seg[k].as_str();
                    if matches!(t, "-f" | "-s" | "-w") {
                        k += 2;
                        continue;
                    }
                    if t.starts_with('-') && t.len() > 1 {
                        k += 1;
                        continue;
                    }
                    positionals += 1;
                    k += 1;
                }
                if positionals >= 2 {
                    return Some("uniq <output file>".to_string());
                }
            }
            if matches!(tok.as_str(), "-exec" | "-execdir" | "-ok" | "-okdir") {
                match next_prog {
                    Some(p) if next.is_some_and(|n| n.contains('/') && !is_system_bin(n)) => {
                        return Some(format!("find {tok} {p} (by path)"));
                    }
                    Some(p) if FIND_EXEC_READ.contains(&p) => {}
                    Some(p) => return Some(format!("find {tok} {p}")),
                    None => return Some(format!("find {tok}")),
                }
            }
            if let Some(target) = redirect_target(tok) {
                let target = target.or(next);
                match target {
                    Some(t) if redirect_is_safe(t) => {}
                    Some(t) => return Some(format!("redirect > {t}")),
                    None => return Some("redirect > (no target)".to_string()),
                }
            }
            if prog == "sed" {
                for t in &seg[i + 1..] {
                    if t == "--in-place"
                        || t.starts_with("--in-place=")
                        || (t.starts_with('-') && !t.starts_with("--") && t[1..].contains('i'))
                    {
                        return Some("sed -i".to_string());
                    }
                }
            }
            if matches!(prog, "python" | "python2" | "python3") {
                match next {
                    Some("-c") => return Some(format!("{prog} -c")),
                    Some("-") => return Some(format!("{prog} heredoc")),
                    _ => {}
                }
            }
            if prog == "perl" && matches!(next, Some("-e") | Some("-E")) {
                return Some("perl -e".to_string());
            }
            if matches!(prog, "bash" | "sh" | "zsh") && next == Some("-c") {
                return Some(format!("{prog} -c"));
            }
            if prog == "git" {
                // Global options that set config or the helper path run a
                // program of the line's choosing (`-c core.pager=…`,
                // `-c core.fsmonitor=…`) whatever the subcommand.
                let mut j = i + 1;
                while let Some(t) = seg.get(j).map(String::as_str) {
                    if t == "-c"
                        || (t.starts_with("-c") && t.len() > 2)
                        || t.starts_with("--config-env")
                        || t.starts_with("--exec-path")
                    {
                        return Some(format!("git {t} (sets config for the run)"));
                    }
                    if t.starts_with("--git-dir") || t.starts_with("--work-tree") {
                        // A repository of the line's choosing, bare or not,
                        // whose config (`core.fsmonitor`, `diff.external`)
                        // runs a program on a read. `-C` stays a read: `cd X
                        // && git status` reaches the same repository.
                        return Some(format!("git {t} (a repository of the line's choosing)"));
                    }
                    if matches!(t, "-C" | "--git-dir" | "--work-tree" | "--namespace") {
                        j += 2;
                    } else if t.starts_with('-') {
                        j += 1;
                    } else {
                        break;
                    }
                }
            }
            if prog == "git"
                && let Some((sub_idx, sub)) = git_subcommand(seg, i)
            {
                let arg = seg.get(sub_idx + 1).map(String::as_str);
                // Flags of the read subcommands that write a file or run a
                // program: `--output=<file>`, an external diff driver, `grep
                // -O<pager>`. Read up to the segment's end (a later segment
                // is its own command).
                for t in &seg[sub_idx + 1..] {
                    let t = t.as_str();
                    if t == "--output"
                        || t.starts_with("--output=")
                        || t == "--ext-diff"
                        || t == "--textconv"
                        || t == "--filters"
                        || t.starts_with("--open-files-in-pager")
                        || (t.starts_with("-O") && !t.starts_with("--"))
                    {
                        return Some(format!("git {sub} {t}"));
                    }
                }
                if GIT_DANGER.contains(&sub) {
                    return Some(format!("git {sub}"));
                }
                if (sub == "stash" || sub == "worktree") && arg != Some("list") {
                    return Some(
                        format!("git {sub} {}", arg.unwrap_or(""))
                            .trim()
                            .to_string(),
                    );
                }
            }
        }
    }
    None
}

/// Whether `tok` names a program by a path in [`SYSTEM_BIN_DIRS`] and nowhere
/// deeper (`/bin/ls`, not `/bin/x/ls` or `/bin/../tmp/ls`).
fn is_system_bin(tok: &str) -> bool {
    SYSTEM_BIN_DIRS.iter().any(|dir| {
        tok.strip_prefix(dir)
            .is_some_and(|rest| !rest.is_empty() && !rest.contains('/'))
    })
}

/// `Some(reason)` when `tok` assigns a [`HAZARD_VARS`] variable (or one of the
/// [`HAZARD_VAR_PREFIXES`] families). `PATH` passes when every entry is a
/// system directory (`env -i PATH=/bin ls`), and nothing else does: an entry
/// read from a variable (`PATH=/x:$PATH`) or a quote is not a known directory.
fn assignment_hazard(tok: &str) -> Option<String> {
    if !is_assignment(tok) {
        return None;
    }
    let (name, value) = tok.split_once('=')?;
    if name == "PATH" {
        let system = value.split(':').all(|entry| {
            let entry = entry.trim_end_matches('/');
            SYSTEM_BIN_DIRS
                .iter()
                .any(|dir| dir.trim_end_matches('/') == entry)
        });
        return (!system).then(|| format!("{name}= (changes which program a name runs)"));
    }
    (HAZARD_VARS.contains(&name) || HAZARD_VAR_PREFIXES.iter().any(|p| name.starts_with(p)))
        .then(|| format!("{name}= (changes what a later program runs or reads)"))
}

/// The flags by which a read tool, at a segment head, runs a program or
/// writes: `rg --pre <prog>`, `sort --compress-program=<prog>`, `printf -v
/// NAME` (an assignment), `less`/`more` with a `+<command>`, a log file or a
/// key file, `sysctl name=value`/`-w`, `hostname <name>`/`-F`, `file -C`,
/// `tree -R`, and `date` setting the clock (`-s`, or a positional operand
/// without BSD's `-j`).
fn runs_or_writes_by_flag(head: &str, args: &[String]) -> Option<String> {
    let flag = |t: &str| format!("{head} {t}");
    match head {
        "rg" => args
            .iter()
            .find(|t| *t == "--pre" || t.starts_with("--pre="))
            .map(|t| flag(t)),
        "sort" => args
            .iter()
            .find(|t| t.starts_with("--compress-program"))
            .map(|t| flag(t)),
        "printf" => args
            .iter()
            .take_while(|t| t.starts_with('-'))
            .find(|t| t.starts_with("-v"))
            .map(|t| flag(t)),
        "less" | "more" => args
            .iter()
            .find(|t| {
                t.starts_with('+')
                    || t.starts_with("--log-file")
                    || t.starts_with("--LOG-FILE")
                    || t.starts_with("--lesskey")
                    || (t.starts_with('-')
                        && !t.starts_with("--")
                        && t[1..].chars().any(|c| matches!(c, 'o' | 'O' | 'k')))
            })
            .map(|t| flag(t)),
        "sysctl" => args
            .iter()
            .find(|t| {
                t.contains('=')
                    || (t.starts_with('-') && !t.starts_with("--") && t[1..].contains('w'))
            })
            .map(|t| format!("sysctl {t} (sets a kernel value)")),
        "hostname" => args
            .iter()
            .filter(|t| redirect_target(t).is_none())
            .find(|t| !t.starts_with('-') || t.starts_with("-F"))
            .map(|t| format!("hostname {t} (sets the host name)")),
        "file" => args
            .iter()
            .find(|t| {
                t.starts_with("--compile")
                    || (t.starts_with('-') && !t.starts_with("--") && t[1..].contains('C'))
            })
            .map(|t| format!("file {t} (writes a compiled magic file)")),
        "tree" => args
            .iter()
            .find(|t| t.starts_with('-') && !t.starts_with("--") && t[1..].contains('R'))
            .map(|t| format!("tree {t} (writes 00Tree.html files)")),
        "date" => {
            let mut k = 0;
            let mut positional = None;
            let mut no_set = false;
            while let Some(t) = args.get(k).map(String::as_str) {
                if let Some(target) = redirect_target(t) {
                    // `2>/dev/null` is the shell's, not an operand.
                    k += if target.is_none() { 2 } else { 1 };
                    continue;
                }
                if t == "-s" || t.starts_with("--set") {
                    return Some(flag(t));
                }
                if t == "-j" {
                    no_set = true;
                }
                if matches!(t, "-r" | "-f" | "-v" | "-d" | "-z") {
                    k += 2;
                    continue;
                }
                if !t.starts_with('-') && !t.starts_with('+') && positional.is_none() {
                    positional = Some(t);
                }
                k += 1;
            }
            positional
                .filter(|_| !no_set)
                .map(|t| format!("date {t} (sets the clock)"))
        }
        _ => None,
    }
}

/// Whether `tok` is a `VAR=value` assignment prefix.
fn is_assignment(tok: &str) -> bool {
    let Some(eq) = tok.find('=') else {
        return false;
    };
    let name = &tok[..eq];
    let mut cs = name.chars();
    matches!(cs.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && cs.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// A `timeout` duration: digits with an optional `.frac` and `s`/`m`/`h`/`d`.
fn is_duration(tok: &str) -> bool {
    let body = tok.trim_end_matches(['s', 'm', 'h', 'd']);
    !body.is_empty() && body.chars().all(|c| c.is_ascii_digit() || c == '.')
}

/// Whether a segment is a continuation fragment, not a command: it starts with a
/// flag (`-name` after a `\(` split) or a redirect (`2>/dev/null` after a `)`).
fn is_fragment(head: &str) -> bool {
    head.starts_with('-') || head.starts_with('<') || redirect_target(head).is_some()
}

/// `xargs` flags that take the next token as their value.
const XARGS_VALUE_FLAGS: &[&str] = &[
    "-n", "-I", "-L", "-P", "-d", "-E", "-s", "-a", "-J", "-R", "-S",
];

/// The POSITIVE filter on one segment: `None` when its head is a read-only
/// program (with the git / tmutil / python refinements), else the reason.
pub(crate) fn segment_head<S: AsRef<str>>(seg: &[String], python_allow: &[S]) -> Option<String> {
    head_from(seg, 0, python_allow)
}

/// The head check from token `j` on. Re-entered for the command a wrapper
/// hands its arguments to — `xargs <cmd>`, `env [VAR=v] <cmd>`, `timeout N
/// <cmd>` — so a wrapper on the read-only list cannot launder the program it
/// runs: `ls | xargs touch` is `touch`, `env FOO=1 ./deploy.sh` is `deploy.sh`.
fn head_from<S: AsRef<str>>(seg: &[String], mut j: usize, python_allow: &[S]) -> Option<String> {
    // Leading VAR=value assignments, a `timeout [flags] N` or `time [-p]`
    // wrapper, and keyword prefixes (the command after `do` / `then` / `if`
    // is the one that runs), in any order: `do n=$(…)` assigns.
    loop {
        let start = j;
        while j < seg.len() && (is_assignment(&seg[j]) || KEYWORD_PREFIX.contains(&seg[j].as_str()))
        {
            j += 1;
        }
        match seg.get(j).map(String::as_str) {
            Some("timeout") => {
                j += 1;
                while j < seg.len() && (seg[j].starts_with('-') || is_duration(&seg[j])) {
                    j += 1;
                }
            }
            Some("time") => {
                j += 1;
                while seg.get(j).map(String::as_str) == Some("-p") {
                    j += 1;
                }
            }
            _ => {}
        }
        if j == start {
            break;
        }
    }
    let head_tok = seg.get(j)?;
    if is_fragment(head_tok) {
        return None;
    }
    if head_tok.contains('/') && !is_system_bin(head_tok) {
        // `/tmp/evil/ls` and `./ls` are programs of the line's choosing
        // whatever their basename says.
        return Some(format!("{head_tok} (a program named by path)"));
    }
    let head = program(head_tok);
    let rest = &seg[j + 1..];
    let arg = rest.first().map(String::as_str);
    if let Some(reason) = runs_or_writes_by_flag(head, rest) {
        return Some(reason);
    }
    match head {
        "aterm" | "aterm-ctl" | "aterm-drive" => aterm_read(head, rest),
        "trustc" | "rustc" => {
            // Version and configuration queries only: anything else compiles.
            let query = !rest.is_empty()
                && rest.iter().enumerate().all(|(k, t)| {
                    matches!(t.as_str(), "-vV" | "-Vv" | "-V" | "-v" | "--version")
                        || t == "--print"
                        || t.starts_with("--print=")
                        || (k > 0 && rest[k - 1] == "--print" && !t.starts_with('-'))
                });
            (!query).then(|| format!("{head} (compiles)"))
        }
        "xargs" => {
            // `xargs [flags] [cmd [args]]`: the fed command is the head that
            // counts; a bare `xargs` runs echo.
            let mut k = j + 1;
            while k < seg.len() && seg[k].starts_with('-') && seg[k] != "-" {
                k += if XARGS_VALUE_FLAGS.contains(&seg[k].as_str()) {
                    2
                } else {
                    1
                };
            }
            if k < seg.len() {
                head_from(seg, k, python_allow)
            } else {
                None
            }
        }
        "env" => {
            // `env [-i] [-u NAME] [VAR=v]... [cmd]`: the command is the head;
            // a bare `env` prints the environment; `-S` splits a string into a
            // command line this scan cannot see.
            let mut k = j + 1;
            while k < seg.len() {
                let t = seg[k].as_str();
                if t == "--split-string"
                    || t.starts_with("--split-string=")
                    || (t.starts_with('-') && !t.starts_with("--") && t[1..].contains('S'))
                {
                    return Some("env -S".to_string());
                }
                if matches!(t, "-u" | "-C" | "-P" | "--unset" | "--chdir") {
                    k += 2;
                    continue;
                }
                if t == "--" {
                    k += 1;
                    break;
                }
                if (t.starts_with('-') && t.len() > 1) || is_assignment(t) {
                    k += 1;
                    continue;
                }
                break;
            }
            if k < seg.len() {
                head_from(seg, k, python_allow)
            } else {
                None
            }
        }
        "git" => {
            let Some((sub_idx, sub)) = git_subcommand(seg, j) else {
                return Some("git without a subcommand".to_string());
            };
            let next = seg.get(sub_idx + 1).map(String::as_str);
            if !GIT_READ.contains(&sub) {
                return Some(format!("git {sub}"));
            }
            if (sub == "worktree" || sub == "stash") && next != Some("list") {
                return Some(
                    format!("git {sub} {}", next.unwrap_or(""))
                        .trim()
                        .to_string(),
                );
            }
            let args = &seg[sub_idx + 1..];
            // A long flag matches bare or with its `=value`.
            let has = |flags: &[&str]| {
                args.iter().any(|a| {
                    flags
                        .iter()
                        .any(|f| a == f || (f.starts_with("--") && a.starts_with(&format!("{f}="))))
                })
            };
            let writes = match sub {
                "branch" => {
                    // Short flags combine (`-avv`), so they are read by letter:
                    // d D m M c C u f t write; a r l list. A positional with
                    // no list flag CREATES a branch (`git branch feature-x`) —
                    // `-v`/`--verbose` are no list flag: `git branch -v x`
                    // creates `x` (measured in a scratch repo).
                    let short = |a: &str| a.starts_with('-') && !a.starts_with("--") && a.len() > 1;
                    let write_short = args
                        .iter()
                        .any(|a| short(a) && a[1..].chars().any(|c| "dDmMcCuft".contains(c)));
                    let list_short = args
                        .iter()
                        .any(|a| short(a) && a[1..].chars().any(|c| "arl".contains(c)));
                    let list_long = has(&[
                        "--list",
                        "--all",
                        "--remotes",
                        "--contains",
                        "--no-contains",
                        "--merged",
                        "--no-merged",
                        "--points-at",
                        "--show-current",
                    ]);
                    let positional = args.iter().any(|a| !a.starts_with('-'));
                    write_short
                        || has(&[
                            "--delete",
                            "--move",
                            "--copy",
                            "--force",
                            "--track",
                            "--no-track",
                            "--set-upstream",
                            "--set-upstream-to",
                            "--unset-upstream",
                            "--edit-description",
                            "--create-reflog",
                        ])
                        || (positional && !list_short && !list_long)
                }
                "tag" => {
                    has(&["-d", "-a", "-s", "-f", "-m", "--delete", "-F"])
                        || (args.iter().any(|a| !a.starts_with('-'))
                            && !has(&[
                                "-l",
                                "--list",
                                "--contains",
                                "--no-contains",
                                "--points-at",
                                "--merged",
                                "--no-merged",
                                "-v",
                                "--verify",
                                "-n",
                            ]))
                }
                "remote" => matches!(
                    next,
                    Some(
                        "add"
                            | "remove"
                            | "rm"
                            | "rename"
                            | "set-url"
                            | "set-head"
                            | "set-branches"
                            | "prune"
                            | "update"
                    )
                ),
                "config" => {
                    has(&[
                        "--unset",
                        "--unset-all",
                        "--add",
                        "--replace-all",
                        "--edit",
                        "-e",
                        "--remove-section",
                        "--rename-section",
                    ]) || args.iter().filter(|a| !a.starts_with('-')).count() >= 2
                }
                _ => false,
            };
            if writes {
                return Some(format!("git {sub} (write form)"));
            }
            None
        }
        "tmutil" => match arg {
            Some(v) if TMUTIL_READ.contains(&v) => None,
            Some(v) => Some(format!("tmutil {v}")),
            None => Some("tmutil without a verb".to_string()),
        },
        "python" | "python2" | "python3" => match arg {
            Some(path) if !path.starts_with('-') => {
                if path.split('/').any(|c| c == "..") {
                    // `scripts/../../tmp/x_score.py` matches `scripts/*score*.py`
                    // as text and runs something else.
                    Some(format!(
                        "{head} {path} steps out of its directory with `..`"
                    ))
                } else if python_allow.iter().any(|g| glob_match(g.as_ref(), path)) {
                    None
                } else {
                    Some(format!(
                        "{head} {path} is not on the read-only script allowlist"
                    ))
                }
            }
            Some(flag) => Some(format!("{head} {flag}")),
            None => Some(format!("{head} without a script")),
        },
        h if READ_ONLY.contains(&h) => None,
        h => Some(h.to_string()),
    }
}

/// `aterm ctl` verbs that only read: the screen, the session's state and
/// history, the roster. By VERB AND FORM, never by verb alone: `meta` reads
/// but `meta set attention …` writes the owner's badge, and `inbox` reads but
/// `inbox seen` writes (`meta` and `inbox` are judged separately below).
/// `image`, `window`, `video` and `cast` are left out — they write a file.
const ATERM_CTL_READ: &[&str] = &[
    "status",
    "text",
    "screen",
    "line",
    "lines",
    "offscreen",
    "cell",
    "cursor",
    "dims",
    "modes",
    "title",
    "cwd",
    "colors",
    "search",
    "blocks",
    "blocktext",
    "metrics",
    "timeline",
    "history",
    "panes",
    "ready",
    "await",
    "wait",
    "family",
    "edges",
    "grants",
    "ls",
    "windows",
    "sessions",
    "help",
    "version",
    "who",
    "whoami",
];

/// `aterm pkg` and `aterm drive` verbs that only read.
const ATERM_PKG_READ: &[&str] = &["list", "which", "doctor"];
const ATERM_DRIVE_READ: &[&str] = &["phase", "classify", "help"];

/// `aterm …` (or its `aterm-ctl` / `aterm-drive` argv0 aliases): `None` when
/// the form only reads — `--version`, `help`, the [`ATERM_CTL_READ`] verbs,
/// bare `meta`, bare `inbox` and `inbox get <id>`, and the read verbs of `pkg`
/// and `drive`.
fn aterm_read(head: &str, rest: &[String]) -> Option<String> {
    let args: &[String] = match head {
        "aterm-ctl" => return aterm_ctl_read(rest),
        "aterm-drive" => {
            return match rest.first().map(String::as_str) {
                Some(v) if ATERM_DRIVE_READ.contains(&v) => None,
                v => Some(
                    format!("aterm-drive {}", v.unwrap_or(""))
                        .trim()
                        .to_string(),
                ),
            };
        }
        _ => rest,
    };
    let sub = args.first().map(String::as_str);
    let verb = args.get(1).map(String::as_str);
    match sub {
        Some("--version" | "-V" | "version" | "help" | "--help" | "-h") => None,
        Some("ctl") => aterm_ctl_read(&args[1..]),
        Some("pkg") if verb.is_some_and(|v| ATERM_PKG_READ.contains(&v)) => None,
        Some("drive") if verb.is_some_and(|v| ATERM_DRIVE_READ.contains(&v)) => None,
        Some(s) => Some(
            format!("aterm {s} {}", verb.unwrap_or(""))
                .trim()
                .to_string(),
        ),
        None => Some("aterm (opens a session)".to_string()),
    }
}

/// The arguments of `aterm ctl`: its own flags (`--sock P`, `--pid N`,
/// `--timeout S`), an optional `@<target>`, then the verb.
fn aterm_ctl_read(args: &[String]) -> Option<String> {
    let mut k = 0;
    while let Some(t) = args.get(k).map(String::as_str) {
        match t {
            "--sock" | "--pid" | "--timeout" => k += 2,
            "-h" | "--help" | "-V" | "--version" => return None,
            _ if t.starts_with("--sock=") || t.starts_with("--pid=") => k += 1,
            _ if t.starts_with("--timeout=") => k += 1,
            _ => break,
        }
    }
    if args.get(k).is_some_and(|t| t.starts_with('@')) {
        k += 1;
    }
    let verb = args.get(k).map(String::as_str);
    let tail = args.get(k + 1..).unwrap_or(&[]);
    let first = tail.first().map(String::as_str);
    let reads = match verb {
        Some(v) if ATERM_CTL_READ.contains(&v) => true,
        // `<verb> --help` describes the verb and never runs it.
        Some(_) if tail.iter().all(|t| t == "--help" || t == "-h") && !tail.is_empty() => true,
        Some("meta") => tail.is_empty(),
        Some("inbox") => first.is_none() || (first == Some("get") && tail.len() == 2),
        _ => false,
    };
    (!reads).then(|| {
        format!("aterm ctl {} {}", verb.unwrap_or(""), first.unwrap_or(""))
            .trim()
            .to_string()
    })
}

/// The programs a line hands to awk and sed, read from the RAW words (quotes
/// resolved, not dropped): `system(…)`, a redirect or a pipe in an awk
/// program; a `w`/`W`/`e` command or an `s///w` / `s///e` flag in a sed
/// script; a program file (`-f`) this scan cannot read. Every word is looked
/// at, not only heads, so `xargs awk …` and `timeout 5 sed …` are covered.
pub(crate) fn program_scan(cmd: &str) -> Option<String> {
    for seg in raw_words(cmd) {
        for (i, w) in seg.iter().enumerate() {
            let reason = match program(w) {
                "awk" | "gawk" | "mawk" | "nawk" => awk_args_write(&seg[i + 1..]),
                "sed" | "gsed" => sed_args_write(&seg[i + 1..]),
                _ => None,
            };
            if reason.is_some() {
                return reason;
            }
        }
    }
    None
}

/// `awk [-F s] [-v a=b]... 'program' files…`: the first non-flag word is the
/// program.
fn awk_args_write(args: &[String]) -> Option<String> {
    let mut k = 0;
    while k < args.len() {
        let w = args[k].as_str();
        if w == "-f"
            || w == "--file"
            || w.starts_with("--file=")
            || (w.starts_with("-f") && !w.starts_with("--"))
        {
            return Some("awk -f (a program file)".to_string());
        }
        if matches!(w, "-F" | "-v" | "-W" | "-e" | "--source") {
            if w == "-e" || w == "--source" {
                break;
            }
            k += 2;
            continue;
        }
        if w == "--" {
            k += 1;
            break;
        }
        if w.starts_with('-') && w.len() > 1 {
            k += 1;
            continue;
        }
        break;
    }
    // `-e PROGRAM` / `--source PROGRAM`: the program is the next word.
    if matches!(
        args.get(k).map(String::as_str),
        Some("-e") | Some("--source")
    ) {
        k += 1;
    }
    let program = args.get(k)?;
    awk_program_writes(program)
        .then(|| "awk writes (system, a redirect or a pipe in the program)".to_string())
}

/// Whether an awk program does more than read: `system(…)`, `>>`, a
/// coprocess, a `>` that is a redirect (to a string, a variable or an
/// expression — a `>` against a number, a field or `>=` is a comparison), or a
/// `|` that pipes to a command (`||` is or; a `|` inside a `/regex/` or a
/// string is neither).
fn awk_program_writes(p: &str) -> bool {
    if p.contains("system(") || p.contains(">>") || p.contains("|&") {
        return true;
    }
    let cs: Vec<char> = p.chars().collect();
    let mut k = 0;
    // The last non-space character seen outside strings and regexes: a `/`
    // after an operator or an opener starts a regex, elsewhere it divides.
    let mut prev: Option<char> = None;
    while k < cs.len() {
        let c = cs[k];
        match c {
            '"' => {
                k += 1;
                while k < cs.len() && cs[k] != '"' {
                    k += if cs[k] == '\\' { 2 } else { 1 };
                }
                k += 1;
                prev = Some('"');
                continue;
            }
            '/' if prev.is_none_or(|p| "{;(,!~&|=\n".contains(p)) => {
                k += 1;
                while k < cs.len() && cs[k] != '/' {
                    k += if cs[k] == '\\' { 2 } else { 1 };
                }
                k += 1;
                prev = Some('/');
                continue;
            }
            '|' => {
                if cs.get(k + 1) == Some(&'|') {
                    k += 2;
                    prev = Some('|');
                    continue;
                }
                return true;
            }
            '>' => {
                if cs.get(k + 1) == Some(&'=') || prev == Some('-') || prev == Some('<') {
                    k += 1;
                    prev = Some('>');
                    continue;
                }
                let next = cs[k + 1..].iter().find(|d| !d.is_whitespace());
                match next {
                    Some(d) if d.is_ascii_digit() || matches!(d, '$' | '-' | '+' | '.') => {}
                    _ => return true,
                }
            }
            _ => {}
        }
        if !c.is_whitespace() {
            prev = Some(c);
        }
        k += 1;
    }
    false
}

/// `sed [-n] [-E] [-e SCRIPT]... [SCRIPT] files…`: every `-e` script, else the
/// first non-flag word.
fn sed_args_write(args: &[String]) -> Option<String> {
    let mut scripts: Vec<&str> = Vec::new();
    let mut explicit = false;
    let mut k = 0;
    while k < args.len() {
        let w = args[k].as_str();
        if w == "-f" || w == "--file" || w.starts_with("--file=") {
            return Some("sed -f (a script file)".to_string());
        }
        if let Some(s) = w.strip_prefix("--expression=") {
            scripts.push(s);
            explicit = true;
            k += 1;
            continue;
        }
        if w == "-e"
            || w == "--expression"
            || (w.starts_with('-') && !w.starts_with("--") && w.ends_with('e') && w.len() > 1)
        {
            if let Some(s) = args.get(k + 1) {
                scripts.push(s);
            }
            explicit = true;
            k += 2;
            continue;
        }
        if matches!(w, "-l" | "--line-length") {
            k += 2;
            continue;
        }
        if w == "--" {
            k += 1;
            if !explicit && let Some(s) = args.get(k) {
                scripts.push(s);
            }
            break;
        }
        if w.starts_with('-') && w.len() > 1 {
            k += 1;
            continue;
        }
        if !explicit {
            scripts.push(w);
        }
        break;
    }
    scripts.iter().find_map(|s| sed_script_writes(s))
}

/// Walk a sed script command by command: `w`/`W` write a file, `e` runs a
/// command, so do the `w` and `e` flags of `s///`; anything this walker does
/// not know is refused too (a tie breaks toward not-read-only).
fn sed_script_writes(script: &str) -> Option<String> {
    let cs: Vec<char> = script.chars().collect();
    let n = cs.len();
    let unparsed = || Some("sed (unparsed script)".to_string());
    // From the char after an opening delimiter, past its closing one.
    let skip_delimited = |i: &mut usize, d: char| -> bool {
        while *i < n {
            if cs[*i] == '\\' {
                *i += 2;
                continue;
            }
            if cs[*i] == d {
                *i += 1;
                return true;
            }
            *i += 1;
        }
        false
    };
    let to_line_end = |i: &mut usize| {
        while *i < n && cs[*i] != '\n' {
            *i += 1;
        }
    };
    let to_command_end = |i: &mut usize| {
        while *i < n && cs[*i] != '\n' && cs[*i] != ';' && cs[*i] != '}' {
            *i += 1;
        }
    };
    let mut i = 0;
    while i < n {
        let c = cs[i];
        if c.is_whitespace() || c == ';' || c == '{' || c == '}' {
            i += 1;
            continue;
        }
        // Addresses: N[~M], $, /re/[IM], \cREc[IM], joined by `,`, then `!`.
        loop {
            if i >= n {
                return unparsed();
            }
            match cs[i] {
                d if d.is_ascii_digit() => {
                    while i < n && (cs[i].is_ascii_digit() || cs[i] == '~') {
                        i += 1;
                    }
                }
                '$' => i += 1,
                '/' => {
                    i += 1;
                    if !skip_delimited(&mut i, '/') {
                        return unparsed();
                    }
                    while i < n && matches!(cs[i], 'I' | 'M') {
                        i += 1;
                    }
                }
                '\\' => {
                    let Some(&d) = cs.get(i + 1) else {
                        return unparsed();
                    };
                    i += 2;
                    if !skip_delimited(&mut i, d) {
                        return unparsed();
                    }
                    while i < n && matches!(cs[i], 'I' | 'M') {
                        i += 1;
                    }
                }
                _ => break,
            }
            while i < n && cs[i].is_whitespace() {
                i += 1;
            }
            if i < n && cs[i] == ',' {
                i += 1;
                while i < n && cs[i].is_whitespace() {
                    i += 1;
                }
                continue;
            }
            break;
        }
        while i < n && (cs[i].is_whitespace() || cs[i] == '!') {
            i += 1;
        }
        if i >= n {
            return unparsed();
        }
        let cmd = cs[i];
        i += 1;
        match cmd {
            '{' | '}' => {}
            '#' | ':' | 'a' | 'i' | 'c' | 'r' | 'R' => to_line_end(&mut i),
            'b' | 't' | 'T' | 'v' => to_command_end(&mut i),
            'w' | 'W' => return Some("sed w (writes a file)".to_string()),
            'e' => return Some("sed e (runs a command)".to_string()),
            'q' | 'Q' | 'l' | 'L' => {
                while i < n && (cs[i].is_whitespace() || cs[i].is_ascii_digit()) {
                    i += 1;
                }
            }
            's' => {
                let Some(&d) = cs.get(i) else {
                    return unparsed();
                };
                i += 1;
                if !skip_delimited(&mut i, d) || !skip_delimited(&mut i, d) {
                    return unparsed();
                }
                while i < n {
                    match cs[i] {
                        'g' | 'p' | 'i' | 'I' | 'm' | 'M' | '0'..='9' => i += 1,
                        'w' => return Some("sed s///w (writes a file)".to_string()),
                        'e' => return Some("sed s///e (runs a command)".to_string()),
                        _ => break,
                    }
                }
            }
            'y' => {
                let Some(&d) = cs.get(i) else {
                    return unparsed();
                };
                i += 1;
                if !skip_delimited(&mut i, d) || !skip_delimited(&mut i, d) {
                    return unparsed();
                }
            }
            'd' | 'D' | 'p' | 'P' | 'n' | 'N' | 'g' | 'G' | 'h' | 'H' | 'x' | '=' | 'z' | 'F' => {}
            other => return Some(format!("sed {other} (unparsed script)")),
        }
    }
    None
}

/// The command as shell WORDS with their quotes resolved, in segments split
/// where [`split_segments`] splits (a `$(…)` or backtick substitution, inside
/// double quotes too, is its own segment): what a program's argument really
/// says, which the quote-stripped tokens cannot.
pub(crate) fn raw_words(src: &str) -> Vec<Vec<String>> {
    let chars: Vec<char> = src.chars().collect();
    let mut scan = WordScan {
        chars: &chars,
        i: 0,
        segments: Vec::new(),
        seg: Vec::new(),
        cur: String::new(),
        in_word: false,
    };
    scan.run(None);
    scan.segments
}

struct WordScan<'a> {
    chars: &'a [char],
    i: usize,
    segments: Vec<Vec<String>>,
    seg: Vec<String>,
    cur: String,
    in_word: bool,
}

impl WordScan<'_> {
    fn end_word(&mut self) {
        if self.in_word {
            self.seg.push(std::mem::take(&mut self.cur));
            self.in_word = false;
        }
    }
    fn end_seg(&mut self) {
        self.end_word();
        if !self.seg.is_empty() {
            self.segments.push(std::mem::take(&mut self.seg));
        }
    }
    /// A nested substitution: its words are their own segments.
    fn nested(&mut self, stop: char) {
        self.end_seg();
        self.run(Some(stop));
        self.in_word = true;
    }
    fn run(&mut self, stop: Option<char>) {
        let mut depth = 0usize;
        while self.i < self.chars.len() {
            let c = self.chars[self.i];
            match c {
                '\\' => {
                    if let Some(&n) = self.chars.get(self.i + 1) {
                        self.cur.push(n);
                        self.i += 2;
                    } else {
                        self.i += 1;
                    }
                    self.in_word = true;
                }
                '\'' => {
                    self.i += 1;
                    while self.i < self.chars.len() && self.chars[self.i] != '\'' {
                        self.cur.push(self.chars[self.i]);
                        self.i += 1;
                    }
                    self.i += 1;
                    self.in_word = true;
                }
                '"' => {
                    self.i += 1;
                    self.in_word = true;
                    while self.i < self.chars.len() {
                        let d = self.chars[self.i];
                        match d {
                            '\\' => {
                                match self.chars.get(self.i + 1) {
                                    Some(&n) if matches!(n, '"' | '\\' | '$' | '`') => {
                                        self.cur.push(n);
                                    }
                                    Some(&n) => {
                                        self.cur.push('\\');
                                        self.cur.push(n);
                                    }
                                    None => {}
                                }
                                self.i += 2;
                            }
                            '"' => {
                                self.i += 1;
                                break;
                            }
                            '$' if self.chars.get(self.i + 1) == Some(&'(') => {
                                self.i += 2;
                                self.nested(')');
                            }
                            '`' => {
                                self.i += 1;
                                self.nested('`');
                            }
                            _ => {
                                self.cur.push(d);
                                self.i += 1;
                            }
                        }
                    }
                }
                '$' if self.chars.get(self.i + 1) == Some(&'(') => {
                    self.i += 2;
                    self.nested(')');
                    self.in_word = false;
                }
                '`' if stop == Some('`') => {
                    self.i += 1;
                    self.end_seg();
                    return;
                }
                '`' => {
                    self.i += 1;
                    self.nested('`');
                    self.in_word = false;
                }
                '(' => {
                    depth += 1;
                    self.end_seg();
                    self.i += 1;
                }
                ')' => {
                    self.i += 1;
                    self.end_seg();
                    if depth == 0 && stop == Some(')') {
                        return;
                    }
                    depth = depth.saturating_sub(1);
                }
                ';' | '|' | '&' | '\n' => {
                    self.end_seg();
                    self.i += 1;
                }
                c if c.is_whitespace() => {
                    self.end_word();
                    self.i += 1;
                }
                '>' | '<' => {
                    // Where [`split_segments`] detaches a redirect from the
                    // word before it, so does this reader: `awk>/dev/null`
                    // is `awk` and a redirect.
                    if self.in_word
                        && self
                            .cur
                            .chars()
                            .last()
                            .is_some_and(|l| !matches!(l, '>' | '<'))
                        && !self.cur.chars().all(|d| d.is_ascii_digit())
                    {
                        self.end_word();
                    }
                    self.cur.push(c);
                    self.in_word = true;
                    self.i += 1;
                }
                _ => {
                    self.cur.push(c);
                    self.in_word = true;
                    self.i += 1;
                }
            }
        }
        self.end_seg();
    }
}

/// A `*`/`?` glob over the whole string (`*` spans `/` too, like fnmatch without
/// FNM_PATHNAME).
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0, 0);
    let (mut star, mut mark): (Option<usize>, usize) = (None, 0);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ro(cmd: &str) -> bool {
        classify_command(cmd).read_only
    }

    /// The corpus from the session that shaped the rules: each line was a real
    /// worker prompt, each verdict the one the manager gave.
    #[test]
    fn the_session_corpus_classifies_as_the_manager_did() {
        let corpus: &[(&str, bool)] = &[
            (
                "git status --short --branch && git pull 2>&1 | tail -20",
                false,
            ),
            (
                "git show --stat --format='%h %ad %s' d48f | head -60; echo ----; git log --oneline --all --grep='handoff' -i | head -20",
                true,
            ),
            (
                "echo \"== x ==\"; for f in /tmp/a b; do if [ -e \"$f\" ]; then echo \"PRESENT $f ($(stat -f '%Sm' \"$f\"))\"; else echo MISSING; fi; done; uptime; df -h ~ | tail -1",
                true,
            ),
            (
                "timeout 120 python3 scripts/sat2026_standing.py 2>&1 | tail -40",
                // A script is not a read by default (DEFAULT_PYTHON_ALLOW is
                // empty); `the_python_allowlist_is_opt_in` pins it opted in.
                false,
            ),
            (
                "find ~ -maxdepth 5 -name 'x.sh' -not -path '*/Library/*' 2>/dev/null | head",
                true,
            ),
            ("find ~ -name '*.stage' -delete", false),
            ("rm -rf ~/x/*.stage", false),
            ("ls ~/x | xargs rm", false),
            ("cat a.txt > b.txt", false),
            ("git log -3 && git push origin main", false),
            ("targo --unverified build --release", false),
            ("python3 -c 'import os; os.remove(\"x\")'", false),
            ("git stash list; git branch -a", true),
            ("sed -i '' 's/a/b/' file", false),
            (
                "cd ~/ay && grep -rn 'x' reports/H.md | head -3; git -C ~/veripb-alab log -3 --format='%h' 2>/dev/null || ls -d ~/*veripb* 2>/dev/null",
                true,
            ),
            (
                "ps -Ao pid=,etime=,args= | grep -iE 'bank' | grep -v grep | head; ls /Volumes",
                true,
            ),
            (
                "git log -1 --format='%h %an <%ae> %ad%n%(trailers)' --date=iso 2769; ls ~/ay/proofs | wc -l",
                true,
            ),
            (
                "perl -e 'alarm 110; exec @ARGV' find / -xdev \\( -name 'a.json' -o -name 'b.tsv' \\) -not -path '/System/*' 2>/dev/null; echo \"find-exit=$?\"",
                true,
            ),
            (
                "find benchmarks/sat -type f \\( -name '*.cnf' -o -name '*.cnf.xz' \\) -exec du -k {} + | sort -rn | head -40",
                true,
            ),
            (
                "wc -l $(grep -rl 'x' crates --include='*.rs' | head -3) 2>/dev/null | tail -4",
                true,
            ),
            (
                "tmutil listlocalsnapshots / | head; tmutil destinationinfo | head -8",
                true,
            ),
            ("python3 - <<'EOF'\nimport json\nEOF", false),
        ];
        for (cmd, want) in corpus {
            let v = classify_command(cmd);
            assert_eq!(
                v.read_only, *want,
                "{cmd:?}: expected read_only={want}, got {v:?}"
            );
        }
    }

    /// The reason names the rule, so a notes file explains each hand-off.
    #[test]
    fn reasons_name_the_deciding_rule() {
        assert_eq!(
            classify_command("git log -3 && git push origin main").reason,
            "git push"
        );
        assert_eq!(
            classify_command("cat a.txt > b.txt").reason,
            "redirect > b.txt"
        );
        assert_eq!(classify_command("ls ~/x | xargs rm").reason, "xargs rm");
        assert_eq!(
            classify_command("find ~ -name '*.stage' -delete").reason,
            "find -delete"
        );
        assert_eq!(classify_command("sed -i '' 's/a/b/' file").reason, "sed -i");
        assert_eq!(classify_command("python3 -c 'x'").reason, "python3 -c");
        assert_eq!(
            classify_command("python3 - <<'EOF'\nx\nEOF").reason,
            "python3 heredoc"
        );
        assert_eq!(
            classify_command("find . -exec rm {} \\;").reason,
            "find -exec rm"
        );
        assert_eq!(
            classify_command("./run.sh").reason,
            "./run.sh (a program named by path)"
        );
        assert_eq!(classify_command("run.sh").reason, "run.sh");
        assert_eq!(classify_command("git").reason, "git without a subcommand");
        assert_eq!(classify_command("git stash").reason, "git stash");
        assert_eq!(
            classify_command("git worktree add ../x").reason,
            "git worktree add"
        );
        assert!(classify_command("ls").reason.contains("read-only"));
    }

    /// (b): a `>` inside a quoted string is data; a redirect to /dev/null and a
    /// descriptor dup are not writes; an unquoted `>` is.
    #[test]
    fn quotes_hide_redirect_glyphs_but_not_substitutions() {
        assert!(ro("git log --format='%h <%ae> -> %s' | head"));
        assert!(ro("grep x file 2>/dev/null; ls >/dev/null 2>&1"));
        assert!(!ro("ls >> log.txt"));
        assert!(!ro("ls 2> err.txt"));
        assert!(!ro("ls &> all.txt"));
        // A substitution INSIDE double quotes still runs, so it is still judged.
        assert!(!ro("echo \"now: $(rm -rf x)\""));
        assert!(ro("echo \"now: $(date)\""));
        // A quoted danger word is data.
        assert!(ro("grep -n 'rm -rf' notes.md"));
        assert!(ro("echo \"git push later\""));
    }

    /// (a): the alarm wrapper is stripped, with either quote; any other `perl -e`
    /// is inline code and refused.
    #[test]
    fn the_alarm_idiom_is_a_wrapper_not_a_program() {
        assert!(ro("perl -e \"alarm 30; exec @ARGV\" ls -la"));
        assert!(ro("perl -e 'alarm 30; exec @ARGV' git status"));
        assert!(!ro("perl -e 'alarm 30; exec @ARGV' rm -rf x"));
        assert!(!ro("perl -e 'unlink \"x\"'"));
        assert_eq!(classify_command("perl -e 'print 1'").reason, "perl -e");
    }

    /// (c): danger tokens anywhere, including behind a path and behind a shell
    /// keyword, and the inline-code interpreters.
    #[test]
    fn danger_tokens_anywhere_fail_the_line() {
        assert!(!ro("ls | xargs /bin/rm"));
        assert!(!ro("for f in *; do rm $f; done"));
        assert!(!ro("if true; then cp a b; fi"));
        assert!(!ro("bash -c 'ls'"));
        assert!(!ro("sh -c ls"));
        assert!(!ro("zsh -c 'echo hi'"));
        assert!(!ro("eval ls"));
        assert!(!ro("source ~/.zshrc"));
        assert!(!ro(". ~/.zshrc"));
        assert!(!ro("sudo ls"));
        assert!(!ro("kill -9 123"));
        assert!(!ro("open ."));
        assert!(!ro("curl https://x.y"));
        assert!(!ro("tee out.txt"));
        assert!(!ro("nohup ls &"));
        assert!(!ro("git -C ~/x commit -m x"));
        assert!(!ro("git -c core.pager=cat fetch origin"));
        assert!(!ro("git worktree prune"));
        assert!(!ro("git worktree remove x"));
        assert!(!ro("find . -execdir mv {} /tmp \\;"));
        assert!(!ro("find . -ok rm {} \\;"));
        assert!(!ro("sed -Ei 's/a/b/' f"));
        assert!(!ro("sed --in-place 's/a/b/' f"));
    }

    /// (d): an unknown head is not read-only even with no danger token; the
    /// keyword laundering (`do <cmd>`) is seen through; git needs a read
    /// subcommand; the write FORMS of the read subcommands are refused.
    #[test]
    fn segment_heads_must_be_known_reads() {
        assert!(!ro("./deploy.sh"));
        assert!(!ro("for f in *; do ./x.sh $f; done"));
        assert!(!ro("if true; then exec claude; fi"));
        assert!(ro("for f in *; do ls $f; done"));
        assert!(ro("VAR=1 OTHER=2 ls"));
        assert!(ro("timeout 5 ls; timeout -k 1 5 git status"));
        assert!(ro("git -C ~/x --no-pager log -1"));
        assert!(!ro("git --git-dir=.git status"));
        assert!(ro("git worktree list; git stash list"));
        assert!(!ro("git worktree"));
        assert!(!ro("git branch -D old"));
        assert!(!ro("git branch -m new"));
        assert!(!ro("git tag v1.0"));
        assert!(!ro("git tag -d v1.0"));
        assert!(ro("git tag -l 'v*'"));
        assert!(ro("git tag --contains abc"));
        assert!(!ro("git remote add x url"));
        assert!(ro("git remote -v; git remote show origin"));
        assert!(!ro("git config user.name Bob"));
        assert!(!ro("git config --unset user.name"));
        assert!(ro("git config --get user.name; git config --list"));
        assert!(!ro("tmutil deletelocalsnapshots 2026-01-01"));
        assert!(!ro("tmutil"));
        assert!(!ro("python3 scripts/wipe.py"));
        assert!(!ro("python3 scripts/sat_score_table.py"));
        assert!(!ro("python3 -m http.server"));
        assert!(!ro("python3"));
        // The caller's allowlist replaces the default.
        let allow = ["tools/*.py".to_string()];
        assert!(classify_command_with("python3 tools/audit.py", &allow).read_only);
        assert!(!classify_command_with("python3 scripts/sat2026_standing.py", &allow).read_only);
        assert!(!ro("echo hi; ruby -e 'puts 1'"));
        assert!(ro("[ -d x ] && echo yes || echo no"));
        assert!(ro("[[ -d x ]] && true"));
        assert!(ro("test -f x && cat x"));
        assert!(ro("which rg; type -a git; env | sort"));
        assert!(ro("ls\ncat x"));
        assert!(!ro("ls\nmake"));
        assert!(!ro("ls `rm x`"));
        assert!(!ro(""));
        assert_eq!(classify_command("   ").reason, "empty command");
    }

    /// The write forms an adversarial pass found the built binary calling
    /// read-only, each now refused with the head or rule that decides it, and
    /// the read-only neighbours that must stay reads.
    #[test]
    fn a_wrapper_cannot_launder_the_program_it_runs() {
        assert_eq!(classify_command("ls | xargs touch").reason, "touch");
        assert_eq!(
            classify_command("find . -name '*.log' | xargs -n1 ./deploy.sh").reason,
            "./deploy.sh (a program named by path)"
        );
        assert!(!ro("echo x | xargs python3 evil.py"));
        assert!(!ro("ls | xargs -I {} sh -c 'cat {}'"));
        assert!(ro("ls | xargs -n1 wc -l"));
        assert!(ro("ls | xargs -0 -I {} cat {}"));
        assert!(ro("ls | xargs"));
        assert_eq!(
            classify_command("env FOO=1 ./deploy.sh").reason,
            "./deploy.sh (a program named by path)"
        );
        assert_eq!(classify_command("env touch /tmp/x").reason, "touch");
        assert!(!ro("env -S 'rm x'"));
        assert!(!ro("/usr/bin/env python3 wipe.py"));
        assert!(ro("env FOO=1 ls; env -i PATH=/bin ls; env -u HOME ls"));
        assert!(ro("env | sort"));
        assert!(ro("timeout 5 env FOO=1 git status"));
        assert!(!ro("timeout 5 xargs ./x.sh"));
    }

    #[test]
    fn a_backgrounded_command_is_a_head_and_a_redirect_ampersand_is_not() {
        assert_eq!(
            classify_command("ls & ./deploy.sh").reason,
            "./deploy.sh (a program named by path)"
        );
        assert_eq!(classify_command("ls & touch /tmp/x").reason, "touch");
        assert!(ro("ls & wc -l x"));
        assert!(ro("ls >/dev/null 2>&1 & echo done"));
        assert!(ro("ls &> /dev/null"));
        assert!(!ro("ls &> all.txt"));
        assert!(ro("cat <&3 2>&1"));
    }

    #[test]
    fn a_backtick_inside_double_quotes_still_runs() {
        assert_eq!(classify_command("echo \"now: `rm -rf x`\"").reason, "rm");
        assert!(ro("echo \"now: `date`\""));
        assert_eq!(strip_quotes("echo \"a `date` b\""), "echo \"\" `date` ");
    }

    #[test]
    fn the_alarm_idiom_is_exactly_alarm_then_exec() {
        assert!(!ro(
            "perl -e 'alarm 5; system(\"rm -rf x\"); exec @ARGV' ls"
        ));
        assert_eq!(
            classify_command("perl -e 'alarm 5; unlink \"x\"; exec @ARGV' ls").reason,
            "perl -e"
        );
        assert!(ro("perl -e 'alarm 5 ; exec @ARGV;' ls"));
        assert!(ro("perl -e \"alarm 110;exec @ARGV\" git status"));
        assert!(is_alarm_body("alarm 30; exec @ARGV"));
        assert!(is_alarm_body(" alarm 30 ; exec @ARGV ; "));
        assert!(!is_alarm_body("alarm 30; exec @ARGV; print 1"));
        assert!(!is_alarm_body("alarm; exec @ARGV"));
        assert!(!is_alarm_body("alarm 5; system('x'); exec @ARGV"));
    }

    #[test]
    fn git_branch_creation_and_remote_set_branches_are_writes() {
        assert!(!ro("git branch feature-x"));
        assert!(!ro("git branch -f main HEAD~3"));
        assert!(!ro("git branch --force main HEAD~3"));
        assert!(!ro("git branch --track x origin/x"));
        assert!(!ro("git branch -t x origin/x"));
        assert!(!ro("git branch --set-upstream-to=origin/x"));
        assert!(!ro("git branch -u origin/x"));
        assert!(!ro("git branch --create-reflog x"));
        assert_eq!(
            classify_command("git branch feature-x").reason,
            "git branch (write form)"
        );
        assert!(ro("git branch"));
        assert!(ro(
            "git branch -a; git branch -r; git branch -avv; git branch -vv"
        ));
        assert!(ro("git branch --list 'feat/*'; git branch -l"));
        assert!(ro("git branch --contains abc; git branch --merged main"));
        assert!(ro("git branch --show-current; git branch --points-at HEAD"));
        assert!(ro("git branch --format='%(refname:short)'"));
        assert!(!ro("git remote set-branches origin main"));
        assert!(!ro("git remote set-branches --add origin main"));
        assert!(ro("git remote get-url origin"));
    }

    /// Lane B's review (2026-09-23), measured with a harmless `touch` under
    /// zsh and bash 3.2: the shell's ARITHMETIC expands an array subscript
    /// held in a variable's value, and runs the substitution inside it, from
    /// behind a quote this reader treats as opaque. Every form that
    /// evaluates arithmetic on a non-literal is refused; the literal forms
    /// a read uses stay reads (the negative controls).
    #[test]
    fn arithmetic_that_can_expand_a_subscript_is_not_a_read() {
        for line in [
            "let 'x=a[$(touch M)]'",
            "local -i x=1",
            "[[ 'a[$(touch M)]' -eq 1 ]]",
            "x='a[$(touch M)]'; [[ $x -lt 1 ]]",
            "if [[ $n -ge 2 ]]; then echo big; fi",
            "[[ -v a[x] ]]",
            "[ -v 'a[$(touch M)]' ]",
            "test -v 'a[$(touch M)]'",
            "x='a[$(touch M)]'; echo ${a[x]}",
            "x='a[$(touch M)]'; echo ${#a[x]}",
            "x='a[$(touch M)]'; echo \"${a[$x]}\"",
            "x='a[$(touch M)]'; echo ${y:x}",
            "x='a[$(touch M)]'; echo ${y:0:x}",
            "x='a[$(touch M)]'; echo ${a[@]:x}",
            "x='a[$(touch M)]'; echo $[x]",
            "x='a[$(touch M)]'; echo \"$[x]\"",
            "x='a[$(touch M)]'; a=(1 2); echo $a[x]",
            "echo ${(e)x}",
            "cat <<EOF\n${a[x]}\nEOF",
            "cat <<EOF\n$a[x]\nEOF",
        ] {
            let v = classify_command(line);
            assert!(!v.read_only, "{line:?} read-only ({})", v.reason);
        }
        // Negative controls: literal subscripts and offsets, the default
        // forms, the pattern forms, `[`/`test` comparing integers, `[[`
        // comparing strings, and a quoted here-document body.
        for line in [
            "echo ${PIPESTATUS[0]} ${X[@]} ${#X[*]} ${a[-1]}",
            "echo ${y:1} ${y: -2} ${y:1:3} ${a[@]:1:2}",
            "echo ${X:-default} ${X:+set} ${X:?unset} ${#X} ${#} ${!}",
            "echo ${f%.rs} ${f##*/} ${f/a/b} ${!pre*}",
            "[ \"$n\" -eq 1 ] && echo one; test $n -gt 2",
            "[[ $a == b* ]] && echo match",
            "cat <<'EOF'\n${a[x]} $[x]\nEOF",
            "cat <<EOF\n${HOME} and $USER\nEOF",
            "echo 'a[$x]' \"$HOME\"",
        ] {
            let v = classify_command(line);
            assert!(v.read_only, "{line:?} refused: {}", v.reason);
        }
    }

    /// Git reads honour the repository's config and `.gitattributes`
    /// (`core.fsmonitor`, `diff.external`, a textconv driver), and a
    /// worker in accept-edits can write both — the residual gap the
    /// approval rule's doc names. What IS refused: a repository named on
    /// the line (`--git-dir`, `--work-tree`) and the flags that run the
    /// filter and textconv drivers on purpose. `-C` stays a read: `cd X &&
    /// git status` reaches the same repository.
    #[test]
    fn git_reads_of_a_chosen_repository_or_through_its_drivers_are_refused() {
        for line in [
            "git --git-dir=/tmp/evil status",
            "git --git-dir /tmp/evil log",
            "git --work-tree=/tmp/x diff",
            "git cat-file --filters HEAD:x",
            "git cat-file --textconv HEAD:x",
            "git log -p --textconv -1",
        ] {
            assert!(!ro(line), "{line}");
        }
        assert!(ro("git -C /tmp/x status"));
        assert!(ro("git cat-file -p HEAD:x; git log -p -1"));
    }

    /// The read tools' write forms (lane B's review, each measured or read
    /// from the tool's manual): `git branch -v <name>` creates the branch,
    /// `sysctl` sets with `=`/`-w`, `hostname` sets with an operand or
    /// `-F`, `file -C` writes `magic.mgc`, `tree -R` writes `00Tree.html`,
    /// and `<>` opens a file read-write, creating it.
    #[test]
    fn the_write_forms_of_system_reads_are_refused() {
        for line in [
            "git branch -v newb1",
            "git branch --verbose newb2",
            "git branch -vv nb6",
            "sysctl kern.maxfiles=1",
            "sysctl -w kern.maxfiles=1",
            "hostname evil",
            "hostname -F /tmp/name",
            "file -C -m /tmp/magic",
            "file -bC x",
            "tree -R -H . -L 1",
            "cat <>new.txt",
            "exec 3<>/tmp/x",
        ] {
            assert!(!ro(line), "{line}");
        }
        // Negative controls: the list and query forms.
        for line in [
            "git branch -v; git branch -vv; git branch -av; git branch -v --list 'f*'",
            "sysctl -n hw.ncpu; sysctl kern.maxfiles",
            "hostname; hostname -s 2>/dev/null",
            "file -b x; file --mime-type x",
            "tree -L 2 -a",
            "cat < in.txt",
        ] {
            let v = classify_command(line);
            assert!(v.read_only, "{line} refused: {}", v.reason);
        }
    }

    #[test]
    fn output_file_flags_of_the_read_tools_are_writes() {
        assert_eq!(
            classify_command("sort -o /etc/hosts /etc/hosts").reason,
            "sort -o"
        );
        assert!(!ro("sort --output=x f"));
        assert!(!ro("sort --output x f"));
        assert!(!ro("sort -ro out.txt f"));
        assert!(ro("sort -rn f | head; sort -t: -k2 f; sort -u f"));
        assert_eq!(
            classify_command("uniq in.txt out.txt").reason,
            "uniq <output file>"
        );
        assert!(ro(
            "uniq -c in.txt; sort x | uniq -c | sort -rn; uniq -f 1 in.txt"
        ));
        assert!(!ro("uniq -f 1 in.txt out.txt"));
        assert_eq!(classify_command("tree -o /tmp/out.txt").reason, "tree -o");
        assert!(ro("tree -L 2 -a"));
        assert_eq!(
            classify_command("find . -newer x -fprint /tmp/out").reason,
            "find -fprint"
        );
        assert!(!ro("find . -fprintf /tmp/o '%p\\n'"));
        assert!(!ro("find . -fls /tmp/o"));
        assert!(!ro("find . -fprint0 /tmp/o"));
        assert!(ro("find . -name '*.rs' -print0 | xargs -0 wc -l"));
    }

    #[test]
    fn a_python_path_cannot_step_out_of_the_allowlist() {
        assert!(!ro("python3 scripts/../../tmp/evil_score.py"));
        assert!(
            classify_command("python3 scripts/../../tmp/evil_score.py")
                .reason
                .contains("`..`")
        );
        let allow: Vec<String> = ["scripts/*score*.py", "scripts/*report*.py"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let ro_with = |cmd: &str| classify_command_with(cmd, &allow).read_only;
        assert!(!ro_with("python3 scripts/../../tmp/evil_score.py"));
        assert!(!ro_with("python3 scripts/x/../wipe_report.py"));
        assert!(ro_with("python3 scripts/sat_score_table.py"));
        assert!(ro_with("python3 scripts/sub/report_x.py"));
    }

    /// The awk program and the sed script are read from the raw words: what
    /// the quote-stripped scan cannot see.
    #[test]
    fn awk_programs_and_sed_scripts_are_read_not_dropped() {
        assert!(!ro("awk 'BEGIN{system(\"rm -rf x\")}'"));
        assert!(!ro("awk '{print > \"out\"}' f"));
        assert!(!ro("awk '{print $1 > out}' f"));
        assert!(!ro("awk '{print >> \"out\"}' f"));
        assert!(!ro("awk '{print | \"sh\"}' f"));
        assert!(!ro("awk '{print |& \"sh\"}' f"));
        assert!(!ro("awk -f prog.awk f"));
        assert!(!ro("ls | xargs awk 'BEGIN{system(\"rm x\")}'"));
        assert!(!ro("echo \"$(awk 'BEGIN{system(\"rm x\")}')\""));
        assert!(ro("awk -F: '{print $1}' /etc/passwd"));
        assert!(ro("awk '$3 > 100 {print $1}' f"));
        assert!(ro("awk '$1 > $2' f; awk 'NR>1' f; awk '$3 >= 4' f"));
        assert!(ro("awk '/foo|bar/' f; awk '/a/ || /b/ {print}' f"));
        // A `>` against a variable could be `print > out`: refused on purpose
        // (a read handed over costs a glance; a write approved does not).
        assert!(!ro("awk -v n=3 'NR > n' f"));
        assert!(ro("awk '{print \"a>b|c\"}' f"));
        assert!(ro("awk '{ s += $1 } END { print s / NR }' f"));

        assert!(!ro("sed 's/a/b/w /tmp/out' file"));
        assert!(!ro("sed -n '/x/w out' f"));
        assert!(!ro("sed 's/a/b/e' f"));
        assert!(!ro("sed '1e date' f"));
        assert!(!ro("sed -f script.sed f"));
        assert!(!ro("sed -e 's/a/b/' -e 'w out' f"));
        assert!(!ro("sed -ne 's/a/b/w out' f"));
        assert!(!ro("sed --expression='w out' f"));
        assert_eq!(
            classify_command("sed 's/a/b/w /tmp/out' file").reason,
            "sed s///w (writes a file)"
        );
        assert!(ro("sed -n 's/new/old/p' f"));
        assert!(ro("sed -e 's/a/b/g' -e '/^$/d' f"));
        assert!(ro("sed '1,10p;$!d' f; sed -n '5,$p' f; sed '/^#/d' f"));
        assert!(ro("sed -E 's/(a|b)+/x/' f; sed 's|/usr|/opt|' f"));
        assert!(ro("sed 's/w/e/; s/e/w/g' f"));
        assert!(ro("sed -n '/start/,/end/{p}' f; sed 'y/abc/xyz/' f"));
        assert!(ro("sed '$!N;P;D' f; sed -n '$=' f; sed 2q f"));
        assert!(ro("sed -e '1i\\header' f"));
        assert_eq!(sed_script_writes("s/a/b/"), None);
        assert_eq!(
            sed_script_writes("/x/w out").as_deref(),
            Some("sed w (writes a file)")
        );
        assert!(
            sed_script_writes("s/a/b").is_some(),
            "unterminated is unparsed"
        );
        assert!(
            sed_script_writes("1,3").is_some(),
            "an address without a command is unparsed"
        );
        assert!(
            sed_script_writes("k").is_some(),
            "an unknown command is unparsed"
        );
    }

    #[test]
    fn raw_words_resolve_quotes_and_split_where_the_segments_do() {
        assert_eq!(
            raw_words("awk 'a b' \"c $x\" d\\ e | grep x; echo `date`"),
            vec![
                vec!["awk", "a b", "c $x", "d e"],
                vec!["grep", "x"],
                vec!["echo"],
                vec!["date"],
            ]
        );
        assert_eq!(
            raw_words("echo \"x $(stat -f '%m' \"$f\") y\""),
            vec![
                vec!["echo", "x "],
                vec!["stat", "-f", "%m", "$f"],
                vec![" y"]
            ]
        );
        assert_eq!(
            raw_words("echo \"a \\\" b\" 'c\\d'"),
            vec![vec!["echo", "a \" b", "c\\d"]]
        );
    }

    /// Every line the 2026-09-23 audit (APR-4) measured the shipped
    /// classifier calling read-only, each a write or a program of the line's
    /// choosing. `⏎` in the audit is a newline here. The `rm_policy` corpus
    /// pins the same shapes for an `rm` verdict.
    #[test]
    fn the_measured_bypasses_are_not_read_only() {
        let bypasses = [
            "git log --oneline -3 # what's new\ngit push --force",
            "ls # don't worry\nmv a b",
            "ls # don't\nrm -rf \"$S/x\"",
            "ls # it's\nrm -rf / # '",
            "cat <<EOF\necho '\nEOF\nrm -rf /",
            "echo $'a\\'b'\nrm -rf /\necho '",
            "rm>/dev/null -rf /",
            "ls >&out.txt",
            "git diff --output=/Users//_owner/.zshrc",
            "git log --output=/tmp/x",
            "git -c core.fsmonitor='touch /tmp/pwn' status",
            "git -c core.pager='sh -c \"rm -rf ~\"' log",
            "git -C ~/x -c core.pager=cat log",
            "git grep -O\"touch /tmp/x\" foo",
            "git diff --ext-diff",
            "rg --pre ./x.sh foo",
            "rg --pre=./x.sh foo",
            "sort --compress-program=./x.sh f",
            "/tmp/evil/ls",
            "./ls",
            "env -i PATH=/tmp/evil ls",
            "printf -v PATH /evil",
            "export PATH=/tmp/evil:$PATH; ls",
            "PATH=/tmp/evil ls",
            "GIT_EXTERNAL_DIFF=./x.sh git diff",
            "HOME=/tmp/evil git log",
            "less +!touch\\ x file",
            "less -o copy.txt file",
            "date -s 12:00",
            "date 0101120026",
            "python3 scripts/purge_report.py --delete-all",
            "ls $'x\\ny'",
            "echo ${x:-$(rm -rf y)}",
            "echo \"unterminated",
            "cat <<EOF\n$(rm -rf x)\nEOF",
            "cat <<EOF",
            "awk>/dev/null 'BEGIN{system(\"rm x\")}'",
        ];
        for cmd in bypasses {
            let v = classify_command(cmd);
            assert!(!v.read_only, "{cmd:?} must not be read-only: {v:?}");
        }
    }

    /// The negative controls for the pre-pass: the shapes it READS rather than
    /// refuses stay reads, so the refusals above are about the hazard and not
    /// about the construct.
    #[test]
    fn comments_heredocs_and_quoting_that_only_read_stay_reads() {
        assert!(ro("git log --oneline -3 # what's new"));
        assert!(ro("ls # don't worry\ncat x"));
        assert!(ro("cat <<'EOF'\n$(this is data)\nEOF"));
        assert!(ro("cat <<EOF\nplain body with 'quote\nEOF\nls"));
        assert!(ro("grep -c x <<-EOF\n\tbody\n\tEOF"));
        assert!(ro("cat <<< 'here string'"));
        assert!(ro("echo $'plain'; echo $\"x\""));
        assert!(ro("echo ${HOME} ${x:-y} ${#x}"));
        assert!(ro("echo \"#not a comment\" x#y $#"));
        assert!(ro("ls >&2; ls 2>&1; ls >&-"));
        assert!(ro("ls>/dev/null"));
        // A body under a quoted terminator is never expanded.
        assert_eq!(
            prelex("cat <<'EOF'\n$(rm x)\nEOF\nls").as_deref(),
            Ok("cat <<'EOF'\nls")
        );
        assert_eq!(prelex("ls # a 'b\ncat").as_deref(), Ok("ls \ncat"));
        assert!(prelex("cat <<EOF\n$(rm x)\nEOF").is_err());
        // A here-document still feeds an interpreter its program.
        assert!(!ro("python3 <<'EOF'\nprint(1)\nEOF"));
        assert!(!ro("bash <<'EOF'\nls\nEOF"));
    }

    /// A program word is judged where it runs: as an argument it is data
    /// (APR-7's false escalations), at a head — also behind a wrapper, a
    /// keyword or a substitution — it is the program.
    #[test]
    fn a_program_word_is_judged_only_at_a_head() {
        assert!(ro("grep -rn open src"));
        assert!(ro("rg -n make Cargo.toml"));
        assert!(ro("grep -c kill crates"));
        assert!(ro("git log --author make"));
        assert!(ro("echo rm; grep 'rm -rf' notes.md"));
        assert!(!ro("open src"));
        assert!(!ro("ls; make"));
        assert!(!ro("ls | xargs kill"));
        assert!(!ro("timeout 5 rm x"));
        assert!(!ro("env rm x"));
        assert!(!ro("for f in *; do make; done"));
        assert!(!ro("echo $(make)"));
        assert!(!ro("ls & open ."));
        assert_eq!(classify_command("ls; make").reason, "make");
    }

    #[test]
    fn the_added_reads_are_reads_and_their_neighbours_are_not() {
        assert!(ro(
            "pgrep -f aterm; sleep 5; strings /bin/ls | head; lsof -p 1"
        ));
        assert!(ro("uname -a; whoami; id; sw_vers; fold -w 80 f"));
        assert!(ro("trustc -vV; rustc --version; trustc --print sysroot"));
        assert!(!ro("trustc x.rs"));
        assert!(!ro("trustc -vV x.rs"));
        assert!(!ro("trustc"));
        assert!(!ro("pkill aterm"));
        assert!(ro("env -i PATH=/bin:/usr/bin ls"));
        assert!(ro(
            "date +%s; date -u; date -r 1700000000; date -j -f %s 1 +%Y"
        ));
        assert!(ro("less -R f; more f; printf '%s\\n' x"));
        assert!(ro("git log -3 --format=%h; git -C ~/x --no-pager log -1"));
        assert!(ro("rg --pre-glob '*.gz' x; sort -rn f"));
        assert!(ro("/bin/ls; /usr/bin/wc -l f"));
        assert!(!ro("/bin/x/ls"));
        assert!(ro("FOO=1 ls; LC_ALL=C sort f"));
        assert!(ro("date 2>/dev/null; date +%s 2> /dev/null"));
        assert!(!ro("date 2>/dev/null 0101120026"));
        // `time` is seen through like `timeout`, and an assignment after a
        // keyword is an assignment.
        assert!(ro("time ls; time -p git status"));
        assert!(!ro("time rm x"));
        assert!(ro("for d in *; do n=$(ls $d | wc -l); echo $n; done"));
        assert!(!ro("for d in *; do n=$(rm $d); done"));
    }

    /// `aterm ctl` is read-only by verb AND form: `meta` reads and `meta set`
    /// writes the owner's badge; `inbox` reads and `inbox seen` writes.
    #[test]
    fn aterm_forms_are_judged_by_their_full_subform() {
        for read in [
            "aterm ctl status",
            "aterm ctl @s-1 status",
            "aterm ctl --sock /tmp/a.sock @s-1 text",
            "aterm ctl ls",
            "aterm ctl windows",
            "aterm ctl help",
            "aterm ctl @s-1 meta",
            "aterm ctl @s-1 inbox",
            "aterm ctl @s-1 inbox get 3",
            "aterm ctl turn --help",
            "aterm-ctl status",
            "aterm --version",
            "aterm help introspection",
            "aterm pkg list",
            "aterm pkg which claude",
            "aterm pkg doctor",
            "aterm drive phase",
            "aterm drive classify -- 'ls'",
        ] {
            assert!(ro(read), "{read:?}: {:?}", classify_command(read));
        }
        for write in [
            "aterm ctl @s-1 meta set attention hi",
            "aterm ctl @s-1 meta unset attention",
            "aterm ctl @s-1 inbox seen 3 handled",
            "aterm ctl @s-1 key 1",
            "aterm ctl @s-1 send x",
            "aterm ctl @s-1 turn 'go'",
            "aterm ctl @s-1 image /tmp/x.png",
            "aterm ctl @s-1 post to=@s-2 kind=note hi",
            "aterm ctl",
            "aterm pkg install x",
            "aterm pkg repair",
            "aterm drive watch",
            "aterm",
            "aterm ctl status > /tmp/out",
        ] {
            assert!(!ro(write), "{write:?} must not be read-only");
        }
    }

    #[test]
    fn the_python_allowlist_is_opt_in() {
        assert!(DEFAULT_PYTHON_ALLOW.is_empty());
        assert!(!ro("python3 scripts/x_report.py"));
        let allow = ["scripts/*standing*.py".to_string()];
        assert!(
            classify_command_with(
                "timeout 120 python3 scripts/sat2026_standing.py 2>&1 | tail -40",
                &allow
            )
            .read_only
        );
    }

    #[test]
    fn glob_matches_like_fnmatch() {
        assert!(glob_match(
            "scripts/*standing*.py",
            "scripts/sat2026_standing.py"
        ));
        assert!(glob_match("scripts/*report*.py", "scripts/a/b/report_x.py"));
        assert!(!glob_match("scripts/*standing*.py", "scripts/standing.txt"));
        assert!(glob_match("?.py", "a.py"));
        assert!(!glob_match("?.py", "ab.py"));
        assert!(glob_match("*", ""));
        assert!(!glob_match("a", ""));
        assert!(glob_match("a*b*c", "aXXbYYc"));
        assert!(!glob_match("a*b*c", "aXXbYY"));
    }

    #[test]
    fn strip_quotes_keeps_shape() {
        assert_eq!(strip_quotes("echo 'a b' \"c\""), "echo \"\" \"\"");
        assert_eq!(
            strip_quotes("find \\( -name 'x' \\)"),
            "find ( -name \"\" )"
        );
        assert_eq!(
            strip_quotes("echo \"x $(stat -f '%m' \"$f\")\""),
            "echo \"\" $(stat -f \"\" \"\") "
        );
        assert_eq!(strip_quotes("find . \\;"), "find . \\;");
        assert_eq!(strip_quotes("echo 'unterminated"), "echo \"\"");
    }

    #[test]
    fn split_segments_sees_every_separator() {
        let segs = split_segments("a; b && c || d | e $(f) (g) `h`\ni \\; j & k 2>&1 &>l");
        let heads: Vec<&str> = segs.iter().map(|s| s[0].as_str()).collect();
        assert_eq!(heads, ["a", "b", "c", "d", "e", "f", "g", "h", "i", "k"]);
        assert_eq!(segs[8], ["i", "\\;", "j"]);
        assert_eq!(segs[9], ["k", "2>&1", "&>l"]);
    }
}
