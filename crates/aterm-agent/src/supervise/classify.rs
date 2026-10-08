// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Is this shell command line read-only? A pure function a supervisor calls
//! hundreds of times a session, with the corpus that shaped it pinned as tests.
//!
//! A pre-pass ([`prelex`]) reads the line as the shell does where the segment
//! readers would not: a `#` comment ends at its newline, a here-document body
//! is data up to its terminator, `$'…'` and `${…}` are followed or refused,
//! and an unterminated quote is refused. The segment-head check is a positive
//! filter (an unknown program is not read-only, and a program named by a path
//! outside `/bin` and `/usr/bin` is unknown). Quoted strings are dropped before
//! it runs, so a `>` inside a `git --format` string is not a redirect, except
//! that awk and sed programs are read back from the raw words (`system(…)`,
//! `s///w file`). A program word is judged only at a segment head: `grep -rn
//! open src` is a read, `open src` is not. Wrappers (`xargs`, `env`,
//! `timeout`) are seen through; `&` ends a segment like `;`; the name behind a
//! redirect (`2>/dev/null rm x`) or zsh's `-` modifier (`- rm x`) is a head.
//! Flags by which a read tool runs a program or writes a file (`git -c`, `git
//! --output`, `rg --pre`, `sort --compress-program`, `printf -v`, `less +…`,
//! `date -s`) and assignments that change a later program (`PATH=`, `GIT_*=`,
//! `HOME=`) are refused wherever they sit.
//!
//! A command substitution is a word of the command around it, never a
//! boundary, and its body is judged as a command line. A command whose name
//! this check cannot read cannot be cleared: a head supplied by a substitution
//! or parameter (`$(printf 'r''m') -rf /`, `$R -rf /`), and likewise a python
//! script path, awk program, sed script or `export` operand. Nesting deeper
//! than [`MAX_SUBSTITUTION_DEPTH`] is refused.
//!
//! An expansion's value is unknown. Unquoted, it word-splits into any number
//! of words, any of which may be a flag or verb. Double-quoted
//! ([`QUOTED_SUBSTITUTION`], [`QUOTED_PARAMETER`]) it is one word, but can
//! still be a one-word flag (`sort "$(echo -o/f)" x` writes `/f`). So among
//! the arguments of a tool whose write or run form a flag selects
//! ([`SELECTED_BY_AN_ARGUMENT`]), and of every `git` subcommand, any expansion
//! before `--` is refused ([`runs_or_writes_by_flag`], the git arm of
//! [`head_from`]). `test`/`[` go by argument count
//! ([`test_expansions_are_operands`]): a quoted expansion that is an operand
//! of a 1-, 2- or 3-argument form stays a read (they do not evaluate numeric
//! operands as arithmetic); an unquoted one does not (`[ $n ]` with `n='-v
//! a[$(cmd)]'` is `test -v`, whose subscript bash evaluates). Tools with no
//! write form (`ls`, `cat`, `grep`, `wc`, `echo`, `cd`) keep expansion
//! arguments.
//!
//! zsh arithmetic runs a command substitution inside an array subscript from
//! a quoted word, a variable's value or a globbed file name (`printf %d
//! 'path[$(cmd)]'`). So a word zsh evaluates as arithmetic (`printf`'s
//! arguments under a numeric format, `-t`'s operand in `test`/`[`/`[[`, an
//! `export -i`/`-E`/`-F` value, a value for one of [`ZSH_ARITHMETIC_VARS`])
//! must be a plain number ([`arithmetic_scan`]), and a `test`/`[` word that
//! may brace-expand or glob into `-t` is refused.
//!
//! The cost is deliberate: reads a person would clear at a glance (`git diff
//! $(git merge-base HEAD origin/main)`, `find "$d" -name x`) now ask. An
//! escalation only asks a person; a false approval runs the command, so every
//! tie breaks toward "not read-only".
//!
//! This judges one reading of a line. A screen-read command whose rows were
//! joined with spaces differs from the one the shell runs (a newline ends a
//! segment), so a caller classifies both readings and approves only when both
//! are read-only (`supervise::policy::approval`).
//!
//! It reads the line as zsh 5.9 and bash 3.2 do under their DEFAULT options,
//! with no user alias or function. Claude Code's Bash tool runs each command
//! after replaying the person's shell startup (its shell snapshot), so the
//! approval rule that asks this check also reads that startup
//! (`supervise::policy::shell_startup`) and approves nothing where it
//! differs. Where that shell may be zsh, the rule also asks the line to be
//! PLAIN ([`plain_zsh_line`]), a positive grammar that admits no expansion at
//! all, since the forms refused here were found one sweep at a time.

/// The verdict on one command line: `read_only`, and `reason` naming the rule
/// (token or segment head) that decided it, so a supervisor's notes say why.
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

/// An unquoted `$(…)` or backtick substitution in [`split_segments`]' segments
/// and [`program_words`]' words: one word (or part of one) of the command
/// around it, never a boundary; its body becomes segments of its own. It holds
/// nothing the splitters split on, no `/` and no `=`, and no program list
/// names it, so a program it names is unreadable. Unquoted, its output
/// word-splits into any number of words ([`has_expansion`]).
pub(crate) const SUBSTITUTION: &str = "$…";

