// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What the worker's Bash tool's shell carries in from the person's shell startup.

use std::path::{Path, PathBuf};

use super::git_config::{WorkerEnv, git_relevant};
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

/// Startup aliases and functions, plus whether the shell may be zsh ([`Self::may_run_zsh`]),
/// whether zsh's `EXTENDED_GLOB` may be on ([`Self::may_extended_glob`]) and whether a glob may
/// meet a name's leading dot ([`Self::may_dot_glob`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShellStartup {
    names: Vec<(String, String, bool)>,
    zsh: bool,
    extended_glob: bool,
    dot_glob: bool,
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

    /// Whether zsh's `EXTENDED_GLOB` may be on when the Bash tool runs the line under zsh: a zsh
    /// snapshot sets it (`setopt extendedglob`, in any spelling zsh reads) or holds a line at its
    /// top level that this check does not read ([`parse_snapshot`]), a `.zshenv` zsh reads is not
    /// proven to set no option ([`zshenv_sets_no_option`]), or zsh may run with no snapshot of its
    /// own read. zsh's default is off. The Bash tool's own `setopt NO_EXTENDED_GLOB
    /// NO_BARE_GLOB_QUAL`, which Claude Code 2.1.284 runs after sourcing the snapshot (commit
    /// 0f8552bf66; the process line `harness::upgrade_drive_tests` keeps), only turns options off,
    /// so a version without it changes nothing here, and nothing here counts on it.
    #[must_use]
    pub const fn may_extended_glob(&self) -> bool {
        self.extended_glob
    }

    /// Whether a `*`, a `?` or a bracket expression may meet a name's leading `.` in the Bash
    /// tool's shell: a snapshot holds a line at its top level that this check does not read
    /// ([`parse_snapshot`]), a `.zshenv` zsh reads is not proven to set no option
    /// ([`zshenv_sets_no_option`]), a shell the Bash tool may run has no snapshot of its own read,
    /// or bash may run with `GLOBIGNORE` in its environment. zsh's `GLOB_DOTS` and bash's
    /// `dotglob` are off by default, and a snapshot that turns either on is no startup this check
    /// reads at all ([`ZSH_INERT`], [`BASH_SHOPTS`]); bash 3.2.57 ignores a `GLOBIGNORE` it gets
    /// from its environment (MEASURED 2026-11-05 in a folder holding `a.json` and
    /// `.credentials.json`: `GLOBIGNORE=zz bash -c 'echo *.json'` printed `a.json` alone, and
    /// `bash -c 'GLOBIGNORE=zz; echo *.json'` both), which a newer bash that `CLAUDE_CODE_SHELL`
    /// names is not measured to do.
    #[must_use]
    pub const fn may_dot_glob(&self) -> bool {
        self.dot_glob
    }

    /// Each alias and function read, and each [`READER_ENV`] or git variable the startup may set,
    /// as a `$NAME` entry.
    #[must_use]
    pub fn names(&self) -> &[(String, String, bool)] {
        &self.names
    }

    /// The startup [`Self::names`], [`Self::may_run_zsh`], [`Self::may_extended_glob`] and
    /// [`Self::may_dot_glob`] describe.
    #[must_use]
    pub const fn of_parts(
        names: Vec<(String, String, bool)>,
        zsh: bool,
        extended_glob: bool,
        dot_glob: bool,
    ) -> Self {
        Self {
            names,
            zsh,
            extended_glob,
            dot_glob,
        }
    }

    /// `Err` naming the alias or function `line` may run, or the reader it may run whose
    /// variable ([`READER_ENV`]) the startup may set (a `$NAME` entry of [`Self::names`]).
    pub fn admits(&self, line: &str) -> Result<(), String> {
        if self.names.is_empty() {
            return Ok(());
        }
        let texts = dequotings(line)?;
        let anywhere = |name: &str| texts.iter().any(|t| names_word(t, name));
        let reparse = REPARSE.iter().any(|w| anywhere(w));
        let heads = command_heads(line);
        for (name, what, alias) in &self.names {
            if let Some(var) = name.strip_prefix('$') {
                if let Some(reader) = reader_named(&texts, var) {
                    return Err(format!(
                        "`{reader}` reads `{name}`, which {what}: a value this check does not \
                         read may make it read a file or run a command the line does not name"
                    ));
                }
                continue;
            }
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

/// Each variable of the Bash tool's environment that makes a reader the classifier approves read
/// a file its words do not name or run a command, with the programs that read it. MEASURED
/// 2026-11-05 on macOS 27.0.1 (`docs/measured/classifier-audit-3-fixes-2026-10-29.md`): BSD grep
/// 2.6.0-FreeBSD (`/usr/bin/grep`, `egrep`, `fgrep`, which Claude Code's `grep` shadow falls back
/// to) puts `$GREP_OPTIONS`'s words before its own, so `GREP_OPTIONS='-e x f' grep y g` reads
/// `f`; Claude Code's rg 14.1.1 reads the config `$RIPGREP_CONFIG_PATH` names, whose lines are
/// its flags and operands (an operand read another file, `--pre` ran a command); python 3.9.6
/// runs a `sitecustomize` from `$PYTHONPATH`. `PYTHONHOME` and `PYTHONUSERBASE` move where it
/// imports its startup modules from, REASONED from the same. macOS's `/usr/bin/python3` is the
/// xcrun shim `/usr/bin/git` and `/usr/bin/strings` are (one binary), and
/// `DEVELOPER_DIR=<dir> /usr/bin/python3 -c 'print(1)'` ran `<dir>/usr/bin/xcrun` (MEASURED the
/// same day); `SDKROOT` names the SDK that shim reads, REASONED from git's. Each is held by its
/// name, whatever its value: this reading knows no place the worker writes, so it cannot tell a
/// planted developer directory from Xcode's ([`readers_of`] adds git to both).
pub const READER_ENV: &[(&str, &[&str])] = &[
    ("GREP_OPTIONS", &["grep", "egrep", "fgrep"]),
    ("RIPGREP_CONFIG_PATH", &["rg"]),
    ("PYTHONPATH", PYTHON),
    ("PYTHONHOME", PYTHON),
    ("PYTHONUSERBASE", PYTHON),
    ("DEVELOPER_DIR", PYTHON),
    ("SDKROOT", PYTHON),
];

/// The python heads the classifier approves (an allowlisted script, `classify::bash`).
const PYTHON: &[&str] = &["python", "python2", "python3"];

/// The first program `var` ([`READER_ENV`]) redirects that a word of `texts` names, by itself or
/// as the last part of a path (`/usr/bin/grep`, which no shell function shadows), where any
/// word naming it may run it (behind `xargs`, `find -exec` or a substitution).
fn reader_named(texts: &[String], var: &str) -> Option<&'static str> {
    let readers: Vec<&'static str> = readers_of(var).collect();
    let word_char = |c: char| c.is_alphanumeric() || "_-.+@%,:=~/".contains(c);
    texts.iter().find_map(|t| {
        t.split(|c: char| !word_char(c)).find_map(|w| {
            let last = w.rsplit('/').next().unwrap_or(w);
            let last = last
                .len()
                .checked_sub(4)
                .filter(|&at| last.is_char_boundary(at) && last[at..].eq_ignore_ascii_case(".exe"))
                .map_or(last, |at| &last[..at]);
            readers.iter().copied().find(|r| *r == last)
        })
    })
}

/// The programs variable `var` redirects: its [`READER_ENV`] readers, and `git` for one that
/// moves what git loads or which git runs ([`git_startup_var`]).
fn readers_of(var: &str) -> impl Iterator<Item = &'static str> {
    READER_ENV
        .iter()
        .filter(move |(v, _)| *v == var)
        .flat_map(|(_, readers)| readers.iter().copied())
        .chain(git_startup_var(var).then_some("git"))
}

