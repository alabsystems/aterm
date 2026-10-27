// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What the worker's Bash tool's shell carries in from the person's shell startup.

use std::path::{Path, PathBuf};

use super::git_config::WorkerEnv;
use crate::harness::footer::read_small;

/// The shell a snapshot was made from, by its file name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// zsh (`snapshot-zsh-…`).
    Zsh,
    /// bash (`snapshot-bash-…`).
    Bash,
}

impl Kind {
    const fn name(self) -> &'static str {
        match self {
            Self::Zsh => "zsh",
            Self::Bash => "bash",
        }
    }

    /// The kind Claude Code names a snapshot of `shell` by: `zsh` when the
    /// path says so, else `bash` when it says that (2.1.284's `srt`).
    fn of(shell: &str) -> Option<Self> {
        if shell.contains("zsh") {
            Some(Self::Zsh)
        } else if shell.contains("bash") {
            Some(Self::Bash)
        } else {
            None
        }
    }
}

/// Startup aliases and functions, plus whether the shell may be zsh ([`Self::may_run_zsh`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShellStartup {
    names: Vec<(String, String, bool)>,
    zsh: bool,
}

/// Words that make the shell parse text of the line as a command again, so
/// that any word of it may be one an alias is expanded at.
const REPARSE: &[&str] = &["eval", "trap", "emulate", "sched", "fc"];

impl ShellStartup {
    /// Whether the Bash tool may run the line under zsh.
    #[must_use]
    pub const fn may_run_zsh(&self) -> bool {
        self.zsh
    }

    /// Each alias and function read.
    #[must_use]
    pub fn names(&self) -> &[(String, String, bool)] {
        &self.names
    }

    /// The startup [`Self::names`] and [`Self::may_run_zsh`] describe.
    #[must_use]
    pub const fn of_parts(names: Vec<(String, String, bool)>, zsh: bool) -> Self {
        Self { names, zsh }
    }

    /// `Err` naming the alias or function `line` may run.
    pub fn admits(&self, line: &str) -> Result<(), String> {
        if self.names.is_empty() {
            return Ok(());
        }
        let texts = dequotings(line)?;
        let anywhere = |name: &str| texts.iter().any(|t| names_word(t, name));
        let reparse = REPARSE.iter().any(|w| anywhere(w));
        let heads = command_heads(line);
        for (name, what, alias) in &self.names {
            let runs = if *alias && !reparse {
                heads.iter().any(|h| names_word(h, name))
            } else {
                anywhere(name)
            };
            if runs {
                return Err(format!(
                    "`{name}` is {what}: the Bash tool's shell runs it, not the command this \
                     check reads"
                ));
            }
        }
        Ok(())
    }
}

/// The most snapshots one check reads before it gives up (and escalates).
const MAX_SNAPSHOTS: usize = 64;
/// The most of one snapshot (or `.zshenv`) read; a larger one is unknown.
const MAX_FILE_BYTES: u64 = 8 << 20;
/// The most session directories under `session-env/` listed.
const MAX_SESSION_ENV_DIRS: usize = 4096;

/// The line Claude Code writes before its own additions to a snapshot (the
/// `rg` fallback, its `find`/`grep`/`pkill` shadows, `PATH`).
const CLAUDE_PART: &str = "# Check for rg availability";

