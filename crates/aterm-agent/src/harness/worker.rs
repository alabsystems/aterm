// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE WORKERS `spawn agent=<word>` STARTS, ONE ROW PER HARNESS
//! ([`WORKERS`]): Claude Code, Gemini CLI, agy and Muse Code. One mechanism
//! writes every worker's first line ([`line`]) and resolves the program it
//! types ([`starter`]); a row says only what differs between the CLIs. Each
//! row's flags were read in the CLI's own source or bundle (never by running
//! it), and the relaunch lanes that already carry each one
//! (`upgrade_gemini`, `upgrade_agy`, `upgrade_muse`) agree with them:
//!
//! * **The task is argv, never typed** ([`WorkerCli::operand`]): the task
//!   file's whole text through `"$(command cat -- "$ATERM_TASK_FILE")"`, as
//!   for Claude Code ([`upgrade::TASK_OPERAND`]).
//!   * Claude Code: `claude [--model <m>] -- "<task>"`, its first prompt an
//!     operand.
//!   * Gemini CLI 0.46.0: `--prompt-interactive` (`-i`, "Execute the provided
//!     prompt and continue in interactive mode"): `interactive =
//!     !!argv.promptInteractive || …` in the bundle, so the TUI stays up after
//!     the first turn. Its value is ATTACHED
//!     ([`upgrade::TASK_PROMPT_INTERACTIVE`]): yargs splits `--name=value` at
//!     its first `=` and keeps the rest whole (`/^--?([^=]+)=([\s\S]*)$/`),
//!     where a next-word value opening with `-` is refused (`Not enough
//!     arguments following`). Its positional, which 0.46.0 also reads as an
//!     interactive prompt, would be read as a query list.
//!   * agy 1.4: `--prompt-interactive` ("Run an initial prompt interactively
//!     and continue the session"); it takes no positional prompt (exit 2).
//!     Its flag set takes an attached value whatever it holds.
//!   * Muse Code 1.4.10: `muse [OPTIONS] [PROMPT]` ("pass a prompt to start a
//!     session", the TUI root's parser): `--` then the prompt, as Claude
//!     Code takes it.
//!
//!   agy's task file holds no blank line ([`WorkerCli::blank_lines`]): its
//!   screen draws one inside a message as the gap under a message, so the
//!   rows past it would read as the agent's reply.
//!
//!   Every lane drops that first prompt from a relaunch's argv
//!   (`upgrade_gemini`'s and `upgrade_agy`'s `prompt-interactive` and `i`,
//!   `upgrade_muse`'s positional): a crashed worker relaunched on its
//!   session is never handed its task again.
//! * **`model=`** is each CLI's own `--model <m>` ([`WorkerCli::model_flag`]):
//!   all four take one, so none refuses it. A model opening with `-` is no
//!   model: yargs (Gemini CLI) and clap (Muse Code) read it as a flag and
//!   exit, so the line refuses it.
//! * **The folder trust**: where the worker's shell stands in the one folder
//!   its starter trusted for it (the spawn's `cwd=`), Muse Code's line
//!   carries its trust-for-this-run flag ([`WorkerCli::trust_flag`]:
//!   `--trust-workspace`, "Trust this workspace for this run …; does not
//!   save trust") in place of a dialog a rule would answer. Gemini CLI's
//!   folder is trusted by a TRUSTED-FOLDERS FILE OF THE WORKER'S OWN
//!   ([`WorkerCli::trust_file_env`]: `GEMINI_CLI_TRUSTED_FOLDERS_PATH`,
//!   bundle: `Storage.getTrustedFoldersPath`) naming that folder alone
//!   ([`trusted_folders_text`]), never its `--skip-trust`: that flag sets
//!   `GEMINI_CLI_TRUST_WORKSPACE=true` in the CLI's own environment (bundle
//!   0.46.0), under which `checkPathTrust` trusts EVERY
//!   path, and its tools' environment keeps every `GEMINI_CLI_*` name — so a
//!   Gemini CLI the worker's shell tool starts in any other folder would load
//!   that folder's settings, hooks and MCP servers with no dialog. The file
//!   names the folder by its path, so it rides wherever the shell stands: a
//!   Gemini CLI started anywhere else is untrusted by it. GEMINI CLI NEEDS NO
//!   DIALOG ANSWERED: its first prompt is submitted while its folder-trust
//!   dialog stands (the bundle's initial-prompt effect does not wait on that
//!   dialog), and its `Trust folder` relaunches the CLI with the same argv
//!   (`relaunchApp`), so a dialog answered on a worker's screen would send
//!   the task a second time; the window harness never presses it on a
//!   worker (`supervise::policy::approval`). Claude Code and agy have no
//!   such flag: their dialog is answered by the window harness's rule, in
//!   that same folder alone.
//! * **Gemini CLI's own restart** ([`WorkerCli::session_flag`]): its
//!   `/settings` restart, its IDE-trust and admin-settings restarts all exit
//!   199, and its parent node runs the CLI again with the same argv — the
//!   first prompt with it. So the line starts the session under an id of its
//!   own (`--session-id <uuid>`, [`fresh_session_id`]): the restarted CLI
//!   finds that session already written and refuses to start (`Session ID
//!   … already exists`, exit 42) rather than send the task again. The
//!   relaunch lane drops the flag (`upgrade_gemini`'s `session-id`) and
//!   resumes the session by its id.
//! * **The program** ([`starter`]): the starter's own, where the starter runs
//!   the same agent — Claude Code's as its relaunch reads it
//!   (`relaunch::caller_claude`); agy's and Muse Code's image, or atpkg's
//!   managed twin where it is a store build (as their relaunch lanes type
//!   it); Gemini CLI's PATH word `gemini`, which its lane types too (its
//!   process is `node` running a Cellar script) — with the homes its relaunch
//!   carries ([`WorkerCli::dirs`]) and the flags of its argv that move its
//!   config or say how it reaches its provider ([`Starter::args`]: agy's
//!   `--gemini_dir` and `--app_data_dir`, Muse Code's `--provider`,
//!   `--preset`, `--base-url` and `-c`/`--config`), which its relaunch lane
//!   carries too. Else the CLI's word on the worker's PATH, where atpkg's
//!   shell hook puts its managed link.