/// A variable a `.zshenv` may set that changes what git loads or which git runs
/// (`git_config::git_relevant`'s, past `PATH` and `HOME`, which nearly every `.zshenv` names):
/// git's probe runs with the worker's own environment, which holds no `.zshenv` export.
fn git_startup_var(var: &str) -> bool {
    git_relevant(var) && !matches!(var, "PATH" | "HOME")
}

/// The [`READER_ENV`] and [`git_startup_var`] variables `text` (a `.zshenv`) names in its code,
/// in any quoting ([`zshenv_forms`]), so may set, each once.
fn reader_vars_named(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in text.lines() {
        for form in zshenv_forms(line) {
            for word in form.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
                let redirects = READER_ENV.iter().any(|(v, _)| *v == word) || git_startup_var(word);
                if redirects && !out.iter().any(|v| v == word) {
                    out.push(word.to_string());
                }
            }
        }
    }
    out
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

/// zsh options (`setopt`) that do not affect non-interactive `zsh -c` command resolution or paths
/// (`noextendedglob` among them: `extendedglob` is no inert option but one a snapshot's
/// [`ShellStartup::may_extended_glob`] records, under which a proven line holds none of its
/// operators, `classify::plain_zsh_line`).
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
            | "GLOBIGNORE"
    )
}

