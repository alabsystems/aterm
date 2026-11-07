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
    /// The model a fresh start runs where `model=` names none ([`Fresh::model`]); `None`
    /// leaves the CLI its own default.
    pub default_model: Option<&'static str>,
    /// Flag words (each flag with its value) every fresh start carries.
    pub default_flags: &'static [&'static str],
    /// CLI flag that trusts the starting folder for this run alone ([`Fresh::trusted_here`]).
    pub trust_flag: Option<&'static str>,
    /// CLI flag that names the permission mode a fresh start runs in
    /// ([`Fresh::permission_mode`]), and the modes it takes; `None` for a CLI with no such flag.
    pub permission_flag: Option<ModeFlag>,
    /// CLI flag that lets the run drive the person's own browser ([`Fresh::chrome`]); `None` for a CLI
    /// with no such flag.
    pub chrome_flag: Option<&'static str>,
    /// CLI flag that names, comma-joined, the only built-in tools the run has ([`Fresh::tools`]);
    /// `None` for a CLI with no such flag.
    pub tools_flag: Option<&'static str>,
    /// CLI flag that hands the run one settings document of its own ([`Fresh::settings`]); `None`
    /// for a CLI with no such flag.
    pub settings_flag: Option<&'static str>,
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
    /// reaches its provider, and its permission posture held to THE CLI'S PERMISSION FLOOR: the
    /// least-asking posture the CLI's own flags spell that never stops a worker for a person's
    /// box on an ordinary edit or command and keeps the CLI's own sandbox or safety classifier,
    /// never one that skips them (owner, 2026-11-04: workers are fully automated by default;
    /// `DESIGN-workers-subagents-2026-11-04.md` §2: "its CLI's most automated mode short of
    /// bypass"). A starter at or above the floor lends its own posture; one below it, or naming
    /// none, lends the floor, which is so what an argv naming nothing lends
    /// ([`Self::unnamed_posture`]). Every CLI's floor is held here but Claude Code's, whose mode
    /// [`starter`] holds to it once it has read the starter's route ([`claude_starter_args`]),
    /// and codex-alab's, which [`starter_of`] judges beside its starter's own config
    /// ([`codex_lent`]). Each floor's first word is a posture marker of atpkg's managed twin
    /// ([`atpkg::yolo::decide`]), so a twin the line runs adds no yolo flag beside it: the floor
    /// holds whether or not the line runs that twin.
    flags: fn(&[String]) -> Result<Vec<String>, String>,
    /// Why a worker carrying this environment must not start, under the user's home: a CLI
    /// that, signed out, opens a sign-in page by itself and never reads its task
    /// ([`WorkerCli::signed_out`]); `None` for a CLI with no such hazard.
    login: Option<LoginGate>,
}

/// A CLI's flag naming the permission mode its run starts in ([`WorkerCli::permission_flag`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModeFlag {
    /// The flag, followed on the line by the mode.
    pub flag: &'static str,
    /// Every mode the flag takes, as the CLI spells it.
    pub modes: &'static [&'static str],
}

/// Claude Code's `--permission-mode` and the modes a worker's line may name.
pub const CLAUDE_PERMISSION_MODE: ModeFlag = ModeFlag {
    flag: "--permission-mode",
    modes: &[
        "default",
        "acceptEdits",
        "plan",
        "auto",
        "dontAsk",
        "bypassPermissions",
    ],
};

/// Claude Code's `--chrome` (2.1.311: "Enable Claude in Chrome integration"): the run drives the
/// person's own Chrome, where their sign-ins are.
pub const CLAUDE_CHROME_FLAG: &str = "--chrome";

/// Claude Code's `--tools`: the only built-in tools the run has (the real-Claude lab's lean launch,
/// `--tools Read`, ran 2.1.310 with Read its one tool, `crates/aterm-lab/src/autopilot_scenes.rs`).
/// A tool the run lacks is never called, whatever its permission mode or rules say. It names the
/// built-in set alone, so an MCP server's tools, Claude in Chrome's among them, stay (the lean
/// launch drops those with `--strict-mcp-config` beside it).
pub const CLAUDE_TOOLS_FLAG: &str = "--tools";

/// Claude Code's `--settings`: one settings document, as a JSON object or a file's path, read
/// above the user's and project's own. Measured on 2.1.318 in the real-Claude lab
/// (`docs/measured/claude-chrome-auto-2026-11-05.md`): of two on one line only the last is read,
/// and auto mode's classifier rules (`autoMode`) are read from it while a project's
/// `.claude/settings.json` and `settings.local.json` are not, a trusted folder's included. A line
/// that carries one is never handed aterm's usage tap, whose value is a `--settings` too
/// ([`super::tap::launch_value`]).
pub const CLAUDE_SETTINGS_FLAG: &str = "--settings";

/// Why a worker carrying an environment (its variables, then the user's home where known) must
/// not start: a CLI's sign-in floor ([`grok::signed_out`]).
type LoginGate = fn(&[(String, String)], Option<&Path>) -> Option<String>;

/// Claude Code's plan mode, as `--permission-mode` and the pill's flag spell it.
const CLAUDE_PLAN: &str = "plan";

/// The mode a Claude Code worker starts in where the mode its starter lends is below the floor,
/// or where it lends none ([`claude_worker_mode`]).
pub const CLAUDE_WORKER_FLOOR: &str = "auto";

/// The modes at or above a Claude Code worker's floor, which a starter lends its worker as they
/// are ([`claude_worker_mode`]): none of them stops for a person's box on each edit or command.
const CLAUDE_FLOOR_KEEPS: [&str; 3] = [CLAUDE_WORKER_FLOOR, "dontAsk", "bypassPermissions"];

/// The permission flags of a Claude Code starter's argv ([`claude_flags_mode`]): the mode its
/// worker takes from it, which [`starter`] then holds to the floor ([`claude_worker_mode`]).
/// [`claude_mode_flags`] spells a mode the starter moved to since it started.
fn claude_permission_flags(argv: &[String]) -> Result<Vec<String>, String> {
    Ok(claude_flags_mode(argv.iter().skip(1))
        .map_or_else(Vec::new, |mode| claude_mode_flags(&mode)))
}

/// The permission mode Claude Code reads from `tokens`, a run's flags up to a `--`, as
/// `--permission-mode` spells it. The last of `--dangerously-skip-permissions` and
/// `--permission-mode` wins, as Claude Code reads them. Plan mode never rides: it says what the
/// starter is doing now, not what it may do, and a worker in it could do no work.
fn claude_flags_mode<'a>(tokens: impl IntoIterator<Item = &'a String>) -> Option<String> {
    let mut mode = None;
    let mut tokens = tokens.into_iter();
    while let Some(tok) = tokens.next() {
        match tok.as_str() {
            "--" => break,
            "--dangerously-skip-permissions" => mode = Some("bypassPermissions".to_string()),
            "--permission-mode" => {
                mode = tokens
                    .next()
                    .filter(|m| *m != CLAUDE_PLAN)
                    .cloned()
                    .or(mode);
            }
            other => {
                if let Some(value) = other.strip_prefix("--permission-mode=")
                    && value != CLAUDE_PLAN
                {
                    mode = Some(value.to_string());
                }
            }
        }
    }
    mode
}

/// The flags that start a Claude Code worker in `mode`, as the permission pill or a
/// `--permission-mode` spells it.
fn claude_mode_flags(mode: &str) -> Vec<String> {
    match mode {
        "bypassPermissions" => vec!["--dangerously-skip-permissions".to_string()],
        "manual" => vec![
            CLAUDE_PERMISSION_MODE.flag.to_string(),
            "default".to_string(),
        ],
        mode => vec![CLAUDE_PERMISSION_MODE.flag.to_string(), mode.to_string()],
    }
}

/// THE MODE A CLAUDE CODE WORKER STARTS IN, from the one its starter lends it (`inherited`, as
/// `--permission-mode` or the pill's flag spells it; `None` where the starter names none or is
/// planning). A worker is fully automated by default (owner, 2026-11-04: "these workers likely
/// need to be started in auto / yolo mode!"), which supersedes, for this floor, the ruling that a
/// worker runs with its parent's permissions (owner ruling 2026-11-02). Where `floored`, a
/// starter in a mode that stops for a person's box (manual, `acceptEdits`), or one that names
/// none, lends [`CLAUDE_WORKER_FLOOR`], and one at or above the floor ([`CLAUDE_FLOOR_KEEPS`]:
/// auto, `dontAsk`, bypass) lends its own, so aterm never raises a worker to bypass. Not
/// `floored`, for a starter whose requests leave Anthropic's own API ([`claude_route_kept`]),
/// the worker keeps its starter's mode, as the 2026-11-02 ruling has it: through a gateway, auto
/// mode raises a classifier billing notice that nothing answers, and in the lab its classifier
/// then refused every command (`docs/measured/workers-lab-2026-10-16.md`, "Auto mode blocks a
/// gateway's Bash").
fn claude_worker_mode(inherited: Option<&str>, floored: bool) -> Option<&str> {
    if !floored {
        return inherited;
    }
    Some(
        inherited
            .filter(|mode| CLAUDE_FLOOR_KEEPS.contains(mode))
            .unwrap_or(CLAUDE_WORKER_FLOOR),
    )
}

