// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;

use super::*;

/// Only Claude Code's twin carries the block, only where an atpkg is named, and never on Windows,
/// whose `.cmd` twin carries no prelude.
#[test]
fn only_claudes_twin_carries_the_block() {
    let atpkg = Path::new("/Applications/aterm.app/Contents/MacOS/atpkg");
    assert!(of("claude"));
    for other in ["codex", "agy", "muse", "codex-old", "claudex"] {
        assert!(!of(other), "{other}");
        assert_eq!(sh_prelude(other, Some(atpkg)), "", "{other}");
    }
    assert_eq!(sh_prelude("claude", None), "");
    assert_eq!(sh_prelude("claude", Some(Path::new(""))), "");
    assert_eq!(
        aterm_beside(atpkg).as_deref(),
        Some(Path::new(if cfg!(windows) {
            "/Applications/aterm.app/Contents/MacOS/aterm.exe"
        } else {
            "/Applications/aterm.app/Contents/MacOS/aterm"
        }))
    );
    if cfg!(windows) {
        assert_eq!(sh_prelude("claude", Some(atpkg)), "");
    }
}

/// The block names the aterm beside atpkg, quoted for `sh`, asks it with the launch's own
/// arguments after aterm's verb, and puts what it printed ahead of them.
#[cfg(unix)]
#[test]
fn the_block_asks_the_aterm_beside_atpkg() {
    let block = sh_prelude("claude", Some(Path::new("/it's/MacOS/atpkg")));
    assert!(block.starts_with("# atpkg: aterm's usage tap"), "{block}");
    assert!(
        block.contains(
            "  if [ -t 1 ]; then __atpkg_tty=1; else __atpkg_tty=0; fi\n  \
             __atpkg_tap=$(ATERM_LAUNCH_STDOUT_TTY=$__atpkg_tty '/it'\\''s/MacOS/aterm' harness \
             tap --launch -- \"$@\" </dev/null 2>/dev/null) || __atpkg_tap=\n"
        ),
        "{block}"
    );
    assert!(
        block.contains("then set -- --settings \"$__atpkg_tap\" \"$@\"; fi\n"),
        "{block}"
    );
    assert!(!block.contains("/it's/"), "{block}");
    assert!(
        !block.lines().any(|l| l.trim().starts_with("exec ")),
        "the shim's own exec stays the one exec: {block}"
    );
}

/// A scratch layout for the executed twins: a store build that prints its argv, and a stand-in
/// aterm beside a stand-in atpkg.
#[cfg(unix)]
struct Laid {
    root: crate::testdir::Dir,
    atpkg: PathBuf,
}

#[cfg(unix)]
impl Laid {
    fn new() -> Self {
        let root = crate::testdir::dir("atpkg-launch-tap-");
        let bundle = root.join("MacOS");
        std::fs::create_dir_all(&bundle).unwrap();
        let atpkg = bundle.join("atpkg");
        exe(&atpkg, "#!/bin/sh\nexit 2\n");
        exe(
            &root.join("store-build"),
            "#!/bin/sh\nprintf '<%s>' \"$@\"\nprintf '\\n'\n",
        );
        Self { root, atpkg }
    }

    /// Lay the stand-in aterm, which answers `harness tap --launch -- <args>` with `body` (its
    /// arguments are written to `asked` first).
    fn aterm(&self, body: &str) {
        exe(
            &aterm_beside(&self.atpkg).unwrap(),
            &format!(
                "#!/bin/sh\nprintf '<%s>' \"$@\" > '{}'\n{body}\n",
                self.asked().display()
            ),
        );
    }

    fn asked(&self) -> PathBuf {
        self.root.join("asked")
    }

    /// Lay Claude Code's twin as atpkg lays it (its `--yolo` block, then the usage tap's) and run
    /// it with `args` under `env` (every other variable of the usage tap's removed).
    fn run(&self, args: &[&str], env: &[(&str, &str)]) -> String {
        self.run_twin(args, env, false)
    }