/// The shells Claude Code's Bash tool may run for `worker` (2.1.284's `BMn`), on
/// Windows when `windows`: there 2.1.310's start sets `SHELL` to Git for Windows' bash
/// (`CLAUDE_CODE_GIT_BASH_PATH` names only a bash or an sh) before the Bash tool picks
/// its shell, so only `CLAUDE_CODE_SHELL` can name zsh.
pub fn tool_kinds(worker: &WorkerEnv, windows: bool) -> Vec<Kind> {
    let mut out = Vec::new();
    if let Some(k) = worker.var("CLAUDE_CODE_SHELL").and_then(Kind::of) {
        out.push(k);
    }
    let fallback = if windows {
        Kind::Bash
    } else {
        Kind::of(worker.var("SHELL").unwrap_or("")).unwrap_or(Kind::Zsh)
    };
    if !out.contains(&fallback) {
        out.push(fallback);
    }
    out
}

/// What the worker's Bash tool's shell starts with (module header), for a worker on Windows
/// when `windows` ([`tool_kinds`]).
pub fn read(worker: &WorkerEnv, launch: &Path, windows: bool) -> Result<ShellStartup, String> {
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
    let kinds = tool_kinds(worker, windows);
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
    let mut set: Vec<(String, String)> = READER_ENV
        .iter()
        .filter(|(var, _)| worker.var(var).is_some())
        .map(|(var, _)| {
            (
                (*var).to_string(),
                "the worker's environment sets".to_string(),
            )
        })
        .collect();
    for (source, key, _) in worker.settings_vars(launch, "a command")? {
        if READER_ENV.iter().any(|(v, _)| *v == key) {
            set.push((
                key,
                format!("Claude Code's settings ({source}) give the Bash tool"),
            ));
        }
    }
    for kind in kinds {
        match kind {
            Kind::Zsh => {
                let (unproven, named) = zshenv(worker)?;
                state.extended_glob |= unproven;
                state.dot_glob |= unproven;
                for (var, path) in named {
                    set.push((var, format!("{} may set", path.display())));
                }
            }
            Kind::Bash => {
                bash_env(worker)?;
                state.dot_glob |= worker.var("GLOBIGNORE").is_some();
            }
        }
        // A shell that may run with no snapshot of its own read: Claude Code writes one from the
        // person's startup before the command it runs, unseen by this check.
        if !snapshots.iter().any(|(k, _)| *k == kind) {
            state.extended_glob |= kind == Kind::Zsh;
            state.dot_glob = true;
        }
    }
    for (var, what) in set {
        define(&mut state, format!("${var}"), what, false)?;
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
    let sessions = match std::fs::read_dir(&root) {
        Ok(sessions) => sessions,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("{} cannot be listed ({e})", root.display())),
    };
    for (n, session) in sessions.filter_map(Result::ok).enumerate() {
        if n >= MAX_SESSION_ENV_DIRS {
            return Err(format!(
                "more than {MAX_SESSION_ENV_DIRS} sessions in {}",
                root.display()
            ));
        }
        let files = match std::fs::read_dir(session.path()) {
            Ok(files) => files,
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                continue;
            }
            Err(e) => {
                return Err(format!(
                    "{} cannot be listed ({e})",
                    session.path().display()
                ));
            }
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

/// Read one snapshot's person's part (everything above [`CLAUDE_PART`]). A zsh snapshot's
/// `setopt extendedglob` (any spelling zsh reads: case and `_` ignored) marks
/// [`ShellStartup::may_extended_glob`]. A line at either snapshot's top level other than the
/// forms read here (a zsh function's opening line, a `setopt`, a bash `set -o`, `shopt` or
/// `eval $'…'`, an `alias --`), a blank, a comment, a closing brace and Claude Code's own
/// `unalias -a 2>/dev/null || true` may set an option unseen: it marks
/// [`ShellStartup::may_dot_glob`], and in a zsh snapshot [`ShellStartup::may_extended_glob`].
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
                let option = name.to_ascii_lowercase().replace('_', "");
                if option == "extendedglob" {
                    state.extended_glob = true;
                } else if !ZSH_INERT.contains(&option.as_str()) {
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
            (_, ["alias", "--", ..]) => {
                let def = line.strip_prefix("alias -- ").unwrap_or_default();
                let name = alias_name(def)
                    .ok_or_else(|| format!("{source} has an alias this check cannot name"))?;
                define(state, name, format!("an alias in {source}"), true)?;
            }
            (
                _,
                [
                    "set" | "shopt" | "setopt" | "unsetopt" | "emulate" | "alias" | "function"
                    | "autoload" | "zmodload" | "hash",
                    ..,
                ]
                | [_, "()", ..],
            ) => {
                return Err(unnamed());
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
            _ if !line.starts_with('#') && !SNAPSHOT_LINES.contains(&line.trim_end()) => {
                state.extended_glob |= kind == Kind::Zsh;
                state.dot_glob = true;
            }
            _ => {}
        }
    }
    Ok(())
}

/// The lines at a snapshot's top level, besides comments and the forms [`parse_snapshot`] reads,
/// that set no option: a blank, a function's closing brace, and Claude Code's own line before the
/// functions.
const SNAPSHOT_LINES: &[&str] = &["", "}", "unalias -a 2>/dev/null || true"];

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
        let (name, after) = rest.split_once('\'')?;
        if !after.starts_with('=') {
            return None;
        }
        name
    } else {
        let name = def.split_once('=')?.0;
        if name.contains(char::is_whitespace) {
            return None;
        }
        name
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
/// ([`zshenv_hazard`]), or that cannot be read; else whether one of them is not
/// proven to set no option ([`zshenv_sets_no_option`]), so that `EXTENDED_GLOB`
/// and `GLOB_DOTS` may be on ([`ShellStartup::may_extended_glob`],
/// [`ShellStartup::may_dot_glob`]), and each [`READER_ENV`] or [`git_startup_var`]
/// variable one names ([`reader_vars_named`]), with that file.
fn zshenv(worker: &WorkerEnv) -> Result<(bool, Vec<(String, PathBuf)>), String> {
    let dot = worker
        .var("ZDOTDIR")
        .filter(|d| !d.is_empty())
        .or_else(|| worker.var("HOME").filter(|h| !h.is_empty()))
        .map(|d| Path::new(d).join(".zshenv"));
    let mut unproven = false;
    let mut named = Vec::new();
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
        unproven |= !zshenv_sets_no_option(&text);
        named.extend(
            reader_vars_named(&text)
                .into_iter()
                .map(|v| (v, path.clone())),
        );
    }
    Ok((unproven, named))
}