/// zsh options (`setopt`) that do not affect non-interactive `zsh -c` command resolution or paths.
pub const ZSH_INERT: &[&str] = &[
    "alwayslastprompt",
    "noalwayslastprompt",
    "alwaystoend",
    "appendhistory",
    "noappendhistory",
    "autocd",
    "autocontinue",
    "autolist",
    "noautolist",
    "automenu",
    "noautomenu",
    "autoparamkeys",
    "noautoparamkeys",
    "autoparamslash",
    "noautoparamslash",
    "autopushd",
    "autoremoveslash",
    "noautoremoveslash",
    "autoresume",
    "banghist",
    "nobanghist",
    "bashautolist",
    "beep",
    "nobeep",
    "bgnice",
    "nobgnice",
    "cdsilent",
    "checkjobs",
    "nocheckjobs",
    "checkrunningjobs",
    "nocheckrunningjobs",
    "noclobber",
    "combiningchars",
    "completealiases",
    "completeinword",
    "correct",
    "correctall",
    "cshjunkiehistory",
    "debugbeforecmd",
    "nodebugbeforecmd",
    "dvorak",
    "emacs",
    "errexit",
    "errreturn",
    "evallineno",
    "noevallineno",
    "extendedglob",
    "noextendedglob",
    "nobareglobqual",
    "extendedhistory",
    "flowcontrol",
    "noflowcontrol",
    "globalrcs",
    "noglobalrcs",
    "globcomplete",
    "hashcmds",
    "nohashcmds",
    "hashdirs",
    "nohashdirs",
    "hashexecutablesonly",
    "hashlistall",
    "nohashlistall",
    "histallowclobber",
    "histbeep",
    "nohistbeep",
    "histexpiredupsfirst",
    "histfcntllock",
    "histfindnodups",
    "histignorealldups",
    "histignoredups",
    "histignorespace",
    "histlexwords",
    "histnofunctions",
    "histnostore",
    "histreduceblanks",
    "histsavebycopy",
    "nohistsavebycopy",
    "histsavenodups",
    "histsubstpattern",
    "histverify",
    "hup",
    "nohup",
    "ignoreeof",
    "incappendhistory",
    "incappendhistorytime",
    "interactivecomments",
    "nointeractivecomments",
    "listambiguous",
    "nolistambiguous",
    "listbeep",
    "nolistbeep",
    "listpacked",
    "listrowsfirst",
    "listtypes",
    "nolisttypes",
    "localloops",
    "localoptions",
    "localpatterns",
    "localtraps",
    "login",
    "longlistjobs",
    "mailwarning",
    "menucomplete",
    "monitor",
    "nomonitor",
    "noexec",
    "notify",
    "nonotify",
    "nounset",
    "overstrike",
    "pipefail",
    "printeightbit",
    "printexitvalue",
    "promptbang",
    "nopromptbang",
    "promptcr",
    "nopromptcr",
    "promptpercent",
    "nopromptpercent",
    "promptsp",
    "nopromptsp",
    "promptsubst",
    "pushdignoredups",
    "pushdminus",
    "pushdsilent",
    "pushdtohome",
    "rcs",
    "norcs",
    "recexact",
    "rmstarsilent",
    "rmstarwait",
    "sharehistory",
    "singlelinezle",
    "sourcetrace",
    "sunkeyboardhack",
    "transientrprompt",
    "verbose",
    "vi",
    "warncreateglobal",
    "warnnestedvar",
    "xtrace",
    "zle",
    "nozle",
];

/// bash `set -o` options (as the snapshot replays them: only those on) that change nothing about a command's words or which command runs.
pub const BASH_SET_INERT: &[&str] = &[
    "braceexpand",
    "emacs",
    "errexit",
    "errtrace",
    "functrace",
    "hashall",
    "histexpand",
    "history",
    "ignoreeof",
    "interactive-comments",
    "monitor",
    "noclobber",
    "noexec",
    "nolog",
    "notify",
    "nounset",
    "onecmd",
    "pipefail",
    "privileged",
    "verbose",
    "vi",
    "xtrace",
];

/// bash 3.2's shopts, each with its default in a `bash -l -c`.
pub const BASH_SHOPTS: &[(&str, Option<bool>)] = &[
    ("cdable_vars", Some(false)),
    ("cdspell", None),
    ("checkhash", None),
    ("checkwinsize", None),
    ("cmdhist", None),
    ("compat31", Some(false)),
    ("dotglob", Some(false)),
    ("execfail", Some(false)),
    ("expand_aliases", None),
    ("extdebug", Some(false)),
    ("extglob", None),
    ("extquote", Some(true)),
    ("failglob", Some(false)),
    ("force_fignore", None),
    ("gnu_errfmt", None),
    ("histappend", None),
    ("histreedit", None),
    ("histverify", None),
    ("hostcomplete", None),
    ("huponexit", None),
    ("interactive_comments", Some(true)),
    ("lithist", None),
    ("login_shell", None),
    ("mailwarn", None),
    ("no_empty_cmd_completion", None),
    ("nocaseglob", Some(false)),
    ("nocasematch", Some(false)),
    ("nullglob", Some(false)),
    ("progcomp", None),
    ("promptvars", None),
    ("restricted_shell", None),
    ("shift_verbose", None),
    ("sourcepath", Some(true)),
    ("xpg_echo", Some(false)),
];

