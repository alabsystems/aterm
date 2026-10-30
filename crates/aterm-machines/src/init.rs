// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The local, per-user macOS setup, sharing the machines probe/apply/read-back engine.

use crate::console::Presence;
use crate::engine::{Journal, Place};
use crate::items::defaults::Target;
use crate::items::{Ctx, Item};
use crate::model::{Effect, HostFacts, ItemReport, Mode, Outcome, PValue, Probe, Read};
use crate::port::{Live, System};
use aterm_converge::{Class, Laps, Layer, Remedy, Report, ReportMode, Requires};
use aterm_types::macos_setup::{Access, Selection, Setting};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex, PoisonError};

/// `aterm init` usage, printed by `--help`, after a usage error and as `aterm help init`: what
/// runs by itself, the three opt-ins and the exit meanings, each setting in its System State
/// label ([`Setting::label`]), the one source both surfaces read.
#[must_use]
pub fn usage() -> String {
    // Each flag's settings by label, as many to a line as fit in 90 columns past its indent.
    let rows = |settings: &[Setting]| {
        let mut lines: Vec<String> = Vec::new();
        for label in settings.iter().map(|setting| setting.label()) {
            match lines.last_mut() {
                Some(line) if 16 + line.len() + 2 + label.len() <= 90 => {
                    line.push_str("; ");
                    line.push_str(label);
                }
                _ => lines.push(label.to_owned()),
            }
        }
        lines.join("\n                ")
    };
    format!(
        "\
aterm init [--plan | --check] [--json] [--verbose] [--only <id,...>] [--vault <url|path>]
           [--profile] [--sign-out] [--clone] [--develop | --no-develop] [--no-wait]
aterm init --converge [--check] [--json] [--verbose] [--trigger subscriber|window|setup]
  aterm.app's first launch does this setup by itself; init is for SSH and CLI-only Macs,
  and Linux (the base layer, the vault and --clone; each Mac row reads \"macOS only\").
  Run it as yourself, without sudo; safe to run again. Over SSH too: what `aterm doctor`
  restores, the ALab toolset and the Command Line Tools if missing (one sudo password). Then,
  on this Mac's console account (remote or another login defers; preferences need a person):
    {spotlight}
    {secrets} (open default ALab joins automatically on the LAN; --vault may ask for a password)
    {permissions} (a row for each grant to switch on)
  and these three, all of them unless a flag names the ones to run:
    --profile   {profile}
    --sign-out  {sign_out}
    --clone     {clone}
                (your own, no forks or archived; init.toml [github] scopes or disables it)
  --plan reports, --check only reads, --json prints the report, --verbose every row (else
  the changed, failed and needs-you rows, then a tally); --only runs the ids or families
  named (base, workstation, init.spotlight, ...); --develop records this checkout as a
  develop machine, fetched and provisioned each run, and --no-develop drops it.
  --converge runs one converge pass now, as the subscriber and window do; it asks nothing.
  Exit: 0 done (what waits on an admin or a person at the console included); 75 waits on
  a lock (--no-wait) or a step, 77 a permission, 69 the network; 1 otherwise; 2 usage.
",
        spotlight = Setting::Spotlight.label(),
        secrets = Setting::Secrets.label(),
        permissions = Setting::Permissions.label(),
        profile = rows(&PROFILE),
        sign_out = rows(&SIGN_OUT),
        clone = rows(&[Setting::Repositories]),
    )
}

/// The settings `--profile` applies: every workstation preference.
const PROFILE: [Setting; 6] = [
    Setting::CapsLock,
    Setting::NaturalScrolling,
    Setting::TrackpadScrolling,
    Setting::Desktop,
    Setting::Widgets,
    Setting::Dock,
];

/// The settings `--sign-out` applies.
const SIGN_OUT: [Setting; 2] = [Setting::Messages, Setting::FaceTime];

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Action {
    #[default]
    Apply,
    Plan,
    Check,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct Options {
    action: Action,
    json: bool,
    help: bool,
    /// `--only` words: the items kept ([`aterm_converge::select`]); empty runs all.
    only: Vec<String>,
    /// `--develop` (`Some(true)`) or `--no-develop` (`Some(false)`).
    develop: Option<bool>,
    /// `--vault`: the vault to join when this Mac has none.
    vault: Option<String>,
    /// `--verbose`: print every row as it finishes, not only the changed, failed and needs-you
    /// rows.
    verbose: bool,
    /// The groups `--profile`, `--sign-out` and `--clone` picked; `None` when no flag did, and
    /// every group runs (no question: setup is on by default).
    choice: Option<Choice>,
    /// The `--only` words that name no base or develop item: workstation items, which only a
    /// built workstation knows ([`converge`] refuses a word that names none).
    station: Vec<String>,
    /// `--no-wait`: the base layer stands aside for an install pass holding the store
    /// ([`BaseLock::Try`]) where a run waits for it ([`BaseLock::Wait`]).
    no_wait: bool,
    /// `--converge`: the converge pass ([`crate::sync::converge`]) in place of the setup.
    converge: bool,
    /// `--trigger`: who started a `--converge` pass (a person's `aterm init` without it).
    trigger: Option<crate::sync::Trigger>,
}

impl Options {
    /// What this run's base layer may do with the store lock ([`Host::base`]).
    const fn base_lock(&self, applies: bool) -> BaseLock {
        match (applies, self.no_wait) {
            (false, _) => BaseLock::Read,
            (true, false) => BaseLock::Wait,
            (true, true) => BaseLock::Try,
        }
    }
    /// Whether this run converges the workstation layer.
    fn workstation(&self) -> bool {
        (self.only.is_empty() && self.develop.is_none())
            || !self.station.is_empty()
            || self.only.iter().any(|word| word == "workstation")
    }

    /// Whether this run converges `base.pkg.toolset` alone: every `--only` word is its id and
    /// no `--develop` runs. Its passes are `aterm pkg seed` and `aterm pkg update`, which act on
    /// the store that home's `aterm.toml` names, as they do typed there, so such a run may go
    /// under another home or a private state root. Every other row reaches past the home (the
    /// login's keychain, the Mac's Command Line Tools, the account's preferences).
    fn toolset_alone(&self) -> bool {
        !self.only.is_empty()
            && self.develop.is_none()
            && self.only.iter().all(|word| word == crate::init_toolset::ID)
    }

    /// Whether this run may join a vault: a workstation run its `--only` words, if any, do not
    /// narrow to other items (`--only init.spotlight` looks for no vault).
    fn joins(&self) -> bool {
        self.workstation()
            && (self.only.is_empty()
                || self.only.iter().any(|word| {
                    word.starts_with("init.secrets@")
                        || aterm_converge::selects(
                            word,
                            "init.secrets@",
                            crate::init_secrets::FAMILY,
                            Some(Layer::Workstation),
                        )
                }))
    }
}

/// The workstation groups a run applies: every one, or the ones `--profile`, `--sign-out` and
/// `--clone` name. Nothing asks: first-run setup is on by default and the person turns a part
/// off later (owner ruling 2026-10-25, AGENTS.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Choice {
    /// The workstation preferences ([`PROFILE`]).
    profile: bool,
    /// The Messages and FaceTime sign-outs ([`SIGN_OUT`]).
    sign_out: bool,
    /// GitHub and the repositories ([`Setting::Repositories`]).
    clone: bool,
}

impl Choice {
    /// Every group, the default.
    const ALL: Self = Self {
        profile: true,
        sign_out: true,
        clone: true,
    };

    /// No group.
    const NONE: Self = Self {
        profile: false,
        sign_out: false,
        clone: false,
    };
}

fn options(args: &[OsString]) -> Result<Options, String> {
    let mut options = Options::default();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let only = match arg.to_str() {
            Some("--only") => Some(
                args.next()
                    .and_then(|words| words.to_str())
                    .ok_or("--only takes a comma-separated list of items")?,
            ),
            Some(arg) => arg.strip_prefix("--only="),
            None => None,
        };
        let vault = match arg.to_str() {
            Some("--vault") => Some(
                args.next()
                    .and_then(|source| source.to_str())
                    .ok_or("--vault takes a vault URL or path")?,
            ),
            Some(arg) => arg.strip_prefix("--vault="),
            None => None,
        };
        if let Some(source) = vault {
            // A flag after a bare `--vault` is a missing value, not a vault named `--plan`.
            if source.trim().is_empty() || source.starts_with('-') {
                return Err("--vault takes a vault URL or path".into());
            }
            options.vault = Some(source.to_owned());
            continue;
        }
        let trigger = match arg.to_str() {
            Some("--trigger") => Some(args.next().and_then(|word| word.to_str()).unwrap_or("")),
            Some(arg) => arg.strip_prefix("--trigger="),
            None => None,
        };
        if let Some(word) = trigger {
            options.trigger = Some(
                crate::sync::Trigger::parse(word)
                    .ok_or("--trigger takes subscriber, window, setup or init")?,
            );
            continue;
        }
        if let Some(words) = only {
            let words: Vec<String> = words
                .split(',')
                .map(str::trim)
                .filter(|word| !word.is_empty())
                .map(str::to_owned)
                .collect();
            if words.is_empty() {
                return Err("--only takes a comma-separated list of items".into());
            }
            options.only.extend(words);
            continue;
        }
        match arg.to_str() {
            Some("--help" | "-h") => options.help = true,
            Some("--json") => options.json = true,
            Some("--verbose") => options.verbose = true,
            Some("--no-wait") => options.no_wait = true,
            Some("--converge") => options.converge = true,
            Some(flag @ ("--profile" | "--sign-out" | "--clone")) => {
                let choice = options.choice.get_or_insert(Choice::NONE);
                match flag {
                    "--profile" => choice.profile = true,
                    "--sign-out" => choice.sign_out = true,
                    _ => choice.clone = true,
                }
            }
            Some(flag @ ("--develop" | "--no-develop")) => {
                let develop = flag == "--develop";
                if options.develop.is_some_and(|asked| asked != develop) {
                    return Err("choose either --develop or --no-develop".into());
                }
                options.develop = Some(develop);
            }
            Some("--plan" | "--check") => {
                let action = if arg == "--plan" {
                    Action::Plan
                } else {
                    Action::Check
                };
                if options.action != Action::Apply && options.action != action {
                    return Err("choose either --plan or --check".into());
                }
                options.action = action;
            }
            _ => return Err(format!("unknown argument {arg:?}")),
        }
    }
    if options.trigger.is_some() && !options.converge {
        return Err("--trigger names who started a --converge pass".into());
    }
    if options.converge
        && (options.action == Action::Plan
            || !options.only.is_empty()
            || options.vault.is_some()
            || options.choice.is_some()
            || options.develop.is_some()
            || options.no_wait)
    {
        return Err("--converge takes only --check, --json, --verbose and --trigger".into());
    }
    if options.develop.is_some() && options.only.iter().any(|word| word == "workstation") {
        return Err("--develop and --no-develop cannot be combined with --only workstation".into());
    }
    if options.choice.is_some() && (options.develop.is_some() || !options.only.is_empty()) {
        return Err(
            "--profile, --sign-out and --clone pick within the whole run; --only and --develop \
             narrow it: use one"
                .into(),
        );
    }
    Ok(options)
}

/// Refuse `--vault` for a run that leaves the workstation layer out, once [`Options::station`]
/// is known.
fn vault_reaches(options: &Options) -> Result<(), String> {
    if options.vault.is_some() && !options.workstation() {
        return Err("--vault joins in the workstation layer, which this run leaves out".into());
    }
    Ok(())
}

/// Base layer `aterm init` runs before its own items.
pub struct Base {
    /// The items, in run order.
    pub items: Vec<Box<dyn aterm_converge::Item>>,
    /// Store lock held while base items run and released before `base.clt`, `base.pkg.toolset`
    /// and workstation items.
    pub held: Option<Box<dyn std::any::Any>>,
    /// `base.clt` ([`crate::init_clt`]), run once the lock is released.
    pub clt: Option<crate::init_clt::CommandLineTools>,
    /// `base.pkg.toolset` ([`crate::init_toolset`]), run once the lock is released (its passes
    /// take it) and after `base.clt`.
    pub toolset: Option<crate::init_toolset::Toolset>,
    /// Toolset `targo` used by `develop.workspace-tools@<path>` ([`crate::init_develop::Targo`]).
    pub targo: Option<crate::init_develop::Targo>,
}

impl Base {
    /// No base items.
    #[must_use]
    pub fn none() -> Self {
        Self {
            items: Vec::new(),
            held: None,
            clt: None,
            toolset: None,
            targo: None,
        }
    }

    /// Return `--only` selectors that match none of these items (`aterm_converge::selects`).
    #[must_use]
    pub fn unnamed(&self, only: &[String]) -> Vec<String> {
        only.iter()
            .filter(|word| {
                !self.items.iter().any(|item| {
                    aterm_converge::selects(word, &item.id(), item.family(), Some(item.layer()))
                }) && !self
                    .clt
                    .as_ref()
                    .is_some_and(|_| crate::init_clt::selected(word))
                    && !self
                        .toolset
                        .as_ref()
                        .is_some_and(|_| crate::init_toolset::selected(word))
            })
            .cloned()
            .collect()
    }

    /// These items narrowed to `only` ([`aterm_converge::select`]).
    #[must_use]
    pub fn narrowed(self, only: &[String]) -> Self {
        if only.is_empty() {
            return self;
        }
        let Self {
            items,
            held,
            clt,
            toolset,
            targo,
        } = self;
        Self {
            items: aterm_converge::select(items, only).0,
            held,
            clt: clt.filter(|_| only.iter().any(|word| crate::init_clt::selected(word))),
            toolset: toolset
                .filter(|_| only.iter().any(|word| crate::init_toolset::selected(word))),
            targo,
        }
    }
}

/// What the base layer's run may do with the store lock ([`Host::base`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BaseLock {
    /// A plan or a check: no lock, nothing changed.
    Read,
    /// An apply, which meets an install pass holding the store by waiting for it (a person at
    /// the terminal) or by leaving the pass's own items to it.
    Wait,
    /// `--no-wait`'s apply: each item stands aside for an install pass holding the store
    /// (exit 75).
    Try,
}

/// What `aterm init` takes from the `aterm` that runs it ([`main_entry`]).
pub struct Host<'a> {
    /// The base layer ([`Base`]), its store lock as [`BaseLock`] says.
    pub base: &'a dyn Fn(BaseLock) -> Base,
    /// Whether this process holds Full Disk Access, as the runner measures it.
    pub full_disk_access: &'a dyn Fn() -> Option<bool>,
    /// This Mac's setup records, shared with the first launch ([`Records`]).
    pub records: &'a dyn Records,
    /// aterm's macOS grants, as the Permissions phase reads them ([`Grants`]).
    pub grants: Grants,
    /// Hand the grants the Permissions phase left to the person to the running aterm window's
    /// permission walk (`permissions walk now <grants>`), for a person at this Mac's console,
    /// Full Disk Access and Accessibility only when that window is the app macOS holds
    /// responsible for the run (they are that app's grants, [`crate::port::responsible_window`]):
    /// the line to print once a walk takes them, `None` where none does (no window, no person
    /// here), and the rows' own words stand.
    pub walk: &'a dyn Fn(&[Access]) -> Option<String>,
    /// `--converge`'s machine: the converge pass reads and changes it only through these
    /// ([`crate::sync::Ports`]).
    pub converge: &'a dyn crate::sync::Ports,
}

/// This Mac's setup records (`profile.lock` and the initial-setup record), which `aterm init`
/// shares with the first launch's setup and `aterm doctor --apply`: a pass that changes the
/// workstation holds them, so two surfaces never relaunch the Dock over each other, and a whole
/// profile it verified is recorded, so the first launch neither runs nor offers it again.
pub trait Records: Sync {
    /// Hold the records for a workstation pass, waiting while another setup holds them
    /// (`wait` hears one line as that wait starts). `whole`: the pass applies every preference
    /// and both sign-outs, recorded as the plan the way `aterm doctor --apply` records it.
    ///
    /// # Errors
    /// The records cannot be held or written; nothing was changed.
    fn hold(&self, whole: bool, wait: &dyn Fn(&str)) -> Result<Box<dyn Held>, String>;
}

/// A hold on the setup records ([`Records::hold`]), let go when dropped.
pub trait Held {
    /// Retain the selected preference items left for a later permission grant.
    ///
    /// # Errors
    /// The record cannot be written; the preference pass must not start.
    fn withheld(&self, _ids: &[String]) -> Result<(), String> {
        Ok(())
    }

    /// Retain fully verified Mac stages of a whole-profile pass, even if another
    /// stage failed. `settings` covers each completed stage in full.
    ///
    /// # Errors
    /// The completion record cannot be written.
    fn checkpoint(&self, _settings: Selection) -> Result<(), String> {
        Ok(())
    }

    /// Record the whole profile verified, as `aterm doctor --apply` records its pass; only for a
    /// hold taken `whole`.
    ///
    /// # Errors
    /// The record cannot be written.
    fn verified(self: Box<Self>) -> Result<(), String>;
}

/// No setup records: a test's run, which shares nothing with a first launch.
pub struct NoRecords;

impl Records for NoRecords {
    fn hold(&self, _whole: bool, _wait: &dyn Fn(&str)) -> Result<Box<dyn Held>, String> {
        Ok(Box::new(NoRecords))
    }
}

impl Held for NoRecords {
    fn verified(self: Box<Self>) -> Result<(), String> {
        Ok(())
    }
}

/// aterm's macOS grants as `aterm init`'s Permissions phase reads them, without a prompt: each
/// of [`Access::ALL`] the phase lists, for a run that signs out (`true`, which needs
/// Accessibility too).
pub type Grants = fn(bool) -> Vec<(Access, Grant)>;

/// How one of aterm's macOS grants reads ([`Grants`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Grant {
    /// On.
    On,
    /// Only the person can switch it on: what to do, in one line.
    NeedsYou(String),
    /// Not needed on this Mac or for this run, saying why.
    NotNeeded(String),
    /// It could not be read, saying why.
    Unread(String),
}

/// No grant read: a run that lists none.
#[must_use]
pub fn no_grants(_sign_out: bool) -> Vec<(Access, Grant)> {
    Vec::new()
}

/// Why `aterm init` refuses a run under another home or a private state root that is not
/// [`Options::toolset_alone`].
const AWAY_FROM_HOME: &str = "run in your own login and home directory; under another home \
                              or a private state root only `--only base.pkg.toolset` runs, \
                              in that home";

