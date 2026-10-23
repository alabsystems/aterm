// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Workers started by `spawn agent=<word>`, one row per harness ([`WORKERS`]).

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
    /// CLI flag that trusts the starting folder for this run alone ([`Fresh::trusted_here`]).
    pub trust_flag: Option<&'static str>,
    /// Environment variable naming a per-worker trusted-folders file ([`Fresh::trust_file`]).
    pub trust_file_env: Option<&'static str>,
    /// CLI flag that starts a new session under [`Fresh::session`].
    pub session_flag: Option<&'static str>,
    /// Environment pairs set on a fresh start and dropped first when tight on space.
    pub nicety: &'static [(&'static str, &'static str)],
    /// Starter environment variables naming directories that must be absolute when carried.
    pub dirs: &'static [&'static str],
    launches: Launches,
    /// The starter's environment a worker's line carries: its relaunch lane's
    /// own pairs, under the user's home.
    homes: fn(&ProcArgs, &Path) -> Vec<(String, String)>,
    /// The flags of the starter's argv a worker's line carries ([`Starter::args`]):
    /// the ones its relaunch lane carries that move its config or say how it
    /// reaches its provider.
    flags: fn(&[String]) -> Result<Vec<String>, String>,
}

/// No flag of a starter's rides: Claude Code (whose relaunch reads none) and
/// Gemini CLI (whose homes are its environment's).
const fn no_flags(_: &[String]) -> Result<Vec<String>, String> {
    Ok(Vec::new())
}

/// Every worker harness `spawn` can start, one row per harness.
pub static WORKERS: [WorkerCli; 4] = [
    WorkerCli {
        agent: Program::Claude,
        word: "claude",
        operand: upgrade::TASK_OPERAND,
        model_flag: "--model",
        trust_flag: None,
        trust_file_env: None,
        session_flag: None,
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
        nicety: &[],
        dirs: &[
            super::upgrade_gemini::HOME_ENV,
            super::upgrade_gemini::CLI_HOME_ENV,
        ],
        launches: Launches::Word {
            is: super::upgrade_gemini::is_gemini_process,
        },
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

/// Worker row for `agent`, or `None` if unsupported by `spawn`.
#[must_use]
pub fn worker_cli(agent: Program) -> Option<&'static WorkerCli> {
    WORKERS.iter().find(|w| w.agent == agent)
}

/// Program, environment, and argv flags carried from a starter onto a worker's launch line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Starter {
    pub exe: PathBuf,
    pub env: Vec<(String, String)>,
    /// Carried starter argv flags ([`WorkerCli::flags`]), or `Err` on an unrecognized flag.
    pub args: Result<Vec<String>, String>,
}

impl WorkerCli {
    /// Whether the worker's first prompt may hold a blank line ([`aterm_phase::ScreenReader::bounds_messages`]).
    #[must_use]
    pub fn blank_lines(&self) -> bool {
        self.agent.reader().bounds_messages()
    }

    /// Plain [`Starter`] using the row's PATH word and empty environment/flags.
    #[must_use]
    pub fn plain(&self) -> Starter {
        Starter {
            exe: PathBuf::from(self.word),
            env: Vec::new(),
            args: Ok(Vec::new()),
        }
    }
}

/// Read process `pid` under `home` as a starter of `cli`'s kind, or `None`.
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

/// Resolve `image` and `args` as a [`Starter`] for `cli`, or `None`.
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

/// What a worker's line names beside its program ([`fn@line`]).
#[derive(Clone, Copy, Debug, Default)]
pub struct Fresh<'a> {
    /// `model=`: the CLI's `--model`.
    pub model: Option<&'a str>,
    /// Whether the shell stands in the folder its starter trusted for it.
    pub trusted_here: bool,
    /// Per-worker trusted-folders file passed via [`WorkerCli::trust_file_env`].
    pub trust_file: Option<&'a Path>,
    /// New session id passed via [`WorkerCli::session_flag`].
    pub session: Option<&'a str>,
}

/// Render the JSON text of a worker's `trustedFolders.json` trusting `folder` alone.
#[must_use]
pub fn trusted_folders_text(folder: &str) -> String {
    let mut map = std::collections::BTreeMap::new();
    map.insert(folder, "TRUST_FOLDER");
    aterm_json::to_string(&map).unwrap_or_else(|_| String::from("{}"))
}

/// Generate a random v4 UUID for [`Fresh::session`].
///
/// # Errors
/// The system random source failed ([`aterm_uds::rand::fill`]).
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

/// Format the shell command line a worker runs at its first prompt.
///
/// # Errors
/// Line exceeds budget, a carried directory is relative, starter argv is unreadable, or `model` starts with `-`.
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

/// [`fn@line`] by its parts.
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
