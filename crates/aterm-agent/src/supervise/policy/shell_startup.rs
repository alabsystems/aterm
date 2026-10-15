// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What the worker's Bash tool's shell carries in from the person's shell
//! startup, read so that the rm rule's proof ([`super::rm_breaker`]) and the
//! read-only classifier ([`crate::supervise::classify`]), which model zsh 5.9
//! and bash 3.2 under default options with no user alias or function, prove
//! nothing where that model does not hold ([`read`],
//! [`ShellStartup::admits`]).
//!
//! Claude Code writes a shell snapshot at session start
//! (`<claude dir>/shell-snapshots/snapshot-<zsh|bash>-…sh`: the login shell's
//! non-default options, functions and aliases) and runs every Bash command as
//! `$SHELL -c 'source <snapshot> && setopt NO_EXTENDED_GLOB NO_BARE_GLOB_QUAL
//! && eval <command>'`, so what the snapshot carries is in force, as are
//! `.zshenv`, `BASH_ENV` and similar, `CLAUDE_ENV_FILE` and SessionStart hook
//! env scripts. Snapshots are not named per session, so every snapshot of the
//! Bash tool's shell ([`tool_kinds`]) is judged; with none, or one unreadable,
//! the startup is unknown.
//!
//! Fails the check ([`read`]): an option outside the inert lists
//! ([`ZSH_INERT`], [`BASH_SET_INERT`], [`BASH_SHOPTS`]), which escalates every
//! line; a function named for a hook `zsh -c` runs or a word of the Bash
//! tool's own chain ([`ALWAYS_RUN`]); a `.zshenv` that changes options or runs
//! unseen code ([`zshenv_hazard`]); bash's `BASH_ENV`, `ENV`, `SHELLOPTS`,
//! `BASHOPTS` or `POSIXLY_CORRECT`; `CLAUDE_CODE_SHELL_PREFIX` or
//! `CLAUDE_ENV_FILE`; a non-empty hook env script under
//! `<claude dir>/session-env/`; and any of those variables (or `SHELL`,
//! `CLAUDE_CODE_SHELL`, `ZDOTDIR`, `HOME`) given to the Bash tool by Claude
//! Code's settings ([`super::git_config::WorkerEnv::settings_env_where`]).
//!
//! Aliases and functions fail only the lines that may run them
//! ([`ShellStartup::admits`]): a function by any word under any quoting
//! ([`dequotings`]), an alias by a word in command position
//! ([`command_heads`]), or any word on a line that `eval`s its text. Claude
//! Code's own shadows (after its `# Check for rg availability` line) are not
//! read.
//!
//! Residuals: a `.zshenv` that sources nothing and names no option is taken as
//! setting none; another session's snapshot of the same shell stands in for a
//! missing one; a function named by an expansion is not seen (the line models
//! approve no command named by an expansion).

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
    fn name(self) -> &'static str {
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

/// What the Bash tool's shell starts with that the line models do not
/// assume and that fails only some lines: the aliases and functions the
/// person's startup defines, each with where it was read and whether it is
/// an alias — and whether that shell may be zsh, under which the read-only
/// rule takes only a plain line ([`Self::may_run_zsh`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShellStartup {
    names: Vec<(String, String, bool)>,
    zsh: bool,
}

/// Words that make the shell parse text of the line as a command again, so
/// that any word of it may be one an alias is expanded at.
const REPARSE: &[&str] = &["eval", "trap", "emulate", "sched", "fc"];

impl ShellStartup {
    /// Whether the Bash tool may run the line under zsh: a zsh snapshot was
    /// read, or zsh is among the shells it may pick ([`tool_kinds`]).
    #[must_use]
    pub fn may_run_zsh(&self) -> bool {
        self.zsh
    }