use std::path::{Path, PathBuf};

use aterm_phase::Program;
use atpkg::caller_shell::ProcArgs;

use super::upgrade::{self, Dialect};

/// How a starter's process is known to run a row's CLI, and the program a
/// worker's line types for it ([`starter_of`]).
#[derive(Clone, Copy)]
enum Launches {
    /// Claude Code: read by its relaunch's own reader
    /// (`relaunch::caller_claude`).
    Claude,
    /// A CLI known by its image and argv (`is`), typed by its PATH word:
    /// Gemini CLI, whose image is `node` running its script.
    Word {
        is: fn(Option<&Path>, &[String]) -> bool,
    },
    /// A CLI whose image is named by its word: its own path, or — where
    /// `store_build` says it is a build in atpkg's store — atpkg's managed
    /// twin (the path its relaunch lane types), else the word.
    Image { store_build: fn(&Path) -> bool },
}

/// One harness a worker can run ([`WORKERS`]).
pub struct WorkerCli {
    /// The agent, whose reader reads the worker's screen.
    pub agent: Program,
    /// `spawn agent=<word>`, and the program on the worker's PATH.
    pub word: &'static str,
    /// The fixed text the line ends with, the task as one argument.
    pub operand: &'static str,
    /// The flag `model=` names the model by, followed by the model.
    pub model_flag: &'static str,
    /// The CLI's flag that trusts the folder it starts in for this run alone,
    /// carried only where the worker's shell stands in the folder its starter
    /// trusted for it ([`Fresh::trusted_here`]).
    pub trust_flag: Option<&'static str>,
    /// The variable naming a TRUSTED-FOLDERS FILE the CLI reads in place of
    /// its own (Gemini CLI's `GEMINI_CLI_TRUSTED_FOLDERS_PATH`): the line
    /// names the file written for this worker ([`Fresh::trust_file`],
    /// [`trusted_folders_text`]), which trusts the starter's folder alone,
    /// wherever the shell stands ([module](self)).
    pub trust_file_env: Option<&'static str>,
    /// The flag that starts a NEW session under the id the line names
    /// ([`Fresh::session`]): Gemini CLI's `--session-id`, so its own restart,
    /// which runs the same argv again, refuses the id it already wrote rather
    /// than send the task a second time ([module](self)).
    pub session_flag: Option<&'static str>,
    /// Whether the first prompt may hold a BLANK LINE: `false` where the
    /// CLI's screen cannot bound a message holding one. agy draws a
    /// message's blank line as a blank row with the rows under it two
    /// columns in, as its reply's (measured in the lab 2026-10-21,
    /// `aterm_phase::agy::fixtures::lab::WORKER_FIRST_PROMPT`), so its reader
    /// reads the rows past it as the reply: a task's later paragraph, and the
    /// fact line's `NEEDS YOU:` (a need nobody said, raised as the worker's
    /// first turn ended), and both in the worker's result. Such a task file
    /// drops its blank lines, the fact on the line under the task.
    pub blank_lines: bool,
    /// Environment a fresh start sets that the line drops first when it
    /// would not fit (Claude Code's survey switch).
    pub nicety: &'static [(&'static str, &'static str)],
    /// The variables of a starter's environment the line carries that name a
    /// DIRECTORY: each must be absolute, since the worker starts in its own
    /// folder, where a relative one names another — save an XDG base
    /// directory's set empty, which the XDG rule reads as unset (and Muse
    /// Code's relaunch carries as set, [`super::upgrade_muse::relaunch_env`]).
    pub dirs: &'static [&'static str],
    launches: Launches,
    /// The starter's environment a worker's line carries: its relaunch lane's
    /// own pairs, under the user's home.
    homes: fn(&ProcArgs, &Path) -> Vec<(String, String)>,
    /// The flags of the starter's argv a worker's line carries ([`Starter::args`]):
    /// the ones its relaunch lane carries that move its config or say how it
    /// reaches its provider. `Err` names a flag no table knows, which could
    /// hold another's value: nothing guesses.
    flags: fn(&[String]) -> Result<Vec<String>, String>,
}