/// Function names the shell runs whatever the line says: the zsh hooks a
/// `zsh -c` runs (`chpwd`, `zshexit`; `precmd`, `preexec`, `periodic` and
/// `zshaddhistory` never fire there), the handlers, and the words of the Bash
/// tool's own chain around the command (`eval` runs it).
const ALWAYS_RUN: &[&str] = &[
    "chpwd",
    "zshexit",
    "command_not_found_handler",
    "command_not_found_handle",
    "eval",
    "setopt",
    "shopt",
    "pwd",
    "true",
    "builtin",
    "source",
    "unalias",
    "unset",
    "export",
];

/// The variables Claude Code's settings may give the Bash tool that change
/// which shell runs a command, what it reads first or its options.
fn shell_relevant(key: &str) -> bool {
    matches!(
        key,
        "SHELL"
            | "CLAUDE_CODE_SHELL"
            | "CLAUDE_CODE_SHELL_PREFIX"
            | "CLAUDE_ENV_FILE"
            | "ZDOTDIR"
            | "HOME"
            | "BASH_ENV"
            | "ENV"
            | "SHELLOPTS"
            | "BASHOPTS"
            | "POSIXLY_CORRECT"
    )
}

/// The shells Claude Code's Bash tool may run for `worker` (2.1.284's `BMn`).
pub fn tool_kinds(worker: &WorkerEnv) -> Vec<Kind> {
    let mut out = Vec::new();
    if let Some(k) = worker.var("CLAUDE_CODE_SHELL").and_then(Kind::of) {
        out.push(k);
    }
    let shell = worker.var("SHELL").unwrap_or("");
    let fallback = Kind::of(shell).unwrap_or(Kind::Zsh);
    if !out.contains(&fallback) {
        out.push(fallback);
    }
    out
}

/// What the worker's Bash tool's shell starts with (module header).
pub fn read(worker: &WorkerEnv, launch: &Path) -> Result<ShellStartup, String> {
    for key in ["CLAUDE_CODE_SHELL_PREFIX", "CLAUDE_ENV_FILE"] {
        if worker.var(key).is_some_and(|v| !v.is_empty()) {
            return Err(format!(
                "the worker's `{key}` puts the person's own shell code before every command the Bash tool runs"
            ));
        }
    }
    worker.settings_env_where(launch, shell_relevant, "a command")?;
    let claude = worker
        .claude_dir(launch)
        .map_err(|why| format!("the worker's Claude Code directory is unknown: {why}"))?;
    hook_env_scripts(&claude)?;
    let kinds = tool_kinds(worker);
    let snapshots = snapshots(&claude, &kinds)?;
    let mut state = ShellStartup {
        zsh: kinds.contains(&Kind::Zsh),
        ..ShellStartup::default()
    };
    for (kind, path) in &snapshots {
        let text = read_small(path, MAX_FILE_BYTES + 1)
            .ok_or_else(|| format!("{} cannot be read", path.display()))?;
        if text.len() as u64 > MAX_FILE_BYTES {
            return Err(format!(
                "{} is larger than {MAX_FILE_BYTES} bytes",
                path.display()
            ));
        }
        parse_snapshot(*kind, &text, &path.display().to_string(), &mut state)?;
    }
    for kind in kinds {
        match kind {
            Kind::Zsh => zshenv(worker)?,
            Kind::Bash => bash_env(worker)?,
        }
    }
    Ok(state)
}

/// How [`read`]'s error starts when no snapshot stands ([`unwritten`]).
pub const NO_SNAPSHOT: &str = "no shell snapshot of ";

/// Whether `why`, a [`read`] error, says no snapshot stands: Claude Code writes one only once
/// its Bash tool has run a command in a session, and removes it when that session ends, so in a
/// config folder where no running session ran one yet there is none.
#[must_use]
pub fn unwritten(why: &str) -> bool {
    why.starts_with(NO_SNAPSHOT)
}

