// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE PERSON'S `shell_args` UNDER INTEGRATION.
//!
//! bash and PowerShell are integrated through their argv (`bash --rcfile …`,
//! `pwsh … -NoExit -Command …`), and an argv override used to REPLACE the whole
//! argv, so `shell_args = ["-o", "vi"]` silently did nothing for bash — the
//! default shell on Linux — while integration was on, which it is by default.
//! [`merge_shell_args`] puts the person's arguments into the integration's argv
//! where the shell accepts them, or, for arguments no integrated start can honour,
//! says why integration stands aside so the caller runs them as given.

use crate::{BASH_LOADER_FILE, InjectionEnv, ShellType};

/// What becomes of `shell_args` for an integrated shell ([`merge_shell_args`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArgsMerge {
    /// The integration stays; its argv (and, for a login bash, its env) now
    /// carries the person's arguments.
    Integrated,
    /// The arguments ask for a start no integration can stand in for — a command
    /// or a script instead of an interactive shell, POSIX mode, a restricted
    /// shell. The caller runs the shell with them as given, WITHOUT integration,
    /// and says this sentence (it completes "shell integration is off: …").
    Bypass(String),
}

/// Merge the person's `shell_args` into `injection`, the integration [`crate::prepare`]
/// built for `shell`.
///
/// **bash:** long options go before the loader's `--rcfile` and short ones after
/// it, the only order bash accepts (`bash -i --posix` is "invalid option"). On
/// every Unix but macOS, a login shell (`-l`, `--login`) or `--norc` is started the
/// way kitty and Ghostty start it — `--posix` with `ENV` naming the loader, which
/// then reads exactly what bash would have (see the loader's own comment,
/// `BASH_LOADER`) — and a `--rcfile`/`--init-file` file is read in place of
/// `~/.bashrc`. On macOS and under Git Bash the loader already does a login
/// shell's reading, so `-l`/`--login` is left to it, and `--norc`, `--noprofile`
/// and `--rcfile` stand integration aside.
///
/// **PowerShell:** the arguments go before `-ExecutionPolicy … -NoExit -Command …`
/// (pwsh's `-Login` must be first), except a `-NoExit` of the person's own, which
/// the integration already passes.
///
/// Every other shell is integrated without touching argv (zsh, fish, cmd: the
/// spawn hands them `shell_args` itself; WSL: the Windows seam splices them in as
/// `wsl.exe` options), and so is any shell when `args` is empty: [`ArgsMerge::Integrated`],
/// `injection` untouched.
pub fn merge_shell_args(
    shell: ShellType,
    injection: &mut InjectionEnv,
    args: &[String],
) -> ArgsMerge {
    merge_shell_args_in(shell, injection, args, &|key| std::env::var(key).ok())
}

/// Why `args` would stand integration aside for `shell` ([`ArgsMerge::Bypass`]),
/// decided without preparing anything: a config check's answer. `None` when the
/// shell is integrated with them.
#[must_use]
pub fn shell_args_bypass(shell: ShellType, args: &[String]) -> Option<String> {
    let mut probe = crate::injection_for(shell, std::path::Path::new("/"))?;
    match merge_shell_args_in(shell, &mut probe, args, &|_| None) {
        ArgsMerge::Integrated => None,
        ArgsMerge::Bypass(why) => Some(why),
    }
}

/// [`merge_shell_args`] over the environment it reads (`ENV`, `HISTFILE`, `HOME`,
/// for a bash started in POSIX mode).
pub(crate) fn merge_shell_args_in(
    shell: ShellType,
    injection: &mut InjectionEnv,
    args: &[String],
    env: &dyn Fn(&str) -> Option<String>,
) -> ArgsMerge {
    if args.is_empty() {
        return ArgsMerge::Integrated;
    }
    match shell {
        ShellType::Bash => merge_bash(injection, args, env),
        ShellType::PowerShell => merge_powershell(injection, args),
        _ => ArgsMerge::Integrated,
    }
}

/// Whether this platform's bash loader reads a login shell's files itself
/// (`bash/rcfile`, macOS and Git Bash) rather than starting bash as bash starts
/// (`bash/bashrc`).
const LOGIN_STYLE_LOADER: bool = matches!(BASH_LOADER_FILE.as_bytes(), b"rcfile");