    /// `Err` naming the alias or function `line` may run: a function whose
    /// name is a word of the line under any quoting of it ([`dequotings`]), an
    /// alias whose name is a word in command position ([`command_heads`]) —
    /// or any word, under any quoting, when the line may parse its own text
    /// again (`eval`, [`REPARSE`]).
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

/// zsh options (as `setopt` lists them) that change nothing a
/// non-interactive `zsh -c` does to a command's words, which command runs, or
/// where a path points: history, completion, the line editor, prompts and
/// jobs; `autocd`, `correct`, `banghist` and `interactivecomments` (inert
/// without a terminal); options that only stop a command sooner; tracing;
/// `extendedglob`/`nobareglobqual`, which Claude Code resets; and the
/// directory stack's (every Bash tool command starts with an empty stack, so a
/// stack entry is a directory the line itself visited).
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

/// bash `set -o` options (as the snapshot replays them: only those on) that
/// change nothing about a command's words or which command runs: the defaults,
/// `monitor` and `onecmd` (the snapshot turns them on), the interactive-only
/// ones, `histexpand`, tracing, and those that only stop a command sooner.
/// `allexport`, `keyword`, `noglob`, `physical` and `posix` are not here.
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

/// bash 3.2's shopts, each with its default in a `bash -l -c`; `None` where no
/// value changes a command's words or which command runs (interactive-only,
/// `extglob`, which Claude Code resets, and `expand_aliases`, which its
/// snapshot turns on). A shopt not listed is unknown.
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
/// which shell runs a command or what it reads first.
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

/// The shells Claude Code's Bash tool may run for `worker` (2.1.284's
/// `BMn`): `CLAUDE_CODE_SHELL` when it names bash or zsh (and is executable,
/// which is not asked here: both are kept), else `$SHELL` when it names one,
/// else the first zsh found (bash first when `$SHELL` names bash).
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

/// What the worker's Bash tool's shell starts with (module header): `Ok`
/// with the aliases and functions that fail the lines naming them, `Err`
/// naming the option, variable or file that fails every line — or why the
/// startup could not be read. `launch` is the session's cwd (its project's
/// Claude Code settings).
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

/// Every snapshot of a shell in `kinds` under `claude`'s `shell-snapshots/`;
/// `Err` when there is none, or more than [`MAX_SNAPSHOTS`].
fn snapshots(claude: &Path, kinds: &[Kind]) -> Result<Vec<(Kind, PathBuf)>, String> {
    let dir = claude.join("shell-snapshots");
    let names: Vec<&str> = kinds.iter().map(|k| k.name()).collect();
    let unknown = || {
        format!(
            "no shell snapshot of {} in {}: the Bash tool's shell startup is unknown",
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

/// Read one snapshot's person's part (everything above [`CLAUDE_PART`]):
/// `Err` naming an option that fails every line, `state` given the names it
/// defines. `source` names the file in a reason.
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
    let option = |line: &str| {
        format!(
            "the Bash tool's {} starts with `{line}` from the person's shell startup (Claude \
             Code's shell snapshot {source})",
            kind.name()
        )
    };
    for line in &lines[1..end] {
        if line.starts_with(char::is_whitespace) {
            continue;
        }
        let words: Vec<&str> = line.split_whitespace().collect();
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
                    return Err(option(line.trim_end()));
                }
            }
            (Kind::Zsh, ["setopt", ..]) => return Err(option(line.trim_end())),
            (Kind::Bash, ["set", "-o", name]) => {
                if !BASH_SET_INERT.contains(name) {
                    return Err(option(line.trim_end()));
                }
            }
            (Kind::Bash, ["shopt", flag @ ("-s" | "-u"), name]) => {
                match BASH_SHOPTS.iter().find(|(n, _)| n == name) {
                    Some((_, None)) => {}
                    Some((_, Some(default))) if *default == (*flag == "-s") => {}
                    _ => return Err(option(line.trim_end())),
                }
            }
            (_, ["set" | "shopt" | "setopt" | "unsetopt" | "emulate", ..]) => {
                return Err(option(line.trim_end()));
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

/// Record `name` (`what` it is, and whether an `alias`), or `Err` for one
/// the shell runs whatever the line says ([`ALWAYS_RUN`], zsh's `TRAP…`
/// functions; an alias of such a name is refused the same way, as the
/// simplest reading of the Bash tool's own chain).
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

/// Whether `line` has a word spelled `name`: an occurrence not glued to a
/// character of a word on either side — a letter, a digit, or one of
/// `_-.+@%,:=/~`, so `/bin/ls` and `ls-x` hold no `ls`, while `\ls`,
/// `"ls"`, `(ls)` and `x;ls` do.
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

/// `line` and what it reads as after each level of quote removal the shell
/// may apply (once as a command, once more per `eval`), each also with every
/// `\`, `'` and `"` dropped: the spellings under which zsh and bash look a
/// function's name up (`r\m`, `r""m` and `$'\x72m'` all run a function `rm`).
/// `Err` for a line quoted more deeply than [`MAX_QUOTE_LEVELS`].
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
/// `\"`, `\\` and a backslash-newline undone. An unclosed quote runs to the
/// end.
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
/// `\UHHHHHHHH` and `\cX`. Any other escape is its character.
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

/// Every word of `line` that may stand in command position, where zsh and bash
/// expand a plain alias (an over-approximation): the first word, one after
/// `;`, `&`, `|`, `(`, `)`, `{`, `}`, a backtick or a newline wherever they
/// stand, after a `]]`, and after a word that keeps the next in command
/// position (a [`KEEPS_COMMAND`] word, an assignment, a redirection and its
/// target, a count or an option).
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
            // zsh's short `if [[ … ]] cmd` (SHORT_LOOPS, on by default).
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

/// `Err` naming a `.zshenv` zsh reads before every command the Bash tool runs — the
/// worker's `$ZDOTDIR`'s (else `$HOME`'s), `/etc/zshenv` and a Debian-built
/// zsh's `/etc/zsh/zshenv` — that holds
/// what the snapshot would not show ([`zshenv_hazard`]), or that cannot be
/// read.
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
                "{} runs before every command the Bash tool runs and {what}",
                path.display()
            ));
        }
    }
    Ok(())
}