/// The permission flags a Claude Code worker starts with, from the flags its starter lends it
/// (`inherited`: its argv's, [`claude_permission_flags`], or its pill's), held to the floor
/// where `floored` ([`claude_worker_mode`]).
fn claude_worker_flags(inherited: &[String], floored: bool) -> Vec<String> {
    let mode = claude_flags_mode(inherited);
    claude_worker_mode(mode.as_deref(), floored).map_or_else(Vec::new, claude_mode_flags)
}

/// The mode a Claude Code starter shows on `rows`, its tab's screen (`None`: none was read), as
/// `--permission-mode` spells it: the mode on the pill is the one it runs in now, which a
/// shift+tab may have moved from its argv's. A screen showing no pill (a box covers it), or plan
/// mode's, shows none, and the argv's stands.
fn claude_shown_mode(rows: Option<&[String]>) -> Option<String> {
    rows.and_then(super::relaunch::screen_mode)
        .filter(|mode| mode != CLAUDE_PLAN)
}

/// Whether `key`, a variable of a Claude Code launch's environment or settings, takes its
/// requests off Anthropic's own API: a route variable ([`super::netwatch::ROUTE_KEYS`]: a
/// gateway's base URL, a cloud provider's switch or endpoint, a socket) other than a proxy's,
/// which carries them to the same API.
fn leaves_the_api(key: &str) -> bool {
    use super::witness::{HTTP_PROXY_KEYS, HTTPS_PROXY_KEYS};
    super::netwatch::ROUTE_KEYS.contains(&key)
        && !HTTPS_PROXY_KEYS.contains(&key)
        && !HTTP_PROXY_KEYS.contains(&key)
}

/// WHY A CLAUDE CODE WORKER KEEPS ITS STARTER'S PERMISSION MODE in place of the floor
/// ([`claude_worker_mode`]), from `facts`, its starter's exec environment and the settings
/// documents it reads: a variable that takes its requests off Anthropic's own API
/// ([`leaves_the_api`]) set, not blank, in either, or an environment or a settings document that
/// could not be read or parsed, which leaves the route untold. `None`: the route is Anthropic's
/// own, and the floor holds. The starter's are read in place of the worker's, which do not exist
/// until its line runs: a variable the person's shell startup exports reaches both. The lab's
/// stub, which a development seam counts as the default route elsewhere
/// ([`super::netwatch::lab_route`]), is a gateway here: auto mode's classifier gets no answer it
/// can read from it.
fn claude_route_kept(facts: &super::netwatch::RouteFacts) -> Option<String> {
    use super::netwatch::{Source, settings_of};
    let leaves = |key: &str, value: &str| leaves_the_api(key) && !value.trim().is_empty();
    let Some(env) = &facts.env else {
        return Some("its environment could not be read".to_string());
    };
    if let Some((key, _)) = env
        .iter()
        .filter_map(|kv| kv.split_once('='))
        .find(|(key, value)| leaves(key, value))
    {
        return Some(format!("{key} in its environment"));
    }
    for (at, source) in &facts.settings {
        match source {
            Source::Missing => {}
            Source::Unreadable => return Some(format!("{at} could not be read")),
            Source::Text(text) => {
                let Some(settings) = settings_of(text) else {
                    return Some(format!("{at} could not be parsed"));
                };
                if let Some((key, _)) = settings.env.iter().find(|(key, value)| leaves(key, value))
                {
                    return Some(format!("{key} in {at}"));
                }
            }
        }
    }
    None
}

/// What decides the route of Claude Code process `process`, launched in `cwd` (`None`: not
/// known), under the user's `home`, read now: its exec environment, and every settings document
/// it reads ([`crate::supervise::policy::claude_settings::sources_under`], the managed ones in
/// `managed`, and the managed preference domains in `prefs`), none opened inside a file-provider
/// domain of `home`.
fn claude_route_facts(
    process: &ProcArgs,
    cwd: Option<&Path>,
    home: &Path,
    managed: Option<&Path>,
    prefs: &Path,
) -> super::netwatch::RouteFacts {
    use crate::supervise::policy::{WorkerEnv, claude_settings};
    let worker = WorkerEnv::new(process.clone());
    let settings = claude_settings::sources_under(
        &worker,
        cwd.unwrap_or_else(|| Path::new("")),
        managed,
        Some(home),
    )
    .into_iter()
    .chain(claude_settings::managed_preferences(&worker, prefs))
    .map(|source| (source.name, super::netwatch::Source::from(source.text)))
    .collect();
    super::netwatch::RouteFacts {
        env: Some(process.env.clone()),
        settings,
    }
}

/// The flags a Claude Code starter whose argv is `argv` lends its worker's line: the mode its
/// pill shows on `shown`, its tab's screen ([`claude_shown_mode`]), else its argv's
/// ([`claude_permission_flags`]), held to the floor where every route in `routes` is Anthropic's
/// own ([`claude_route_kept`], [`claude_worker_mode`]). The route is decided once, here, for the
/// argv's mode and the pill's alike, so a starter whose route keeps its mode lends the pill's as
/// it is, whatever its argv names. `routes` holds one reading per folder whose project settings
/// the worker's Claude Code may read: the starter's, and the worker's own launch folder where that
/// is another; a route any of them names keeps the mode. The reason a route keeps it is logged.
fn claude_starter_args(
    cli: &WorkerCli,
    argv: &[String],
    shown: Option<&[String]>,
    routes: &[super::netwatch::RouteFacts],
) -> Result<Vec<String>, String> {
    let kept = routes.iter().find_map(claude_route_kept);
    if let Some(why) = &kept {
        aterm_log::info!(
            "worker: its starter's Claude Code lends its own permission mode, with no floor: \
             {why}"
        );
    }
    let inherited = match claude_shown_mode(shown) {
        Some(mode) => Ok(claude_mode_flags(&mode)),
        None => (cli.flags)(argv),
    };
    inherited.map(|inherited| claude_worker_flags(&inherited, kept.is_none()))
}

/// The flags of a Codex starter's argv that move its config, say how it reaches its provider or
/// say what it may do unasked, the last held to the worker's floor ([`codex_floored`]); its model
/// and folders stay its own, set by flag or by a `-c` of [`CODEX_OWN_KEYS`]
/// ([`codex_flag_rides`]). A `--profile` rides whole, whatever it sets.
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
    "-a",
    "--ask-for-approval",
    "--approve-for-me",
    "-s",
    "--sandbox",
    "--dangerously-bypass-approvals-and-sandbox",
    "--yolo",
    "--not-so-yolo",
];

/// The config keys a Codex starter's `-c`/`--config` sets that stay its own: its
/// model (the worker's is `model=`'s).
const CODEX_OWN_KEYS: &[&str] = &["model"];

/// Whether a Codex starter's flag `name`, as `tokens` spell it, rides onto its
/// worker's line ([`CODEX_CONFIG_FLAGS`]).
fn codex_flag_rides(name: &str, tokens: &[String]) -> bool {
    CODEX_CONFIG_FLAGS.contains(&name)
        && codex_config_set(name, tokens)
            .is_none_or(|(key, _)| !CODEX_OWN_KEYS.contains(&key.first().copied().unwrap_or("")))
}

/// The value of Codex flag `name` as `tokens` spell it (the flag's own token, then its value's):
/// the next token, or the rest of an attached one (`-anever`, `-a=never`, `--config=k=v`).
fn codex_flag_value<'a>(name: &str, tokens: &'a [String]) -> &'a str {
    match tokens {
        [_, value, ..] => value.as_str(),
        [attached] => attached
            .strip_prefix(name)
            .map_or("", |rest| rest.strip_prefix('=').unwrap_or(rest)),
        [] => "",
    }
}

/// What a Codex `-c`/`--config` named `name` sets, as `tokens` spell it: its dotted key, each
/// segment trimmed of blanks and quotes, and its value as typed; `None` for any other flag.
fn codex_config_set<'a>(name: &str, tokens: &'a [String]) -> Option<(Vec<&'a str>, &'a str)> {
    if name != "-c" && name != "--config" {
        return None;
    }
    let set = codex_flag_value(name, tokens);
    let (key, value) = set.split_once('=').unwrap_or((set, ""));
    let key = key
        .split('.')
        .map(|segment| segment.trim().trim_matches(['"', '\'']))
        .collect();
    Some((key, value))
}