/// Run `aterm init`. Every row carries its time, the JSON's `phases` the run's (this login's
/// context, the questions, the base and workstation layers), and one aterm.log line says the
/// same totals.
pub fn main_entry(args: Vec<OsString>, host: &Host<'_>) -> ExitCode {
    let mut options = match options(&args) {
        Ok(options) => options,
        Err(why) => {
            eprintln!("aterm init: {why}\n{}", usage());
            return ExitCode::from(2);
        }
    };
    if options.help {
        print!("{}", usage());
        return ExitCode::SUCCESS;
    }
    if options.converge {
        return converge_entry(&options, host.converge);
    }
    let mut laps = Laps::start();
    let composed = (host.base)(BaseLock::Read);
    // A word no base item or develop selector names is a workstation word, which only a built
    // workstation knows ([`converge`]); a develop run builds none.
    let unnamed = unnamed(&composed, &options.only);
    if !unnamed.is_empty() && options.develop.is_some() {
        eprintln!(
            "aterm init: --only {} names no item ({ONLY_WORDS})\n{}",
            unnamed.join(","),
            usage()
        );
        return ExitCode::from(2);
    }
    options.station = unnamed;
    if let Err(why) = vault_reaches(&options) {
        eprintln!("aterm init: {why}\n{}", usage());
        return ExitCode::from(2);
    }
    if !crate::sessions::is_live_account() && !options.toolset_alone() {
        eprintln!("aterm init: {AWAY_FROM_HOME}");
        return ExitCode::FAILURE;
    }
    // The look for the ALab vault overlaps reading the Mac (its context and the develop
    // layer); the secrets step takes its answer as the workstation groups are prepared, which
    // is before the base layer runs, so every question comes before any change.
    let ahead = (options.action == Action::Apply && options.joins() && options.vault.is_none())
        .then(crate::init_secrets::look_ahead)
        .flatten();
    let groups = |sys: &dyn System, ctx: &Ctx, mode: Mode, reading: &Reading<'_>| {
        prepare_groups(
            sys,
            ctx,
            mode,
            ahead.clone(),
            options.vault.as_deref(),
            reading,
            host.grants,
        )
    };
    let sys = Live::new().with_full_disk_access(if options.workstation() {
        (host.full_disk_access)()
    } else {
        None
    });
    let cwd = std::env::current_dir().ok();
    let printer = Printer::new(options.verbose, options.json);
    let result = sys
        .journal()
        .map_err(Refused::Failed)
        .and_then(|mut journal| {
            converge(
                &sys,
                &options,
                &Front {
                    base: &|applies| (host.base)(options.base_lock(applies)),
                    groups: &groups,
                    targo: composed.targo.clone(),
                    cwd: cwd.as_deref(),
                    records: host.records,
                    on_row: &|row| printer.row(row),
                },
                journal.as_mut(),
                &mut laps,
            )
        });
    printer.flush();
    let rows = match result {
        Ok(rows) => rows,
        Err(Refused::Usage(why)) => {
            eprintln!("aterm init: {why}\n{}", usage());
            return ExitCode::from(2);
        }
        Err(Refused::Failed(why)) => {
            aterm_log::info!("aterm init: no report; {laps}");
            eprintln!("aterm init: {why}");
            return ExitCode::FAILURE;
        }
    };
    let complete = complete(&rows, options.action);
    let code = status(&rows, options.action);
    aterm_log::info!("aterm init: {} rows, exit {code}; {laps}", rows.len());
    if options.json {
        match report_json(options.action, complete, rows, &laps) {
            Ok(report) => println!("{report}"),
            Err(why) => {
                eprintln!("aterm init: cannot encode report: {why}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        println!("{}", tally(&rows, options.action));
        let missing = walk_grants(&rows);
        if options.action == Action::Apply
            && !missing.is_empty()
            && let Some(line) = (host.walk)(&missing)
        {
            println!("aterm init: {line}");
        }
    }
    ExitCode::from(code)
}

/// `aterm init --converge`: one converge pass ([`crate::sync::converge`]) over `ports`, which
/// it reaches only through them (no workstation item, question or setup record), at background
/// QoS. `--check` probes and writes nothing. Its rows print as init's do, then a tally (or the
/// JSON report). Exit 0 once it ran with no failed row (what it defers included), 75 while
/// another pass holds the lock, 1 for a failed row, an unwritten record or a pass that cannot
/// run here. A subscriber's pass from a program that is not the installed release app (a held
/// copy) is the installed app's to run ([`hand_off`]).
fn converge_entry(options: &Options, ports: &dyn crate::sync::Ports) -> ExitCode {
    ports.background();
    let trigger = options.trigger.unwrap_or(crate::sync::Trigger::Init);
    if trigger == crate::sync::Trigger::Subscriber
        && let Some(program) = ports.installed_release_elsewhere()
        && let Some(code) = hand_off(options, ports, &program)
    {
        return code;
    }
    let laps = Laps::start();
    let (mode, action) = if options.action == Action::Check {
        (Mode::Plan, Action::Check)
    } else {
        (Mode::Apply, Action::Apply)
    };
    let printer = Printer::new(options.verbose, options.json);
    let pass = crate::sync::converge(ports, trigger, mode, &mut |row| printer.row(row));
    printer.flush();
    let pass = match pass {
        Ok(pass) => pass,
        Err(crate::sync::Stop::Held) => {
            eprintln!(
                "aterm init: another converge pass holds {}/{}; this one did not run",
                aterm_converge::FileJournal::DIR,
                crate::sync::LOCK
            );
            return ExitCode::from(75);
        }
        Err(crate::sync::Stop::Refused(why)) => {
            eprintln!("aterm init: {why}");
            return ExitCode::FAILURE;
        }
    };
    let failed = pass.rows.iter().any(|row| row.outcome.is_failure());
    let complete = !pass.rows.is_empty() && !failed;
    if options.json {
        match report_json(action, complete, pass.rows, &laps) {
            Ok(report) => println!("{report}"),
            Err(why) => {
                eprintln!("aterm init: cannot encode report: {why}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        println!(
            "{} \u{b7} converge {}/{}",
            tally(&pass.rows, action),
            trigger.word(),
            pass.due.word()
        );
    }
    if let Some(why) = &pass.unrecorded {
        eprintln!("aterm init: the pass's record was not written: {why}");
    }
    if failed || pass.unrecorded.is_some() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// Run the subscriber's pass from `program`, the installed release app, with this run's flags,
/// and end as it ends: a held copy a vault subscriber's job runs (the snapshot an admin Mac
/// keeps, a program `agent on --program` named) then applies the installed app's rows, as the
/// window's pass does, so two builds never take turns undoing each other's writes and a fix in
/// a later app reaches the subscriber's side. The installed app's run lowers its own QoS and
/// writes its own ledger line. `None` where it could not be started or waited for, or ended
/// with a usage error (an installed app older than this copy, with no such pass; each said in
/// aterm.log): this copy runs the pass itself, as before.
fn hand_off(options: &Options, ports: &dyn crate::sync::Ports, program: &Path) -> Option<ExitCode> {
    let mut args = ["init", "--converge", "--trigger"]
        .map(str::to_owned)
        .to_vec();
    args.push(crate::sync::Trigger::Subscriber.word().to_owned());
    if options.action == Action::Check {
        args.push("--check".to_owned());
    }
    if options.json {
        args.push("--json".to_owned());
    }
    if options.verbose {
        args.push("--verbose".to_owned());
    }
    match ports.run_elsewhere(program, &args) {
        Ok(Some(2)) => {
            ports.log(&format!(
                "converge trigger=subscriber: the installed app ({}) does not take the pass \
                 (an app older than this copy); this copy runs it",
                program.display()
            ));
            None
        }
        Ok(Some(code)) => Some(ExitCode::from(u8::try_from(code).unwrap_or(1))),
        Ok(None) => {
            ports.log(&format!(
                "converge trigger=subscriber: the installed app's pass ({}) ended by a signal",
                program.display()
            ));
            Some(ExitCode::FAILURE)
        }
        Err(why) => {
            ports.log(&format!(
                "converge trigger=subscriber: the installed app's pass ({}) did not start \
                 ({why}); this copy runs it",
                program.display()
            ));
            None
        }
    }
}

/// Whether this login owns this Mac's console, as `aterm init`'s context reads it
/// (`ctx.console`; no keyboard list): the converge pass's [`aterm_converge::Scope::at_console`].
///
/// # Errors
/// This process runs as root, or its account cannot be read or names another uid.
pub fn console_owned(sys: &dyn System) -> Result<bool, String> {
    context_reading(sys, WorkstationGroup::Accounts, Selection::all())
        .map(|(ctx, _)| ctx.console.is_some())
}

/// The grants the Permissions phase left to the person ([`is_grant`]), in its row order: what
/// `aterm init` hands to the window's permission walk after its tally.
fn walk_grants(rows: &[ItemReport]) -> Vec<Access> {
    rows.iter()
        .filter(|row| is_grant(row))
        .filter_map(|row| {
            let word = row.id.strip_prefix(PERMISSION)?;
            Access::ALL.into_iter().find(|access| access.word() == word)
        })
        .collect()
}

/// How `aterm init` prints each row as it finishes ([`Front::on_row`]): the changed, failed and
/// needs-you rows, or with `--verbose` every row, each with its id; none with `--json`, which
/// prints the whole report at the end. A row that finishes while another lane's question or
/// attached program (a join password, `sudo`) owns the terminal waits for it
/// ([`crate::port::terminal_free`]), so no row lands inside a prompt.
struct Printer {
    verbose: bool,
    json: bool,
    /// Rendered rows not printed yet.
    later: Mutex<Vec<String>>,
}

impl Printer {
    fn new(verbose: bool, json: bool) -> Self {
        Self {
            verbose,
            json,
            later: Mutex::new(Vec::new()),
        }
    }

    fn row(&self, row: &ItemReport) {
        self.row_to(row, &mut std::io::stdout().lock());
    }

    /// Print what is held back once the run is over.
    fn flush(&self) {
        self.flush_to(&mut std::io::stdout().lock());
    }

    /// Write `row`, with every row held back before it, to `out`, unless a question or an
    /// attached program owns the terminal; then hold it back too.
    fn row_to(&self, row: &ItemReport, out: &mut dyn std::io::Write) {
        if self.json || !(self.verbose || shown(row)) {
            return;
        }
        let mut later = self.later.lock().unwrap_or_else(PoisonError::into_inner);
        later.push(rendered(row));
        // The terminal stays the printer's while it writes, so no prompt starts mid-row.
        if let Some(_free) = crate::port::terminal_free() {
            Self::write(&mut later, out);
        }
    }

    fn flush_to(&self, out: &mut dyn std::io::Write) {
        Self::write(
            &mut self.later.lock().unwrap_or_else(PoisonError::into_inner),
            out,
        );
    }

    /// One write for `rows`, which it empties: the lanes finish rows at the same time.
    fn write(rows: &mut Vec<String>, out: &mut dyn std::io::Write) {
        if rows.is_empty() {
            return;
        }
        let _ = out.write_all(rows.concat().as_bytes());
        let _ = out.flush();
        rows.clear();
    }
}

/// Whether the default output prints `row`: a change, a failure, or what a person or another
/// step still has to do; the rows already right (or not for this Mac) only count in the tally.
fn shown(row: &ItemReport) -> bool {
    !matches!(row.outcome, Outcome::Ok | Outcome::NotApplicable)
}

/// `row` as `aterm init` prints it: its word, title and id, then what changed or why.
fn rendered(row: &ItemReport) -> String {
    use std::fmt::Write as _;
    let word = if waiting(row) {
        "waiting"
    } else {
        word(row.outcome)
    };
    let mut text = format!("{word}: {} ({})\n", row.title, row.id);
    if let (Some(from), Some(to)) = (&row.from, &row.to) {
        let _ = writeln!(text, "    {from} \u{2192} {to}");
    } else if let (Outcome::Ok, Some(Layer::Develop), Some(now)) = (row.outcome, row.layer, &row.to)
    {
        let _ = writeln!(text, "    {now}");
    }
    if let Some(why) = &row.why {
        let _ = writeln!(text, "    {why}");
    }
    text
}

/// The one line `aterm init` ends with: how many rows read right, changed, failed or wait, and
/// the first that needs the person, by name.
fn tally(rows: &[ItemReport], action: Action) -> String {
    let count = |pick: fn(&ItemReport) -> bool| rows.iter().filter(|row| pick(row)).count();
    let mut parts = vec![format!(
        "{} ok",
        count(|row| matches!(row.outcome, Outcome::Ok | Outcome::NotApplicable))
    )];
    for (n, what) in [
        (count(|row| row.outcome == Outcome::Changed), "changed"),
        (count(|row| row.outcome == Outcome::Planned), "to change"),
        (
            count(|row| matches!(row.outcome, Outcome::Failed | Outcome::Unknown)),
            "failed",
        ),
        (
            count(waits_by_itself),
            "waiting to be let in (it finishes by itself)",
        ),
        (count(waits_on_a_step), "waiting on another step"),
    ] {
        if n > 0 {
            parts.push(format!("{n} {what}"));
        }
    }
    let needs: Vec<&ItemReport> = rows.iter().filter(|row| needs_you(row)).collect();
    if let Some(first) = needs.first() {
        parts.push(format!("{} needs you: {}", needs.len(), first.title));
    }
    if rows
        .iter()
        .any(|row| row.why.as_deref() == Some(aterm_secrets::swarm::SWARM_OFF))
    {
        parts.push("[swarm] enabled = false".to_owned());
    }
    let tail = if action == Action::Plan {
        " (nothing changed)"
    } else {
        ""
    };
    format!("aterm init: {}{tail}", parts.join(" \u{b7} "))
}

/// Whether `row` waits on something other than a person: a lock, the network, or another row
/// that does ([`waits_by_itself`] apart).
fn waits_on_a_step(row: &ItemReport) -> bool {
    row.outcome == Outcome::Blocked
        && !waits_by_itself(row)
        && row
            .stood_aside
            .as_ref()
            .is_some_and(|aside| !matches!(aside, aterm_converge::StoodAside::Permission(_)))
}

/// Whether only the person can finish `row`: a step of theirs, a grant, or a precondition no
/// other step lifts.
fn needs_you(row: &ItemReport) -> bool {
    matches!(row.outcome, Outcome::Person | Outcome::Deferred)
        || (row.outcome == Outcome::Blocked && !waits_by_itself(row) && !waits_on_a_step(row))
}

/// The develop layer as `aterm doctor` reads it on this Mac ([`crate::init_develop::doctor`]),
/// for the user `aterm init --develop` would converge, in the context init builds when it leaves
/// the workstation out (root refused). The context is built only once the account's own
/// `init.toml` has a `[develop]` table or cannot be read, or an `--only` word asks
/// ([`crate::init_develop::may_record`]), so on a Mac that records no develop layer doctor runs
/// nothing for it (no account reading) and shows no row. The caller asks only in the
/// account's own shell.
pub struct Doctor<S: System = Live> {
    sys: S,
    /// The account's home as its account record names it (`getpwuid`), where its `init.toml`
    /// is read first; `None` builds the context straight away.
    home: Option<PathBuf>,
    targo: Option<crate::init_develop::Targo>,
    ctx: std::sync::OnceLock<Result<Ctx, String>>,
    /// This login's rust-docs item ([`Self::rust_docs`]), built once.
    docs: std::sync::OnceLock<Result<Box<dyn Item>, String>>,
}

impl Doctor {
    /// Nothing read yet; `targo` is the toolset's ([`crate::init_develop::Targo`]).
    #[must_use]
    pub fn new(targo: Option<crate::init_develop::Targo>) -> Self {
        Self::over(Live::new(), aterm_uds::account::home(), targo)
    }
}

impl<S: System> Doctor<S> {
    /// [`Doctor::new`] over `sys`, the account's home `home`.
    fn over(sys: S, home: Option<PathBuf>, targo: Option<crate::init_develop::Targo>) -> Self {
        Self {
            sys,
            home,
            targo,
            ctx: std::sync::OnceLock::new(),
            docs: std::sync::OnceLock::new(),
        }
    }

    /// This login's `init.spotlight.rust-docs@<user>` item ([`crate::init_spotlight`]) bound as
    /// a relay-class repair of the account's own, for bare `aterm doctor` to apply once Spotlight
    /// is found indexing rust-docs pages: the context and the item are built only on this call,
    /// so a Mac whose docs are out of Spotlight runs nothing for it.
    ///
    /// # Errors
    /// This login's context cannot be read, or it resolves no one user.
    pub fn rust_docs(&self) -> Result<Vec<Box<dyn aterm_converge::Item + '_>>, String> {
        let ctx = self.ctx().as_ref().map_err(Clone::clone)?;
        let item = self
            .docs
            .get_or_init(|| crate::init_spotlight::rust_docs_item(ctx))
            .as_ref()
            .map_err(Clone::clone)?;
        Ok(crate::engine::bind(
            [(
                item.as_ref(),
                Place {
                    layer: Layer::Workstation,
                    class: Class::Relay,
                    requires: Requires::Account,
                    remedy: Remedy::Command("aterm doctor".into()),
                },
            )],
            &self.sys,
            ctx,
        ))
    }

    fn ctx(&self) -> &Result<Ctx, String> {
        self.ctx.get_or_init(|| {
            context_for_selected(&self.sys, WorkstationGroup::All, Selection::none())
        })
    }

    /// The layer for this run, reading the profile alone (`asked`: an `--only` word named a
    /// develop item, so a Mac that records none reads its record row). A context that cannot
    /// be built is one `develop.record` row saying why.
    #[must_use]
    pub fn compose(&self, asked: bool) -> crate::init_develop::Composed {
        if !asked
            && self.home.as_deref().is_some_and(|home| {
                !crate::init_develop::may_record(&self.sys, self.sys.euid(), home)
            })
        {
            return crate::init_develop::Composed::default();
        }
        match self.ctx() {
            Ok(ctx) => crate::init_develop::doctor(&self.sys, ctx, asked, self.targo.clone()),
            Err(why) => crate::init_develop::Composed::unreadable(format!(
                "cannot tell whose develop layer this is: {why}"
            )),
        }
    }

    /// The rows of `composed`, narrowed to `only` ([`crate::init_develop::read`]); none, and
    /// nothing run, when it holds no item.
    ///
    /// # Errors
    /// As [`crate::init_develop::read`].
    pub fn rows(
        &self,
        composed: &crate::init_develop::Composed,
        only: &[String],
    ) -> Result<Vec<ItemReport>, String> {
        if composed.items.is_empty() {
            return Ok(Vec::new());
        }
        let nobody = Ctx {
            facts: HostFacts::default(),
            users: Vec::new(),
            console: None,
            host_name: String::new(),
            cache_dir: PathBuf::new(),
            disrupt: false,
        };
        let ctx = self.ctx().as_ref().unwrap_or(&nobody);
        crate::init_develop::read(&self.sys, ctx, composed, only)
    }
}

/// What `--only` takes, for its refusal.
const ONLY_WORDS: &str = "a base id such as base.clt or base.shell.rc, a family such as shell \
                          or pkg, or base; a workstation id such as init.spotlight or \
                          init.trackpad, a family such as keyboard, or workstation; a develop id \
                          such as develop.checkout@~/aterm, a family such as checkout, or \
                          develop";

/// Why `aterm init` refuses `line` (`aterm init …`, as a remedy prints it:
/// [`aterm_converge::Remedy::command`]) before it changes anything, with `base` the base layer
/// and `station` the workstation's items: its arguments do not parse, or an `--only` word names
/// nothing init runs ([`ONLY_WORDS`]). `None` when init takes it.
#[cfg(any(test, debug_assertions))]
#[must_use]
pub fn refusal(line: &str, base: &Base, station: &[Box<dyn Item>]) -> Option<String> {
    let Some(args) = line.strip_prefix("aterm init") else {
        return Some(format!("{line:?} is not an `aterm init` line"));
    };
    let args: Vec<OsString> = args.split_whitespace().map(OsString::from).collect();
    let options = match options(&args) {
        Ok(options) => options,
        Err(why) => return Some(why),
    };
    let unknown: Vec<String> = unnamed(base, &options.only)
        .into_iter()
        .filter(|word| {
            !station.iter().any(|item| {
                aterm_converge::selects(word, &item.id(), item.family(), Some(Layer::Workstation))
            })
        })
        .collect();
    (!unknown.is_empty())
        .then(|| format!("--only {} names no item ({ONLY_WORDS})", unknown.join(",")))
}

/// Each `aterm init …` command `text` names, as far as its flags go (`--only` and `--vault`
/// with their value), its value's trailing punctuation and backtick cut: what a person copies
/// from a printed remedy into [`refusal`].
#[cfg(any(test, debug_assertions))]
#[must_use]
pub fn init_lines(text: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find("aterm init") {
        let mut line = String::from("aterm init");
        let cut = |word: &str| word.trim_end_matches(['`', ')', ',', ';', '.', ':']).len();
        let mut words = rest[at + "aterm init".len()..].split_whitespace();
        while let Some(word) = words.next() {
            let flag = &word[..cut(word)];
            if !flag.starts_with("--") {
                break;
            }
            line.push(' ');
            line.push_str(flag);
            if flag.len() < word.len() {
                break;
            }
            if matches!(flag, "--only" | "--vault") {
                let Some(value) = words.next() else {
                    break;
                };
                let kept = &value[..cut(value)];
                line.push(' ');
                line.push_str(kept);
                if kept.len() < value.len() {
                    break;
                }
            }
        }
        lines.push(line);
        rest = &rest[at + "aterm init".len()..];
    }
    lines
}

/// Return `--only` words that match no base item, `workstation`, or develop selector.
fn unnamed(base: &Base, only: &[String]) -> Vec<String> {
    base.unnamed(only)
        .into_iter()
        .filter(|word| word != "workstation" && !crate::init_develop::names(word))
        .collect()
}

/// Why a run did not start.
#[derive(Debug, PartialEq, Eq)]
enum Refused {
    /// A usage error: exit 2.
    Usage(String),
    /// Anything else: exit 1.
    Failed(String),
}

impl From<String> for Refused {
    fn from(why: String) -> Self {
        Self::Failed(why)
    }
}

/// Front-door inputs for a converge pass.
struct Front<'a> {
    base: &'a dyn Fn(bool) -> Base,
    groups: &'a Prepare<'a>,
    targo: Option<crate::init_develop::Targo>,
    cwd: Option<&'a Path>,
    /// This Mac's setup records, held over the Desktop lane of a pass that changes preferences.
    records: &'a dyn Records,
    /// Hears each row as its item finishes, from whichever lane ran it ([`run_layers`]).
    on_row: &'a (dyn Fn(&ItemReport) + Sync),
}

#[cfg(any(test, debug_assertions))]
type TestWorkstation<'a> = dyn Fn(&Ctx) -> Result<Vec<Vec<Box<dyn Item>>>, String> + 'a;

/// The actual CLI convergence and records handoff, with inert workstation groups supplied by
/// another crate's tests. The base layer is empty; the injected system supplies every read and
/// effect of the workstation items.
///
/// # Errors
/// As the CLI's context, item composition, record hold or convergence.
#[cfg(any(test, debug_assertions))]
pub fn converge_workstation_for_test(
    sys: &dyn System,
    records: &dyn Records,
    only: &[String],
    prepare: &TestWorkstation<'_>,
) -> Result<Vec<ItemReport>, String> {
    let groups = |_: &dyn System, ctx: &Ctx, _: Mode, _: &Reading<'_>| {
        Ok((
            prepare(ctx)?
                .into_iter()
                .map(|items| (items, workstation_place(Class::Preference)))
                .collect(),
            Choice::ALL,
        ))
    };
    converge(
        sys,
        &Options {
            only: only.to_vec(),
            station: only.to_vec(),
            ..Options::default()
        },
        &Front {
            base: &|_| Base::none(),
            groups: &groups,
            targo: None,
            cwd: None,
            records,
            on_row: &|_| {},
        },
        &mut crate::engine::NoJournal,
        &mut Laps::start(),
    )
    .map_err(|refused| match refused {
        Refused::Usage(why) | Refused::Failed(why) => why,
    })
}

/// Initialization keeps agent ancestry when a tool call has no controlling terminal.
fn init_presence(sys: &dyn System) -> Presence {
    match crate::console::presence(sys) {
        Presence::NoTerminal if sys.agent_ancestor().is_some() => Presence::AgentTab,
        presence => presence,
    }
}

/// Converge the requested base, workstation, and develop layers, each step a phase of `laps`:
/// this login's context (and the develop layer's profile), the questions asked before any
/// change (the vault's join password), then the layers ([`run_layers`]).
fn converge(
    sys: &dyn System,
    options: &Options,
    front: &Front<'_>,
    journal: &mut (dyn Journal + Send),
    laps: &mut Laps,
) -> Result<Vec<ItemReport>, Refused> {
    let mode = if options.action == Action::Apply {
        Mode::Apply
    } else {
        Mode::Plan
    };
    // Preferences and repository setup remain a person's picks. Account sign-out also runs in
    // the console user's graphic session with no terminal, but never from SSH or an agent tab.
    let presence = init_presence(sys);
    let restricted = options.choice.is_some_and(|choice| {
        choice.profile
            || choice.clone
            || (choice.sign_out && !matches!(presence, Presence::AtConsole | Presence::NoTerminal))
    });
    if restricted
        && sys.macos()
        && let Some(why) = presence.why()
    {
        return Err(Refused::Usage(format!(
            "--profile, --sign-out and --clone are picks for a person at this Mac's console \
             ({why}): run them there, or leave them out"
        )));
    }
    let only = &options.only;
    let workstation = options.workstation();
    let (ctx, keyboards) = if workstation {
        local_context(sys)?
    } else {
        (
            context_for_selected(sys, WorkstationGroup::All, Selection::none())?,
            None,
        )
    };
    let asked = only.iter().any(|word| crate::init_develop::names(word));
    let develop = if options.develop.is_some() || only.is_empty() || asked {
        crate::init_develop::layer(
            sys,
            &ctx,
            &crate::init_develop::Inputs {
                want: options.develop,
                asked,
                cwd: front.cwd,
                targo: front.targo.clone(),
            },
        )
    } else {
        crate::init_develop::Composed::default()
    };
    let nothing: Vec<&str> = only
        .iter()
        .filter(|word| crate::init_develop::names(word))
        .filter(|word| {
            !develop.items.iter().any(|(item, place)| {
                aterm_converge::selects(word, &item.id(), item.family(), Some(place.layer))
            })
        })
        .map(String::as_str)
        .collect();
    if !nothing.is_empty() && !develop.unreadable {
        let checkouts: Vec<String> = develop
            .items
            .iter()
            .map(|(item, _)| item.id())
            .filter(|id| id.starts_with("develop.checkout@"))
            .collect();
        return Err(Refused::Usage(format!(
            "--only {} names no develop item of this run ({})",
            nothing.join(","),
            if checkouts.is_empty() {
                "init.toml's [develop] records no checkout; `aterm init --develop` records one"
                    .to_owned()
            } else {
                format!("its checkouts: {}", checkouts.join(", "))
            }
        )));
    }
    laps.lap("context");
    let (groups, choice) = if workstation {
        (front.groups)(
            sys,
            &ctx,
            mode,
            &Reading {
                keyboards: keyboards.as_deref(),
                only,
                choice: options.choice,
            },
        )?
    } else {
        (Vec::new(), Choice::NONE)
    };
    // A workstation word names an item of the workstation this run built; away from a Mac's
    // console one row stands for every one of them.
    let unknown: Vec<&str> = options
        .station
        .iter()
        .filter(|word| {
            (ctx.console.is_some() || !sys.macos())
                && !groups.iter().any(|(items, place)| {
                    items.iter().any(|item| {
                        aterm_converge::selects(word, &item.id(), item.family(), Some(place.layer))
                    })
                })
        })
        .map(String::as_str)
        .collect();
    if !unknown.is_empty() {
        return Err(Refused::Usage(format!(
            "--only {} names no item ({ONLY_WORDS})",
            unknown.join(",")
        )));
    }
    // A pass that changes the preferences or signs out holds the setup records the first
    // launch holds for the same pass, over the Desktop lane alone: the base layer, the vault and
    // the clones never wait behind a first-launch pass, and System State's presses and the
    // subscriber's repair wait only while preferences change. A whole profile the lane verified
    // is recorded for the first launch.
    let whole = choice.profile && choice.sign_out && only.is_empty();
    let holds = mode == Mode::Apply
        && groups.iter().any(|(items, place)| {
            place.class == Class::Preference
                && items.iter().any(|item| {
                    only.is_empty()
                        || only.iter().any(|word| {
                            aterm_converge::selects(
                                word,
                                &item.id(),
                                item.family(),
                                Some(place.layer),
                            )
                        })
                })
        });
    let records = front.records;
    let desktop: &Around<'_> = &|lane| {
        if !holds {
            return lane();
        }
        let held = records.hold(whole, &|line| eprintln!("aterm init: {line}"))?;
        let withheld: Vec<String> = groups
            .iter()
            .filter(|(_, place)| {
                place.layer == Layer::Workstation && place.class == Class::Preference
            })
            .flat_map(|(items, place)| items.iter().map(move |item| (item, place)))
            .filter_map(|(item, place)| {
                let id = item.id();
                (item.withheld().is_some()
                    && (only.is_empty()
                        || only.iter().any(|word| {
                            aterm_converge::selects(word, &id, item.family(), Some(place.layer))
                        })))
                .then_some(id)
            })
            .collect();
        if !withheld.is_empty() {
            held.withheld(&withheld)?;
        }
        let rows = lane()?;
        if whole {
            let verified = verified_mac_settings(&ctx, &groups, &rows);
            if !verified.is_empty() {
                held.checkpoint(verified)?;
            }
            if verified == Selection::all().workstation()
                && rows
                    .iter()
                    .filter(|row| {
                        row.layer == Some(Layer::Workstation)
                            && row.class == Some(Class::Preference)
                    })
                    .all(|row| counts(row, Action::Apply))
            {
                held.verified()?;
            }
        }
        Ok(rows)
    };
    laps.lap("questions");
    Ok(run_layers(
        sys,
        &ctx,
        (front.base)(mode == Mode::Apply).narrowed(only),
        &groups,
        &develop.items,
        &Narrowing {
            only,
            record: options.develop.is_some() || develop.unreadable,
        },
        mode,
        journal,
        laps,
        &Hooks {
            on_row: front.on_row,
            desktop,
        },
    )?)
}

/// The one envelope every `--json` surface prints (`aterm pkg doctor --json` too), with the
/// run's phases.
fn report_json(
    action: Action,
    complete: bool,
    rows: Vec<ItemReport>,
    laps: &Laps,
) -> Result<String, String> {
    let mode = match action {
        Action::Apply => ReportMode::Apply,
        Action::Plan => ReportMode::Plan,
        Action::Check => ReportMode::Check,
    };
    let mut report = Report::new(mode, complete, rows);
    report.phases = laps.phases();
    aterm_json::to_string(&report).map_err(|why| why.to_string())
}

/// Compute the exit status for `rows` under `action` (`0`, `75`, `77`, `69`, or `1`): a run
/// left only by rows that stood aside exits with the first of 75, 77 and 69 one of them maps to
/// ([`aterm_converge::StoodAside::exit_status`]). A row waiting on an admin counts as done
/// ([`counts`]).
fn status(rows: &[ItemReport], action: Action) -> u8 {
    if complete(rows, action) {
        return 0;
    }
    let incomplete: Vec<&ItemReport> = rows.iter().filter(|row| !counts(row, action)).collect();
    let codes: Option<Vec<u8>> = incomplete
        .iter()
        .map(|row| {
            row.stood_aside
                .as_ref()
                .map(aterm_converge::StoodAside::exit_status)
        })
        .collect();
    let codes = codes.unwrap_or_default();
    [75, 77, 69]
        .into_iter()
        .find(|code| codes.contains(code))
        .unwrap_or(1)
}

/// [`status`] of an apply over `rows`: another crate's tests of the base layer it composes.
#[cfg(any(test, debug_assertions))]
#[must_use]
pub fn apply_status_for_test(rows: &[ItemReport]) -> u8 {
    status(rows, Action::Apply)
}

/// Whether `row` waits on another person (an admin approving this Mac's vault join) and
/// finishes by itself: the subscriber, which runs without this run or its person, lands the
/// join. A row waiting for that join (GitHub's sign-in) waits too, but only another run
/// finishes it.
fn waits_by_itself(row: &ItemReport) -> bool {
    crate::init_secrets::finishes_by_itself(row)
}

/// Whether `row` waits on anything: on an admin, or on a row that does.
fn waiting(row: &ItemReport) -> bool {
    matches!(
        row.stood_aside,
        Some(aterm_converge::StoodAside::Waiting(_))
    )
}

/// Fully covered Mac stages in a whole raw init. Keep the prepared preference
/// IDs and the native services' expected IDs, so absent or unbuilt rows cannot
/// earn completion merely because every row that happened to return succeeded.
fn verified_mac_settings(ctx: &Ctx, groups: &[Group], rows: &[ItemReport]) -> Selection {
    let accounts = crate::init_accounts::items(ctx)
        .ok()
        .map(|items| items.iter().map(|item| item.id()).collect::<Vec<_>>());
    let preferences: Vec<String> = groups
        .iter()
        .filter(|(_, place)| place.layer == Layer::Workstation && place.class == Class::Preference)
        .flat_map(|(items, _)| items.iter().map(|item| item.id()))
        .filter(|id| {
            id != ACCOUNTS_GROUP
                && accounts
                    .as_ref()
                    .is_none_or(|accounts| !accounts.contains(id))
        })
        .collect();
    let covered = |expected: &[String]| {
        !expected.is_empty()
            && expected.iter().all(|id| {
                let mut matching = rows
                    .iter()
                    .filter(|row| {
                        row.layer == Some(Layer::Workstation)
                            && row.class == Some(Class::Preference)
                            && row.id == *id
                    })
                    .peekable();
                matching.peek().is_some() && matching.all(|row| counts(row, Action::Apply))
            })
    };
    let mut verified = Selection::none();
    for (group, expected) in [
        (WorkstationGroup::Preferences, Some(preferences)),
        (WorkstationGroup::Accounts, accounts),
    ] {
        if expected.as_deref().is_some_and(&covered) {
            let selected = group.selected(Selection::all());
            for setting in Setting::ALL {
                if selected.contains(setting) {
                    verified.set(setting, true);
                }
            }
        }
    }
    verified
}

/// The workstation items shared by init, doctor and first-launch setup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkstationGroup {
    /// Durable account registration and Apple's Sign Out controls.
    Accounts,
    /// The workstation preferences and immediate input activation, without accounts.
    Preferences,
    /// Apply already-saved input preferences to the active login session.
    InputActivation,
    /// The complete workstation profile, excluding optional repository setup.
    All,
    /// The selected preference rows whose keys sit in another app's sandbox container
    /// ([`crate::items::defaults::CONTAINED`]), withheld without Full Disk Access
    /// ([`crate::items::Item::withheld`]).
    Withheld,
}

impl WorkstationGroup {
    /// The selected rows belonging to this part of the pass.
    #[must_use]
    pub fn selected(self, mut selection: Selection) -> Selection {
        for setting in Setting::ALL {
            let belongs = match self {
                Self::All => setting.is_workstation(),
                Self::Accounts => matches!(setting, Setting::Messages | Setting::FaceTime),
                Self::Preferences | Self::Withheld => {
                    setting.is_workstation()
                        && !matches!(setting, Setting::Messages | Setting::FaceTime)
                }
                Self::InputActivation => {
                    matches!(setting, Setting::CapsLock | Setting::NaturalScrolling)
                }
            };
            if !belongs {
                selection.set(setting, false);
            }
        }
        selection
    }
}

/// Run the same workstation items used by `aterm init` for the console account.
///
/// # Errors
/// The console account or its required input facts cannot be resolved.
pub fn run_workstation(
    sys: &dyn System,
    group: WorkstationGroup,
    mode: Mode,
) -> Result<Vec<ItemReport>, String> {
    prepare_workstation(sys, group)?.run(sys, mode)
}

/// Run only the chosen workstation groups, without probing unchecked settings.
///
/// # Errors
/// The console account or required selected input facts cannot be resolved.
pub fn run_workstation_selected(
    sys: &dyn System,
    group: WorkstationGroup,
    mode: Mode,
    selected: Selection,
) -> Result<Vec<ItemReport>, String> {
    prepare_workstation_selected(sys, group, selected)?.run(sys, mode)
}

/// A workstation pass whose before-images and apply share one resolved account and item set.
pub struct Workstation {
    ctx: Ctx,
    items: Vec<Box<dyn Item>>,
    /// Each item's id and the setting it was built for ([`workstation_items_selected`]).
    settings: Vec<(String, Setting)>,
    /// Each item's before-images, or why they could not be taken; `None` until taken.
    snapshots: Option<Vec<Result<PreferenceSnapshot, String>>>,
    /// The items this pass leaves alone ([`Workstation::hold`]), by id.
    held: Vec<(String, Hold)>,
}

type PreferenceSnapshot = Vec<(Target, Read<PValue>)>;

/// How a pass leaves an item alone ([`Workstation::hold`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hold {
    /// Its change is not made in this pass.
    Back(String),
    /// It made its change once and was changed since.
    AsIs(String),
}

/// The shared engine's rows and the items whose change it reached.
#[derive(Debug)]
pub struct WorkstationRun {
    /// The common probe/apply/read-back results.
    pub rows: Vec<ItemReport>,
    /// Each item whose apply or refresh ran, in run order.
    pub attempts: Vec<Attempt>,
    /// The items that left keys in another app's container alone for want of Full Disk Access
    /// ([`Item::withheld`]), by id, held or not.
    pub withheld: Vec<String>,
    /// Each item's id and the setting it was built for.
    pub settings: Vec<(String, Setting)>,
}

impl WorkstationRun {
    /// Each row with the setting its item was built for, in run order.
    #[must_use]
    pub fn tagged(&self) -> Vec<(Setting, &ItemReport)> {
        tag(&self.settings, &self.rows)
    }
}

fn tag<'a>(
    settings: &[(String, Setting)],
    rows: &'a [ItemReport],
) -> Vec<(Setting, &'a ItemReport)> {
    rows.iter()
        .filter_map(|row| {
            settings
                .iter()
                .find(|(id, _)| *id == row.id)
                .map(|(_, setting)| (*setting, row))
        })
        .collect()
}

/// One item whose apply or refresh this pass ran.
#[derive(Debug, Clone)]
pub struct Attempt {
    /// The item's id, as its row has it.
    pub id: String,
    /// Whether its read-back verified the change.
    pub verified: bool,
    /// The before-images the snapshot guard admitted for it just before its mutation,
    /// including a write that failed after partially taking effect.
    pub preferences: Vec<(Target, Read<PValue>)>,
}

impl Workstation {
    /// Typed before-images of precisely the preference keys this pass would change.
    ///
    /// # Errors
    /// Two items would change the same key.
    pub fn preference_changes(&mut self, sys: &dyn System) -> Result<PreferenceSnapshot, String> {
        let mut changes = Vec::new();
        let mut snapshots = Vec::new();
        for item in &self.items {
            // A held item changes nothing in this pass ([`Workstation::hold`]): no key of
            // it is backed up for, or activated after, a change nobody makes.
            if self.held.iter().any(|(id, _)| *id == item.id()) {
                snapshots.push(Ok(Vec::new()));
                continue;
            }
            let snapshot = item.preference_changes(sys, &self.ctx).and_then(|snapshot| {
                if snapshot
                    .iter()
                    .any(|(_, read)| matches!(read, Read::Unusable(_)))
                {
                    Err("a value it would replace could not be read for recovery; left unchanged"
                        .into())
                } else {
                    Ok(snapshot)
                }
            });
            for (target, read) in snapshot.iter().flatten() {
                if changes.iter().any(|(saved, _)| saved == target) {
                    return Err(
                        "workstation items overlap a preference key; no changes applied".into(),
                    );
                }
                changes.push((target.clone(), read.clone()));
            }
            snapshots.push(snapshot);
        }
        self.snapshots = Some(snapshots);
        Ok(changes)
    }

    /// Run the common probe/apply/read-back engine over these prepared items.
    ///
    /// # Errors
    /// The journal cannot be resolved or the items' dependency order is invalid.
    pub fn run(self, sys: &dyn System, mode: Mode) -> Result<Vec<ItemReport>, String> {
        Ok(self.run_recording(sys, mode)?.rows)
    }

    /// Run the shared engine and give each row the setting its item was built for.
    ///
    /// # Errors
    /// The journal cannot be resolved or the items' dependency order is invalid.
    pub fn run_tagged(
        self,
        sys: &dyn System,
        mode: Mode,
    ) -> Result<Vec<(Setting, ItemReport)>, String> {
        let run = self.run_recording(sys, mode)?;
        Ok(run
            .tagged()
            .into_iter()
            .map(|(setting, row)| (setting, row.clone()))
            .collect())
    }

    /// Run the shared engine and retain exactly which snapshotted items reached mutation,
    /// including a write that failed after partially taking effect.
    ///
    /// # Errors
    /// The journal cannot be resolved or the items' dependency order is invalid.
    pub fn run_recording(self, sys: &dyn System, mode: Mode) -> Result<WorkstationRun, String> {
        let mut journal = sys.journal()?;
        self.run_recording_with(sys, mode, journal.as_mut())
    }