/// One argument of a bash argv, after the long options.
#[derive(Debug)]
struct Short {
    text: String,
    /// The value of a `-o`/`-O` before it (`vi` in `-o vi`), never an option.
    value: bool,
}

/// bash's arguments, sorted as bash parses them.
#[derive(Debug, Default)]
struct BashArgs {
    /// Long options kept as they are (`--login`, `--norc`, `--verbose`, …).
    longs: Vec<String>,
    /// Short-option clusters and their values, in order.
    shorts: Vec<Short>,
    login: bool,
    norc: bool,
    noprofile: bool,
    /// The file a `--rcfile`/`--init-file` named (taken out of `longs`).
    rcfile: Option<String>,
}

/// bash's long options (`shell.c`'s `long_args`), which it also takes with one
/// dash while only long options came before (`-login`, `-posix`, `-rcfile f`).
const BASH_LONG_OPTIONS: [&str; 18] = [
    "debug",
    "debugger",
    "dump-po-strings",
    "dump-strings",
    "help",
    "init-file",
    "login",
    "noediting",
    "noprofile",
    "norc",
    "posix",
    "pretty-print",
    "protected",
    "rcfile",
    "restricted",
    "verbose",
    "version",
    "wordexp",
];

/// Sort `args` as bash parses them, or say why no integrated interactive bash can
/// run them. A long option is kept as `--name`, however it was spelled.
fn parse_bash_args(args: &[String]) -> Result<BashArgs, String> {
    let mut out = BashArgs::default();
    // Only long options so far: bash reads `-name` as `--name` here.
    let mut leading = true;
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        i += 1;
        if arg == "-" || arg == "--" {
            return Err(format!(
                "`{arg}` in shell_args ends bash's options, so what follows is a script"
            ));
        }
        let long = arg.strip_prefix("--").or_else(|| {
            arg.strip_prefix('-')
                .filter(|name| leading && BASH_LONG_OPTIONS.contains(name))
        });
        if let Some(long) = long {
            match long {
                "login" => out.login = true,
                "norc" => out.norc = true,
                "noprofile" => out.noprofile = true,
                "rcfile" | "init-file" => {
                    let Some(file) = args.get(i) else {
                        return Err(format!("`{arg}` in shell_args names no file"));
                    };
                    i += 1;
                    out.rcfile = Some(file.clone());
                    continue;
                }
                "posix" => {
                    return Err(format!(
                        "`{arg}` in shell_args asks for a POSIX-mode bash, which reads only $ENV"
                    ));
                }
                "restricted" => {
                    return Err(format!("`{arg}` in shell_args asks for a restricted bash"));
                }
                "debug" | "debugger" | "noediting" | "protected" | "verbose" => {}
                _ => {
                    return Err(format!(
                        "`{arg}` in shell_args does not start an interactive bash"
                    ));
                }
            }
            out.longs.push(format!("--{long}"));
            continue;
        }
        leading = false;
        let Some(flags) = arg.strip_prefix('-').or_else(|| arg.strip_prefix('+')) else {
            return Err(format!(
                "`{arg}` in shell_args names a script for bash to run, not an interactive shell"
            ));
        };
        // The `-o`/`-O` in the cluster, in order: each takes the next argument.
        let mut values = Vec::new();
        for flag in flags.chars() {
            match flag {
                'c' => {
                    return Err(
                        "`-c` in shell_args runs a command, not an interactive shell".to_string(),
                    );
                }
                's' => {
                    return Err(
                        "`-s` in shell_args reads commands with positional arguments".to_string(),
                    );
                }
                'r' => return Err("`-r` in shell_args asks for a restricted bash".to_string()),
                'D' => {
                    return Err("`-D` in shell_args does not start an interactive bash".to_string());
                }
                'l' if arg.starts_with('-') => out.login = true,
                'o' | 'O' => values.push(flag),
                _ => {}
            }
        }
        out.shorts.push(Short {
            text: arg.clone(),
            value: false,
        });
        for flag in values {
            let Some(value) = args.get(i) else {
                return Err(format!("`{arg}` in shell_args names no option"));
            };
            i += 1;
            if flag == 'o' && arg.starts_with('-') && value == "posix" {
                return Err(
                    "`-o posix` in shell_args asks for a POSIX-mode bash, which reads only $ENV"
                        .to_string(),
                );
            }
            out.shorts.push(Short {
                text: value.clone(),
                value: true,
            });
        }
    }
    Ok(out)
}

