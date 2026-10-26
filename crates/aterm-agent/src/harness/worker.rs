// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Workers started by `spawn agent=<word>`, one row per harness ([`WORKERS`]).

use std::path::{Path, PathBuf};

use aterm_phase::Program;
use atpkg::caller_shell::ProcArgs;

use super::upgrade::{self, Dialect};

#[path = "worker_grok.rs"]
pub mod grok;

/// How a starter's process is known to run a row's CLI, and the program a
/// worker's line types for it ([`starter_of`]).
#[derive(Clone, Copy)]
enum Launches {
    /// Claude Code: read by its relaunch's own reader
    /// (`relaunch::caller_claude`).
    Claude,
    /// A CLI whose image is named by its word (agy, Muse Code, Grok Build): its
    /// own path, or — where `store_build` says it is a build in atpkg's store —
    /// atpkg's managed twin (the path its relaunch lane types), else the word.
    Image { store_build: fn(&Path) -> bool },
    /// codex-alab: an image of ALab bytes ([`super::codex_alab::alab_bytes`]),
    /// typed as atpkg's managed twin where that twin execs ALab bytes too, else
    /// as itself. A vendor Codex image is no starter: the worker's line is then
    /// [`WorkerCli::unstarted`]'s, and a `codex` that never claims its tab is
    /// stalled `why=startup`.
    Alab,
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
    /// Why a worker carrying this environment must not start, under the user's home: a CLI
    /// that, signed out, opens a sign-in page by itself and never reads its task
    /// ([`WorkerCli::signed_out`]); `None` for a CLI with no such hazard.
    login: Option<LoginGate>,
}

/// Why a worker carrying an environment (its variables, then the user's home where known) must
/// not start: a CLI's sign-in floor ([`grok::signed_out`]).
type LoginGate = fn(&[(String, String)], Option<&Path>) -> Option<String>;

/// No flag of a starter's rides: Claude Code (whose relaunch reads none).
const fn no_flags(_: &[String]) -> Result<Vec<String>, String> {
    Ok(Vec::new())
}

/// The flags of a Codex starter's argv that move its config or say how it
/// reaches its provider; its model, folders and approval posture stay its own,
/// set by flag or by a `-c` of [`CODEX_OWN_KEYS`] ([`codex_flag_rides`]). A
/// `--profile` rides whole, whatever it sets.
const CODEX_CONFIG_FLAGS: &[&str] = &[
    "-c",
    "--config",
    "-p",
    "--profile",
    "--enable",
    "--disable",
    "--strict-config",
    "--oss",
    "--local-provider",
];

/// The config keys a Codex starter's `-c`/`--config` sets that stay its own: its
/// model (the worker's is `model=`'s), and its approval and sandbox posture (the
/// legacy keys and the permission profiles that select a sandbox alike).
const CODEX_OWN_KEYS: &[&str] = &[
    "model",
    "approval_policy",
    "approvals_reviewer",
    "sandbox_mode",
    "sandbox_workspace_write",
    "sandbox_permissions",
    "default_permissions",
    "permissions",
];

/// Whether a Codex starter's flag `name`, as `tokens` spell it, rides onto its
/// worker's line ([`CODEX_CONFIG_FLAGS`]).
fn codex_flag_rides(name: &str, tokens: &[String]) -> bool {
    if !CODEX_CONFIG_FLAGS.contains(&name) {
        return false;
    }
    if name != "-c" && name != "--config" {
        return true;
    }
    let value = match tokens {
        [_, value, ..] => value.as_str(),
        [attached] => attached
            .strip_prefix(name)
            .map_or("", |rest| rest.strip_prefix('=').unwrap_or(rest)),
        [] => "",
    };
    let key = value.split('=').next().unwrap_or("").trim();
    !CODEX_OWN_KEYS
        .iter()
        .any(|own| key == *own || key.strip_prefix(own).is_some_and(|r| r.starts_with('.')))
}

/// The `CODEX_HOME` a Codex starter's worker carries: the starter's own, where
/// set; else, for a starter run under another `HOME` than `user`'s, that home's
/// `.codex` (Codex's default), so the worker reads the starter's config and login.
fn codex_home_env(args: &ProcArgs, user: &Path) -> Vec<(String, String)> {
    let env = super::codex_alab::HOME_ENV;
    if let Some(home) = args.env_var(env).filter(|home| !home.is_empty()) {
        return vec![(env.to_string(), home.to_string())];
    }
    args.env_var("HOME")
        .map(Path::new)
        .filter(|home| home.is_absolute() && *home != user)
        .and_then(|home| home.join(".codex").to_str().map(str::to_string))
        .map(|codex| vec![(env.to_string(), codex)])
        .unwrap_or_default()
}