/// Every snapshot of a shell in `kinds` under `claude`'s `shell-snapshots/`;
/// `Err` when there is none ([`NO_SNAPSHOT`]), or more than [`MAX_SNAPSHOTS`].
fn snapshots(claude: &Path, kinds: &[Kind]) -> Result<Vec<(Kind, PathBuf)>, String> {
    let dir = claude.join("shell-snapshots");
    let names: Vec<&str> = kinds.iter().map(|k| k.name()).collect();
    let unknown = || {
        format!(
            "{NO_SNAPSHOT}{} in {}: the Bash tool's shell startup is unknown (Claude \
             Code writes one only once its Bash tool has run in a session)",
            names.join(" or "),
            dir.display()
        )
    };
    let entries = std::fs::read_dir(&dir).map_err(|_| unknown())?;
    let mut out = Vec::new();
    for entry in entries.filter_map(Result::ok) {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(rest) = name.strip_prefix("snapshot-") else {
            continue;
        };
        if !name.ends_with(".sh") {
            continue;
        }
        let Some(kind) = kinds
            .iter()
            .find(|k| rest.starts_with(&format!("{}-", k.name())))
        else {
            continue;
        };
        out.push((*kind, entry.path()));
        if out.len() > MAX_SNAPSHOTS {
            return Err(format!(
                "more than {MAX_SNAPSHOTS} shell snapshots in {}",
                dir.display()
            ));
        }
    }
    if out.is_empty() {
        return Err(unknown());
    }
    out.sort();
    Ok(out)
}

/// `Err` naming a non-empty SessionStart (or `Setup`, `CwdChanged`,
/// `FileChanged`) hook env script under `claude`'s `session-env/<session>/`,
/// which Claude Code prepends to every command of that session's Bash tool.
fn hook_env_scripts(claude: &Path) -> Result<(), String> {
    let root = claude.join("session-env");
    let Ok(sessions) = std::fs::read_dir(&root) else {
        return Ok(());
    };
    for (n, session) in sessions.filter_map(Result::ok).enumerate() {
        if n >= MAX_SESSION_ENV_DIRS {
            return Err(format!(
                "more than {MAX_SESSION_ENV_DIRS} sessions in {}",
                root.display()
            ));
        }
        let Ok(files) = std::fs::read_dir(session.path()) else {
            continue;
        };
        for file in files.filter_map(Result::ok) {
            let name = file.file_name();
            let is_script = name.to_str().is_some_and(|n| {
                ["setup", "sessionstart", "cwdchanged", "filechanged"]
                    .iter()
                    .any(|h| n.starts_with(&format!("{h}-hook-")))
                    && n.ends_with(".sh")
            });
            if is_script && file.metadata().map_or(true, |m| m.len() > 0) {
                return Err(format!(
                    "a hook's env script ({}) runs before every command of its session's Bash tool",
                    file.path().display()
                ));
            }
        }
    }
    Ok(())
}

/// Read one snapshot's person's part (everything above [`CLAUDE_PART`]).
pub fn parse_snapshot(
    kind: Kind,
    text: &str,
    source: &str,
    state: &mut ShellStartup,
) -> Result<(), String> {
    let lines: Vec<&str> = text.lines().collect();
    if lines.first().map(|l| l.trim_end()) != Some("# Snapshot file") {
        return Err(format!("{source} is no shell snapshot this check reads"));
    }
    state.zsh |= kind == Kind::Zsh;
    let end = lines
        .iter()
        .rposition(|l| l.trim_end() == CLAUDE_PART)
        .ok_or_else(|| format!("{source} is not a whole shell snapshot (no `{CLAUDE_PART}`)"))?;
    for (at, line) in lines.iter().enumerate().take(end).skip(1) {
        if line.starts_with(char::is_whitespace) {
            continue;
        }
        let words: Vec<&str> = line.split_whitespace().collect();
        let option = |what: &str| {
            format!(
                "the Bash tool's {} starts with {what} from the person's shell startup (line {} \
                 of Claude Code's shell snapshot {source})",
                kind.name(),
                at + 1
            )
        };
        let named = || option(&format!("`{}`", words.join(" ")));
        let unnamed = || {
            option(&format!(
                "a `{}` line this check does not read",
                words.first().copied().unwrap_or_default()
            ))
        };
        match (kind, words.as_slice()) {
            (Kind::Zsh, [name, "()", "{"]) => {
                define(
                    state,
                    unquote(name),
                    format!("a function in {source}"),
                    false,
                )?;
            }
            (Kind::Zsh, ["setopt", name]) => {
                if !ZSH_INERT.contains(&name.to_ascii_lowercase().replace('_', "").as_str()) {
                    return Err(named());
                }
            }
            (Kind::Zsh, ["setopt", ..]) => return Err(unnamed()),
            (Kind::Bash, ["set", "-o", name]) => {
                if !BASH_SET_INERT.contains(name) {
                    return Err(named());
                }
            }
            (Kind::Bash, ["shopt", flag @ ("-s" | "-u"), name]) => {
                match BASH_SHOPTS.iter().find(|(n, _)| n == name) {
                    Some((_, None)) => {}
                    Some((_, Some(default))) if *default == (*flag == "-s") => {}
                    _ => return Err(named()),
                }
            }
            (_, ["set" | "shopt" | "setopt" | "unsetopt" | "emulate", ..]) => {
                return Err(unnamed());
            }
            (_, ["alias", "--", ..]) => {
                let def = line.strip_prefix("alias -- ").unwrap_or_default();
                let name = alias_name(def)
                    .ok_or_else(|| format!("{source} has an alias this check cannot name"))?;
                define(state, name, format!("an alias in {source}"), true)?;
            }
            (Kind::Bash, ["eval", ..]) => {
                let name = line
                    .strip_prefix("eval $'")
                    .and_then(|r| r.split_once(" ()"))
                    .map(|(n, _)| n)
                    .filter(|n| !n.is_empty() && !n.contains(['\\', '\'', ' ']))
                    .ok_or_else(|| format!("{source} has a function this check cannot name"))?;
                define(
                    state,
                    name.to_string(),
                    format!("a function in {source}"),
                    false,
                )?;
            }
            _ => {}
        }
    }
    Ok(())
}