/// [`merge_shell_args`] for bash: `injection.argv_override` is `[bash, --rcfile,
/// <loader>]` ([`crate::prepare`]).
fn merge_bash(
    injection: &mut InjectionEnv,
    args: &[String],
    env: &dyn Fn(&str) -> Option<String>,
) -> ArgsMerge {
    let Some([argv0, _, loader]) = injection.argv_override.as_deref() else {
        return ArgsMerge::Integrated;
    };
    let (argv0, loader) = (argv0.clone(), loader.clone());
    let mut parsed = match parse_bash_args(args) {
        Ok(parsed) => parsed,
        Err(why) => return ArgsMerge::Bypass(why),
    };
    let mut argv = vec![argv0];
    if LOGIN_STYLE_LOADER {
        if parsed.norc || parsed.noprofile || parsed.rcfile.is_some() {
            return ArgsMerge::Bypass(
                "`--norc`, `--noprofile` or `--rcfile` in shell_args chooses bash's startup \
                 files, which aterm's loader reads itself on this system"
                    .to_string(),
            );
        }
        // The loader reads the login files itself, and a login bash would ignore
        // the `--rcfile` that runs it.
        parsed.longs.retain(|long| long != "--login");
        parsed.shorts.retain_mut(|short| {
            if short.value || !short.text.starts_with('-') {
                return true;
            }
            short.text.retain(|c| c != 'l');
            short.text != "-"
        });
        argv.extend(parsed.longs);
        argv.extend(["--rcfile".to_string(), loader]);
    } else if parsed.login || parsed.norc {
        // No `--rcfile` can run under these: a login bash ignores it, and nothing
        // keeps a non-login bash from its system bashrc. In POSIX mode bash reads
        // only `$ENV`, the loader, which reads what bash would have.
        argv.extend(parsed.longs);
        argv.push("--posix".to_string());
        posix_start_env(&mut injection.env_add, parsed.noprofile, env);
    } else {
        argv.extend(parsed.longs);
        argv.extend(["--rcfile".to_string(), loader]);
        if let Some(file) = parsed.rcfile {
            injection
                .env_add
                .push(("ATERM_BASH_RCFILE".to_string(), file));
        }
    }
    argv.extend(parsed.shorts.into_iter().map(|short| short.text));
    injection.argv_override = Some(argv);
    ArgsMerge::Integrated
}

/// The env of a bash started `--posix` with `$ENV` naming the loader, which reads
/// each of these and unsets it: `ENV` itself (spelled through
/// `$ATERM_SHELL_INTEGRATION_DIR`, because bash expands `$ENV`'s value and a path
/// is not to be expanded twice), the caller's own `ENV` to put back, and — when
/// aterm's environment names no `HISTFILE` — `~/.bash_history`, which POSIX mode
/// would otherwise have replaced with `~/.sh_history`, exported for the start only.
fn posix_start_env(
    env_add: &mut Vec<(String, String)>,
    noprofile: bool,
    env: &dyn Fn(&str) -> Option<String>,
) {
    let mut add = |key: &str, value: String| env_add.push((key.to_string(), value));
    add("ATERM_BASH_POSIX", "1".to_string());
    add(
        "ENV",
        format!("${{ATERM_SHELL_INTEGRATION_DIR}}/bash/{BASH_LOADER_FILE}"),
    );
    if let Some(theirs) = env("ENV") {
        add("ATERM_BASH_ENV", theirs);
    }
    if env("HISTFILE").is_none()
        && let Some(home) = env("HOME").filter(|home| !home.is_empty())
    {
        add("HISTFILE", format!("{home}/.bash_history"));
        add("ATERM_BASH_HISTFILE", "1".to_string());
    }
    if noprofile {
        add("ATERM_BASH_NOPROFILE", "1".to_string());
    }
}