/// Every worker harness `spawn` can start, one row per harness.
pub static WORKERS: [WorkerCli; 5] = [
    WorkerCli {
        agent: Program::Claude,
        word: "claude",
        operand: upgrade::TASK_OPERAND,
        model_flag: "--model",
        trust_flag: None,
        nicety: &super::relaunch::CLAUDE_LAUNCH_ENV,
        dirs: &[super::accounts::HOME_ENV, super::accounts::HOMEDIR_ENV],
        launches: Launches::Claude,
        homes: |_, _| Vec::new(),
        flags: no_flags,
        login: None,
    },
    WorkerCli {
        agent: Program::Codex,
        word: "codex",
        operand: upgrade::TASK_OPERAND,
        model_flag: "--model",
        trust_flag: None,
        nicety: &[],
        dirs: &[super::codex_alab::HOME_ENV],
        launches: Launches::Alab,
        homes: codex_home_env,
        flags: |argv| match super::codex_alab::kept_flags(argv, codex_flag_rides) {
            Ok(kept) => Ok(kept),
            // A starter no relaunch could resume (`codex exec`, `--remote`, …)
            // lends no flags; its worker is a fresh interactive codex-alab.
            Err(upgrade::ArgvRefusal::NotResumable(_)) => Ok(Vec::new()),
            Err(upgrade::ArgvRefusal::UnknownFlag(flag)) => Err(flag),
        },
        login: None,
    },
    WorkerCli {
        agent: Program::Agy,
        word: super::upgrade_agy::PROGRAM,
        operand: upgrade::TASK_PROMPT_INTERACTIVE,
        model_flag: "--model",
        trust_flag: None,
        nicety: &[],
        dirs: &[super::upgrade_agy::HOME_ENV],
        launches: Launches::Image {
            store_build: super::upgrade_agy::store_build,
        },
        homes: |args, home| super::upgrade_agy::relaunch_env(|n| args.env_var(n), home),
        flags: |argv| Ok(super::upgrade_agy::config_flags(argv)),
        login: None,
    },
    WorkerCli {
        agent: Program::Muse,
        word: super::upgrade_muse::PROGRAM,
        operand: upgrade::TASK_OPERAND,
        model_flag: "--model",
        trust_flag: Some("--trust-workspace"),
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
        login: None,
    },
    WorkerCli {
        agent: Program::Grok,
        word: grok::PROGRAM,
        operand: upgrade::TASK_OPERAND,
        model_flag: "--model",
        trust_flag: None,
        nicety: &[grok::NO_SELF_UPDATE],
        dirs: &[grok::HOME_ENV],
        launches: Launches::Image {
            store_build: grok::store_build,
        },
        homes: |args, home| grok::relaunch_env(|n| args.env_var(n), home),
        flags: grok::config_flags,
        login: Some(grok::signed_out),
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
    /// Variables the worker's line removes from its environment: a pooled login's
    /// ([`super::relaunch::pool_args`]), none of a starter's own.
    pub unset: Vec<&'static str>,
}

impl WorkerCli {
    /// Why nothing would see this row's worker end, or `None` where something does: aterm's
    /// engine reads and answers an agent whose screen policy acts on it
    /// ([`Program::supervisable`]), and codex-alab supervises itself under its proven claim.
    /// Any other worker's task would stay `working` whatever it did, and
    /// `await task` would never wake on it, so `spawn` refuses it (fail closed) until its
    /// reader reads its turn end.
    ///
    /// This refusal is all that keeps Grok Build workers from starting today, and flipping
    /// `supervisable(Grok)` lifts it. The sign-in floor behind [`Self::signed_out`] then stands
    /// alone: it refuses a missing, empty or refused sign-in file, not an expired one grok has
    /// not refused yet (`docs/DESIGN-harness-repos-2026-10-24.md` D40). The Grok worker tests
    /// pin the refusal, so the flip turns them red and is made on purpose.
    #[must_use]
    pub fn end_unseen(&self) -> Option<String> {
        if matches!(self.launches, Launches::Alab) || self.agent.supervisable() {
            return None;
        }
        Some(format!(
            "{} is read here, not supervised: no reader sees its turn end yet, so its task \
             could never be marked done and `await task` would never wake on it; no worker was \
             started",
            self.agent.product()
        ))
    }

    /// The atpkg program `spawn agent=<word>` installs where it is absent: a vendor row outside
    /// the default set (Grok Build), which no machine installs unasked, so a lead naming it is
    /// the opt-in; once installed, atpkg's update pass keeps it current. `None` for a row the
    /// default set keeps (its pass installs it) and for one atpkg does not ship.
    #[must_use]
    pub fn on_demand(&self) -> Option<&'static str> {
        atpkg::vendor_direct::spec(self.word)
            .filter(|spec| !spec.default_set)
            .map(|spec| spec.program)
    }

    /// Why a worker on `starter` must not start under the user's home `user` (`None`: not
    /// known): a CLI that, signed out, opens a sign-in page by itself and never reads its task
    /// (Grok Build, [`grok::signed_out`]). `None` to start it. [`admit`] asks this first, before
    /// anything is installed.
    #[must_use]
    pub fn signed_out(&self, starter: &Starter, user: Option<&Path>) -> Option<String> {
        self.login.and_then(|login| login(&starter.env, user))
    }

    /// Whether the worker's first prompt may hold a blank line
    /// ([`aterm_phase::ScreenReader::bounds_messages`]).
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
            unset: Vec::new(),
        }
    }

    /// The line's program where no starter of this kind was found: for
    /// codex-alab, atpkg's managed twin where it execs ALab bytes; else
    /// [`Self::plain`]'s word.
    #[must_use]
    pub fn unstarted(&self) -> Starter {
        self.unstarted_by(|| super::upgrade_drive::managed_agent_twin(self.word))
    }

    /// [`Self::unstarted`] with the managed twin read by `twin`.
    #[must_use]
    pub fn unstarted_by(&self, twin: impl FnOnce() -> Option<PathBuf>) -> Starter {
        let exe = match self.launches {
            Launches::Alab => twin().and_then(super::self_claimed::alab_twin),
            _ => None,
        };
        Starter {
            exe: exe.unwrap_or_else(|| PathBuf::from(self.word)),
            ..self.plain()
        }
    }
}