/// No flag of a starter's rides: Claude Code (whose relaunch reads none) and
/// Gemini CLI (whose homes are its environment's).
fn no_flags(_: &[String]) -> Result<Vec<String>, String> {
    Ok(Vec::new())
}

/// EVERY WORKER A SPAWN CAN START, one row per harness. Codex has none: its
/// screen lane was retired (2026-10-16), and `spawn agent=codex` stays
/// refused.
pub static WORKERS: [WorkerCli; 4] = [
    WorkerCli {
        agent: Program::Claude,
        word: "claude",
        operand: upgrade::TASK_OPERAND,
        model_flag: "--model",
        trust_flag: None,
        trust_file_env: None,
        session_flag: None,
        blank_lines: true,
        nicety: &super::relaunch::CLAUDE_LAUNCH_ENV,
        dirs: &[super::accounts::HOME_ENV, super::accounts::HOMEDIR_ENV],
        launches: Launches::Claude,
        homes: |_, _| Vec::new(),
        flags: no_flags,
    },
    WorkerCli {
        agent: Program::Gemini,
        word: super::upgrade_gemini::PROGRAM,
        operand: upgrade::TASK_PROMPT_INTERACTIVE,
        model_flag: "--model",
        trust_flag: None,
        trust_file_env: Some(super::upgrade_gemini::TRUSTED_FOLDERS_ENV),
        session_flag: Some("--session-id"),
        blank_lines: true,
        nicety: &[],
        dirs: &[
            super::upgrade_gemini::HOME_ENV,
            super::upgrade_gemini::CLI_HOME_ENV,
        ],
        launches: Launches::Word {
            is: super::upgrade_gemini::is_gemini_process,
        },
        // Its relaunch carries a trusted-folders file a starter's own spawn
        // named; a worker of that starter is trusted by its own alone.
        homes: |args, home| {
            super::upgrade_gemini::relaunch_env(|n| args.env_var(n), home)
                .into_iter()
                .filter(|(name, _)| name != super::upgrade_gemini::TRUSTED_FOLDERS_ENV)
                .collect()
        },
        flags: no_flags,
    },
    WorkerCli {
        agent: Program::Agy,
        word: super::upgrade_agy::PROGRAM,
        operand: upgrade::TASK_PROMPT_INTERACTIVE,
        model_flag: "--model",
        trust_flag: None,
        trust_file_env: None,
        session_flag: None,
        blank_lines: false,
        nicety: &[],
        dirs: &[super::upgrade_agy::HOME_ENV],
        launches: Launches::Image {
            store_build: super::upgrade_agy::store_build,
        },
        homes: |args, home| super::upgrade_agy::relaunch_env(|n| args.env_var(n), home),
        flags: |argv| Ok(super::upgrade_agy::config_flags(argv)),
    },
    WorkerCli {
        agent: Program::Muse,
        word: super::upgrade_muse::PROGRAM,
        operand: upgrade::TASK_OPERAND,
        model_flag: "--model",
        trust_flag: Some("--trust-workspace"),
        trust_file_env: None,
        session_flag: None,
        blank_lines: true,
        nicety: &[],
        dirs: &[
            super::upgrade_muse::DATA_ENV,
            super::upgrade_muse::CONFIG_ENV,
            super::upgrade_muse::HOME_ENV,
        ],
        launches: Launches::Image {
            store_build: super::upgrade_muse::store_build,
        },
        homes: |args, home| super::upgrade_muse::relaunch_env(|n| args.env_var(n), home),
        flags: |argv| {
            super::upgrade_muse::provider_flags(argv).map_err(|refusal| match refusal {
                upgrade::ArgvRefusal::UnknownFlag(flag)
                | upgrade::ArgvRefusal::NotResumable(flag) => flag,
            })
        },
    },
];