/// A double-quoted `"$(…)"` substitution: exactly one word whatever its output,
/// though that word may still be a flag ([`runs_or_writes_by_flag`]).
/// [`strip_quotes`] marks it ([`QUOTE_SUB_MARK`]) and [`split_segments`] reads
/// it as this. It deliberately does not contain [`SUBSTITUTION`], so
/// `t.contains(SUBSTITUTION)` tells unquoted from quoted. Its body is still
/// judged, and a command it names is still unreadable. Shown as `$(…)`.
pub(crate) const QUOTED_SUBSTITUTION: &str = "$“…”";

/// A parameter expansion inside double quotes (`"$X"`, `"${X}"`) as
/// [`strip_quotes`] marks it: one word whose value is unknown. A single-quoted
/// `'$X'` is literal and leaves no `$`. Shown as `"$…"`.
pub(crate) const QUOTED_PARAMETER: &str = "$“p”";

/// The private marker [`strip_quotes`] writes before a substitution found inside
/// double quotes, so [`split_segments`] reads it as [`QUOTED_SUBSTITUTION`]. A
/// control character no shell word carries; literal ones in a line are dropped,
/// so it cannot be spoofed.
const QUOTE_SUB_MARK: char = '\u{1}';

/// Substitutions nested deeper than this are not read: the line is refused
/// ([`prelex`]), so no reader after it recurses further.
pub(crate) const MAX_SUBSTITUTION_DEPTH: usize = 16;

/// Why an unquoted `(` inside a word is no read: zsh reads it as a glob group or
/// qualifier, and the `e:…:` and `+cmd` qualifiers run a command during
/// expansion (`ls -d /(e:'print -u2 X':)`, `echo $(print /)(e:…:)`, `ls -d
/// /(+print)`).
const GLOB_QUALIFIER_RUNS: &str =
    "a ( inside a word (zsh reads a glob qualifier, and `(e:…:)` or `(+cmd)` runs a command)";

/// Why zsh's `~` parameter flag is no read: `$~N`, `$^~N`, `$~^N` (and `${~N}`,
/// refused by [`braced_is_literal`]) glob the value as a pattern, whose
/// qualifier runs a command (`N='/(e:print -u2 X:)'; echo $~N`). Refused
/// inside double quotes too.
const GLOB_SUBST_RUNS: &str = "zsh's $~ flag (the value is globbed as a pattern, and a qualifier `(e:…:)` in it runs a command)";

/// The python scripts run as reads when no `--allow-python` glob is given:
/// none. A script's name is no evidence of what it does; a project names its
/// generators with `--allow-python`, e.g. `scripts/*standing*.py`.
pub const DEFAULT_PYTHON_ALLOW: &[&str] = &[];

/// Programs a segment may start with and still be a read. Shell keywords are
/// here because a `for`/`if` segment's head is the keyword; the command after
/// `do`/`then` is checked too ([`segment_head`]). `let` and `local` are not:
/// both evaluate arithmetic, whose array subscripts expand (`let
/// 'x=a[$(touch M)]'` runs `touch`).
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
/// is `rm x`. Stripped before the head check so the keyword cannot launder it.
const KEYWORD_PREFIX: &[&str] = &[
    "do", "then", "else", "if", "elif", "while", "until", "!", "{",
];

/// Whether a word closes a `{` group in zsh (the other closing word is `]]`,
/// [`danger_scan`]). zsh runs a command written after a closing word in the
/// same list (`{ ls } always { cmd }`, `if [[ -n a ]] cmd`, `while [[ -z $d
/// ]] cmd`), so a closing word must end its segment or be followed only by
/// its compound's redirects and closing words (a word after such a redirect
/// is a parse error). A `}` on a line with no `{` word, or `]]` with no `[[`,
/// closes nothing.
///
/// A `}` closes as itself or glued to a word's end, unless the word's own
/// braces open it: `{ echo a} always { cmd }` and `{ echo ${HOME}} always {
/// cmd }` run `cmd` (a quoted `}` reaches this as `""`), while `{a,b}`,
/// `${HOME}`, `{}` and an escaped `\}` close nothing.
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

/// Variables whose assignment changes what a later program runs or reads: the
/// search path, shell startup and splitting, pagers and editors a read tool may
/// exec, git and rg config roots. `PATH` is allowed when every entry is a
/// [`SYSTEM_BIN_DIRS`] directory. zsh's own ([`ZSH_RUNS_A_VALUE`]): `NULLCMD`
/// and `READNULLCMD` name the command a bare redirect runs, `MODULE_PATH` is
/// where autoloaded modules load from, and `STTY` in a command prefix runs as
/// code on a terminal. Arrays zsh ties to these count ([`ZSH_TIED`]).
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
/// (`path=/tmp/x ls` runs `/tmp/x/ls`). Every pair `zsh -f` declares, checked
/// by `zsh_tied_arrays_are_the_measured_ones`. An array assignment is judged
/// as its scalar's ([`assignment_hazard`]).
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
/// program, function, command line and directory. Re-checked by
/// `zsh_parameters_it_runs_are_the_measured_ones` where zsh is installed.
/// `STTY` runs only on a terminal and is in [`HAZARD_VARS`] separately.
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
    // (b) Quoted strings are opaque to the danger scan; a substitution they
    // held is marked ([`QUOTE_SUB_MARK`]) as a one-word
    // [`QUOTED_SUBSTITUTION`].
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
    // (f) The words zsh evaluates as arithmetic.
    if let Some(reason) = arithmetic_scan(&cmd) {
        return Verdict::no(reason);
    }
    Verdict {
        read_only: true,
        reason: "every segment read-only".to_string(),
    }
}