/// CODEX-ALAB'S PERMISSION FLOOR (the row's, [`WorkerCli::unnamed_posture`]): `--approve-for-me`,
/// "Route approval requests through automatic review using the workspace-write sandbox"
/// (codex-alab 0.166.0's `codex --help`). It sets `approvals_reviewer = "auto_review"`,
/// `approval_policy = "on-request"` and `sandbox_mode = "workspace-write"` as command-line
/// overrides (codex-alab's `codex-rs/utils/cli/src/shared_options.rs`): Codex's own counterpart
/// of Claude Code's auto mode, a reviewer agent answering each request to leave the sandbox,
/// which stays. Those overrides sit above every config layer, codex-alab's packaged in-aterm
/// defaults (`approval_policy = "never"`, `sandbox_mode = "danger-full-access"`, the lowest
/// layer) among them. codex-alab's own approval loop leaves a request whose reviewer is chosen in
/// any layer but its packaged defaults to that reviewer (its `alab-autopilot/src/approval.rs`), so
/// the reviewer, not that loop's one-shot yes, judges a floored worker's requests; edits and
/// commands inside the sandbox ask no one. The reviewer does not answer every request: Guardian
/// V2 hands one to a person where its model's catalog policy turns review off for that kind of
/// request, or where its review returns no decision (codex-alab's
/// `ext/guardian-v2/src/async_scorer/approval.rs`, `ext/guardian-reviewer/src/routing.rs`), and
/// that box waits for a person, since codex-alab's loop has already left it to the reviewer and
/// aterm answers no codex-alab box. Measured 2026-11-05 on that build: it refuses the flag beside
/// `--sandbox` or `--ask-for-approval`, or twice, so [`codex_floored`] drops those.
pub const CODEX_WORKER_FLOOR: [&str; 1] = ["--approve-for-me"];

/// The config keys that set a Codex run's posture, which a `-c` of a starter below the floor
/// sets in vain beside [`CODEX_WORKER_FLOOR`]'s overrides ([`codex_floored`]).
const CODEX_POSTURE_KEYS: &[&str] = &["approval_policy", "sandbox_mode", "approvals_reviewer"];

/// The `approvals_reviewer` values that leave a Codex run's requests to its reviewer agent, not
/// a person: `auto_review` and its older spelling `guardian_subagent` (codex-alab's
/// `codex-rs/protocol/src/config_types.rs`).
const CODEX_REVIEWERS: &[&str] = &["auto_review", "guardian_subagent"];

/// The posture key and value a Codex `-c` named `name` sets as `tokens` spell it
/// ([`CODEX_POSTURE_KEYS`]), else `None`. The key is read as Codex reads it, and atpkg's twin
/// with it: the text before the first `=`, trimmed of blanks (codex-alab's
/// `codex-rs/utils/cli/src/config_override.rs`), so a quoted key sets no posture. The value is
/// trimmed of blanks and quotes, as its TOML reading leaves it.
fn codex_posture_set<'a>(name: &str, tokens: &'a [String]) -> Option<(&'a str, &'a str)> {
    if name != "-c" && name != "--config" {
        return None;
    }
    let (key, value) = codex_flag_value(name, tokens).split_once('=')?;
    let key = key.trim();
    CODEX_POSTURE_KEYS
        .contains(&key)
        .then(|| (key, value.trim().trim_matches(['"', '\''])))
}

/// The flags `kept` a Codex starter lends its worker ([`CODEX_CONFIG_FLAGS`]), one unit per flag:
/// its name as Codex's parser reads it ([`super::codex_alab::option_name`]) and its tokens.
fn codex_units(kept: &[String]) -> Vec<(String, &[String])> {
    let mut units = Vec::new();
    let mut i = 0;
    while i < kept.len() {
        let (name, attached) = super::codex_alab::option_name(&kept[i]);
        let takes = !attached
            && matches!(
                super::codex_alab::flag(&name),
                Some((super::codex_alab::Arity::One, _))
            );
        let end = if takes {
            (i + 2).min(kept.len())
        } else {
            i + 1
        };
        units.push((name, &kept[i..end]));
        i = end;
    }
    units
}

/// A Codex starter's own config, read beside its argv where that names no approval policy
/// ([`codex_floored`]): its `config.toml` (an empty table where it has none), and whether it
/// runs inside aterm, where codex-alab's packaged defaults set `approval_policy = "never"`
/// beneath every layer (codex-alab's `alab-core/aterm-defaults.toml`, read only under
/// `TERM_PROGRAM=aterm`).
#[derive(Clone, Debug, PartialEq)]
struct CodexConfig {
    file: aterm_toml::Value,
    in_aterm: bool,
}

impl CodexConfig {
    /// What the file sets `key` to as Codex reads it: the table of `profile` (the starter's
    /// `--profile`, else the file's own `profile` key) over the file's top level.
    fn value(&self, profile: Option<&str>, key: &str) -> Option<&aterm_toml::Value> {
        let profile = profile.or_else(|| self.file.get("profile")?.as_str());
        profile
            .and_then(|profile| self.file.get("profiles")?.get(profile)?.get(key))
            .or_else(|| self.file.get(key))
    }
}

/// THE CONFIG A CODEX STARTER `args` READS (`None`: not told), under the user's home `user`:
/// `config.toml` in its `CODEX_HOME`, else in `.codex` under its `HOME`, else under `user`
/// ([`super::codex_rollout::codex_home`]). A relative `CODEX_HOME`, whose folder is the
/// starter's own working directory's, and a file that cannot be read or parsed tell nothing.
/// The user layer alone is read: a trusted project's `.codex/config.toml`, the system's and a
/// managed one are not, and the worker reads its own folder's.
fn codex_config(args: &ProcArgs, user: &Path) -> Option<CodexConfig> {
    let home = super::codex_rollout::codex_home(
        args.env_var(super::codex_alab::HOME_ENV),
        args.env_var("HOME").or_else(|| user.to_str()),
        None,
    )?;
    let file = match super::accounts::read_bounded(&home.join("config.toml"), 256 * 1024) {
        Ok(None) => aterm_toml::Value::Table(aterm_toml::Table::new()),
        Ok(Some(text)) => aterm_toml::from_str(&text).ok()?,
        Err(_) => return None,
    };
    Some(CodexConfig {
        file,
        in_aterm: args.env_var("TERM_PROGRAM") == Some("aterm"),
    })
}