/// The row of `agent`, `None` for one no worker runs (Codex, the generic
/// reader).
#[must_use]
pub fn worker_cli(agent: Program) -> Option<&'static WorkerCli> {
    WORKERS.iter().find(|w| w.agent == agent)
}

/// What a worker's line types and carries: the program, and the starter's
/// environment and flags when it runs the same agent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Starter {
    pub exe: PathBuf,
    pub env: Vec<(String, String)>,
    /// The flags of the starter's argv the line carries
    /// ([`WorkerCli::flags`]); `Err`: its argv held a flag no table knows,
    /// and the line is refused rather than started on another config or
    /// provider than the starter's.
    pub args: Result<Vec<String>, String>,
}

impl WorkerCli {
    /// The worker's program when no starter's own rides the line: the word,
    /// under the shell's own environment.
    #[must_use]
    pub fn plain(&self) -> Starter {
        Starter {
            exe: PathBuf::from(self.word),
            env: Vec::new(),
            args: Ok(Vec::new()),
        }
    }
}

/// THE STARTER'S OWN AGENT OF `cli`'S KIND, as a worker's line follows it:
/// the program and the environment of process `pid` (the group in front of
/// the starter's terminal) under the user's `home`; `None` where `pid` runs
/// no such agent, or its argv cannot be read.
#[must_use]
pub fn starter(cli: &WorkerCli, pid: u32, home: &Path) -> Option<Starter> {
    if let Launches::Claude = cli.launches {
        let c = super::relaunch::caller_claude(pid, home)?;
        let env = super::relaunch::required_env(
            c.home.as_deref(),
            c.homedir.as_deref(),
            c.login.as_deref(),
        )
        .into_iter()
        .map(|(n, v)| (n.to_string(), v.to_string()))
        .collect();
        return Some(Starter {
            exe: c.exe,
            env,
            args: Ok(Vec::new()),
        });
    }
    let args = atpkg::caller_shell::process_args(pid)?;
    let image = super::upgrade_drive::exe_of(pid);
    starter_of(cli, image.as_deref(), &args, home, || {
        super::upgrade_drive::managed_agent_twin(cli.word)
    })
}

