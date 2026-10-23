// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pure, fail-closed shell command read-only classifier for supervisor auto-approval.

/// Read-only classification verdict and deciding rule for a shell command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    /// `true` when every segment is a known read-only command and no danger token appears.
    pub read_only: bool,
    /// Rule that decided the verdict (`"every segment read-only"` or the offending token/head).
    pub reason: String,
}

impl Verdict {
    /// Not read-only, for `reason`; a substitution token ([`SUBSTITUTION`],
    /// [`QUOTED_SUBSTITUTION`]) is shown as `$(…)`, a quoted parameter
    /// ([`QUOTED_PARAMETER`]) as `"$…"`.
    fn no(reason: impl Into<String>) -> Self {
        Self {
            read_only: false,
            reason: reason
                .into()
                .replace(QUOTED_SUBSTITUTION, "$(…)")
                .replace(QUOTED_PARAMETER, "\"$…\"")
                .replace(SUBSTITUTION, "$(…)"),
        }
    }
}

/// Placeholder token representing an unquoted command or backtick substitution.
pub(crate) const SUBSTITUTION: &str = "$…";

/// Placeholder token representing a double-quoted command substitution.
pub(crate) const QUOTED_SUBSTITUTION: &str = "$“…”";

/// Placeholder token representing a double-quoted parameter expansion.
pub(crate) const QUOTED_PARAMETER: &str = "$“p”";

/// The private marker [`strip_quotes`] writes before a substitution found inside
/// double quotes, so [`split_segments`] reads it as [`QUOTED_SUBSTITUTION`].
const QUOTE_SUB_MARK: char = '\u{1}';

/// Maximum nesting depth for command substitutions before [`prelex`] refuses the line.
pub(crate) const MAX_SUBSTITUTION_DEPTH: usize = 16;

/// Why an unquoted `(` inside a word is no read: zsh reads it as a glob group or
/// qualifier, and the `e:…:` and `+cmd` qualifiers run a command during
/// expansion (`ls -d /(e:'print -u2 X':)`, `echo $(print /)(e:…:)`, `ls -d
/// /(+print)`).
const GLOB_QUALIFIER_RUNS: &str =
    "a ( inside a word (zsh reads a glob qualifier, and `(e:…:)` or `(+cmd)` runs a command)";

/// Why zsh's `~` parameter flag is no read: `$~N`, `$^~N`, `$~^N` (and `${~N}`,
/// refused by [`braced_is_literal`]) glob the value as a pattern, whose
/// qualifier runs a command (`N='/(e:print -u2 X:)'; echo $~N`).
const GLOB_SUBST_RUNS: &str = "zsh's $~ flag (the value is globbed as a pattern, and a qualifier `(e:…:)` in it runs a command)";

/// Why an arithmetic evaluation, `$((…))` or the `((…))` command, is no read:
/// bash and zsh assign any variable in it, the second and later of a list too,
/// and evaluate a variable's value as arithmetic, whose subscript runs a
/// substitution.
const ARITHMETIC_RUNS: &str = "an arithmetic evaluation ($((…)) or ((…)): it may assign any variable, PATH among \
     them, and a value's subscript runs a substitution)";

/// Default `--allow-python` glob list (empty).
pub const DEFAULT_PYTHON_ALLOW: &[&str] = &[];

/// Programs a segment may start with and still be a read. Shell keywords are
/// here because a `for`/`if` segment's head is the keyword; the command after
/// `do`/`then` is checked too ([`segment_head`]).
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

/// Keyword heads that merely prefix the command a segment really runs: `do rm x`
/// is `rm x`.
const KEYWORD_PREFIX: &[&str] = &[
    "do", "then", "else", "if", "elif", "while", "until", "!", "{",
];

/// Whether a word closes a `{` group in zsh: as itself or glued to a word's end, unless the
/// word's own braces open it.
fn closes_a_brace(tok: &str) -> bool {
    let mut depth = 0usize;
    let mut chars = tok.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                chars.next();
            }
            '{' => depth += 1,
            '}' if chars.peek().is_none() => return depth == 0,
            '}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    false
}

/// The directories a program may be named from by path and still be the program
/// its basename says: `/bin/ls` is `ls`, `/tmp/evil/ls` is not.
const SYSTEM_BIN_DIRS: &[&str] = &["/bin/", "/usr/bin/", "/sbin/", "/usr/sbin/"];

/// Variables whose assignment changes what a later program runs or reads: the search path,
/// shell startup and splitting, pagers and editors a read tool may exec, git and rg config
/// roots.
const HAZARD_VARS: &[&str] = &[
    "PATH",
    "NULLCMD",
    "READNULLCMD",
    "MODULE_PATH",
    "STTY",
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

/// zsh's tied pairs, array then scalar: assigning the array assigns the scalar
/// (`path=/tmp/x ls` runs `/tmp/x/ls`).
const ZSH_TIED: &[(&str, &str)] = &[
    ("cdpath", "CDPATH"),
    ("fignore", "FIGNORE"),
    ("fpath", "FPATH"),
    ("mailpath", "MAILPATH"),
    ("manpath", "MANPATH"),
    ("module_path", "MODULE_PATH"),
    ("path", "PATH"),
    ("psvar", "PSVAR"),
    ("zsh_eval_context", "ZSH_EVAL_CONTEXT"),
];

/// The variables whose value zsh 5.9 runs, loads or sources during an ordinary
/// line of reads, found by assigning each declared or documented name a marker
/// program, function, command line and directory.
#[cfg(test)]
const ZSH_RUNS_A_VALUE: &[&str] = &[
    "MODULE_PATH",
    "NULLCMD",
    "PATH",
    "READNULLCMD",
    "module_path",
    "path",
];

/// `git <sub>` reads. `worktree` and `stash` are read-only only as `list`.
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

/// Classify `cmd` using [`DEFAULT_PYTHON_ALLOW`].
pub fn classify_command(cmd: &str) -> Verdict {
    classify_command_with(cmd, DEFAULT_PYTHON_ALLOW)
}

/// Classify `cmd`, permitting `python3 <path>` when `<path>` matches `python_allow`.
pub fn classify_command_with<S: AsRef<str>>(cmd: &str, python_allow: &[S]) -> Verdict {
    let cmd = match prelex(cmd) {
        Ok(cmd) => cmd,
        Err(reason) => return Verdict::no(reason),
    };
    let cmd = strip_alarm_idiom(&cmd);
    let stripped = strip_quotes(&cmd);
    let segments = split_segments(&stripped);
    if segments.is_empty() {
        return Verdict::no("empty command");
    }

    if let Some(reason) = danger_scan(&segments) {
        return Verdict::no(reason);
    }
    if let Some(reason) = program_scan(&cmd) {
        return Verdict::no(reason);
    }
    // The flag hazards on the shell's argv words, where a quote or a backslash
    // hid a flag from the quote-stripped scans (`find '-delete'`, `sort '-o' f`).
    if let Some(reason) = argv_flag_hazard(&cmd) {
        return Verdict::no(reason);
    }
    for seg in &segments {
        if let Some(reason) = segment_head(seg, python_allow) {
            return Verdict::no(reason);
        }
    }
    if let Some(reason) = arithmetic_scan(&cmd) {
        return Verdict::no(reason);
    }
    Verdict {
        read_only: true,
        reason: "every segment read-only".to_string(),
    }
}

/// Classify every segment of `cmd` except `rm` segments.
pub(crate) fn classify_except_rm<S: AsRef<str>>(cmd: &str, python_allow: &[S]) -> Verdict {
    let lexed = match prelex(cmd) {
        Ok(cmd) => cmd,
        Err(reason) => return Verdict::no(reason),
    };
    let lexed = strip_alarm_idiom(&lexed);
    let (segments, left_out): (Vec<Vec<String>>, Vec<Vec<String>>) =
        split_segments(&strip_quotes(&lexed))
            .into_iter()
            .partition(|seg| {
                let head = seg.iter().find(|t| !is_assignment(t));
                match head {
                    None => false,
                    Some(h) => {
                        let p = program(h).to_ascii_lowercase();
                        p != "rm" && p != "mktemp"
                    }
                }
            });
    if let Some(reason) = left_out
        .iter()
        .flat_map(|seg| seg.iter().take_while(|t| is_assignment(t)))
        .find_map(|t| assignment_hazard(t))
    {
        return Verdict::no(reason);
    }
    if let Some(reason) = left_out
        .iter()
        .find_map(|seg| (0..seg.len()).find_map(|i| redirect_hazard(seg, i)))
    {
        return Verdict::no(reason);
    }
    if let Some(reason) = danger_scan(&segments) {
        return Verdict::no(reason);
    }
    if let Some(reason) = program_scan(&lexed) {
        return Verdict::no(reason);
    }
    // The flag hazards on the argv words a quote or a backslash hid, over the
    // same segments the resolver left for this check (the rm and mktemp
    // commands, judged by the resolver, are skipped as they are above).
    if let Some(reason) = program_words(&lexed)
        .iter()
        .filter(|seg| {
            seg.iter().find(|t| !is_assignment(t)).is_none_or(|h| {
                let p = program(h).to_ascii_lowercase();
                p != "rm" && p != "mktemp"
            })
        })
        .find_map(|seg| seg_flag_hazard(seg))
    {
        return Verdict::no(reason);
    }
    for seg in &segments {
        if is_mktemp_guard(seg) {
            continue;
        }
        if let Some(reason) = segment_head(seg, python_allow) {
            return Verdict::no(reason);
        }
    }
    if let Some(reason) = arithmetic_scan(&lexed) {
        return Verdict::no(reason);
    }
    Verdict {
        read_only: true,
        reason: "every segment but the rm and mktemp commands read-only".to_string(),
    }
}

/// Verify that `line` uses only plain zsh grammar whose expansions behave as text or paths.
pub(crate) fn plain_zsh_line(line: &str) -> Result<(), String> {
    let chars: Vec<char> = line.chars().collect();
    plain_zsh(&chars, 0)
}

/// How deep [`plain_dollar`] follows a `$(…)` inside a `$(…)`.
const PLAIN_NEST: u8 = 8;

/// [`plain_zsh_line`] over `chars`, `depth` command substitutions in: inside one (`depth > 0`) a
/// newline and a comment are refused too, so its `)` is the one zsh closes it with (a `#` opens a
/// comment inside `$(…)` that can hide a `)` from this reader).
fn plain_zsh(chars: &[char], depth: u8) -> Result<(), String> {
    #[derive(PartialEq)]
    enum Quote {
        None,
        Single,
        Double,
    }
    let boundary = |c: Option<&char>| c.is_none_or(|c| " \t\n;|&".contains(*c));
    let mut quote = Quote::None;
    let mut word = String::new();
    let mut quoted = false;
    let mut target = false;
    let mut command = true;
    let mut end_word = |word: &mut String, quoted: &mut bool, target: &mut bool, separator| {
        let whole = !word.is_empty() || *quoted;
        if whole && command && !*quoted && ZSH_RESERVED.contains(&word.as_str()) {
            return Err(format!("the reserved word `{word}`"));
        }
        command = separator || (command && (!whole || *target));
        *target &= !whole && !separator;
        word.clear();
        *quoted = false;
        Ok(())
    };
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let at_start = word.is_empty() && !quoted;
        if c.is_control() && c != '\t' && c != '\n' {
            return Err(format!("the control character `{}`", c.escape_default()));
        }
        if depth > 0 && c == '\n' && quote == Quote::None {
            return Err("a newline inside `$(…)`".into());
        }
        match quote {
            Quote::Single => {
                if c == '\'' {
                    quote = Quote::None;
                }
            }
            Quote::Double => match c {
                '"' => quote = Quote::None,
                '$' => {
                    i = plain_dollar(chars, i, depth)?;
                    continue;
                }
                '`' | '\\' => return Err(format!("`{c}` inside double quotes")),
                _ => {}
            },
            Quote::None => match c {
                '\'' => (quote, quoted) = (Quote::Single, true),
                '"' => (quote, quoted) = (Quote::Double, true),
                ' ' | '\t' => end_word(&mut word, &mut quoted, &mut target, false)?,
                '\n' | ';' | '|' | '&' => end_word(&mut word, &mut quoted, &mut target, true)?,
                '#' if at_start => {
                    if depth > 0 {
                        return Err("a comment inside `$(…)`".into());
                    }
                    while i < chars.len() && chars[i] != '\n' {
                        i += 1;
                    }
                    continue;
                }
                '$' => {
                    i = plain_dollar(chars, i, depth)?;
                    word.push('$');
                    continue;
                }
                '`' | '\\' | '(' | ')' | '{' | '}' => return Err(format!("`{c}`")),
                '[' if at_start && chars.get(i + 1) == Some(&'[') => return Err("`[[`".into()),
                '=' if at_start => return Err("a word opening with `=`".into()),
                '=' if !word.starts_with('-') => return Err(format!("the assignment `{word}=`")),
                '~' if !(at_start
                    && (chars.get(i + 1) == Some(&'/') || boundary(chars.get(i + 1)))) =>
                {
                    return Err("a `~` other than `~` or `~/`".into());
                }
                '<' => {
                    if !at_start || chars.get(i + 1).is_some_and(|n| "<>&(".contains(*n)) {
                        return Err("a `<` other than a plain input redirect".into());
                    }
                    target = true;
                }
                '>' => {
                    let mut j = i + 1;
                    if chars.get(j) == Some(&'&') {
                        j += 1;
                        let digits = j;
                        while chars.get(j).is_some_and(char::is_ascii_digit) {
                            j += 1;
                        }
                        if j == digits || !boundary(chars.get(j)) {
                            return Err("a `>&` onto other than a descriptor".into());
                        }
                    } else {
                        while chars.get(j).is_some_and(|c| *c == ' ' || *c == '\t') {
                            j += 1;
                        }
                        let onto: String = chars.iter().skip(j).take(9).collect();
                        if onto != "/dev/null" || !boundary(chars.get(j + 9)) {
                            return Err("a redirect onto other than /dev/null".into());
                        }
                        j += 9;
                    }
                    if !quoted && word.bytes().all(|b| b.is_ascii_digit()) {
                        word.clear();
                    }
                    end_word(&mut word, &mut quoted, &mut target, false)?;
                    i = j;
                    continue;
                }
                _ => word.push(c),
            },
        }
        i += 1;
    }
    if quote != Quote::None {
        return Err("an unterminated quote".into());
    }
    end_word(&mut word, &mut quoted, &mut target, true)
}

