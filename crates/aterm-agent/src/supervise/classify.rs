// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Is this shell command line READ-ONLY? The one judgment a supervisor makes
//! hundreds of times a session, so it lives here as a pure function with the
//! corpus that shaped it pinned as tests. Every rule below was needed against a
//! real Claude Code worker: the danger scan is a NEGATIVE filter over every
//! token, the segment-head check a POSITIVE filter (an unknown program is not
//! read-only), and quoted strings are dropped BEFORE either runs so a `>` inside
//! a `git --format` string is not a redirect — except that the PROGRAMS a line
//! hands to awk and sed are read back from the raw words, since `system(…)`
//! and `s///w file` live inside those quotes. A wrapper (`xargs`, `env`,
//! `timeout`) is seen through to the command it runs, and `&` ends a segment
//! like `;`, so a backgrounded command is a head too. False negatives (a read
//! handed to the manager) cost a human a glance; a false positive would
//! auto-approve a write, so every tie breaks toward "not read-only".

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
/// is given: the standing / report / score generators of the owner's campaign.
pub const DEFAULT_PYTHON_ALLOW: &[&str] = &[
    "scripts/*standing*.py",
    "scripts/*report*.py",
    "scripts/*score*.py",
];

/// Programs a segment may START with and still be a read. Shell keywords are
/// here because a `for`/`if` segment's head is the keyword; the real command
/// after a `do`/`then` is checked too (see [`segment_head`]).
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
    "let",
    "local",
    "export",
    "cd",
    "tmutil",
];

/// Keyword heads that merely PREFIX the command a segment really runs: `do rm x`
/// is `rm x`. Stripped before the head check so the keyword cannot launder it.
const KEYWORD_PREFIX: &[&str] = &[
    "do", "then", "else", "if", "elif", "while", "until", "!", "{",
];

/// A danger token ANYWHERE — not only at a segment head — fails the line.
const DANGER: &[&str] = &[
    "rm",
    "mv",
    "cp",
    "tee",
    "chmod",
    "chown",
    "truncate",
    "dd",
    "sudo",
    "kill",
    "pkill",
    "killall",
    "cargo",
    "targo",
    "make",
    "npm",
    "pip",
    "pip3",
    "brew",
    "curl",
    "wget",
    "ssh",
    "scp",
    "rsync",
    "nohup",
    "open",
    "osascript",
    "eval",
    "source",
];

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
    // (a) The worker's timeout idiom is a wrapper, not a perl program.
    let cmd = strip_alarm_idiom(cmd);
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
/// so the command run after `ls &` is a head of its own.
fn split_segments(s: &str) -> Vec<Vec<String>> {
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

/// The program name of a token: its basename, so `/bin/rm` and `rm` agree.
fn program(tok: &str) -> &str {
    tok.rsplit('/').next().unwrap_or(tok)
}

/// An output redirect token: `Some(target)` when `tok` redirects (`>`, `>>`,
/// `2>`, `&>`, `>|`, `>file`); `None` when the target is the NEXT token.
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

fn redirect_is_safe(target: &str) -> bool {
    target.starts_with('&') || target == "/dev/null"
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

/// The NEGATIVE filter: any danger token anywhere, a redirect to a file, a
/// `find -delete` / `-exec <writer>`, `sed -i`, an inline-code interpreter, a
/// python heredoc, or `xargs` feeding a writer.
fn danger_scan(segments: &[Vec<String>]) -> Option<String> {
    for seg in segments {
        for (i, tok) in seg.iter().enumerate() {
            let prog = program(tok);
            let next = seg.get(i + 1).map(String::as_str);
            let next_prog = next.map(program);
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
            if DANGER.contains(&prog) {
                return Some(prog.to_string());
            }
            if tok == "." && i == 0 {
                return Some("source".to_string());
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
            if prog == "git"
                && let Some((sub_idx, sub)) = git_subcommand(seg, i)
            {
                let arg = seg.get(sub_idx + 1).map(String::as_str);
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
fn segment_head<S: AsRef<str>>(seg: &[String], python_allow: &[S]) -> Option<String> {
    head_from(seg, 0, python_allow)
}

/// The head check from token `j` on. Re-entered for the command a wrapper
/// hands its arguments to — `xargs <cmd>`, `env [VAR=v] <cmd>`, `timeout N
/// <cmd>` — so a wrapper on the read-only list cannot launder the program it
/// runs: `ls | xargs touch` is `touch`, `env FOO=1 ./deploy.sh` is `deploy.sh`.
fn head_from<S: AsRef<str>>(seg: &[String], mut j: usize, python_allow: &[S]) -> Option<String> {
    // Leading VAR=value assignments.
    while j < seg.len() && is_assignment(&seg[j]) {
        j += 1;
    }
    // A leading `timeout [flags] N`: flags, their numeric values, the duration.
    if seg.get(j).map(String::as_str) == Some("timeout") {
        j += 1;
        while j < seg.len() && (seg[j].starts_with('-') || is_duration(&seg[j])) {
            j += 1;
        }
    }
    // Keyword prefixes: the command after `do` / `then` / `if` is the one that runs.
    while j < seg.len() && KEYWORD_PREFIX.contains(&seg[j].as_str()) {
        j += 1;
    }
    let head_tok = seg.get(j)?;
    if is_fragment(head_tok) {
        return None;
    }
    let head = program(head_tok);
    let rest = &seg[j + 1..];
    let arg = rest.first().map(String::as_str);
    match head {
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
                    // d D m M c C u f t write; a r l v list. A positional with
                    // no list flag CREATES a branch (`git branch feature-x`).
                    let short = |a: &str| a.starts_with('-') && !a.starts_with("--") && a.len() > 1;
                    let write_short = args
                        .iter()
                        .any(|a| short(a) && a[1..].chars().any(|c| "dDmMcCuft".contains(c)));
                    let list_short = args
                        .iter()
                        .any(|a| short(a) && a[1..].chars().any(|c| "arlv".contains(c)));
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
                        "--verbose",
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

/// The programs a line hands to awk and sed, read from the RAW words (quotes
/// resolved, not dropped): `system(…)`, a redirect or a pipe in an awk
/// program; a `w`/`W`/`e` command or an `s///w` / `s///e` flag in a sed
/// script; a program file (`-f`) this scan cannot read. Every word is looked
/// at, not only heads, so `xargs awk …` and `timeout 5 sed …` are covered.
fn program_scan(cmd: &str) -> Option<String> {
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
fn raw_words(src: &str) -> Vec<Vec<String>> {
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
                true,
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
        assert_eq!(classify_command("./run.sh").reason, "run.sh");
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
        assert!(ro("git --git-dir=.git status"));
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
        assert!(ro("python3 scripts/sat_score_table.py"));
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
            "deploy.sh"
        );
        assert!(!ro("echo x | xargs python3 evil.py"));
        assert!(!ro("ls | xargs -I {} sh -c 'cat {}'"));
        assert!(ro("ls | xargs -n1 wc -l"));
        assert!(ro("ls | xargs -0 -I {} cat {}"));
        assert!(ro("ls | xargs"));
        assert_eq!(
            classify_command("env FOO=1 ./deploy.sh").reason,
            "deploy.sh"
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
        assert_eq!(classify_command("ls & ./deploy.sh").reason, "deploy.sh");
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
        assert!(!ro("python3 scripts/x/../wipe_report.py"));
        assert!(ro("python3 scripts/sat_score_table.py"));
        assert!(ro("python3 scripts/sub/report_x.py"));
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