/// What a worker's launcher found of the program its CLI runs ([`ensure_program`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Readiness {
    /// A program the default set keeps, or one atpkg does not ship: nothing to install.
    Kept,
    /// An on-demand program ([`WorkerCli::on_demand`]) already installed.
    Present,
    /// An on-demand program atpkg's verified install laid just now.
    Installed,
}

/// Make sure the program `cli` runs is there before its worker's line is typed: an on-demand
/// program ([`WorkerCli::on_demand`]) that is not `present` is installed by `install` (atpkg's
/// verified install, as `aterm pkg install <program>` runs it), unless `held` names why a person
/// keeps it off this machine (`[packages] exclude`, or a removal), which a lead's spawn does not
/// override.
///
/// # Errors
///
/// Why no worker can start: the program is held back, its install failed, or the install left
/// nothing installed (atpkg serves no build for this machine).
pub fn ensure_program(
    cli: &WorkerCli,
    present: impl Fn(&str) -> bool,
    held: impl FnOnce(&str) -> Option<String>,
    install: impl FnOnce(&str) -> Result<(), String>,
) -> Result<Readiness, String> {
    let Some(program) = cli.on_demand() else {
        return Ok(Readiness::Kept);
    };
    if present(program) {
        return Ok(Readiness::Present);
    }
    if let Some(why) = held(program) {
        return Err(format!(
            "{program} is not installed and {why}, so `spawn agent={}` installs nothing",
            cli.word
        ));
    }
    install(program).map_err(|why| format!("atpkg's install of {program} failed: {why}"))?;
    if !present(program) {
        return Err(format!(
            "atpkg's install of {program} left no {program} installed (no build serves this \
             machine, or its install was refused)"
        ));
    }
    Ok(Readiness::Installed)
}

/// Why a worker's launcher starts no worker ([`admit`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refused {
    /// The CLI is signed out for the login its line would start on ([`WorkerCli::signed_out`]).
    /// Only a person can sign in.
    SignedOut(String),
    /// The CLI's program is not installed and is held back, or its install failed
    /// ([`ensure_program`]).
    Program(String),
}

impl Refused {
    /// The refusal's words.
    #[must_use]
    pub fn why(&self) -> &str {
        match self {
            Self::SignedOut(why) | Self::Program(why) => why,
        }
    }
}

/// A worker's starter that its launcher admitted ([`admit`]). [`fn@line`] takes one in place of
/// a bare [`Starter`], so no line can be written without the sign-in and install checks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Admitted {
    starter: Starter,
    readiness: Readiness,
}