/// What in a `.zshenv`'s text may set an option, alias or function the
/// snapshot's listing would not show (under `emulate` the listing is
/// relative to that emulation; a conditional one may differ between the
/// snapshot's login shell and the Bash tool's), or run code this check does
/// not read: `None` when nothing does. Comments aside (a `#` that starts a
/// word: `${#x}` is code), every line is read; the words are matched
/// anywhere in it, and an empty `()` (a function, or an empty array) counts.
pub fn zshenv_hazard(text: &str) -> Option<String> {
    const WORDS: &[&str] = &[
        "emulate", "setopt", "unsetopt", "options", "set", "source", ".", "eval", "alias",
        "function", "autoload", "zmodload", "builtin", "exec", "trap",
    ];
    for line in text.lines() {
        let comment = line.char_indices().find(|&(at, c)| {
            c == '#'
                && line[..at]
                    .chars()
                    .next_back()
                    .is_none_or(|p| p.is_whitespace() || ";&|(".contains(p))
        });
        let code = comment.map_or(line, |(at, _)| &line[..at]);
        if code.contains("()") {
            return Some(format!("defines a function (`{}`)", line.trim()));
        }
        for word in code.split(|c: char| c.is_whitespace() || ";&|(){}[]`\"'".contains(c)) {
            if WORDS.contains(&word) {
                return Some(format!("runs `{word}` (`{}`)", line.trim()));
            }
        }
    }
    None
}

/// `Err` naming a variable of the worker's that bash reads before a
/// command or that sets its options: `BASH_ENV` (a file it sources), `ENV`,
/// `SHELLOPTS`, `BASHOPTS`, `POSIXLY_CORRECT`.
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