/// Record `name` (`what` it is, and whether an `alias`), or `Err` for one the
/// shell runs whatever the line says ([`ALWAYS_RUN`], zsh's `TRAP…`
/// functions); an alias of such a name is refused the same way.
fn define(state: &mut ShellStartup, name: String, what: String, alias: bool) -> Result<(), String> {
    if ALWAYS_RUN.contains(&name.as_str()) || name.starts_with("TRAP") {
        return Err(format!(
            "`{name}` is {what}, which the Bash tool's shell runs on every command or at events \
             no line names"
        ));
    }
    if !state
        .names
        .iter()
        .any(|(n, _, a)| *n == name && *a == alias)
    {
        state.names.push((name, what, alias));
    }
    Ok(())
}

/// An alias definition's name (`ll='ls -l'`, `'..'='cd ..'`).
fn alias_name(def: &str) -> Option<String> {
    let name = if let Some(rest) = def.strip_prefix('\'') {
        rest.split_once('\'')?.0
    } else {
        def.split_once('=')?.0
    };
    (!name.is_empty()).then(|| name.to_string())
}

fn unquote(word: &str) -> String {
    word.strip_prefix('\'')
        .and_then(|w| w.strip_suffix('\''))
        .unwrap_or(word)
        .to_string()
}

/// Whether `line` has a word spelled `name`.
pub fn names_word(line: &str, name: &str) -> bool {
    let word_char = |c: char| c.is_alphanumeric() || "_-.+@%,:=/~".contains(c);
    line.match_indices(name).any(|(at, _)| {
        let before = line[..at].chars().next_back();
        let after = line[at + name.len()..].chars().next();
        !before.is_some_and(word_char) && !after.is_some_and(word_char)
    })
}

/// The most times [`dequotings`] removes a level of quoting (an `eval` of
/// an `eval` …) before it gives up on the line.
const MAX_QUOTE_LEVELS: usize = 8;

/// `line` across quote-removal levels, plus variants with `\`, `'`, and `"` stripped.
pub fn dequotings(line: &str) -> Result<Vec<String>, String> {
    let strip = |t: &str| t.replace(['\\', '\'', '"'], "");
    let mut out = vec![line.to_string(), strip(line)];
    let mut cur = line.to_string();
    for _ in 0..MAX_QUOTE_LEVELS {
        let next = unquote_once(&cur);
        if next == cur {
            return Ok(out);
        }
        out.push(strip(&next));
        out.push(next.clone());
        cur = next;
    }
    Err(format!(
        "a line quoted more than {MAX_QUOTE_LEVELS} levels deep, which this check does not read"
    ))
}