    /// Leave the items `held` names alone in this pass, each as its [`Hold`] says.
    pub fn hold(&mut self, held: &[(String, Hold)]) {
        self.held = held.to_vec();
    }

    /// A pass over `items` at a context of no account and no host facts.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn of(items: Vec<Box<dyn Item>>) -> Self {
        Self {
            ctx: Ctx {
                facts: HostFacts::default(),
                users: Vec::new(),
                console: None,
                host_name: String::new(),
                cache_dir: PathBuf::new(),
                disrupt: true,
            },
            items,
            snapshots: None,
            settings: Vec::new(),
            held: Vec::new(),
        }
    }

    fn run_recording_with(
        self,
        sys: &dyn System,
        mode: Mode,
        journal: &mut dyn Journal,
    ) -> Result<WorkstationRun, String> {
        let attempted = Arc::new(Mutex::new(Vec::new()));
        let withheld: Vec<String> = self
            .items
            .iter()
            .filter(|item| item.withheld().is_some())
            .map(|item| item.id())
            .collect();
        let mut snapshots = self.snapshots.map(Vec::into_iter);
        let items: Vec<Box<dyn Item>> = self
            .items
            .into_iter()
            .map(|inner| {
                Box::new(Guard {
                    before: snapshots.as_mut().and_then(Iterator::next),
                    inner,
                    attempted: Arc::clone(&attempted),
                }) as Box<dyn Item>
            })
            .collect();
        let bound: Vec<_> = crate::engine::bind(
            items
                .iter()
                .map(|item| (item.as_ref(), workstation_place(Class::Preference))),
            sys,
            &self.ctx,
        )
        .into_iter()
        .flat_map(|item| {
            let id = item.id();
            match self.held.iter().find(|(held, _)| *held == id) {
                None => vec![item],
                Some((_, Hold::Back(why))) => aterm_converge::held_back(vec![item], why),
                Some((_, Hold::AsIs(why))) => aterm_converge::left_as_is(vec![item], why),
            }
        })
        .collect();
        let rows =
            aterm_converge::run(&bound, mode, journal).map_err(|refused| refused.to_string())?;
        let attempted = attempted
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let attempts = attempted
            .into_iter()
            .filter_map(|(id, preferences)| {
                let row = rows.iter().find(|row| row.id == id)?;
                row.stood_aside.is_none().then_some(Attempt {
                    verified: matches!(
                        row.outcome,
                        Outcome::Ok | Outcome::Changed | Outcome::NotApplicable
                    ),
                    id,
                    preferences,
                })
            })
            .collect();
        Ok(WorkstationRun {
            rows,
            attempts,
            settings: self.settings,
            withheld,
        })
    }
}

/// Wrapper that validates recovery before-images and records attempted items.
struct Guard {
    inner: Box<dyn Item>,
    /// Saved before-images, or why the pass could not take them (`None` when unsnapshotted).
    before: Option<Result<PreferenceSnapshot, String>>,
    attempted: Arc<Mutex<Vec<(String, PreferenceSnapshot)>>>,
}

impl Guard {
    fn validate(&self, sys: &dyn System, ctx: &Ctx) -> Result<(), String> {
        let Some(before) = &self.before else {
            return Ok(());
        };
        let before = before.as_ref().map_err(Clone::clone)?;
        let current = self.inner.preference_changes(sys, ctx)?;
        if current.len() == before.len()
            && before.iter().all(|(target, before)| {
                let mut matching = current.iter().filter(|(now, _)| now == target);
                matching
                    .next()
                    .is_some_and(|(_, now)| same_snapshot_read(before, now))
                    && matching.next().is_none()
            })
        {
            Ok(())
        } else {
            Err("workstation preferences changed after the recovery snapshot; run `aterm doctor --apply` again".into())
        }
    }

    fn record_attempt(&self) {
        let id = self.inner.id();
        let mut attempted = self
            .attempted
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if !attempted.iter().any(|(saved, _)| *saved == id) {
            attempted.push((
                id,
                self.before.clone().and_then(Result::ok).unwrap_or_default(),
            ));
        }
    }

    /// Undo [`Guard::record_attempt`] for an item whose refresh had nothing to do.
    fn forget_attempt(&self) {
        let id = self.inner.id();
        self.attempted
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|(saved, _)| *saved != id);
    }
}

impl Item for Guard {
    fn id(&self) -> String {
        self.inner.id()
    }
    fn family(&self) -> &'static str {
        self.inner.family()
    }
    fn title(&self) -> String {
        self.inner.title()
    }
    fn probe(&self, sys: &dyn System, ctx: &Ctx) -> Probe {
        self.inner.probe(sys, ctx)
    }
    fn apply(&self, sys: &dyn System, ctx: &Ctx) -> Result<(), String> {
        self.validate(sys, ctx)?;
        self.record_attempt();
        self.inner.apply(sys, ctx)
    }
    fn preference_changes(
        &self,
        sys: &dyn System,
        ctx: &Ctx,
    ) -> Result<PreferenceSnapshot, String> {
        self.inner.preference_changes(sys, ctx)
    }
    fn refresh(&self, sys: &dyn System, ctx: &Ctx) -> Option<Result<(), String>> {
        if let Err(why) = self.validate(sys, ctx) {
            return Some(Err(why));
        }
        self.record_attempt();
        let refreshed = self.inner.refresh(sys, ctx);
        if refreshed.is_none() {
            self.forget_attempt();
        }
        refreshed
    }
    fn revert(&self) -> Option<String> {
        self.inner.revert()
    }
    fn effect(&self) -> Effect {
        self.inner.effect()
    }
    fn activation(&self) -> Option<Arc<dyn crate::items::Activation>> {
        self.inner.activation()
    }
    fn after(&self) -> Vec<String> {
        self.inner.after()
    }
    fn detail(&self, sys: &dyn System, ctx: &Ctx) -> Option<String> {
        self.inner.detail(sys, ctx)
    }
    fn stood_aside(&self) -> Option<aterm_converge::StoodAside> {
        self.inner.stood_aside()
    }
    fn read_aside(&self) -> Option<aterm_converge::StoodAside> {
        self.inner.read_aside()
    }
    fn withheld(&self) -> Option<String> {
        self.inner.withheld()
    }
    fn stands_aside(&self, sys: &dyn System, ctx: &Ctx) -> Option<aterm_converge::StoodAside> {
        self.inner.stands_aside(sys, ctx)
    }
}

fn same_snapshot_read(a: &Read<PValue>, b: &Read<PValue>) -> bool {
    match (a, b) {
        (Read::Absent, Read::Absent) => true,
        (Read::Value(a), Read::Value(b)) => same_snapshot_value(a, b),
        _ => false,
    }
}

fn same_snapshot_value(a: &PValue, b: &PValue) -> bool {
    match (a, b) {
        (PValue::Real(a), PValue::Real(b)) => a.to_bits() == b.to_bits(),
        (PValue::Array(a), PValue::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| same_snapshot_value(a, b))
        }
        (PValue::Dict(a), PValue::Dict(b)) => {
            a.len() == b.len()
                && a.iter().all(|(key, value)| {
                    let mut matching = b.iter().filter(|(other, _)| other == key);
                    matching
                        .next()
                        .is_some_and(|(_, other)| same_snapshot_value(value, other))
                        && matching.next().is_none()
                })
        }
        _ => a == b,
    }
}

/// Resolve the console account and build the shared workstation items once.
///
/// # Errors
/// The console account or required host facts cannot be resolved.
pub fn prepare_workstation(
    sys: &dyn System,
    group: WorkstationGroup,
) -> Result<Workstation, String> {
    prepare_workstation_selected(sys, group, Selection::all())
}

/// Prepare only the chosen rows.
///
/// # Errors
/// The console account or required selected input facts cannot be resolved.
pub fn prepare_workstation_selected(
    sys: &dyn System,
    group: WorkstationGroup,
    selected: Selection,
) -> Result<Workstation, String> {
    let selected = group.selected(selected);
    if selected.is_empty() {
        return Ok(Workstation {
            ctx: Ctx {
                facts: HostFacts::default(),
                users: Vec::new(),
                console: None,
                host_name: String::new(),
                cache_dir: PathBuf::new(),
                disrupt: true,
            },
            items: Vec::new(),
            settings: Vec::new(),
            snapshots: None,
            held: Vec::new(),
        });
    }
    let ctx = context_for_selected(sys, group, selected)?;
    if ctx.console.is_none() {
        return Err(outside_console_session(sys));
    }
    let (settings, items) = workstation_items_selected(&ctx, group, selected)?
        .into_iter()
        .map(|(setting, item)| ((item.id(), setting), item))
        .unzip();
    Ok(Workstation {
        ctx,
        items,
        settings,
        snapshots: None,
        held: Vec::new(),
    })
}

/// Resolve this login's console context without inventorying keyboards or preference services.
///
/// # Errors
/// The login's console account cannot be established.
pub fn initial_setup_context(sys: &dyn System) -> Result<Ctx, String> {
    let ctx = context_for_selected(sys, WorkstationGroup::Accounts, Selection::none())?;
    if ctx.console.is_none() {
        return Err(outside_console_session(sys));
    }
    Ok(ctx)
}

/// The refusal of a step that reads or changes the console owner's own session from outside it
/// ([`crate::console::away`]): an SSH login of that same account is told so, not sent to log in.
fn outside_console_session(sys: &dyn System) -> String {
    let why = crate::console::away(sys)
        .and_then(Presence::why)
        .unwrap_or("its console changed hands while this read it");
    format!("run as the user logged in at this Mac's console, in its own session ({why})")
}

/// Run the selected Spotlight policy through the same probe/apply/read-back engine.
///
/// # Errors
/// Its items or account journal cannot be prepared.
pub fn run_initial_spotlight(
    sys: &dyn System,
    ctx: &Ctx,
    mode: Mode,
) -> Result<Vec<ItemReport>, String> {
    Workstation {
        ctx: ctx.clone(),
        items: crate::init_spotlight::items(ctx)?,
        settings: Vec::new(),
        snapshots: None,
        held: Vec::new(),
    }
    .run(sys, mode)
}

/// Apply System State's Secrets stage.
///
/// # Errors
/// The context names no one user, or the journal cannot be resolved; nothing was changed.
pub fn run_initial_secrets(
    sys: &dyn System,
    ctx: &Ctx,
    ops: Arc<dyn crate::init_secrets::SecretsOps>,
    source: &str,
    replaces: crate::init_secrets::Replaces,
    password: &mut Option<aterm_secrets::SecretBytes>,
    wait: std::time::Duration,
) -> Result<Vec<ItemReport>, String> {
    let mut journal = sys.journal()?;
    run_initial_secrets_with(
        sys,
        ctx,
        ops,
        source,
        replaces,
        password,
        wait,
        journal.as_mut(),
    )
}

/// [`run_initial_secrets`], journaling in `journal`.
///
/// # Errors
/// As [`run_initial_secrets`].
#[expect(
    clippy::too_many_arguments,
    reason = "run_initial_secrets's inputs plus the journal it resolves"
)]
pub fn run_initial_secrets_with(
    sys: &dyn System,
    ctx: &Ctx,
    ops: Arc<dyn crate::init_secrets::SecretsOps>,
    source: &str,
    replaces: crate::init_secrets::Replaces,
    password: &mut Option<aterm_secrets::SecretBytes>,
    wait: std::time::Duration,
    journal: &mut dyn Journal,
) -> Result<Vec<ItemReport>, String> {
    let vault = crate::init_secrets::selected(ctx, ops, source, wait, replaces)?;
    run_vault_item(vault, sys, ctx, password, journal)
}

/// The converge pass's join of the ALab vault (`sync.peers`): [`run_initial_secrets_with`]
/// with no password, except that the subscriber its join puts in place is a repair, never a
/// person's On (`init_secrets::Vault::keeping_off`), so a turn-off recorded since the pass read
/// this Mac stays.
///
/// # Errors
/// As [`run_initial_secrets`].
pub(crate) fn run_converge_join(
    sys: &dyn System,
    ctx: &Ctx,
    ops: Arc<dyn crate::init_secrets::SecretsOps>,
    source: &str,
    replaces: crate::init_secrets::Replaces,
    wait: std::time::Duration,
    journal: &mut dyn Journal,
) -> Result<Vec<ItemReport>, String> {
    let vault = crate::init_secrets::selected(ctx, ops, source, wait, replaces)?.keeping_off();
    run_vault_item(vault, sys, ctx, &mut None, journal)
}

/// Run `vault`, the Secrets stage's vault item, as an apply journaled in `journal`, holding
/// `password` for its join and handing back what the join did not spend.
fn run_vault_item(
    vault: crate::init_secrets::Vault,
    sys: &dyn System,
    ctx: &Ctx,
    password: &mut Option<aterm_secrets::SecretBytes>,
    journal: &mut dyn Journal,
) -> Result<Vec<ItemReport>, String> {
    vault.hold_password(password.take());
    let bound = crate::engine::bind(
        [(&vault as &dyn Item, workstation_place(Class::Install))],
        sys,
        ctx,
    );
    let rows =
        aterm_converge::run(&bound, Mode::Apply, journal).map_err(|refused| refused.to_string());
    drop(bound);
    *password = vault.unspent_password();
    rows
}

/// Workstation items, each with the setting whose System State row it reports to.
type TaggedItems = Vec<(Setting, Box<dyn Item>)>;

/// The selected workstation items, each tagged when it is built with the setting
/// whose System State row it reports to: ids and families are never parsed for it
/// (`trackpad` is shared by two settings, and both sign-outs are family `init`).
fn workstation_items_selected(
    ctx: &Ctx,
    group: WorkstationGroup,
    selected: Selection,
) -> Result<TaggedItems, String> {
    let mut items: TaggedItems = Vec::new();
    let mut add = |setting: Setting, built: Vec<Box<dyn Item>>| {
        items.extend(built.into_iter().map(|item| (setting, item)));
    };
    let only = |setting: Setting| {
        let mut one = Selection::none();
        one.set(setting, selected.contains(setting));
        one
    };
    let accounts = |add: &mut dyn FnMut(Setting, Vec<Box<dyn Item>>)| {
        for setting in [Setting::Messages, Setting::FaceTime] {
            if selected.contains(setting) {
                add(
                    setting,
                    crate::init_accounts::selected_items(ctx, only(setting))?,
                );
            }
        }
        Ok::<_, String>(())
    };
    match group {
        WorkstationGroup::Withheld => {
            items.extend(crate::init_preferences::withheld_items(ctx, selected)?);
        }
        WorkstationGroup::All | WorkstationGroup::Preferences => {
            // One call for the whole selection, each item tagged as it is built: the
            // widget, desktop and Dock items of a pass share its one relaunch
            // ([`crate::init_preferences::selected_tagged_items`]).
            for (setting, item) in crate::init_preferences::selected_tagged_items(ctx, selected)? {
                add(setting, vec![item]);
            }
            if selected.contains(Setting::NaturalScrolling) {
                add(Setting::NaturalScrolling, crate::init_trackpad::items(ctx)?);
            }
            if selected.contains(Setting::TrackpadScrolling) {
                add(
                    Setting::TrackpadScrolling,
                    crate::init_trackpad::behavior_items(ctx)?,
                );
            }
            if group == WorkstationGroup::All {
                accounts(&mut add)?;
            }
        }
        WorkstationGroup::Accounts => accounts(&mut add)?,
        WorkstationGroup::InputActivation => {
            if selected.contains(Setting::CapsLock) {
                add(
                    Setting::CapsLock,
                    crate::init_preferences::attached_keyboard_items(ctx)?,
                );
            }
            if selected.contains(Setting::NaturalScrolling) {
                add(Setting::NaturalScrolling, crate::init_trackpad::items(ctx)?);
            }
        }
    }
    Ok(items)
}

/// Items that share one place in the desired state.
type Group = (Vec<Box<dyn Item>>, Place);

const ACCOUNTS_GROUP: &str = "init.accounts";

/// What [`converge`] read and was told before the workstation's groups are built.
struct Reading<'a> {
    /// Why the keyboard list could not be read ([`local_context`]): only the Caps Lock rows go
    /// unread.
    keyboards: Option<&'a str>,
    /// The run's `--only` words.
    only: &'a [String],
    /// What `--profile`, `--sign-out` and `--clone` picked, when one was given.
    choice: Option<Choice>,
}

/// How a run prepares the workstation's groups ([`prepare_groups`], or a test's stand-in), with
/// the groups it picked.
type Prepare<'a> =
    dyn Fn(&dyn System, &Ctx, Mode, &Reading<'_>) -> Result<(Vec<Group>, Choice), String> + 'a;

/// Prepare workstation groups, reading optional profiles and prompting for vault credentials
/// before probing; the look for the ALab vault answers with `ahead`'s, and `vault` is
/// `--vault`'s. Who is at this run ([`crate::console::presence`]) decides whether it may
/// change the person's own Mac: over SSH, or under another login's console, nothing of the
/// workstation is read; from an agent's tool call or with no terminal, the vault's item is
/// built as a plan builds it (never a prompt, never the network, `--vault` included) and
/// [`groups`] leaves preferences to them while still building the account sign-out in the
/// console user's graphic session. The vault is read only when this run names its
/// item. Every group runs unless a flag names the ones to (nothing asks), and for a person at
/// the console the Permissions phase lists aterm's grants. A group that cannot be built is one
/// row saying why ([`Unbuilt`]); the others run.
fn prepare_groups(
    sys: &dyn System,
    ctx: &Ctx,
    mode: Mode,
    ahead: Option<Arc<crate::init_secrets::Lookahead>>,
    vault: Option<&str>,
    reading: &Reading<'_>,
    grants: Grants,
) -> Result<(Vec<Group>, Choice), String> {
    prepare_groups_with(sys, ctx, mode, vault, reading, grants, |ctx| {
        crate::init_secrets::live(ctx, ahead)
    })
}

/// [`prepare_groups`] with the vault's port from `secrets` (a test's scripted one).
fn prepare_groups_with(
    sys: &dyn System,
    ctx: &Ctx,
    mode: Mode,
    vault: Option<&str>,
    reading: &Reading<'_>,
    grants: Grants,
    secrets: impl FnOnce(&Ctx) -> Result<Arc<dyn crate::init_secrets::SecretsOps>, String>,
) -> Result<(Vec<Group>, Choice), String> {
    if !sys.macos() {
        return elsewhere(sys, ctx, mode, vault, reading, secrets);
    }
    let presence = init_presence(sys);
    if ctx.console.is_none() {
        return Ok((vec![away_from_console(presence)], Choice::NONE));
    }
    let person = presence == Presence::AtConsole;
    let asks = if person { mode } else { Mode::Plan };
    let secrets = vault_items(sys, ctx, asks, vault, reading, secrets)?;
    let github = built(
        "init.github",
        "init",
        Setting::Repositories.label(),
        crate::init_github::items(sys, ctx, secrets.waited_on.as_deref()),
    );
    let choice = reading.choice.unwrap_or(Choice::ALL);
    let mut groups = groups(
        ctx,
        presence,
        secrets,
        github,
        reading.keyboards,
        choice,
        reading.only,
    );
    if person {
        groups.push((
            permissions(grants, choice.sign_out),
            workstation_place(Class::Person),
        ));
    }
    Ok((groups, choice))
}

/// The vault's items, asking (`asks`) for its join password only in a run that applies for a
/// person; none when `--only` names neither the workstation nor the vault.
fn vault_items(
    sys: &dyn System,
    ctx: &Ctx,
    asks: Mode,
    vault: Option<&str>,
    reading: &Reading<'_>,
    secrets: impl FnOnce(&Ctx) -> Result<Arc<dyn crate::init_secrets::SecretsOps>, String>,
) -> Result<crate::init_secrets::Secrets, String> {
    let names_vault = reading.only.is_empty()
        || ctx.users.iter().any(|user| {
            reading.only.iter().any(|word| {
                word == "workstation"
                    || aterm_converge::selects(
                        word,
                        &crate::init_secrets::item_id(user),
                        crate::init_secrets::FAMILY,
                        Some(Layer::Workstation),
                    )
            })
        });
    Ok(if names_vault {
        crate::init_secrets::items(sys, ctx, secrets(ctx)?, asks, vault)?
    } else {
        crate::init_secrets::Secrets {
            items: Vec::new(),
            waited_on: None,
        }
    })
}

/// The workstation layer on a machine that is not a Mac (Linux): one row per macOS group, each
/// reading "macOS only"; the vault, asking its join password only of a person at a terminal (an
/// SSH session's included: nobody sits at a Linux box's console); and the repositories, unless
/// the flags leave them out. Each runs in any shell of the account.
fn elsewhere(
    sys: &dyn System,
    ctx: &Ctx,
    mode: Mode,
    vault: Option<&str>,
    reading: &Reading<'_>,
    secrets: impl FnOnce(&Ctx) -> Result<Arc<dyn crate::init_secrets::SecretsOps>, String>,
) -> Result<(Vec<Group>, Choice), String> {
    let person = sys.terminal() && sys.agent_ancestor().is_none();
    let asks = if person { mode } else { Mode::Plan };
    let secrets = vault_items(sys, ctx, asks, vault, reading, secrets)?;
    let github = built(
        "init.github",
        "init",
        Setting::Repositories.label(),
        crate::init_github::items(sys, ctx, secrets.waited_on.as_deref()),
    );
    let choice = reading.choice.unwrap_or(Choice::ALL);
    let anywhere = |class| Place {
        requires: Requires::Account,
        ..workstation_place(class)
    };
    let mac_only: Vec<Box<dyn Item>> = [
        ("init.preferences", "init", "Workstation preferences"),
        (
            crate::init_spotlight::ID,
            "spotlight",
            Setting::Spotlight.label(),
        ),
        (ACCOUNTS_GROUP, "init", "Sign out of Messages and FaceTime"),
        (
            "init.permissions",
            "permissions",
            Setting::Permissions.label(),
        ),
    ]
    .into_iter()
    .map(|(id, family, title)| {
        Box::new(MacOnly {
            id: id.to_owned(),
            family,
            title: title.to_owned(),
        }) as Box<dyn Item>
    })
    .collect();
    let mut groups = vec![
        (mac_only, anywhere(Class::Asked)),
        (secrets.items, anywhere(Class::Install)),
    ];
    if choice.clone {
        groups.push((github, anywhere(Class::Install)));
    }
    Ok((groups, choice))
}

/// A workstation group that exists on macOS alone ([`elsewhere`]): its one row is not
/// applicable here, never a failure.
struct MacOnly {
    id: String,
    family: &'static str,
    title: String,
}

impl Item for MacOnly {
    fn id(&self) -> String {
        self.id.clone()
    }
    fn family(&self) -> &'static str {
        self.family
    }
    fn title(&self) -> String {
        self.title.clone()
    }
    fn probe(&self, _sys: &dyn System, _ctx: &Ctx) -> Probe {
        Probe::NotApplicable {
            why: "macOS only".into(),
        }
    }
    fn apply(&self, _sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        Err("macOS only".into())
    }
}

/// The id of the one row that stands for the workstation rows a person at this Mac's console
/// makes ([`AwayFromConsole`]).
const AWAY: &str = "init.workstation";

/// The optional workstation items cannot be enumerated without reading their inputs, and are
/// the person's to make: one deferred row, saying why, until a person runs init at this Mac's
/// console.
struct AwayFromConsole(Presence);

impl Item for AwayFromConsole {
    fn id(&self) -> String {
        AWAY.into()
    }
    fn family(&self) -> &'static str {
        "init"
    }
    fn title(&self) -> String {
        "Workstation setup at this Mac's console".into()
    }
    fn probe(&self, _sys: &dyn System, _ctx: &Ctx) -> Probe {
        Probe::Deferred { why: self.why() }
    }
    fn apply(&self, _sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        Err(self.why())
    }
}

impl AwayFromConsole {
    /// The presence's line; the engine's own where the console was lost after it was read.
    fn why(&self) -> String {
        self.0
            .needs_person()
            .unwrap_or_else(|| aterm_converge::NOT_AT_CONSOLE.into())
    }
}

/// [`AwayFromConsole`] in its place: read in any shell of the account, so the engine never
/// turns it into its own `Blocked` console row; applied by nobody.
fn away_from_console(presence: Presence) -> Group {
    (
        vec![Box::new(AwayFromConsole(presence))],
        Place {
            requires: Requires::Account,
            ..workstation_place(Class::Person)
        },
    )
}

/// Whether `row` is the deferred stand-in for the workstation rows ([`AwayFromConsole`]).
fn waits_for_a_person(row: &ItemReport) -> bool {
    row.id == AWAY && row.outcome == Outcome::Deferred
}

/// A group whose items could not be built (an init.toml that cannot be read, a keyboard list
/// in words this build does not read): one row says why, and every other group still runs.
struct Unbuilt {
    id: String,
    family: &'static str,
    title: &'static str,
    why: String,
}

impl Item for Unbuilt {
    fn id(&self) -> String {
        self.id.clone()
    }
    fn family(&self) -> &'static str {
        self.family
    }
    fn title(&self) -> String {
        self.title.into()
    }
    fn probe(&self, _sys: &dyn System, _ctx: &Ctx) -> Probe {
        Probe::Unusable {
            why: self.why.clone(),
        }
    }
    fn apply(&self, _sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        Err(self.why.clone())
    }
}

/// The items `items` built, or the one [`Unbuilt`] row (`id`, `family`, `title`) saying why
/// they could not be.
fn built(
    id: &str,
    family: &'static str,
    title: &'static str,
    items: Result<Vec<Box<dyn Item>>, String>,
) -> Vec<Box<dyn Item>> {
    items.unwrap_or_else(|why| {
        vec![Box::new(Unbuilt {
            id: id.to_owned(),
            family,
            title,
            why,
        })]
    })
}

/// Workstation item groups in run order, each paired with its [`Place`]: for a person at the
/// console (`presence`), the groups `choice` picks, Rust in Spotlight and fseventsd's memory
/// ([`crate::init_fseventsd`], restarted only when the run's `only` words name it),
/// with the keyboard list unread (`keyboards`) the Caps Lock rows one row saying why; with no
/// terminal the account sign-out still runs, while preferences and agent or remote runs defer
/// ([`AwayFromConsole`]). Then the vault, and its GitHub profile (`github`, already built) when
/// `choice` clones.
fn groups(
    ctx: &Ctx,
    presence: Presence,
    secrets: crate::init_secrets::Secrets,
    github: Vec<Box<dyn Item>>,
    keyboards: Option<&str>,
    choice: Choice,
    only: &[String],
) -> Vec<Group> {
    let place = workstation_place;
    let person = presence == Presence::AtConsole;
    let account_console = person || (choice.sign_out && presence == Presence::NoTerminal);
    let mut groups = Vec::new();
    if !account_console {
        groups.push(away_from_console(presence));
    }
    if person && choice.profile {
        let preferences = match keyboards {
            None => built(
                "init.preferences",
                "init",
                "Workstation preferences",
                crate::init_preferences::items(ctx),
            ),
            Some(why) => {
                let mut rest = Selection::all();
                rest.set(Setting::CapsLock, false);
                let mut rows = built(
                    "init.preferences",
                    "init",
                    "Workstation preferences",
                    crate::init_preferences::selected_items(ctx, rest),
                );
                rows.insert(
                    0,
                    Box::new(Unbuilt {
                        id: "keyboard.caps-lock".into(),
                        family: "keyboard",
                        title: Setting::CapsLock.label(),
                        why: why.to_owned(),
                    }),
                );
                rows
            }
        };
        groups.push((preferences, place(Class::Preference)));
        groups.push((
            built(
                "init.trackpad",
                "trackpad",
                Setting::NaturalScrolling.label(),
                crate::init_trackpad::items(ctx),
            ),
            place(Class::Preference),
        ));
        groups.push((
            built(
                "init.trackpad.behavior",
                "trackpad",
                Setting::TrackpadScrolling.label(),
                crate::init_trackpad::behavior_items(ctx),
            ),
            place(Class::Preference),
        ));
    }
    if person {
        groups.push((
            built(
                crate::init_spotlight::ID,
                "spotlight",
                Setting::Spotlight.label(),
                crate::init_spotlight::items(ctx),
            ),
            place(Class::Asked),
        ));
        let named = only.iter().any(|word| word == crate::init_fseventsd::ID);
        groups.push((
            vec![Box::new(crate::init_fseventsd::Fseventsd::new(named))],
            crate::init_fseventsd::place(),
        ));
    }
    if account_console && choice.sign_out {
        groups.push((
            built(
                ACCOUNTS_GROUP,
                "init",
                "Sign out of Messages and FaceTime",
                crate::init_accounts::items(ctx),
            ),
            place(Class::Preference),
        ));
    }
    groups.push((secrets.items, place(Class::Install)));
    if choice.clone {
        groups.push((github, place(Class::Install)));
    }
    groups
}

/// The Permissions phase's rows: Full Disk Access, Developer Tools and debugger access, and
/// Accessibility for a run that signs out (`sign_out`), read once for all of them by `grants`
/// when the first is probed.
fn permissions(grants: Grants, sign_out: bool) -> Vec<Box<dyn Item>> {
    let readings = Arc::new(Readings {
        grants,
        sign_out,
        read: std::sync::OnceLock::new(),
    });
    [
        Access::FullDiskAccess,
        Access::DeveloperTools,
        Access::Debugger,
        Access::Accessibility,
    ]
    .into_iter()
    .filter(|access| sign_out || *access != Access::Accessibility)
    .map(|access| {
        Box::new(Permission {
            access,
            readings: Arc::clone(&readings),
        }) as Box<dyn Item>
    })
    .collect()
}

/// aterm's grants, read once for every row of the Permissions phase ([`permissions`]).
struct Readings {
    grants: Grants,
    sign_out: bool,
    read: std::sync::OnceLock<Vec<(Access, Grant)>>,
}