/// The index past the `$` form at `chars[at]` (a `$`) where zsh expands it plainly, quoted or
/// not: a parameter read `$NAME` or `${NAME}` (an ASCII name, a letter or `_` first), or a
/// command substitution `$(…)` whose text is itself plain ([`plain_zsh`], one level deeper:
/// what it runs is the classifier's to judge).
fn plain_dollar(chars: &[char], at: usize, depth: u8) -> Result<usize, String> {
    let name_from = |from: usize| -> Option<usize> {
        let first = chars.get(from)?;
        if !(first.is_ascii_alphabetic() || *first == '_') {
            return None;
        }
        Some(
            (from..chars.len())
                .find(|&k| !(chars[k].is_ascii_alphanumeric() || chars[k] == '_'))
                .unwrap_or(chars.len()),
        )
    };
    let end = match chars.get(at + 1) {
        Some('(') if chars.get(at + 2) == Some(&'(') => {
            return Err("an arithmetic `$((`".into());
        }
        Some('(') => {
            if depth >= PLAIN_NEST {
                return Err("command substitutions nested too deep".into());
            }
            let close = substitution_close(chars, at + 2, 0)?;
            plain_zsh(&chars[at + 2..close], depth + 1)
                .map_err(|why| format!("inside `$(…)`: {why}"))?;
            close + 1
        }
        Some('{') => match name_from(at + 2) {
            Some(end) if chars.get(end) == Some(&'}') => end + 1,
            _ => return Err("a `${…}` other than `${NAME}`".into()),
        },
        _ => name_from(at + 1)
            .ok_or_else(|| "a `$` other than `$NAME`, `${NAME}` or `$(…)`".to_string())?,
    };
    match chars.get(end) {
        Some(c) if !c.is_ascii() || "[:({".contains(*c) => Err(format!(
            "`{c}` right after an expansion (a subscript, a modifier or a qualifier under zsh)"
        )),
        _ => Ok(end),
    }
}

/// The index of the `)` that closes the `$(` whose text starts at `from`: the first `)` outside
/// quotes and outside any `$(…)` within, `nested` deep.
fn substitution_close(chars: &[char], from: usize, nested: u8) -> Result<usize, String> {
    if nested >= PLAIN_NEST {
        return Err("command substitutions nested too deep".into());
    }
    let mut quote: Option<char> = None;
    let mut k = from;
    while k < chars.len() {
        let c = chars[k];
        match (quote, c) {
            (Some('\''), '\'') | (Some('"'), '"') => quote = None,
            (Some('\''), _) => {}
            (_, '`' | '\\') => return Err(format!("`{c}` inside `$(…)`")),
            (_, '$') if chars.get(k + 1) == Some(&'(') => {
                if chars.get(k + 2) == Some(&'(') {
                    return Err("an arithmetic `$((`".into());
                }
                k = substitution_close(chars, k + 2, nested + 1)?;
            }
            (None, '\'' | '"') => quote = Some(c),
            (None, ')') => return Ok(k),
            _ => {}
        }
        k += 1;
    }
    Err("a `$(` with no `)`".into())
}

/// zsh 5.9's reserved words (`reswords`, `{`, `}` and `[[` aside, which a
/// plain line never holds): where one opens a command the line is a compound
/// or a declaration, not a simple command ([`plain_zsh_line`]).
const ZSH_RESERVED: &[&str] = &[
    "!",
    "case",
    "coproc",
    "declare",
    "do",
    "done",
    "elif",
    "else",
    "end",
    "esac",
    "export",
    "fi",
    "float",
    "for",
    "foreach",
    "function",
    "if",
    "integer",
    "local",
    "nocorrect",
    "readonly",
    "repeat",
    "select",
    "then",
    "time",
    "typeset",
    "until",
    "while",
];

/// A segment the rm rule's resolver (`supervise::policy::rm_breaker`) reads as a guard on a
/// `$(mktemp -d)`: `exit` or `exit <n>` (`D=$(mktemp -d) || exit 1`), and `:` with its words
/// (`: "${D:?}"`).
fn is_mktemp_guard(seg: &[String]) -> bool {
    match seg {
        [head, ..] if head == ":" => true,
        [head] => head == "exit",
        [head, n] => head == "exit" && !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()),
        _ => false,
    }
}

