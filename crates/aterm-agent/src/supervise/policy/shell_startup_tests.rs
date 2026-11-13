// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shell-startup policy tests over fixture Claude Code directories and snapshots.

use super::*;

struct Scratch(aterm_tempfile::TempDir);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = aterm_tempfile::Builder::new()
            .prefix(&format!("aterm-shell-startup-{tag}-"))
            .tempdir()
            .expect("scratch dir");
        std::fs::create_dir_all(dir.path().join("home")).expect("scratch home");
        std::fs::create_dir_all(dir.path().join("project")).expect("scratch project");
        std::fs::create_dir_all(dir.path().join("claude/shell-snapshots"))
            .expect("scratch snapshots");
        Self(dir)
    }

    fn claude(&self) -> PathBuf {
        self.0.path().join("claude")
    }

    fn home(&self) -> PathBuf {
        self.0.path().join("home")
    }

    fn project(&self) -> PathBuf {
        self.0.path().join("project")
    }

    fn snapshot(&self, kind: &str, ms: u64, text: &str) {
        let path = self
            .claude()
            .join(format!("shell-snapshots/snapshot-{kind}-{ms}-a1b2c3.sh"));
        std::fs::write(path, text).expect("write snapshot");
    }

    fn worker(&self, shell: &str, extra: &[&str]) -> WorkerEnv {
        let mut vars = vec![
            format!("HOME={}", self.home().display()),
            format!("SHELL={shell}"),
            format!("CLAUDE_CONFIG_DIR={}", self.claude().display()),
        ];
        vars.extend(extra.iter().map(|v| v.to_string()));
        WorkerEnv::from_env(vars)
    }

    /// [`read`] for these Unix workers on every host.
    fn read(&self, worker: &WorkerEnv) -> Result<ShellStartup, String> {
        read(worker, &self.project(), false)
    }
}

const CLAUDE_TAIL: &str = "# Check for rg availability
if ! (unalias rg 2>/dev/null; command -v rg) >/dev/null 2>&1; then
  alias rg='/opt/claude/rg'