/// THE FLAGS A CODEX WORKER STARTS WITH, from the ones its starter lends it (`kept`), held to
/// [`CODEX_WORKER_FLOOR`], beside the starter's own `config` (`None`: not told, as for a worker
/// no starter lends one, [`WorkerCli::unnamed_posture`]). A starter at or above the floor lends
/// its posture as it is: the floor itself (`--approve-for-me`, or its alias `--not-so-yolo`),
/// bypass (`--dangerously-bypass-approvals-and-sandbox`, or its alias `--yolo`), an approval
/// policy of `never`, by `-a`/`--ask-for-approval` or else by a `-c approval_policy`, which asks
/// no one, or a policy named beside a `-c approvals_reviewer` of [`CODEX_REVIEWERS`], which
/// leaves every request to the reviewer, not a person (the last `-c` of a key wins, as Codex
/// reads them). Each of those holds a posture marker of atpkg's twin, so the twin adds no yolo
/// flag beside it, or is the twin's own `--yolo`, which it spells as bypass once.
///
/// Where its argv names no policy, the policy its config gives stands (its `--profile`'s table,
/// else the file's, else, inside aterm, codex-alab's packaged `never`), as Codex reads them: one
/// at or above the floor by the same rule is lent spelled as `-a <policy>`, a twin marker, so the
/// worker runs at its starter's posture and the twin's yolo never decides it. A starter so in
/// codex-alab's packaged defaults lends `-a never` beside them, as the 2026-11-04 design has a
/// bypass parent pass bypass on. A reviewer with no policy named anywhere lends the floor.
///
/// One below it (a person answering under any other policy, a sandbox alone with no policy
/// given) or naming none lends the floor in place of every flag of its posture (`-a`, `-s`, and
/// a `-c` of [`CODEX_POSTURE_KEYS`]), so no worker is started past the floor, nor with a flag
/// Codex refuses beside it; the rest stays, the sandbox's own `sandbox_workspace_write`
/// settings and a `--profile` among them (the floor's overrides sit above a profile).
fn codex_floored(kept: Vec<String>, config: Option<&CodexConfig>) -> Vec<String> {
    let at_floor = |policy: Option<&str>, reviewer: Option<&str>| {
        policy == Some("never")
            || (policy.is_some() && reviewer.is_some_and(|by| CODEX_REVIEWERS.contains(&by)))
    };
    let units = codex_units(&kept);
    let mut lends_own = false;
    let (mut by_flag, mut by_config, mut reviewer, mut profile) = (None, None, None, None);
    for (name, tokens) in &units {
        match name.as_str() {
            "--approve-for-me"
            | "--not-so-yolo"
            | "--dangerously-bypass-approvals-and-sandbox"
            | "--yolo" => lends_own = true,
            "-a" | "--ask-for-approval" => by_flag = Some(codex_flag_value(name, tokens).trim()),
            "-p" | "--profile" => profile = Some(codex_flag_value(name, tokens).trim()),
            _ => match codex_posture_set(name, tokens) {
                Some(("approval_policy", policy)) => by_config = Some(policy),
                Some(("approvals_reviewer", by)) => reviewer = Some(by),
                _ => {}
            },
        }
    }
    let named = by_flag.or(by_config);
    if lends_own || at_floor(named, reviewer) {
        return kept;
    }
    if named.is_none()
        && let Some(config) = config
    {
        // A granular policy is a table, which no `-a` spells: it lends the floor.
        let policy = config.value(profile, "approval_policy").map_or(
            config.in_aterm.then_some("never"),
            aterm_toml::Value::as_str,
        );
        let by = config
            .value(profile, "approvals_reviewer")
            .and_then(aterm_toml::Value::as_str);
        if let Some(policy) = policy.filter(|policy| at_floor(Some(*policy), reviewer.or(by))) {
            let mut own = kept.clone();
            own.extend(["-a".to_string(), policy.to_string()]);
            return own;
        }
    }
    let mut floored: Vec<String> = units
        .iter()
        .filter(|(name, tokens)| {
            !matches!(
                name.as_str(),
                "-a" | "--ask-for-approval" | "-s" | "--sandbox"
            ) && codex_posture_set(name, tokens).is_none()
        })
        .flat_map(|(_, tokens)| tokens.iter().cloned())
        .collect();
    floored.extend(CODEX_WORKER_FLOOR.map(str::to_string));
    floored
}

/// Whether a Codex flag `name`, as `tokens` spell it, says what the run may do unasked: its
/// approval policy, sandbox, reviewer, or an alias of the floor or of bypass.
fn codex_posture_rides(name: &str, tokens: &[String]) -> bool {
    matches!(
        name,
        "-a" | "--ask-for-approval"
            | "-s"
            | "--sandbox"
            | "--approve-for-me"
            | "--not-so-yolo"
            | "--dangerously-bypass-approvals-and-sandbox"
            | "--yolo"
    ) || codex_posture_set(name, tokens).is_some()
}

/// The posture flags the root of a Codex starter's `argv` names ahead of what no relaunch could
/// resume (`exec`, `--remote`, …): those of the longest head of `argv` Codex's flag table reads.
fn codex_root_posture(argv: &[String]) -> Vec<String> {
    (1..argv.len())
        .rev()
        .find_map(|end| super::codex_alab::kept_flags(&argv[..end], codex_posture_rides).ok())
        .unwrap_or_default()
}

/// What a Codex starter that ran `argv` lends its worker, beside its own `config`
/// ([`codex_floored`]): its flags that ride ([`codex_flag_rides`]). One no relaunch could resume
/// (`codex exec`, `--remote`, …) lends only the posture its root names ahead of that
/// ([`codex_root_posture`]), held to the floor alike: its worker is a fresh interactive
/// codex-alab.
///
/// # Errors
///
/// A flag no table here knows, as typed.
fn codex_lent(argv: &[String], config: Option<&CodexConfig>) -> Result<Vec<String>, String> {
    match super::codex_alab::kept_flags(argv, codex_flag_rides) {
        Ok(kept) => Ok(codex_floored(kept, config)),
        Err(upgrade::ArgvRefusal::NotResumable(_)) => {
            Ok(codex_floored(codex_root_posture(argv), config))
        }
        Err(upgrade::ArgvRefusal::UnknownFlag(flag)) => Err(flag),
    }
}

/// The `CODEX_HOME` and `CODEX_AUTH_PATH` a Codex starter's worker carries:
/// the starter's own, where set; else, for a starter run under another `HOME`
/// than `user`'s, that home's `.codex` (Codex's default), so the worker reads
/// the starter's config and login.
fn codex_home_env(args: &ProcArgs, user: &Path) -> Vec<(String, String)> {
    let env = super::codex_alab::HOME_ENV;
    let mut out = if let Some(home) = args.env_var(env).filter(|home| !home.is_empty()) {
        vec![(env.to_string(), home.to_string())]
    } else {
        args.env_var("HOME")
            .map(Path::new)
            .filter(|home| home.is_absolute() && *home != user)
            .and_then(|home| home.join(".codex").to_str().map(str::to_string))
            .map(|codex| vec![(env.to_string(), codex)])
            .unwrap_or_default()
    };
    let auth_env = super::codex_alab::AUTH_PATH_ENV;
    if let Some(auth) = args.env_var(auth_env).filter(|auth| !auth.is_empty()) {
        out.push((auth_env.to_string(), auth.to_string()));
    }
    out
}