/// A `.zshenv` line's code past a comment ([`zshenv_code`]), as written, with one level of
/// quoting removed, and with every backslash and quote dropped: the spellings
/// [`zshenv_hazard`] and [`reader_vars_named`] read a word in.
fn zshenv_forms(line: &str) -> [String; 3] {
    let code = zshenv_code(line);
    [
        code.to_string(),
        unquote_once(code),
        code.replace(['\\', '\'', '"'], ""),
    ]
}

/// `line`, a `.zshenv` line, without the comment that ends it, if any.
fn zshenv_code(line: &str) -> &str {
    let comment = line.char_indices().find(|&(at, c)| {
        c == '#'
            && !line[..at].contains(['\'', '"', '\\', '`'])
            && line[..at]
                .chars()
                .next_back()
                .is_none_or(|p| p.is_whitespace() || ";&|(".contains(p))
    });
    comment.map_or(line, |(at, _)| &line[..at])
}

/// `.zshenv` constructs that may alter options, aliases, or functions invisibly to the snapshot:
/// a function, one of [`ZSHENV_WORDS`], a word naming a parameter of [`ZSHENV_NAMES`], or a
/// parameter flag. MEASURED 2026-11-05 with zsh 5.9, each in a `.zshenv` a `zsh -c` read:
/// `options+=(extendedglob on)` and `options=(extendedglob on)` set that option, as
/// `${(P)n::=on}` after `n=options\[extendedglob\]` did, and `histchars='!^x'` (or `HISTCHARS`)
/// made a word opening with `#` no comment (`docs/measured/classifier-zsh-glob-2026-11-05.md`).
pub fn zshenv_hazard(text: &str) -> Option<String> {
    for (n, line) in text.lines().enumerate() {
        let forms = zshenv_forms(line);
        if forms.iter().any(|f| f.contains("()")) {
            return Some(format!("its line {} defines a function", n + 1));
        }
        if forms.iter().any(|f| f.contains("${(")) {
            return Some(format!("its line {} holds a parameter flag `${{(`", n + 1));
        }
        for form in &forms {
            for word in form.split(|c: char| c.is_whitespace() || ";&|(){}[]`\"'".contains(c)) {
                if let Some(word) = ZSHENV_WORDS.iter().find(|w| **w == word) {
                    return Some(format!("its line {} runs `{word}`", n + 1));
                }
            }
            for word in form.split(|c: char| c.is_whitespace() || ";&|(){}`\"'".contains(c)) {
                let named = ZSHENV_NAMES.iter().find(|name| {
                    word.strip_prefix(**name)
                        .is_some_and(|rest| rest.is_empty() || rest.starts_with(['=', '+', '[']))
                });
                if let Some(name) = named {
                    return Some(format!("its line {} names `{name}`", n + 1));
                }
            }
        }
    }
    None
}