fi
# Shadow find/grep with embedded bfs/ugrep
unalias find 2>/dev/null || true
unalias grep 2>/dev/null || true
find () {
  ARGV0=bfs \"$_cc_bin\" -S dfs \"$@\"
}
# Shadow pkill to refuse patterns matching the CLI process
unalias pkill 2>/dev/null || true
function pkill {
  command pkill ${1+\"$@\"}
}
export PATH='/usr/bin:/bin'
";

fn zsh_snapshot(functions: &str, options: &str, aliases: &str) -> String {
    format!(
        "# Snapshot file
# Unset all aliases to avoid conflicts with functions
unalias -a 2>/dev/null || true
# Functions
{functions}# Shell Options
setopt nohashdirs
setopt login
{options}# Aliases
alias -- run-help=man
alias -- which-command=whence
{aliases}{CLAUDE_TAIL}"
    )
}

fn bash_snapshot(shopts: &[(&str, bool)], functions: &str, sets: &str, aliases: &str) -> String {
    let mut out = String::from(
        "# Snapshot file\n# Unset all aliases to avoid conflicts with functions\nunalias -a \
         2>/dev/null || true\n# Shopt\n",
    );
    for (name, default) in BASH_SHOPTS {
        let on = shopts
            .iter()
            .find(|(n, _)| n == name)
            .map_or(default.unwrap_or(*name == "login_shell"), |(_, on)| *on);
        out.push_str(&format!("shopt {} {name}\n", if on { "-s" } else { "-u" }));
    }
    out.push_str("# Functions\n");
    out.push_str(functions);
    out.push_str(
        "# Shell Options\nset -o braceexpand\nset -o hashall\nset -o interactive-comments\nset \
         -o monitor\nset -o onecmd\n",
    );
    out.push_str(sets);
    out.push_str("shopt -s expand_aliases\n# Aliases\n");
    out.push_str(aliases);
    out.push_str(CLAUDE_TAIL);
    out
}

#[test]
fn a_default_zsh_snapshot_fails_no_line_and_its_aliases_fail_the_lines_naming_them() {
    let s = Scratch::new("zsh-default");
    s.snapshot(
        "zsh",
        1,
        &zsh_snapshot(
            "ll2 () {\n\tls -l \"$@\"\n}\n",
            "setopt autocd\nsetopt histignoredups\nsetopt interactivecomments\n",
            "alias -- ll='ls -l'\nalias -- 'we ird'=echo\n",
        ),
    );
    let state = s.read(&s.worker("/bin/zsh", &[])).expect("a clean startup");
    assert_eq!(state.admits("rm -rf /private/tmp/x/y"), Ok(()));
    assert_eq!(state.admits("git status --short"), Ok(()));
    let why = state.admits("cd /tmp && ll").expect_err("an alias");
    assert!(why.contains("`ll` is an alias"), "{why}");
    assert!(state.admits("x; ll2 .").is_err(), "a function");
    assert_eq!(state.admits("find . -name x | grep y; rg z"), Ok(()));
    assert_eq!(state.admits("/bin/ll x; ll-x; x.ll"), Ok(()));
    assert!(
        state.admits("\\ll").is_err(),
        "a backslash skips an alias, not a function"
    );
}

#[test]
fn an_option_the_models_do_not_assume_fails_every_line_naming_it() {
    for opt in [
        "globsubst",
        "shwordsplit",
        "ksharrays",
        "rcexpandparam",
        "nullglob",
        "cshnullglob",
        "nonomatch",
        "magicequalsubst",
        "kshglob",
        "globdots",
        "nocaseglob",
        "rcquotes",
        "ignorebraces",
        "noequals",
        "noshortloops",
        "noaliases",
        "posixbuiltins",
        "cdablevars",
        "pathdirs",
        "aliasfuncdef",
        "no_such_option_yet",
    ] {
        let s = Scratch::new("zsh-opt");
        s.snapshot("zsh", 1, &zsh_snapshot("", &format!("setopt {opt}\n"), ""));
        let why = s.read(&s.worker("/bin/zsh", &[])).expect_err(opt);
        assert!(why.contains(&format!("`setopt {opt}`")), "{opt}: {why}");
    }
    let s = Scratch::new("zsh-kshprint");
    s.snapshot(
        "zsh",
        1,
        &zsh_snapshot("", "setopt noaliases             off\n", ""),
    );
    assert!(s.read(&s.worker("/bin/zsh", &[])).is_err());
    let s = Scratch::new("zsh-reset");
    s.snapshot(
        "zsh",
        1,
        &zsh_snapshot("", "setopt extendedglob\nsetopt nobareglobqual\n", ""),
    );
    assert!(s.read(&s.worker("/bin/zsh", &[])).is_ok());
    let s = Scratch::new("zsh-body");
    s.snapshot(
        "zsh",
        1,
        &zsh_snapshot("f () {\n\tsetopt localoptions globsubst\n}\n", "", ""),
    );
    assert!(s.read(&s.worker("/bin/zsh", &[])).is_ok());
}

#[test]
fn every_snapshot_of_the_tools_shell_is_judged_and_none_is_unknown() {
    let s = Scratch::new("which");
    let err = s.read(&s.worker("/bin/zsh", &[])).expect_err("no snapshot");
    assert!(err.contains("no shell snapshot of zsh"), "{err}");
    assert!(unwritten(&err), "{err}");
    s.snapshot("bash", 1, &bash_snapshot(&[], "", "", ""));
    let err = s
        .read(&s.worker("/bin/zsh", &[]))
        .expect_err("no zsh snapshot");
    assert!(unwritten(&err), "a bash snapshot is no zsh one: {err}");
    s.snapshot("zsh", 2, &zsh_snapshot("", "", ""));
    assert!(s.read(&s.worker("/bin/zsh", &[])).is_ok());
    s.snapshot("zsh", 3, &zsh_snapshot("", "setopt globsubst\n", ""));
    let err = s
        .read(&s.worker("/bin/zsh", &[]))
        .expect_err("an option it cannot model");
    assert!(!unwritten(&err), "a snapshot stands: {err}");
    assert_eq!(
        tool_kinds(
            &s.worker("/bin/zsh", &["CLAUDE_CODE_SHELL=/bin/bash"]),
            false
        ),
        [Kind::Bash, Kind::Zsh]
    );
    assert_eq!(tool_kinds(&s.worker("/bin/sh", &[]), false), [Kind::Zsh]);
    let zsh_shell = r"CLAUDE_CODE_SHELL=C:\Program Files\Git\usr\bin\zsh.exe";
    assert_eq!(
        tool_kinds(&s.worker("/bin/zsh", &[]), true),
        [Kind::Bash],
        "on Windows Claude Code's start names Git for Windows' bash, whatever SHELL says"
    );
    assert_eq!(
        tool_kinds(&s.worker("/bin/zsh", &[zsh_shell]), true),
        [Kind::Zsh, Kind::Bash]
    );
    let w = Scratch::new("windows");
    let err = read(&w.worker("/bin/zsh", &[]), &w.project(), true).expect_err("no snapshot");
    assert!(err.contains("no shell snapshot of bash"), "{err}");
    w.snapshot("bash", 1, &bash_snapshot(&[], "", "", ""));
    let windows = read(&w.worker("/bin/zsh", &[]), &w.project(), true).expect("bash's");
    assert!(!windows.may_run_zsh(), "{windows:?}");
    let named = read(&w.worker("/bin/zsh", &[zsh_shell]), &w.project(), true);
    assert!(
        named.as_ref().is_ok_and(ShellStartup::may_run_zsh),
        "a zsh CLAUDE_CODE_SHELL names may run there: {named:?}"
    );
    let t = Scratch::new("truncated");
    t.snapshot("zsh", 1, "# Snapshot file\n# Functions\n");
    assert!(t.read(&t.worker("/bin/zsh", &[])).is_err());
}

#[test]
fn a_bash_snapshot_is_judged_against_bash_3_2s_defaults() {
    let s = Scratch::new("bash-default");
    s.snapshot(
        "bash",
        1,
        &bash_snapshot(
            &[("extglob", true), ("histappend", true)],
            "eval $'rm () \\n{ \\n    command rm -i \"$@\"\\n}' > /dev/null 2>&1\n",
            "set -o noclobber\n",
            "alias -- ll='ls -l'\n",
        ),
    );
    let state = s.read(&s.worker("/bin/bash", &[])).expect("defaults");
    assert!(state.admits("rm -rf /tmp/x/y").is_err(), "a function rm");
    assert!(state.admits("ll").is_err());
    assert_eq!(state.admits("cat /etc/hosts"), Ok(()));
    for (shopts, sets, what) in [
        (&[("nullglob", true)][..], "", "shopt -s nullglob"),
        (&[("dotglob", true)][..], "", "shopt -s dotglob"),
        (
            &[("interactive_comments", false)][..],
            "",
            "shopt -u interactive_comments",
        ),
        (&[][..], "set -o keyword\n", "set -o keyword"),
        (&[][..], "set -o posix\n", "set -o posix"),
        (&[][..], "set -o noglob\n", "set -o noglob"),
        (&[][..], "shopt -s globstar\n", "shopt -s globstar"),
    ] {
        let s = Scratch::new("bash-opt");
        s.snapshot("bash", 1, &bash_snapshot(shopts, "", sets, ""));
        let why = s.read(&s.worker("/bin/bash", &[])).expect_err(what);
        assert!(why.contains(what), "{what}: {why}");
    }
    for var in ["BASH_ENV=/x/env", "SHELLOPTS=noglob", "POSIXLY_CORRECT=1"] {
        let s = Scratch::new("bash-env");
        s.snapshot("bash", 1, &bash_snapshot(&[], "", "", ""));
        let why = s.read(&s.worker("/bin/bash", &[var])).expect_err(var);
        assert!(
            why.contains(var.split('=').next().unwrap_or_default()),
            "{why}"
        );
    }
}

#[test]
fn a_hook_or_function_that_runs_unnamed_fails_every_line() {
    for f in [
        "chpwd",
        "zshexit",
        "TRAPEXIT",
        "TRAPDEBUG",
        "command_not_found_handler",
        "eval",
        "setopt",
    ] {
        let s = Scratch::new("hookfn");
        s.snapshot(
            "zsh",
            1,
            &zsh_snapshot(&format!("{f} () {{\n\tprint hi\n}}\n"), "", ""),
        );
        let why = s.read(&s.worker("/bin/zsh", &[])).expect_err(f);
        assert!(why.contains(&format!("`{f}` is a function")), "{why}");
    }
}

#[test]
fn what_runs_before_every_command_besides_the_snapshot_is_read() {
    let s = Scratch::new("zshenv");
    s.snapshot("zsh", 1, &zsh_snapshot("", "", ""));
    let zshenv = s.home().join(".zshenv");
    std::fs::write(
        &zshenv,
        "# comment: emulate sh\nexport PATH=\"$HOME/bin:$PATH\"\ntypeset -U path\nN=${#PATH}\n",
    )
    .expect("zshenv");
    assert!(s.read(&s.worker("/bin/zsh", &[])).is_ok());
    for text in [
        "emulate sh",
        "[[ -o login ]] || setopt globsubst",
        "options[globsubst]=on",
        ". \"$HOME/.cargo/env\"",
        "source ~/.zshenv.local",
        "eval \"$(tool init zsh)\"",
        "X=\" # \"; eval \"$(tool init zsh)\"",
        "ev\\al \"$(tool init zsh)\"",
        "e'v'al \"$(tool init zsh)\"",
        "hash git=/tmp/evil",
        "x=${#y}; set -o shwordsplit",
        "rm() { echo }",
    ] {
        std::fs::write(&zshenv, text).expect("zshenv");
        let why = s.read(&s.worker("/bin/zsh", &[])).expect_err(text);
        assert!(why.contains(".zshenv"), "{text}: {why}");
    }
    for bad_alias in [
        "alias -g G='| sh'\n",
        "alias -- -g G='| sh'\n",
        "alias -- '-g' G='| sh'\n",
        "hash git=/tmp/evil\n",
    ] {
        let s = Scratch::new("bad-alias");
        s.snapshot("zsh", 1, &zsh_snapshot("", "", bad_alias));
        assert!(s.read(&s.worker("/bin/zsh", &[])).is_err(), "{bad_alias}");
    }
    let zdot = s.0.path().join("zdot");
    std::fs::create_dir_all(&zdot).expect("zdotdir");
    std::fs::write(zdot.join(".zshenv"), "setopt globsubst\n").expect("zshenv");
    std::fs::write(&zshenv, "").expect("zshenv");
    let with = format!("ZDOTDIR={}", zdot.display());
    assert!(s.read(&s.worker("/bin/zsh", &[&with])).is_err());

    let s = Scratch::new("envfile");
    s.snapshot("zsh", 1, &zsh_snapshot("", "", ""));
    for var in [
        "CLAUDE_ENV_FILE=/x/env.sh",
        "CLAUDE_CODE_SHELL_PREFIX=/x/wrap",
    ] {
        assert!(s.read(&s.worker("/bin/zsh", &[var])).is_err(), "{var}");
    }
    let hooks = s.claude().join("session-env/0b8a");
    std::fs::create_dir_all(&hooks).expect("session-env");
    std::fs::write(hooks.join("sessionstart-hook-0.sh"), "").expect("empty hook script");
    assert!(
        s.read(&s.worker("/bin/zsh", &[])).is_ok(),
        "an empty one runs nothing"
    );
    std::fs::write(hooks.join("sessionstart-hook-0.sh"), "setopt globsubst\n").expect("hook");
    let why = s
        .read(&s.worker("/bin/zsh", &[]))
        .expect_err("a hook script");
    assert!(why.contains("sessionstart-hook-0.sh"), "{why}");

    let s = Scratch::new("settings");
    s.snapshot("zsh", 1, &zsh_snapshot("", "", ""));
    std::fs::create_dir_all(s.project().join(".claude")).expect(".claude");
    std::fs::write(
        s.project().join(".claude/settings.json"),
        "{\"env\": {\"ZDOTDIR\": \"/x/zdot\"}}",
    )
    .expect("settings");
    let why = s
        .read(&s.worker("/bin/zsh", &[]))
        .expect_err("settings env");
    assert!(why.contains("`ZDOTDIR`"), "{why}");
}

/// A glob may meet a name's leading dot wherever the startup may turn `GLOB_DOTS` or `dotglob` on
/// unseen: a line at either snapshot's top level this check does not read, a `.zshenv` not proven
/// to set no option, a shell the Bash tool may run with no snapshot of its own read, and bash
/// with `GLOBIGNORE` in its environment; and a `GLOBIGNORE` Claude Code's settings give the Bash
/// tool, which the worker's environment does not show, fails the read. A snapshot whose every
/// top-level line is read leaves it off, as does a bash worker's read with no `GLOBIGNORE` (bash
/// reads no `.zshenv`, which keeps a host's `/etc/zshenv` out of it).
#[test]
fn a_glob_may_meet_a_leading_dot_wherever_the_startup_may_set_it_unseen() {
    let parsed = |kind: Kind, snapshot: &str| {
        let mut state = ShellStartup::default();
        parse_snapshot(kind, snapshot, "s", &mut state).expect("read");
        state
    };
    let zsh = parsed(Kind::Zsh, &zsh_snapshot("options[globdots]=on\n", "", ""));
    assert!(zsh.may_dot_glob() && zsh.may_extended_glob(), "{zsh:?}");
    let bash = parsed(Kind::Bash, &bash_snapshot(&[], "GLOBIGNORE=zz\n", "", ""));
    assert!(bash.may_dot_glob() && !bash.may_extended_glob(), "{bash:?}");
    let s = Scratch::new("dotglob-zshenv");
    s.snapshot("zsh", 1, &zsh_snapshot("", "", ""));
    std::fs::write(s.home().join(".zshenv"), "typeset -U path\n").expect("zshenv");
    let state = s.read(&s.worker("/bin/zsh", &[])).expect("read");
    assert!(state.may_dot_glob(), "an unproven .zshenv: {state:?}");
    let s = Scratch::new("dotglob-env");
    s.snapshot("bash", 1, &bash_snapshot(&[], "", "", ""));
    let state = s
        .read(&s.worker("/bin/bash", &["GLOBIGNORE=zz"]))
        .expect("read");
    assert!(state.may_dot_glob(), "GLOBIGNORE: {state:?}");
    std::fs::create_dir_all(s.project().join(".claude")).expect(".claude");
    std::fs::write(
        s.project().join(".claude/settings.json"),
        "{\"env\": {\"GLOBIGNORE\": \"zz\"}}",
    )
    .expect("settings");
    let why = s
        .read(&s.worker("/bin/bash", &[]))
        .expect_err("GLOBIGNORE in the settings' env");
    assert!(why.contains("`GLOBIGNORE`"), "{why}");
    for (snapshot, worker) in [
        ("bash", ["/bin/zsh", "CLAUDE_CODE_SHELL=/bin/bash"]),
        ("zsh", ["/bin/bash", "CLAUDE_CODE_SHELL=/bin/zsh"]),
    ] {
        let s = Scratch::new("dotglob-unseen");
        let text = if snapshot == "zsh" {
            zsh_snapshot("", "", "")
        } else {
            bash_snapshot(&[], "", "", "")
        };
        s.snapshot(snapshot, 1, &text);
        let state = s.read(&s.worker(worker[0], &worker[1..])).expect("read");
        assert!(
            state.may_dot_glob(),
            "only a {snapshot} snapshot read: {state:?}"
        );
    }
    // NEGATIVE CONTROL
    for state in [
        parsed(
            Kind::Zsh,
            &zsh_snapshot("f () {\n\tsetopt globdots\n}\n", "", ""),
        ),
        parsed(Kind::Zsh, &zsh_snapshot("", "", "alias -- ll='ls -l'\n")),
        parsed(
            Kind::Bash,
            &bash_snapshot(
                &[("extglob", true)],
                "eval $'rm () \\n{ \\n    command rm -i \"$@\"\\n}' > /dev/null 2>&1\n",
                "set -o noclobber\n",
                "alias -- ll='ls -l'\n",
            ),
        ),
    ] {
        assert!(!state.may_dot_glob(), "{state:?}");
    }
    let s = Scratch::new("dotglob-clean");
    s.snapshot("bash", 1, &bash_snapshot(&[], "", "", ""));
    let state = s.read(&s.worker("/bin/bash", &[])).expect("read");
    assert!(!state.may_dot_glob() && !state.may_run_zsh(), "{state:?}");
}

/// zsh's `EXTENDED_GLOB` may be on wherever the startup may set it unseen: a zsh snapshot's
/// `setopt extendedglob` in any spelling zsh reads, a line at the snapshot's top level this check
/// does not read, a `.zshenv` not proven to set no option, or zsh running with no snapshot of its
/// own read. A snapshot whose every top-level line is read and sets it nowhere, and a `.zshenv`
/// of plain assignments, leave it off (judged by the parts themselves, which a host's
/// `/etc/zshenv` does not reach).
#[test]
fn extended_glob_may_be_on_wherever_the_startup_may_set_it_unseen() {
    let read = |snapshot: &str, zshenv: Option<&str>| {
        let s = Scratch::new("xglob");
        s.snapshot("zsh", 1, snapshot);
        if let Some(text) = zshenv {
            std::fs::write(s.home().join(".zshenv"), text).expect("zshenv");
        }
        s.read(&s.worker("/bin/zsh", &[]))
            .expect("a startup this check reads")
    };
    for spelling in [
        "extendedglob",
        "EXTENDED_GLOB",
        "extended_glob",
        "Extended_Glob",
    ] {
        let state = read(&zsh_snapshot("", &format!("setopt {spelling}\n"), ""), None);
        assert!(state.may_extended_glob(), "{spelling}");
    }
    for top in [
        "options[extendedglob]=on\n",
        "options+=(extendedglob on)\n",
        "turn_it_on\n",
        "export X=1\n",
    ] {
        let state = read(&zsh_snapshot(top, "", ""), None);
        assert!(state.may_extended_glob(), "a top-level line: {top:?}");
    }
    for zshenv in [
        "typeset -U path\n",
        "X=$(id -u)\n",
        "export X=\"a b\"\n",
        "ZDOTDIR=$HOME/.zsh\n",
        "[[ -n $SSH_TTY ]] && export X=1\n",
        "x=1 tool\n",
        "N=${#PATH}\n",
    ] {
        let state = read(&zsh_snapshot("", "", ""), Some(zshenv));
        assert!(state.may_extended_glob(), "a .zshenv: {zshenv:?}");
    }
    let s = Scratch::new("xglob-bash");
    s.snapshot("bash", 1, &bash_snapshot(&[], "", "", ""));
    let state = s
        .read(&s.worker("/bin/zsh", &["CLAUDE_CODE_SHELL=/bin/bash"]))
        .expect("bash's snapshot");
    assert!(
        state.may_run_zsh() && state.may_extended_glob(),
        "zsh may run with none of its own snapshots read: {state:?}"
    );
    // NEGATIVE CONTROL
    for snapshot in [
        zsh_snapshot("", "", ""),
        zsh_snapshot("", "setopt noextendedglob\nsetopt NO_EXTENDED_GLOB\n", ""),
        zsh_snapshot("f () {\n\tsetopt localoptions extendedglob\n}\n", "", ""),
        zsh_snapshot("", "", "alias -- ll='ls -l'\n"),
    ] {
        let mut state = ShellStartup::default();
        parse_snapshot(Kind::Zsh, &snapshot, "s", &mut state).expect("read");
        assert!(
            state.may_run_zsh() && !state.may_extended_glob(),
            "{snapshot:?}"
        );
    }
    for zshenv in [
        "",
        "# setopt extendedglob\n\nexport PATH=\"$HOME/bin:${PATH}\"\n",
        "export LANG=en_US.UTF-8 EDITOR\nX=1 # a note\nexport GOPATH=$HOME/go\n",
    ] {
        assert_eq!(zshenv_hazard(zshenv), None, "{zshenv:?}");
        assert!(zshenv_sets_no_option(zshenv), "{zshenv:?}");
    }
}

/// A `.zshenv` that names `options` or `histchars` (`HISTCHARS`) as a parameter, or holds a
/// parameter flag, may set an option or move the comment character unseen, so it fails every
/// line. MEASURED 2026-11-05 with zsh 5.9, each in a `.zshenv` a `zsh -c` read: the first three
/// rows, the `(P)` pair and the `print -v` row set `EXTENDED_GLOB`, and the three `histchars`
/// rows made `#c` no comment, so the command after it on that line ran
/// (`docs/measured/classifier-zsh-glob-2026-11-05.md`). A value that only spells the word is no
/// parameter.
#[test]
fn a_zshenv_naming_options_or_histchars_or_a_parameter_flag_fails_every_line() {
    for (text, verdict) in [
        ("options+=(extendedglob on)", "its line 1 names `options`"),
        ("options=(extendedglob on)", "its line 1 names `options`"),
        (
            "export A=1\noptions[extendedglob]=on",
            "its line 2 runs `options`",
        ),
        (
            "n=options\\[extendedglob\\]; : ${(P)n::=on}",
            "its line 1 holds a parameter flag `${(`",
        ),
        ("histchars='!^x'", "its line 1 names `histchars`"),
        ("HISTCHARS='!^x'", "its line 1 names `HISTCHARS`"),
        ("typeset histchars=x", "its line 1 names `histchars`"),
        (
            "print -v 'options[extendedglob]' on",
            "its line 1 runs `options`",
        ),
    ] {
        assert_eq!(zshenv_hazard(text).as_deref(), Some(verdict), "{text}");
    }
    let s = Scratch::new("zshenv-names");
    s.snapshot("zsh", 1, &zsh_snapshot("", "", ""));
    std::fs::write(s.home().join(".zshenv"), "options+=(globsubst on)\n").expect("zshenv");
    let why = s.read(&s.worker("/bin/zsh", &[])).expect_err("options");
    assert!(why.contains("its line 1 names `options`"), "{why}");
    // NEGATIVE CONTROL
    for text in [
        "export GREP_OPTIONS=\"--exclude-options\"",
        "export MY_OPTIONS=1 LESSHISTFILE=-",
        "N=${#PATH}",
        "export PATH=\"$HOME/bin:${PATH}\"",
    ] {
        assert_eq!(zshenv_hazard(text), None, "{text}");
    }
}

#[test]
fn a_word_is_named_only_whole() {
    for (line, hit) in [
        ("ls", true),
        ("x; ls -l", true),
        ("(ls)", true),
        ("\"ls\" x", true),
        ("a|ls", true),
        ("/bin/ls", false),
        ("lsof", false),
        ("x-ls", false),
        ("ls.txt", false),
        ("k=ls", false),
    ] {
        assert_eq!(names_word(line, "ls"), hit, "{line}");
    }
}

#[test]
fn a_prompt_or_history_hook_is_an_ordinary_function() {
    let s = Scratch::new("prompt-hooks");
    s.snapshot(
        "zsh",
        1,
        &zsh_snapshot(
            "precmd () {\n\tvcs_info\n}\npreexec () {\n\tprint -Pn \"\\e]0;$1\\a\"\n}\n\
             periodic () {\n\t:\n}\nzshaddhistory () {\n\t:\n}\n",
            "",
            "",
        ),
    );
    let state = s
        .read(&s.worker("/bin/zsh", &[]))
        .expect("an ordinary startup");
    assert_eq!(state.admits("git status --short"), Ok(()));
    for f in ["precmd", "preexec", "periodic", "zshaddhistory"] {
        let why = state.admits(&format!("x; {f}")).expect_err(f);
        assert!(why.contains(&format!("`{f}` is a function")), "{why}");
    }
}

#[test]
fn a_function_is_named_by_any_quoting_of_its_name() {
    let s = Scratch::new("quoted-fn");
    s.snapshot(
        "zsh",
        1,
        &zsh_snapshot(
            "rm () {\n\tcommand rm -i \"$@\"\n}\ngit () {\n\tcommand git \"$@\"\n}\n",
            "",
            "",
        ),
    );
    let state = s.read(&s.worker("/bin/zsh", &[])).expect("defaults");
    for line in [
        "r\\m -rf /private/tmp/x",
        "r\"\"m -rf /private/tmp/x",
        "r''m -rf /private/tmp/x",
        "\"r\"m -rf /private/tmp/x",
        "$'\\x72m' -rf /private/tmp/x",
        "$'\\162m' -rf /private/tmp/x",
        "$'r\\x6d' -rf /private/tmp/x",
        "$\"rm\" -rf /private/tmp/x",
        "r\\\nm -rf /private/tmp/x",
        "eval \"r\\\\m d\"",
        "eval \"\\$'\\\\x72m' e\"",
        "g''it status",
        "c\\d /tmp && g\\it log",
    ] {
        assert!(state.admits(line).is_err(), "{line:?}");
    }
    for line in ["/bin/rm -f /private/tmp/x", "rmdir x", "echo arm", "gitk"] {
        assert_eq!(state.admits(line), Ok(()), "{line:?}");
    }
}

#[test]
fn an_alias_fails_only_a_line_that_may_run_it() {
    let s = Scratch::new("omz-aliases");
    let mut aliases = String::from("alias -- -='cd -'\nalias -- ...=../..\n");
    for n in 1..=9 {
        aliases.push_str(&format!("alias -- {n}='cd -{n}'\n"));
    }
    aliases.push_str("alias -- _='sudo '\nalias -- ll='ls -lh'\nalias -- md='mkdir -p'\n");
    s.snapshot(
        "zsh",
        1,
        &zsh_snapshot(
            "",
            "setopt autopushd\nsetopt pushdignoredups\nsetopt pushdminus\nsetopt pushdtohome\n",
            &aliases,
        ),
    );
    let state = s
        .read(&s.worker("/bin/zsh", &[]))
        .expect("oh-my-zsh's directory options change nothing the models read");
    for line in [
        "cargo test 2>&1 | tail -n 5",
        "git log -1 --oneline",
        "cd - && ls",
        "cd ../.. && ls ...",
        "sleep 1; echo 2 3",
        "head -5 x | sort -k 2",
        "find . -maxdepth 2 -name ll",
        "echo ll md",
        "sudo ll",
        "nice -n 5 ls",
    ] {
        assert_eq!(state.admits(line), Ok(()), "{line:?}");
    }
    for line in [
        "5",
        "true && 1",
        "x; 2",
        "x || 3",
        "x | 4 y",
        "(6)",
        "{ 7; }",
        "echo $(ll)",
        "echo `ll`",
        "echo \"$(ll)\"",
        "X=1 ll",
        "X=1 Y=2 ll",
        "time ll",
        "time -p ll",
        "! ll",
        "if ll; then :; fi",
        "if [[ -n x ]] ll",
        "if (( 1 )) ll",
        "while [[ -z $d ]] { ll; d=1 }",
        "while :; do ll; done",
        "for i in a; do md x; done",
        "> /dev/null ll",
        ">/dev/null ll",
        "2>&1 ll",
        ">| /tmp/f ll",
        "noglob ll",
        "nocorrect ll",
        "repeat 2 ll",
        "_ ll",
        "x\nll",
        "-",
        "cd /tmp; -",
        "...",
        "eval \"ll x\"",
        "eval ll",
        "\\ll",
    ] {
        assert!(state.admits(line).is_err(), "{line:?}");
    }
}

#[test]
fn a_startup_verdict_names_a_line_by_its_number_never_its_text() {
    let secret = "planted-zshenv-0005"; // gitleaks:allow
    for (text, verdict) in [
        (
            format!("export A=1\nalias deploy='AWS_SECRET_ACCESS_KEY={secret} ./deploy'\n"),
            "its line 2 runs `alias`",
        ),
        (
            format!("# {secret}\nexport B=2\nk() {{ echo {secret} }}\n"),
            "its line 3 defines a function",
        ),
        (
            format!("eval \"$(tool --token {secret} init zsh)\""),
            "its line 1 runs `eval`",
        ),
    ] {
        assert_eq!(zshenv_hazard(&text).as_deref(), Some(verdict), "{text}");
    }
    let s = Scratch::new("zshenv-number");
    s.snapshot("zsh", 1, &zsh_snapshot("", "", ""));
    std::fs::write(
        s.home().join(".zshenv"),
        format!("alias k='K={secret} k'\n"),
    )
    .expect("zshenv");
    let why = s.read(&s.worker("/bin/zsh", &[])).expect_err("an alias");
    assert!(why.contains("its line 1 runs `alias`"), "{why}");
    assert!(!why.contains(secret), "{why}");
    let s = Scratch::new("snapshot-line");
    s.snapshot(
        "zsh",
        1,
        &zsh_snapshot("", &format!("setopt {secret} on\n"), ""),
    );
    let why = s
        .read(&s.worker("/bin/zsh", &[]))
        .expect_err("a setopt row");
    assert!(
        why.contains("a `setopt` line this check does not read"),
        "{why}"
    );
    assert!(!why.contains(secret), "{why}");
}

/// `admits` for `line` under `state` names the startup's `$var` (a `READER_ENV` or git
/// variable it may set).
fn fails_by(state: &ShellStartup, line: &str, var: &str) {
    let why = state.admits(line).expect_err(line);
    assert!(why.contains(&format!("`${var}`")), "{line}: {why}");
}

/// A `READER_ENV` variable the worker's environment sets fails a line that may run a reader it
/// redirects, by name or by a path's last part, behind `xargs`, `find -exec` or a substitution
/// (`GREP_OPTIONS='-e x f'` makes BSD grep read `f`, MEASURED 2026-11-05 on macOS 27.0.1); a
/// startup that sets none, or only another, passes the line.
#[test]
fn a_reader_variable_the_worker_sets_fails_the_lines_running_its_reader() {
    let s = Scratch::new("reader-env");
    s.snapshot("zsh", 1, &zsh_snapshot("", "", ""));
    let clean = s.read(&s.worker("/bin/zsh", &[])).expect("a clean startup");
    for line in ["grep x f", "rg x", "python3 r.py"] {
        assert_eq!(clean.admits(line), Ok(()), "{line}");
    }
    let env = s
        .read(&s.worker("/bin/zsh", &["GREP_OPTIONS=-e x /etc/passwd"]))
        .expect("a startup");
    for line in [
        "grep x f",
        "egrep x f",
        "/usr/bin/fgrep x f",
        "find . -exec grep x {} +",
        "ls | xargs grep x",
        "echo $(gr''ep x f)",
    ] {
        fails_by(&env, line, "GREP_OPTIONS");
    }
    // NEGATIVE CONTROL: another reader, or the name inside a longer word.
    for line in ["rg x", "cat grep.txt", "cat my-grep"] {
        assert_eq!(env.admits(line), Ok(()), "{line}");
    }
    let python = s
        .read(&s.worker("/bin/zsh", &["PYTHONPATH=/tmp/x"]))
        .expect("a startup");
    fails_by(&python, "python3 r.py", "PYTHONPATH");
    assert_eq!(python.admits("grep x f"), Ok(()));
}

/// A `READER_ENV` variable Claude Code's settings for the launch directory give the Bash tool
/// fails a line running its reader, as one the worker's environment sets does.
#[test]
fn a_reader_variable_claude_codes_settings_give_fails_the_lines_running_its_reader() {
    let s = Scratch::new("reader-settings");
    s.snapshot("zsh", 1, &zsh_snapshot("", "", ""));
    std::fs::create_dir_all(s.project().join(".claude")).expect(".claude");
    std::fs::write(
        s.project().join(".claude/settings.json"),
        "{\"env\": {\"RIPGREP_CONFIG_PATH\": \"/x/rg.conf\", \"LANG\": \"C\"}}",
    )
    .expect("settings");
    let settings = s.read(&s.worker("/bin/zsh", &[])).expect("a startup");
    fails_by(&settings, "rg -n x src", "RIPGREP_CONFIG_PATH");
    // NEGATIVE CONTROL: another reader.
    assert_eq!(settings.admits("grep x f"), Ok(()));
}

/// A `.zshenv` that names a `READER_ENV` variable in its code, in any quoting, fails a line
/// running its reader. bash reads no `.zshenv`.
#[test]
fn a_reader_variable_a_zshenv_names_fails_the_lines_running_its_reader() {
    let s = Scratch::new("reader-zshenv");
    s.snapshot("zsh", 1, &zsh_snapshot("", "", ""));
    let zshenv = s.home().join(".zshenv");
    for text in [
        "export RIPGREP_CONFIG_PATH=\"$HOME/.ripgreprc\"\n",
        "typeset -x RIPGREP_CONFIG_PATH=~/.rg\n",
        "export RIPGREP_\\CONFIG_PATH=~/.rg\n",
        "export \"RIPGREP_CONFIG_PATH\"=~/.rg\n",
    ] {
        std::fs::write(&zshenv, text).expect("zshenv");
        let state = s.read(&s.worker("/bin/zsh", &[])).expect(text);
        fails_by(&state, "rg x", "RIPGREP_CONFIG_PATH");
    }
    // NEGATIVE CONTROL: a .zshenv naming no such variable, or one only inside a longer name or
    // a comment.
    for text in [
        "export PATH=\"$HOME/bin:$PATH\"\n",
        "export MY_RIPGREP_CONFIG_PATHS=x\n",
        "# export RIPGREP_CONFIG_PATH=~/.rg\n",
    ] {
        std::fs::write(&zshenv, text).expect("zshenv");
        let state = s.read(&s.worker("/bin/zsh", &[])).expect(text);
        assert_eq!(state.admits("rg x"), Ok(()), "{text}");
    }
    let b = Scratch::new("reader-zshenv-bash");
    b.snapshot("bash", 1, &bash_snapshot(&[], "", "", ""));
    std::fs::write(b.home().join(".zshenv"), "export GREP_OPTIONS=-r\n").expect("zshenv");
    let bash = b.read(&b.worker("/bin/bash", &[])).expect("a bash startup");
    assert_eq!(bash.admits("grep x f"), Ok(()));
}

/// macOS's `/usr/bin/python3` is the xcrun shim `/usr/bin/git` is, and `DEVELOPER_DIR=<dir>`
/// made it run `<dir>/usr/bin/xcrun` (MEASURED 2026-11-05 on macOS 27.0.1), so a `DEVELOPER_DIR`
/// or `SDKROOT` the worker's environment, Claude Code's settings or a `.zshenv` may set fails a
/// python line and a git read, whatever its value; a startup that sets neither passes both.
#[test]
fn an_xcrun_shim_variable_the_startup_may_set_fails_python_and_git() {
    let s = Scratch::new("xcrun-shim");
    s.snapshot("zsh", 1, &zsh_snapshot("", "", ""));
    let clean = s.read(&s.worker("/bin/zsh", &[])).expect("a clean startup");
    for line in ["python3 tools/r.py", "git status --short"] {
        assert_eq!(clean.admits(line), Ok(()), "{line}");
    }
    for var in ["DEVELOPER_DIR", "SDKROOT"] {
        let kv = format!("{var}=/Applications/Xcode.app");
        let env = s
            .read(&s.worker("/bin/zsh", &[kv.as_str()]))
            .expect("a startup");
        for line in [
            "python3 tools/r.py",
            "/usr/bin/python3 tools/r.py",
            "git status --short",
        ] {
            fails_by(&env, line, var);
        }
        // NEGATIVE CONTROL: a reader no xcrun shim runs.
        assert_eq!(env.admits("cat README.md"), Ok(()), "{var}");
    }
    std::fs::create_dir_all(s.project().join(".claude")).expect(".claude");
    let settings = s.project().join(".claude/settings.json");
    std::fs::write(&settings, "{\"env\": {\"DEVELOPER_DIR\": \"./dev\"}}").expect("settings");
    let state = s.read(&s.worker("/bin/zsh", &[])).expect("a startup");
    fails_by(&state, "python3 tools/r.py", "DEVELOPER_DIR");
    std::fs::remove_file(&settings).expect("settings gone");
    std::fs::write(s.home().join(".zshenv"), "export SDKROOT=\"$HOME/sdk\"\n").expect("zshenv");
    let state = s.read(&s.worker("/bin/zsh", &[])).expect("a startup");
    fails_by(&state, "python3 tools/r.py", "SDKROOT");
    fails_by(&state, "git log -1", "SDKROOT");
}

/// A `.zshenv` that names a variable moving what git loads or which git runs
/// (`git_config::git_relevant`'s past `PATH` and `HOME`) fails a git read, as git's probe runs
/// with the worker's own environment; one that names `PATH`, git's pager, or such a name only in
/// a comment does not.
#[test]
fn a_git_variable_a_zshenv_names_fails_a_git_read() {
    let s = Scratch::new("git-zshenv");
    s.snapshot("zsh", 1, &zsh_snapshot("", "", ""));
    let zshenv = s.home().join(".zshenv");
    for (text, var) in [
        ("export GIT_EXTERNAL_DIFF=/tmp/x\n", "GIT_EXTERNAL_DIFF"),
        ("export XDG_CONFIG_HOME=\"$HOME/.cfg\"\n", "XDG_CONFIG_HOME"),
        (
            "DEVELOPER_DIR=/tmp/dev; export DEVELOPER_DIR\n",
            "DEVELOPER_DIR",
        ),
    ] {
        std::fs::write(&zshenv, text).expect("zshenv");
        let state = s.read(&s.worker("/bin/zsh", &[])).expect(text);
        fails_by(&state, "git diff HEAD~1", var);
        assert_eq!(state.admits("cat README.md"), Ok(()), "{text}");
    }
    // NEGATIVE CONTROL: PATH, git's pager, or a git variable only in a comment.
    for text in [
        "export PATH=\"$HOME/bin:$PATH\"\n",
        "# export GIT_DIR=/tmp/x\n",
        "export GIT_PAGER=delta\n",
    ] {
        std::fs::write(&zshenv, text).expect("zshenv");
        let state = s.read(&s.worker("/bin/zsh", &[])).expect(text);
        assert_eq!(state.admits("git log -1"), Ok(()), "{text}");
    }
}