/// Every worker harness `spawn` can start, one row per harness.
pub static WORKERS: [WorkerCli; 5] = [
    WorkerCli {
        agent: Program::Claude,
        word: "claude",
        operand: upgrade::TASK_OPERAND,
        model_flag: "--model",
        default_model: None,
        default_flags: &[],
        trust_flag: None,
        permission_flag: Some(CLAUDE_PERMISSION_MODE),
        chrome_flag: Some(CLAUDE_CHROME_FLAG),
        tools_flag: Some(CLAUDE_TOOLS_FLAG),
        settings_flag: Some(CLAUDE_SETTINGS_FLAG),
        nicety: &super::relaunch::CLAUDE_LAUNCH_ENV,
        dirs: &[super::accounts::HOME_ENV, super::accounts::HOMEDIR_ENV],
        launches: Launches::Claude,
        homes: |_, _| Vec::new(),
        flags: claude_permission_flags,
        login: None,
    },
    WorkerCli {
        agent: Program::Codex,
        word: "codex",
        operand: upgrade::TASK_OPERAND,
        model_flag: "--model",
        default_model: None,
        default_flags: &[],
        trust_flag: None,
        permission_flag: None,
        chrome_flag: None,
        tools_flag: None,
        settings_flag: None,
        nicety: &[],
        dirs: &[
            super::codex_alab::HOME_ENV,
            super::codex_alab::AUTH_PATH_ENV,
        ],
        launches: Launches::Alab,
        homes: codex_home_env,
        // Its argv alone: [`starter_of`] reads a starter's config beside it.
        flags: |argv| codex_lent(argv, None),
        login: None,
    },
    WorkerCli {
        agent: Program::Agy,
        word: super::upgrade_agy::PROGRAM,
        operand: upgrade::TASK_PROMPT_INTERACTIVE,
        model_flag: "--model",
        default_model: None,
        default_flags: &[],
        trust_flag: None,
        permission_flag: None,
        chrome_flag: None,
        tools_flag: None,
        settings_flag: None,
        nicety: &[],
        dirs: &[super::upgrade_agy::HOME_ENV],
        launches: Launches::Image {
            store_build: super::upgrade_agy::store_build,
        },
        homes: |args, home| super::upgrade_agy::relaunch_env(|n| args.env_var(n), home),
        flags: |argv| Ok(super::upgrade_agy::worker_flags(argv)),
        login: None,
    },
    WorkerCli {
        agent: Program::Muse,
        word: super::upgrade_muse::PROGRAM,
        operand: upgrade::TASK_OPERAND,
        model_flag: "--model",
        default_model: Some(super::upgrade_muse::DEFAULT_MODEL),
        default_flags: super::upgrade_muse::DEFAULT_FLAGS,
        trust_flag: Some("--trust-workspace"),
        permission_flag: None,
        chrome_flag: None,
        tools_flag: None,
        settings_flag: None,
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
            super::upgrade_muse::worker_flags(argv).map_err(|refusal| match refusal {
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
        default_model: None,
        default_flags: &[],
        trust_flag: None,
        permission_flag: None,
        chrome_flag: None,
        tools_flag: None,
        settings_flag: None,
        nicety: &[grok::NO_SELF_UPDATE],
        dirs: &[grok::HOME_ENV],
        launches: Launches::Image {
            store_build: grok::store_build,
        },
        homes: |args, home| grok::relaunch_env(|n| args.env_var(n), home),
        flags: grok::worker_flags,
        login: Some(grok::signed_out),
    },
];

/// Worker row for `agent`, or `None` if unsupported by `spawn`.
#[must_use]
pub fn worker_cli(agent: Program) -> Option<&'static WorkerCli> {
    WORKERS.iter().find(|w| w.agent == agent)
}

/// The Codex program a launch aterm makes runs where it finds no starter's own: the program
/// Codex's row types then ([`WorkerCli::unstarted`]), atpkg's managed twin where it execs
/// codex-alab, else the bare word `codex`. The bare word is what the worker's own shell finds on
/// its `PATH`, which aterm does not read before it types the line, so it names no build aterm can
/// read ([`super::codex_alab::AuthPathUnread::Unnamed`]) and keeps no row's login its own,
/// whatever `~/.local/bin/codex` is. The judges of whether a launch keeps a row's
/// `CODEX_AUTH_PATH` login its own read this program where no starter's own program is found:
/// the accounts report's `next:`, `accounts login|renew`, and the `aterm ai` pool facts (`aterm
/// ai login`, `aterm ai review`).
#[must_use]
pub fn codex_launch_program() -> PathBuf {
    codex_launch_program_by(super::upgrade_drive::managed_agent_twin)
}

/// [`codex_launch_program`] with atpkg's managed twin found in `agents`, the `agents/` directory
/// of an atpkg layout a caller already holds (`None`: no layout, so no twin), as
/// [`WorkerCli::unstarted`] finds it in the configured one.
#[must_use]
pub fn codex_launch_program_in(agents: Option<&Path>) -> PathBuf {
    codex_launch_program_by(|word| super::upgrade_drive::agent_twin_in(agents?, word))
}

/// The program a launch aterm makes of `cli`, a pooled CLI other than Claude Code and Codex
/// (Grok Build, Kimi Code CLI), runs where it finds no starter's own: atpkg's managed copy in
/// `layout` (`None`: no atpkg layout) where one is laid, as the doctor finds it
/// ([`crate::ai::facts::managed_of`]), else its program word, which the launch's shell looks for
/// on its `PATH`. The accounts report's `next:` and `accounts use` judge by it whether a launch
/// would start anything here ([`crate::ai::facts::launch_finds_nothing`]).
#[must_use]
pub fn pooled_launch_program_in(
    cli: super::accounts::Cli,
    layout: Option<&atpkg::store::Layout>,
) -> PathBuf {
    layout
        .and_then(|layout| crate::ai::facts::managed_of(layout, cli.id()))
        .map_or_else(|| PathBuf::from(cli.id()), |managed| managed.path)
}

/// [`codex_launch_program`] with atpkg's managed twin of the Codex row's word read by `twin`.
fn codex_launch_program_by(twin: impl FnOnce(&str) -> Option<PathBuf>) -> PathBuf {
    let Some(cli) = worker_cli(Program::Codex) else {
        return PathBuf::from("codex");
    };
    cli.unstarted_by(|| twin(cli.word)).exe
}

/// The words of the [`WORKERS`] a `spawn` starts, as a usage line names them (`claude|…`): none
/// whose end nothing sees ([`WorkerCli::end_unseen`]), which `spawn` refuses.
#[must_use]
pub fn started_words() -> String {
    WORKERS
        .iter()
        .filter(|w| w.end_unseen().is_none())
        .map(|w| w.word)
        .collect::<Vec<_>>()
        .join("|")
}

/// The [`WORKERS`] row a program spawn's `program` names by its file name, where `spawn agent=`
/// starts that CLI ([`started_words`]) under the harness that concludes its worker: `spawn
/// program=` refuses it and names the agent form instead, since as a program worker its harness
/// concludes nothing of its task (aterm refuses a harness's mark on one). `None` for any other
/// program, Grok Build and Kimi Code included, which no harness concludes, and a wrapper that
/// starts one of those CLIs, which runs as the program it names.
#[must_use]
pub fn harness_of_program(program: &str) -> Option<&'static WorkerCli> {
    let name = Path::new(program).file_name()?.to_str()?;
    WORKERS
        .iter()
        .find(|w| w.word == name && w.end_unseen().is_none())
}

/// The shell command line a program worker runs at its first prompt: `program` and each of
/// `args`, quoted for `dialect`, with the task file as its standard input
/// ([`upgrade::TASK_STDIN`]). The program runs as the tab's own command, so the shell's
/// integration reports its exit code.
///
/// # Errors
/// `program` or an argument holds a control character, or the line exceeds its budget.
pub fn program_line(dialect: Dialect, program: &Path, args: &[String]) -> Result<String, String> {
    upgrade::task_line_env(dialect, &[], program, args, upgrade::TASK_STDIN)
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
            "{} is read, not supervised, by the aterm of the Mac asked to start it: no reader \
             there sees its turn end yet, so its task could never be marked done and `await \
             task` would never wake on it; no worker was started",
            self.agent.product()
        ))
    }

    /// The atpkg program this row runs: the `atpkg::vendor_direct::VENDORS` row of its word, every
    /// one in atpkg's default set. `None` for a program atpkg does not ship.
    #[must_use]
    pub fn atpkg_program(&self) -> Option<&'static str> {
        atpkg::vendor_direct::spec(self.word).map(|spec| spec.program)
    }

    /// Why a worker on `starter` must not start under the user's home `user` (`None`: not
    /// known): a CLI that, signed out, opens a sign-in page by itself and never reads its task
    /// (Grok Build, [`grok::signed_out`]). `None` to start it. [`admit`] asks this first, before
    /// atpkg's store is read or anything is installed.
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

    /// WHAT THIS ROW LENDS A WORKER WHOSE STARTER NAMES NO POSTURE, through its own
    /// [`Self::flags`] read of an argv naming nothing: the CLI's permission floor. Claude Code's
    /// lends none here: [`starter`] holds its mode to the floor once it has read the starter's
    /// route ([`claude_route_kept`]), and with no starter no route is read.
    ///
    /// # Errors
    ///
    /// None for an argv naming nothing; the row's [`Self::flags`] says what it refuses.
    pub fn unnamed_posture(&self) -> Result<Vec<String>, String> {
        (self.flags)(&[self.word.to_string()])
    }

    /// The line where no starter of this kind was found (a worker a lead of another CLI spawns,
    /// or one spawned from outside a session): for codex-alab, atpkg's managed twin where it
    /// execs ALab bytes; else [`Self::plain`]'s word. No starter lends a posture, so the line
    /// names the CLI's floor ([`Self::unnamed_posture`]): neither its own default, which asks a
    /// person (agy's and Muse Code's lines hold words atpkg's twin does not know, and the twin
    /// runs them as typed), nor the twin's yolo (codex-alab's bypass or packaged
    /// danger-full-access, Grok Build's always-approve), which is past the floor. Two lines name
    /// none: an unstarted Claude
    /// Code's, whose route no starter could tell ([`claude_route_kept`]), so atpkg's twin, where
    /// the line reaches it, starts it in bypass, as before the floor; and a bare `codex`, which
    /// may be a vendor Codex that refuses codex-alab's floor flag and would not start.
    #[must_use]
    pub fn unstarted(&self) -> Starter {
        self.unstarted_by(|| super::upgrade_drive::managed_agent_twin(self.word))
    }

    /// [`Self::unstarted`] with the managed twin read by `twin`.
    #[must_use]
    pub fn unstarted_by(&self, twin: impl FnOnce() -> Option<PathBuf>) -> Starter {
        let (exe, args) = match self.launches {
            Launches::Alab => match twin().and_then(super::self_claimed::alab_twin) {
                Some(twin) => (twin, self.unnamed_posture()),
                None => (PathBuf::from(self.word), Ok(Vec::new())),
            },
            _ => (PathBuf::from(self.word), self.unnamed_posture()),
        };
        Starter {
            exe,
            args,
            ..self.plain()
        }
    }
}

/// What a worker's launcher found of the program its CLI runs ([`ensure_program`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Readiness {
    /// A program atpkg does not ship ([`WorkerCli::atpkg_program`]): nothing to install.
    Unshipped,
    /// atpkg's store holds a build of it.
    Present,
    /// atpkg's verified install laid it just now.
    Installed,
    /// atpkg's store holds no build of it and installs none here. The line runs any copy of the
    /// shell's own.
    KeptOff {
        /// Why, as whatever keeps it off says it ([`Admitted::kept_off_why`]).
        why: String,
        /// That in the launcher's words ([`Admitted::kept_off`]).
        said: String,
    },
}