/// The rest of an rm line judged as a read: [`classify_command_with`] over every
/// segment except the `rm` commands (any letter case, after assignments),
/// assignment-only segments and the `mktemp` commands the rm resolver follows
/// (`supervise::policy::rm_breaker`). The rm rule requires this besides the
/// resolver's verdict. The skipped segments' assignments ([`assignment_hazard`]:
/// `REPORTTIME=A; cat f | cat; rm …` runs `A`) and redirects
/// ([`redirect_hazard`]: `mktemp > ~/.zshrc; rm …` truncates the file) are
/// still judged.
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

/// `Ok` when `line` is PLAIN: written only in the part of zsh's grammar that
/// expands nothing, so no zsh form this module has not measured can hide in
/// it. What `approve = "safe"`'s read-only rule also requires of a line the
/// Bash tool may run under zsh (decided 2026-09-30 under the owner's standing
/// direction, closing the audit's "the zsh sweeps are exhaustive only over the
/// forms tried": [`classify_command_with`] refuses the forms it knows, and
/// this admits only what cannot be one). It is a list of SIMPLE commands: no
/// reserved word opens one ([`ZSH_RESERVED`]: `for path in …` assigns zsh's
/// tied `path`, `repeat` evaluates arithmetic), a leading redirect included
/// (`2>/dev/null for …` is the loop). Outside single quotes the line
/// holds no `$`, backtick, backslash, `(`, `)`, `{`, `}` or `[[`; no word
/// that opens with `=`, and no `=` in a word that is not a flag (an
/// assignment, `path=` among them); a `~` only as `~` or `~/…`; a `<` only as
/// a plain input redirect; a `>` only onto `/dev/null` or a descriptor; and
/// no control character. Inside double quotes: no `$`, backtick or
/// backslash. A `#` that opens a word comments to the end of its line. `Err`
/// names the first thing that is not plain.
pub(crate) fn plain_zsh_line(line: &str) -> Result<(), String> {
    #[derive(PartialEq)]
    enum Quote {
        None,
        Single,
        Double,
    }
    let chars: Vec<char> = line.chars().collect();
    let boundary = |c: Option<&char>| c.is_none_or(|c| " \t\n;|&".contains(*c));
    let mut quote = Quote::None;
    // The current word's unquoted text, whether a quote has opened in it,
    // whether it is a `<` redirect's target, and whether it stands where a
    // command's first word does.
    let mut word = String::new();
    let mut quoted = false;
    let mut target = false;
    let mut command = true;
    // A word ends: one that opens a command may not be a reserved word, a
    // separator makes the next word a command's first, and a redirect's
    // target (like its `2` in `2>`) leaves that slot to the word after it —
    // zsh reads `</dev/null for …` as a loop.
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
        match quote {
            Quote::Single => {
                if c == '\'' {
                    quote = Quote::None;
                }
            }
            Quote::Double => match c {
                '"' => quote = Quote::None,
                '$' | '`' | '\\' => return Err(format!("`{c}` inside double quotes")),
                _ => {}
            },
            Quote::None => match c {
                '\'' => (quote, quoted) = (Quote::Single, true),
                '"' => (quote, quoted) = (Quote::Double, true),
                ' ' | '\t' => end_word(&mut word, &mut quoted, &mut target, false)?,
                '\n' | ';' | '|' | '&' => end_word(&mut word, &mut quoted, &mut target, true)?,
                '#' if at_start => {
                    while i < chars.len() && chars[i] != '\n' {
                        i += 1;
                    }
                    continue;
                }
                '$' | '`' | '\\' | '(' | ')' | '{' | '}' => return Err(format!("`{c}`")),
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
                // Whatever the word before it (a descriptor, `&>`, or an
                // argument it ends), a `>` writes only where its target says.
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

/// A segment the rm rule's resolver reads as a guard on a `$(mktemp -d)`
/// (`supervise::policy::rm_breaker`): `exit` or `exit <n>` (`D=$(mktemp -d)
/// || exit 1`), and `:` with its words (`: "${D:?}"`), which the shell
/// expands and runs nothing of. Neither writes; a redirect on either, and a
/// substitution in a word, are still judged ([`danger_scan`],
/// [`program_scan`], which see every segment). `exit` takes digits only:
/// zsh evaluates its word as arithmetic, whose array subscript runs a
/// command substitution from inside quotes (`exit 'a[$(cmd)]'` runs `cmd`,
/// measured, zsh 5.9), and `exit 1 2` does not exit zsh at all.
fn is_mktemp_guard(seg: &[String]) -> bool {
    match seg {
        [head, ..] if head == ":" => true,
        [head] => head == "exit",
        [head, n] => head == "exit" && !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()),
        _ => false,
    }
}

/// The shell's reading of the parts the segment readers get wrong, applied
/// first: `Ok` is the line with `#` comments and here-document bodies removed
/// (newlines kept), `$"…"` as `"…"` and a backslash-free `$'…'` as `'…'`; `Err`
/// names a construct this reader will not follow (not read-only).
///
/// Substitutions nested deeper than [`MAX_SUBSTITUTION_DEPTH`] are refused here,
/// so later readers never recurse past it.
///
/// Why: a comment's apostrophe would open a quote the shell never opened,
/// hiding later lines (`git log # what's new⏎git push --force`); a quote in a
/// here-document body does the same; `$'a\'b'` ends where `'…'` would not. A
/// body under an unquoted terminator still expands `$(…)` and backticks, so it
/// is refused, as is a `${…}` holding a quote or substitution, and any quote,
/// substitution or here-document left open.
pub(crate) fn prelex(src: &str) -> Result<String, String> {
    let mut lx = Prelex {
        chars: src.chars().collect(),
        i: 0,
        out: String::with_capacity(src.len()),
        heredocs: Vec::new(),
        depth: 0,
        backticks: 0,
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
    /// How many substitutions the reader is inside.
    depth: usize,
    /// How many of those are backtick substitutions.
    backticks: usize,
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

    /// Whether the unquoted `(` at the cursor is part of a word (after a letter,
    /// quote, `)`, `}`, glob, `~`, `=` or `<…>` range), which zsh reads as a glob
    /// group or qualifier ([`GLOB_QUALIFIER_RUNS`]). Not one opening a subshell,
    /// group or `((…))`, nor `<(…)`, `>(…)` or `=(…)`, whose bodies are commands.
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
    /// substitution. Returns having consumed the closing character.
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
    /// flag ([`GLOB_SUBST_RUNS`]). The same bare, in double quotes, or in a
    /// `${…}` operand ([`Self::braced_parameter`]).
    fn parameter_hazard(&self) -> Option<String> {
        let after = &self.chars[self.i + 1..];
        // zsh's flags before a bare name (`$=V`, `$^V`, `$+V`, `$#V`), in
        // any order: `~` among them globs the value.
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
    /// terminator lines, dropping them from the output. An unterminated body
    /// runs to the end of the line, as the shell reads it.
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
/// integer, `@` or `*`, and a substring offset or length is an integer. The name
/// is a plain, positional or special parameter, optionally led by `#`; a `!` is
/// `${!}` or an index/name listing, never an indirection; no `${NAME@OP}`; any
/// unknown form (including zsh's `${(e)x}` flags) is refused. Default-value and
/// pattern forms evaluate no arithmetic, but `:=` and `=` assign, and zsh
/// evaluates some variables as arithmetic later (`${REPORTTIME:=A}` when a job
/// ends), so such an assignment is judged as `NAME=v` is.
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
            // not read as a name in turn.
            return Ok(());
        }
        if body.starts_with('!') {
            // bash's `${!NAME…}` expands the variable named by NAME's value,
            // subscript and all (`A='a[$(cmd)]'; echo "${!A}"` runs `cmd`).
            // Only the listings stay: `${!A[@]}`/`${!A[*]}` (indices) and
            // `${!A@}`/`${!A*}` (names).
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
        // substitutions and all.
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
    // `${NAME:=v}` and `${NAME=v}` assign `NAME`: judged as `NAME=v`
    // ([`assignment_hazard`]).
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
/// then `[`. zsh subscripts special parameters too (`A='path[$(cmd)]'; echo
/// $*[A]` runs `cmd`), and a lone `#` flag is `$#` itself.
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
/// ([`dollar_arithmetic`]). The body expands as a double-quoted word.
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
/// ([`is_alarm_body`]). Any other `perl -e` stays and the danger scan refuses
/// it.
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
/// backtick inside a backtick substitution, none at the top. A literal
/// [`QUOTE_SUB_MARK`] the line carries is dropped, so it cannot be spoofed in.
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
                        // A parameter inside double quotes: one word whose
                        // value this check cannot read ([`QUOTED_PARAMETER`]).
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
                // function writes may reach the splitter.
                if c != QUOTE_SUB_MARK {
                    out.push(c);
                }
                *i += 1;
            }
        }
    }
}

/// Split on `;`, `&`, `&&`, `||`, `|`, `(`, `)` and newline; each segment is its
/// whitespace-split tokens. A substitution is not a separator but one word
/// ([`SUBSTITUTION`], glued to adjacent text), so `$(printf rm) -rf /` has an
/// unreadable head and `git tag $(date +%s)` keeps its positional; its body
/// becomes segments of its own, first, since it runs first. An escaped
/// character (find's `\;`) is literal; a redirect's `&` (`2>&1`, `&>f`, `<&3`)
/// stays in its token, so only job-control `&` splits. A `>`, `<` or `&>` glued
/// to the end of a non-descriptor word starts its own token, as the shell
/// reads `rm>/dev/null -rf /`.
fn split_segments(s: &str) -> Vec<Vec<String>> {
    let chars: Vec<char> = s.chars().collect();
    let mut segments = Vec::new();
    let mut i = 0;
    split_level(&chars, &mut i, None, &mut segments);
    segments
}

/// One level of [`split_segments`]: the top (`stop` none), a `$(…)` body
/// (`)`) or a backtick body. Returns having consumed the closing character.
fn split_level(chars: &[char], i: &mut usize, stop: Option<char>, segments: &mut Vec<Vec<String>>) {
    let mut cur = String::new();
    let flush = |cur: &mut String, segments: &mut Vec<Vec<String>>| {
        let toks: Vec<String> = cur.split_whitespace().map(str::to_string).collect();
        if !toks.is_empty() {
            segments.push(toks);
        }
        cur.clear();
    };
    // Subshell parens open at this level: a `)` closes the substitution only
    // when none is.
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
            // A substitution [`strip_quotes`] flagged as double-quoted: one
            // word ([`QUOTED_SUBSTITUTION`]), never split, its body still run.
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
                // A stray mark (no substitution follows): drop it (the outer
                // `*i += 1` consumes it).
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

/// The program name of a token: its basename, so `/bin/rm` and `rm` agree.
pub(crate) fn program(tok: &str) -> &str {
    tok.rsplit('/').next().unwrap_or(tok)
}

/// An output redirect token: `Some(target)` when `tok` redirects (`>`, `>>`,
/// `2>`, `&>`, `>|`, `>file`); `None` when the target is the next token.
fn redirect_target(tok: &str) -> Option<Option<&str>> {
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

/// A redirect target that writes no file: `/dev/null`, a descriptor (`&1`) or a
/// close (`&-`). `>&word` with another word writes both streams to `word`.
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

/// The negative filter over every token: a file redirect, `find -delete` /
/// `-exec <writer>`, `sed -i`/`-I`, an inline-code interpreter, a python
/// heredoc, `xargs` feeding a writer, a git write or a git flag that runs a
/// program or writes a file, the equivalent flags of `rg`, `sort`, `less` and
/// `printf`, a clock-setting `date`, a word after a closing `}`
/// ([`closes_a_brace`]) or `]]`, and an assignment to a [`HAZARD_VARS`]
/// variable. Program names are the head check's job.
fn danger_scan(segments: &[Vec<String>]) -> Option<String> {
    // A `}` or `]]` closes something only on a line with a `{` or `[[`
    // word, before it or not, and a `}` glued to a word's end closes too
    // ([`closes_a_brace`]).
    let has = |open: &str| segments.iter().flatten().any(|t| t == open);
    let (braces, brackets) = (has("{"), has("[["));
    let closing = |t: &str| (braces && closes_a_brace(t)) || (brackets && t == "]]");
    for seg in segments {
        if let Some(reason) = loop_assignment_hazard(seg) {
            return Some(reason);
        }
        for (i, tok) in seg.iter().enumerate() {
            let prog = program(tok);
            let next = seg.get(i + 1).map(String::as_str);
            let next_prog = next.map(program);
            if let Some(reason) = assignment_hazard(tok) {
                return Some(reason);
            }
            if closing(tok) {
                // The compound's own redirects (`{ …; } 2>&1`, `[[ … ]]
                // 2>/dev/null`) are judged as every redirect is.
                let mut k = i + 1;
                while let Some(span) = seg.get(k).map(|t| redirect_span(t)).filter(|&n| n > 0) {
                    k += span;
                }
                if seg.get(k).is_some_and(|n| !closing(n)) {
                    return Some(format!("a word after a closing {tok} (zsh may run it)"));
                }
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
                // `[[` arithmetic comparisons, and `-v name[sub]` in `[[`, `[`
                // and `test`, evaluate arithmetic, running substitutions held in
                // a value's subscript. `[`/`test` `-eq` compare without evaluating.
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
                // A word the shell brace-expands or globs may become `-t` and
                // its operand (`[ {-t,A} ]` is `[ -t A ]`; `[ * ]` beside files
                // `-t` and `S=5` is `[ -t S=5 ]`), whose arithmetic
                // [`arithmetic_scan`] cannot see; a word starting with `/`
                // expands to paths only. `[[` expands neither.
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
            if prog == "sed" {
                // macOS sed edits in place under `-I` as under `-i`, alone or
                // in a cluster (`-nI`).
                for t in &seg[i + 1..] {
                    if t == "--in-place"
                        || t.starts_with("--in-place=")
                        || (t.starts_with('-')
                            && !t.starts_with("--")
                            && t[1..].contains(['i', 'I']))
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
                // Read up to the segment's end.
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
        }
    }
    None
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
            // `-A<n>` `-B<n>` `-C<n>` `-e<pattern>` `-f<file>` `-m<n>`: the
            // rest of the cluster is the value.
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
/// [`HAZARD_VAR_PREFIXES`] family). `PATH` passes only when every entry is a
/// literal system directory (`env -i PATH=/bin ls`). Arrays zsh ties to a
/// scalar are judged as the scalar ([`ZSH_TIED`]).
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
/// in 'path[$(cmd)]'` runs `cmd`). So every name the loop assigns is judged
/// with every word it may take ([`assignment_hazard`]); a loop whose words are
/// not in the segment (`for N; do`, zsh's `for N (…)`) is judged with an
/// unreadable word. [`HAZARD_VARS`] are refused the same way.
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
/// assigned ([`plain_number`]). Found by testing every declared or documented
/// name: those that evaluate at assignment, `MBEGIN`/`MEND` after a matching
/// `[[ … =~ … ]]`, and three evaluated on use (`DIRSTACKSIZE` at a `cd`,
/// `REPORTTIME` and `REPORTMEMORY` when a forked job ends). Re-checked by
/// `zsh_arithmetic_vars_are_the_measured_ones` where zsh is installed.
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

/// The characters by which zsh expands a word into more file names than it
/// spells: `*`, `?`, `[`, and under `EXTENDED_GLOB` (which a user's `setopt`
/// may carry into Claude Code's shell snapshot) `^`, `#` and a `(` group (see
/// [`GLOB_QUALIFIER_RUNS`]). `[ ^-vS ]` beside files `-t`, `-vS`, `S=5` is `[ -t
/// S=5 ]`. Its `~` only excludes names, but changes the word's text, so
/// [`expands_apart`] counts it.
pub(crate) const GLOB_CHARS: &[char] = &['*', '?', '[', '^', '#', '('];

/// Whether the shell may make other words of the quote-stripped word `t`: an
/// unquoted `{` it brace-expands, a [`GLOB_CHARS`] character, or an
/// `EXTENDED_GLOB` `~` past the first character (a leading one is one path).
fn expands_apart(t: &str) -> bool {
    t.contains('{') || t.contains(GLOB_CHARS) || t.chars().skip(1).any(|c| c == '~')
}

/// Read tools whose write or run form an argument selects (`sort -o`, `rg
/// --pre`, `find -delete`, `test -v`, `uniq IN OUT`, `date -s`, `less +…`, a
/// compiling `trustc`): an expansion before `--` may be that flag, so it is
/// refused ([`expansion_in_an_option_slot`]). `test`/`[` go by argument count
/// ([`test_expansions_are_operands`]); `printf` is judged up to its format
/// ([`runs_or_writes_by_flag`]); `git` has its own arm in [`head_from`].
const SELECTED_BY_AN_ARGUMENT: &[&str] = &[
    "sort", "tree", "sed", "uniq", "less", "more", "sysctl", "file", "date", "rg", "find", "test",
    "[", "trustc", "rustc",
];

/// Whether `tok` carries an expansion whose value is unreadable: a substitution
/// ([`SUBSTITUTION`], [`QUOTED_SUBSTITUTION`]), a double-quoted parameter
/// ([`QUOTED_PARAMETER`]), or an unquoted `$X`. Single-quoted and escaped `$`
/// are literal.
fn has_expansion(tok: &str) -> bool {
    let b = tok.as_bytes();
    b.iter()
        .enumerate()
        .any(|(k, &c)| c == b'$' && (k == 0 || b[k - 1] != b'\\'))
}

/// The first word in `args` carrying an expansion ([`has_expansion`]) where
/// `head` reads options: anywhere before `--`, since even a quoted expansion
/// can start with `-`. `test`/`[` use [`test_expansions_are_operands`].
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
    let end = args.iter().position(|t| t == "--").unwrap_or(args.len());
    args[..end]
        .iter()
        .find(|t| has_expansion(t))
        .map(String::as_str)
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
/// ]`, `[ X -gt Y ]`, `[ ! -f X ]`). An unquoted expansion can split into
/// another form (`[ $n ]` with `n='-v a[$(cmd)]'`), so it is never an operand.
/// `-v` and `-R` evaluate a subscript and are excluded; numeric primaries do
/// not evaluate arithmetic in `[`/`test`. Four or more words: no expansion.
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

/// Unary `test`/`[` primaries whose operand is a string or file
/// ([`test_expansions_are_operands`]). `-t`'s operand is zsh arithmetic
/// ([`arithmetic_scan`]); `-v` and `-R` evaluate a variable's subscript, so
/// they are excluded.
pub(crate) const TEST_UNARY: &[&str] = &[
    "-b", "-c", "-d", "-e", "-f", "-g", "-h", "-k", "-p", "-r", "-s", "-t", "-u", "-w", "-x", "-L",
    "-O", "-G", "-N", "-S", "-z", "-n",
];

/// Binary `test`/`[` primaries ([`TEST_UNARY`]). Numeric ones do not evaluate
/// operands as arithmetic in `[`/`test` (only bash's `[[` does).
pub(crate) const TEST_BINARY: &[&str] = &[
    "=", "==", "!=", "<", ">", "-nt", "-ot", "-ef", "-eq", "-ne", "-gt", "-ge", "-lt", "-le",
];

/// Flags by which a read tool at a segment head runs a program or writes: `rg
/// --pre`, `sort --compress-program=`, `printf -v NAME`, `less`/`more` with
/// `+<command>`, a log or key file, `sysctl name=value`/`-w`, `hostname
/// <name>`/`-F`, `file -C`, `tree -R`, `test -v`, and `date` setting the clock
/// (`-s`, or a positional without BSD's `-j`); and for
/// [`SELECTED_BY_AN_ARGUMENT`] tools, any substitution among the arguments.
fn runs_or_writes_by_flag(head: &str, args: &[String]) -> Option<String> {
    let flag = |t: &str| format!("{head} {t}");
    let unreadable = |t: &str| format!("{head} {t} (a word this check cannot read may be a flag)");
    if SELECTED_BY_AN_ARGUMENT.contains(&head)
        && let Some(t) = expansion_in_an_option_slot(head, args)
    {
        return Some(unreadable(t));
    }
    match head {
        "rg" => args
            .iter()
            .find(|t| *t == "--pre" || t.starts_with("--pre="))
            .map(|t| flag(t)),
        "sort" => args
            .iter()
            .find(|t| t.starts_with("--compress-program"))
            .map(|t| flag(t)),
        "printf" => {
            // Its options, then its format: a substitution among them may be
            // `-v NAME`, as may a brace-expanded (`{-vS,%s}`) or globbed (`-*`,
            // [`expands_apart`]) word; after the format every word is data.
            let format = args.iter().position(|t| !t.starts_with('-'));
            let opts = &args[..format.map_or(args.len(), |k| k + 1)];
            opts.iter()
                .find(|t| has_expansion(t) || expands_apart(t))
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
    head_from(seg, 0, python_allow)
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
/// `time [-p]`, and keyword prefixes (`do`, `then`, `if`), in any order.
fn skip_prefixes(seg: &[String], mut j: usize) -> usize {
    loop {
        let start = j;
        while let Some(t) = seg.get(j).map(String::as_str) {
            if is_assignment(t) || KEYWORD_PREFIX.contains(&t) || t == "-" {
                j += 1;
            } else if redirect_span(t) > 0 {
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
    j
}

/// What the wrapper at `seg[j]` (`xargs`, `env`, by name or from a system
/// directory) runs. `None`: no wrapper. `Some(Ok(None))`: it runs no word of the
/// line (bare `env` prints, bare `xargs` runs `echo`).
/// `Some(Ok(Some((k, dirs))))`: the command at `k`, in each `env -C` directory
/// in `dirs`. `Some(Err(reason))`: it hides its command (`env -S`) or chooses
/// its program itself (`env -P`).
///
/// `env`'s short options are read as `getopt` does: a cluster ends at the first
/// value-taking option, whose value is the rest of the word or the next word.
fn wrapper(seg: &[String], j: usize) -> Option<Wrapped<'_>> {
    let tok = seg.get(j)?.as_str();
    if tok.contains('/') && !is_system_bin(tok) {
        return None;
    }
    match program(tok) {
        "xargs" => {
            // `xargs [flags] [cmd [args]]`: the fed command is the head that
            // counts.
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
            // `env [-i] [-u NAME] [-C DIR] [VAR=v]... [cmd]`.
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

/// The command a segment runs, as [`head_from`] sees it: past prefixes
/// ([`skip_prefixes`]) and through wrappers ([`wrapper`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Runs<'s> {
    /// The index of the command's word in the segment.
    pub at: usize,
    /// The `env -C`/`--chdir` directories on the way, outermost first: the
    /// command runs in one of them, relative to the shell's own.
    pub chdir: Vec<&'s str>,
    /// Whether an `xargs` feeds it words the line does not show.
    pub fed: bool,
}

/// What segment `seg` runs ([`Runs`]): `Ok(None)` for no word of the line
/// (only assignments, bare `env` or `xargs`), `Err` when a wrapper hides or
/// picks the program ([`wrapper`]). Shared by the head check and
/// `supervise::policy::git_config` so neither misses a wrapper.
pub(crate) fn runs(seg: &[String]) -> Result<Option<Runs<'_>>, String> {
    let mut j = 0;
    let mut chdir = Vec::new();
    let mut fed = false;
    loop {
        j = skip_prefixes(seg, j);
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
            }
        }
    }
}

/// The segments of `line` as shell words ([`program_words`]), read as
/// [`classify_command_with`] reads it: through [`prelex`], with the `perl -e
/// 'alarm N; exec @ARGV'` timeout wrapper removed.
pub(crate) fn command_words(line: &str) -> Result<Vec<Vec<String>>, String> {
    Ok(program_words(&strip_alarm_idiom(&prelex(line)?)))
}

/// The head check from token `j`, re-entered for a wrapper's command (`xargs
/// <cmd>`, `env [VAR=v] <cmd>`, `timeout N <cmd>`), so a listed wrapper cannot
/// launder what it runs: `ls | xargs touch` is `touch`.
fn head_from<S: AsRef<str>>(seg: &[String], j: usize, python_allow: &[S]) -> Option<String> {
    let j = skip_prefixes(seg, j);
    let head_tok = seg.get(j)?;
    if is_fragment(head_tok) {
        return None;
    }
    if head_tok.contains('$') {
        // A substitution or parameter head: only the shell knows what it
        // names, so it cannot be cleared.
        return Some(format!(
            "a command whose name this check cannot read ({head_tok})"
        ));
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
        "xargs" | "env" => match wrapper(seg, j) {
            // The command the wrapper runs is the head that counts; a bare
            // `xargs` runs echo, a bare `env` prints the environment.
            Some(Err(reason)) => Some(reason),
            Some(Ok(Some((k, _)))) => head_from(seg, k, python_allow),
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
            // An expansion in a git read's arguments before `--` is unreadable:
            // it may be a verb (`remote "$(echo add)"`), change the arity
            // (`config` get vs set) or be a flag (`diff "$(echo
            // --output=f)"`). After `--` every word is a path.
            let before_dashes = args.iter().position(|a| a == "--").unwrap_or(args.len());
            if let Some(t) = args[..before_dashes].iter().find(|a| has_expansion(a)) {
                return Some(format!(
                    "git {sub} {t} (a word this check cannot read may be a flag or a verb)"
                ));
            }
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
                    // Short flags combine (`-avv`), so read by letter: d D m M
                    // c C u f t write; a r l list. A positional with no list
                    // flag creates a branch; `-v` is not a list flag.
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
        "export" => {
            // Each operand must be a literal NAME or NAME=value: `export
            // "$(…)"`, `export $X` and `export "PATH=/x"` assign an unnamed
            // variable, possibly `PATH`.
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
                    // zsh's integer and float types: the value is arithmetic,
                    // whose subscript runs a command.
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
                    // `scripts/$(…)_score.py` matches `scripts/*score*.py` as
                    // text, but the shell may make it any path.
                    Some(format!(
                        "{head} {path}: a script path this check cannot read"
                    ))
                } else if path.split('/').any(|c| c == "..") {
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

/// `aterm ctl` verbs that only read (screen, session state and history, the
/// roster), by verb and form: `meta set attention …` and `inbox seen` write,
/// so `meta` and `inbox` are judged separately. `image`, `window`, `video` and
/// `cast` write a file and are left out.
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
    "resizes",
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

/// `aterm …` (or `aterm-ctl` / `aterm-drive`): `None` when the form only reads:
/// `--version`, `help`, the [`ATERM_CTL_READ`] verbs, bare `meta`, bare `inbox`
/// and `inbox get <id>`, and the read verbs of `pkg` and `drive`.
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

/// The programs a line hands to awk and sed, read from raw words with quotes
/// resolved and expansions marked ([`program_words`]): `system(…)`, a redirect
/// or pipe in awk; `w`/`W`/`e` or `s///w`/`s///e` in sed; a program file (`-f`)
/// or a program supplied by a substitution or parameter. Every word is checked,
/// so `xargs awk …` is covered.
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

/// Whether zsh's `printf` reads its arguments as arithmetic under `format`: a
/// `*` width or precision, or any conversion but `%s`, `%b`, `%q`, `%c` and
/// `%%`. A length modifier or unknown letter reads as numeric. Unicode escapes
/// are decoded first, as zsh does ([`decode_unicode_escapes`]), so an escaped
/// `%` counts (octal and `x` escapes do not convert). An undecodable Unicode
/// escape reads as numeric.
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
/// 8. Other escapes stay as written. `None` for an escape with no hex digit or
/// no character.
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
/// in `test`, `[` and `[[`. zsh arithmetic runs substitutions in array
/// subscripts from quoted words, named variables' values, `"$A"`, or globbed
/// names, so each such word must be a plain number ([`plain_number`]).
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
/// quote, expansion or glob. Under `EXTENDED_GLOB` `#` repeats the previous
/// character, so `8#A` can glob to a file `A`, while `16#ff` and `2#101` match
/// only digit-led names ([`GLOB_CHARS`]).
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
    if program.contains(SUBSTITUTION) {
        return Some(format!("awk: a program this check cannot read ({program})"));
    }
    awk_program_writes(program)
        .then(|| "awk writes (system, a redirect or a pipe in the program)".to_string())
}

/// Whether an awk program does more than read: `system(…)`, `>>`, a coprocess,
/// a `>` redirect (not a comparison against a number, field or `>=`), or a `|`
/// to a command (not `||`, nor inside a regex or string).
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

/// The command as shell words with quotes resolved, segmented as
/// [`split_segments`] does (a substitution, even in double quotes, is a
/// [`SUBSTITUTION`] in its word, its body's words segments of their own,
/// first). Every unquoted or double-quoted parameter expansion is also marked a
/// [`SUBSTITUTION`], so a parameter-supplied awk program or sed script is
/// refused by [`program_scan`] like a substitution-supplied one.
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
        self.i += 1; // past `$`
        match self.chars.get(self.i) {
            Some('{') => {
                self.i += 1;
                while self.i < self.chars.len() && self.chars[self.i] != '}' {
                    self.i += 1;
                }
                if self.i < self.chars.len() {
                    self.i += 1; // past `}`
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
                // A lone `$` (no name after it: `sed "s/ *$//"`) is literal
                // to the shell, and so to the program it is handed to.
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
                    // [`split_segments`] does: `awk>/dev/null` is `awk` and a
                    // redirect.
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

/// Lines whose command this check could not name, or whose deciding argument
/// the split cut off, each with the rule that refuses it; each was once
/// classified read-only. Shared with `supervise::policy::approval`'s test,
/// which draws each as a box.
#[cfg(test)]
pub(crate) const MEASURED_SUBSTITUTION_BYPASSES: &[(&str, &str)] = &[
    // The substitution names the program; what follows it was read as a
    // flag-first fragment, or as a command of its own.
    ("$(printf 'r''m') -rf /usr", UNREADABLE_SUBSTITUTION),
    ("`printf %s r m` -rf /usr", UNREADABLE_SUBSTITUTION),
    ("$(printf 'r''m') -- /usr", UNREADABLE_SUBSTITUTION),
    ("$(printf 'r''m') 2>/dev/null /usr", UNREADABLE_SUBSTITUTION),
    ("$(printf 'r''m') ls", UNREADABLE_SUBSTITUTION),
    ("ls; $(printf 'r''m') -rf /usr", UNREADABLE_SUBSTITUTION),
    // Program words judged only at heads must still refuse these.
    ("$(which rm) -rf /usr", UNREADABLE_SUBSTITUTION),
    ("`echo rm` -rf /usr", UNREADABLE_SUBSTITUTION),
    // Behind an assignment, keyword, redirect or wrapper.
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
    // The same with no quote in the substitution, which the rm breaker's lexer
    // passes to `classify_except_rm`.
    (
        "ls | xargs $(echo qm | tr q r) -rf /usr",
        UNREADABLE_SUBSTITUTION,
    ),
    ("env $(echo qm | tr q r) -rf /usr", UNREADABLE_SUBSTITUTION),
    (
        "timeout 5 $(echo qm | tr q r) -rf /usr",
        UNREADABLE_SUBSTITUTION,
    ),
    // Inside another substitution, which runs too.
    (
        "echo $( $(printf 'r''m') -rf /usr )",
        UNREADABLE_SUBSTITUTION,
    ),
    (
        "echo \"$( $(printf 'r''m') -rf /usr )\"",
        UNREADABLE_SUBSTITUTION,
    ),
    // An argument the cut took from the command whose write form it decides.
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