/// One of aterm's macOS grants in the Permissions phase: only the person switches it on, in
/// System Settings, so its row names where; a run counts it done but `--check` does not
/// ([`counts`]). A grant that cannot be read here is not applicable to the run, saying why.
struct Permission {
    access: Access,
    readings: Arc<Readings>,
}

/// The id prefix of the Permissions phase's rows ([`Permission`]).
const PERMISSION: &str = "init.permissions.";

impl Item for Permission {
    fn id(&self) -> String {
        format!("{PERMISSION}{}", self.access.word())
    }
    fn family(&self) -> &'static str {
        "permissions"
    }
    fn title(&self) -> String {
        self.access.name().into()
    }
    fn probe(&self, _sys: &dyn System, _ctx: &Ctx) -> Probe {
        let readings = self
            .readings
            .read
            .get_or_init(|| (self.readings.grants)(self.readings.sign_out));
        match readings.iter().find(|(access, _)| *access == self.access) {
            Some((_, Grant::On)) => Probe::Ok { now: "On".into() },
            Some((_, Grant::NeedsYou(how))) => Probe::Person {
                step: self.id(),
                how: how.clone(),
            },
            Some((_, Grant::NotNeeded(why))) => Probe::NotApplicable { why: why.clone() },
            // A grant this process cannot read (outside aterm.app, or one that reads only once
            // Full Disk Access is on) is no failure of the run: aterm.app's Permissions reads it.
            Some((_, Grant::Unread(why))) => Probe::NotApplicable {
                why: format!("{why}; aterm.app's Permissions reads it"),
            },
            None => Probe::NotApplicable {
                why: "not read on this Mac".into(),
            },
        }
    }
    fn apply(&self, _sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        Err("only you can switch it on, in System Settings".into())
    }
}

fn workstation_place(class: Class) -> Place {
    Place {
        layer: Layer::Workstation,
        class,
        requires: Requires::Console,
        remedy: Remedy::Command("aterm init".into()),
    }
}

/// [`run_layers`] with no develop item and no `--only`: the base and workstation passes.
#[cfg(test)]
fn run(
    sys: &dyn System,
    ctx: &Ctx,
    base: Base,
    groups: &[Group],
    mode: Mode,
    journal: &mut (dyn Journal + Send),
) -> Result<Vec<ItemReport>, String> {
    run_layers(
        sys,
        ctx,
        base,
        groups,
        &[],
        &Narrowing {
            only: &[],
            record: false,
        },
        mode,
        journal,
        &mut Laps::start(),
        &Hooks::QUIET,
    )
}

/// Second-pass narrowing: `--only` selectors and whether `develop.record` is always kept.
struct Narrowing<'a> {
    only: &'a [String],
    record: bool,
}

/// What a run's caller hears and wraps while its lanes go ([`run_layers`]).
struct Hooks<'a> {
    /// Hears each row as its item finishes, from whichever lane ran it.
    on_row: &'a (dyn Fn(&ItemReport) + Sync),
    /// Runs the Desktop lane, handed to it as a call: [`converge`] holds the setup records
    /// around it ([`Records`]), so only that lane waits behind a first-launch pass.
    desktop: &'a Around<'a>,
}

impl Hooks<'_> {
    /// Hear nothing, and run the Desktop lane as it is.
    #[cfg(test)]
    const QUIET: Hooks<'static> = Hooks {
        on_row: &|_| {},
        desktop: &|lane| lane(),
    };
}

/// One lane's run, handed to [`Hooks::desktop`] to call.
type LaneRun<'a> = dyn Fn() -> Result<Vec<ItemReport>, String> + 'a;

/// What runs a lane around its call ([`Hooks::desktop`]).
type Around<'a> = dyn Fn(&LaneRun<'_>) -> Result<Vec<ItemReport>, String> + Sync + 'a;

/// The lanes of one run ([`run_layers`]), each its items in order, all at the same time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lane {
    /// After the base layer, on the calling thread: Rust in Spotlight (rustup, once the base
    /// layer let the store lock go) and aterm's grants.
    Base,
    /// Apple's Command Line Tools, the vault, GitHub and the develop layer: installs and the
    /// network.
    Network,
    /// The preferences, the trackpad and the sign-outs: every Dock relaunch and every UI
    /// automation stays in this one lane.
    Desktop,
}

/// The lane a workstation group runs in, by its place's class.
const fn lane(class: Class) -> Lane {
    match class {
        Class::Preference => Lane::Desktop,
        Class::Install | Class::Elevate => Lane::Network,
        _ => Lane::Base,
    }
}

/// Opens once, for every thread waiting on it: the base layer let the store lock go.
#[derive(Default)]
struct Latch {
    open: Mutex<bool>,
    opened: std::sync::Condvar,
}

impl Latch {
    fn open(&self) {
        *self.open.lock().unwrap_or_else(PoisonError::into_inner) = true;
        self.opened.notify_all();
    }

    fn wait(&self) {
        let open = self.open.lock().unwrap_or_else(PoisonError::into_inner);
        drop(
            self.opened
                .wait_while(open, |open| !*open)
                .unwrap_or_else(PoisonError::into_inner),
        );
    }
}

/// Opens its latch when dropped, a panic's unwinding included, so no lane waits on a base
/// layer that ended.
struct Opens<'a>(&'a Latch);

impl Drop for Opens<'_> {
    fn drop(&mut self) {
        self.0.open();
    }
}

/// The id of the Network lane's gate ([`AfterBase`]), whose row [`run_layers`] drops.
const AFTER_BASE: &str = "init.after-base";

/// Holds the Network lane, after the vault's join, until the base layer let the store lock go
/// ([`Latch`]): `base.clt` and the develop layer's tools may take it.
struct AfterBase<'a>(&'a Latch);

impl Item for AfterBase<'_> {
    fn id(&self) -> String {
        AFTER_BASE.into()
    }
    fn family(&self) -> &'static str {
        "init"
    }
    fn title(&self) -> String {
        "the base layer let the store lock go".into()
    }
    fn probe(&self, _sys: &dyn System, _ctx: &Ctx) -> Probe {
        self.0.wait();
        Probe::Ok { now: self.title() }
    }
    fn apply(&self, _sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        Ok(())
    }
}

/// One run's journal, shared by its lanes: each record is written under its lock (the file
/// journal's own lock keeps other processes' records whole).
struct Shared<'a, 'j>(&'a Mutex<&'j mut (dyn Journal + Send)>);

impl Journal for Shared<'_, '_> {
    fn before_change(
        &mut self,
        id: &str,
        from: &str,
        to: &str,
        revert: Option<&str>,
    ) -> Result<(), String> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .before_change(id, from, to, revert)
    }

    fn failed(&mut self, id: &str, why: &str) -> Result<(), String> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .failed(id, why)
    }
}

/// Run `base` items, release the store lock, and then run `base.clt`, `groups`, and `develop`
/// in three lanes at once ([`Lane`]): the Desktop lane and the vault's join in the Network lane
/// start at once, the rest of the Network lane (`base.clt`, GitHub, the develop layer) once
/// the base layer let the store lock go ([`AfterBase`]), and the Base lane follows the base
/// layer. Each row reaches `hooks.on_row` as its item finishes, and the Desktop lane runs
/// inside `hooks.desktop`; the rows come back in composition order whichever lane finished
/// first. The phases `base` (the base layer) and `workstation` (the rest, until every lane is
/// done) of `laps` add up to the run's wall time; a lane's own time is the sum of its rows'
/// `elapsed_ms`.
#[expect(
    clippy::too_many_arguments,
    reason = "one run's whole composition, each part its own input"
)]
fn run_layers(
    sys: &dyn System,
    ctx: &Ctx,
    base: Base,
    groups: &[Group],
    develop: &[(Box<dyn Item>, Place)],
    narrowing: &Narrowing<'_>,
    mode: Mode,
    journal: &mut (dyn Journal + Send),
    laps: &mut Laps,
    hooks: &Hooks<'_>,
) -> Result<Vec<ItemReport>, String> {
    let on_row = hooks.on_row;
    let scope = aterm_converge::Scope {
        at_console: ctx.console.is_some(),
        ..aterm_converge::Scope::EVERY
    };
    let Base {
        items,
        held,
        clt,
        toolset,
        ..
    } = base;
    let order: Vec<String> = clt
        .iter()
        .map(Item::id)
        .chain(toolset.iter().map(Item::id))
        .chain(
            groups
                .iter()
                .flat_map(|(items, _)| items.iter().map(|item| item.id())),
        )
        .chain(develop.iter().map(|(item, _)| item.id()))
        .collect();
    let released = Latch::default();
    let gate = AfterBase(&released);
    let (mut spotlight, mut network, mut desktop, mut later) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for (items, place) in groups {
        for item in items {
            let at = (item.as_ref(), place.clone());
            match lane(place.class) {
                Lane::Base => spotlight.push(at),
                Lane::Desktop => desktop.push(at),
                // The vault's join starts at once; what may take the store lock waits behind
                // the gate.
                Lane::Network if item.family() == crate::init_secrets::FAMILY => network.push(at),
                Lane::Network => later.push(at),
            }
        }
    }
    let gated: Vec<(&dyn Item, Place)> = clt
        .as_ref()
        .map(|clt| (clt as &dyn Item, crate::init_clt::place()))
        .into_iter()
        .chain(
            toolset
                .as_ref()
                .map(|toolset| (toolset as &dyn Item, crate::init_toolset::place())),
        )
        .chain(later)
        .chain(
            develop
                .iter()
                .map(|(item, place)| (item.as_ref(), place.clone())),
        )
        .collect();
    if !gated.is_empty() {
        network.push((&gate as &dyn Item, crate::init_clt::place()));
        network.extend(gated);
    }
    let journal = Mutex::new(journal);
    let run = |lane: &[(&dyn Item, Place)]| -> Result<Vec<ItemReport>, String> {
        if lane.is_empty() {
            return Ok(Vec::new());
        }
        let bound = crate::engine::bind(lane.iter().cloned(), sys, ctx);
        let bound = if narrowing.only.is_empty() {
            bound
        } else {
            let mut words = narrowing.only.to_vec();
            if narrowing.record {
                words.push(crate::init_develop::RECORD.to_owned());
            }
            words.push(AFTER_BASE.to_owned());
            aterm_converge::select(bound, &words).0
        };
        let mut rows =
            aterm_converge::run_scoped(&bound, mode, scope, &mut Shared(&journal), &mut |row| {
                if row.id != AFTER_BASE {
                    on_row(row);
                }
            })
            .map_err(|refused| refused.to_string())?;
        rows.retain(|row| row.id != AFTER_BASE);
        Ok(rows)
    };
    let network_lane = || run(&network);
    let desktop_lane = || (hooks.desktop)(&|| run(&desktop));
    let (base_rows, lanes) = std::thread::scope(|threads| {
        let network_thread = std::thread::Builder::new()
            .name("aterm-init-network".into())
            .spawn_scoped(threads, network_lane)
            .ok();
        let desktop_thread = std::thread::Builder::new()
            .name("aterm-init-desktop".into())
            .spawn_scoped(threads, desktop_lane)
            .ok();
        let opens = Opens(&released);
        let base_rows =
            aterm_converge::run_scoped(&items, mode, scope, &mut Shared(&journal), &mut |row| {
                on_row(row);
            })
            .map_err(|refused| refused.to_string());
        drop(held);
        drop(opens);
        laps.lap("base");
        let spotlight_rows = run(&spotlight);
        // A lane whose thread could not start runs here, after the others.
        let joined = |thread: Option<std::thread::ScopedJoinHandle<'_, _>>,
                      inline: &dyn Fn() -> Result<Vec<ItemReport>, String>| {
            match thread {
                Some(thread) => thread
                    .join()
                    .unwrap_or_else(|_| Err("a setup lane stopped unexpectedly".into())),
                None => inline(),
            }
        };
        let network_rows = joined(network_thread, &network_lane);
        let desktop_rows = joined(desktop_thread, &desktop_lane);
        (base_rows, [spotlight_rows, network_rows, desktop_rows])
    });
    let mut rows = base_rows?;
    let mut rest = Vec::new();
    for lane in lanes {
        rest.extend(lane?);
    }
    rest.sort_by_key(|row| order.iter().position(|id| *id == row.id));
    rows.extend(rest);
    laps.lap("workstation");
    Ok(rows)
}

/// Collect only what this setup uses, avoiding fleet, TCC database and unrelated app probes,
/// with why the keyboard list could not be read apart ([`context_reading`]): it leaves only
/// the Caps Lock rows unread, never the run.
fn local_context(sys: &dyn System) -> Result<(Ctx, Option<String>), String> {
    context_reading(sys, WorkstationGroup::All, Selection::all())
}

fn context_for_selected(
    sys: &dyn System,
    group: WorkstationGroup,
    selected: Selection,
) -> Result<Ctx, String> {
    let (ctx, keyboards) = context_reading(sys, group, selected)?;
    keyboards.map_or(Ok(ctx), Err)
}

/// [`context_for_selected`]'s reading, with why the keyboard list (`hidutil list`) could not
/// be read, if it could not, apart: the context then lists no keyboard.
fn context_reading(
    sys: &dyn System,
    group: WorkstationGroup,
    selected: Selection,
) -> Result<(Ctx, Option<String>), String> {
    if sys.euid() == 0 {
        return Err("run as the logged-in user, without sudo".into());
    }
    // The account and the machine, read in this process (no `id`, `dscl`, `uname` or
    // `sw_vers` child).
    let crate::port::Identity {
        user,
        arch,
        os_version,
    } = sys.identity()?;
    if user.uid != sys.euid() {
        return Err(format!(
            "the account database names {} (uid {}), not this process's uid {}",
            user.name,
            user.uid,
            sys.euid()
        ));
    }
    // This login's own console session: it owns `/dev/console` and this process runs in its
    // graphic session, not over SSH (`console::remote`). Otherwise (another login's console,
    // nobody's, a remote shell) nothing of the console session is read.
    let at_console = !crate::console::remote(sys);
    let facts = HostFacts {
        arch,
        os_version,
        console_user: at_console.then(|| user.name.clone()),
        users: vec![user.clone()],
        full_disk_access: sys.full_disk_access(),
        ..HostFacts::default()
    };
    if !at_console {
        return Ok((
            Ctx {
                facts,
                users: vec![user],
                console: None,
                host_name: String::new(),
                cache_dir: PathBuf::new(),
                disrupt: true,
            },
            None,
        ));
    }
    let keyboards = if group == WorkstationGroup::Accounts || !selected.contains(Setting::CapsLock)
    {
        Ok(Vec::new())
    } else {
        let command = crate::facts::hidutil_keyboards();
        sys.run_complete_stdout(&command)
            .map_err(|e| format!("{}: {e}", command.display()))
            .and_then(|keyboard_list| {
                if keyboard_list.success() {
                    crate::facts::parse_hidutil_keyboards_complete(&keyboard_list.out())
                } else {
                    Err(format!(
                        "{}: {}",
                        command.display(),
                        keyboard_list.describe()
                    ))
                }
            })
    };
    let (keyboards, unread) = match keyboards {
        Ok(keyboards) => (keyboards, None),
        Err(why) => (Vec::new(), Some(why)),
    };
    let facts = HostFacts { keyboards, ..facts };
    Ok((
        Ctx {
            facts,
            users: vec![user.clone()],
            console: Some(user),
            host_name: String::new(),
            cache_dir: PathBuf::new(),
            disrupt: true,
        },
        unread,
    ))
}

fn complete(rows: &[ItemReport], action: Action) -> bool {
    !rows.is_empty() && rows.iter().all(|row| counts(row, action))
}

/// Whether `row` counts as done for `action`: a row that waits on an admin's approval does
/// ([`waits_by_itself`]). For a run that is not `--check`, a grant only the person switches on
/// ([`is_grant`]) and the workstation rows deferred to a person at this Mac's console
/// ([`waits_for_a_person`]) are not this run's to finish, so an apply or a plan completes with
/// them, naming them; `--check` still reads them as not verified.
fn counts(row: &ItemReport, action: Action) -> bool {
    matches!(
        row.outcome,
        Outcome::Ok | Outcome::Changed | Outcome::NotApplicable
    ) || (action == Action::Plan && row.outcome == Outcome::Planned)
        || waits_by_itself(row)
        || (action != Action::Check && (is_grant(row) || waits_for_a_person(row)))
}

/// Whether `row` is one of the Permissions phase's grants ([`Permission`]) waiting for the
/// person: only they switch it on, so an apply or a plan is done with it, naming it; `--check`
/// still reads it as not in place.
fn is_grant(row: &ItemReport) -> bool {
    row.id.starts_with(PERMISSION) && row.outcome == Outcome::Person
}