/// [`starter`]'s reading of one process, `image` its executable and `args`
/// its exec arguments, `twin` atpkg's managed twin of `cli`'s word where
/// asked (a store build). `None` for a process that runs no `cli`, and for
/// Claude Code, which its relaunch's reader reads.
pub(crate) fn starter_of(
    cli: &WorkerCli,
    image: Option<&Path>,
    args: &ProcArgs,
    home: &Path,
    twin: impl FnOnce() -> Option<PathBuf>,
) -> Option<Starter> {
    let exe = match cli.launches {
        Launches::Claude => return None,
        Launches::Word { is } => is(image, &args.argv).then(|| PathBuf::from(cli.word))?,
        Launches::Image { store_build } => {
            let image =
                image.filter(|i| i.file_name().and_then(|n| n.to_str()) == Some(cli.word))?;
            if store_build(image) {
                twin().unwrap_or_else(|| PathBuf::from(cli.word))
            } else {
                image.to_path_buf()
            }
        }
    };
    Some(Starter {
        exe,
        env: (cli.homes)(args, home),
        args: (cli.flags)(&args.argv),
    })
}

/// What a worker's line names beside its program ([`line`]).
#[derive(Clone, Copy, Debug, Default)]
pub struct Fresh<'a> {
    /// `model=`: the CLI's `--model`.
    pub model: Option<&'a str>,
    /// The shell stands in the folder its starter trusted for it: the row's
    /// [`WorkerCli::trust_flag`] rides.
    pub trusted_here: bool,
    /// The trusted-folders file written for this worker, which the row's
    /// [`WorkerCli::trust_file_env`] names to the CLI.
    pub trust_file: Option<&'a Path>,
    /// The new session's id, which the row's [`WorkerCli::session_flag`]
    /// starts it under ([`fresh_session_id`]).
    pub session: Option<&'a str>,
}

/// THE TEXT OF A WORKER'S TRUSTED-FOLDERS FILE ([`WorkerCli::trust_file_env`]):
/// Gemini CLI's `trustedFolders.json` form, `{"<folder>": "TRUST_FOLDER"}`,
/// the one folder its starter trusted for it (`TRUST_FOLDER` holds the folder
/// and what is under it; bundle 0.46.0's `isPathTrusted`) and no other — not
/// even one the user trusted in their own file.
#[must_use]
pub fn trusted_folders_text(folder: &str) -> String {
    let mut map = std::collections::BTreeMap::new();
    map.insert(folder, "TRUST_FOLDER");
    aterm_json::to_string(&map).unwrap_or_else(|_| String::from("{}"))
}

/// A NEW SESSION'S ID for [`Fresh::session`]: a random version-4 UUID, the
/// form Gemini CLI's `--session-id` takes and its relaunch lane types back
/// (`upgrade_gemini::is_session_uuid`).
///
/// # Errors
/// The system's random source failed ([`aterm_uds::rand::fill`]).
pub fn fresh_session_id() -> std::io::Result<String> {
    let mut b = [0u8; 16];
    aterm_uds::rand::fill(&mut b)?;
    Ok(session_id_of(b))
}

/// [`fresh_session_id`] of the 16 random bytes `b`.
#[must_use]
pub(crate) fn session_id_of(mut b: [u8; 16]) -> String {
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h = aterm_codec::hex::encode_lower(&b);
    format!(
        "{}-{}-{}-{}-{}",
        &h[..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..]
    )
}