/// [`merge_shell_args`] for PowerShell: `injection.argv_override` is `[pwsh,
/// -ExecutionPolicy, Bypass, -NoExit, -Command, <dot-source>]`.
fn merge_powershell(injection: &mut InjectionEnv, args: &[String]) -> ArgsMerge {
    let Some(ours) = injection.argv_override.as_deref() else {
        return ArgsMerge::Integrated;
    };
    let Some((argv0, tail)) = ours.split_first() else {
        return ArgsMerge::Integrated;
    };
    let mut argv = vec![argv0.clone()];
    for arg in args {
        match pwsh_parameter(arg) {
            Some(PwshParameter::Ours) => continue,
            Some(PwshParameter::Foreign) => {
                return ArgsMerge::Bypass(format!(
                    "`{arg}` in shell_args runs PowerShell other than as the interactive shell \
                     aterm integrates (or sets the execution policy it needs)"
                ));
            }
            None => argv.push(arg.clone()),
        }
    }
    argv.extend(tail.iter().cloned());
    injection.argv_override = Some(argv);
    ArgsMerge::Integrated
}

/// A PowerShell parameter the merge cannot pass beside the integration's own.
enum PwshParameter {
    /// `-NoExit`: the integration passes it already.
    Ours,
    /// A command, a file, an encoded command, help or the version, non-interactive
    /// mode, or the execution policy the integration sets.
    Foreign,
}