/// Make sure the program `cli` runs is there before its worker's line is typed. Every program
/// atpkg ships is in its default set, so its update pass lands one on every Mac it serves; a
/// spawn that comes first (a fresh Mac before that pass lands it, or one whose install failed)
/// installs it now by `install` (atpkg's verified install, as `aterm pkg install <program>`
/// runs it) where `present` finds no build in atpkg's store, so the line never types a program
/// that is not there. `held` names why atpkg installs no copy here, where it does not (a person's
/// `[packages] exclude`, removal or no to unattended installs, which a lead's spawn does not
/// override, or no atpkg that can install it on this Mac): then nothing is installed, and the
/// launcher says so and runs any copy of the shell's own. `held` is asked only of an absent
/// program, and `install` only of one nobody holds.
///
/// # Errors
///
/// Why no worker can start: the install failed (atpkg's words), or it left nothing installed.
pub fn ensure_program(
    cli: &WorkerCli,
    present: impl Fn(&str) -> bool,
    held: impl FnOnce(&str) -> Option<String>,
    install: impl FnOnce(&str) -> Result<(), String>,
) -> Result<Readiness, String> {
    let Some(program) = cli.atpkg_program() else {
        return Ok(Readiness::Unshipped);
    };
    if present(program) {
        return Ok(Readiness::Present);
    }
    if let Some(why) = held(program) {
        let said = format!(
            "atpkg keeps no {program} here ({why}), and `spawn agent={}` installs none, so the \
             line runs any {program} of the shell's own",
            cli.word
        );
        return Ok(Readiness::KeptOff { why, said });
    }
    install(program).map_err(|why| format!("{}{why}", install_failed(program)))?;
    if !present(program) {
        return Err(format!(
            "no build of {program} serves this machine, or its install was refused; atpkg's \
             install left no {program} installed"
        ));
    }
    Ok(Readiness::Installed)
}

/// What [`ensure_program`]'s words for a failed install of `program` lead atpkg's own with.
fn install_failed(program: &str) -> String {
    format!("atpkg's install of {program}{INSTALL_FAILED}")
}

/// Where atpkg's own words start in [`install_failed`]'s.
const INSTALL_FAILED: &str = " failed: ";

/// How atpkg leads a line that follows its failure (`print_unreachable_followup` in atpkg's
/// `cli.rs`: when to re-run, when a GitHub limit resets). Such a line says nothing of the cause.
const ATPKG_FOLLOW_UP: &str = "atpkg:   ";

/// What atpkg said of its failure, from its stderr lines joined by ` | ` (`PassRun::why`): the last
/// line that is no follow-up ([`ATPKG_FOLLOW_UP`]). A line starts where one is led `atpkg:`, since
/// a refusal can hold ` | ` of its own (`vendor.rs`'s `payload must be raw-binary | tar-gz-binary`),
/// and a stderr line with no such lead stays with the line before it. Its own
/// `atpkg: install <program> failed: ` goes, since [`Refused::short`] already says that.
fn atpkg_failure(said: &str) -> &str {
    const JOIN: &str = " | ";
    let mut rest = said;
    let line = loop {
        let Some(at) = rest.rfind(" | atpkg:") else {
            break if rest.starts_with(ATPKG_FOLLOW_UP) {
                said
            } else {
                rest
            };
        };
        let line = &rest[at + JOIN.len()..];
        if !line.starts_with(ATPKG_FOLLOW_UP) {
            break line;
        }
        rest = &rest[..at];
    };
    line.strip_prefix("atpkg: install ")
        .and_then(|rest| rest.split_once(INSTALL_FAILED))
        .map_or(line, |(_, cause)| cause)
}

/// The bytes a stall's attention gives a launcher's refusal (`harness_host::StartupHold::Refused`),
/// whose whole words stay in its log line and launch note.
pub const REFUSAL_ROOM: usize = 85;

/// Why a worker's launcher starts no worker ([`admit`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refused {
    /// The CLI is signed out for the login its line would start on ([`WorkerCli::signed_out`]),
    /// or the launch's local check of a Google login found no usable grant and its renewal left
    /// the login signed out ([`super::accounts_pool::prepare_google_worker`]). Only a person can
    /// sign in.
    SignedOut(String),
    /// atpkg's install of the CLI's absent program failed, or left nothing installed
    /// ([`ensure_program`]).
    Program(String),
    /// The preparation that follows the install failed with no person's sign-in to ask for: its
    /// pool's location is unavailable, its account's selection changed, or the renewal of a
    /// signed-out login could not run, which a later launch tries again ([`Admitted::prepare`]).
    Preparation(String),
}

impl Refused {
    /// The refusal's whole words.
    #[must_use]
    pub fn why(&self) -> &str {
        match self {
            Self::SignedOut(why) | Self::Program(why) | Self::Preparation(why) => why,
        }
    }

    /// The refusal in the few words a stall's attention has room for, the cause and what mends it
    /// first: a person's sign-in, the line atpkg said of its failure (`atpkg_failure`), or a
    /// preparation's own words. [`Self::why`] keeps the whole of it for the launch's note and log.
    #[must_use]
    pub fn short(&self) -> String {
        match self {
            Self::SignedOut(_) => "signed out, a person must sign in".to_owned(),
            Self::Program(why) => why.split_once(INSTALL_FAILED).map_or_else(
                || why.clone(),
                |(_, said)| format!("install failed: {}", atpkg_failure(said)),
            ),
            Self::Preparation(why) => why.clone(),
        }
    }
}

/// Words with no kind of their own are a preparation's.
impl From<String> for Refused {
    fn from(why: String) -> Self {
        Self::Preparation(why)
    }
}

/// Words with no kind of their own are a preparation's.
impl From<&str> for Refused {
    fn from(why: &str) -> Self {
        Self::Preparation(why.to_owned())
    }
}

/// The declared file source of an admitted Codex starter. Selector metadata
/// only: no auth bytes are read and no native grant authority is inferred.
#[must_use]
pub fn declared_codex_auth_file(
    starter: &Starter,
    home: Option<&Path>,
    cwd: &Path,
) -> Option<PathBuf> {
    let selected = |name: &str| {
        starter
            .env
            .iter()
            .rev()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    };
    if let Some(path) = selected("CODEX_AUTH_PATH") {
        let path = PathBuf::from(path);
        return path.is_absolute().then_some(path);
    }
    let selected_home = selected("HOME").map(Path::new).or(home);
    let home = match selected("CODEX_HOME").filter(|home| !home.is_empty()) {
        Some(home) => super::codex_rollout::codex_home(Some(home), None, Some(cwd))?,
        None => selected_home
            .filter(|home| home.is_absolute())?
            .join(".codex"),
    };
    Some(home.join("auth.json"))
}

/// A worker's starter that its launcher admitted ([`admit`]). [`fn@line`] takes one in place of
/// a bare [`Starter`], so no line can be written without the sign-in and install checks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Admitted {
    starter: Starter,
    readiness: Readiness,
}

impl Admitted {
    /// Prepare authentication after the program's installation has completed, retaining
    /// its readiness and returning the guard needed to recheck the prepared starter.
    ///
    /// # Errors
    /// Preparation's own refusal, or [`Refused::SignedOut`] where the replacement starter fails
    /// the CLI's sign-in check.
    pub fn prepare<T>(
        self,
        cli: &WorkerCli,
        user: Option<&Path>,
        prepare: impl FnOnce(Starter) -> Result<(Starter, T), Refused>,
    ) -> Result<(Self, T), Refused> {
        let (starter, guard) = prepare(self.starter)?;
        if let Some(why) = cli.signed_out(&starter, user) {
            return Err(Refused::SignedOut(why));
        }
        Ok((
            Self {
                starter,
                readiness: self.readiness,
            },
            guard,
        ))
    }

    /// This admission for a Claude Code worker whose parent lends it no flags, or whose starter
    /// could not be read ([`StarterRead::Unread`]), typed as the build atpkg's managed twin execs
    /// where its line, with `model`, holds nothing the twin reads as typed: the word would reach
    /// the twin, which adds yolo. A worker off the floor runs with its parent's permissions (owner
    /// ruling 2026-11-02), and a read that failed does not know those to be bypass, so that worker
    /// starts in the person's settings. A parent lends none only where its route keeps its worker
    /// off the floor ([`starter`]) and it names no mode: at the floor the line names `auto`, which
    /// the twin runs as typed. An unread starter gets no floor, for the reason an unread route gets
    /// none ([`claude_route_kept`]). Such a worker starts without the twin's usage tap, as one typed
    /// by its starter's image does. Read after admission, because admission can lay the twin. With
    /// no such twin the starter stays as it is.
    #[must_use]
    pub fn past_the_twin(self, cli: &WorkerCli, model: Option<&str>) -> Self {
        self.past_the_twin_by(cli, model, || {
            let twin = super::upgrade_drive::managed_agent_twin(cli.word)?;
            super::upgrade_drive::twin_build(&twin, cli.word)
        })
    }