/// The words of a `.zshenv` line that may set what the snapshot's listing
/// would not show, or run code this check does not read ([`zshenv_hazard`],
/// whose verdict quotes one of these, of [`ZSHENV_NAMES`] or `${(`, and nothing
/// else of the line).
const ZSHENV_WORDS: &[&str] = &[
    "emulate", "setopt", "unsetopt", "options", "set", "source", ".", "eval", "alias", "function",
    "autoload", "zmodload", "builtin", "exec", "trap", "hash",
];

/// The parameters a `.zshenv` word that names one (alone, or led by it and then `=`, `+=` or a
/// subscript: `options+=(…)`, `options[x]=on`, `histchars=…`, `print -v options[x] on`) makes a
/// [`zshenv_hazard`]: `options`, whose elements are zsh's options, and `histchars`
/// (`HISTCHARS`), whose third character opens a comment where `#` would.
const ZSHENV_NAMES: &[&str] = &["options", "histchars", "HISTCHARS"];

/// Whether every line of `text`, a `.zshenv` with no [`zshenv_hazard`], is proven to set no
/// option: blank or a comment, or an optional `export` and then words each `NAME=VALUE` (or,
/// after `export`, a bare `NAME`) whose `NAME` is none that changes which `.zshenv` zsh reads
/// next (`ZDOTDIR`, `HOME`), on a line with no quote holding a blank, no `\`, `` ` ``, `;`, `&`,
/// `|`, `<`, `>`, `(`, `)`, `[` or `]`, and no `$` but one before a `NAME` or `{NAME}`. A
/// `.zshenv` any other line holds may set `EXTENDED_GLOB` or `GLOB_DOTS`
/// ([`ShellStartup::may_extended_glob`], [`ShellStartup::may_dot_glob`]): [`zshenv_hazard`] reads
/// words, and a name built at run time is none.
fn zshenv_sets_no_option(text: &str) -> bool {
    let name = |w: &str| {
        w.chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && w.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    };
    let plain_dollars = |code: &str| {
        code.match_indices('$').all(|(at, _)| {
            let rest = &code[at + 1..];
            let braced = rest
                .strip_prefix('{')
                .and_then(|r| r.split_once('}'))
                .is_some_and(|(n, _)| name(n));
            let bare = rest
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
            braced || bare
        })
    };
    text.lines().all(|line| {
        let code = zshenv_code(line).trim();
        if code.is_empty() {
            return true;
        }
        if code.contains(['\\', '`', ';', '&', '|', '<', '>', '(', ')', '[', ']'])
            || !plain_dollars(code)
        {
            return false;
        }
        let mut words = code.split_whitespace().peekable();
        let export = words.next_if_eq(&"export").is_some();
        words.peek().is_some()
            && words.all(|w| {
                let target = w.split_once('=').map_or(w, |(n, _)| n);
                let quoted = w.matches(['\'', '"']).count() % 2 == 1;
                (export || w.contains('='))
                    && name(target)
                    && !quoted
                    && !["ZDOTDIR", "HOME"].contains(&target)
            })
    })
}

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
