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

    fn read(&self, worker: &WorkerEnv) -> Result<ShellStartup, String> {
        read(worker, &self.project())
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
    s.snapshot("bash", 1, &bash_snapshot(&[], "", "", ""));
    assert!(s.read(&s.worker("/bin/zsh", &[])).is_err());
    s.snapshot("zsh", 2, &zsh_snapshot("", "", ""));
    assert!(s.read(&s.worker("/bin/zsh", &[])).is_ok());
    s.snapshot("zsh", 3, &zsh_snapshot("", "setopt globsubst\n", ""));
    assert!(s.read(&s.worker("/bin/zsh", &[])).is_err());
    assert_eq!(
        tool_kinds(&s.worker("/bin/zsh", &["CLAUDE_CODE_SHELL=/bin/bash"])),
        [Kind::Bash, Kind::Zsh]
    );
    assert_eq!(tool_kinds(&s.worker("/bin/sh", &[])), [Kind::Zsh]);
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
        "x=${#y}; set -o shwordsplit",
        "rm() { echo }",
    ] {
        std::fs::write(&zshenv, text).expect("zshenv");
        let why = s.read(&s.worker("/bin/zsh", &[])).expect_err(text);
        assert!(why.contains(".zshenv"), "{text}: {why}");
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