    /// [`Self::past_the_twin`] with the twin's build read by `build`.
    #[must_use]
    fn past_the_twin_by(
        mut self,
        cli: &WorkerCli,
        model: Option<&str>,
        build: impl FnOnce() -> Option<PathBuf>,
    ) -> Self {
        let (Launches::Claude, Ok(args)) = (&cli.launches, &self.starter.args) else {
            return self;
        };
        let mut flags = args.clone();
        if let Some(model) = model.or(cli.default_model) {
            flags.extend([cli.model_flag.to_string(), model.to_string()]);
        }
        if matches!(
            atpkg::yolo::decide(cli.word, &flags),
            atpkg::yolo::Launch::Default(_)
        ) && let Some(build) = build()
        {
            self.starter.exe = build;
        }
        self
    }

    /// The starter the line carries.
    #[must_use]
    pub const fn starter(&self) -> &Starter {
        &self.starter
    }

    /// What admission found of the CLI's program.
    #[must_use]
    pub const fn readiness(&self) -> &Readiness {
        &self.readiness
    }

    /// What the launcher says where atpkg's store holds no build of the CLI's program and
    /// installs none here ([`Readiness::KeptOff`]).
    #[must_use]
    pub fn kept_off(&self) -> Option<&str> {
        match &self.readiness {
            Readiness::KeptOff { said, .. } => Some(said),
            _ => None,
        }
    }

    /// Why atpkg installs none of the CLI's program here, as whatever keeps it off says it
    /// ([`Readiness::KeptOff`]): what a stall names if no copy of the shell's own starts.
    #[must_use]
    pub fn kept_off_why(&self) -> Option<&str> {
        match &self.readiness {
            Readiness::KeptOff { why, .. } => Some(why),
            _ => None,
        }
    }

    /// `starter` taken as admitted, for this crate's line tests.
    #[cfg(test)]
    pub(crate) const fn assumed(starter: Starter) -> Self {
        Self {
            starter,
            readiness: Readiness::Unshipped,
        }
    }
}

/// ADMIT A WORKER BEFORE ITS LINE IS WRITTEN, SIGN-IN FIRST. A CLI that, signed out, opens a
/// sign-in page by itself ([`WorkerCli::signed_out`]) is refused before atpkg's store is read or
/// anything is installed: installing it would put a signed-out program on every shell's `PATH`
/// (atpkg lays its `agents/<program>` twin), and its line would open that page in the person's
/// browser. Only a starter that passes the sign-in check gets [`ensure_program`]'s install,
/// through `present`, `held` and `install`.
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

/// What reading a process as a starter of a row's kind came to ([`starter`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StarterRead {
    /// It runs the row's CLI.
    Read(Starter),
    /// It runs no CLI of the row's kind: its argv and image were read, or it has ended.
    Other,
    /// Its argv or image could not be read, so what it runs is not known.
    Unread,
}

/// Whether process `pid` has ended, gone or exited and not yet reaped: it holds no argv or image
/// to read, which is no failed read (the client of a spawn typed in a plain tab, or that line's
/// first pipeline stage, stands in front until it ends). A query with no answer is no end. Ask it
/// after the read that failed: an end only moves forward, and one asked before can come too soon.
fn ended(pid: u32) -> bool {
    use aterm_uds::process::{ProcessPresence, process_presence};
    i32::try_from(pid).is_ok_and(|pid| {
        matches!(
            process_presence(pid),
            ProcessPresence::Exited | ProcessPresence::Absent
        )
    })
}

/// What one process of a group read as ([`group_claude`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MemberRead {
    /// Claude Code.
    Claude,
    /// No Claude Code: its argv and image were read, or it has ended.
    Other,
    /// Its argv or image could not be read.
    Unread,
}

/// What a group's reading came to ([`claude_of_group`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GroupClaude {
    /// This process is the group's Claude Code.
    At(u32),
    /// Every process of the group was read, and none is Claude Code.
    None,
    /// Not known: no table of its processes, a member that could not be read and no Claude Code
    /// read beside it, or Claude Codes side by side, none inside another, with nothing to say
    /// which one the terminal's person drives.
    Unread,
}

/// How many parents [`group_claude`] follows from a member.
const PARENTS_MAX: usize = 4096;

/// [`GroupClaude`] of a group's `members`, with each process's parent by `parent`: the Claude Code
/// no other Claude Code of the group runs. A pid says nothing here: a long-lived lead's child can
/// hold the lower one, once the kernel has wrapped its pids.
fn group_claude(members: &[(u32, MemberRead)], parent: impl Fn(u32) -> Option<u32>) -> GroupClaude {
    let claudes: Vec<u32> = members
        .iter()
        .filter(|(_, read)| *read == MemberRead::Claude)
        .map(|(pid, _)| *pid)
        .collect();
    let outer: Vec<u32> = claudes
        .iter()
        .copied()
        .filter(|pid| {
            !std::iter::successors(parent(*pid), |up| parent(*up))
                .take(PARENTS_MAX)
                .any(|up| claudes.contains(&up))
        })
        .collect();
    match outer.as_slice() {
        [one] => GroupClaude::At(*one),
        [] if !members.iter().any(|(_, read)| *read == MemberRead::Unread) => GroupClaude::None,
        _ => GroupClaude::Unread,
    }
}

/// The process a Claude Code starter's line is read from, of the group `leader` leads: the leader
/// where it is Claude Code, else the member of its group that is ([`group_claude`]), where another
/// program leads the group (`time`, a script with no `exec`, a pipeline's first stage).
fn claude_of_group(leader: u32, home: &Path) -> GroupClaude {
    use super::{relaunch::caller_claude, upgrade_drive};
    if caller_claude(leader, home).is_some() {
        return GroupClaude::At(leader);
    }
    let table = upgrade_drive::table();
    if table.is_empty() {
        return GroupClaude::Unread;
    }
    let members: Vec<(u32, MemberRead)> = table
        .iter()
        .map(|(pid, ..)| *pid)
        .filter(|pid| upgrade_drive::process_group(*pid) == Some(i64::from(leader)))
        .map(|pid| {
            let read = if caller_claude(pid, home).is_some() {
                MemberRead::Claude
            } else if (atpkg::caller_shell::process_args(pid).is_some()
                && upgrade_drive::exe_of(pid).is_some())
                || ended(pid)
            {
                MemberRead::Other
            } else {
                MemberRead::Unread
            };
            (pid, read)
        })
        .collect();
    group_claude(&members, |pid| {
        table
            .iter()
            .find(|(row, ..)| *row == pid)
            .map(|(_, ppid, _)| *ppid)
    })
}

/// Read process `pid` under `home` as a starter of `cli`'s kind, a Claude Code row reading the group
/// it leads ([`claude_of_group`]). A Claude Code starter's mode is read here, from `shown`'s rows
/// (its tab's screen, whose pill shows the mode it runs in now; `None` where none was read) over its
/// argv, and held to its worker's floor where its route is read ([`claude_starter_args`]): at the
/// starter's own folder and at `worker_cwd`'s, the folder the worker's line runs in (the spawn's
/// `cwd=`), whose project settings may name a route the starter's lack. Another agent's starter
/// calls neither, so its caller never decides which kind of agent reads them.
#[must_use]
pub fn starter(
    cli: &WorkerCli,
    pid: u32,
    home: &Path,
    shown: impl FnOnce() -> Option<Vec<String>>,
    worker_cwd: impl FnOnce() -> Option<PathBuf>,
) -> StarterRead {
    if let Launches::Claude = cli.launches {
        let pid = match claude_of_group(pid, home) {
            GroupClaude::At(claude) => claude,
            GroupClaude::None => return StarterRead::Other,
            GroupClaude::Unread => return StarterRead::Unread,
        };
        let Some(c) = super::relaunch::caller_claude(pid, home) else {
            return StarterRead::Unread;
        };
        let Some(process) = atpkg::caller_shell::process_args(pid) else {
            return StarterRead::Unread;
        };
        let shown = shown();
        let worker_cwd = worker_cwd();
        let routes = {
            use crate::supervise::policy::claude_settings::{MANAGED_DIR, MANAGED_PREFERENCES_DIR};
            let cwd = aterm_sysprobe::process_cwd(pid);
            let at = |cwd: Option<&Path>| {
                claude_route_facts(
                    &process,
                    cwd,
                    home,
                    MANAGED_DIR.map(Path::new),
                    Path::new(MANAGED_PREFERENCES_DIR),
                )
            };
            let mut routes = vec![at(cwd.as_deref())];
            routes.extend(
                worker_cwd
                    .as_deref()
                    .filter(|own| Some(*own) != cwd.as_deref())
                    .map(|own| at(Some(own))),
            );
            routes
        };
        let env = super::relaunch::required_env(
            c.home.as_deref(),
            c.homedir.as_deref(),
            c.login.as_deref(),
        )
        .into_iter()
        .map(|(n, v)| (n.to_string(), v.to_string()))
        .collect();
        return StarterRead::Read(Starter {
            exe: c.exe,
            env,
            args: claude_starter_args(cli, &process.argv, shown.as_deref(), &routes),
            unset: Vec::new(),
        });
    }
    let Some(args) = atpkg::caller_shell::process_args(pid) else {
        return if ended(pid) {
            StarterRead::Other
        } else {
            StarterRead::Unread
        };
    };
    let image = super::upgrade_drive::exe_of(pid);
    match starter_of(cli, image.as_deref(), &args, home, || {
        super::upgrade_drive::managed_agent_twin(cli.word)
    }) {
        Some(starter) => StarterRead::Read(starter),
        None if image.is_none() && !ended(pid) => StarterRead::Unread,
        None => StarterRead::Other,
    }
}