/// Strip `#` comments and non-expanding here-doc bodies, and refuse complex substitutions.
pub(crate) fn prelex(src: &str) -> Result<String, String> {
    let mut lx = Prelex {
        chars: src.chars().collect(),
        i: 0,
        out: String::with_capacity(src.len()),
        heredocs: Vec::new(),
        depth: 0,
        backticks: 0,
        escaped_to: 0,
    };
    lx.run(None)?;
    if !lx.heredocs.is_empty() {
        // `cat <<EOF` with no newline after it: the shell reads the body from
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
    /// How many substitutions the reader is inside.
    depth: usize,
    /// How many of those are backtick substitutions.
    backticks: usize,
    /// The position right after the last character a backslash escaped
    /// (`0`: none yet), where no word starts ([`Self::at_word_start`]).
    escaped_to: usize,
}

impl Prelex {
    fn peek(&self, k: usize) -> Option<char> {
        self.chars.get(self.i + k).copied()
    }

    /// Whether position `i` starts a word: the shell's rule for where `#`
    /// opens a comment.
    fn at_word_start(&self) -> bool {
        self.i == 0
            || (self.escaped_to != self.i
                && (self.chars[self.i - 1].is_whitespace()
                    || matches!(self.chars[self.i - 1], ';' | '&' | '|' | '(' | ')')))
    }

    /// Whether the unquoted `(` at the cursor is part of a word (after a letter,
    /// quote, `)`, `}`, glob, `~`, `=` or `<…>` range), which zsh reads as a glob
    /// group or qualifier ([`GLOB_QUALIFIER_RUNS`]).
    fn glued_paren(&self) -> bool {
        let boundary = |k: usize| {
            k == 0 || {
                let p = self.chars[k - 1];
                p.is_whitespace() || matches!(p, ';' | '&' | '|' | '(')
            }
        };
        if boundary(self.i) {
            return false;
        }
        let p = self.chars[self.i - 1];
        !(matches!(p, '<' | '>' | '=') && boundary(self.i - 1))
    }

    /// One substitution deeper: its body, read by [`Self::run`] up to and
    /// including `stop`, or refused past [`MAX_SUBSTITUTION_DEPTH`].
    fn substitution(&mut self, stop: char) -> Result<(), String> {
        if self.depth >= MAX_SUBSTITUTION_DEPTH {
            return Err(format!(
                "command substitutions nested deeper than {MAX_SUBSTITUTION_DEPTH}"
            ));
        }
        let backtick = usize::from(stop == '`');
        self.depth += 1;
        self.backticks += backtick;
        self.run(Some(stop))?;
        self.depth -= 1;
        self.backticks -= backtick;
        Ok(())
    }

    /// At a backslash inside a backtick substitution: the shell rereads the
    /// body with `\`` as a backtick, so `` `echo \`rm x\`` `` runs `rm x`.
    fn nested_backtick(&self) -> Result<(), String> {
        if self.backticks > 0 && self.peek(1) == Some('`') {
            return Err("a backtick substitution nested in another (\\`)".to_string());
        }
        Ok(())
    }

    /// One level: the top (`stop` none), a `$(…)` (`)`), or a backtick
    /// substitution.
    fn run(&mut self, stop: Option<char>) -> Result<(), String> {
        let mut depth = 0usize;
        while let Some(c) = self.peek(0) {
            match c {
                '\\' => {
                    self.nested_backtick()?;
                    self.out.push(c);
                    self.i += 1;
                    if let Some(n) = self.peek(0) {
                        self.out.push(n);
                        self.i += 1;
                        self.escaped_to = self.i;
                    }
                }
                '\'' => self.single_quoted()?,
                '"' => self.double_quoted()?,
                '$' if self.peek(1) == Some('\'') => {
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
                    self.i += 1;
                    self.double_quoted()?;
                }
                '$' if self.peek(1) == Some('{') => self.braced_parameter()?,
                '$' if self.peek(1) == Some('(') && self.peek(2) == Some('(') => {
                    return Err(ARITHMETIC_RUNS.to_string());
                }
                '$' if self.peek(1) == Some('(') => {
                    self.out.push_str("$(");
                    self.i += 2;
                    self.substitution(')')?;
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
                    self.substitution('`')?;
                    self.out.push('`');
                }
                '(' if self.glued_paren() => {
                    return Err(GLOB_QUALIFIER_RUNS.to_string());
                }
                '(' if self.peek(1) == Some('(') => {
                    return Err(ARITHMETIC_RUNS.to_string());
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
                    while self.peek(0).is_some_and(|d| d != '\n') {
                        self.i += 1;
                    }
                }
                '<' if self.peek(1) == Some('<') && self.peek(2) == Some('<') => {
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
                    self.nested_backtick()?;
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
                '$' if self.peek(1) == Some('(') && self.peek(2) == Some('(') => {
                    return Err(ARITHMETIC_RUNS.to_string());
                }
                '$' if self.peek(1) == Some('(') => {
                    self.out.push_str("$(");
                    self.i += 2;
                    self.substitution(')')?;
                    self.out.push(')');
                }
                '$' if self.peek(1) == Some('{') => self.braced_parameter()?,
                '$' => self.bare_parameter()?,
                '`' => {
                    self.out.push('`');
                    self.i += 1;
                    self.substitution('`')?;
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
    /// is a form [`Self::parameter_hazard`] refuses.
    fn bare_parameter(&mut self) -> Result<(), String> {
        if let Some(why) = self.parameter_hazard() {
            return Err(why);
        }
        self.out.push('$');
        self.i += 1;
        Ok(())
    }

    /// Whether the `$` at the cursor opens an arithmetic evaluation (`$[x]`, or
    /// zsh's unbraced subscript `$name[x]` behind any flags or on a special
    /// parameter, [`dollar_arithmetic`]), whose subscript runs a substitution
    /// held in the value (`x='a[$(touch M)]'; echo $[x]`), or carries zsh's `~`
    /// flag ([`GLOB_SUBST_RUNS`]).
    fn parameter_hazard(&self) -> Option<String> {
        let after = &self.chars[self.i + 1..];
        if after
            .iter()
            .take_while(|c| ZSH_PARAMETER_FLAGS.contains(**c))
            .any(|&c| c == '~')
        {
            return Some(GLOB_SUBST_RUNS.to_string());
        }
        dollar_arithmetic(after).map(str::to_string)
    }

    /// `${…}`: copied when it holds no quote, substitution or nested `${` (every
    /// form a read uses: `${VAR}`, `${VAR:-x}`, `${#VAR}`), when every
    /// arithmetic part is a literal ([`braced_is_literal`]; subscripts and
    /// substring offsets are evaluated, so `x='a[$(touch M)]'; echo ${a[x]}`
    /// runs it), and when no `$` in an operand is a form refused bare
    /// ([`Self::parameter_hazard`]; `${x:-$path[A]}` and `${x:-$~N}` run code).
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
                '$' => {
                    if let Some(why) = self.parameter_hazard() {
                        return Err(format!("{why}, inside a ${{…}} expansion"));
                    }
                    self.i += 1;
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

    /// At a newline: consume the queued bodies up to and including their
    /// terminator lines, dropping them from the output.
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

/// The arithmetic parts of a `${…}` body are literals: its subscript is an
/// integer, `@` or `*`, and a substring offset or length is an integer.
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
        if body == "!#" {
            // bash's `${!#}` is the last positional parameter; its value is
            return Ok(());
        }
        if body.starts_with('!') {
            let name = ["[@]", "[*]", "@", "*"]
                .iter()
                .find_map(|listing| r.strip_suffix(listing));
            return match name {
                Some(n) if is_name(n) => Ok(()),
                _ => refuse("an indirection (bash expands the parameter its value names)"),
            };
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
    let name = &rest[..name_len];
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
    if rest.starts_with('@') {
        // bash 4.4's `${NAME@OP}`; `@P` expands the value as a prompt string,
        return refuse("a transformation (bash's @P expands the value as a prompt)");
    }
    if let Some(r) = rest.strip_prefix(':')
        && !r.starts_with(['-', '=', '+', '?'])
    {
        // `${name:offset[:length]}`: both are arithmetic.
        if !r.split(':').take(2).all(int) || r.split(':').count() > 2 {
            return refuse("a substring offset that is not a literal (evaluated as arithmetic)");
        }
    }
    if let Some(value) = rest.strip_prefix(":=").or_else(|| rest.strip_prefix('='))
        && let Some(why) = assignment_hazard(&format!("{name}={value}"))
    {
        return refuse(&format!("a default assignment, {why}"));
    }
    Ok(())
}

/// zsh's parameter flags, any run of which may stand between a `$` and the
/// name (`$=V`, `$^V`, `$+V`, `$#V`, `$~V`).
const ZSH_PARAMETER_FLAGS: &str = "=^+#~";

/// Whether the text after a `$` (`after`) opens an arithmetic evaluation: `$[x]`,
/// or zsh's unbraced subscript: any run of flags ([`ZSH_PARAMETER_FLAGS`]), then
/// a name, one special parameter (`*`, `@`, `?`, `-`, `!`, `$`, `#`) or neither,
/// then `[`.
fn dollar_arithmetic(after: &[char]) -> Option<&'static str> {
    if after.first() == Some(&'[') {
        return Some("a $[…] arithmetic expansion");
    }
    let name_char = |c: &char| c.is_ascii_alphanumeric() || *c == '_';
    let mut k = after
        .iter()
        .take_while(|c| ZSH_PARAMETER_FLAGS.contains(**c))
        .count();
    if after.get(k).is_some_and(name_char) {
        k += after[k..].iter().take_while(|c| name_char(c)).count();
    } else if after
        .get(k)
        .is_some_and(|c| matches!(c, '*' | '@' | '?' | '-' | '!' | '$' | '#'))
    {
        k += 1;
    }
    (k > 0 && after.get(k) == Some(&'['))
        .then_some("a $name[…] subscript (zsh evaluates it as arithmetic)")
}

/// The arithmetic an unquoted here-document body expands is literal: its `${…}`
/// forms pass [`braced_is_literal`], and no `$` opens `$[…]` or a subscript
/// ([`dollar_arithmetic`]).
fn body_arithmetic_is_literal(line: &str) -> Result<(), String> {
    let mut rest = line;
    while let Some(at) = rest.find('$') {
        let after = &rest[at + 1..];
        let chars: Vec<char> = after.chars().collect();
        if let Some(why) = dollar_arithmetic(&chars) {
            return Err(format!("a here-document body with {why}"));
        }
        if let Some(b) = after.strip_prefix('{') {
            let Some(end) = b.find('}') else {
                return Err("an unterminated ${ in a here-document body".to_string());
            };
            braced_is_literal(&b[..end])?;
        }
        rest = after;
    }
    Ok(())
}

/// Remove every `perl -e 'alarm N; exec @ARGV'` wrapper with exactly that body
/// ([`is_alarm_body`]).
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

/// Replace every quoted string with `""`. A substitution inside double quotes
/// stays glued to that `""`, preceded by [`QUOTE_SUB_MARK`] (so `x="$(…)"` is
/// still one assignment and [`split_segments`] reads a
/// [`QUOTED_SUBSTITUTION`]); a parameter inside double quotes becomes
/// [`QUOTED_PARAMETER`]; find's `\(`/`\)` become bare parens; other escapes
/// stay verbatim.
fn strip_quotes(src: &str) -> String {
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
                    Some(&n) if n == QUOTE_SUB_MARK => out.push('\\'),
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
                        out.push(QUOTE_SUB_MARK);
                        out.push_str("$(");
                        *i += 2;
                        strip_into(chars, i, out, Some(')'));
                        out.push(')');
                        continue;
                    }
                    if d == '`' {
                        out.push(QUOTE_SUB_MARK);
                        out.push('`');
                        *i += 1;
                        strip_into(chars, i, out, Some('`'));
                        out.push('`');
                        continue;
                    }
                    if d == '$'
                        && chars.get(*i + 1).is_some_and(|&n| {
                            n.is_ascii_alphanumeric()
                                || matches!(n, '_' | '{' | '@' | '*' | '#' | '?' | '$' | '!' | '-')
                        })
                    {
                        out.push_str(QUOTED_PARAMETER);
                        *i += 1;
                        match chars.get(*i) {
                            Some('{') => {
                                while *i < chars.len() && chars[*i] != '}' && chars[*i] != '"' {
                                    *i += 1;
                                }
                                if chars.get(*i) == Some(&'}') {
                                    *i += 1;
                                }
                            }
                            Some(&n) if n.is_ascii_alphanumeric() || n == '_' => {
                                while chars
                                    .get(*i)
                                    .is_some_and(|&n| n.is_ascii_alphanumeric() || n == '_')
                                {
                                    *i += 1;
                                }
                            }
                            Some(_) => *i += 1,
                            None => {}
                        }
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
                // A literal QUOTE_SUB_MARK is dropped: only the marker this
                if c != QUOTE_SUB_MARK {
                    out.push(c);
                }
                *i += 1;
            }
        }
    }
}

/// Split on `;`, `&`, `&&`, `||`, `|`, `(`, `)` and newline; each segment is its
/// whitespace-split tokens.
fn split_segments(s: &str) -> Vec<Vec<String>> {
    let chars: Vec<char> = s.chars().collect();
    let mut segments = Vec::new();
    let mut i = 0;
    split_level(&chars, &mut i, None, &mut segments);
    segments
}

/// One level of [`split_segments`]: the top (`stop` none), a `$(…)` body
/// (`)`) or a backtick body.
fn split_level(chars: &[char], i: &mut usize, stop: Option<char>, segments: &mut Vec<Vec<String>>) {
    let mut cur = String::new();
    let flush = |cur: &mut String, segments: &mut Vec<Vec<String>>| {
        let toks: Vec<String> = cur.split_whitespace().map(str::to_string).collect();
        if !toks.is_empty() {
            segments.push(toks);
        }
        cur.clear();
    };
    let mut depth = 0usize;
    while *i < chars.len() {
        let c = chars[*i];
        match c {
            '\\' => {
                cur.push(c);
                if let Some(&n) = chars.get(*i + 1) {
                    cur.push(n);
                    *i += 1;
                }
            }
            _ if c == QUOTE_SUB_MARK
                && chars.get(*i + 1) == Some(&'$')
                && chars.get(*i + 2) == Some(&'(') =>
            {
                *i += 3;
                split_level(chars, i, Some(')'), segments);
                cur.push_str(QUOTED_SUBSTITUTION);
                continue;
            }
            _ if c == QUOTE_SUB_MARK && chars.get(*i + 1) == Some(&'`') => {
                *i += 2;
                split_level(chars, i, Some('`'), segments);
                cur.push_str(QUOTED_SUBSTITUTION);
                continue;
            }
            _ if c == QUOTE_SUB_MARK => {
                // Stray mark; consumed by the outer `*i += 1`.
            }
            '$' if chars.get(*i + 1) == Some(&'(') => {
                *i += 2;
                split_level(chars, i, Some(')'), segments);
                cur.push_str(SUBSTITUTION);
                continue;
            }
            '`' if stop == Some('`') => {
                *i += 1;
                break;
            }
            '`' => {
                *i += 1;
                split_level(chars, i, Some('`'), segments);
                cur.push_str(SUBSTITUTION);
                continue;
            }
            ')' if depth == 0 && stop == Some(')') => {
                *i += 1;
                break;
            }
            '(' => {
                depth += 1;
                flush(&mut cur, segments);
            }
            ')' => {
                depth = depth.saturating_sub(1);
                flush(&mut cur, segments);
            }
            ';' | '|' | '\n' => flush(&mut cur, segments),
            '>' | '<' => {
                if glued_to_a_word(&cur) {
                    cur.push(' ');
                }
                cur.push(c);
            }
            '&' if chars.get(*i + 1) == Some(&'&') => {
                flush(&mut cur, segments);
                *i += 1;
            }
            '&' => {
                let prev = (*i).checked_sub(1).map(|k| chars[k]);
                let next = chars.get(*i + 1).copied();
                if next == Some('>') && glued_to_a_word(&cur) {
                    cur.push(' ');
                    cur.push(c);
                } else if prev == Some('>') || prev == Some('<') || next == Some('>') {
                    cur.push(c);
                } else {
                    flush(&mut cur, segments);
                }
            }
            _ => cur.push(c),
        }
        *i += 1;
    }
    flush(&mut cur, segments);
}

/// Whether a redirect glyph arriving now would be glued to a word (`rm>x`),
/// not to a descriptor (`2>x`), another glyph (`>>`, `<>`), or `&` (`&>x`).
fn glued_to_a_word(cur: &str) -> bool {
    let word = cur.rsplit(char::is_whitespace).next().unwrap_or("");
    let Some(last) = word.chars().last() else {
        return false;
    };
    !matches!(last, '>' | '<' | '&') && !word.chars().all(|c| c.is_ascii_digit())
}

/// Basename of command token `tok`.
pub(crate) fn program(tok: &str) -> &str {
    tok.rsplit('/').next().unwrap_or(tok)
}

/// An output redirect token: `Some(target)` when `tok` redirects (`>`, `>>`,
/// `2>`, `&>`, `>|`, `>file`); `None` when the target is the next token.
fn redirect_target(tok: &str) -> Option<Option<&str>> {
    let pos = tok.find('>')?;
    if tok[..pos].ends_with('<') {
        return None;
    }
    let rest = tok[pos..].trim_start_matches('>');
    let rest = rest.strip_prefix('|').unwrap_or(rest);
    Some((!rest.is_empty()).then_some(rest))
}

/// A redirect target that writes no file: `/dev/null`, a descriptor (`&1`) or a
/// close (`&-`).
fn redirect_is_safe(target: &str) -> bool {
    target == "/dev/null"
        || target.strip_prefix('&').is_some_and(|fd| {
            fd == "-" || (!fd.is_empty() && fd.bytes().all(|b| b.is_ascii_digit()))
        })
}

/// `Some(reason)` when `seg[i]` is a redirect that writes a file: `<>`
/// (read-write, creating it), or `>`, `>>`, `>|`, `&>` or `N>` to a target
/// other than `/dev/null` or a descriptor ([`redirect_is_safe`]).
fn redirect_hazard(seg: &[String], i: usize) -> Option<String> {
    let tok = seg[i].as_str();
    if tok.contains("<>") {
        return Some(format!("redirect <> {tok}"));
    }
    match redirect_target(tok)?.or(seg.get(i + 1).map(String::as_str)) {
        Some(t) if redirect_is_safe(t) => None,
        Some(t) => Some(format!("redirect > {t}")),
        None => Some("redirect > (no target)".to_string()),
    }
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

/// The negative filter over every token: a file redirect, `find -delete` / `-fprint` /
/// `-exec <writer>`, `sed -i`/`-I`, `sort -o`, `tree -o`, `uniq IN OUT`, an inline-code
/// interpreter, a python heredoc, `xargs` feeding a writer, a git write or a git flag that
/// runs a program or writes a file, arithmetic in `[[`/`test`, a word after a closing `}`
/// ([`closes_a_brace`]) or `]]`, and an assignment to a [`HAZARD_VARS`] variable.
fn danger_scan(segments: &[Vec<String>]) -> Option<String> {
    let has = |open: &str| segments.iter().flatten().any(|t| t == open);
    let (braces, brackets) = (has("{"), has("[["));
    let closing = |t: &str| (braces && closes_a_brace(t)) || (brackets && t == "]]");
    for seg in segments {
        if let Some(reason) = loop_assignment_hazard(seg) {
            return Some(reason);
        }
        for (i, tok) in seg.iter().enumerate() {
            let prog = program(tok);
            if let Some(reason) = assignment_hazard(tok) {
                return Some(reason);
            }
            if closing(tok) {
                let mut k = i + 1;
                while let Some(span) = seg.get(k).map(|t| redirect_span(t)).filter(|&n| n > 0) {
                    k += span;
                }
                if seg.get(k).is_some_and(|n| !closing(n)) {
                    return Some(format!("a word after a closing {tok} (zsh may run it)"));
                }
            }
            if prog == "xargs" {
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
                if let Some(t) = seg[i + 1..]
                    .iter()
                    .take_while(|t| *t != "]")
                    .find(|t| tok != "[[" && !t.starts_with('/') && expands_apart(t))
                {
                    return Some(format!(
                        "{tok} {t} (the shell may expand it into `-t` and an operand)"
                    ));
                }
            }
            if let Some(reason) = redirect_hazard(seg, i) {
                return Some(reason);
            }
            if let Some(reason) = flag_hazard_at(seg, i) {
                return Some(reason);
            }
        }
    }
    None
}

/// The option-and-flag forms by which a listed read tool writes or runs, read from token `seg[i]`
/// over either reading ([`danger_scan`], [`argv_flag_hazard`]); a flag token such as `-delete` is
/// refused whatever program it follows, quoted or not.
fn flag_hazard_at(seg: &[String], i: usize) -> Option<String> {
    let tok = &seg[i];
    let prog = program(tok);
    let next = seg.get(i + 1).map(String::as_str);
    let next_prog = next.map(program);
    if tok == "-delete" {
        return Some("find -delete".to_string());
    }
    if tok.starts_with("-fprint") || tok == "-fls" {
        return Some(format!("find {tok}"));
    }
    if prog == "sort" || prog == "tree" {
        // `sort -o FILE` / `--output=FILE`, `tree -o FILE`: an output
        // file. Neither has another short flag with an `o` in it, and
        // sort takes any prefix of `--output` for it
        // ([`long_option_spells`]).
        for t in &seg[i + 1..] {
            if long_option_spells(t, "output")
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
            Some(p) if FIND_EXEC_READ.contains(&p) => {
                // The program's own write flags, as at a head (`file -C`
                // writes magic.mgc, MEASURED 2026-10-22), up to `;` or `+`.
                let args: Vec<String> = seg[i + 2..]
                    .iter()
                    .take_while(|t| !matches!(t.as_str(), ";" | "\\;" | "+"))
                    .cloned()
                    .collect();
                if let Some(reason) = runs_or_writes_by_flag_reading(p, &args, Reading::Argv) {
                    return Some(format!("find {tok} {reason}"));
                }
            }
            Some(p) => return Some(format!("find {tok} {p}")),
            None => return Some(format!("find {tok}")),
        }
    }
    if prog == "sed" {
        // macOS sed edits in place under `-I` as under `-i`, alone or
        // in a cluster (`-nI`).
        for t in &seg[i + 1..] {
            if t == "--in-place"
                || t.starts_with("--in-place=")
                || (t.starts_with('-') && !t.starts_with("--") && t[1..].contains(['i', 'I']))
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
                // A repository of the line's choosing, whose config
                // (`core.fsmonitor`, `diff.external`) runs a program on
                // a read. `-C` stays a read: `cd X && git status`
                // reaches the same repository.
                return Some(format!("git {t} (a repository of the line's choosing)"));
            }
            if t == "--help" || t == "-h" {
                return Some(format!("git {t} {GIT_HELP_RUNS}"));
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
        // Read subcommands' flags that write a file or run a program:
        // `--output=<file>`, an external diff driver, `grep -O<pager>`.
        for t in &seg[sub_idx + 1..] {
            let t = t.as_str();
            if t == "--output"
                || t.starts_with("--output=")
                || t == "--ext-diff"
                || t == "--textconv"
                || t == "--filters"
                || t.starts_with("--open-files-in-pager")
                || (t.starts_with("-O") && !t.starts_with("--"))
                || (sub == "grep" && grep_opens_pager(t))
            {
                return Some(format!("git {sub} {t}"));
            }
            if t == "--help" {
                return Some(format!("git {sub} --help {GIT_HELP_RUNS}"));
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
    None
}

/// The flag-and-option hazards read from the shell's argv words ([`program_words`]), where a
/// quote or a backslash no longer hides a flag (`find '-delete'`, `sort '-o' out`).
fn argv_flag_hazard(cmd: &str) -> Option<String> {
    program_words(cmd)
        .iter()
        .find_map(|seg| seg_flag_hazard(seg))
}

/// One argv segment's flag hazards: every token's [`flag_hazard_at`], the head's
/// [`runs_or_writes_by_flag`] and a git read's write form ([`git_read_writes`]).
fn seg_flag_hazard(seg: &[String]) -> Option<String> {
    for i in 0..seg.len() {
        if let Some(reason) = flag_hazard_at(seg, i) {
            return Some(reason);
        }
    }
    let Ok(Some(run)) = runs(seg) else {
        return None;
    };
    let head = program(&seg[run.at]);
    // An expansion among a git read's words is the quote-stripped head check's to name.
    if head == "git"
        && let Some((sub_idx, sub)) = git_subcommand(seg, run.at)
        && GIT_READ.contains(&sub)
        && !seg[sub_idx + 1..].iter().any(|a| a.contains(SUBSTITUTION))
        && git_read_writes(sub, &seg[sub_idx + 1..])
    {
        return Some(format!("git {sub} (write form)"));
    }
    runs_or_writes_by_flag_reading(head, &seg[run.at + 1..], Reading::Argv)
}

/// Why `git <sub> --help` (and `git --help <sub>`, `git -h <sub>`) is no read:
/// it is `git help <sub>`, which runs the viewer `man.viewer`/`man.<tool>.cmd`
/// (or with `help.format = web`, `web.browser`/`browser.<tool>.cmd`) names,
/// keys a repository's config may set, even without a terminal.
const GIT_HELP_RUNS: &str = "(runs git help: the man or web viewer the config names)";

/// Whether a `git grep` argument turns on `-O` / `--open-files-in-pager`, which
/// runs the pager on matching files even without a terminal: a short cluster
/// holding `O` before any value-taking letter (`-nO`, `-iOless`, `-3O`; not
/// `-eO`), or a long option git would take as its abbreviation (`--open`,
/// `--op`; the ambiguous `--o` counts too).
fn grep_opens_pager(t: &str) -> bool {
    if let Some(long) = t.strip_prefix("--") {
        let name = long.split('=').next().unwrap_or("");
        return !name.is_empty() && "open-files-in-pager".starts_with(name);
    }
    let Some(cluster) = t.strip_prefix('-') else {
        return false;
    };
    for c in cluster.chars() {
        match c {
            'O' => return true,
            'A' | 'B' | 'C' | 'e' | 'f' | 'm' => return false,
            _ => {}
        }
    }
    false
}

/// Whether `tok` names a program by a path in [`SYSTEM_BIN_DIRS`] and nowhere
/// deeper (`/bin/ls`, not `/bin/x/ls` or `/bin/../tmp/ls`).
fn is_system_bin(tok: &str) -> bool {
    SYSTEM_BIN_DIRS.iter().any(|dir| {
        tok.strip_prefix(dir)
            .is_some_and(|rest| !rest.is_empty() && !rest.contains('/'))
    })
}

/// Whether every entry of the search path `value` is a [`SYSTEM_BIN_DIRS`]
/// directory (`/bin:/usr/bin`): a program found there is the one its name says.
fn is_system_search_path(value: &str) -> bool {
    value.split(':').all(|entry| {
        let entry = entry.trim_end_matches('/');
        SYSTEM_BIN_DIRS
            .iter()
            .any(|dir| dir.trim_end_matches('/') == entry)
    })
}

/// `Some(reason)` when `tok` assigns a [`HAZARD_VARS`] variable (or a
/// [`HAZARD_VAR_PREFIXES`] family).
fn assignment_hazard(tok: &str) -> Option<String> {
    if !is_assignment(tok) {
        return None;
    }
    let (written, value) = tok.split_once('=')?;
    let name = ZSH_TIED
        .iter()
        .find(|(array, _)| *array == written)
        .map_or(written, |(_, scalar)| *scalar);
    if name == "PATH" {
        return (!is_system_search_path(value))
            .then(|| format!("{written}= (changes which program a name runs)"));
    }
    if ZSH_ARITHMETIC_VARS.contains(&name) && !plain_number(value) {
        return Some(format!(
            "{written}= (zsh evaluates the value as arithmetic, whose subscript can run a command)"
        ));
    }
    (HAZARD_VARS.contains(&name) || HAZARD_VAR_PREFIXES.iter().any(|p| name.starts_with(p)))
        .then(|| format!("{written}= (changes what a later program runs or reads)"))
}

/// A `for` loop assigns each word to its variables, and zsh evaluates each as
/// arithmetic when the variable is one of [`ZSH_ARITHMETIC_VARS`] (`for SECONDS
/// in 'path[$(cmd)]'` runs `cmd`).
fn loop_assignment_hazard(seg: &[String]) -> Option<String> {
    let j = skip_prefixes(seg, 0);
    if seg.get(j).map(String::as_str) != Some("for") {
        return None;
    }
    let names: Vec<&str> = seg[j + 1..]
        .iter()
        .map(String::as_str)
        .take_while(|t| is_name(t) && *t != "in")
        .collect();
    let after = j + 1 + names.len();
    let unreadable = [SUBSTITUTION.to_string()];
    let words = match seg.get(after).map(String::as_str) {
        Some("in") => &seg[after + 1..],
        _ => &unreadable[..],
    };
    names.iter().find_map(|name| {
        words
            .iter()
            .find_map(|w| assignment_hazard(&format!("{name}={w}")))
            .map(|why| format!("for {why}"))
    })
}

/// The variables whose assigned value zsh 5.9 evaluates as arithmetic, so an
/// assignment (`NAME=v`, `NAME=v cmd`, `for NAME in v`) runs a command from the
/// value's subscript (`SECONDS='path[$(cmd)]'`); only a plain number may be
/// assigned ([`plain_number`]).
const ZSH_ARITHMETIC_VARS: &[&str] = &[
    "COLUMNS",
    "DIRSTACKSIZE",
    "EGID",
    "ERRNO",
    "EUID",
    "FUNCNEST",
    "GID",
    "HISTSIZE",
    "KEYTIMEOUT",
    "LINES",
    "LISTMAX",
    "MAILCHECK",
    "MBEGIN",
    "MEND",
    "OPTIND",
    "RANDOM",
    "REPORTMEMORY",
    "REPORTTIME",
    "SAVEHIST",
    "SECONDS",
    "SHLVL",
    "TRY_BLOCK_ERROR",
    "TRY_BLOCK_INTERRUPT",
    "UID",
    "ZLE_RPROMPT_INDENT",
];

/// Characters that trigger zsh filename globbing (`EXTENDED_GLOB` included).
pub(crate) const GLOB_CHARS: &[char] = &['*', '?', '[', '^', '#', '('];

/// Whether the shell may make other words of the quote-stripped word `t`: an
/// unquoted `{` it brace-expands, a [`GLOB_CHARS`] character, or an
/// `EXTENDED_GLOB` `~` past the first character (a leading one is one path).
fn expands_apart(t: &str) -> bool {
    t.contains('{') || t.contains(GLOB_CHARS) || t.chars().skip(1).any(|c| c == '~')
}

/// Whether `t` spells the long option `--{name}` as `getopt_long` (macOS
/// `sort` 2.3-Apple, GNU tools) and git's option parser read one: the whole
/// name or any prefix of it, bare or with `=value` (`--o`, `--outp=f`,
/// `--compress`).
fn long_option_spells(t: &str, name: &str) -> bool {
    t.strip_prefix("--")
        .map(|r| r.split_once('=').map_or(r, |(n, _)| n))
        .is_some_and(|n| !n.is_empty() && name.starts_with(n))
}

/// Read tools whose write or run form an argument selects (`sort -o`, `rg
/// --pre`, `find -delete`, `test -v`, `uniq IN OUT`, `date -s`, `less +…`, a
/// compiling `trustc`): an expansion before `--` may be that flag, so it is
/// refused ([`expansion_in_an_option_slot`]).
const SELECTED_BY_AN_ARGUMENT: &[&str] = &[
    "sort", "tree", "sed", "uniq", "less", "more", "sysctl", "file", "date", "rg", "find", "test",
    "[", "trustc", "rustc",
];

/// Whether `tok` carries an expansion whose value is unreadable: a substitution
/// ([`SUBSTITUTION`], [`QUOTED_SUBSTITUTION`]), a double-quoted parameter
/// ([`QUOTED_PARAMETER`]), or an unquoted `$X`.
fn has_expansion(tok: &str) -> bool {
    let b = tok.as_bytes();
    b.iter()
        .enumerate()
        .any(|(k, &c)| c == b'$' && (k == 0 || b[k - 1] != b'\\'))
}

/// The first word in `args` carrying an expansion ([`has_expansion`]) where
/// `head` reads options: anywhere before `--`, since even a quoted expansion
/// can start with `-`.
fn expansion_in_an_option_slot<'a>(head: &str, args: &'a [String]) -> Option<&'a str> {
    if head == "test" || head == "[" {
        let ops = match args.split_last() {
            Some((last, rest)) if head == "[" && last == "]" => rest,
            _ => args,
        };
        if test_expansions_are_operands(ops) {
            return None;
        }
        return ops.iter().find(|t| has_expansion(t)).map(String::as_str);
    }
    let end = option_slots_end(head, args);
    args[..end]
        .iter()
        .find(|t| has_expansion(t))
        .or_else(|| {
            // An unquoted expansion past `--` may split into two of uniq's operands (bash).
            (head == "uniq")
                .then(|| args[end..].iter().find(|t| has_unquoted_expansion(t)))
                .flatten()
        })
        .map(String::as_str)
}

/// Where `head`'s option slots end: at `--`, except for `find`, whose primaries `--` does not end,
/// and `sysctl`, whose `name=value` operand writes (`find -- d $X` with `X=-delete` removed `d`,
/// MEASURED 2026-10-22).
fn option_slots_end(head: &str, args: &[String]) -> usize {
    if matches!(head, "find" | "sysctl") {
        return args.len();
    }
    args.iter().position(|t| t == "--").unwrap_or(args.len())
}

/// The first quote-stripped word in `head`'s option slots the shell may brace-expand or glob into
/// a flag ([`may_expand_into_a_flag`]), or, for `uniq` and `sysctl`, into any other operand (a
/// second operand is uniq's output file; a `name=value` one sets a value). MEASURED 2026-10-22,
/// zsh 5.9 and bash 3.2: `find d -{delete,true}` removed `d`, `sort *.txt` beside a file `-o.txt`
/// wrote `.txt`, `uniq *.txt` overwrote the second file, `uniq a{,2}.txt` wrote `a2.txt`. `test`,
/// `[` and `printf` judge their own expanding words.
fn glob_in_an_option_slot<'a>(head: &str, args: &'a [String]) -> Option<&'a str> {
    if matches!(head, "test" | "[" | "printf") {
        return None;
    }
    let any_operand = matches!(head, "uniq" | "sysctl");
    let end = if head == "uniq" {
        args.len()
    } else {
        option_slots_end(head, args)
    };
    args[..end]
        .iter()
        .find(|t| may_expand_into_a_flag(t) || (any_operand && expands_apart(&t.replace("{}", ""))))
        .map(String::as_str)
}

/// Whether the shell may expand the quote-stripped word `t` into a word that starts with `-` or
/// `+`: it brace-expands or globs ([`expands_apart`]) and does not start with a literal that no
/// expansion moves (`HEAD~1`, `src/*.rs` and `./*` start with their own text; `-del*`, `{-a,b}`,
/// `*.txt` and a blanked quote `""del*` may not).
fn may_expand_into_a_flag(t: &str) -> bool {
    // `{}` (find's placeholder) brace-expands in neither shell.
    expands_apart(&t.replace("{}", ""))
        && !t.chars().next().is_some_and(|c| {
            c.is_alphanumeric() || matches!(c, '.' | '/' | '_' | '@' | '%' | ':' | ',' | '=' | '~')
        })
}

/// Whether `tok` carries an unquoted expansion ([`SUBSTITUTION`] or bare `$X`),
/// which word-splits and can change its command's word count.
fn has_unquoted_expansion(tok: &str) -> bool {
    has_expansion(
        &tok.replace(QUOTED_SUBSTITUTION, "")
            .replace(QUOTED_PARAMETER, ""),
    )
}

/// Whether every expansion in a `test`/`[` expression is a quoted operand under
/// POSIX's 1-, 2- and 3-argument rules (`[ X ]`, `[ ! X ]`, `[ -z X ]`, `[ X = Y
/// ]`, `[ X -gt Y ]`, `[ !
fn test_expansions_are_operands(ops: &[String]) -> bool {
    if ops.iter().any(|t| has_unquoted_expansion(t)) {
        return false;
    }
    let is = |k: usize, set: &[&str]| {
        ops.get(k)
            .is_some_and(|t| !has_expansion(t) && set.contains(&t.as_str()))
    };
    match ops.len() {
        0 | 1 => true,
        2 => is(0, &["!"]) || is(0, TEST_UNARY),
        3 => is(1, TEST_BINARY) || (is(0, &["!"]) && is(1, TEST_UNARY)),
        _ => !ops.iter().any(|t| has_expansion(t)),
    }
}

/// Unary `test`/`[` primaries whose operand is a string or file.
pub(crate) const TEST_UNARY: &[&str] = &[
    "-b", "-c", "-d", "-e", "-f", "-g", "-h", "-k", "-p", "-r", "-s", "-t", "-u", "-w", "-x", "-L",
    "-O", "-G", "-N", "-S", "-z", "-n",
];

/// Binary `test`/`[` primaries.
pub(crate) const TEST_BINARY: &[&str] = &[
    "=", "==", "!=", "<", ">", "-nt", "-ot", "-ef", "-eq", "-ne", "-gt", "-ge", "-lt", "-le",
];

/// Flags by which a read tool at a segment head runs a program or writes: `rg --pre`, `sort
/// --compress-program=`, `printf -v NAME`, `less`/`more` with `+<command>`, a log or key
/// file, `sysctl name=value`/`-w`, `hostname <name>`/`-F`, `file -C`, `tree -R`, `test -v`,
/// and `date` setting the clock (`-s`, or a positional without BSD's `-j`); and for
/// [`SELECTED_BY_AN_ARGUMENT`] tools, any expansion before `--`.
fn runs_or_writes_by_flag(head: &str, args: &[String]) -> Option<String> {
    runs_or_writes_by_flag_reading(head, args, Reading::QuoteStripped)
}

/// Which tokenization a flag scan reads. Only the quote-stripped words keep the quoted/unquoted
/// marks an expansion, brace or glob refusal rests on ([`expansion_in_an_option_slot`],
/// [`glob_in_an_option_slot`]); the argv words get the literal-flag checks alone.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Reading {
    QuoteStripped,
    Argv,
}

/// [`runs_or_writes_by_flag`] under a chosen [`Reading`].
fn runs_or_writes_by_flag_reading(head: &str, args: &[String], reading: Reading) -> Option<String> {
    let flag = |t: &str| format!("{head} {t}");
    let unreadable = |t: &str| format!("{head} {t} (a word this check cannot read may be a flag)");
    if reading == Reading::QuoteStripped
        && SELECTED_BY_AN_ARGUMENT.contains(&head)
        && let Some(t) = expansion_in_an_option_slot(head, args)
    {
        return Some(unreadable(t));
    }
    if reading == Reading::QuoteStripped
        && SELECTED_BY_AN_ARGUMENT.contains(&head)
        && let Some(t) = glob_in_an_option_slot(head, args)
    {
        return Some(format!(
            "{head} {t} (the shell may brace-expand or glob it into a flag or another operand)"
        ));
    }
    match head {
        "rg" => args
            .iter()
            .find(|t| *t == "--pre" || t.starts_with("--pre="))
            .map(|t| flag(t)),
        "sort" => args
            .iter()
            .find(|t| {
                t.starts_with("--compress-program") || long_option_spells(t, "compress-program")
            })
            .map(|t| flag(t)),
        "printf" => {
            let format = args.iter().position(|t| !t.starts_with('-'));
            let opts = &args[..format.map_or(args.len(), |k| k + 1)];
            opts.iter()
                .find(|t| {
                    reading == Reading::QuoteStripped && (has_expansion(t) || expands_apart(t))
                })
                .map(|t| unreadable(t))
                .or_else(|| {
                    args.iter()
                        .take_while(|t| t.starts_with('-'))
                        .find(|t| t.starts_with("-v"))
                        .map(|t| flag(t))
                })
        }
        "test" => args
            .iter()
            .find(|t| *t == "-v")
            .map(|t| format!("test {t} (evaluates arithmetic)")),
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
    tok.find('=').is_some_and(|eq| is_name(&tok[..eq]))
}

/// Whether `tok` is a shell variable name.
fn is_name(tok: &str) -> bool {
    let mut cs = tok.chars();
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

/// The positive filter on one segment: `None` when its head is a read-only
/// program (with the git / tmutil / python refinements), else the reason.
fn segment_head<S: AsRef<str>>(seg: &[String], python_allow: &[S]) -> Option<String> {
    head_from(seg, 0, true, python_allow)
}

/// How many tokens a redirect in command-name position spans: 1 with its target
/// glued (`2>/dev/null`, `<in`, `>&2`, `<<EOF`), 2 with the target next (`>`
/// `out`), 0 for a non-redirect.
fn redirect_span(tok: &str) -> usize {
    let op = tok.trim_start_matches(|c: char| c.is_ascii_digit());
    if !(op.starts_with(['<', '>']) || op.starts_with("&>")) {
        return 0;
    }
    if op.trim_start_matches(['<', '>', '&', '|']).is_empty() {
        2
    } else {
        1
    }
}

/// Where `seg`'s command starts from word `j`: past `VAR=value` assignments,
/// redirects (`2>/dev/null rm x`), zsh's `-` modifier, `timeout [flags] N` or
/// `time [-p]`, and keyword prefixes (`do`, `then`, `if`), in any order — a
/// keyword only where the shell reads one ([`skip_prefixes_at`]).
fn skip_prefixes(seg: &[String], j: usize) -> usize {
    skip_prefixes_at(seg, j, true).0
}

/// [`skip_prefixes`] from word `j`, where `keyword` says whether a reserved
/// word at `j` is read as one: at a segment's start it is, as the word a
/// wrapper runs (`env if`, `xargs for`) it is not.
fn skip_prefixes_at(seg: &[String], mut j: usize, mut keyword: bool) -> (usize, bool) {
    loop {
        let start = j;
        while let Some(t) = seg.get(j).map(String::as_str) {
            if KEYWORD_PREFIX.contains(&t) {
                if !keyword {
                    return (j, false);
                }
                j += 1;
            } else if is_assignment(t) || t == "-" {
                keyword = false;
                j += 1;
            } else if redirect_span(t) > 0 {
                keyword = false;
                j += redirect_span(t);
            } else {
                break;
            }
        }
        match seg.get(j).map(String::as_str) {
            Some("timeout") => {
                j += 1;
                while j < seg.len() && (seg[j].starts_with('-') || is_duration(&seg[j])) {
                    j += 1;
                }
                keyword = false;
            }
            Some("time") => {
                let at = j;
                j += 1;
                while seg.get(j).map(String::as_str) == Some("-p") {
                    j += 1;
                }
                if seg.get(j).is_some_and(|t| t.starts_with('-')) {
                    // `/usr/bin/time`'s own options (`-o FILE` writes FILE,
                    return (at, false);
                }
                keyword = false;
            }
            _ => {}
        }
        if j == start {
            break;
        }
    }
    (j, keyword)
}

/// bash 3.2's reserved words (`time` aside, which [`skip_prefixes_at`] reads
/// through as a wrapper wherever it stands): where the shell reads a command's
/// name instead of a keyword, each is the name of a program the line does not
/// show ([`head_from`]), though [`READ_ONLY`] and [`KEYWORD_PREFIX`] list some
/// of them as the keywords they are at a command's start.
const SHELL_RESERVED: &[&str] = &[
    "!", "case", "coproc", "do", "done", "elif", "else", "esac", "fi", "for", "function", "if",
    "in", "select", "then", "until", "while", "{", "}", "[[", "]]",
];

/// What the wrapper at `seg[j]` (`xargs`, `env`, by name or from a system
/// directory) runs.
fn wrapper(seg: &[String], j: usize) -> Option<Wrapped<'_>> {
    let tok = seg.get(j)?.as_str();
    if tok.contains('/') && !is_system_bin(tok) {
        return None;
    }
    match program(tok) {
        "xargs" => {
            let mut k = j + 1;
            while k < seg.len() && seg[k].starts_with('-') && seg[k] != "-" {
                k += if XARGS_VALUE_FLAGS.contains(&seg[k].as_str()) {
                    2
                } else {
                    1
                };
            }
            Some(Ok((k < seg.len()).then(|| (k, Vec::new()))))
        }
        "env" => {
            let mut k = j + 1;
            let mut dirs = Vec::new();
            while k < seg.len() {
                let t = seg[k].as_str();
                if t == "--split-string" || t.starts_with("--split-string=") {
                    return Some(Err("env -S".to_string()));
                }
                if t == "--" {
                    k += 1;
                    break;
                }
                if let Some(dir) = t.strip_prefix("--chdir=") {
                    dirs.push(dir);
                    k += 1;
                    continue;
                }
                if t == "--chdir" || t == "--unset" {
                    if t == "--chdir"
                        && let Some(dir) = seg.get(k + 1)
                    {
                        dirs.push(dir.as_str());
                    }
                    k += 2;
                    continue;
                }
                if t.starts_with("--") {
                    k += 1;
                    continue;
                }
                if t.starts_with('-') && t.len() > 1 {
                    let mut width = 1;
                    for (at, c) in t.char_indices().skip(1) {
                        if c == 'S' {
                            return Some(Err("env -S".to_string()));
                        }
                        if !matches!(c, 'u' | 'C' | 'P') {
                            continue;
                        }
                        let glued = &t[at + c.len_utf8()..];
                        let value = if glued.is_empty() {
                            width = 2;
                            seg.get(k + 1).map(String::as_str)
                        } else {
                            Some(glued)
                        };
                        if c == 'P' && !value.is_some_and(is_system_search_path) {
                            return Some(Err(format!(
                                "env -P {} (looks the command up in a directory of the \
                                 line's choosing)",
                                value.unwrap_or("")
                            )));
                        }
                        if c == 'C'
                            && let Some(dir) = value
                        {
                            dirs.push(dir);
                        }
                        break;
                    }
                    k += width;
                    continue;
                }
                if is_assignment(t) {
                    k += 1;
                    continue;
                }
                break;
            }
            Some(Ok((k < seg.len()).then_some((k, dirs))))
        }
        _ => None,
    }
}

/// What a wrapper runs ([`wrapper`]): the index of its command and the `env
/// -C` directories on the way, no command, or why it hides one.
type Wrapped<'s> = Result<Option<(usize, Vec<&'s str>)>, String>;

/// Resolved command execution target of a shell segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Runs<'s> {
    /// Index of the command word within the segment.
    pub at: usize,
    /// Outer-to-inner `env -C`/`--chdir` directory arguments preceding the command.
    pub chdir: Vec<&'s str>,
    /// Whether `xargs` feeds additional arguments to the command.
    pub fed: bool,
}

/// Resolve the [`Runs`] target of segment `seg` past prefixes and wrappers.
pub(crate) fn runs(seg: &[String]) -> Result<Option<Runs<'_>>, String> {
    let mut j = 0;
    let mut keyword = true;
    let mut chdir = Vec::new();
    let mut fed = false;
    loop {
        j = skip_prefixes_at(seg, j, keyword).0;
        if j >= seg.len() {
            return Ok(None);
        }
        match wrapper(seg, j) {
            None => return Ok(Some(Runs { at: j, chdir, fed })),
            Some(Err(reason)) => return Err(reason),
            Some(Ok(None)) => return Ok(None),
            Some(Ok(Some((k, dirs)))) => {
                fed |= program(&seg[j]) == "xargs";
                chdir.extend(dirs);
                j = k;
                keyword = false;
            }
        }
    }
}

/// Parse `line` into per-segment shell words with quotes resolved.
pub(crate) fn command_words(line: &str) -> Result<Vec<Vec<String>>, String> {
    Ok(program_words(&strip_alarm_idiom(&prelex(line)?)))
}

/// Whether a listed git read's arguments `args` (after the subcommand `sub`) take a write form:
/// a branch or tag created, moved or deleted, a remote verb that changes the config, a config
/// set, edit or removal. Read on both readings, so a quoted verb or flag is still a write
/// (`git remote 'add' x u`, `git branch -a '-m' a b`, MEASURED 2026-10-22).
fn git_read_writes(sub: &str, args: &[String]) -> bool {
    let has = |flags: &[&str]| {
        args.iter().any(|a| {
            flags
                .iter()
                .any(|f| a == f || (f.starts_with("--") && a.starts_with(&format!("{f}="))))
        })
    };
    let has_write = |flags: &[&str]| {
        has(flags)
            || args.iter().any(|a| {
                flags
                    .iter()
                    .filter_map(|f| f.strip_prefix("--"))
                    .any(|name| long_option_spells(a, name))
            })
    };
    match sub {
        "branch" => {
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
                || has_write(&[
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
            has_write(&["-d", "-a", "-s", "-f", "-m", "--delete", "-F"])
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
            // The verb is the first operand: `git remote -v add x u` adds (MEASURED).
            args.iter()
                .map(String::as_str)
                .find(|a| !a.starts_with('-')),
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
            has_write(&[
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
    }
}

/// The head check from token `j`, re-entered for a wrapper's command (`xargs
/// <cmd>`, `env [VAR=v] <cmd>`, `timeout N <cmd>`), so a listed wrapper cannot
/// launder what it runs: `ls | xargs touch` is `touch`.
fn head_from<S: AsRef<str>>(
    seg: &[String],
    j: usize,
    keyword: bool,
    python_allow: &[S],
) -> Option<String> {
    let (j, keyword) = skip_prefixes_at(seg, j, keyword);
    let head_tok = seg.get(j)?;
    if !keyword && SHELL_RESERVED.contains(&head_tok.as_str()) {
        return Some(format!(
            "`{head_tok}` where the shell reads a command's name (behind an assignment, a \
             redirect, a wrapper or `time` it is no keyword: a program named `{head_tok}` runs)"
        ));
    }
    if is_fragment(head_tok) {
        return None;
    }
    if head_tok.contains('$') {
        return Some(format!(
            "a command whose name this check cannot read ({head_tok})"
        ));
    }
    if head_tok.contains('/') && !is_system_bin(head_tok) {
        // `/tmp/evil/ls` and `./ls` are programs of the line's choosing
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
            let query = !rest.is_empty()
                && rest.iter().enumerate().all(|(k, t)| {
                    matches!(t.as_str(), "-vV" | "-Vv" | "-V" | "-v" | "--version")
                        || t == "--print"
                        || t.starts_with("--print=")
                        || (k > 0 && rest[k - 1] == "--print" && !t.starts_with('-'))
                });
            (!query).then(|| format!("{head} (compiles)"))
        }
        "xargs" | "env" => match wrapper(seg, j) {
            Some(Err(reason)) => Some(reason),
            Some(Ok(Some((k, _)))) => head_from(seg, k, false, python_allow),
            _ => None,
        },
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
            let before_dashes = args.iter().position(|a| a == "--").unwrap_or(args.len());
            if let Some(t) = args[..before_dashes].iter().find(|a| has_expansion(a)) {
                return Some(format!(
                    "git {sub} {t} (a word this check cannot read may be a flag or a verb)"
                ));
            }
            // A word the shell may brace-expand or glob into a flag, a verb or another operand:
            // `git remote {add,x} u` and `git config user.{name,email}` write (MEASURED 2026-10-22).
            let operand_counts = matches!(sub, "config" | "remote");
            if let Some(t) = seg[j + 1..sub_idx]
                .iter()
                .find(|a| expands_apart(a))
                .or_else(|| {
                    args[..before_dashes]
                        .iter()
                        .find(|a| may_expand_into_a_flag(a) || (operand_counts && expands_apart(a)))
                })
            {
                return Some(format!(
                    "git {sub} {t} (the shell may brace-expand or glob it into a flag or a verb)"
                ));
            }
            // Past `--` config still counts operands: bash splits `git config -- $X` into a set.
            if sub == "config"
                && let Some(t) = args[before_dashes..]
                    .iter()
                    .find(|a| has_unquoted_expansion(a) || expands_apart(a))
            {
                return Some(format!("git config {t} (may split into a set)"));
            }
            if git_read_writes(sub, args) {
                return Some(format!("git {sub} (write form)"));
            }
            None
        }
        "export" => {
            let mut k = 0;
            while let Some(t) = rest.get(k).map(String::as_str) {
                let span = redirect_span(t);
                if span > 0 {
                    k += span;
                    continue;
                }
                if !(t.starts_with('-') || is_assignment(t) || is_name(t)) {
                    return Some(format!("export {t} (an assignment this check cannot read)"));
                }
                if t.starts_with('-') && t.contains(['i', 'E', 'F']) {
                    return Some(format!(
                        "export {t} (zsh evaluates the values as arithmetic)"
                    ));
                }
                k += 1;
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
                if path.contains('$') {
                    Some(format!(
                        "{head} {path}: a script path this check cannot read"
                    ))
                } else if path.split('/').any(|c| c == "..") {
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

/// `aterm ctl` verbs that only read (screen, session state and history, the
/// roster, a worker's result, the survey), by verb and form: `meta set
/// attention …`, `inbox seen` and `task mark` write, so `meta`, `inbox` and
/// `task` are judged separately.
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
    "log",
    "brief",
    "resizes",
    "history",
    "result",
    "survey",
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

/// `aterm pkg` and `aterm drive` verbs that only read. `drive ledger` reads
/// too, unless `--out` names a file it writes ([`aterm_drive_read`]).
const ATERM_PKG_READ: &[&str] = &["list", "which", "doctor"];
const ATERM_DRIVE_READ: &[&str] = &["phase", "classify", "help", "-h", "--help"];

/// `aterm …` (or `aterm-ctl` / `aterm-drive`): `None` when the form only reads:
/// `--version`, `help`, the [`ATERM_CTL_READ`] verbs, bare `meta`, bare `inbox`
/// and `inbox get <id>`, bare `task`, and the read verbs of `pkg` and `drive`.
fn aterm_read(head: &str, rest: &[String]) -> Option<String> {
    let args: &[String] = match head {
        "aterm-ctl" => return aterm_ctl_read(rest),
        "aterm-drive" => return aterm_drive_read("aterm-drive", rest),
        _ => rest,
    };
    let sub = args.first().map(String::as_str);
    let verb = args.get(1).map(String::as_str);
    match sub {
        Some("--version" | "-V" | "version" | "help" | "--help" | "-h") => None,
        Some("ctl") => aterm_ctl_read(&args[1..]),
        Some("pkg") if verb.is_some_and(|v| ATERM_PKG_READ.contains(&v)) => None,
        Some("drive") => aterm_drive_read("aterm drive", &args[1..]),
        Some(s) => Some(
            format!("aterm {s} {}", verb.unwrap_or(""))
                .trim()
                .to_string(),
        ),
        None => Some("aterm (opens a session)".to_string()),
    }
}

/// The arguments of `aterm drive`: its one option (`--socket P`), then the
/// command.
fn aterm_drive_read(head: &str, args: &[String]) -> Option<String> {
    let mut k = 0;
    while let Some(t) = args.get(k).map(String::as_str) {
        match t {
            "--socket" | "--sock" => k += 2,
            _ => break,
        }
    }
    let verb = args.get(k).map(String::as_str);
    let writes_out = || {
        args[k..]
            .iter()
            .any(|t| t == "--out" || t.starts_with("--out="))
    };
    match verb {
        Some(v) if ATERM_DRIVE_READ.contains(&v) => None,
        Some("ledger") if !writes_out() => None,
        Some("ledger") => Some(format!("{head} ledger --out (writes a file)")),
        v => Some(format!("{head} {}", v.unwrap_or("")).trim().to_string()),
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
        Some(_) if tail.iter().all(|t| t == "--help" || t == "-h") && !tail.is_empty() => true,
        Some("meta" | "task") => tail.is_empty(),
        Some("inbox") => first.is_none() || (first == Some("get") && tail.len() == 2),
        _ => false,
    };
    (!reads).then(|| {
        format!("aterm ctl {} {}", verb.unwrap_or(""), first.unwrap_or(""))
            .trim()
            .to_string()
    })
}

/// The programs a line hands to awk and sed, read from raw words with quotes
/// resolved and expansions marked ([`program_words`]): `system(…)`, a redirect
/// or pipe in awk; `w`/`W`/`e` or `s///w`/`s///e` in sed; a program file (`-f`)
/// or a program supplied by a substitution or parameter.
fn program_scan(cmd: &str) -> Option<String> {
    for seg in program_words(cmd) {
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

/// Whether zsh `printf` evaluates arguments as arithmetic under `format`.
pub(crate) fn numeric_format(format: &str) -> bool {
    let Some(format) = decode_unicode_escapes(format) else {
        return true;
    };
    let mut cs = format.chars();
    while let Some(c) = cs.next() {
        if c != '%' {
            continue;
        }
        loop {
            match cs.next() {
                Some('%' | 's' | 'b' | 'q' | 'c') => break,
                Some('0'..='9' | '$' | '.' | '-' | '+' | ' ' | '#' | '\'') => {}
                _ => return true,
            }
        }
    }
    false
}

/// `format` with each Unicode escape decoded in one pass, as zsh's `printf` does
/// before reading conversions: `\u` and up to 4 hex digits, or `\U` and up to
/// 8.
fn decode_unicode_escapes(format: &str) -> Option<String> {
    let mut out = String::with_capacity(format.len());
    let mut cs = format.chars().peekable();
    while let Some(c) = cs.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        let digits = match cs.next() {
            Some('u') => 4,
            Some('U') => 8,
            Some(e) => {
                out.extend([c, e]);
                continue;
            }
            None => {
                out.push(c);
                break;
            }
        };
        let mut code = String::new();
        while code.len() < digits
            && let Some(d) = cs.next_if(char::is_ascii_hexdigit)
        {
            code.push(d);
        }
        out.push(
            u32::from_str_radix(&code, 16)
                .ok()
                .and_then(char::from_u32)?,
        );
    }
    Some(out)
}

/// The words zsh evaluates as arithmetic, from raw words ([`program_words`]):
/// `printf`'s arguments after a possibly numeric format ([`numeric_format`];
/// zsh takes any first word but `--` as the format), and the operand after `-t`
/// in `test`, `[` and `[[`.
fn arithmetic_scan(cmd: &str) -> Option<String> {
    for seg in program_words(cmd) {
        let Ok(Some(r)) = runs(&seg) else {
            continue;
        };
        let head = program(&seg[r.at]);
        let args = &seg[r.at + 1..];
        let evaluated: Vec<&String> = match head {
            "printf" => {
                let f = usize::from(args.first().is_some_and(|a| a == "--"));
                match args.get(f) {
                    Some(format) if !format.contains(SUBSTITUTION) && !numeric_format(format) => {
                        Vec::new()
                    }
                    _ => args.iter().skip(f + 1).collect(),
                }
            }
            "test" | "[" | "[[" => args
                .windows(2)
                .filter(|w| w[0] == "-t" && w[1] != "]" && w[1] != "]]")
                .map(|w| &w[1])
                .collect(),
            _ => Vec::new(),
        };
        if let Some(w) = evaluated.into_iter().find(|w| !plain_number(w)) {
            return Some(format!(
                "{head} {w} (zsh evaluates it as arithmetic, whose subscript can run a command)"
            ));
        }
    }
    None
}

/// A word zsh arithmetic reads as a number only: digits, sign, point, blanks,
/// and letters only inside a number (`0x1f`, `1e5`, `16#ff`); no name, subscript,
/// quote, expansion or glob.
fn plain_number(w: &str) -> bool {
    let digit = |r: &str| r.starts_with(|c: char| c.is_ascii_digit());
    w.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '#' | '.' | '+' | '-' | ' ' | '\t'))
        && w.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '#'))
            .all(|run| {
                run.is_empty()
                    || (digit(run)
                        && !run
                            .get(1..)
                            .and_then(|r| r.strip_prefix('#'))
                            .is_some_and(|r| !digit(r)))
            })
}

/// `awk [-F s] [-v a=b]... 'program' files…`: the first non-flag word is the
/// program, or every `-e`/`--source` word (gawk) when there is one.
fn awk_args_write(args: &[String]) -> Option<String> {
    let mut programs: Vec<&str> = Vec::new();
    let mut explicit = false;
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
        if matches!(w, "-e" | "--source") {
            programs.extend(args.get(k + 1).map(String::as_str));
            explicit = true;
            k += 2;
            continue;
        }
        if let Some(p) = w.strip_prefix("--source=") {
            programs.push(p);
            explicit = true;
            k += 1;
            continue;
        }
        if matches!(w, "-F" | "-v" | "--field-separator" | "--assign") {
            k += 2;
            continue;
        }
        if w.starts_with("--field-separator=")
            || w.starts_with("--assign=")
            || ((w.starts_with("-F") || w.starts_with("-v")) && !w.starts_with("--"))
        {
            k += 1;
            continue;
        }
        if w == "--" {
            k += 1;
            break;
        }
        if w.starts_with('-') && w.len() > 1 {
            return Some(format!("awk {w} (an option this check does not read)"));
        }
        break;
    }
    if !explicit {
        programs.push(args.get(k)?);
    }
    if let Some(program) = programs.iter().find(|p| p.contains(SUBSTITUTION)) {
        return Some(format!("awk: a program this check cannot read ({program})"));
    }
    programs
        .iter()
        .any(|p| awk_program_writes(p))
        .then(|| "awk writes (system, a redirect or a pipe in the program)".to_string())
}

/// Whether an awk program does more than read: the `system` builtin in any
/// spelling (`system(…)`, `system (…)`, and a bare `system`, which runs
/// `$0`), anywhere in the text; any `@` (gawk's `@include`, `@load` and its
/// indirect calls, which reach `system` by a computed name); `>>`; a
/// coprocess; any `>` in a `print` or `printf` statement outside its
/// parentheses, which awk reads as a redirect whatever follows (`print $1 >
/// $2`, `print 1 > 2`); a `>` elsewhere unless a comparison against a
/// number, field or `>=`; or a `|` to a command (not `||`).
fn awk_program_writes(p: &str) -> bool {
    if p.contains(">>") || p.contains("|&") || p.contains('@') || names_awk_word(p, "system") {
        return true;
    }
    let cs: Vec<char> = p.chars().collect();
    let ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let mut k = 0;
    const START: char = '\n';
    let mut prev: Option<char> = None;
    let mut prev_at = 0;
    let mut depth = 0usize;
    let mut print_at: Option<usize> = None;
    while k < cs.len() {
        let c = cs[k];
        if (c.is_ascii_alphabetic() || c == '_') && !(k > 0 && ident(cs[k - 1])) {
            let start = k;
            while k < cs.len() && ident(cs[k]) {
                k += 1;
            }
            let word: String = cs[start..k].iter().collect();
            if word == "print" || word == "printf" {
                print_at = Some(depth);
            }
            prev = if matches!(
                word.as_str(),
                "print" | "printf" | "return" | "else" | "do" | "case" | "in"
            ) {
                Some(START)
            } else {
                Some(cs[k - 1])
            };
            prev_at = k - 1;
            continue;
        }
        match c {
            '"' => {
                k += 1;
                while k < cs.len() && cs[k] != '"' {
                    k += if cs[k] == '\\' { 2 } else { 1 };
                }
                prev = Some('"');
                prev_at = k;
                k += 1;
                continue;
            }
            '#' => {
                while k < cs.len() && cs[k] != '\n' {
                    k += 1;
                }
                continue;
            }
            '/' if !prev.is_some_and(|p| {
                p.is_ascii_alphanumeric() || matches!(p, '_' | ')' | ']' | '"' | '/' | '.' | '$')
            }) =>
            {
                if let Some(op @ ('+' | '-')) = prev
                    && prev_at > 0
                    && cs[prev_at - 1] == op
                {
                    // `x++ / 2` divides, `+ +/re/` matches: either may be meant.
                    return true;
                }
                k = past_awk_regex(&cs, k + 1);
                prev = Some('/');
                prev_at = k.saturating_sub(1);
                continue;
            }
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            ';' | '{' | '}' => print_at = None,
            '\n' if !prev.is_some_and(|p| ",\\&|".contains(p)) => {
                print_at = None;
                prev = Some(START);
                k += 1;
                continue;
            }
            '|' => {
                if cs.get(k + 1) == Some(&'|') {
                    k += 2;
                    prev = Some('|');
                    prev_at = k - 1;
                    continue;
                }
                return true;
            }
            '>' => {
                if print_at.is_some_and(|d| depth <= d) {
                    return true;
                }
                if cs.get(k + 1) == Some(&'=') || prev == Some('-') || prev == Some('<') {
                    k += 1;
                    prev = Some('>');
                    prev_at = k - 1;
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
            prev_at = k;
        }
        k += 1;
    }
    false
}

/// Past the regex whose text starts at `k` (after its opening `/`): to the
/// char after its closing `/`, skipping `\` escapes and `[…]` bracket
/// expressions (a `]` first in one, or after its `^`, is a member; `[:…:]`,
/// `[.….]` and `[=…=]` hold a `]` of their own).
fn past_awk_regex(cs: &[char], mut k: usize) -> usize {
    while k < cs.len() && cs[k] != '/' {
        match cs[k] {
            '\\' => k += 2,
            '[' => {
                k += 1;
                if cs.get(k) == Some(&'^') {
                    k += 1;
                }
                if cs.get(k) == Some(&']') {
                    k += 1;
                }
                while k < cs.len() && cs[k] != ']' {
                    if cs[k] == '[' && matches!(cs.get(k + 1), Some(':' | '.' | '=')) {
                        let close = cs[k + 1];
                        k += 2;
                        while k < cs.len() && !(cs[k] == close && cs.get(k + 1) == Some(&']')) {
                            k += 1;
                        }
                        k += 2;
                        continue;
                    }
                    k += if cs[k] == '\\' { 2 } else { 1 };
                }
                k += 1;
            }
            _ => k += 1,
        }
    }
    k + 1
}

/// Whether `word` stands in `text` as a whole awk identifier (no letter,
/// digit or `_` either side), wherever it is: in a string too, which only
/// over-refuses.
fn names_awk_word(text: &str, word: &str) -> bool {
    let ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    text.match_indices(word).any(|(at, _)| {
        !text[..at].chars().next_back().is_some_and(ident)
            && !text[at + word.len()..].chars().next().is_some_and(ident)
    })
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
    if let Some(s) = scripts.iter().find(|s| s.contains(SUBSTITUTION)) {
        return Some(format!("sed: a script this check cannot read ({s})"));
    }
    scripts.iter().find_map(|s| sed_script_writes(s))
}

/// Walk a sed script command by command: `w`/`W` write, `e` runs, as do
/// `s///`'s `w` and `e` flags; anything unknown is refused.
fn sed_script_writes(script: &str) -> Option<String> {
    let cs: Vec<char> = script.chars().collect();
    let n = cs.len();
    let unparsed = || Some("sed (unparsed script)".to_string());
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

/// Split `src` into segments of quote-resolved words with substitutions marked.
pub(crate) fn program_words(src: &str) -> Vec<Vec<String>> {
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
    /// A nested substitution: its body's words are segments of their own,
    /// and the substitution is a [`SUBSTITUTION`] in the word being read.
    fn nested(&mut self, stop: char) {
        let seg = std::mem::take(&mut self.seg);
        let cur = std::mem::take(&mut self.cur);
        self.in_word = false;
        self.run(Some(stop));
        self.seg = seg;
        self.cur = cur;
        self.cur.push_str(SUBSTITUTION);
        self.in_word = true;
    }
    /// A parameter expansion (`$name`, `${…}`, `$1`, `$@`): consumed and marked
    /// a [`SUBSTITUTION`].
    fn mark_parameter(&mut self) {
        self.i += 1;
        match self.chars.get(self.i) {
            Some('{') => {
                self.i += 1;
                while self.i < self.chars.len() && self.chars[self.i] != '}' {
                    self.i += 1;
                }
                if self.i < self.chars.len() {
                    // past `}`
                    self.i += 1;
                }
            }
            Some(&c) if c.is_ascii_alphanumeric() || c == '_' => {
                while self.i < self.chars.len()
                    && (self.chars[self.i].is_ascii_alphanumeric() || self.chars[self.i] == '_')
                {
                    self.i += 1;
                }
            }
            Some('@' | '*' | '#' | '?' | '$' | '!' | '-') => self.i += 1,
            _ => {
                self.cur.push('$');
                self.in_word = true;
                return;
            }
        }
        self.cur.push_str(SUBSTITUTION);
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
                            '$' => self.mark_parameter(),
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
                }
                '$' => self.mark_parameter(),
                '`' if stop == Some('`') => {
                    self.i += 1;
                    self.end_seg();
                    return;
                }
                '`' => {
                    self.i += 1;
                    self.nested('`');
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
                    // Detach a redirect from the preceding word as
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

/// Match `pattern` (`*` and `?`, spanning `/`) against the whole `text`.
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

/// Corpus of substitution-bypass command lines and the rules that refuse them.
#[cfg(test)]
pub(crate) const MEASURED_SUBSTITUTION_BYPASSES: &[(&str, &str)] = &[
    ("$(printf 'r''m') -rf /usr", UNREADABLE_SUBSTITUTION),
    ("`printf %s r m` -rf /usr", UNREADABLE_SUBSTITUTION),
    ("$(printf 'r''m') -- /usr", UNREADABLE_SUBSTITUTION),
    ("$(printf 'r''m') 2>/dev/null /usr", UNREADABLE_SUBSTITUTION),
    ("$(printf 'r''m') ls", UNREADABLE_SUBSTITUTION),
    ("ls; $(printf 'r''m') -rf /usr", UNREADABLE_SUBSTITUTION),
    ("$(which rm) -rf /usr", UNREADABLE_SUBSTITUTION),
    ("`echo rm` -rf /usr", UNREADABLE_SUBSTITUTION),
    ("A=1 $(printf 'r''m') -rf /usr", UNREADABLE_SUBSTITUTION),
    (
        "if $(printf 'r''m') -rf /usr; then echo; fi",
        UNREADABLE_SUBSTITUTION,
    ),
    ("do $(printf 'r''m') -rf /usr", UNREADABLE_SUBSTITUTION),
    (
        "2>/dev/null $(printf 'r''m') -rf /usr",
        UNREADABLE_SUBSTITUTION,
    ),
    ("env $(printf 'r''m') -rf /usr", UNREADABLE_SUBSTITUTION),
    (
        "timeout 5 $(printf 'r''m') -rf /usr",
        UNREADABLE_SUBSTITUTION,
    ),
    ("ls | xargs $(printf 'r''m') -rf", UNREADABLE_SUBSTITUTION),
    (
        "ls | xargs $(echo qm | tr q r) -rf /usr",
        UNREADABLE_SUBSTITUTION,
    ),
    ("env $(echo qm | tr q r) -rf /usr", UNREADABLE_SUBSTITUTION),
    (
        "timeout 5 $(echo qm | tr q r) -rf /usr",
        UNREADABLE_SUBSTITUTION,
    ),
    (
        "echo $( $(printf 'r''m') -rf /usr )",
        UNREADABLE_SUBSTITUTION,
    ),
    (
        "echo \"$( $(printf 'r''m') -rf /usr )\"",
        UNREADABLE_SUBSTITUTION,
    ),
    (
        "git branch $(git rev-parse --short HEAD)",
        "git branch $(…) (a word this check cannot read may be a flag or a verb)",
    ),
    (
        "git tag $(date +%s)",
        "git tag $(…) (a word this check cannot read may be a flag or a verb)",
    ),
    ("uniq a.txt $(echo b.txt)", "uniq <output file>"),
];

/// The reason a program named by a substitution is refused with.
#[cfg(test)]
const UNREADABLE_SUBSTITUTION: &str = "a command whose name this check cannot read ($(…))";

#[cfg(test)]
#[path = "classify_tests.rs"]
mod tests;