const fn word(outcome: Outcome) -> &'static str {
    match outcome {
        Outcome::Ok => "ok",
        Outcome::Changed => "changed",
        Outcome::Planned => "planned",
        Outcome::Person => "needs action",
        Outcome::Blocked => "blocked",
        Outcome::NotApplicable => "not applicable",
        Outcome::Deferred => "deferred",
        Outcome::Unknown => "unknown",
        Outcome::Failed => "failed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::items::defaults::tests::{ctx, export, user};
    use crate::model::{Effect, Probe};
    use crate::port::{Fake, Ran, Stat};
    use aterm_converge::{FileJournal, NoJournal};
    use std::sync::Mutex;

    /// A machines item whose probes answer from a list (the last one repeats).
    struct Listed {
        id: String,
        probes: Mutex<Vec<Probe>>,
    }

    impl Listed {
        fn boxed(id: impl Into<String>, probes: Vec<Probe>) -> Box<dyn Item> {
            Box::new(Self {
                id: id.into(),
                probes: Mutex::new(probes),
            })
        }
    }

    impl Item for Listed {
        fn id(&self) -> String {
            self.id.clone()
        }
        fn family(&self) -> &'static str {
            "test"
        }
        fn title(&self) -> String {
            self.id.clone()
        }
        fn probe(&self, _: &dyn System, _: &Ctx) -> Probe {
            let mut probes = self.probes.lock().unwrap();
            if probes.len() > 1 {
                probes.remove(0)
            } else {
                probes[0].clone()
            }
        }
        fn apply(&self, _: &dyn System, _: &Ctx) -> Result<(), String> {
            Ok(())
        }
        fn revert(&self) -> Option<String> {
            Some(format!("undo {}", self.id))
        }
    }

    fn ok() -> Probe {
        Probe::Ok { now: "b".into() }
    }

    fn change() -> Probe {
        Probe::Change {
            from: "a".into(),
            to: "b".into(),
        }
    }

    fn verified_accounts(ctx: &Ctx) -> Vec<Box<dyn Item>> {
        crate::init_accounts::items(ctx)
            .unwrap()
            .into_iter()
            .map(|item| Listed::boxed(item.id(), vec![ok()]))
            .collect()
    }

    /// [`prepare_groups`] for an apply with no flag, no `--only`, no vault look and no grant.
    fn prepared(sys: &dyn System, ctx: &Ctx) -> Vec<Group> {
        prepare_groups(
            sys,
            ctx,
            Mode::Apply,
            None,
            None,
            &Reading {
                keyboards: None,
                only: &[],
                choice: None,
            },
            no_grants,
        )
        .unwrap()
        .0
    }

    #[test]
    fn an_empty_selection_prepares_and_runs_without_any_host_probe() {
        let sys = Fake::root();
        let pass =
            prepare_workstation_selected(&sys, WorkstationGroup::All, Selection::none()).unwrap();
        let rows = pass
            .run_recording_with(&sys, Mode::Plan, &mut NoJournal)
            .unwrap()
            .rows;
        assert!(rows.is_empty());
        assert!(
            sys.calls().is_empty(),
            "even account and input facts remain unread"
        );
    }

    /// The widget, desktop and Dock items of one pass share its one relaunch: one
    /// `DesktopActivation` across the whole selection, as one
    /// `init_preferences::selected_tagged_items` call builds it.
    #[test]
    fn the_desktop_items_of_one_pass_share_one_activation() {
        let who = user("alice", 501);
        let context = ctx(&[who]);
        let items =
            workstation_items_selected(&context, WorkstationGroup::All, Selection::all()).unwrap();
        let mut order: Vec<Setting> = items.iter().map(|(setting, _)| *setting).collect();
        order.dedup();
        assert_eq!(
            order,
            [
                Setting::CapsLock,
                Setting::Widgets,
                Setting::Desktop,
                Setting::Dock,
                Setting::NaturalScrolling,
                Setting::TrackpadScrolling,
                Setting::Messages,
                Setting::FaceTime,
            ]
        );
        assert!(
            items
                .iter()
                .filter(|(setting, _)| *setting == Setting::CapsLock)
                .all(|(_, item)| item.family() == "keyboard"),
        );
        let (settings, activations): (Vec<Setting>, Vec<_>) = items
            .iter()
            .filter_map(|(setting, item)| Some((*setting, item.activation()?)))
            .unzip();
        assert_eq!(
            settings,
            [
                Setting::Widgets,
                Setting::Widgets,
                Setting::Widgets,
                Setting::Desktop,
                Setting::Dock
            ]
        );
        let ids: Vec<String> = activations.iter().map(|a| a.id()).collect();
        assert!(
            activations
                .windows(2)
                .all(|w| std::sync::Arc::ptr_eq(&w[0], &w[1])),
            "one DesktopActivation per pass, not one per setting (ids {ids:?})"
        );
    }

    /// The row a preference key belongs to is read off the keys the items write: every key an
    /// item of a full-profile pass would change names the row that item reports to, so a
    /// Restore can say what it put back in System State's row names.
    #[test]
    fn every_key_an_item_changes_names_the_row_it_reports_to() {
        let who = user("alice", 501);
        let mut context = ctx(&[who]);
        context.facts.full_disk_access = Some(true);
        context.facts.arch = "arm64".into();
        let sys = Fake::root();
        crate::init_preferences::tests::installed_dock_apps(&sys, &context.users[0]);
        sys.simulate(|_, cmd| {
            let line = cmd.display();
            (line.contains("/usr/bin/defaults") && line.contains(" export "))
                .then(|| export("<dict/>"))
        });
        let items =
            workstation_items_selected(&context, WorkstationGroup::Preferences, Selection::all())
                .unwrap();
        let mut named = Vec::new();
        for (setting, item) in &items {
            for (target, _) in item.preference_changes(&sys, &context).unwrap() {
                assert_eq!(
                    crate::init_preferences::setting_of_preference(&target.domain, &target.key),
                    Some(*setting),
                    "{}",
                    target.describe()
                );
                named.push(*setting);
            }
        }
        for setting in [
            Setting::CapsLock,
            Setting::NaturalScrolling,
            Setting::TrackpadScrolling,
            Setting::Desktop,
            Setting::Widgets,
            Setting::Dock,
        ] {
            assert!(named.contains(&setting), "no key of {setting:?} was read");
        }
        assert_eq!(
            crate::init_preferences::setting_of_preference("com.apple.dock", "no-such-key"),
            None
        );
    }

    #[test]
    fn selected_widgets_do_not_read_keyboard_dock_trackpad_or_accounts() {
        let who = user("alice", 501);
        for (full_disk_access, keys) in [(Some(true), 3), (Some(false), 2), (None, 2)] {
            let mut context = ctx(std::slice::from_ref(&who));
            context.facts.full_disk_access = full_disk_access;
            let sys = Fake::root();
            sys.on(
                "alice$ /usr/bin/defaults export com.apple.WindowManager -",
                export("<dict/>"),
            );
            sys.on(
                "alice$ /usr/bin/defaults export com.apple.notificationcenterui -",
                export("<dict/>"),
            );
            let mut selected = Selection::none();
            selected.set(Setting::Widgets, true);
            let items =
                workstation_items_selected(&context, WorkstationGroup::All, selected).unwrap();
            let ids: Vec<String> = items.iter().map(|(_, item)| item.id()).collect();
            assert_eq!(
                ids,
                [
                    "consumer.widgets.desktop@alice",
                    "consumer.widgets.stage-manager@alice",
                    "consumer.widgets.notification-center@alice"
                ]
            );
            assert!(
                items
                    .iter()
                    .all(|(setting, _)| *setting == Setting::Widgets)
            );
            let mut snapshots = 0;
            for (_, item) in &items {
                snapshots += item.preference_changes(&sys, &context).unwrap().len();
            }
            assert_eq!(snapshots, keys, "{full_disk_access:?}");
            for (_, item) in &items[..2] {
                assert!(matches!(
                    item.probe(&sys, &context),
                    Probe::Change { .. } | Probe::Deferred { .. }
                ));
            }
            let calls = sys.calls();
            for call in &calls {
                assert!(
                    call.contains("com.apple.WindowManager")
                        || (full_disk_access == Some(true)
                            && call.contains("com.apple.notificationcenterui"))
                        || call.contains("/init-activation/desktopwidgets.pending")
                        || call.contains("/init-activation/widgets.pending"),
                    "{full_disk_access:?}: unchecked setting queried: {call}"
                );
            }
            assert_eq!(
                calls
                    .iter()
                    .any(|call| call.contains("com.apple.notificationcenterui")),
                full_disk_access == Some(true),
                "{calls:?}"
            );
        }
    }

    #[test]
    fn the_runners_full_disk_access_reaches_the_widgets_row_through_the_port() {
        for full_disk_access in [None, Some(false), Some(true)] {
            let mut sys = Fake::root();
            sys.euid = 501;
            sys.full_disk_access = full_disk_access;
            sys.logged_in(&crate::items::defaults::tests::user("alice", 501));
            sys.stat_of(
                "/dev/console",
                Stat {
                    uid: 501,
                    gid: 20,
                    mode: 0o600,
                    dir: false,
                    symlink: false,
                    len: 0,
                },
            );
            sys.on(
                "alice$ /usr/bin/defaults export com.apple.WindowManager -",
                export("<dict><key>StandardHideWidgets</key><true/><key>StageManagerHideWidgets</key><true/></dict>"),
            );
            sys.on(
                "alice$ /usr/bin/defaults export com.apple.notificationcenterui -",
                export("<dict><key>widgets</key><dict><key>instances</key><array/></dict></dict>"),
            );
            let mut selected = Selection::none();
            selected.set(Setting::Widgets, true);
            let pass = prepare_workstation_selected(&sys, WorkstationGroup::All, selected).unwrap();
            let rows = pass
                .run_recording_with(&sys, Mode::Plan, &mut NoJournal)
                .unwrap()
                .rows;
            assert_eq!(rows.len(), 3);
            assert!(
                rows.iter().all(|row| row.outcome == Outcome::Ok),
                "{rows:?}"
            );
            let granted = full_disk_access == Some(true);
            assert_eq!(
                rows[2].title.contains("Full Disk Access"),
                !granted,
                "{}",
                rows[2].title
            );
            assert_eq!(
                sys.calls()
                    .iter()
                    .any(|call| call.contains("com.apple.notificationcenterui")),
                granted,
                "{:?}",
                sys.calls()
            );
        }
    }

    #[test]
    fn the_withheld_group_reaches_only_the_keys_a_pass_without_the_grant_left_alone() {
        assert_eq!(
            WorkstationGroup::Withheld.selected(Selection::all()),
            WorkstationGroup::Preferences.selected(Selection::all())
        );
        let mut selected = Selection::none();
        selected.set(Setting::Widgets, true);
        selected.set(Setting::Dock, true);
        let console = |full_disk_access| {
            let mut sys = Fake::root();
            sys.euid = 501;
            sys.full_disk_access = full_disk_access;
            sys.logged_in(&crate::items::defaults::tests::user("alice", 501));
            sys.stat_of(
                "/dev/console",
                Stat {
                    uid: 501,
                    gid: 20,
                    mode: 0o600,
                    dir: false,
                    symlink: false,
                    len: 0,
                },
            );
            sys.on(
                "alice$ /usr/bin/defaults export com.apple.notificationcenterui -",
                export("<dict><key>widgets</key><dict><key>instances</key><array><data>Kg==</data></array></dict></dict>"),
            );
            sys
        };
        let sys = console(Some(true));
        let mut follow_up =
            prepare_workstation_selected(&sys, WorkstationGroup::Withheld, selected).unwrap();
        let snapshot = follow_up.preference_changes(&sys).unwrap();
        assert_eq!(snapshot.len(), 1, "{snapshot:?}");
        assert_eq!(snapshot[0].0.domain, "com.apple.notificationcenterui");
        let run = follow_up
            .run_recording_with(&sys, Mode::Plan, &mut NoJournal)
            .unwrap();
        let rows: Vec<(&str, &str, Outcome)> = run
            .rows
            .iter()
            .map(|row| (row.id.as_str(), row.title.as_str(), row.outcome))
            .collect();
        assert_eq!(
            rows,
            [(
                "consumer.widgets.notification-center@alice",
                "No Notification Center widgets",
                Outcome::Planned
            )]
        );
        assert!(run.withheld.is_empty());
        for call in sys.calls() {
            assert!(
                !call.contains("com.apple.WindowManager") && !call.contains("com.apple.dock"),
                "{call}"
            );
        }

        let sys = console(None);
        sys.on(
            "alice$ /usr/bin/defaults export com.apple.WindowManager -",
            export("<dict/>"),
        );
        let mut widgets = Selection::none();
        widgets.set(Setting::Widgets, true);
        let pass =
            prepare_workstation_selected(&sys, WorkstationGroup::Preferences, widgets).unwrap();
        let run = pass
            .run_recording_with(&sys, Mode::Plan, &mut NoJournal)
            .unwrap();
        assert_eq!(run.withheld, ["consumer.widgets.notification-center@alice"]);
        let sys = console(Some(true));
        sys.on(
            "alice$ /usr/bin/defaults export com.apple.WindowManager -",
            export("<dict/>"),
        );
        let mut selected = Selection::none();
        selected.set(Setting::Widgets, true);
        let mut all =
            prepare_workstation_selected(&sys, WorkstationGroup::Preferences, selected).unwrap();
        assert_eq!(all.preference_changes(&sys).unwrap().len(), 3);
        assert!(
            sys.calls()
                .iter()
                .any(|call| call.contains("com.apple.WindowManager"))
        );
    }

    #[test]
    fn a_single_selected_account_needs_no_keyboard_inventory_or_other_service_read() {
        let mut sys = Fake::root();
        sys.euid = 501;
        sys.logged_in(&crate::items::defaults::tests::user("alice", 501));
        sys.stat_of(
            "/dev/console",
            Stat {
                uid: 501,
                gid: 20,
                mode: 0o600,
                dir: false,
                symlink: false,
                len: 0,
            },
        );
        sys.on(
            "alice$ /usr/bin/defaults export com.apple.imservice.ids.iMessage -",
            export("<dict/>"),
        );
        let mut selected = Selection::none();
        selected.set(Setting::Messages, true);
        let pass = prepare_workstation_selected(&sys, WorkstationGroup::All, selected).unwrap();
        let rows = pass
            .run_recording_with(&sys, Mode::Plan, &mut NoJournal)
            .unwrap()
            .rows;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].outcome, Outcome::Ok, "{:?}", rows[0].why);
        for call in sys.calls() {
            assert!(
                !call.contains("hidutil")
                    && !call.contains("FaceTime")
                    && !call.contains("osascript")
                    && !call.contains("scroll-direction"),
                "unchecked setting queried: {call}"
            );
        }
        let inventory = crate::facts::hidutil_keyboards().display();
        sys.on(
            &inventory,
            Ran::ok(
                "Services:\nVendorID ProductID Product  Built-In\n0x5ac    0x1       Keyboard 1\n",
            ),
        );
        let account_only = initial_setup_context(&sys).unwrap();
        assert!(account_only.facts.keyboards.is_empty());
        assert!(!sys.calls().contains(&inventory));
    }

    fn saved_off(sys: &Fake) {
        sys.on(
            "alice$ /usr/bin/defaults export -g -",
            export("<dict><key>com.apple.swipescrolldirection</key><false/></dict>"),
        );
    }

    fn at_console(alice: crate::model::User) -> Ctx {
        Ctx {
            console: Some(alice.clone()),
            ..ctx(&[alice])
        }
    }

    /// A base-layer item (no console needed) whose probes answer from a list.
    struct BaseItem {
        probes: Mutex<Vec<Probe>>,
        applied: std::sync::Arc<std::sync::atomic::AtomicU32>,
    }

    impl aterm_converge::Item for BaseItem {
        fn id(&self) -> String {
            "base.pkg.shims".into()
        }
        fn family(&self) -> &'static str {
            "pkg"
        }
        fn layer(&self) -> Layer {
            Layer::Base
        }
        fn class(&self) -> Class {
            Class::Relay
        }
        fn title(&self) -> String {
            "the installed programs' commands".into()
        }
        fn probe(&self) -> Probe {
            let mut probes = self.probes.lock().unwrap();
            if probes.len() > 1 {
                probes.remove(0)
            } else {
                probes[0].clone()
            }
        }
        fn apply(&self) -> Result<(), String> {
            self.applied
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
        fn explain(&self, probe: &Probe) -> aterm_converge::Explain {
            aterm_converge::Explain {
                why: format!("{probe:?}"),
                remedy: Remedy::Repair,
            }
        }
    }

    fn place(class: Class) -> Place {
        Place {
            layer: Layer::Workstation,
            class,
            requires: Requires::Console,
            remedy: Remedy::Command("aterm init".into()),
        }
    }

    fn journaled(journal: &FileJournal) -> Vec<aterm_json::Value> {
        match std::fs::read_to_string(journal.path()) {
            Ok(text) => text
                .lines()
                .map(|line| aterm_json::from_str(line).unwrap())
                .collect(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => panic!("{}: {e}", journal.path().display()),
        }
    }

    #[test]
    fn a_workstation_pass_journals_in_the_journal_of_the_system_it_runs_on() {
        let sys = Fake::root();
        let pass = Workstation {
            items: vec![
                Listed::boxed("init.test.changes", vec![change(), ok()]),
                Listed::boxed("init.test.right", vec![ok()]),
            ],
            ctx: ctx(&[user("alice", 501)]),
            snapshots: None,
            settings: Vec::new(),
            held: Vec::new(),
        };
        let rows = pass.run_recording(&sys, Mode::Apply).unwrap().rows;
        assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
        let text =
            String::from_utf8(sys.written(Fake::JOURNAL).expect("the fake's journal")).unwrap();
        let ids: Vec<String> = text
            .lines()
            .map(|line| {
                let line: aterm_json::Value = aterm_json::from_str(line).unwrap();
                line["id"].as_str().unwrap().to_owned()
            })
            .collect();
        assert_eq!(ids, ["init.test.changes"]);
    }

    struct Counted {
        id: &'static str,
        probes: Mutex<Vec<Probe>>,
        applies: Arc<std::sync::atomic::AtomicU32>,
        refreshes: Arc<std::sync::atomic::AtomicU32>,
        aside: Option<aterm_converge::StoodAside>,
        /// The preference keys it would change, or why it cannot say
        /// ([`Item::preference_changes`]), one answer per call.
        keys: Mutex<Vec<Result<PreferenceSnapshot, String>>>,
    }

    impl Counted {
        fn boxed(
            id: &'static str,
            probes: Vec<Probe>,
        ) -> (Box<dyn Item>, [Arc<std::sync::atomic::AtomicU32>; 2]) {
            Self::aside(id, probes, None)
        }

        fn aside(
            id: &'static str,
            probes: Vec<Probe>,
            aside: Option<aterm_converge::StoodAside>,
        ) -> (Box<dyn Item>, [Arc<std::sync::atomic::AtomicU32>; 2]) {
            let counts = [Arc::default(), Arc::default()];
            let item = Box::new(Self {
                id,
                probes: Mutex::new(probes),
                applies: Arc::clone(&counts[0]),
                refreshes: Arc::clone(&counts[1]),
                aside,
                keys: Mutex::new(vec![Ok(Vec::new())]),
            });
            (item, counts)
        }
    }

    impl Item for Counted {
        fn id(&self) -> String {
            self.id.into()
        }
        fn family(&self) -> &'static str {
            "test"
        }
        fn title(&self) -> String {
            self.id.into()
        }
        fn probe(&self, _: &dyn System, _: &Ctx) -> Probe {
            let mut probes = self.probes.lock().unwrap();
            if probes.len() > 1 {
                probes.remove(0)
            } else {
                probes[0].clone()
            }
        }
        fn apply(&self, _: &dyn System, _: &Ctx) -> Result<(), String> {
            self.applies
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
        fn refresh(&self, _: &dyn System, _: &Ctx) -> Option<Result<(), String>> {
            self.refreshes
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Some(Ok(()))
        }
        fn stood_aside(&self) -> Option<aterm_converge::StoodAside> {
            self.aside.clone()
        }
        fn preference_changes(
            &self,
            _: &dyn System,
            _: &Ctx,
        ) -> Result<Vec<(Target, Read<PValue>)>, String> {
            let mut keys = self.keys.lock().unwrap();
            if keys.len() > 1 {
                keys.remove(0)
            } else {
                keys[0].clone()
            }
        }
    }

    #[test]
    fn a_held_item_has_no_before_images() {
        let alice = user("alice", 501);
        let item = |id: &'static str, domain: &str| {
            Box::new(Counted {
                id,
                probes: Mutex::new(vec![change()]),
                applies: Arc::default(),
                refreshes: Arc::default(),
                aside: None,
                keys: Mutex::new(vec![Ok(vec![(
                    Target::user(domain, "k", &alice),
                    Read::Absent,
                )])]),
            }) as Box<dyn Item>
        };
        let mut pass = Workstation {
            items: vec![
                item("init.test.dock", "com.apple.dock"),
                item("init.test.widgets", "com.apple.WindowManager"),
            ],
            ctx: ctx(std::slice::from_ref(&alice)),
            snapshots: None,
            settings: Vec::new(),
            held: Vec::new(),
        };
        pass.hold(&[("init.test.dock".into(), Hold::Back("held".into()))]);
        let before = pass.preference_changes(&Fake::root()).unwrap();
        let domains: Vec<&str> = before.iter().map(|(t, _)| t.domain.as_str()).collect();
        assert_eq!(domains, ["com.apple.WindowManager"]);
        assert_eq!(
            pass.snapshots.as_ref().map(Vec::len),
            Some(2),
            "one snapshot per item, in item order"
        );
    }

    #[test]
    fn an_item_without_before_images_changes_nothing_and_the_others_run() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let alice = user("alice", 501);
        let item = |id: &'static str, keys| {
            let applies = Arc::new(AtomicU32::new(0));
            let item = Box::new(Counted {
                id,
                probes: Mutex::new(vec![change(), ok()]),
                applies: Arc::clone(&applies),
                refreshes: Arc::default(),
                aside: None,
                keys: Mutex::new(keys),
            }) as Box<dyn Item>;
            (item, applies)
        };
        let target = |domain: &str| Target::user(domain, "k", &alice);
        let (cleared, cleared_applies) = item(
            "init.test.cleared",
            vec![
                Err("plutil timed out; left untouched".into()),
                Ok(vec![(target("com.apple.dock"), Read::Absent)]),
            ],
        );
        let (unusable, unusable_applies) = item(
            "init.test.unusable",
            vec![Ok(vec![(
                target("com.apple.finder"),
                Read::Unusable("cfprefsd timed out".into()),
            )])],
        );
        let (widgets, widgets_applies) = item(
            "init.test.widgets",
            vec![Ok(vec![(target("com.apple.WindowManager"), Read::Absent)])],
        );
        let mut pass = Workstation {
            items: vec![cleared, unusable, widgets],
            ctx: ctx(std::slice::from_ref(&alice)),
            settings: Vec::new(),
            snapshots: None,
            held: Vec::new(),
        };
        let sys = Fake::root();
        let before = pass.preference_changes(&sys).unwrap();
        let domains: Vec<&str> = before.iter().map(|(t, _)| t.domain.as_str()).collect();
        assert_eq!(domains, ["com.apple.WindowManager"]);
        let run = pass
            .run_recording_with(&sys, Mode::Apply, &mut NoJournal)
            .unwrap();
        let outcome = |id: &str| {
            let row = run.rows.iter().find(|row| row.id == id).unwrap();
            (row.outcome, row.why.clone().unwrap_or_default())
        };
        let (cleared_outcome, cleared_why) = outcome("init.test.cleared");
        assert_eq!(cleared_outcome, Outcome::Failed);
        assert!(cleared_why.contains("plutil timed out"), "{cleared_why}");
        let (unusable_outcome, unusable_why) = outcome("init.test.unusable");
        assert_eq!(unusable_outcome, Outcome::Failed);
        assert!(unusable_why.contains("read for recovery"), "{unusable_why}");
        assert_eq!(outcome("init.test.widgets").0, Outcome::Changed);
        assert_eq!(
            cleared_applies.load(Ordering::SeqCst),
            0,
            "no unbacked write"
        );
        assert_eq!(
            unusable_applies.load(Ordering::SeqCst),
            0,
            "no unbacked write"
        );
        assert_eq!(widgets_applies.load(Ordering::SeqCst), 1);
        let attempts: Vec<&str> = run.attempts.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(attempts, ["init.test.widgets"]);
    }

    /// The ids of the fake's journal lines, in order.
    fn journaled_ids(sys: &Fake) -> Vec<String> {
        let text =
            String::from_utf8(sys.written(Fake::JOURNAL).expect("the fake's journal")).unwrap();
        text.lines()
            .map(|line| {
                let line: aterm_json::Value = aterm_json::from_str(line).unwrap();
                line["id"].as_str().unwrap().to_owned()
            })
            .collect()
    }

    #[test]
    fn a_held_item_changes_nothing_and_only_reached_items_are_attempts() {
        use std::sync::atomic::Ordering;
        let sys = Fake::root();
        let (held, held_counts) = Counted::boxed("init.test.held", vec![change()]);
        let (held_ok, held_ok_counts) = Counted::boxed("init.test.held-ok", vec![ok()]);
        let (as_is, as_is_counts) = Counted::boxed("init.test.as-is", vec![change()]);
        let (free, free_counts) = Counted::boxed("init.test.free", vec![change(), ok()]);
        let (fails, fails_counts) = Counted::boxed("init.test.fails", vec![change()]);
        let mut pass = Workstation {
            items: vec![
                held,
                held_ok,
                as_is,
                free,
                fails,
                Listed::boxed("init.test.right", vec![ok()]),
            ],
            ctx: ctx(&[user("alice", 501)]),
            snapshots: None,
            settings: Vec::new(),
            held: Vec::new(),
        };
        pass.hold(&[
            (
                "init.test.held".into(),
                Hold::Back("held by the test".into()),
            ),
            (
                "init.test.held-ok".into(),
                Hold::Back("held by the test".into()),
            ),
            ("init.test.as-is".into(), Hold::AsIs("changed since".into())),
        ]);
        let run = pass.run_recording(&sys, Mode::Apply).unwrap();
        let outcome = |id: &str| {
            let row = run.rows.iter().find(|row| row.id == id).unwrap();
            (row.outcome, row.why.clone())
        };
        assert_eq!(
            outcome("init.test.held"),
            (
                Outcome::Blocked,
                Some("a; not changed: held by the test".into())
            )
        );
        assert_eq!(outcome("init.test.held-ok").0, Outcome::Ok);
        assert_eq!(
            outcome("init.test.as-is"),
            (
                Outcome::NotApplicable,
                Some("a; left as it is: changed since".into())
            )
        );
        assert_eq!(outcome("init.test.free").0, Outcome::Changed);
        assert_eq!(outcome("init.test.fails").0, Outcome::Failed);
        let counts = |counts: &[Arc<std::sync::atomic::AtomicU32>; 2]| {
            counts.each_ref().map(|count| count.load(Ordering::SeqCst))
        };
        assert_eq!(counts(&held_counts), [0, 0], "a held change never runs");
        assert_eq!(counts(&held_ok_counts), [0, 0], "nor a held refresh");
        assert_eq!(counts(&as_is_counts), [0, 0], "nor one left as it is");
        assert_eq!(counts(&free_counts), [1, 0]);
        assert_eq!(counts(&fails_counts), [1, 0]);
        assert_eq!(journaled_ids(&sys), ["init.test.free", "init.test.fails"]);
        let attempts: Vec<(&str, bool)> = run
            .attempts
            .iter()
            .map(|attempt| (attempt.id.as_str(), attempt.verified))
            .collect();
        assert_eq!(
            attempts,
            [("init.test.free", true), ("init.test.fails", false)],
            "an Ok item with no refresh, and a held one, reached nothing"
        );
    }

    #[test]
    fn an_apply_that_stood_aside_is_no_attempt() {
        let sys = Fake::root();
        let (aside, counts) = Counted::aside(
            "init.test.aside",
            vec![change()],
            Some(aterm_converge::StoodAside::Permission(
                "Accessibility is not granted".into(),
            )),
        );
        let (fails, _) = Counted::boxed("init.test.fails", vec![change()]);
        let pass = Workstation {
            items: vec![aside, fails],
            ctx: ctx(&[user("alice", 501)]),
            snapshots: None,
            settings: Vec::new(),
            held: Vec::new(),
        };
        let run = pass.run_recording(&sys, Mode::Apply).unwrap();
        let row = run
            .rows
            .iter()
            .find(|row| row.id == "init.test.aside")
            .unwrap();
        assert_eq!(row.outcome, Outcome::Blocked, "{row:?}");
        assert!(row.stood_aside.is_some(), "{row:?}");
        assert_eq!(counts[0].load(std::sync::atomic::Ordering::SeqCst), 1);
        let attempts: Vec<&str> = run.attempts.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(attempts, ["init.test.fails"], "{:?}", run.attempts);
    }

    #[test]
    fn init_on_a_fake_system_journals_one_line_per_changed_item() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let mut journal = FileJournal::under(scratch.path());
        let alice = at_console(user("alice", 501));
        let sys = Fake::root();
        sys.scroll_state(true);
        saved_off(&sys);
        let groups = || {
            vec![
                (
                    vec![
                        Listed::boxed("init.test.changes", vec![change(), ok()]),
                        Listed::boxed("init.test.right", vec![ok()]),
                    ],
                    place(Class::Preference),
                ),
                (
                    crate::init_trackpad::items(&alice).unwrap(),
                    place(Class::Preference),
                ),
            ]
        };
        let before = Fake::root();
        before.scroll_state(true);
        saved_off(&before);
        let planned = run(
            &before,
            &alice,
            Base::none(),
            &groups(),
            Mode::Plan,
            &mut NoJournal,
        )
        .unwrap();
        let rows = run(
            &sys,
            &alice,
            Base::none(),
            &groups(),
            Mode::Apply,
            &mut journal,
        )
        .unwrap();
        assert_eq!(
            rows.iter().map(|r| r.outcome).collect::<Vec<_>>(),
            [Outcome::Changed, Outcome::Ok, Outcome::Changed]
        );
        let lines = journaled(&journal);
        assert_eq!(
            lines
                .iter()
                .map(|l| l["id"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["init.test.changes", "init.trackpad.scroll-direction@alice"]
        );
        assert_eq!(
            planned
                .iter()
                .filter(|r| r.outcome == Outcome::Planned)
                .count(),
            lines.len()
        );
        for ((line, requested), row) in lines
            .iter()
            .zip(planned.iter().filter(|r| r.outcome == Outcome::Planned))
            .zip(rows.iter().filter(|r| r.outcome == Outcome::Changed))
        {
            assert_eq!(line["id"].as_str(), Some(requested.id.as_str()));
            assert_eq!(requested.id, row.id);
            assert_eq!(line["from"].as_str(), row.from.as_deref());
            assert_eq!(line["from"].as_str(), requested.from.as_deref());
            assert_eq!(line["to"].as_str(), requested.to.as_deref());
            assert_eq!(line["revert"].as_str(), row.revert.as_deref());
        }
        assert!(rows.iter().all(|r| r.layer == Some(Layer::Workstation)));
    }

    #[test]
    fn what_the_base_layer_holds_is_released_before_an_item_that_may_take_it_is_read() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
        struct Held(Arc<AtomicBool>);
        impl Drop for Held {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        struct Sees {
            released: Arc<AtomicBool>,
            saw: Arc<Mutex<Vec<bool>>>,
        }
        impl Item for Sees {
            fn id(&self) -> String {
                "init.test.sees".into()
            }
            fn family(&self) -> &'static str {
                "test"
            }
            fn title(&self) -> String {
                self.id()
            }
            fn probe(&self, _: &dyn System, _: &Ctx) -> Probe {
                self.saw
                    .lock()
                    .unwrap()
                    .push(self.released.load(Ordering::SeqCst));
                ok()
            }
            fn apply(&self, _: &dyn System, _: &Ctx) -> Result<(), String> {
                Ok(())
            }
        }
        let released = Arc::new(AtomicBool::new(false));
        let saw = Arc::new(Mutex::new(Vec::new()));
        let applied = Arc::new(AtomicU32::new(0));
        let base = Base {
            items: vec![Box::new(BaseItem {
                probes: Mutex::new(vec![change(), ok()]),
                applied: Arc::clone(&applied),
            })],
            held: Some(Box::new(Held(Arc::clone(&released)))),
            clt: None,
            toolset: None,
            targo: None,
        };
        let groups = vec![(
            vec![Box::new(Sees {
                released: Arc::clone(&released),
                saw: Arc::clone(&saw),
            }) as Box<dyn Item>],
            place(Class::Install),
        )];
        let alice = at_console(user("alice", 501));
        let scratch = aterm_tempfile::tempdir().unwrap();
        let mut journal = FileJournal::under(scratch.path());
        let rows = run(
            &Fake::root(),
            &alice,
            base,
            &groups,
            Mode::Apply,
            &mut journal,
        )
        .unwrap();
        assert_eq!(
            rows.iter()
                .map(|row| (row.id.as_str(), row.outcome))
                .collect::<Vec<_>>(),
            [
                ("base.pkg.shims", Outcome::Changed),
                ("init.test.sees", Outcome::Ok)
            ]
        );
        assert_eq!(applied.load(Ordering::SeqCst), 1);
        let saw = saw.lock().unwrap();
        assert!(
            !saw.is_empty() && saw.iter().all(|&released| released),
            "{saw:?}"
        );
    }

    #[test]
    fn init_journals_nothing_when_every_item_is_already_right() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let mut journal = FileJournal::under(scratch.path());
        let alice = at_console(user("alice", 501));
        let sys = Fake::root();
        sys.scroll_state(false);
        saved_off(&sys);
        let groups = vec![
            (
                vec![Listed::boxed("init.test.right", vec![ok()])],
                place(Class::Preference),
            ),
            (
                crate::init_trackpad::items(&alice).unwrap(),
                place(Class::Preference),
            ),
        ];
        let rows = run(
            &sys,
            &alice,
            Base::none(),
            &groups,
            Mode::Apply,
            &mut journal,
        )
        .unwrap();
        assert!(rows.iter().all(|r| r.outcome == Outcome::Ok), "{rows:?}");
        assert!(sys.calls().contains(&"scroll-direction set false".into()));
        assert!(journaled(&journal).is_empty());
        assert!(!scratch.path().join(FileJournal::DIR).exists());
    }

    #[test]
    fn init_places_every_item_at_the_workstation_layer_run_at_the_console() {
        let alice = ctx(&[user("alice", 501)]);
        let mut sys = Fake::root();
        sys.euid = 501;
        let profile =
            b"[github]\naccount='octocat'\nowner='octocat'\ntoken_file='~/.secrets/github'\n";
        let path = "/Users//alice/.config/aterm/init.toml";
        sys.file(path, profile).stat_of(
            path,
            Stat {
                uid: 501,
                gid: 20,
                mode: 0o600,
                dir: false,
                symlink: false,
                len: profile.len() as u64,
            },
        );
        let groups = groups(
            &alice,
            Presence::AtConsole,
            crate::init_secrets::Secrets {
                items: Vec::new(),
                waited_on: None,
            },
            crate::init_github::items(&sys, &alice, None).unwrap(),
            None,
            Choice::ALL,
            &[],
        );
        let bound = crate::engine::bind(
            groups.iter().flat_map(|(items, place)| {
                items.iter().map(move |item| (item.as_ref(), place.clone()))
            }),
            &sys,
            &alice,
        );
        let github: Vec<String> = bound
            .iter()
            .filter(|b| b.class() == Class::Install)
            .map(|b| b.id())
            .collect();
        assert_eq!(
            github.len(),
            2,
            "the GitHub sign-in and checkouts: {github:?}"
        );
        assert!(github.iter().all(|id| id.contains("github")), "{github:?}");
        let asked: Vec<String> = bound
            .iter()
            .filter(|b| b.class() == Class::Asked)
            .map(|b| b.id())
            .collect();
        assert_eq!(
            asked,
            [
                "init.spotlight.rustup-profile@alice",
                "init.spotlight.rust-docs@alice",
                "init.spotlight.build-noindex@alice"
            ]
        );
        for b in &bound {
            assert_eq!(b.layer(), Layer::Workstation, "{}", b.id());
            assert_eq!(b.requires(), Requires::Console, "{}", b.id());
            assert!(
                matches!(
                    b.class(),
                    Class::Preference | Class::Install | Class::Asked | Class::Elevate
                ),
                "{}",
                b.id()
            );
        }
    }

    #[test]
    fn a_snapshotted_pass_refuses_new_or_retyped_preferences_before_mutation() {
        use crate::items::defaults::tests::{ctx, export, user};
        const READ: &str = "alice$ /usr/bin/defaults export -g -";
        for (original, observed, checked, natural) in [
            ("<false/>", "<true/>", "<true/>", true),
            (
                "<true/>",
                "<integer>1</integer>",
                "<integer>1</integer>",
                true,
            ),
            ("<false/>", "<false/>", "<true/>", false),
        ] {
            let sys = Fake::root();
            sys.scroll_state(natural);
            let pref = |value| {
                export(&format!(
                    "<dict><key>com.apple.swipescrolldirection</key>{value}</dict>"
                ))
            };
            sys.on(READ, pref(original))
                .on(READ, pref(observed))
                .on(READ, pref(checked));
            let ctx = ctx(&[user("alice", 501)]);
            let mut pass = Workstation {
                items: crate::init_trackpad::items(&ctx).unwrap(),
                ctx,
                settings: Vec::new(),
                snapshots: None,
                held: Vec::new(),
            };
            pass.preference_changes(&sys).unwrap();
            let recording = pass
                .run_recording_with(&sys, Mode::Apply, &mut NoJournal)
                .unwrap();
            assert!(recording.attempts.is_empty());
            let rows = recording.rows;
            assert_eq!(rows[0].outcome, Outcome::Failed);
            assert!(rows[0].why.as_ref().unwrap().contains("recovery snapshot"));
            assert!(
                !sys.calls().iter().any(
                    |call| call.contains(" write ") || call.starts_with("scroll-direction set")
                )
            );
        }
    }

    #[test]
    fn an_unchanged_snapshot_runs_the_shared_setting_and_verifies_its_write() {
        use crate::items::defaults::tests::{ctx, export, user};
        const READ: &str = "alice$ /usr/bin/defaults export -g -";
        const WRITE: &str =
            "alice$ /usr/bin/defaults write -g com.apple.swipescrolldirection -bool false";
        let sys = Fake::root();
        sys.scroll_state(true);
        for value in [true, true, true, true, false, false] {
            sys.on(
                READ,
                export(&format!(
                    "<dict><key>com.apple.swipescrolldirection</key><{}/></dict>",
                    if value { "true" } else { "false" }
                )),
            );
        }
        sys.on(WRITE, Ran::ok(""));
        let ctx = ctx(&[user("alice", 501)]);
        let mut pass = Workstation {
            items: crate::init_trackpad::items(&ctx).unwrap(),
            ctx,
            settings: Vec::new(),
            snapshots: None,
            held: Vec::new(),
        };
        assert_eq!(pass.preference_changes(&sys).unwrap().len(), 1);
        let recording = pass
            .run_recording_with(&sys, Mode::Apply, &mut NoJournal)
            .unwrap();
        assert_eq!(recording.attempts.len(), 1);
        assert!(recording.attempts[0].verified);
        assert_eq!(recording.attempts[0].preferences.len(), 1);
        let rows = recording.rows;
        assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
        assert_eq!(
            sys.calls()
                .iter()
                .filter(|call| call.as_str() == WRITE)
                .count(),
            1
        );
        assert!(!sys.natural_scrolling().unwrap());
    }

    #[test]
    fn overlapping_shared_items_cannot_publish_an_ambiguous_snapshot() {
        use crate::items::defaults::tests::{ctx, export, user};
        let sys = Fake::root();
        for _ in 0..2 {
            sys.on(
                "alice$ /usr/bin/defaults export -g -",
                export("<dict><key>com.apple.swipescrolldirection</key><true/></dict>"),
            );
        }
        let ctx = ctx(&[user("alice", 501)]);
        let mut items = crate::init_trackpad::items(&ctx).unwrap();
        items.extend(crate::init_trackpad::items(&ctx).unwrap());
        let mut pass = Workstation {
            ctx,
            items,
            settings: Vec::new(),
            snapshots: None,
            held: Vec::new(),
        };
        assert!(
            pass.preference_changes(&sys)
                .unwrap_err()
                .contains("overlap")
        );
        assert!(!sys.calls().iter().any(|call| call.contains(" write ")));
    }

    /// Setup records `--converge` must never hold.
    struct NoSetup;

    impl Records for NoSetup {
        fn hold(&self, _whole: bool, _wait: &dyn Fn(&str)) -> Result<Box<dyn Held>, String> {
            panic!("--converge held the setup records")
        }
    }

    fn grants_unread(_sign_out: bool) -> Vec<(Access, Grant)> {
        panic!("--converge read the grants")
    }

    /// `aterm init <args>` over a Host whose every setup port panics and `ports`, its exit
    /// status as `ExitCode`'s `Debug` spells it.
    fn converge_run(args: &[&str], ports: &dyn crate::sync::Ports) -> String {
        let host = Host {
            base: &|_| panic!("--converge composed init's base layer"),
            full_disk_access: &|| panic!("--converge measured Full Disk Access"),
            records: &NoSetup,
            grants: grants_unread,
            walk: &|_| panic!("--converge walked the grants"),
            converge: ports,
        };
        format!(
            "{:?}",
            main_entry(args.iter().map(OsString::from).collect(), &host)
        )
    }

    fn exit(code: u8) -> String {
        format!("{:?}", ExitCode::from(code))
    }

    /// `aterm init --converge` runs the converge pass alone, at background QoS: no setup port
    /// (init's base layer, Full Disk Access, the records, the grants, the walk) is touched, the
    /// pass records itself, and `--check` leaves the state directory as it found it.
    /// NEGATIVE CONTROL: routing `--converge` into the whole of `aterm init` composes init's
    /// base layer, whose port panics.
    #[test]
    fn converge_runs_the_pass_alone_and_check_writes_nothing() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let ports = crate::sync::tests::Inert::new(scratch.path());
        assert_eq!(converge_run(&["--converge"], &ports), exit(0));
        assert_eq!(
            ports.backgrounds(),
            1,
            "the runner lowers its QoS as it starts"
        );
        assert!(crate::sync::read_record(scratch.path()).is_some());
        assert!(
            !ports.applies().is_empty(),
            "the pass applied its due kinds"
        );

        let checked = aterm_tempfile::tempdir().unwrap();
        let ports = crate::sync::tests::Inert::new(checked.path());
        assert_eq!(
            converge_run(&["--converge", "--check", "--json"], &ports),
            exit(0)
        );
        assert_eq!(ports.applies(), Vec::<&str>::new());
        assert_eq!(ports.base_applied(), Vec::<String>::new());
        let left: Vec<_> = std::fs::read_dir(checked.path()).unwrap().collect();
        assert!(left.is_empty(), "--check wrote {left:?}");
    }

    /// A subscriber's pass from a held copy is the installed app's: it runs `init --converge
    /// --trigger subscriber` there with this run's flags, ends with that run's exit, and runs no
    /// pass here. A window's or a person's pass never hands off, and a hand-off that cannot be
    /// started, or that an app with no such pass refuses as usage, leaves this copy to run the
    /// pass, saying so in aterm.log.
    /// NEGATIVE CONTROL: a runner that never asks `installed_release_elsewhere` runs the
    /// subscriber's pass here, from the held copy.
    #[test]
    fn a_held_copys_subscriber_pass_runs_from_the_installed_app() {
        let app = PathBuf::from("/Applications/aterm.app/Contents/MacOS/aterm");
        let scratch = aterm_tempfile::tempdir().unwrap();
        let mut ports = crate::sync::tests::Inert::new(scratch.path());
        ports.elsewhere = Some(app.clone());
        *ports.elsewhere_exit.lock().unwrap() = Ok(Some(75));
        assert_eq!(
            converge_run(&["--converge", "--trigger", "subscriber", "--json"], &ports),
            exit(75),
            "the installed app's exit"
        );
        let words = |args: &[&str]| args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
        assert_eq!(
            *ports.ran_elsewhere.lock().unwrap(),
            [(
                app.clone(),
                words(&["init", "--converge", "--trigger", "subscriber", "--json"])
            )]
        );
        assert!(ports.applies().is_empty(), "no pass here");
        assert!(crate::sync::read_record(scratch.path()).is_none());

        for args in [&["--converge", "--trigger", "window"][..], &["--converge"]] {
            assert_eq!(converge_run(args, &ports), exit(0), "{args:?}");
        }
        assert_eq!(
            ports.ran_elsewhere.lock().unwrap().len(),
            1,
            "never handed off"
        );

        for (answer, said) in [
            (Err("No such file or directory".to_owned()), "did not start"),
            (Ok(Some(2)), "does not take the pass"),
        ] {
            let unstarted = aterm_tempfile::tempdir().unwrap();
            let mut ports = crate::sync::tests::Inert::new(unstarted.path());
            ports.elsewhere = Some(app.clone());
            *ports.elsewhere_exit.lock().unwrap() = answer;
            assert_eq!(
                converge_run(&["--converge", "--trigger", "subscriber"], &ports),
                exit(0)
            );
            assert!(!ports.applies().is_empty(), "this copy ran the pass");
            assert!(
                ports.logged().iter().any(|line| line.contains(said)),
                "{:?}",
                ports.logged()
            );
        }
    }

    /// Exit 75 while another pass holds the lock (applying nothing), 1 for a failed row, 2 for
    /// a flag `--converge` does not take, before any port is read.
    #[test]
    fn converge_exits_75_on_a_held_lock_1_on_a_failed_row_and_2_on_usage() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let ports = crate::sync::tests::Inert::new(scratch.path());
        let held = crate::sync::try_lock(scratch.path()).unwrap().unwrap();
        assert_eq!(converge_run(&["--converge"], &ports), exit(75));
        assert_eq!(ports.applies(), Vec::<&str>::new());
        drop(held);
        *ports.fleet_done.lock().unwrap() = Err("the issuer did not answer".to_owned());
        assert_eq!(converge_run(&["--converge"], &ports), exit(1));

        let untouched = crate::sync::tests::Inert::new(scratch.path());
        for args in [
            &["--converge", "--plan"][..],
            &["--converge", "--only", "base"],
            &["--converge", "--no-wait"],
            &["--converge", "--profile"],
            &["--converge", "--develop"],
            &["--converge", "--vault", "/tmp/vault"],
            &["--converge", "--trigger", "nightly"],
            &["--converge", "--trigger"],
            &["--trigger", "window"],
        ] {
            assert_eq!(converge_run(args, &untouched), exit(2), "{args:?}");
        }
        assert_eq!(untouched.backgrounds(), 0, "a usage error reads nothing");
        assert_eq!(
            options(&[
                "--converge".into(),
                "--trigger=window".into(),
                "--check".into()
            ])
            .unwrap(),
            Options {
                action: Action::Check,
                converge: true,
                trigger: Some(crate::sync::Trigger::Window),
                ..Options::default()
            }
        );
    }

    #[test]
    fn modes_are_exclusive_and_invalid_arguments_fail_before_probes() {
        assert!(options(&["--plan".into(), "--check".into()]).is_err());
        assert!(options(&["--unknown".into(), "--help".into()]).is_err());
        assert_eq!(
            options(&["--check".into(), "--json".into()]).unwrap(),
            Options {
                action: Action::Check,
                json: true,
                ..Options::default()
            }
        );
        let vault = |args: &[&str]| {
            options(&args.iter().map(OsString::from).collect::<Vec<_>>()).map(|o| o.vault)
        };
        assert_eq!(
            vault(&["--vault", "https://vault.example/v1/lab"])
                .unwrap()
                .as_deref(),
            Some("https://vault.example/v1/lab")
        );
        assert_eq!(
            vault(&["--vault=~/vault.txt"]).unwrap().as_deref(),
            Some("~/vault.txt")
        );
        assert!(vault(&["--vault"]).is_err());
        assert!(vault(&["--vault="]).is_err());
        assert!(vault(&["--vault", "--plan"]).is_err(), "a flag is no vault");
        assert!(
            vault_reaches(
                &options(&["--vault=~/vault.txt".into(), "--only".into(), "base".into()]).unwrap()
            )
            .is_err(),
            "the vault joins in the workstation layer"
        );
        assert_eq!(
            options(&[
                "--only".into(),
                "base.clt".into(),
                "--only=shell,pkg".into()
            ])
            .unwrap()
            .only,
            ["base.clt", "shell", "pkg"]
        );
        assert!(options(&["--only".into()]).is_err());
        assert!(options(&["--only=".into()]).is_err());
    }

    /// Every `aterm init` command this crate prints is one init takes: each [`Remedy::Init`]
    /// it builds ([`crate::init_clt::place`], [`crate::init_spotlight::ID`], the base layer)
    /// rendered by [`Remedy::command`], the one spelling every surface prints, and each one
    /// its texts name. A word that names nothing, a value left off and a word without `--only`
    /// are the negative controls.
    #[test]
    fn every_init_remedy_this_crate_prints_is_a_line_init_takes() {
        let base = Base {
            items: Vec::new(),
            held: None,
            clt: Some(crate::init_clt::CommandLineTools::new(
                Box::new(|_: &dyn System| crate::init_clt::Found::Present("cc".into())),
                PathBuf::from("/Applications/aterm.app/Contents/MacOS/aterm"),
            )),
            toolset: None,
            targo: None,
        };
        let alice = crate::items::defaults::tests::user("alice", 501);
        let station =
            crate::init_spotlight::items(&crate::items::defaults::tests::ctx(&[alice])).unwrap();
        let line = |only: &str| Remedy::Init { only: only.into() }.command().unwrap();
        let mut lines = vec![
            crate::init_clt::place().remedy.command().unwrap(),
            line(crate::init_spotlight::ID),
            line(Layer::Base.name()),
            line(Layer::Workstation.name()),
            Remedy::Develop.command().unwrap(),
        ];
        for text in [
            crate::init_clt::NO_TTY,
            crate::items::developer::LOW_DATA,
            crate::init_github::NEEDS_GIT,
            crate::controller::github::NO_GIT,
        ] {
            let named = init_lines(text);
            assert!(!named.is_empty(), "{text}");
            lines.extend(named);
        }
        for line in &lines {
            assert_eq!(refusal(line, &base, &station), None, "{line}");
        }
        for refused in [
            line("init.spotlite"),
            line("base.cl"),
            "aterm init --only".to_owned(),
            "aterm init base.clt".to_owned(),
            "aterm pkg repair".to_owned(),
        ] {
            assert!(refusal(&refused, &base, &station).is_some(), "{refused}");
        }
        assert_eq!(
            init_lines("run `aterm init --only base.clt` in a terminal; or (aterm init --develop)"),
            ["aterm init --only base.clt", "aterm init --develop"]
        );
    }

    #[test]
    fn only_narrows_the_base_layer_and_names_the_words_it_does_not_know() {
        let base = || Base {
            items: vec![Box::new(BaseItem {
                probes: Mutex::new(vec![ok()]),
                applied: std::sync::Arc::default(),
            }) as Box<dyn aterm_converge::Item>],
            held: None,
            clt: Some(crate::init_clt::CommandLineTools::new(
                Box::new(|_: &dyn System| crate::init_clt::Found::Present("cc".into())),
                PathBuf::from("/Applications/aterm.app/Contents/MacOS/aterm"),
            )),
            toolset: None,
            targo: None,
        };
        assert_eq!(
            base().unnamed(&["base.clt".into(), "pkg".into(), "base".into()]),
            Vec::<String>::new()
        );
        assert_eq!(
            base().unnamed(&["workstation".into(), "base.cl".into()]),
            ["workstation", "base.cl"]
        );
        let clt = base().narrowed(&["base.clt".into()]);
        assert!(clt.items.is_empty() && clt.clt.is_some());
        let pkg = base().narrowed(&["pkg".into()]);
        assert_eq!(pkg.items.len(), 1);
        assert!(pkg.clt.is_none());
        let all = base().narrowed(&[]);
        assert!(all.items.len() == 1 && all.clt.is_some());
    }

    #[test]
    fn base_clt_runs_after_the_base_hold_is_released_and_blocks_without_a_terminal() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        struct Held(Arc<AtomicBool>);
        impl Drop for Held {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let released = Arc::new(AtomicBool::new(false));
        let saw = Arc::new(Mutex::new(Vec::new()));
        let base = Base {
            items: Vec::new(),
            held: Some(Box::new(Held(Arc::clone(&released)))),
            clt: Some(crate::init_clt::CommandLineTools::new(
                {
                    let (released, saw) = (Arc::clone(&released), Arc::clone(&saw));
                    Box::new(move |_: &dyn System| {
                        saw.lock().unwrap().push(released.load(Ordering::SeqCst));
                        crate::init_clt::Found::Missing("no developer directory".into())
                    })
                },
                PathBuf::from("/Applications/aterm.app/Contents/MacOS/aterm"),
            )),
            toolset: None,
            targo: None,
        };
        let alice = user("alice", 501);
        let ctx = Ctx {
            console: None,
            ..ctx(&[alice])
        };
        let sys = Fake::default();
        let rows = run(&sys, &ctx, base, &[], Mode::Apply, &mut NoJournal).unwrap();
        assert_eq!(rows.len(), 1, "{rows:?}");
        let row = &rows[0];
        assert_eq!(row.id, crate::init_clt::ID);
        assert_eq!(row.outcome, Outcome::Blocked, "{row:?}");
        assert_eq!(row.layer, Some(Layer::Base));
        assert_eq!(row.class, Some(Class::Elevate));
        assert!(
            row.why
                .as_deref()
                .is_some_and(|why| why.ends_with("run `aterm init --only base.clt` in a terminal")),
            "{row:?}"
        );
        let saw = saw.lock().unwrap();
        assert!(
            !saw.is_empty() && saw.iter().all(|&released| released),
            "{saw:?}"
        );
        assert!(sys.calls().is_empty(), "{:?}", sys.calls());
    }

    /// `base.pkg.toolset` READS ONLY ONCE THE BASE LAYER LET THE STORE LOCK GO (its passes
    /// take it), after `base.clt` and before the develop layer, whose tools run after it
    /// whatever it ends as: once it installed the set, and while it installs nothing
    /// (`auto_install = false`), since their own probe reads the `targo` they need. CONTROL:
    /// with the toolset complete it runs no pass.
    #[test]
    fn the_toolset_installs_once_the_lock_is_let_go_and_the_develop_tools_follow_it() {
        use crate::init_toolset::{Found, Toolset, WAIT_LOCK};
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
        struct Held(Arc<AtomicBool>);
        impl Drop for Held {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        struct Tools(Arc<AtomicU32>);
        impl Item for Tools {
            fn id(&self) -> String {
                "develop.workspace-tools@~/aterm".into()
            }
            fn family(&self) -> &'static str {
                "tooling"
            }
            fn title(&self) -> String {
                self.id()
            }
            fn probe(&self, _: &dyn System, _: &Ctx) -> Probe {
                if self.0.load(Ordering::SeqCst) == 0 {
                    change()
                } else {
                    ok()
                }
            }
            fn apply(&self, _: &dyn System, _: &Ctx) -> Result<(), String> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
            fn after(&self) -> Vec<String> {
                crate::init_develop::TOOLS_AFTER
                    .iter()
                    .map(|&id| id.to_owned())
                    .collect()
            }
        }
        let missing = Found::Missing("none installed".into());
        let complete = Found::Complete("7 active".into());
        let off = Found::Off("auto_install off".into());
        for (reads, outcome, passes) in [
            (vec![missing, complete.clone()], Outcome::Changed, 2),
            (vec![off], Outcome::NotApplicable, 0),
            (vec![complete], Outcome::Ok, 0),
        ] {
            let released = Arc::new(AtomicBool::new(false));
            let saw = Arc::new(Mutex::new(Vec::new()));
            let ran = Arc::new(Mutex::new(Vec::new()));
            let toolset = Toolset::new(
                {
                    let (released, saw) = (Arc::clone(&released), Arc::clone(&saw));
                    let reads = Mutex::new(reads);
                    Box::new(move || {
                        saw.lock().unwrap().push(released.load(Ordering::SeqCst));
                        let mut reads = reads.lock().unwrap();
                        if reads.len() > 1 {
                            reads.remove(0)
                        } else {
                            reads[0].clone()
                        }
                    })
                },
                {
                    let ran = Arc::clone(&ran);
                    Box::new(move |args: &[String], _| {
                        ran.lock().unwrap().push(args.join(" "));
                        Ok(Ran::ok(""))
                    })
                },
                Some(WAIT_LOCK),
                crate::init_toolset::tests::ATPKG,
            );
            let base = Base {
                items: Vec::new(),
                held: Some(Box::new(Held(Arc::clone(&released)))),
                clt: Some(crate::init_clt::CommandLineTools::new(
                    Box::new(|_: &dyn System| crate::init_clt::Found::Present("cc".into())),
                    PathBuf::from("/Applications/aterm.app/Contents/MacOS/aterm"),
                )),
                toolset: Some(toolset),
                targo: None,
            };
            let applied = Arc::new(AtomicU32::new(0));
            let develop: Vec<(Box<dyn Item>, Place)> = vec![(
                Box::new(Tools(Arc::clone(&applied))),
                crate::init_develop::place(Class::Install),
            )];
            let ctx = Ctx {
                console: None,
                ..ctx(&[user("alice", 501)])
            };
            let sys = Fake::default();
            let rows = run_layers(
                &sys,
                &ctx,
                base,
                &[],
                &develop,
                &Narrowing {
                    only: &[],
                    record: false,
                },
                Mode::Apply,
                &mut NoJournal,
                &mut Laps::start(),
                &Hooks::QUIET,
            )
            .unwrap();
            let ids: Vec<&str> = rows.iter().map(|row| row.id.as_str()).collect();
            assert_eq!(
                ids,
                [
                    crate::init_clt::ID,
                    crate::init_toolset::ID,
                    "develop.workspace-tools@~/aterm"
                ]
            );
            assert_eq!(rows[1].outcome, outcome, "{rows:?}");
            assert_eq!(rows[1].layer, Some(Layer::Base));
            assert_eq!(rows[1].class, Some(Class::Install));
            assert_eq!(rows[2].outcome, Outcome::Changed, "{rows:?}");
            assert_eq!(applied.load(Ordering::SeqCst), 1);
            let saw = saw.lock().unwrap();
            assert!(
                !saw.is_empty() && saw.iter().all(|&released| released),
                "read only once the lock was let go: {saw:?}"
            );
            let ran = ran.lock().unwrap();
            assert_eq!(ran.len(), passes, "{ran:?}");
            if passes > 0 {
                assert_eq!(
                    *ran,
                    [
                        "seed --wait-lock 1800",
                        "update --defer-busy-flip --wait-lock 1800"
                    ]
                );
            }
            assert!(sys.calls().is_empty(), "{:?}", sys.calls());
        }
    }

    /// A run under another home or a private state root is `base.pkg.toolset`'s alone: never
    /// a bare run, another base row (`base` takes the keychain's accounts and `base.clt`), the
    /// workstation or the develop layer.
    #[test]
    fn only_the_toolset_row_goes_under_another_home() {
        let alone = |args: &[&str]| {
            options(&args.iter().map(OsString::from).collect::<Vec<_>>())
                .unwrap()
                .toolset_alone()
        };
        for args in [
            &["--only", "base.pkg.toolset"][..],
            &["--only", "base.pkg.toolset", "--check"],
            &["--only", "base.pkg.toolset,base.pkg.toolset", "--plan"],
        ] {
            assert!(alone(args), "{args:?}");
        }
        for args in [
            &[][..],
            &["--check"],
            &["--only", "base"],
            &["--only", "pkg"],
            &["--only", "base.pkg.toolset,base.clt"],
            &["--only", "workstation"],
            &["--only", "develop"],
            &["--only", "base.pkg.toolset", "--develop"],
        ] {
            assert!(!alone(args), "{args:?}");
        }
    }

    #[test]
    fn root_is_refused_without_running_any_command() {
        let sys = Fake::root();
        assert!(local_context(&sys).unwrap_err().contains("without sudo"));
        assert!(sys.calls().is_empty());
    }

    /// Alice's own shell, at a terminal in the console's graphic session, with the console
    /// owned by `console_uid`: what `local_context` reads to resolve her.
    fn alice_at(console_uid: u32) -> Fake {
        let mut sys = Fake::root();
        sys.euid = 501;
        sys.logged_in(&crate::items::defaults::tests::user("alice", 501));
        sys.stat_of(
            "/dev/console",
            Stat {
                uid: console_uid,
                gid: 20,
                mode: 0o600,
                dir: false,
                symlink: false,
                len: 0,
            },
        );
        sys.on(
            &crate::facts::hidutil_keyboards().display(),
            Ran::ok("Services:\n\nDevices:\n"),
        );
        sys
    }

    #[test]
    fn under_another_console_owner_a_base_item_applies_and_a_workstation_item_waits() {
        let refused = prepare_workstation(&alice_at(502), WorkstationGroup::Preferences)
            .err()
            .expect("refused away from the console");
        assert!(refused.contains("at this Mac's console"), "{refused}");
        for (console_uid, at_console) in [(502, false), (501, true)] {
            let sys = alice_at(console_uid);
            let (ctx, _) = local_context(&sys).unwrap();
            assert_eq!(ctx.console.is_some(), at_console);
            let applied = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
            let base = Base {
                items: vec![Box::new(BaseItem {
                    probes: Mutex::new(vec![change(), ok()]),
                    applied: std::sync::Arc::clone(&applied),
                })],
                held: None,
                clt: None,
                toolset: None,
                targo: None,
            };
            let groups = if at_console {
                vec![(
                    vec![Listed::boxed("init.test.dock", vec![change(), ok()])],
                    place(Class::Preference),
                )]
            } else {
                let before = sys.calls();
                let groups = prepared(&sys, &ctx);
                assert_eq!(
                    sys.calls(),
                    before,
                    "no workstation preparation off the console"
                );
                assert_eq!(groups[0].0[0].id(), "init.workstation");
                groups
            };
            let scratch = aterm_tempfile::tempdir().unwrap();
            let mut journal = FileJournal::under(scratch.path());
            let rows = run(&sys, &ctx, base, &groups, Mode::Apply, &mut journal).unwrap();
            assert_eq!(rows[0].id, "base.pkg.shims");
            assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
            assert_eq!(applied.load(std::sync::atomic::Ordering::SeqCst), 1);
            if at_console {
                assert_eq!(rows[1].outcome, Outcome::Changed, "{rows:?}");
            } else {
                // Another login's workstation waits for a person at its console: the base
                // layer's run is done, and only `--check` reads the deferred row as unmet.
                assert_eq!(rows[1].outcome, Outcome::Deferred, "{rows:?}");
                assert_eq!(rows[1].why, Presence::OtherLogin.needs_person());
                assert!(!sys.calls().iter().any(|call| call.contains("defaults")
                    || call.contains("hidutil")
                    || call.contains("accounts.data")));
                assert_eq!(status(&rows, Action::Apply), 0);
                assert_eq!(status(&rows, Action::Plan), 0);
                assert_eq!(status(&rows, Action::Check), 1);
            }
        }
        assert!(
            local_context(&Fake::root())
                .unwrap_err()
                .contains("without sudo")
        );
    }

    /// An SSH session of the console's own login is not the person at it: nothing of the
    /// console session is read, and the workstation waits for them as under another login.
    #[test]
    fn over_ssh_the_console_owners_workstation_waits_for_a_person_at_it() {
        for (remote_login, graphic, at_console) in [
            (true, Some(true), false),
            (false, Some(false), false),
            (false, Some(true), true),
        ] {
            let mut sys = alice_at(501);
            sys.remote_login = remote_login;
            sys.graphic_session = graphic;
            let (ctx, _) = local_context(&sys).unwrap();
            assert_eq!(
                ctx.console.is_some(),
                at_console,
                "{remote_login} {graphic:?}"
            );
            if at_console {
                continue;
            }
            // `aterm doctor`'s and System State's preparation says why, not to log in.
            let refused = prepare_workstation(&sys, WorkstationGroup::Preferences)
                .err()
                .expect("refused outside the console's session");
            assert!(
                refused.contains(Presence::Remote.why().unwrap()),
                "{refused}"
            );
            let groups = prepared(&sys, &ctx);
            let rows = run(
                &sys,
                &ctx,
                Base::none(),
                &groups,
                Mode::Apply,
                &mut NoJournal,
            )
            .unwrap();
            assert_eq!(rows.len(), 1, "{rows:?}");
            assert_eq!(rows[0].outcome, Outcome::Deferred);
            assert_eq!(rows[0].why, Presence::Remote.needs_person());
            assert_eq!(status(&rows, Action::Apply), 0);
            assert!(
                !sys.calls().iter().any(|call| call.contains("hidutil")),
                "{:?}",
                sys.calls()
            );
        }
    }

    /// From an agent's tool call (no terminal), in the console's own session, init runs the
    /// base layer, account sign-out, and the recorded vault and GitHub profile, but leaves the
    /// other personal rows to the person. An agent's tab leaves sign-out out too: no `killall`,
    /// no `rustup set profile`, and the vault is never probed or asked. Control: a person at
    /// the console gets every row.
    #[test]
    fn without_a_person_at_the_console_init_runs_only_account_sign_out() {
        let bound_classes = |sys: &Fake, groups: &[Group], ctx: &Ctx| {
            crate::engine::bind(
                groups.iter().flat_map(|(items, place)| {
                    items.iter().map(move |item| (item.as_ref(), place.clone()))
                }),
                sys,
                ctx,
            )
            .iter()
            .map(|b| (b.id(), b.class()))
            .collect::<Vec<_>>()
        };
        let personal = [Class::Preference, Class::Asked];
        for (terminal, agent, presence) in [
            (false, None, Presence::NoTerminal),
            (true, Some("Claude Code"), Presence::AgentTab),
            (true, None, Presence::AtConsole),
        ] {
            let mut sys = alice_at(501);
            sys.terminal = terminal;
            sys.agent = agent;
            assert_eq!(crate::console::presence(&sys), presence);
            let (ctx, _) = local_context(&sys).unwrap();
            assert!(ctx.console.is_some(), "the console's own session");
            let vault = crate::init_secrets::tests::Scripted::new(vec![Ok(
                crate::init_secrets::tests::unconfigured(),
            )]);
            let port = Arc::clone(&vault);
            let groups = prepare_groups_with(
                &sys,
                &ctx,
                Mode::Apply,
                None,
                &Reading {
                    keyboards: None,
                    only: &[],
                    choice: None,
                },
                no_grants,
                move |_| Ok(port as Arc<dyn crate::init_secrets::SecretsOps>),
            )
            .unwrap()
            .0;
            let bound = bound_classes(&sys, &groups, &ctx);
            if presence == Presence::AtConsole {
                assert!(
                    bound
                        .iter()
                        .any(|(id, _)| id == "init.spotlight.rustup-profile@alice"),
                    "{bound:?}"
                );
                assert!(
                    bound.iter().any(|(_, class)| *class == Class::Preference),
                    "{bound:?}"
                );
                assert_eq!(vault.probes(), 1, "a person's run looks for the ALab vault");
                continue;
            }
            let personal_rows = bound
                .iter()
                .filter(|(_, class)| personal.contains(class))
                .collect::<Vec<_>>();
            if presence == Presence::NoTerminal {
                assert_eq!(personal_rows.len(), 2, "{bound:?}");
                assert!(
                    personal_rows
                        .iter()
                        .all(|(id, _)| id.contains(".signed-out@")),
                    "{bound:?}"
                );
            } else {
                assert!(personal_rows.is_empty(), "{bound:?}");
            }
            assert_eq!(vault.probes(), 0, "nobody to ask: no look for the vault");
            let rows = run(
                &sys,
                &ctx,
                Base::none(),
                &groups,
                Mode::Apply,
                &mut NoJournal,
            )
            .unwrap();
            let away = rows.iter().find(|row| row.id == AWAY);
            if presence == Presence::NoTerminal {
                assert!(away.is_none(), "account sign-out has no stand-in: {rows:?}");
            } else {
                let away = away.expect("the stand-in");
                assert_eq!(away.outcome, Outcome::Deferred);
                assert_eq!(away.why, presence.needs_person());
            }
            let calls = sys.calls();
            for change in ["killall", "set profile", "defaults write"] {
                assert!(
                    !calls.iter().any(|call| call.contains(change)),
                    "{change}: {calls:?}"
                );
            }
            if presence != Presence::NoTerminal {
                assert!(
                    !calls.iter().any(|call| call.contains("osascript")),
                    "{calls:?}"
                );
            }
        }
    }

    /// On Linux init runs the base layer, the vault and GitHub in any shell of the account (an
    /// SSH session's included), and a machine where each is converged exits 0. Control: each
    /// macOS group is one row reading "macOS only", not applicable, never a failure, and a step
    /// still to make is no exit 0.
    #[test]
    fn on_linux_init_runs_the_base_the_vault_and_github_and_the_mac_rows_are_not_applicable() {
        for converged in [true, false] {
            let mut sys = Fake::root();
            sys.euid = 501;
            sys.linux = true;
            sys.terminal = true;
            let alice = user("alice", 501);
            sys.logged_in(&alice);
            crate::init_github::scripted::converged(&sys, &alice, "notes");
            let (ctx, _) = local_context(&sys).unwrap();
            assert!(ctx.console.is_none(), "nobody's console session: {ctx:?}");
            let vault = crate::init_secrets::tests::converged();
            let port = Arc::clone(&vault);
            let (groups, choice) = prepare_groups_with(
                &sys,
                &ctx,
                Mode::Apply,
                None,
                &Reading {
                    keyboards: None,
                    only: &[],
                    choice: None,
                },
                no_grants,
                move |_| Ok(port as Arc<dyn crate::init_secrets::SecretsOps>),
            )
            .unwrap();
            assert_eq!(choice, Choice::ALL);
            let applied = Arc::new(std::sync::atomic::AtomicU32::new(0));
            let base = Base {
                items: vec![Box::new(BaseItem {
                    probes: Mutex::new(if converged {
                        vec![ok()]
                    } else {
                        vec![Probe::Unusable {
                            why: "the shims are not writable".into(),
                        }]
                    }),
                    applied: Arc::clone(&applied),
                })],
                held: None,
                clt: None,
                toolset: None,
                targo: None,
            };
            let rows = run(&sys, &ctx, base, &groups, Mode::Apply, &mut NoJournal).unwrap();
            let outcome = |id: &str| {
                rows.iter()
                    .find(|row| row.id == id)
                    .unwrap_or_else(|| panic!("no {id} row: {rows:#?}"))
                    .outcome
            };
            assert_eq!(
                outcome("base.pkg.shims"),
                if converged {
                    Outcome::Ok
                } else {
                    Outcome::Unknown
                }
            );
            assert_eq!(outcome("init.secrets@alice"), Outcome::Ok, "{rows:#?}");
            assert_eq!(
                outcome("init.github.native@alice"),
                Outcome::Ok,
                "{rows:#?}"
            );
            for mac in [
                "init.preferences",
                crate::init_spotlight::ID,
                ACCOUNTS_GROUP,
                "init.permissions",
            ] {
                let row = rows.iter().find(|row| row.id == mac).expect(mac);
                assert_eq!(row.outcome, Outcome::NotApplicable, "{row:?}");
                assert_ne!(row.outcome, Outcome::Failed);
                assert_eq!(row.why.as_deref(), Some("macOS only"));
            }
            assert_eq!(
                status(&rows, Action::Apply),
                if converged { 0 } else { 1 },
                "{rows:#?}"
            );
            assert_eq!(applied.load(std::sync::atomic::Ordering::SeqCst), 0);
            assert_eq!(
                vault.joins().len(),
                0,
                "a machine in the vault joins nothing"
            );
            let calls = sys.calls();
            for mac in ["killall", "osascript", "defaults", "tccutil", "launchctl"] {
                assert!(
                    !calls.iter().any(|call| call.contains(mac)),
                    "{mac}: {calls:?}"
                );
            }
            assert!(
                calls
                    .iter()
                    .filter(|call| call.contains(" git ") || call.contains("/git "))
                    .all(|call| call.contains(crate::items::github::SYSTEM_GIT)),
                "the distribution's git: {calls:?}"
            );
        }
    }

    #[test]
    fn a_run_left_only_by_stood_aside_rows_exits_75_77_or_69() {
        use aterm_converge::StoodAside;
        let row = |outcome, aside: Option<StoodAside>| ItemReport {
            id: "test".into(),
            family: "test".into(),
            title: "test".into(),
            outcome,
            from: None,
            to: None,
            why: None,
            revert: None,
            effect: Effect::Now,
            detail: None,
            layer: None,
            class: None,
            explain: None,
            stood_aside: aside,
            essential: false,
            elapsed_ms: None,
        };
        let lock = || Some(StoodAside::Lock("held".into()));
        let offline = || Some(StoodAside::Offline("down".into()));
        let permission = || Some(StoodAside::Permission("not granted".into()));
        let waiting = || Some(StoodAside::Waiting("waiting to be let in".into()));
        let done = row(Outcome::Changed, None);
        assert_eq!(status(std::slice::from_ref(&done), Action::Apply), 0);
        let vault = || ItemReport {
            id: "init.secrets@alice".into(),
            ..row(Outcome::Blocked, waiting())
        };
        // A vault join an admin has yet to approve, made now or on an earlier run, is done
        // for this run: the subscriber finishes it.
        for action in [Action::Apply, Action::Check] {
            assert_eq!(status(&[done.clone(), vault()], action), 0);
        }
        // A row waiting for that join is left for another run.
        assert_eq!(
            status(&[vault(), row(Outcome::Blocked, waiting())], Action::Apply),
            75
        );
        assert_eq!(
            status(&[vault(), row(Outcome::Blocked, offline())], Action::Apply),
            69
        );
        assert_eq!(
            status(
                &[done.clone(), row(Outcome::Blocked, lock())],
                Action::Apply
            ),
            75
        );
        assert_eq!(
            status(
                &[done.clone(), row(Outcome::Blocked, offline())],
                Action::Apply
            ),
            69
        );
        assert_eq!(
            status(
                &[
                    row(Outcome::Blocked, offline()),
                    row(Outcome::Blocked, lock())
                ],
                Action::Apply
            ),
            75
        );
        assert_eq!(
            status(
                &[
                    row(Outcome::Blocked, offline()),
                    row(Outcome::Blocked, permission())
                ],
                Action::Apply
            ),
            77
        );
        assert_eq!(
            status(
                &[
                    row(Outcome::Blocked, permission()),
                    row(Outcome::Blocked, lock())
                ],
                Action::Apply
            ),
            75
        );
        assert_eq!(
            status(
                &[row(Outcome::Blocked, lock()), row(Outcome::Failed, None)],
                Action::Apply
            ),
            1
        );
    }

    #[test]
    /// A keyboard list macOS answers in words this build does not read leaves only the Caps
    /// Lock rows unread: one row says why, and every other preference is still built and run.
    /// System State's own reading still refuses it.
    fn an_unreadable_keyboard_list_leaves_only_the_caps_lock_rows_unread() {
        let current_user = || {
            let mut sys = Fake::root();
            sys.euid = 501;
            sys.logged_in(&crate::items::defaults::tests::user("alice", 501));
            sys.stat_of(
                "/dev/console",
                Stat {
                    uid: 501,
                    gid: 20,
                    mode: 0o600,
                    dir: false,
                    symlink: false,
                    len: 0,
                },
            );
            sys
        };
        let command = crate::facts::hidutil_keyboards();
        for invalid in [
            "Services:\nUnknownHeader\nnonempty unparsed row\n",
            "Devices:\n",
        ] {
            let sys = current_user();
            sys.on(&command.display(), Ran::ok(invalid));
            let (ctx, unread) = local_context(&sys).unwrap();
            let why = unread.expect("the list is not read");
            assert!(ctx.console.is_some() && ctx.facts.keyboards.is_empty());
            assert!(
                !sys.calls()
                    .iter()
                    .any(|call| call.contains("defaults") || call.contains("accounts.data"))
            );
            let groups = groups(
                &ctx,
                Presence::AtConsole,
                crate::init_secrets::Secrets {
                    items: Vec::new(),
                    waited_on: None,
                },
                Vec::new(),
                Some(&why),
                Choice::ALL,
                &[],
            );
            let preferences: Vec<&dyn Item> = groups
                .iter()
                .filter(|(_, place)| place.class == Class::Preference)
                .flat_map(|(items, _)| items.iter().map(AsRef::as_ref))
                .collect();
            let keyboard: Vec<&&dyn Item> = preferences
                .iter()
                .filter(|item| item.family() == "keyboard")
                .collect();
            assert_eq!(keyboard.len(), 1, "one Caps Lock row");
            assert_eq!(
                keyboard[0].probe(&sys, &ctx),
                Probe::Unusable { why: why.clone() }
            );
            assert!(
                preferences.len() > 1 && preferences.iter().any(|item| item.family() == "trackpad"),
                "the other preferences are built"
            );
            assert!(context_for_selected(&sys, WorkstationGroup::All, Selection::all()).is_err());
        }
        let sys = current_user();
        sys.on(&command.display(), Ran::ok("Services:\n\nDevices:\n"));
        let (ctx, unread) = local_context(&sys).unwrap();
        assert!(unread.is_none() && ctx.facts.keyboards.is_empty());
        let sys = current_user();
        sys.on(&command.display(), Ran::ok("Services:\n"));
        sys.refuse_complete_stdout(&command.display(), "incomplete captured service list");
        assert!(local_context(&sys).unwrap().1.is_some());
    }

    #[test]
    fn pending_permission_or_unverified_changes_never_count_as_complete() {
        let mut row = ItemReport {
            id: "test".into(),
            family: "test".into(),
            title: "test".into(),
            outcome: Outcome::Ok,
            from: None,
            to: None,
            why: None,
            revert: None,
            effect: Effect::Now,
            detail: None,
            layer: None,
            class: None,
            explain: None,
            stood_aside: None,
            essential: false,
            elapsed_ms: None,
        };
        assert!(!complete(&[], Action::Apply));
        for outcome in [
            Outcome::Person,
            Outcome::Blocked,
            Outcome::Deferred,
            Outcome::Unknown,
            Outcome::Failed,
        ] {
            row.outcome = outcome;
            for action in [Action::Apply, Action::Plan, Action::Check] {
                assert!(!complete(&[row.clone()], action));
            }
        }
        row.outcome = Outcome::Planned;
        assert!(complete(&[row.clone()], Action::Plan));
        assert!(!complete(&[row.clone()], Action::Apply));
        assert!(!complete(&[row], Action::Check));
    }

    fn alice_with_checkout(console_uid: u32, profile: Option<&str>) -> Fake {
        let mut sys = Fake::root();
        sys.euid = 501;
        sys.logged_in(&crate::items::defaults::tests::user("alice", 501));
        let owned = |uid: u32, dir: bool| Stat {
            uid,
            gid: 20,
            mode: if dir { 0o755 } else { 0o600 },
            dir,
            symlink: false,
            len: 0,
        };
        sys.stat_of("/dev/console", owned(console_uid, false));
        sys.stat_of("/Users//alice", owned(501, true));
        sys.stat_of("/Users//alice/aterm", owned(501, true));
        sys.on(
            &crate::facts::hidutil_keyboards().display(),
            Ran::ok("Services:\n\nDevices:\n"),
        );
        let alice = user("alice", 501);
        sys.write_user_file(
            &alice,
            Path::new("/Users//alice/aterm/Cargo.toml"),
            crate::checkout::tests::manifest(Path::new("/srv/origin.git")).as_bytes(),
            0o644,
        )
        .unwrap();
        if let Some(profile) = profile {
            sys.write_user_file(
                &alice,
                Path::new("/Users//alice/.config/aterm/init.toml"),
                profile.as_bytes(),
                0o600,
            )
            .unwrap();
        }
        sys
    }

    struct Composed {
        bases: std::sync::atomic::AtomicU32,
        workstations: std::sync::atomic::AtomicU32,
        base_item: bool,
    }

    impl Composed {
        fn new() -> Self {
            Self {
                bases: std::sync::atomic::AtomicU32::new(0),
                workstations: std::sync::atomic::AtomicU32::new(0),
                base_item: false,
            }
        }

        fn with_base() -> Self {
            Self {
                base_item: true,
                ..Self::new()
            }
        }

        fn converge(
            &self,
            sys: &dyn System,
            options: &Options,
        ) -> Result<Vec<ItemReport>, Refused> {
            self.converge_timed(sys, options, &mut Laps::start())
        }

        fn converge_timed(
            &self,
            sys: &dyn System,
            options: &Options,
            laps: &mut Laps,
        ) -> Result<Vec<ItemReport>, Refused> {
            use std::sync::atomic::Ordering;
            let base = |_: bool| {
                self.bases.fetch_add(1, Ordering::SeqCst);
                let mut base = Base::none();
                if self.base_item {
                    base.items.push(Box::new(BaseItem {
                        probes: Mutex::new(vec![ok()]),
                        applied: Arc::new(std::sync::atomic::AtomicU32::new(0)),
                    }));
                }
                base
            };
            let groups = |_: &dyn System, _: &Ctx, _: Mode, _: &Reading<'_>| {
                self.workstations.fetch_add(1, Ordering::SeqCst);
                Ok((
                    vec![(
                        vec![Listed::boxed("init.test.dock", vec![change(), ok()])],
                        place(Class::Preference),
                    )],
                    Choice::ALL,
                ))
            };
            converge(
                sys,
                options,
                &Front {
                    base: &base,
                    groups: &groups,
                    targo: None,
                    cwd: Some(Path::new("/Users//alice/aterm")),
                    records: &NoRecords,
                    on_row: &|_| {},
                },
                &mut NoJournal,
                laps,
            )
        }

        fn counts(&self) -> (u32, u32) {
            use std::sync::atomic::Ordering;
            (
                self.bases.load(Ordering::SeqCst),
                self.workstations.load(Ordering::SeqCst),
            )
        }
    }

    /// `--json` TIMES THE RUN: its `phases` name this login's context, the questions, the
    /// base layer and the workstation layer, and every row, the base item's and the
    /// workstation item's alike, carries its time (presence only, never a budget).
    #[test]
    fn a_json_report_carries_each_phase_and_every_row_its_time() {
        let sys = alice_with_checkout(501, None);
        let mut laps = Laps::start();
        let rows = Composed::with_base()
            .converge_timed(&sys, &Options::default(), &mut laps)
            .unwrap();
        let complete = complete(&rows, Action::Apply);
        let text = report_json(Action::Apply, complete, rows, &laps).unwrap();
        let report: Report = aterm_json::from_str(&text).unwrap();
        assert_eq!(
            report.phases.keys().map(String::as_str).collect::<Vec<_>>(),
            ["base", "context", "questions", "workstation"],
            "{text}"
        );
        for layer in [Layer::Base, Layer::Workstation] {
            assert!(
                report.items.iter().any(|row| row.layer == Some(layer)),
                "{layer:?}: {text}"
            );
        }
        assert!(
            report.items.iter().all(|row| row.elapsed_ms.is_some()),
            "{text}"
        );
    }

    #[test]
    fn develop_on_a_fake_system_applies_no_preference_item() {
        let develop = Options {
            develop: Some(true),
            ..Options::default()
        };
        let sys = alice_with_checkout(501, None);
        let front = Composed::new();
        let rows = front.converge(&sys, &develop).unwrap();
        assert_eq!(front.counts(), (1, 0), "the workstation is never composed");
        assert!(
            rows.iter()
                .all(|row| row.class != Some(Class::Preference)
                    && row.layer != Some(Layer::Workstation)),
            "{rows:?}"
        );
        assert_eq!(rows[0].id, crate::init_develop::RECORD);
        assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
        assert!(rows.iter().all(|row| row.layer == Some(Layer::Develop)));
        assert!(!sys.calls().iter().any(|call| call.contains("defaults")
            || call.contains("hidutil")
            || call.contains("accounts")));

        let sys = alice_with_checkout(501, None);
        let front = Composed::new();
        let rows = front.converge(&sys, &Options::default()).unwrap();
        assert_eq!(front.counts(), (1, 1));
        let dock = rows.iter().find(|row| row.id == "init.test.dock").unwrap();
        assert_eq!(dock.class, Some(Class::Preference));
        assert_eq!(dock.outcome, Outcome::Changed, "{rows:?}");
        assert!(
            sys.calls().iter().any(|call| call.contains("hidutil")),
            "the workstation inventories its keyboard obligations: {:?}",
            sys.calls()
        );
        assert!(
            !rows.iter().any(|row| row.layer == Some(Layer::Develop)),
            "{rows:?}"
        );
    }

    #[test]
    fn a_fleet_style_run_as_root_is_refused_before_any_develop_item() {
        let develop = Options {
            develop: Some(true),
            ..Options::default()
        };
        let mut root = alice_with_checkout(501, None);
        root.euid = 0;
        let before = root.calls();
        let front = Composed::new();
        let refused = front.converge(&root, &develop).unwrap_err();
        assert_eq!(
            refused,
            Refused::Failed("run as the logged-in user, without sudo".into())
        );
        assert_eq!(front.counts(), (0, 0));
        assert_eq!(root.calls(), before, "nothing ran");

        let sys = alice_with_checkout(501, None);
        let front = Composed::new();
        let rows = front.converge(&sys, &develop).unwrap();
        let ids: Vec<&str> = rows.iter().map(|row| row.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "develop.record",
                "develop.checkout@~/aterm",
                "develop.submodules@~/aterm",
                "develop.workspace-tools@~/aterm"
            ]
        );
        let calls = sys.calls();
        assert!(
            calls
                .iter()
                .any(|call| call == "alice$ write /Users//alice/.config/aterm/init.toml 600"),
            "{calls:?}"
        );
        assert!(
            calls
                .iter()
                .any(|call| call.contains("git -C /Users//alice/aterm")),
            "{calls:?}"
        );
    }

    #[test]
    fn only_takes_workstation_and_develop_words_and_refuses_a_checkout_nobody_records() {
        assert!(options(&["--develop".into(), "--no-develop".into()]).is_err());
        for flag in ["--develop", "--no-develop"] {
            for args in [
                vec![flag, "--only", "workstation"],
                vec!["--only=workstation", flag],
                vec![flag, "--only=base,workstation"],
                vec!["--only", "base", flag, "--only", "workstation"],
            ] {
                let sys = alice_with_checkout(501, None);
                let before = sys.calls();
                let front = Composed::new();
                let args: Vec<OsString> = args.into_iter().map(OsString::from).collect();
                let result = options(&args)
                    .map_err(Refused::Usage)
                    .and_then(|options| front.converge(&sys, &options));
                assert!(
                    matches!(&result, Err(Refused::Usage(_))),
                    "{args:?}: {result:?}"
                );
                assert_eq!(front.counts(), (0, 0), "{args:?}");
                assert_eq!(sys.calls(), before, "{args:?}");
            }
        }
        assert_eq!(
            options(&["--no-develop".into(), "--no-develop".into()])
                .unwrap()
                .develop,
            Some(false)
        );
        assert_eq!(
            unnamed(
                &Base::none(),
                &[
                    "workstation".into(),
                    "develop".into(),
                    "develop.checkout@~/aterm".into(),
                    "tooling".into(),
                    "keyboard".into(),
                ]
            ),
            ["keyboard"]
        );
        let recorded = "[develop]\ncheckouts = [\"~/aterm\"]\n";
        let only = |word: &str| Options {
            only: vec![word.to_owned()],
            ..Options::default()
        };
        let sys = alice_with_checkout(501, Some(recorded));
        let front = Composed::new();
        let refused = front
            .converge(&sys, &only("develop.checkout@~/elsewhere"))
            .unwrap_err();
        assert!(
            matches!(&refused, Refused::Usage(why) if why.contains("its checkouts: develop.checkout@~/aterm")),
            "{refused:?}"
        );
        assert!(
            !sys.calls().iter().any(|call| call.contains("git")),
            "{:?}",
            sys.calls()
        );
        let rows = front
            .converge(&sys, &only("develop.checkout@~/aterm"))
            .unwrap();
        let ids: Vec<&str> = rows.iter().map(|row| row.id.as_str()).collect();
        assert_eq!(ids, ["develop.checkout@~/aterm"]);
        let workstation = options(&["--only".into(), "workstation".into()]).unwrap();
        let rows = front.converge(&sys, &workstation).unwrap();
        let ids: Vec<&str> = rows.iter().map(|row| row.id.as_str()).collect();
        assert_eq!(ids, ["init.test.dock"]);
        assert_eq!(rows[0].class, Some(Class::Preference));
        assert_eq!(rows[0].outcome, Outcome::Changed);
        let sys = alice_with_checkout(501, None);
        let rows = Composed::new().converge(&sys, &only("develop")).unwrap();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].outcome, Outcome::Ok);
        assert!(
            rows[0]
                .to
                .as_deref()
                .is_some_and(|now| now.contains("`aterm init --develop` records one")),
            "{rows:?}"
        );
    }

    #[test]
    fn a_profile_that_cannot_be_read_never_stops_the_base_layer() {
        for (profile, mode, why) in [
            ("[develop\n", 0o600, "invalid ~/.config/aterm/init.toml"),
            (
                "[unknown]\nx = 1\n",
                0o600,
                "invalid ~/.config/aterm/init.toml",
            ),
            (
                "[develop]\ncheckouts = [\"aterm\"]\n",
                0o600,
                "must be ~/<path> or absolute",
            ),
            (
                "[develop]\ncheckouts = [\"~/aterm\"]\n",
                0o664,
                "cannot safely read ~/.config/aterm/init.toml",
            ),
        ] {
            for (only, console) in [
                (Vec::new(), 501),
                (Vec::new(), 0),
                (vec!["base".to_owned()], 0),
                (vec!["base.pkg.shims".to_owned()], 501),
            ] {
                let sys = alice_with_checkout(console, None);
                let alice = user("alice", 501);
                sys.write_user_file(
                    &alice,
                    Path::new("/Users//alice/.config/aterm/init.toml"),
                    profile.as_bytes(),
                    mode,
                )
                .unwrap();
                let front = Composed::with_base();
                let options = Options {
                    only: only.clone(),
                    ..Options::default()
                };
                let rows = front.converge(&sys, &options).unwrap_or_else(|refused| {
                    panic!("{profile:?} {only:?} at {console}: {refused:?}")
                });
                assert_eq!(front.counts().0, 1);
                let base = rows.iter().find(|row| row.id == "base.pkg.shims");
                assert_eq!(base.map(|row| row.outcome), Some(Outcome::Ok), "{rows:?}");
                let record: Vec<&ItemReport> = rows
                    .iter()
                    .filter(|row| row.layer == Some(Layer::Develop))
                    .collect();
                if only.is_empty() {
                    assert_eq!(record.len(), 1, "{profile:?}: {rows:?}");
                    assert_eq!(record[0].id, crate::init_develop::RECORD);
                    assert_eq!(record[0].outcome, Outcome::Unknown);
                    assert!(
                        record[0]
                            .why
                            .as_deref()
                            .is_some_and(|said| said.contains(why)),
                        "{profile:?}: {:?}",
                        record[0].why
                    );
                    assert_eq!(status(&rows, Action::Apply), 1);
                } else {
                    assert!(record.is_empty(), "{only:?}: {rows:?}");
                    assert_eq!(status(&rows, Action::Apply), 0);
                }
                let (ctx, _) = local_context(&sys).unwrap();
                let composed = crate::init_develop::compose(
                    &sys,
                    &ctx,
                    &crate::init_develop::Inputs {
                        want: None,
                        asked: true,
                        cwd: None,
                        targo: None,
                    },
                );
                assert!(
                    composed.is_err_and(|said| said.contains(why)),
                    "{profile:?}"
                );
            }
        }
    }

    #[test]
    fn an_offline_develop_machine_exits_69_not_1() {
        use aterm_converge::StoodAside;
        for (stderr, code) in [
            (
                "fatal: unable to access 'https://github.com/alabsystems/aterm/': Could not \
                 resolve host: github.com\n",
                69,
            ),
            (
                "git@github.com: Permission denied (publickey).\nfatal: Could not read from \
                 remote repository.\n",
                1,
            ),
        ] {
            for action in [Action::Apply, Action::Check] {
                let sys = alice_with_checkout(501, Some("[develop]\ncheckouts = [\"~/aterm\"]\n"));
                let alice = user("alice", 501);
                crate::init_develop::tests::script_local(
                    &sys,
                    &alice,
                    crate::init_develop::tests::ORIGIN,
                    true,
                    "",
                );
                sys.on(
                    &crate::checkout::git(&sys, &alice, Some(Path::new("/Users//alice/aterm")))
                        .args(["ls-remote", "--symref", "origin", "HEAD"])
                        .display(),
                    Ran::exit(128, stderr),
                );
                let options = Options {
                    action,
                    only: vec!["develop.checkout@~/aterm".into()],
                    ..Options::default()
                };
                let rows = Composed::new().converge(&sys, &options).unwrap();
                assert_eq!(rows.len(), 1, "{rows:?}");
                assert_eq!(status(&rows, action), code, "{action:?}: {rows:?}");
                if code == 69 {
                    assert_eq!(rows[0].outcome, Outcome::Blocked);
                    assert!(matches!(rows[0].stood_aside, Some(StoodAside::Offline(_))));
                    assert_eq!(
                        rows[0].explain.as_ref().map(|explain| &explain.remedy),
                        Some(&Remedy::Network)
                    );
                } else {
                    assert_eq!(rows[0].outcome, Outcome::Unknown, "{rows:?}");
                    assert_eq!(rows[0].stood_aside, None);
                }
                assert!(
                    sys.calls().iter().all(|call| !call.contains(" fetch ")),
                    "{:?}",
                    sys.calls()
                );
            }
        }
    }

    #[test]
    fn an_only_word_with_its_path_names_that_checkout_alone() {
        let profile = "[develop]\ncheckouts = [\"~/aterm\", \"~/aterm.old\"]\n";
        let ids = |word: &str| -> Vec<String> {
            let sys = alice_with_checkout(501, Some(profile));
            let options = Options {
                only: vec![word.to_owned()],
                ..Options::default()
            };
            Composed::new()
                .converge(&sys, &options)
                .unwrap()
                .into_iter()
                .map(|row| row.id)
                .collect()
        };
        assert_eq!(
            ids("develop.checkout@~/aterm"),
            ["develop.checkout@~/aterm"]
        );
        assert_eq!(
            ids("develop.checkout"),
            ["develop.checkout@~/aterm", "develop.checkout@~/aterm.old"]
        );
    }

    #[test]
    fn develop_records_whatever_only_says() {
        let ids = |develop: Option<bool>, profile: Option<&str>| -> Vec<String> {
            let sys = alice_with_checkout(501, profile);
            let options = Options {
                develop,
                only: vec!["tooling".into()],
                ..Options::default()
            };
            Composed::new()
                .converge(&sys, &options)
                .unwrap()
                .into_iter()
                .map(|row| row.id)
                .collect()
        };
        assert_eq!(
            ids(Some(true), None),
            ["develop.record", "develop.workspace-tools@~/aterm"]
        );
        assert_eq!(
            ids(None, Some("[develop]\ncheckouts = [\"~/aterm\"]\n")),
            ["develop.workspace-tools@~/aterm"]
        );
    }

    /// alice's Mac as doctor's develop layer sees it: euid 501, her home, and her `init.toml`
    /// when given; the account's context cannot be built (no account is scripted, as for an
    /// account the local directory node does not hold).
    fn develop_doctor(profile: Option<&str>, home: bool) -> (Doctor<Fake>, usize) {
        let mut sys = Fake::root();
        let alice = user("alice", 501);
        sys.euid = alice.uid;
        if let Some(profile) = profile {
            sys.write_user_file(
                &alice,
                &Path::new(&alice.home).join(".config/aterm/init.toml"),
                profile.as_bytes(),
                0o600,
            )
            .unwrap();
        }
        let setup = sys.calls().len();
        let home = home.then(|| PathBuf::from(&alice.home));
        (Doctor::over(sys, home, None), setup)
    }

    /// With no `init.toml`, or one with no `[develop]` table, doctor composes and reads no
    /// develop row and never builds the account's context (no account reading). A profile
    /// that records a checkout, an `--only` develop word, or no home from the account record
    /// (the controls) build it, and its failure here is the one `develop.record` row.
    #[test]
    fn doctor_runs_nothing_for_the_develop_layer_of_a_mac_that_records_none() {
        let id = "identity".to_owned();
        for profile in [None, Some("[github]\naccount = \"alice\"\n")] {
            let (doctor, setup) = develop_doctor(profile, true);
            let composed = doctor.compose(false);
            assert!(
                composed.items.is_empty() && !composed.unreadable,
                "{profile:?}"
            );
            assert_eq!(doctor.rows(&composed, &[]).unwrap(), Vec::new());
            assert_eq!(
                doctor.sys.calls().split_off(setup),
                Vec::<String>::new(),
                "{profile:?}"
            );
        }

        for (profile, asked, home) in [
            (Some("[develop]\ncheckouts = [\"~/aterm\"]\n"), false, true),
            (None, true, true),
            (None, false, false),
        ] {
            let (doctor, setup) = develop_doctor(profile, home);
            let composed = doctor.compose(asked);
            assert!(composed.unreadable, "{profile:?} {asked} {home}");
            let rows = doctor.rows(&composed, &[]).unwrap();
            assert_eq!(rows.len(), 1, "{rows:?}");
            assert_eq!(rows[0].id, crate::init_develop::RECORD);
            assert_eq!(rows[0].outcome, Outcome::Unknown);
            assert!(
                rows[0]
                    .why
                    .as_deref()
                    .is_some_and(|why| why.starts_with("cannot tell whose develop layer this is")),
                "{rows:?}"
            );
            assert!(
                doctor.sys.calls().split_off(setup).contains(&id),
                "{profile:?} {asked} {home}"
            );
        }
    }

    /// DOCTOR'S RUST-DOCS REPAIR is the workstation item `init.spotlight.rust-docs@alice` bound
    /// as a relay-class repair of the account's own: nothing is read before the call, and bare
    /// doctor's scope applies it and reads it back done. NEGATIVE CONTROL: outside the account's
    /// own shell the same item is blocked and its apply never runs.
    #[test]
    fn doctor_s_rust_docs_repair_is_a_relay_item_of_the_account_s_own() {
        const RUSTUP: &str = "alice$ CARGO_HOME=/Users//alice/.cargo \
                              RUSTUP_HOME=/Users//alice/.rustup /Users//alice/.cargo/bin/rustup";
        const STABLE: &str = "/Users//alice/.rustup/toolchains/stable-aarch64-apple-darwin";
        let remove =
            format!("{RUSTUP} component remove rust-docs --toolchain stable-aarch64-apple-darwin");
        let mac = || {
            let sys = alice_with_checkout(501, None);
            let record = format!("{STABLE}/lib/rustlib/components");
            let html = format!("{STABLE}/share/doc/rust/html");
            sys.create_dir_all(Path::new(&format!("{html}/std")), 0o755)
                .unwrap();
            sys.file(&format!("{html}/std/index.html"), b"<html>")
                .file("/Users//alice/.cargo/bin/rustup", b"")
                .on(
                    &format!("{RUSTUP} toolchain list"),
                    Ran::ok("stable-aarch64-apple-darwin (active, default)\n"),
                )
                .file(
                    &record,
                    b"cargo-aarch64-apple-darwin\nrust-docs-aarch64-apple-darwin\n",
                )
                .on(&remove, Ran::ok(""))
                .on_write(&remove, Path::new(&record), b"cargo-aarch64-apple-darwin\n");
            let setup = sys.calls().len();
            (
                Doctor::over(sys, Some(PathBuf::from("/Users//alice")), None),
                setup,
            )
        };
        let run = |doctor: &Doctor<Fake>, scope: aterm_converge::Scope| {
            let items = doctor.rust_docs().expect("alice's context reads");
            aterm_converge::run_scoped(
                &items,
                aterm_converge::Mode::Apply,
                scope,
                &mut aterm_converge::NoJournal,
                &mut |_| {},
            )
            .unwrap()
        };

        let (doctor, setup) = mac();
        assert_eq!(
            doctor.sys.calls().len(),
            setup,
            "nothing read before the call"
        );
        let rows = run(&doctor, aterm_converge::Scope::REPAIR);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].id, "init.spotlight.rust-docs@alice");
        assert_eq!(rows[0].class, Some(Class::Relay));
        assert_eq!(rows[0].layer, Some(Layer::Workstation));
        assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");

        let (doctor, _) = mac();
        let rows = run(
            &doctor,
            aterm_converge::Scope {
                not_own_account: Some("it runs as root"),
                ..aterm_converge::Scope::REPAIR
            },
        );
        assert_eq!(rows[0].outcome, Outcome::Blocked, "{rows:?}");
        assert!(
            !doctor
                .sys
                .calls()
                .iter()
                .any(|call| call == &remove || call.starts_with("rename ")),
            "{:?}",
            doctor.sys.calls()
        );
    }

    /// Lanes meeting in their probes: [`Meet::arrive`] returns once `count` lanes stand in it
    /// at the same time, and fails after a minute (a hang detector): lanes run one after
    /// another never all arrive.
    struct Meet {
        here: Mutex<u32>,
        all: std::sync::Condvar,
        count: u32,
    }

    impl Meet {
        fn new(count: u32) -> Arc<Self> {
            Arc::new(Self {
                here: Mutex::new(0),
                all: std::sync::Condvar::new(),
                count,
            })
        }

        fn arrive(&self) {
            let mut here = self.here.lock().unwrap();
            *here += 1;
            self.all.notify_all();
            let (here, waited) = self
                .all
                .wait_timeout_while(here, std::time::Duration::from_secs(60), |here| {
                    *here < self.count
                })
                .unwrap();
            assert!(
                !waited.timed_out(),
                "only {} of {} lanes ran at once",
                *here,
                self.count
            );
        }
    }

    /// A base item that meets the other lanes in its probe.
    struct MeetsBase(Arc<Meet>);

    impl aterm_converge::Item for MeetsBase {
        fn id(&self) -> String {
            "base.pkg.shims".into()
        }
        fn family(&self) -> &'static str {
            "pkg"
        }
        fn layer(&self) -> Layer {
            Layer::Base
        }
        fn class(&self) -> Class {
            Class::Relay
        }
        fn title(&self) -> String {
            self.id()
        }
        fn probe(&self) -> Probe {
            self.0.arrive();
            ok()
        }
        fn apply(&self) -> Result<(), String> {
            Ok(())
        }
        fn explain(&self, probe: &Probe) -> aterm_converge::Explain {
            aterm_converge::Explain {
                why: format!("{probe:?}"),
                remedy: Remedy::Repair,
            }
        }
    }

    /// A workstation item whose probe runs `then` before it reads right.
    struct Then {
        id: &'static str,
        family: &'static str,
        then: Box<dyn Fn() + Send + Sync>,
    }

    impl Item for Then {
        fn id(&self) -> String {
            self.id.into()
        }
        fn family(&self) -> &'static str {
            self.family
        }
        fn title(&self) -> String {
            self.id.into()
        }
        fn probe(&self, _: &dyn System, _: &Ctx) -> Probe {
            (self.then)();
            ok()
        }
        fn apply(&self, _: &dyn System, _: &Ctx) -> Result<(), String> {
            Ok(())
        }
    }

    fn then(
        id: &'static str,
        family: &'static str,
        then: impl Fn() + Send + Sync + 'static,
    ) -> Box<dyn Item> {
        Box::new(Then {
            id,
            family,
            then: Box::new(then),
        })
    }

    /// The base layer, the vault's join and the preferences run at once: each lane's first
    /// item meets the other two, which a serial run never lets happen.
    #[test]
    fn the_base_vault_and_preference_lanes_run_at_once() {
        let meet = Meet::new(3);
        let base = Base {
            items: vec![Box::new(MeetsBase(Arc::clone(&meet)))],
            held: None,
            clt: None,
            toolset: None,
            targo: None,
        };
        let (dock, vault) = (Arc::clone(&meet), Arc::clone(&meet));
        let groups = vec![
            (
                vec![then("init.test.dock", "dock", move || dock.arrive())],
                place(Class::Preference),
            ),
            (
                vec![then(
                    "init.test.vault",
                    crate::init_secrets::FAMILY,
                    move || vault.arrive(),
                )],
                place(Class::Install),
            ),
        ];
        let alice = at_console(user("alice", 501));
        let rows = run(
            &Fake::root(),
            &alice,
            base,
            &groups,
            Mode::Apply,
            &mut NoJournal,
        )
        .unwrap();
        assert_eq!(
            rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
            ["base.pkg.shims", "init.test.dock", "init.test.vault"]
        );
    }

    /// Each row reaches `on_row` as its item finishes, while the run goes on: the preference
    /// waits for the vault's row to have been printed. The rows come back in composition
    /// order all the same.
    #[test]
    fn each_row_streams_as_it_finishes_and_the_rows_return_in_composition_order() {
        let streamed: Arc<(Mutex<Vec<String>>, std::sync::Condvar)> = Arc::default();
        let seen = Arc::clone(&streamed);
        let groups = vec![
            (
                vec![then("init.test.dock", "dock", move || {
                    let (rows, more) = &*seen;
                    let (rows, waited) = more
                        .wait_timeout_while(
                            rows.lock().unwrap(),
                            std::time::Duration::from_secs(60),
                            |rows| !rows.iter().any(|id| id == "init.test.vault"),
                        )
                        .unwrap();
                    assert!(
                        !waited.timed_out(),
                        "the vault's row never streamed: {rows:?}"
                    );
                })],
                place(Class::Preference),
            ),
            (
                vec![then("init.test.vault", crate::init_secrets::FAMILY, || {})],
                place(Class::Install),
            ),
        ];
        let alice = at_console(user("alice", 501));
        let on_row = |row: &ItemReport| {
            let (rows, more) = &*streamed;
            rows.lock().unwrap().push(row.id.clone());
            more.notify_all();
        };
        let rows = run_layers(
            &Fake::root(),
            &alice,
            Base::none(),
            &groups,
            &[],
            &Narrowing {
                only: &[],
                record: false,
            },
            Mode::Apply,
            &mut NoJournal,
            &mut Laps::start(),
            &Hooks {
                on_row: &on_row,
                desktop: &|lane| lane(),
            },
        )
        .unwrap();
        assert_eq!(
            *streamed.0.lock().unwrap(),
            ["init.test.vault", "init.test.dock"]
        );
        assert_eq!(
            rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
            ["init.test.dock", "init.test.vault"]
        );
    }

    /// The grants an apply hands to the window's walk are the Permissions rows left to the
    /// person, in row order; a grant that is on, a row of another phase and an unknown word are
    /// not. NEGATIVE CONTROL: every needs-you row, read by outcome alone, takes the vault's too.
    #[test]
    fn the_walk_takes_the_grants_the_permissions_phase_left_to_the_person() {
        let rows = [
            report("init.permissions.full-disk-access", Outcome::Person),
            report("init.permissions.developer-tools", Outcome::Ok),
            report("init.permissions.debugger", Outcome::Person),
            report("init.permissions.accessibility", Outcome::Person),
            report("init.permissions.camera", Outcome::Person),
            report("init.secrets.vault", Outcome::Person),
        ];
        assert_eq!(
            walk_grants(&rows),
            [
                Access::FullDiskAccess,
                Access::Debugger,
                Access::Accessibility
            ]
        );
        assert_eq!(
            rows.iter()
                .filter(|row| row.outcome == Outcome::Person)
                .count(),
            5,
            "the outcome alone takes rows that are no grant"
        );
    }

    fn report(id: &str, outcome: Outcome) -> ItemReport {
        ItemReport {
            id: id.into(),
            family: "test".into(),
            title: id.into(),
            outcome,
            from: None,
            to: None,
            why: None,
            revert: None,
            effect: Effect::Now,
            detail: None,
            layer: None,
            class: None,
            explain: None,
            stood_aside: None,
            essential: false,
            elapsed_ms: None,
        }
    }

    /// What the default output prints for `rows`: each shown row, then the tally.
    fn printed(rows: &[ItemReport], action: Action) -> String {
        let mut text: String = rows.iter().filter(|row| shown(row)).map(rendered).collect();
        text.push_str(&tally(rows, action));
        text.push('\n');
        text
    }

    /// A run with every row right prints one line; a changed row adds its row and what
    /// changed, and a row only the person can finish is named in the tally.
    #[test]
    fn a_run_with_every_row_right_prints_one_line() {
        let mut rows: Vec<ItemReport> = (0..45)
            .map(|n| report(&format!("init.test.{n}"), Outcome::Ok))
            .collect();
        let quiet = printed(&rows, Action::Apply);
        assert_eq!(quiet, "aterm init: 45 ok\n");
        rows[3] = ItemReport {
            from: Some("on".into()),
            to: Some("off".into()),
            ..report("init.test.changed", Outcome::Changed)
        };
        let changed = printed(&rows, Action::Apply);
        assert_eq!(changed.lines().count(), 3, "{changed}");
        assert!(
            changed.ends_with("aterm init: 44 ok \u{b7} 1 changed\n"),
            "{changed}"
        );
        rows[5] = ItemReport {
            title: "Full Disk Access".into(),
            why: Some("switch it on".into()),
            ..report("init.permissions.full-disk", Outcome::Person)
        };
        let person = printed(&rows, Action::Apply);
        assert_eq!(person.lines().count(), 5, "{person}");
        assert!(
            person.ends_with("43 ok \u{b7} 1 changed \u{b7} 1 needs you: Full Disk Access\n"),
            "{person}"
        );
    }

    /// A row that finishes while another lane's question or attached program (a join
    /// password, `sudo`) owns the terminal is held back, and printed with the next row or once
    /// the run is over; an ok row is never printed by default.
    #[test]
    fn a_row_waits_while_a_prompt_owns_the_terminal() {
        let printer = Printer::new(false, false);
        let changed = ItemReport {
            from: Some("on".into()),
            to: Some("off".into()),
            ..report("init.test.changed", Outcome::Changed)
        };
        let mut during = Vec::new();
        crate::port::owning_terminal(|| printer.row_to(&changed, &mut during));
        assert!(during.is_empty(), "{}", String::from_utf8_lossy(&during));
        let mut after = Vec::new();
        printer.row_to(&report("init.test.ok", Outcome::Ok), &mut after);
        printer.flush_to(&mut after);
        assert_eq!(String::from_utf8(after).unwrap(), rendered(&changed));
    }

    /// `aterm init --help` and `aterm help init` read one page: every System State setting by
    /// its label, in at most 27 lines (the setup's 25 and `--converge`'s two).
    #[test]
    fn the_usage_names_every_setting_by_its_label_in_27_lines() {
        let usage = usage();
        for setting in Setting::ALL {
            assert!(usage.contains(setting.label()), "{setting:?}: {usage}");
        }
        assert!(usage.lines().count() <= 27, "{usage}");
        assert!(usage.contains("first launch"), "{usage}");
    }

    /// First-run setup is on by default (owner ruling 2026-10-25): a person at the console with
    /// no flag gets every group, the preferences and both sign-outs included, and is asked
    /// nothing; a flag runs only the group it names.
    #[test]
    fn a_person_at_the_console_gets_every_group_and_is_asked_nothing() {
        let prepared_with = |choice: Option<Choice>| {
            let mut sys = alice_at(501);
            sys.terminal = true;
            sys.agent = None;
            assert_eq!(crate::console::presence(&sys), Presence::AtConsole);
            let (ctx, _) = local_context(&sys).unwrap();
            let vault = crate::init_secrets::tests::Scripted::new(vec![Ok(
                crate::init_secrets::tests::unconfigured(),
            )]);
            let (groups, picked) = prepare_groups_with(
                &sys,
                &ctx,
                Mode::Apply,
                None,
                &Reading {
                    keyboards: None,
                    only: &[],
                    choice,
                },
                no_grants,
                move |_| Ok(vault as Arc<dyn crate::init_secrets::SecretsOps>),
            )
            .unwrap();
            let preferences: Vec<String> = groups
                .iter()
                .filter(|(_, place)| place.class == Class::Preference)
                .flat_map(|(items, _)| items.iter().map(|item| item.id()))
                .collect();
            let asked: Vec<String> = sys
                .calls()
                .into_iter()
                .filter(|call| call.starts_with("ask"))
                .collect();
            (preferences, picked, asked)
        };
        let signs_out = |id: &String| id.contains(".signed-out@");
        let (preferences, picked, asked) = prepared_with(None);
        assert_eq!(picked, Choice::ALL);
        assert!(asked.is_empty(), "nothing is asked: {asked:?}");
        assert!(
            preferences.iter().filter(|id| signs_out(id)).count() == 2
                && preferences.iter().any(|id| !signs_out(id)),
            "{preferences:?}"
        );
        let sign_out = Choice {
            sign_out: true,
            ..Choice::NONE
        };
        let (preferences, picked, asked) = prepared_with(Some(sign_out));
        assert_eq!(picked, sign_out);
        assert!(asked.is_empty(), "{asked:?}");
        assert!(
            preferences.len() == 2 && preferences.iter().all(signs_out),
            "{preferences:?}"
        );
    }

    /// The setup records a first launch shares: a pass that changes the preferences holds
    /// them, `whole` for every preference and both sign-outs, and records only a whole pass
    /// it verified.
    #[derive(Default)]
    struct Recorded {
        holds: Mutex<Vec<bool>>,
        verified: Arc<std::sync::atomic::AtomicU32>,
    }

    impl Records for Recorded {
        fn hold(&self, whole: bool, _wait: &dyn Fn(&str)) -> Result<Box<dyn Held>, String> {
            self.holds.lock().unwrap().push(whole);
            Ok(Box::new(Verifies(Arc::clone(&self.verified))))
        }
    }

    struct Verifies(Arc<std::sync::atomic::AtomicU32>);

    impl Held for Verifies {
        fn verified(self: Box<Self>) -> Result<(), String> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
    }

    /// One converge of alice's Mac at her console with a terminal, the groups `groups` builds
    /// and `records` for the setup records.
    fn converge_with(
        sys: &Fake,
        options: &Options,
        groups: &dyn Fn(&Ctx) -> Vec<Group>,
        records: &dyn Records,
    ) -> Result<Vec<ItemReport>, Refused> {
        let prepare = |_: &dyn System, ctx: &Ctx, _: Mode, reading: &Reading<'_>| {
            Ok((groups(ctx), reading.choice.unwrap_or(Choice::ALL)))
        };
        converge(
            sys,
            options,
            &Front {
                base: &|_| Base::none(),
                groups: &prepare,
                targo: None,
                cwd: None,
                records,
                on_row: &|_| {},
            },
            &mut NoJournal,
            &mut Laps::start(),
        )
    }

    /// Only the Desktop lane waits behind a first-launch pass that holds the setup records: the
    /// base layer and the vault's join run meanwhile. Here the hold returns only once the base
    /// item and the vault's join stand in it too (a hang detector), which a hold taken before
    /// the run never lets happen.
    #[test]
    fn only_the_desktop_lane_waits_behind_the_setup_records() {
        struct Behind(Arc<Meet>, Mutex<Vec<String>>);
        impl Records for Behind {
            fn hold(&self, _whole: bool, _wait: &dyn Fn(&str)) -> Result<Box<dyn Held>, String> {
                self.1.lock().unwrap().push("held".into());
                self.0.arrive();
                Ok(Box::new(NoRecords))
            }
        }
        let meet = Meet::new(3);
        let records = Behind(Arc::clone(&meet), Mutex::default());
        let base_meet = Arc::clone(&meet);
        let base = move |_: bool| Base {
            items: vec![Box::new(MeetsBase(Arc::clone(&base_meet)))],
            held: None,
            clt: None,
            toolset: None,
            targo: None,
        };
        let vault = Arc::clone(&meet);
        let groups = move |_: &Ctx| {
            let vault = Arc::clone(&vault);
            vec![
                (
                    vec![Listed::boxed("init.test.dock", vec![ok()])],
                    place(Class::Preference),
                ),
                (
                    vec![then(
                        "init.test.vault",
                        crate::init_secrets::FAMILY,
                        move || vault.arrive(),
                    )],
                    place(Class::Install),
                ),
            ]
        };
        let prepare =
            |_: &dyn System, ctx: &Ctx, _: Mode, _: &Reading<'_>| Ok((groups(ctx), Choice::ALL));
        let sys = alice_with_checkout(501, None);
        let rows = converge(
            &sys,
            &Options::default(),
            &Front {
                base: &base,
                groups: &prepare,
                targo: None,
                cwd: None,
                records: &records,
                on_row: &|_| {},
            },
            &mut NoJournal,
            &mut Laps::start(),
        )
        .unwrap();
        assert_eq!(*records.1.lock().unwrap(), ["held"]);
        assert_eq!(
            rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
            ["base.pkg.shims", "init.test.dock", "init.test.vault"]
        );
    }

    #[test]
    fn a_whole_profile_pass_records_what_it_verified_and_a_part_records_nothing() {
        let dock = |ctx: &Ctx| {
            let mut items = verified_accounts(ctx);
            items.push(Listed::boxed("init.test.dock", vec![change(), ok()]));
            vec![(items, place(Class::Preference))]
        };
        let unverified = |ctx: &Ctx| {
            let mut items = verified_accounts(ctx);
            items.push(Listed::boxed("init.test.dock", vec![change(), change()]));
            vec![(items, place(Class::Preference))]
        };
        let profile = Options {
            choice: Some(Choice {
                profile: true,
                ..Choice::NONE
            }),
            ..Options::default()
        };
        for (options, groups, holds, verified) in [
            (
                &Options::default(),
                &dock as &dyn Fn(&Ctx) -> Vec<Group>,
                [true],
                1,
            ),
            (&Options::default(), &unverified, [true], 0),
            (&profile, &dock, [false], 0),
        ] {
            let sys = alice_with_checkout(501, None);
            let records = Recorded::default();
            converge_with(&sys, options, groups, &records).unwrap();
            assert_eq!(*records.holds.lock().unwrap(), holds, "{options:?}");
            assert_eq!(
                records.verified.load(std::sync::atomic::Ordering::SeqCst),
                verified,
                "{options:?}"
            );
        }
    }

    struct WithheldRecords {
        events: Arc<Mutex<Vec<String>>>,
        refuses: bool,
    }

    impl Records for WithheldRecords {
        fn hold(&self, _whole: bool, _wait: &dyn Fn(&str)) -> Result<Box<dyn Held>, String> {
            Ok(Box::new(Self {
                events: Arc::clone(&self.events),
                refuses: self.refuses,
            }))
        }
    }

    impl Held for WithheldRecords {
        fn withheld(&self, ids: &[String]) -> Result<(), String> {
            self.events.lock().unwrap().extend(ids.iter().cloned());
            if self.refuses {
                Err("withheld record is unwritable".into())
            } else {
                Ok(())
            }
        }

        fn verified(self: Box<Self>) -> Result<(), String> {
            self.events.lock().unwrap().push("verified".into());
            Ok(())
        }
    }

    fn init_withheld_pass(
        only: Vec<String>,
        refuses: bool,
    ) -> (Result<Vec<ItemReport>, Refused>, Vec<String>) {
        let events = Arc::new(Mutex::new(Vec::new()));
        let records = WithheldRecords {
            events: Arc::clone(&events),
            refuses,
        };
        let lane = Arc::clone(&events);
        let groups = move |ctx: &Ctx| {
            let mut widgets = Selection::none();
            widgets.set(Setting::Widgets, true);
            let mut items: Vec<_> = crate::init_preferences::selected_items(ctx, widgets)
                .unwrap()
                .into_iter()
                .filter(|item| item.withheld().is_some())
                .collect();
            assert_eq!(
                items.len(),
                1,
                "the real Notification Center item waits for FDA"
            );
            let lane = Arc::clone(&lane);
            items.push(then("init.test.after-withheld", "test", move || {
                lane.lock().unwrap().push("desktop".into());
            }));
            items.extend(verified_accounts(ctx));
            vec![(items, place(Class::Preference))]
        };
        let result = converge_with(
            &alice_with_checkout(501, None),
            &Options {
                station: unnamed(&Base::none(), &only),
                only,
                ..Options::default()
            },
            &groups,
            &records,
        );
        let events = events.lock().unwrap().clone();
        (result, events)
    }

    #[test]
    fn init_withheld_items_are_retained_before_the_desktop_lane_verifies() {
        let (rows, events) = init_withheld_pass(Vec::new(), false);
        let rows = rows.unwrap();
        assert_eq!(rows[0].id, "consumer.widgets.notification-center@alice");
        assert_eq!(rows[0].outcome, Outcome::Ok);
        assert_eq!(
            events,
            [
                "consumer.widgets.notification-center@alice",
                "desktop",
                "verified"
            ]
        );
    }

    #[test]
    fn init_withheld_record_failure_prevents_the_desktop_lane() {
        let (result, events) = init_withheld_pass(Vec::new(), true);
        assert!(
            result.is_err(),
            "an unrecorded permission follow-up cannot be verified"
        );
        assert_eq!(events, ["consumer.widgets.notification-center@alice"]);
    }

    #[test]
    fn init_withheld_items_outside_the_narrowed_pass_are_not_recorded() {
        let (result, events) = init_withheld_pass(vec!["init.test.after-withheld".into()], false);
        let rows = result.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "init.test.after-withheld");
        assert_eq!(events, ["desktop"]);
    }

    /// `--only init.spotlight` names Rust in Spotlight's rows alone: no base item, no other
    /// workstation row, no look for a vault and no hold on the setup records.
    #[test]
    fn only_init_spotlight_runs_the_spotlight_rows_alone() {
        let parsed = options(&["--only".into(), "init.spotlight".into()]).unwrap();
        assert_eq!(parsed.only, ["init.spotlight"]);
        let options = Options {
            station: unnamed(&Base::none(), &parsed.only),
            ..parsed
        };
        assert!(options.workstation() && !options.joins());
        for only in [
            &[][..],
            &["secrets"],
            &["init.secrets@alice"],
            &["workstation"],
        ] {
            let only: Vec<String> = only.iter().map(|word| (*word).to_owned()).collect();
            let joins = Options {
                station: unnamed(&Base::none(), &only),
                only,
                ..Options::default()
            };
            assert!(joins.joins(), "{joins:?}");
        }
        let sys = alice_with_checkout(501, None);
        let groups = |ctx: &Ctx| {
            vec![
                (
                    vec![Listed::boxed("init.test.dock", vec![change(), ok()])],
                    place(Class::Preference),
                ),
                (
                    crate::init_spotlight::items(ctx).unwrap(),
                    place(Class::Asked),
                ),
            ]
        };
        let records = Recorded::default();
        let rows = converge_with(&sys, &options, &groups, &records).unwrap();
        assert!(
            !rows.is_empty() && rows.iter().all(|row| row.id.starts_with("init.spotlight.")),
            "{rows:?}"
        );
        assert!(records.holds.lock().unwrap().is_empty());
        let unknown = Options {
            only: vec!["init.spotlite".into()],
            station: vec!["init.spotlite".into()],
            ..Options::default()
        };
        assert!(matches!(
            converge_with(&sys, &unknown, &groups, &records),
            Err(Refused::Usage(_))
        ));
    }

    /// `--only init.fseventsd`, the line a press of the memory alert types, names the item, so
    /// it restarts fseventsd; in a plain `aterm init` the same fseventsd asks for no password
    /// and says how to restart it now.
    #[test]
    fn only_init_fseventsd_restarts_it() {
        use aterm_uds::services::{Look, Service};
        let alice = ctx(&[user("alice", 501)]);
        let mut sys = Fake::root();
        sys.euid = 501;
        sys.tty = true;
        for (only, outcome) in [
            (Vec::new(), Outcome::Deferred),
            (vec![crate::init_fseventsd::ID.to_owned()], Outcome::Planned),
        ] {
            sys.services(vec![Look::Running(
                Service {
                    pid: 537,
                    start_s: 1_790_000_000,
                    live: 1,
                    footprint: 20 << 30,
                    ..Service::default()
                },
                128 << 30,
            )]);
            let groups = groups(
                &alice,
                Presence::AtConsole,
                crate::init_secrets::Secrets {
                    items: Vec::new(),
                    waited_on: None,
                },
                Vec::new(),
                None,
                Choice::ALL,
                &only,
            );
            let bound = crate::engine::bind(
                groups.iter().flat_map(|(items, place)| {
                    items
                        .iter()
                        .filter(|item| item.id() == crate::init_fseventsd::ID)
                        .map(move |item| (item.as_ref(), place.clone()))
                }),
                &sys,
                &alice,
            );
            let rows = aterm_converge::run_scoped(
                &bound,
                Mode::Plan,
                aterm_converge::Scope::EVERY,
                &mut aterm_converge::NoJournal,
                &mut |_| {},
            )
            .unwrap();
            let ran: Vec<(&str, Outcome)> =
                rows.iter().map(|r| (r.id.as_str(), r.outcome)).collect();
            assert_eq!(ran, [(crate::init_fseventsd::ID, outcome)], "{only:?}");
        }
        assert!(
            !sys.calls().iter().any(|c| c.starts_with("tty ")),
            "{:?}",
            sys.calls()
        );
    }

    /// Account sign-out runs in the console user's graphic session with no terminal; an agent
    /// tab stays refused, and preferences still need a person at the console.
    #[test]
    fn sign_out_runs_without_a_terminal_but_not_from_an_agent_tab() {
        let options = Options {
            choice: Some(Choice {
                sign_out: true,
                ..Choice::NONE
            }),
            ..Options::default()
        };
        let groups = |_: &Ctx| {
            vec![(
                vec![Listed::boxed("init.test.dock", vec![ok()])],
                place(Class::Preference),
            )]
        };
        for (terminal, agent, allowed) in [
            (false, None, true),
            (false, Some("Claude Code"), false),
            (false, Some("Codex"), false),
            (true, Some("Claude Code"), false),
            (true, None, true),
        ] {
            let mut sys = alice_with_checkout(501, None);
            sys.terminal = terminal;
            sys.agent = agent;
            let ran = converge_with(&sys, &options, &groups, &NoRecords);
            if allowed {
                assert!(ran.is_ok(), "{ran:?}");
            } else {
                assert!(matches!(ran, Err(Refused::Usage(_))), "{ran:?}");
                assert!(!sys.calls().iter().any(|call| call.contains("/usr/bin/id")));
            }
        }
        let ctx = ctx(&[user("alice", 501)]);
        let accounts = self::groups(
            &ctx,
            Presence::NoTerminal,
            crate::init_secrets::Secrets {
                items: Vec::new(),
                waited_on: None,
            },
            Vec::new(),
            None,
            Choice {
                sign_out: true,
                ..Choice::NONE
            },
            &[],
        );
        let ids: Vec<String> = accounts
            .iter()
            .flat_map(|(items, _)| items.iter().map(|item| item.id()))
            .collect();
        assert_eq!(
            ids,
            [
                "init.messages.signed-out@alice",
                "init.facetime.signed-out@alice"
            ]
        );
        let preferences = Options {
            choice: Some(Choice {
                profile: true,
                ..Choice::NONE
            }),
            ..Options::default()
        };
        let mut sys = alice_with_checkout(501, None);
        sys.terminal = false;
        assert!(matches!(
            converge_with(&sys, &preferences, &groups, &NoRecords),
            Err(Refused::Usage(_))
        ));
    }

    #[test]
    fn an_invalid_init_toml_leaves_one_unread_row_and_the_rest_run() {
        let sys = alice_with_checkout(501, Some("[github\n"));
        let (ctx, _) = local_context(&sys).unwrap();
        let read = crate::init_github::items(&sys, &ctx, None);
        assert!(
            read.as_ref().is_err_and(|why| why.contains("init.toml")),
            "the group cannot be built"
        );
        let github = built("init.github", "init", Setting::Repositories.label(), read);
        let groups = groups(
            &ctx,
            Presence::AtConsole,
            crate::init_secrets::Secrets {
                items: Vec::new(),
                waited_on: None,
            },
            github,
            None,
            Choice::ALL,
            &[],
        );
        let applied = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let base = Base {
            items: vec![Box::new(BaseItem {
                probes: Mutex::new(vec![ok()]),
                applied,
            })],
            held: None,
            clt: None,
            toolset: None,
            targo: None,
        };
        let rows = run(&sys, &ctx, base, &groups, Mode::Plan, &mut NoJournal).unwrap();
        assert_eq!(rows[0].id, "base.pkg.shims");
        let github: Vec<&ItemReport> = rows.iter().filter(|row| row.id == "init.github").collect();
        assert_eq!(github.len(), 1, "{rows:?}");
        assert_eq!(github[0].outcome, Outcome::Unknown);
        assert!(
            github[0]
                .why
                .as_deref()
                .is_some_and(|why| why.contains("init.toml"))
        );
        assert!(
            rows.iter().any(|row| row.class == Some(Class::Preference)),
            "{rows:?}"
        );
    }

    /// The Permissions phase: a row for each grant aterm needs, Accessibility only for a run
    /// that signs out; a grant only the person switches on names how, and an apply is done
    /// with it while `--check` is not. A grant this process cannot read (an aterm outside
    /// aterm.app reads none) fails nothing: the run exits 0.
    #[test]
    fn each_missing_grant_is_one_row_naming_how_and_only_a_check_waits_on_it() {
        fn read(sign_out: bool) -> Vec<(Access, Grant)> {
            assert!(!sign_out, "this run does not sign out");
            vec![
                (
                    Access::FullDiskAccess,
                    Grant::NeedsYou("switch on aterm".into()),
                ),
                (Access::DeveloperTools, Grant::On),
                (
                    Access::Debugger,
                    Grant::Unread("tccd did not answer".into()),
                ),
            ]
        }
        let rows = permissions(read, false);
        assert_eq!(
            rows.iter().map(|row| row.id()).collect::<Vec<_>>(),
            [
                "init.permissions.full-disk-access",
                "init.permissions.developer-tools",
                "init.permissions.debugger"
            ]
        );
        let sys = Fake::root();
        let alice = at_console(user("alice", 501));
        assert!(matches!(
            rows[0].probe(&sys, &alice),
            Probe::Person { how, .. } if how == "switch on aterm"
        ));
        assert!(matches!(rows[1].probe(&sys, &alice), Probe::Ok { .. }));
        assert!(matches!(
            rows[2].probe(&sys, &alice),
            Probe::NotApplicable { why } if why.contains("tccd did not answer")
        ));
        assert_eq!(permissions(no_grants, true).len(), 4);
        let reported = run(
            &sys,
            &alice,
            Base::none(),
            &[(rows, workstation_place(Class::Person))],
            Mode::Apply,
            &mut NoJournal,
        )
        .unwrap();
        let person = reported
            .iter()
            .find(|row| row.outcome == Outcome::Person)
            .expect("the grant only the person switches on");
        assert!(counts(person, Action::Apply) && !counts(person, Action::Check));
        assert!(tally(&reported, Action::Apply).ends_with("needs you: Full Disk Access"));
        assert_eq!(status(&reported, Action::Apply), 0, "{reported:?}");
    }

    #[test]
    fn tally_names_swarm_enabled_false_in_init_check_summary() {
        let swarm_off_row = ItemReport {
            id: "init.secrets@alice".into(),
            family: "secrets".into(),
            layer: Some(Layer::Workstation),
            class: None,
            title: "Secrets vault (aterm help secrets)".into(),
            outcome: Outcome::NotApplicable,
            from: None,
            to: None,
            why: Some(aterm_secrets::swarm::SWARM_OFF.into()),
            revert: None,
            effect: aterm_converge::Effect::Now,
            detail: None,
            explain: None,
            stood_aside: None,
            essential: false,
            elapsed_ms: None,
        };
        let line = tally(&[swarm_off_row], Action::Plan);
        assert_eq!(
            line,
            "aterm init: 1 ok \u{b7} [swarm] enabled = false (nothing changed)"
        );
    }
}