/// Resolve `image` and `args` as a [`Starter`] for `cli`, or `None`, under the user's `home`: a
/// codex-alab starter's posture is judged beside the config it reads ([`codex_config`],
/// [`codex_lent`]), every other CLI's from its argv ([`WorkerCli::flags`]).
pub(crate) fn starter_of(
    cli: &WorkerCli,
    image: Option<&Path>,
    args: &ProcArgs,
    home: &Path,
    twin: impl FnOnce() -> Option<PathBuf>,
) -> Option<Starter> {
    let (exe, lent) = match cli.launches {
        Launches::Claude => return None,
        Launches::Image { store_build } => {
            let image =
                image.filter(|i| i.file_name().and_then(|n| n.to_str()) == Some(cli.word))?;
            let exe = if store_build(image) {
                twin().unwrap_or_else(|| PathBuf::from(cli.word))
            } else {
                image.to_path_buf()
            };
            (exe, (cli.flags)(&args.argv))
        }
        Launches::Alab => {
            let image = image
                .filter(|i| super::upgrade_drive::names_program(i, cli.word))
                .filter(|i| super::codex_alab::alab_bytes(i))?;
            let exe = twin()
                .and_then(super::self_claimed::alab_twin)
                .unwrap_or_else(|| image.to_path_buf());
            // The posture codex-alab runs at is its argv's over its config's.
            (
                exe,
                codex_lent(&args.argv, codex_config(args, home).as_ref()),
            )
        }
    };
    Some(Starter {
        exe,
        env: (cli.homes)(args, home),
        args: lent,
        unset: Vec::new(),
    })
}

/// What a worker's line names beside its program ([`fn@line`]).
#[derive(Clone, Copy, Debug, Default)]
pub struct Fresh<'a> {
    /// Exact active aterm config; absence never selects shared native credentials.
    pub config: Option<&'a Path>,
    /// `model=`: the CLI's `--model`; `None` runs the row's
    /// [`WorkerCli::default_model`], where it has one.
    pub model: Option<&'a str>,
    /// Whether the shell stands in the folder its starter trusted for it.
    pub trusted_here: bool,
    /// `--permission-mode`: the mode the CLI starts in, one its [`WorkerCli::permission_flag`]
    /// takes, which no floor moves; `None` for the one its starter lends ([`Starter::args`],
    /// held to its CLI's floor, [`WorkerCli::unnamed_posture`]), else the one its settings name.
    pub permission_mode: Option<&'a str>,
    /// Whether the run drives the person's own browser: the CLI's [`WorkerCli::chrome_flag`] (a claim
    /// worker's, [`super::credit_offers::claim_task`]).
    pub chrome: bool,
    /// The only built-in tools the run has, by the CLI's [`WorkerCli::tools_flag`] (a claim worker's,
    /// [`super::credit_offers::CLAIM_TOOLS`]), or `None` for every one its settings give.
    pub tools: Option<&'a [&'a str]>,
    /// A settings document, a JSON object, the run reads above every other by the CLI's
    /// [`WorkerCli::settings_flag`] (a claim worker's auto-mode rules,
    /// [`super::credit_offers::claim_settings`]), or `None` for none.
    pub settings: Option<&'a str>,
}

/// Format the shell command line a worker runs at its first prompt, for the starter [`admit`]
/// admitted.
///
/// # Errors
/// The line exceeds its budget, a carried directory is relative, the starter's argv holds a flag
/// no table knows, `model` starts with `-`, a permission mode is named for a CLI with no such
/// flag or is none of the modes its flag takes, tools are named for a CLI with no such flag or are
/// none or a name that is not one word, a settings document is named for a CLI with no such flag
/// or is no JSON object, or a variable [`Starter::unset`] removes is one its environment sets.
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
    let mode = match (fresh.permission_mode, cli.permission_flag) {
        (None, _) => None,
        (Some(mode), None) => {
            return Err(format!(
                "{} takes no permission mode on its command line, so its worker cannot start in \
                 {mode:?}",
                cli.agent.product()
            ));
        }
        (Some(mode), Some(flag)) if !flag.modes.contains(&mode) => {
            return Err(format!(
                "the permission mode {mode:?} is none of {}'s ({})",
                cli.agent.product(),
                flag.modes.join(", ")
            ));
        }
        (Some(mode), Some(flag)) => Some((flag.flag, mode)),
    };
    let chrome = match (fresh.chrome, cli.chrome_flag) {
        (false, _) => None,
        (true, Some(flag)) => Some(flag),
        (true, None) => {
            return Err(format!(
                "{} has no browser flag, so its worker cannot drive the person's browser",
                cli.agent.product()
            ));
        }
    };
    let tools = match (fresh.tools, cli.tools_flag) {
        (None, _) => None,
        (Some(_), None) => {
            return Err(format!(
                "{} takes no tool list on its command line, so its worker cannot be held to one",
                cli.agent.product()
            ));
        }
        (Some(tools), Some(flag)) => {
            let word = |tool: &&str| {
                !tool.is_empty()
                    && !tool.starts_with('-')
                    && tool.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
            };
            if tools.is_empty() || !tools.iter().all(word) {
                return Err(format!(
                    "the tool list {tools:?} is none, or names a tool that is not one word"
                ));
            }
            Some((flag, tools.join(",")))
        }
    };
    let settings = match (fresh.settings, cli.settings_flag) {
        (None, _) => None,
        (Some(_), None) => {
            return Err(format!(
                "{} takes no settings document on its command line, so its worker cannot be \
                 handed one",
                cli.agent.product()
            ));
        }
        (Some(doc), Some(_)) if !(doc.starts_with('{') && doc.ends_with('}')) => {
            return Err(format!("the settings document {doc:?} is no JSON object"));
        }
        (Some(doc), Some(flag)) => Some((flag, doc)),
    };
    let mut flags: Vec<String> = args.to_vec();
    if let Some(flag) = chrome {
        flags.push(flag.to_string());
    }
    if let Some((flag, tools)) = tools {
        flags.push(flag.to_string());
        flags.push(tools);
    }
    if let Some((flag, doc)) = settings {
        flags.push(flag.to_string());
        flags.push(doc.to_string());
    }
    if let Some(flag) = cli.trust_flag.filter(|_| fresh.trusted_here) {
        flags.push(flag.to_string());
    }
    flags.extend(cli.default_flags.iter().map(|f| (*f).to_string()));
    if let Some(model) = fresh.model.or(cli.default_model) {
        flags.push(cli.model_flag.to_string());
        flags.push(model.to_string());
    }
    if let Some((flag, mode)) = mode {
        flags.push(flag.to_string());
        flags.push(mode.to_string());
    }
    let both: Vec<(&str, &str)> = env
        .iter()
        .copied()
        .chain(cli.nicety.iter().copied())
        .collect();
    let wrapped = match super::native_keys::Provider::for_connector(cli.word) {
        Some(provider) => super::native_keys::launch(provider, exe, &flags, &both, fresh.config)?,
        None => None,
    };
    let (exe, flags) = wrapped.as_ref().map_or((exe, flags.as_slice()), |launch| {
        (launch.program.as_path(), launch.args.as_slice())
    });
    upgrade::task_line_env_clear(dialect, &both, unset, exe, flags, cli.operand)
        .or_else(|_| upgrade::task_line_env_clear(dialect, env, unset, exe, flags, cli.operand))
}

#[cfg(test)]
#[path = "worker_tests.rs"]
mod tests;