/// One level of the shell's quote removal: `\c` is `c` (a backslash-newline
/// is nothing), `'…'` its text, `$'…'` its text with the escapes decoded
/// ([`ansi_c`]), `"…"` (and bash's `$"…"`) its text with `\$`, `` \` ``,
/// `\"`, `\\` and a backslash-newline undone.
fn unquote_once(s: &str) -> String {
    let cs: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < cs.len() {
        match cs[i] {
            '\\' => {
                if let Some(&n) = cs.get(i + 1)
                    && n != '\n'
                {
                    out.push(n);
                }
                i += 2;
            }
            '\'' => {
                i += 1;
                while i < cs.len() && cs[i] != '\'' {
                    out.push(cs[i]);
                    i += 1;
                }
                i += 1;
            }
            '$' if cs.get(i + 1) == Some(&'\'') => {
                i += 2;
                ansi_c(&cs, &mut i, &mut out);
            }
            '$' if cs.get(i + 1) == Some(&'"') => i += 1,
            '"' => {
                i += 1;
                while i < cs.len() && cs[i] != '"' {
                    if cs[i] == '\\' && cs.get(i + 1).is_some_and(|n| "$`\"\\\n".contains(*n)) {
                        if cs[i + 1] != '\n' {
                            out.push(cs[i + 1]);
                        }
                        i += 2;
                    } else {
                        out.push(cs[i]);
                        i += 1;
                    }
                }
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

/// The text of a `$'…'` from `cs[*i]` (just past its `$'`) to its closing
/// quote, which `*i` is left past, with zsh's and bash's escapes decoded: the
/// letters (`\n`, `\t`, `\e`, …), `\NNN` octal, `\xHH`, `\uHHHH`,
/// `\UHHHHHHHH` and `\cX`.
fn ansi_c(cs: &[char], i: &mut usize, out: &mut String) {
    let digits = |i: &mut usize, radix: u32, max: usize| {
        let mut v = 0u32;
        let mut n = 0;
        while n < max {
            match cs.get(*i).and_then(|c| c.to_digit(radix)) {
                Some(d) => {
                    v = v.saturating_mul(radix).saturating_add(d);
                    *i += 1;
                    n += 1;
                }
                None => break,
            }
        }
        (n > 0).then_some(v)
    };
    while *i < cs.len() {
        let c = cs[*i];
        *i += 1;
        if c == '\'' {
            return;
        }
        if c != '\\' {
            out.push(c);
            continue;
        }
        let Some(&e) = cs.get(*i) else {
            out.push('\\');
            return;
        };
        *i += 1;
        let code = match e {
            'a' => Some(7),
            'b' => Some(8),
            'e' | 'E' => Some(27),
            'f' => Some(12),
            'n' => Some(10),
            'r' => Some(13),
            't' => Some(9),
            'v' => Some(11),
            '0'..='7' => {
                *i -= 1;
                digits(i, 8, 3)
            }
            'x' => digits(i, 16, 2),
            'u' => digits(i, 16, 4),
            'U' => digits(i, 16, 8),
            'c' => cs.get(*i).map(|&x| {
                *i += 1;
                u32::from(x) & 0x1f
            }),
            other => {
                out.push(other);
                continue;
            }
        };
        match code.and_then(char::from_u32) {
            Some(ch) => out.push(ch),
            None => {
                out.push('\\');
                out.push(e);
            }
        }
    }
}

/// Every word of `line` that may stand in command position, where zsh and bash expand a plain alias (an over-approximation).
pub fn command_heads(line: &str) -> Vec<&str> {
    let separator = |c: char, prev: Option<char>| {
        matches!(c, ';' | '\n' | '(' | ')' | '{' | '}' | '`')
            || (matches!(c, '|' | '&') && !matches!(prev, Some('<' | '>')))
    };
    let cs: Vec<(usize, char)> = line.char_indices().collect();
    let mut heads = Vec::new();
    let mut cmdpos = true;
    let mut target = false;
    let mut k = 0;
    while k < cs.len() {
        let (at, c) = cs[k];
        let prev = k.checked_sub(1).map(|p| cs[p].1);
        if separator(c, prev) {
            cmdpos = true;
            target = false;
            k += 1;
            continue;
        }
        if c.is_whitespace() {
            k += 1;
            continue;
        }
        let mut end = k;
        while end < cs.len() {
            let (_, d) = cs[end];
            let before = end.checked_sub(1).map(|p| cs[p].1);
            if d.is_whitespace() || separator(d, before) {
                break;
            }
            end += 1;
        }
        let word = &line[at..cs.get(end).map_or(line.len(), |&(b, _)| b)];
        k = end;
        if word == "]]" {
            cmdpos = true;
            target = false;
            continue;
        }
        if !cmdpos {
            continue;
        }
        heads.push(word);
        let redirect = word.contains(['<', '>']);
        let keeps = target
            || redirect
            || KEEPS_COMMAND.contains(&word)
            || word.contains('=')
            || word.starts_with('-')
            || word.chars().all(|c| c.is_ascii_digit());
        target = redirect && word.ends_with(['<', '>', '&', '|']);
        cmdpos = keeps;
    }
    heads
}

/// Reserved words and precommand modifiers after which zsh or bash reads the
/// next word in command position.
const KEEPS_COMMAND: &[&str] = &[
    "if",
    "then",
    "else",
    "elif",
    "while",
    "until",
    "do",
    "!",
    "time",
    "nocorrect",
    "noglob",
    "command",
    "builtin",
    "exec",
    "-",
    "repeat",
    "coproc",
];

/// `Err` naming a `.zshenv` zsh reads before every Bash tool command (the
/// worker's `$ZDOTDIR`'s, else `$HOME`'s; `/etc/zshenv`; a Debian-built zsh's
/// `/etc/zsh/zshenv`) that holds what the snapshot would not show
/// ([`zshenv_hazard`]), or that cannot be read.
fn zshenv(worker: &WorkerEnv) -> Result<(), String> {
    let dot = worker
        .var("ZDOTDIR")
        .filter(|d| !d.is_empty())
        .or_else(|| worker.var("HOME").filter(|h| !h.is_empty()))
        .map(|d| Path::new(d).join(".zshenv"));
    for path in dot
        .into_iter()
        .chain(["/etc/zshenv", "/etc/zsh/zshenv"].map(PathBuf::from))
    {
        if std::fs::symlink_metadata(&path).is_err() {
            continue;
        }
        let text = read_small(&path, MAX_FILE_BYTES + 1)
            .filter(|t| t.len() as u64 <= MAX_FILE_BYTES)
            .ok_or_else(|| format!("{} cannot be read whole", path.display()))?;
        if let Some(what) = zshenv_hazard(&text) {
            return Err(format!(
                "{} runs before every command the Bash tool runs, and {what}",
                path.display()
            ));
        }
    }
    Ok(())
}

/// `.zshenv` constructs that may alter options, aliases, or functions invisibly to the snapshot.
pub fn zshenv_hazard(text: &str) -> Option<String> {
    for (n, line) in text.lines().enumerate() {
        let comment = line.char_indices().find(|&(at, c)| {
            c == '#'
                && line[..at]
                    .chars()
                    .next_back()
                    .is_none_or(|p| p.is_whitespace() || ";&|(".contains(p))
        });
        let code = comment.map_or(line, |(at, _)| &line[..at]);
        if code.contains("()") {
            return Some(format!("its line {} defines a function", n + 1));
        }
        for word in code.split(|c: char| c.is_whitespace() || ";&|(){}[]`\"'".contains(c)) {
            if let Some(word) = ZSHENV_WORDS.iter().find(|w| **w == word) {
                return Some(format!("its line {} runs `{word}`", n + 1));
            }
        }
    }
    None
}

/// The words of a `.zshenv` line that may set what the snapshot's listing
/// would not show, or run code this check does not read ([`zshenv_hazard`],
/// whose verdict quotes one of these and nothing else of the line).
const ZSHENV_WORDS: &[&str] = &[
    "emulate", "setopt", "unsetopt", "options", "set", "source", ".", "eval", "alias", "function",
    "autoload", "zmodload", "builtin", "exec", "trap",
];

/// `Err` naming a variable of the worker's that makes bash read a file before
/// a command or sets its options.
fn bash_env(worker: &WorkerEnv) -> Result<(), String> {
    for key in [
        "BASH_ENV",
        "ENV",
        "SHELLOPTS",
        "BASHOPTS",
        "POSIXLY_CORRECT",
    ] {
        if worker.var(key).is_some() {
            return Err(format!(
                "the worker's `{key}` changes what the Bash tool's bash runs or how it reads a \
                 command"
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "shell_startup_tests.rs"]
mod tests;