/// Which [`PwshParameter`] `arg` is. PowerShell takes any unambiguous prefix of a
/// parameter's name, in any case, with `:value` attached or not.
fn pwsh_parameter(arg: &str) -> Option<PwshParameter> {
    let name = arg.strip_prefix('-')?;
    let name = name.strip_prefix('-').unwrap_or(name);
    let name = name.split(':').next().unwrap_or(name).to_ascii_lowercase();
    if name.is_empty() {
        return None;
    }
    let abbreviates = |full: &str, min: usize| name.len() >= min && full.starts_with(&name);
    if abbreviates("noexit", 3) {
        return Some(PwshParameter::Ours);
    }
    let foreign = ["command", "file", "encodedcommand", "version", "help"]
        .iter()
        .any(|full| abbreviates(full, 1))
        || abbreviates("commandwithargs", 1)
        || abbreviates("noninteractive", 3)
        || abbreviates("executionpolicy", 2)
        || ["?", "ec", "ep", "cwa"].contains(&name.as_str());
    foreign.then_some(PwshParameter::Foreign)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    fn bash() -> InjectionEnv {
        InjectionEnv {
            env_add: vec![("ATERM_SHELL_INTEGRATION_DIR".to_string(), "/si".to_string())],
            argv_override: Some(strings(&["bash", "--rcfile", "/si/bash/x"])),
        }
    }

    fn no_env(_: &str) -> Option<String> {
        None
    }

    fn merged(args: &[&str], env: &dyn Fn(&str) -> Option<String>) -> (ArgsMerge, InjectionEnv) {
        let mut injection = bash();
        let merge = merge_shell_args_in(ShellType::Bash, &mut injection, &strings(args), env);
        (merge, injection)
    }

    fn argv(injection: &InjectionEnv) -> Vec<&str> {
        injection
            .argv_override
            .as_deref()
            .unwrap_or_default()
            .iter()
            .map(String::as_str)
            .collect()
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    fn added<'a>(injection: &'a InjectionEnv, key: &str) -> Option<&'a str> {
        injection
            .env_add
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// Plain options keep integration: longs before the loader, shorts (and an
    /// option's value) after it — the order bash accepts.
    #[test]
    fn bash_options_go_where_bash_accepts_them() {
        let (merge, injection) = merged(&["-o", "vi", "--verbose", "-O", "extglob", "-i"], &no_env);
        assert_eq!(merge, ArgsMerge::Integrated);
        assert_eq!(
            argv(&injection),
            [
                "bash",
                "--verbose",
                "--rcfile",
                "/si/bash/x",
                "-o",
                "vi",
                "-O",
                "extglob",
                "-i"
            ]
        );
        assert_eq!(injection.env_add.len(), 1, "no env of its own");
        let (_, empty) = merged(&[], &no_env);
        assert_eq!(argv(&empty), ["bash", "--rcfile", "/si/bash/x"], "no args");
        // A long option with one dash, while only long options came before it,
        // as bash reads it; after a short option it is a cluster again. `+o
        // posix` turns POSIX mode off.
        let (merge, injection) = merged(&["-noediting", "-i", "+o", "posix"], &no_env);
        assert_eq!(merge, ArgsMerge::Integrated);
        assert_eq!(
            argv(&injection),
            [
                "bash",
                "--noediting",
                "--rcfile",
                "/si/bash/x",
                "-i",
                "+o",
                "posix"
            ]
        );
    }

    /// What no integrated interactive bash can run stands integration aside, and
    /// says which argument.
    #[test]
    fn bash_args_that_are_no_interactive_shell_stand_integration_aside() {
        for (args, names) in [
            (&["-c", "echo"][..], "`-c`"),
            (&["-ic", "echo"], "`-c`"),
            (&["script.sh"], "`script.sh`"),
            (&["--posix"], "`--posix`"),
            (&["-o", "posix"], "`-o posix`"),
            (&["-io", "posix"], "`-o posix`"),
            (&["-posix"], "`-posix`"),
            (&["--verbose", "-restricted"], "`-restricted`"),
            (&["-help"], "`-help`"),
            (&["--restricted"], "`--restricted`"),
            (&["-r"], "`-r`"),
            (&["-s", "a"], "`-s`"),
            (&["--version"], "`--version`"),
            (&["--", "x"], "`--`"),
            (&["-o"], "`-o`"),
            (&["--rcfile"], "`--rcfile`"),
        ] {
            match merged(args, &no_env).0 {
                ArgsMerge::Bypass(why) => assert!(why.contains(names), "{args:?}: {why}"),
                ArgsMerge::Integrated => panic!("{args:?} kept integration"),
            }
        }
    }

    /// Off macOS a login bash, or `--norc`, starts in POSIX mode on the loader:
    /// `--posix` after the person's long options, `ENV` through the integration
    /// dir, `~/.bash_history` kept, their own `ENV` handed back.
    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn a_login_bash_starts_in_posix_mode_on_the_loader() {
        let env = |key: &str| match key {
            "HOME" => Some("/home/u".to_string()),
            "ENV" => Some("/home/u/.shrc".to_string()),
            _ => None,
        };
        let (merge, injection) = merged(&["-l", "-i", "--noprofile"], &env);
        assert_eq!(merge, ArgsMerge::Integrated);
        assert_eq!(
            argv(&injection),
            ["bash", "--noprofile", "--posix", "-l", "-i"]
        );
        assert_eq!(added(&injection, "ATERM_BASH_POSIX"), Some("1"));
        assert_eq!(
            added(&injection, "ENV"),
            Some("${ATERM_SHELL_INTEGRATION_DIR}/bash/bashrc")
        );
        assert_eq!(added(&injection, "ATERM_BASH_ENV"), Some("/home/u/.shrc"));
        assert_eq!(added(&injection, "HISTFILE"), Some("/home/u/.bash_history"));
        assert_eq!(added(&injection, "ATERM_BASH_HISTFILE"), Some("1"));
        assert_eq!(added(&injection, "ATERM_BASH_NOPROFILE"), Some("1"));

        let (_, norc) = merged(&["--norc"], &|key| {
            (key == "HISTFILE").then(|| "/elsewhere".to_string())
        });
        assert_eq!(argv(&norc), ["bash", "--norc", "--posix"]);
        assert_eq!(added(&norc, "HISTFILE"), None, "theirs is inherited");
        assert_eq!(added(&norc, "ATERM_BASH_ENV"), None);
        assert_eq!(added(&norc, "ATERM_BASH_NOPROFILE"), None);

        let (_, login) = merged(&["--login"], &no_env);
        assert_eq!(argv(&login), ["bash", "--login", "--posix"]);

        let (_, rc) = merged(&["--rcfile", "/home/u/.myrc", "-x"], &no_env);
        assert_eq!(argv(&rc), ["bash", "--rcfile", "/si/bash/x", "-x"]);
        assert_eq!(added(&rc, "ATERM_BASH_RCFILE"), Some("/home/u/.myrc"));

        // One dash, as bash takes it: `-login` is a login shell, `-rcfile` a file.
        let (_, login) = merged(&["-login", "-i"], &no_env);
        assert_eq!(argv(&login), ["bash", "--login", "--posix", "-i"]);
        let (_, rc) = merged(&["-rcfile", "~/.myrc"], &no_env);
        assert_eq!(argv(&rc), ["bash", "--rcfile", "/si/bash/x"]);
        assert_eq!(added(&rc, "ATERM_BASH_RCFILE"), Some("~/.myrc"));
    }

    /// On macOS (and under Git Bash) the loader reads the login files itself:
    /// `-l`/`--login` is left to it, and a choice of startup files stands
    /// integration aside.
    #[cfg(not(all(unix, not(target_os = "macos"))))]
    #[test]
    fn the_login_style_loader_stands_in_for_login() {
        let (merge, injection) = merged(&["-li", "--login", "-o", "vi", "-l"], &no_env);
        assert_eq!(merge, ArgsMerge::Integrated);
        assert_eq!(
            argv(&injection),
            ["bash", "--rcfile", "/si/bash/x", "-i", "-o", "vi"]
        );
        for args in [&["--norc"][..], &["--noprofile"], &["--rcfile", "x"]] {
            assert!(
                matches!(merged(args, &no_env).0, ArgsMerge::Bypass(_)),
                "{args:?}"
            );
        }
    }

    fn pwsh() -> InjectionEnv {
        InjectionEnv {
            env_add: Vec::new(),
            argv_override: Some(strings(&[
                "pwsh",
                "-ExecutionPolicy",
                "Bypass",
                "-NoExit",
                "-Command",
                ". x",
            ])),
        }
    }

    /// PowerShell takes the person's parameters first (`-Login` must be), drops a
    /// `-NoExit` of theirs, and stands aside for a command, a file or a policy.
    #[test]
    fn powershell_parameters_go_first_unless_they_run_something_else() {
        let mut injection = pwsh();
        let args = strings(&["-Login", "-NoLogo", "-noe", "-WorkingDirectory", "/tmp"]);
        assert_eq!(
            merge_shell_args_in(ShellType::PowerShell, &mut injection, &args, &no_env),
            ArgsMerge::Integrated
        );
        assert_eq!(
            injection.argv_override.unwrap(),
            strings(&[
                "pwsh",
                "-Login",
                "-NoLogo",
                "-WorkingDirectory",
                "/tmp",
                "-ExecutionPolicy",
                "Bypass",
                "-NoExit",
                "-Command",
                ". x"
            ])
        );
        for arg in [
            "-c",
            "-Command",
            "-f",
            "-File:x.ps1",
            "-ec",
            "-e",
            "-ex",
            "-v",
            "-NonInteractive",
            "-?",
        ] {
            let mut injection = pwsh();
            assert!(
                matches!(
                    merge_shell_args_in(
                        ShellType::PowerShell,
                        &mut injection,
                        &strings(&[arg]),
                        &no_env
                    ),
                    ArgsMerge::Bypass(_)
                ),
                "{arg}"
            );
        }
    }

    /// The config check says what the spawn would: a sentence for args that stand
    /// integration aside, nothing for args it carries or a shell it never touches.
    #[test]
    fn the_config_check_answers_as_the_merge_does() {
        assert!(
            shell_args_bypass(ShellType::Bash, &strings(&["-c", "x"]))
                .is_some_and(|why| why.contains("`-c`"))
        );
        assert_eq!(
            shell_args_bypass(ShellType::Bash, &strings(&["-o", "vi"])),
            None
        );
        assert_eq!(
            shell_args_bypass(ShellType::Zsh, &strings(&["-c", "x"])),
            None
        );
        assert_eq!(
            shell_args_bypass(ShellType::Unknown, &strings(&["-c"])),
            None
        );
    }

    /// zsh, fish and WSL are integrated without touching argv: the spawn (or the
    /// Windows seam) hands them their arguments itself.
    #[test]
    fn shells_integrated_through_env_keep_their_argv_untouched() {
        for shell in [
            ShellType::Zsh,
            ShellType::Fish,
            ShellType::Wsl,
            ShellType::Cmd,
        ] {
            let mut injection = bash();
            assert_eq!(
                merge_shell_args_in(shell, &mut injection, &strings(&["-c", "x"]), &no_env),
                ArgsMerge::Integrated
            );
            assert_eq!(argv(&injection), ["bash", "--rcfile", "/si/bash/x"]);
        }
    }
}