impl Admitted {
    /// The starter the line carries.
    #[must_use]
    pub const fn starter(&self) -> &Starter {
        &self.starter
    }

    /// What admission found of the CLI's program.
    #[must_use]
    pub const fn readiness(&self) -> Readiness {
        self.readiness
    }

    /// `starter` taken as admitted, for this crate's line tests.
    #[cfg(test)]
    pub(crate) const fn assumed(starter: Starter) -> Self {
        Self {
            starter,
            readiness: Readiness::Kept,
        }
    }
}

/// ADMIT A WORKER BEFORE ITS LINE IS WRITTEN, SIGN-IN FIRST. A CLI that, signed out, opens a
/// sign-in page by itself ([`WorkerCli::signed_out`]) is refused before anything is installed.
/// Installing it would put a signed-out program on every shell's `PATH` (atpkg lays its
/// `agents/<program>` twin), where any agent that typed its name would open that page in the
/// person's browser. Only a starter that passes the sign-in check gets
/// [`ensure_program`]'s install, through `present`, `held` and `install`.
///
/// # Errors
///
/// [`Refused::SignedOut`] before `present`, `held` or `install` is asked, else
/// [`ensure_program`]'s refusal as [`Refused::Program`].
pub fn admit(
    cli: &WorkerCli,
    starter: Starter,
    user: Option<&Path>,
    present: impl Fn(&str) -> bool,
    held: impl FnOnce(&str) -> Option<String>,
    install: impl FnOnce(&str) -> Result<(), String>,
) -> Result<Admitted, Refused> {
    if let Some(why) = cli.signed_out(&starter, user) {
        return Err(Refused::SignedOut(why));
    }
    let readiness = ensure_program(cli, present, held, install).map_err(Refused::Program)?;
    Ok(Admitted { starter, readiness })
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
            unset: Vec::new(),
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
        Launches::Image { store_build } => {
            let image =
                image.filter(|i| i.file_name().and_then(|n| n.to_str()) == Some(cli.word))?;
            if store_build(image) {
                twin().unwrap_or_else(|| PathBuf::from(cli.word))
            } else {
                image.to_path_buf()
            }
        }
        Launches::Alab => {
            let image = image
                .filter(|i| i.file_name().and_then(|n| n.to_str()) == Some(cli.word))
                .filter(|i| super::codex_alab::alab_bytes(i))?;
            twin()
                .and_then(super::self_claimed::alab_twin)
                .unwrap_or_else(|| image.to_path_buf())
        }
    };
    Some(Starter {
        exe,
        env: (cli.homes)(args, home),
        args: (cli.flags)(&args.argv),
        unset: Vec::new(),
    })
}

/// What a worker's line names beside its program ([`fn@line`]).
#[derive(Clone, Copy, Debug, Default)]
pub struct Fresh<'a> {
    /// `model=`: the CLI's `--model`.
    pub model: Option<&'a str>,
    /// Whether the shell stands in the folder its starter trusted for it.
    pub trusted_here: bool,
}

/// Format the shell command line a worker runs at its first prompt, for the starter [`admit`]
/// admitted.
///
/// # Errors
/// The line exceeds its budget, a carried directory is relative, the starter's argv holds a flag
/// no table knows, `model` starts with `-`, or a variable [`Starter::unset`] removes is one its
/// environment sets.
pub fn line(
    cli: &WorkerCli,
    dialect: Dialect,
    admitted: &Admitted,
    fresh: &Fresh<'_>,
) -> Result<String, String> {
    let starter = &admitted.starter;
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
    line_of(
        cli,
        dialect,
        &starter.exe,
        args,
        &env,
        &starter.unset,
        fresh,
    )
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
    unset: &[&str],
    fresh: &Fresh<'_>,
) -> Result<String, String> {
    // A rootless value would resolve under the worker's own folder, not the starter's; a root
    // with no drive (as Windows may spell one) passes. An empty XDG base directory is unset to
    // the XDG rule, as it was to the starter.
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
    let both: Vec<(&str, &str)> = env
        .iter()
        .copied()
        .chain(cli.nicety.iter().copied())
        .collect();
    upgrade::task_line_env_clear(dialect, &both, unset, exe, &flags, cli.operand)
        .or_else(|_| upgrade::task_line_env_clear(dialect, env, unset, exe, &flags, cli.operand))
}

#[cfg(test)]
#[path = "worker_tests.rs"]
mod tests;