/// THE LINE A WORKER'S SHELL RUNS AT ITS FIRST PROMPT: `starter`'s program
/// started fresh with the starter's flags ([`Starter::args`]), the CLI's
/// trust flag where the shell stands in the folder its starter trusted for
/// it, `<model flag> <model>` where one is named, `<session flag> <id>`
/// where the row has one, under the environment the starter's agent carries
/// (refused rather than dropped), the trusted-folders file the row names, and
/// the row's nicety (dropped first), its task the row's fixed operand. So
/// the task is argv — the agent's first prompt — and the shell stays under
/// the agent, as a relaunch needs it.
///
/// # Errors
/// The line cannot be written ([`upgrade::task_line_env`]), a directory it
/// carries is relative ([`WorkerCli::dirs`]: it would name another directory
/// under the worker's `cwd=` than it named for the starter), the starter's
/// argv could not be read ([`Starter::args`]), or the model opens with `-`.
pub fn line(
    cli: &WorkerCli,
    dialect: Dialect,
    starter: &Starter,
    fresh: &Fresh<'_>,
) -> Result<String, String> {
    let args = starter.args.as_ref().map_err(|flag| {
        format!(
            "the starter's {} argv holds `{flag}`, which no table here knows",
            cli.agent.product()
        )
    })?;
    let env: Vec<(&str, &str)> = starter
        .env
        .iter()
        .map(|(n, v)| (n.as_str(), v.as_str()))
        .collect();
    line_of(cli, dialect, &starter.exe, args, &env, fresh)
}

/// Whether `name` is an XDG base directory's variable, which the XDG rule
/// reads as unset where it is empty.
fn xdg_base(name: &str) -> bool {
    name.starts_with("XDG_") && name.ends_with("_HOME")
}

/// [`line`] by its parts.
pub(super) fn line_of(
    cli: &WorkerCli,
    dialect: Dialect,
    exe: &Path,
    args: &[String],
    env: &[(&str, &str)],
    fresh: &Fresh<'_>,
) -> Result<String, String> {
    // Rootless: under the worker's own directory (a root alone, as Windows
    // spells one with no drive, is not the worker's). An empty XDG base
    // directory is unset to the XDG rule, as it was to the starter.
    if let Some((name, value)) = env.iter().find(|(name, value)| {
        cli.dirs.contains(name)
            && !Path::new(value).has_root()
            && !(value.is_empty() && xdg_base(name))
    }) {
        return Err(format!(
            "the starter's {name} ({value:?}) is relative and the directory it names could not \
             be read"
        ));
    }
    if let Some(model) = fresh.model.filter(|m| m.starts_with('-')) {
        return Err(format!(
            "the model {model:?} opens with `-`, which {} reads as a flag",
            cli.agent.product()
        ));
    }
    let mut flags: Vec<String> = args.to_vec();
    if let Some(flag) = cli.trust_flag.filter(|_| fresh.trusted_here) {
        flags.push(flag.to_string());
    }
    if let Some(model) = fresh.model {
        flags.push(cli.model_flag.to_string());
        flags.push(model.to_string());
    }
    if let (Some(flag), Some(id)) = (cli.session_flag, fresh.session) {
        flags.push(flag.to_string());
        flags.push(id.to_string());
    }
    let trust = match (cli.trust_file_env, fresh.trust_file) {
        (Some(name), Some(file)) => {
            let file = file
                .to_str()
                .filter(|f| Path::new(f).has_root())
                .ok_or_else(|| {
                    format!(
                        "the trusted-folders file {} is no absolute path",
                        file.display()
                    )
                })?;
            Some((name, file))
        }
        _ => None,
    };
    // The starter's own trusted-folders file never rides: the worker's is
    // its own spawn's.
    let env: Vec<(&str, &str)> = env
        .iter()
        .copied()
        .filter(|(name, _)| Some(*name) != cli.trust_file_env)
        .chain(trust)
        .collect();
    let both: Vec<(&str, &str)> = env
        .iter()
        .copied()
        .chain(cli.nicety.iter().copied())
        .collect();
    upgrade::task_line_env(dialect, &both, exe, &flags, cli.operand)
        .or_else(|_| upgrade::task_line_env(dialect, &env, exe, &flags, cli.operand))
}

#[cfg(test)]
#[path = "worker_tests.rs"]
mod tests;