    /// [`Self::run`] with the token block laid too where `token` (a twin that is no dev link).
    fn run_twin(&self, args: &[&str], env: &[(&str, &str)], token: bool) -> String {
        let mut prelude = crate::platform::twin_prelude(
            "claude",
            &self.root.join("pkg"),
            None,
            &[],
            &crate::yolo::policy_of("claude"),
        );
        prelude.push_str(&sh_prelude("claude", Some(&self.atpkg)));
        let twin = self.root.join("claude");
        let body = crate::platform::sh_shim_content_twin(
            &self.root.join("store-build"),
            &crate::shim_env::ShimEnv::NONE,
            None,
            &prelude,
        );
        std::fs::write(
            &twin,
            if token {
                with_token_block("claude", body, Some(&self.atpkg))
            } else {
                body
            },
        )
        .unwrap();
        set_exec(&twin);
        let out = aterm_spec::wait::past_text_busy(|| {
            let mut command = std::process::Command::new(&twin);
            command
                .args(args)
                .env_remove("ATERM_PARENT_SESSION_ID")
                .env_remove("ATERM_NO_HARNESS");
            for (key, value) in env {
                command.env(key, value);
            }
            command.output()
        })
        .unwrap();
        assert!(out.status.success(), "{args:?}: {out:?}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

#[cfg(unix)]
fn set_exec(path: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[cfg(unix)]
fn exe(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
    set_exec(path);
}

/// THE TWIN TAPS A CLAUDE CODE STARTED IN AN ATERM TAB FROM ITS START (docs/OWNER_RULINGS.md,
/// 2026-10-24): inside a tab, the value aterm printed rides as
/// `--settings <value>` ahead of exactly the arguments the twin would run without it (the `--yolo`
/// default's flag among them where it applies), one argument whatever it holds, and aterm was
/// asked with exactly those arguments; outside a tab, under `--no-harness`, where aterm printed
/// nothing, failed, or is not there, the launch runs exactly as typed.
#[cfg(unix)]
#[test]
fn the_twin_taps_a_launch_in_an_aterm_tab_and_otherwise_runs_it_as_typed() {
    let laid = Laid::new();
    let tab = [("ATERM_PARENT_SESSION_ID", "s-60776b6e2ed1b73bd013")];
    let value = r#"{"statusLine":{"type":"command","command":"/x/aterm harness tap","refreshInterval":10}}"#;
    laid.aterm(&format!("printf '%s\\n' '{value}'"));
    for args in [
        &["-p", "hello"][..],
        &["--model", "opus"][..],
        &["--model", "opus", "fix it's bug"][..],
        &["-p", "two  spaces", "--resume", "x"][..],
    ] {
        let _ = std::fs::remove_file(laid.asked());
        let untapped = laid.run(args, &[]);
        assert!(
            !laid.asked().exists(),
            "aterm is not asked outside a tab: {args:?}"
        );
        assert_eq!(
            laid.run(args, &tab),
            format!("<--settings><{value}>{untapped}"),
            "{args:?}"
        );
        assert_eq!(
            std::fs::read_to_string(laid.asked()).unwrap(),
            format!(
                "<harness><tap><--launch><-->{}",
                untapped.trim_end_matches('\n')
            ),
            "{args:?}"
        );
    }
    assert_eq!(
        laid.run(&["-p", "hello"], &tab),
        format!("<--settings><{value}><--dangerously-skip-permissions><-p><hello>\n"),
        "the --yolo default's flag rides behind the tap's"
    );
    let typed = "<--model><opus>\n";
    assert_eq!(laid.run(&["--model", "opus"], &[]), typed);
    let _ = std::fs::remove_file(laid.asked());
    assert_eq!(
        laid.run(&["--model", "opus"], &[tab[0], ("ATERM_NO_HARNESS", "1")]),
        typed,
        "under --no-harness"
    );
    assert_eq!(
        laid.run(&["--no-harness", "--model", "opus"], &tab),
        typed,
        "the twin's own --no-harness"
    );
    assert!(
        !laid.asked().exists(),
        "aterm is not asked under --no-harness"
    );
    laid.aterm(":");
    assert_eq!(
        laid.run(&["--model", "opus"], &tab),
        typed,
        "nothing printed"
    );
    laid.aterm(&format!("printf '%s\\n' '{value}'; exit 1"));
    assert_eq!(laid.run(&["--model", "opus"], &tab), typed, "aterm failed");
    std::fs::remove_file(aterm_beside(&laid.atpkg).unwrap()).unwrap();
    assert_eq!(
        laid.run(&["--model", "opus"], &tab),
        typed,
        "no aterm there"
    );
}

/// THE BLOCK TELLS ATERM WHETHER THE LAUNCH'S OUTPUT IS A TERMINAL, EXECUTED: the aterm it asks
/// reads [`STDOUT_TTY_ENV`] as `1` where the twin's standard output is a terminal and `0` where
/// it is a pipe (a tool's shell, `claude … | tee`), judged before the command substitution that
/// captures aterm's answer, whose own output is always a pipe. The variable is that one
/// command's: the build the twin then runs does not hold it.
#[cfg(unix)]
#[test]
fn the_block_tells_aterm_whether_the_launchs_output_is_a_terminal() {
    use std::os::fd::{FromRawFd as _, OwnedFd};
    let laid = Laid::new();
    let said = laid.root.join("said");
    laid.aterm(&format!(
        "printf '%s' \"${{{STDOUT_TTY_ENV}-unset}}\" > '{}'",
        said.display()
    ));
    exe(
        &laid.root.join("store-build"),
        &format!("#!/bin/sh\nprintf '%s\\n' \"${{{STDOUT_TTY_ENV}-unset}}\"\n"),
    );
    let tab = ("ATERM_PARENT_SESSION_ID", "s-60776b6e2ed1b73bd013");
    // This test's own capture of the twin's output is a pipe.
    assert_eq!(
        laid.run(&["--model", "opus"], &[tab]),
        "unset\n",
        "the build does not hold the variable"
    );
    assert_eq!(std::fs::read_to_string(&said).unwrap(), "0", "a pipe");
    // The same twin with a terminal for its output.
    let (master, slave) = aterm_pty::open_pty_pair().expect("a terminal");
    // SAFETY: each fresh PTY descriptor has exactly one owner here.
    let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
    let twin = laid.root.join("claude");
    let status = aterm_spec::wait::past_text_busy(|| {
        std::process::Command::new(&twin)
            .args(["--model", "opus"])
            .env(tab.0, tab.1)
            .env_remove("ATERM_NO_HARNESS")
            .stdin(std::process::Stdio::null())
            .stdout(slave.try_clone().expect("the terminal's descriptor"))
            .status()
    })
    .unwrap();
    assert!(status.success(), "{status:?}");
    assert_eq!(std::fs::read_to_string(&said).unwrap(), "1", "a terminal");
    drop((master, slave));
}

/// THE RENDERER BLOCK, EXECUTED: a launch in an aterm tab that names no renderer runs with
/// `CLAUDE_CODE_NO_FLICKER=1` (Claude Code's fullscreen renderer from its first start); one that
/// names a renderer in its environment (any value, empty included) or by a `"tui":` key in the
/// person's own `settings.json` (`$CLAUDE_CONFIG_DIR`, else `$HOME/.claude`), one outside a tab,
/// and one under `--no-harness` keep what they had. NEGATIVE CONTROL: the plain twin without the
/// block (`sh_prelude` left out) leaves the variable unset in the same tab.
#[cfg(unix)]
#[test]
fn a_tab_launch_that_names_no_renderer_runs_fullscreen() {
    let laid = Laid::new();
    laid.aterm(":");
    let home = laid.root.join("home");
    let config = laid.root.join("config");
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    std::fs::create_dir_all(&config).unwrap();
    exe(
        &laid.root.join("env-build"),
        "#!/bin/sh\nprintf '%s|%s\\n' \"${CLAUDE_CODE_NO_FLICKER-unset}\" \
         \"${CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN-unset}\"\n",
    );
    let twin = |with_block: bool| {
        let prelude = if with_block {
            sh_prelude("claude", Some(&laid.atpkg))
        } else {
            String::new()
        };
        let path = laid.root.join(if with_block { "claude" } else { "plain" });
        std::fs::write(
            &path,
            crate::platform::sh_shim_content_twin(
                &laid.root.join("env-build"),
                &crate::shim_env::ShimEnv::NONE,
                None,
                &prelude,
            ),
        )
        .unwrap();
        set_exec(&path);
        path
    };
    let run = |path: &Path, env: &[(&str, &str)]| {
        let out = aterm_spec::wait::past_text_busy(|| {
            let mut command = std::process::Command::new(path);
            command
                .env("HOME", &home)
                .env_remove("CLAUDE_CONFIG_DIR")
                .env_remove("CLAUDE_CODE_NO_FLICKER")
                .env_remove("CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN")
                .env_remove("ATERM_PARENT_SESSION_ID")
                .env_remove("ATERM_NO_HARNESS");
            for (key, value) in env {
                command.env(key, value);
            }
            command.output()
        })
        .unwrap();
        assert!(out.status.success(), "{env:?}: {out:?}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let with = twin(true);
    let tab = ("ATERM_PARENT_SESSION_ID", "s-60776b6e2ed1b73bd013");
    assert_eq!(run(&with, &[tab]), "1|unset\n", "a tab launch naming none");
    assert_eq!(
        run(&twin(false), &[tab]),
        "unset|unset\n",
        "negative control: no block, no variable"
    );
    assert_eq!(run(&with, &[]), "unset|unset\n", "outside a tab");
    assert_eq!(
        run(&with, &[tab, ("ATERM_NO_HARNESS", "1")]),
        "unset|unset\n",
        "under --no-harness"
    );
    for (env, want) in [
        (("CLAUDE_CODE_NO_FLICKER", "0"), "0|unset\n"),
        (("CLAUDE_CODE_NO_FLICKER", ""), "|unset\n"),
        (("CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN", "1"), "unset|1\n"),
    ] {
        assert_eq!(run(&with, &[tab, env]), want, "{env:?}");
    }
    let settings = home.join(".claude/settings.json");
    std::fs::write(&settings, "{\n  \"theme\": \"auto\"\n}\n").unwrap();
    assert_eq!(run(&with, &[tab]), "1|unset\n", "settings naming no tui");
    std::fs::write(&settings, "{\"theme\":\"auto\",\"tui\" : \"default\"}").unwrap();
    assert_eq!(run(&with, &[tab]), "unset|unset\n", "the person's /tui");
    let config_str = config.to_string_lossy().into_owned();
    assert_eq!(
        run(&with, &[tab, ("CLAUDE_CONFIG_DIR", &config_str)]),
        "1|unset\n",
        "CLAUDE_CONFIG_DIR's settings, not HOME's, are the person's"
    );
    std::fs::write(config.join("settings.json"), "{\"tui\":\"fullscreen\"}").unwrap();
    assert_eq!(
        run(&with, &[tab, ("CLAUDE_CONFIG_DIR", &config_str)]),
        "unset|unset\n",
        "a tui under CLAUDE_CONFIG_DIR"
    );
}

/// THE TOKEN BLOCK (DESIGN-claude-setup-token S8, §5.4): right before the twin's last line, the
/// plain exec, which still names the build; absent for another program
/// and where atpkg names no file.
#[cfg(unix)]
#[test]
fn the_token_block_sits_before_the_last_line_of_a_claude_twin() {
    let body = crate::platform::sh_shim_content_twin(
        Path::new("/store/claude/7/bin/claude"),
        &crate::shim_env::ShimEnv::NONE,
        None,
        &sh_prelude("claude", Some(Path::new("/A/MacOS/atpkg"))),
    );
    let atpkg = Some(Path::new("/A/MacOS/atpkg"));
    let with = with_token_block("claude", body.clone(), atpkg);
    let lines: Vec<&str> = with.lines().collect();
    assert_eq!(
        lines.last().copied(),
        body.lines().last(),
        "the last line stays the plain exec"
    );
    assert_eq!(
        crate::platform::sh_exec_target(lines.last().unwrap().trim()),
        Some(PathBuf::from("/store/claude/7/bin/claude"))
    );
    let block = lines.len() - 5;
    assert!(
        lines[block].starts_with("# atpkg: aterm's token route"),
        "{with}"
    );
    assert_eq!(
        lines[block + 2],
        "  exec '/A/MacOS/aterm' harness accounts token-run --auto -- \
         '/store/claude/7/bin/claude' \"$@\"",
        "{with}"
    );
    assert_eq!(lines[block + 3], "fi");
    assert!(
        with.find("token-run") > with.find("harness tap --launch"),
        "after the tap's block"
    );
    assert_eq!(with_token_block("codex", body.clone(), atpkg), body);
    assert_eq!(with_token_block("claude", body.clone(), None), body);
    let not_plain = format!("{body}echo trailing\n");
    assert_eq!(
        with_token_block("claude", not_plain.clone(), atpkg),
        not_plain,
        "a last line that is not the plain exec"
    );
}

/// THE TWIN WITH THE TOKEN BLOCK, EXECUTED: inside a tab the launch runs as the aterm beside
/// atpkg's `harness accounts token-run --auto -- <build> <args…>`, after the usage tap's block;
/// outside a tab, under `--no-harness` and with that aterm missing, it runs exactly as typed.
#[cfg(unix)]
#[test]
fn the_twin_hands_a_tab_launch_to_the_token_wrapper_and_otherwise_runs_it_as_typed() {
    let laid = Laid::new();
    let tab = [("ATERM_PARENT_SESSION_ID", "s-60776b6e2ed1b73bd013")];
    // The stand-in aterm prints no tap value, and prints the wrapper's words.
    laid.aterm("case \"$2\" in tap) exit 0;; esac\nprintf '[%s]' \"$@\"\nprintf '\\n'");
    let typed = "<--model><opus>\n";
    assert_eq!(
        laid.run_twin(&["--model", "opus"], &tab, true),
        format!(
            "[harness][accounts][token-run][--auto][--][{}][--model][opus]\n",
            laid.root.join("store-build").display()
        )
    );
    assert_eq!(
        laid.run_twin(&["--model", "opus"], &[], true),
        typed,
        "outside a tab"
    );
    assert_eq!(
        laid.run_twin(
            &["--model", "opus"],
            &[tab[0], ("ATERM_NO_HARNESS", "1")],
            true
        ),
        typed,
        "under --no-harness"
    );
    assert_eq!(
        laid.run_twin(&["--model", "opus"], &tab, false),
        typed,
        "the switch off: no block"
    );
    std::fs::remove_file(aterm_beside(&laid.atpkg).unwrap()).unwrap();
    assert_eq!(
        laid.run_twin(&["--model", "opus"], &tab, true),
        typed,
        "no aterm there"
    );
}

/// THE TWIN'S WORDS FOR A VALUE: `--settings`, then the value as one argument a Windows program
/// splits back to it, every character cmd.exe reads as its own behind a caret; `None` for a
/// value outside printable ASCII or past the bound.
#[test]
fn the_twins_words_quote_a_value_for_the_program_and_escape_it_for_cmd() {
    assert_eq!(
        cmd_words(r#"{"a":"b c"}"#).as_deref(),
        Some(r#"--settings ^"{\^"a\^":\^"b c\^"}^""#)
    );
    assert_eq!(
        cmd_words(r"C:\x\").as_deref(),
        Some(r#"--settings ^"C:\x\\^""#),
        "the backslashes ending it doubled, so the closing quote closes"
    );
    assert_eq!(
        cmd_words(r#"a\"b"#).as_deref(),
        Some(r#"--settings ^"a\\\^"b^""#)
    );
    assert_eq!(
        cmd_words("&|<>()%!^").as_deref(),
        Some(r#"--settings ^"^&^|^<^>^(^)^%^!^^^""#)
    );
    assert_eq!(cmd_words("caf\u{e9}"), None, "outside printable ASCII");
    assert_eq!(cmd_words("a\tb"), None, "a control character");
    assert_eq!(cmd_words("a\r\nb"), None, "a line break");
    assert_eq!(
        cmd_words(&"x".repeat(CMD_WORDS_MAX)),
        None,
        "past the bound"
    );
    assert!(cmd_words(&"x".repeat(CMD_WORDS_MAX - 20)).is_some());
    for value in [
        r#"{"statusLine":{"type":"command","command":"C:\\a b\\aterm.exe harness tap","refreshInterval":10}}"#,
        r"\\server\share\",
        r#"" \" \\" "#,
        "",
    ] {
        let words = cmd_words(value).unwrap();
        let rest = words.strip_prefix("--settings ").unwrap();
        assert_eq!(windows_split(&cmd_uncaret(rest)), vec![value], "{value:?}");
        let mut escaped = false;
        for c in rest.chars() {
            assert!(
                escaped || c == '^' || !CMD_SPECIAL.contains(c),
                "every special behind a caret: {rest}"
            );
            escaped = !escaped && c == '^';
        }
    }
}

/// What cmd.exe's parse leaves of `text` outside any quote: each caret taken off, the character
/// after it kept as it is.
fn cmd_uncaret(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '^' {
            out.extend(chars.next());
        } else {
            out.push(c);
        }
    }
    out
}

/// A Windows program's split of a command line into arguments (the CRT's rules since 2008).
fn windows_split(line: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut chars = line.chars().peekable();
    loop {
        while chars.peek() == Some(&' ') {
            chars.next();
        }
        if chars.peek().is_none() {
            return args;
        }
        let mut arg = String::new();
        let mut quoted = false;
        loop {
            let mut backslashes = 0;
            while chars.peek() == Some(&'\\') {
                chars.next();
                backslashes += 1;
            }
            match chars.peek() {
                Some('"') => {
                    arg.extend(std::iter::repeat_n('\\', backslashes / 2));
                    chars.next();
                    if backslashes % 2 == 1 {
                        arg.push('"');
                    } else if quoted && chars.peek() == Some(&'"') {
                        chars.next();
                        arg.push('"');
                    } else {
                        quoted = !quoted;
                    }
                }
                Some(' ') if !quoted => {
                    arg.extend(std::iter::repeat_n('\\', backslashes));
                    break;
                }
                None => {
                    arg.extend(std::iter::repeat_n('\\', backslashes));
                    break;
                }
                Some(&c) => {
                    arg.extend(std::iter::repeat_n('\\', backslashes));
                    arg.push(c);
                    chars.next();
                }
            }
        }
        args.push(arg);
    }
}

/// A plain `.cmd` twin of Claude Code for the block's tests.
fn plain_cmd_twin(target: &Path) -> (String, crate::shim_env::ShimEnv) {
    let env = crate::shim_env::ShimEnv::admit(&["DISABLE_AUTOUPDATER=1".to_string()]).unwrap();
    (
        crate::platform::cmd_shim_content_twin(target, &env, ""),
        env,
    )
}

/// THE `.cmd` BLOCK'S TEXT: after the frame and ahead of the twin's `@set` lines, the ask; the
/// tapped forward right before the plain forward, which stays the last line, so the twin still
/// names its build and its environment to every reader; every line silent and ASCII; no line
/// that expands the launch's arguments also carries a `for` variable or a `call`; the aterm
/// named once to run and once to test, and read back as the atpkg beside it. Another program, an
/// aterm no batch line can quote, and a body that is not the plain twin come back as they were.
#[test]
fn the_cmd_block_wraps_the_plain_twin_and_keeps_what_atpkg_reads_of_it() {
    let target = Path::new(r"C:\Program Files (x86)\aterm\pkg\store\claude\7\bin\claude.exe");
    let aterm = Path::new(r"C:\Users\A&B (x)\AppData\Local\Programs\aterm\aterm.exe");
    let (plain, env) = plain_cmd_twin(target);
    let tapped = cmd_block_for("claude", plain.clone(), aterm);
    assert_ne!(tapped, plain);
    let frame = crate::platform::cmd_framed("");
    let after = tapped.strip_prefix(frame.as_str()).expect("framed");
    assert!(after.starts_with(CMD_NOTE), "{after}");
    let lines: Vec<&str> = after.trim_end_matches("\r\n").split("\r\n").collect();
    let plain_last = plain
        .trim_end_matches("\r\n")
        .rsplit("\r\n")
        .next()
        .unwrap();
    assert_eq!(
        *lines.last().unwrap(),
        plain_last,
        "the plain forward stays last"
    );
    assert_eq!(
        lines[lines.len() - 2],
        format!(":{CMD_LABEL_PLAIN}"),
        "reached only by its label"
    );
    assert_eq!(
        lines[lines.len() - 3],
        "@call exit /b %%errorlevel%%",
        "the tapped forward's quote left open still ends the batch with the build's code"
    );
    assert_eq!(
        lines[lines.len() - 4],
        format!(
            "@set \"__atpkg_tap=\" & \"{}\" %__atpkg_tap%{}",
            target.display(),
            crate::platform::CMD_FORWARD_TAIL
        )
    );
    assert_eq!(
        crate::platform::parse_cmd_shim_target(&tapped),
        Some(target.to_path_buf())
    );
    assert_eq!(crate::platform::parse_cmd_shim_env(&tapped), env);
    assert_eq!(
        cmd_named_atpkg(&tapped),
        Some(std::path::PathBuf::from(
            r"C:\Users\A&B (x)\AppData\Local\Programs\aterm\atpkg.exe"
        ))
    );
    assert_eq!(cmd_named_atpkg(&plain), None);
    assert!(tapped.is_ascii(), "{tapped}");
    assert_eq!(
        tapped.matches(&format!("\"{}\"", aterm.display())).count(),
        2,
        "{tapped}"
    );
    let ask = lines
        .iter()
        .position(|l| l.contains(" harness tap --launch --into "))
        .expect("the ask");
    assert_eq!(
        lines[ask],
        format!(
            "@\"{}\" <nul 2>nul harness tap --launch --into \"%__atpkg_tap_d%\" -- %* && goto \
             {CMD_LABEL_READ} || goto {CMD_LABEL_DROP}",
            aterm.display()
        )
    );
    assert_eq!(lines[ask + 1], format!(":{CMD_LABEL_READ}"));
    assert_eq!(
        lines[ask + 2..ask + 4],
        [
            format!("@if errorlevel 1 goto {CMD_LABEL_DROP}"),
            format!("@if not errorlevel 0 goto {CMD_LABEL_DROP}"),
        ],
        "the read asks aterm's exit itself, which a quote left open can take from the ask's line"
    );
    assert!(
        lines
            .iter()
            .position(|l| l.starts_with("@set \"DISABLE_AUTOUPDATER="))
            > Some(ask),
        "the twin's own environment is set past the ask, for the build alone"
    );
    assert!(lines[..ask].contains(&"@setlocal DisableDelayedExpansion"));
    for line in &lines {
        assert!(
            line.starts_with('@') || line.starts_with(':'),
            "silent: {line}"
        );
        assert!(
            !line.ends_with('(') && !line.starts_with(')'),
            "no block: {line}"
        );
        if let Some(args) = line.find("%*") {
            assert!(
                !line.contains("%%L") && !line[..args].contains("call "),
                "the arguments are expanded once: {line}"
            );
            assert!(
                line.ends_with(crate::platform::CMD_FORWARD_TAIL)
                    || line.ends_with(&format!(
                        " -- %* && goto {CMD_LABEL_READ} || goto {CMD_LABEL_DROP}"
                    )),
                "{line}"
            );
        }
        if line.starts_with("@set \"") {
            assert!(
                line.starts_with("@set \"DISABLE_AUTOUPDATER=") || line.contains(" & \""),
                "no line of the block reads as the twin's environment: {line}"
            );
        }
    }
    assert_eq!(cmd_block_for("codex", plain.clone(), aterm), plain);
    assert_eq!(
        cmd_block_for("claude", plain.clone(), Path::new(r"C:\100%\aterm.exe")),
        plain,
        "an aterm a batch line cannot quote"
    );
    assert_eq!(
        cmd_block_for("claude", tapped.clone(), aterm),
        tapped,
        "a body carrying the block already"
    );
    let legacy = format!("{frame}@\"{}\" %* & @exit /b\r\n", target.display());
    assert_eq!(cmd_block_for("claude", legacy.clone(), aterm), legacy);
    assert_eq!(
        cmd_block_for("claude", "not a shim".to_owned(), aterm),
        "not a shim"
    );
    if cfg!(not(windows)) {
        assert_eq!(
            with_cmd_block("claude", plain.clone(), Some(Path::new("/A/MacOS/atpkg"))),
            plain,
            "a sh host's twin carries no `.cmd` block"
        );
    }
}

/// A scratch layout for the executed `.cmd` twins: Claude Code's build in a store and the aterm
/// beside an atpkg, each the argv-recording stand-in (`tests/argv_fixture.rs`) copied under its
/// name, the folders holding them and the `TEMP` the twin is given named with what cmd.exe reads
/// as its own.
#[cfg(windows)]
struct CmdLaid {
    root: crate::testdir::Dir,
    app: std::path::PathBuf,
    build: std::path::PathBuf,
}

/// What one run of a `.cmd` twin did.
#[cfg(windows)]
#[derive(Debug)]
struct CmdRun {
    code: Option<i32>,
    stdout: String,
    claude: Option<Vec<String>>,
    claude_env: Option<String>,
    aterm: Option<Vec<String>>,
    into: Option<String>,
    temp_left: Vec<std::path::PathBuf>,
}

#[cfg(windows)]
impl CmdLaid {
    fn new() -> Self {
        let root = crate::testdir::dir("atpkg-cmd-tap-");
        let fixture = aterm_tempfile::bin_exe("atpkg-status-fixture");
        let app = root.join("app & (x86) ^ co");
        let build = root.join(r"store\claude\7\bin\claude.exe");
        std::fs::create_dir_all(&app).unwrap();
        std::fs::create_dir_all(build.parent().unwrap()).unwrap();
        std::fs::create_dir_all(root.join("record")).unwrap();
        std::fs::create_dir_all(root.join("agents")).unwrap();
        std::fs::create_dir_all(root.join("tmp & (x) ^ t")).unwrap();
        std::fs::copy(&fixture, app.join("aterm.exe")).unwrap();
        std::fs::write(app.join("atpkg.exe"), b"").unwrap();
        std::fs::copy(&fixture, &build).unwrap();
        Self { root, app, build }
    }

    fn atpkg(&self) -> std::path::PathBuf {
        self.app.join("atpkg.exe")
    }

    fn temp(&self) -> std::path::PathBuf {
        self.root.join("tmp & (x) ^ t")
    }

    /// Lay Claude Code's twin as atpkg lays it on Windows (the plain framed twin, with the block
    /// where `tapped`), returning its path.
    fn twin(&self, tapped: bool) -> std::path::PathBuf {
        let (plain, _) = plain_cmd_twin(&self.build);
        let body = if tapped {
            with_cmd_block("claude", plain, Some(&self.atpkg()))
        } else {
            plain
        };
        let twin = self.root.join(r"agents\claude.cmd");
        std::fs::write(&twin, body).unwrap();
        twin
    }

    /// Run `twin` with `args` under `env`, as a caller's `CreateProcess` runs a batch file
    /// (`std::process::Command`: `cmd.exe /e:ON /v:OFF /d /c`).
    fn run(&self, twin: &Path, args: &[&str], env: &[(&str, &str)]) -> CmdRun {
        let mut command = aterm_uds::spawnfd::command(twin);
        command.args(args);
        self.prepare(&mut command, env);
        self.ran(command)
    }

    /// Every variable the block or the stand-ins read removed, `TEMP` this layout's, the build's
    /// exit code 37, then `env`.
    fn prepare(&self, command: &mut aterm_uds::spawnfd::Command, env: &[(&str, &str)]) {
        for name in [
            "ATERM_PARENT_SESSION_ID",
            "ATERM_NO_HARNESS",
            "__atpkg_tap",
            "__atpkg_tap_d",
            "__atpkg_tap_n",
            "RANDOM",
            "ATPKG_ARGV_FIXTURE_WORDS",
            "ATPKG_ARGV_FIXTURE_EXIT_ATERM",
            "DISABLE_AUTOUPDATER",
            "ATERM_LAUNCH_STDOUT_TTY",
        ] {
            command.env_remove(name);
        }
        command
            .env("TEMP", self.temp())
            .env("ATPKG_ARGV_FIXTURE", self.root.join("record"))
            .env("ATPKG_ARGV_FIXTURE_EXIT_CLAUDE", "37")
            .stdin(std::process::Stdio::null());
        for (key, value) in env {
            command.env(key, value);
        }
    }

    fn ran(&self, mut command: aterm_uds::spawnfd::Command) -> CmdRun {
        let record = self.root.join("record");
        for entry in std::fs::read_dir(&record).unwrap() {
            std::fs::remove_file(entry.unwrap().path()).unwrap();
        }
        let out = command.output().unwrap();
        let argv = |stem: &str| {
            std::fs::read_to_string(record.join(format!("{stem}.argv")))
                .ok()
                .map(|text| {
                    text.strip_suffix('\0')
                        .map(|text| text.split('\0').map(str::to_owned).collect())
                        .unwrap_or_default()
                })
        };
        CmdRun {
            code: out.status.code(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            claude: argv("claude"),
            claude_env: std::fs::read_to_string(record.join("claude.env")).ok(),
            aterm: argv("aterm"),
            into: std::fs::read_to_string(record.join("aterm.into")).ok(),
            temp_left: std::fs::read_dir(self.temp())
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect(),
        }
    }
}

/// The arguments every executed `.cmd` run passes: each character cmd.exe or a program's split
/// reads as its own, alone and inside words, a variable's name in each spelling, an empty
/// argument, and characters outside ASCII.
#[cfg(windows)]
const HOSTILE: [&str; 30] = [
    "plain",
    "two words",
    "quo\"te",
    "\"",
    "\"\"",
    "100%",
    "%PATH%",
    "%ZZ_ATPKG_UNSET%",
    "%__atpkg_tap%",
    "car^et",
    "^",
    "a&b",
    "& echo INJECTED",
    "a|b",
    "| echo INJECTED",
    "<in",
    "out>",
    ">> out",
    "bang!",
    "!PATH!",
    "unicode \u{e9}\u{4e2d}\u{1f600}",
    "",
    "trailing\\",
    "back\\\"slash",
    "(paren)",
    "%~1",
    "%T",
    "%%L",
    "&& echo INJECTED",
    "-p",
];

/// The `--settings` value the executed runs carry: every character cmd.exe reads as its own.
#[cfg(windows)]
const VALUE: &str = r#"{"statusLine":{"type":"command","command":"C:\\a & b\\aterm.exe harness tap ^ % ! ( ) <x> | \"q\" %PATH% !PATH!","refreshInterval":10}}"#;

/// THE `.cmd` TWIN TAPS A CLAUDE CODE STARTED IN AN ATERM TAB, EXECUTED under cmd.exe with
/// stand-ins that record their arguments: inside a tab the aterm beside atpkg is asked with
/// `harness tap --launch --into <folder> --` and exactly the launch's arguments, its standard
/// output the launch's, and the words it wrote ride as `--settings <value>` ahead of exactly
/// those arguments; the build keeps its exit code, holds the twin's environment and none of the
/// block's variables, and the block's folder is gone. Outside a tab, under `--no-harness`, where
/// aterm wrote nothing, failed, or is not there, and where no folder can be made, the launch runs
/// exactly as typed, with its exit code. NEGATIVE CONTROL: the plain twin in the same tab asks
/// nothing and carries no value.
#[cfg(windows)]
#[test]
fn the_cmd_twin_taps_a_launch_in_an_aterm_tab_and_otherwise_runs_it_as_typed() {
    let laid = CmdLaid::new();
    let words = cmd_words(VALUE).unwrap();
    let typed: Vec<String> = HOSTILE.iter().map(|a| (*a).to_owned()).collect();
    let tab = ("ATERM_PARENT_SESSION_ID", "s-60776b6e2ed1b73bd013");
    let with_words = ("ATPKG_ARGV_FIXTURE_WORDS", words.as_str());
    let twin = laid.twin(true);

    let run = laid.run(&twin, &HOSTILE, &[tab, with_words]);
    let mut tapped = vec!["--settings".to_owned(), VALUE.to_owned()];
    tapped.extend(typed.iter().cloned());
    assert_eq!(run.claude.as_ref(), Some(&tapped), "{run:#?}");
    let asked = run.aterm.clone().expect("aterm asked");
    assert_eq!(
        asked[..4],
        ["harness", "tap", "--launch", "--into"],
        "{asked:?}"
    );
    let folder = Path::new(&asked[4]);
    assert_eq!(folder.parent(), Some(laid.temp().as_path()), "{asked:?}");
    assert!(
        folder
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("atpkg-tap-"),
        "{asked:?}"
    );
    assert_eq!(asked[5], "--");
    assert_eq!(
        asked[6..],
        typed[..],
        "aterm asked with exactly the launch's arguments"
    );
    assert_eq!(
        run.into.as_deref(),
        Some("1"),
        "the folder stood when aterm ran"
    );
    assert_eq!(run.code, Some(37), "the build's exit code");
    assert_eq!(
        run.stdout, "aterm ran\nclaude ran\n",
        "aterm's output is the launch's"
    );
    assert_eq!(
        run.claude_env.as_deref(),
        Some("DISABLE_AUTOUPDATER=1\0"),
        "the twin's environment and none of the block's: {run:#?}"
    );
    assert_eq!(
        run.temp_left,
        Vec::<std::path::PathBuf>::new(),
        "the folder is gone"
    );

    let as_typed = |run: &CmdRun, why: &str| {
        assert_eq!(run.claude.as_ref(), Some(&typed), "{why}: {run:#?}");
        assert_eq!(run.code, Some(37), "{why}: {run:#?}");
        assert!(!run.stdout.contains("INJECTED"), "{why}: {run:#?}");
        assert_eq!(run.temp_left, Vec::<std::path::PathBuf>::new(), "{why}");
        assert_eq!(
            run.claude_env.as_deref(),
            Some("DISABLE_AUTOUPDATER=1\0"),
            "{why}"
        );
    };
    let run = laid.run(&twin, &HOSTILE, &[with_words]);
    as_typed(&run, "outside a tab");
    assert_eq!(run.aterm, None, "aterm is not asked outside a tab");
    let run = laid.run(
        &twin,
        &HOSTILE,
        &[("ATERM_PARENT_SESSION_ID", ""), with_words],
    );
    as_typed(&run, "an empty session id");
    assert_eq!(run.aterm, None);
    let run = laid.run(
        &twin,
        &HOSTILE,
        &[tab, ("ATERM_NO_HARNESS", "1"), with_words],
    );
    as_typed(&run, "under --no-harness");
    assert_eq!(run.aterm, None, "aterm is not asked under --no-harness");
    let run = laid.run(
        &twin,
        &HOSTILE,
        &[tab, ("ATERM_NO_HARNESS", ""), with_words],
    );
    assert_eq!(
        run.claude.as_ref(),
        Some(&tapped),
        "an empty ATERM_NO_HARNESS is none: {run:#?}"
    );
    let run = laid.run(&twin, &HOSTILE, &[tab]);
    as_typed(&run, "nothing written");
    assert!(run.aterm.is_some());
    let run = laid.run(
        &twin,
        &HOSTILE,
        &[tab, with_words, ("ATPKG_ARGV_FIXTURE_EXIT_ATERM", "1")],
    );
    as_typed(&run, "aterm failed having written words");
    assert!(run.aterm.is_some());
    let run = laid.run(&twin, &HOSTILE, &[tab, ("__atpkg_tap", "--evil")]);
    as_typed(
        &run,
        "a variable of the block's name in the caller's environment",
    );
    let mut command = aterm_uds::spawnfd::command(&twin);
    command.args(HOSTILE);
    laid.prepare(&mut command, &[tab, with_words]);
    command.env_remove("TEMP");
    let run = laid.ran(command);
    as_typed(&run, "no TEMP, no folder");
    assert_eq!(
        run.into.as_deref(),
        Some("0"),
        "asked with no folder: {run:#?}"
    );

    let plain = laid.twin(false);
    let run = laid.run(&plain, &HOSTILE, &[tab, with_words]);
    as_typed(&run, "negative control: the plain twin");
    assert_eq!(run.aterm, None);

    let twin = laid.twin(true);
    std::fs::remove_file(laid.app.join("aterm.exe")).unwrap();
    let run = laid.run(&twin, &HOSTILE, &[tab, with_words]);
    as_typed(&run, "no aterm there");
    assert_eq!(run.aterm, None);
}

/// THE BLOCK TURNS DELAYED EXPANSION OFF FOR THE BATCH, EXECUTED: a twin cmd.exe runs with
/// delayed expansion on (`/v:on`, or the console's registry default) still hands the build the
/// value aterm wrote, `!`-spelled variables and carets included, exactly. (The launch's own
/// arguments are the caller's: a caller under `/v:on` expands `!` in them before the twin reads
/// a byte.)
#[cfg(windows)]
#[test]
fn the_cmd_twin_hands_the_value_on_exactly_under_delayed_expansion() {
    let laid = CmdLaid::new();
    let words = cmd_words(VALUE).unwrap();
    let twin = laid.twin(true);
    let mut command = aterm_uds::spawnfd::command("cmd.exe");
    command.raw_arg(format!(
        "/d /v:on /c \"\"{}\" plain \"two words\"\"",
        twin.display()
    ));
    laid.prepare(
        &mut command,
        &[
            ("ATERM_PARENT_SESSION_ID", "s-60776b6e2ed1b73bd013"),
            ("ATPKG_ARGV_FIXTURE_WORDS", words.as_str()),
        ],
    );
    let run = laid.ran(command);
    assert_eq!(
        run.claude,
        Some(vec![
            "--settings".to_owned(),
            VALUE.to_owned(),
            "plain".to_owned(),
            "two words".to_owned()
        ]),
        "{run:#?}"
    );
    assert_eq!(run.code, Some(37));
    assert_eq!(run.temp_left, Vec::<std::path::PathBuf>::new());
}

/// TWO LAUNCHES THAT PICK ONE NAME EACH GET A FOLDER OF THEIR OWN, EXECUTED: cmd.exe seeds
/// `%RANDOM%` from the clock's second, so two launches in one second pick the same values; here
/// `RANDOM` is a variable the caller set (cmd.exe reads it in place of its own), so every try
/// picks `7`, and another launch's folder holds the first try's name. The launch passes over
/// that name for its second try, carries the words its own folder held, and leaves the other
/// launch's folder and file as they were. NEGATIVE CONTROL (run as a temporary mutant,
/// reverted): with no second try, the taken name leaves this launch no folder and no words.
#[cfg(windows)]
#[test]
fn a_cmd_launch_whose_folder_name_is_taken_tries_another() {
    let laid = CmdLaid::new();
    let words = cmd_words(VALUE).unwrap();
    let twin = laid.twin(true);
    let taken = laid.temp().join("atpkg-tap-7-7-1");
    std::fs::create_dir(&taken).unwrap();
    std::fs::write(taken.join("theirs"), b"another launch's").unwrap();
    let run = laid.run(
        &twin,
        &["plain"],
        &[
            ("ATERM_PARENT_SESSION_ID", "s-60776b6e2ed1b73bd013"),
            ("ATPKG_ARGV_FIXTURE_WORDS", words.as_str()),
            ("RANDOM", "7"),
        ],
    );
    assert_eq!(
        run.claude,
        Some(vec![
            "--settings".to_owned(),
            VALUE.to_owned(),
            "plain".to_owned()
        ]),
        "{run:#?}"
    );
    let asked = run.aterm.clone().expect("aterm asked");
    assert_eq!(
        Path::new(&asked[4]),
        laid.temp().join("atpkg-tap-7-7-2"),
        "{asked:?}"
    );
    assert_eq!(run.code, Some(37));
    assert_eq!(
        run.claude_env.as_deref(),
        Some("DISABLE_AUTOUPDATER=1\0"),
        "none of the block's variables, its count included: {run:#?}"
    );
    assert_eq!(
        run.temp_left,
        vec![taken.clone()],
        "this launch's folder is gone"
    );
    assert_eq!(
        std::fs::read_dir(&taken)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>(),
        ["theirs"],
        "the other launch's folder holds what it held"
    );
    assert_eq!(
        std::fs::read(taken.join("theirs")).unwrap(),
        b"another launch's"
    );
}

/// A LAUNCH WHOSE ARGUMENTS LEAVE A QUOTE OPEN CARRIES WORDS ONLY WHERE ATERM EXITED 0,
/// EXECUTED: a caller's raw command line `"<twin>" open"quote` leaves cmd.exe in a quote at the
/// ask's `&& goto … || goto …`, which become aterm's arguments, and the ask's line goes on to the
/// read whatever aterm answered. aterm exiting 1 having written words: the build carries none.
/// CONTROL: aterm exiting 0 there: the build carries them. NEGATIVE CONTROL (run as a temporary
/// mutant, reverted): with no look at aterm's exit at the read, the build carries the words of an
/// aterm that exited 1.
#[cfg(windows)]
#[test]
fn a_cmd_launch_with_a_quote_left_open_reads_words_only_of_an_aterm_that_exited_0() {
    let laid = CmdLaid::new();
    let words = cmd_words(VALUE).unwrap();
    let twin = laid.twin(true);
    let run = |aterm_exit: &str| {
        let mut command = aterm_uds::spawnfd::command("cmd.exe");
        command.raw_arg(format!("/d /c \"\"{}\" open\"quote\"", twin.display()));
        laid.prepare(
            &mut command,
            &[
                ("ATERM_PARENT_SESSION_ID", "s-60776b6e2ed1b73bd013"),
                ("ATPKG_ARGV_FIXTURE_WORDS", words.as_str()),
                ("ATPKG_ARGV_FIXTURE_EXIT_ATERM", aterm_exit),
            ],
        );
        laid.ran(command)
    };
    let failed = run("1");
    let asked = failed.aterm.clone().expect("aterm asked");
    assert!(
        asked.iter().any(|arg| arg.contains("goto")),
        "PRECONDITION: the open quote took the ask's line's ends into aterm's arguments: \
         {asked:?}"
    );
    let claude = failed.claude.clone().expect("the build ran");
    assert_ne!(
        claude.first().map(String::as_str),
        Some("--settings"),
        "an aterm that exited 1 hands on no words: {failed:#?}"
    );
    assert_eq!(failed.temp_left, Vec::<std::path::PathBuf>::new());

    let control = run("0");
    let claude = control.claude.clone().expect("the build ran");
    assert_eq!(
        claude[..2],
        ["--settings".to_owned(), VALUE.to_owned()],
        "CONTROL: an aterm that exited 0 hands on its words: {control:#?}"
    );
    assert_eq!(control.temp_left, Vec::<std::path::PathBuf>::new());
}
