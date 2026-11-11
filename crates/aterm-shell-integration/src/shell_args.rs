// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Merging user `shell_args` into shell-integration startup argv ([`merge_shell_args`]).

use crate::{BASH_LOADER_FILE, InjectionEnv, ShellType};

/// What becomes of `shell_args` for an integrated shell ([`merge_shell_args`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArgsMerge {
    /// The integration stays.
    Integrated,
    /// The arguments request a startup mode that bypasses shell integration.
    Bypass(String),
}

/// Merge `shell_args` into `injection`, the integration [`crate::prepare`] built for `shell`.
#[inline]
pub fn merge_shell_args(
    shell: ShellType,
    injection: &mut InjectionEnv,
    args: &[String],
) -> ArgsMerge {
    merge_shell_args_in(shell, injection, args, &|key| std::env::var(key).ok())
}

/// Return the [`ArgsMerge::Bypass`] reason if `args` would bypass integration for `shell`.
#[must_use]
#[inline]
pub fn shell_args_bypass(shell: ShellType, args: &[String]) -> Option<String> {
    let mut probe = crate::injection_for(shell, std::path::Path::new("/"))?;
    match merge_shell_args_in(shell, &mut probe, args, &|_| None) {
        ArgsMerge::Integrated => None,
        ArgsMerge::Bypass(why) => Some(why),
    }
}

/// [`merge_shell_args`] parameterized over the environment (`ENV`, `HISTFILE`, `HOME`).
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

const LOGIN_STYLE_LOADER: bool = !cfg!(all(unix, not(target_os = "macos")));

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

fn parse_bash_args(args: &[String]) -> Result<BashArgs, String> {
    let mut out = BashArgs::default();
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

fn merge_bash(
    injection: &mut InjectionEnv,
    args: &[String],
    env: &dyn Fn(&str) -> Option<String>,
) -> ArgsMerge {
    let Some([argv0, start @ ..]) = injection.argv_override.as_deref() else {
        return ArgsMerge::Integrated;
    };
    let (argv0, start) = (argv0.clone(), start.to_vec());
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
        parsed.longs.retain(|long| long != "--login");
        parsed.shorts.retain_mut(|short| {
            if short.value || !short.text.starts_with('-') {
                return true;
            }
            short.text.retain(|c| c != 'l');
            short.text != "-"
        });
        argv.extend(parsed.longs);
        argv.extend(start);
    } else if parsed.login || parsed.norc {
        argv.extend(parsed.longs);
        argv.push("--posix".to_string());
        posix_start_env(&mut injection.env_add, parsed.noprofile, env);
    } else {
        argv.extend(parsed.longs);
        argv.extend(start);
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

/// The env of a bash started `--posix` with `$ENV` naming the loader.
pub(crate) fn posix_start_env(
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
    if env("HISTFILE").is_none() {
        if crate::BASH_LOGIN_START {
            add("ATERM_BASH_HISTFILE", "home".to_string());
        } else if let Some(home) = env("HOME").filter(|home| !home.is_empty()) {
            add("HISTFILE", format!("{home}/.bash_history"));
            add("ATERM_BASH_HISTFILE", "1".to_string());
        }
    }
    if noprofile {
        add("ATERM_BASH_NOPROFILE", "1".to_string());
    }
}

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

enum PwshParameter {
    /// `-NoExit`: the integration passes it already.
    Ours,
    /// A parameter that runs a command/file, exits, or overrides the integration policy.
    Foreign,
}

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
#[path = "shell_args_tests.rs"]
mod tests;
