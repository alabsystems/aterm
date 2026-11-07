// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The local, per-user macOS setup, sharing the machines probe/apply/read-back engine.

use crate::console::Presence;
use crate::engine::{Journal, Place};
use crate::init_secrets::Here;
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

/// Writes `text`, what `aterm init` answers on standard output (its usage for `--help`, the
/// closing summary and the lines after the tally, the JSON report), to `out` and flushes it. A
/// reader gone (`aterm init | head -1`, a pager quit early) ends the answer, never the run,
/// whose exit status stays its own ([`aterm_uds::stdout::answer_status`]). Any other write
/// error (a full disk under `aterm init --json > file`, EIO) leaves the answer short: it is
/// said in one line through `say` (standard error), whose own failure (the same full disk under
/// `2>&1`) is dropped rather than unwinding, and the run fails (`Err`, its exit status).
fn answer_to(
    out: &mut dyn std::io::Write,
    text: &str,
    say: impl FnOnce(&str) -> std::io::Result<()>,
) -> Result<(), ExitCode> {
    let written = out.write_all(text.as_bytes()).and_then(|()| out.flush());
    aterm_uds::stdout::answer_status("aterm init", written, ()).map_err(|line| {
        let _ = say(&line);
        ExitCode::FAILURE
    })
}

/// [`answer_to`] on this process's standard output, a lost answer said on standard error
/// ([`aterm_uds::stdout::stderr_line`], which returns its error where `eprintln!` would panic).
fn answer(text: &str) -> Result<(), ExitCode> {
    answer_to(&mut std::io::stdout().lock(), text, |line| {
        aterm_uds::stdout::stderr_line(format_args!("{line}"))
    })
}

/// One line on standard error once a run or a converge pass has returned
/// ([`aterm_uds::stdout::stderr_line`]), its own failure dropped: `eprintln!` panics on any
/// stderr write error but EBADF (a full disk under `2>&1`), which would unwind past the exit
/// status the run decided and, after an apply, past the walk of the grants it hands over.
fn say_after_run(line: std::fmt::Arguments<'_>) {
    let _ = aterm_uds::stdout::stderr_line(line);
}

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
           [--[no-]profile] [--[no-]sign-out] [--[no-]clone] [--[no-]develop] [--no-wait]
aterm init --converge [--check] [--json] [--verbose] [--trigger subscriber|window|setup]
           [--repository-work repair|refresh]
  aterm.app's first launch does this setup by itself; init is for SSH and CLI-only Macs,
  and Linux (the base layer, the vault and --clone; each Mac row reads \"macOS only\").
  Run it as yourself, without sudo; safe to run again. Every Mac gets what `aterm doctor`
  restores, the ALab toolset, the Command Line Tools if missing (one sudo password), and
    {spotlight}
    {secrets} (authenticated ALab members admit this Mac on its home LAN, over SSH too)
    {permissions} (a row for each grant to switch on)
  An ALab Mac runs these too (--no-<flag> leaves one out); elsewhere --<flag> adds one:
    --profile   {profile}
    --sign-out  {sign_out}
    --clone     {clone}
                (your own, no forks or archived; init.toml [github] scopes or disables it)
                Initializes only required dependencies selected by the private inventory,
                at exact recorded gitlink commits; this needs no --develop.
  Preferences need you at the console; sign-outs run last, with you at the screen. A group
  off in System State (all after a Stop) stays as is unless a flag names it; one turned
  off mid-run stays as is. A setting you changed stays yours till Set up now or aterm
  doctor --apply. --plan reports, --check only reads, --json prints JSON, --verbose every
  row. Agent and headless preference checks relay the coordinator's last observation.
  --only runs those ids, families or groups (base, github, dock, ...); --[no-]develop
  records or drops this checkout as a develop machine; --no-wait waits for no System State
  pass (else 10 min); --converge runs one pass now. Exit: 0 done, or left to the vault or
  a person (--check: 77); 75 a lock or step, 76 unfinished --converge work,
  77 a permission, 69 offline; 1 else; 2 usage.
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
    /// What `--profile`, `--sign-out`, `--clone` and their `--no-` forms said ([`Picks`]); with
    /// none, this Mac's scope decides (no question: setup is on by default).
    picks: Picks,
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
    /// Requested repository work for one owned converge job, never effect authority.
    repository_work: Option<aterm_secrets::converge::RepositoryWork>,
    repository_request: Option<aterm_secrets::converge::RepositoryRequest>,
}

impl Options {
    /// What this run's base layer may do with the store lock ([`Host::base`], [`lock_for`]).
    fn base_lock(&self, applies: bool) -> BaseLock {
        lock_for(
            &self.only,
            match (applies, self.no_wait) {
                (false, _) => BaseLock::Read,
                (true, false) => BaseLock::Wait,
                (true, true) => BaseLock::Try,
            },
        )
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
}

/// The workstation groups a run applies: by this Mac's scope (every one on an ALab Mac, none
/// elsewhere), the ones `--profile`, `--sign-out` and `--clone` name, less the ones their
/// `--no-` forms leave out ([`Picks`]). Nothing asks: first-run setup is on by default and the
/// person turns a part off later (owner ruling 2026-10-25, AGENTS.md).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
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

    /// The groups `self` takes and `other` does not.
    const fn without(self, other: Self) -> Self {
        Self {
            profile: self.profile && !other.profile,
            sign_out: self.sign_out && !other.sign_out,
            clone: self.clone && !other.clone,
        }
    }

    /// Whether one of `self`'s groups holds `setting`; the base plan's settings are in none.
    const fn names(self, setting: Setting) -> bool {
        match setting {
            Setting::Messages | Setting::FaceTime => self.sign_out,
            Setting::Repositories => self.clone,
            Setting::CapsLock
            | Setting::NaturalScrolling
            | Setting::TrackpadScrolling
            | Setting::Desktop
            | Setting::Widgets
            | Setting::Dock => self.profile,
            Setting::Spotlight | Setting::Permissions | Setting::Secrets => false,
        }
    }

    /// The settings a run with `self`'s groups takes: the base plan every Mac runs
    /// ([`aterm_types::macos_setup::BASE_PLAN`]) and each group's own.
    fn selection(self) -> Selection {
        let mut selection = aterm_types::macos_setup::BASE_PLAN;
        selection.set(Setting::Repositories, self.clone);
        for setting in Setting::ALL {
            if self.names(setting) {
                selection.set(setting, true);
            }
        }
        selection
    }
}

/// How a person names `setting`'s group on `aterm init`'s command line, to run it where their
/// choice in System State left it as it is, or where this Mac's scope leaves it out: the
/// narrowest run that sets it up and nothing else, `aterm init --only` with its word
/// ([`only_word`]). A flag would run more: `--profile` sets every profile group, `--sign-out`
/// both sign-outs, and on an ALab Mac each adds to a scope that takes every group.
#[must_use]
pub const fn naming(setting: Setting) -> &'static str {
    match setting {
        Setting::CapsLock => "aterm init --only keyboard",
        Setting::NaturalScrolling => "aterm init --only scrolling",
        Setting::TrackpadScrolling => "aterm init --only trackpad",
        Setting::Desktop => "aterm init --only desktop",
        Setting::Widgets => "aterm init --only widgets",
        Setting::Dock => "aterm init --only dock",
        Setting::Messages => "aterm init --only init.messages",
        Setting::FaceTime => "aterm init --only init.facetime",
        Setting::Spotlight => "aterm init --only spotlight",
        Setting::Permissions => "aterm init --only permissions",
        Setting::Secrets => "aterm init --only secrets",
        Setting::Repositories => "aterm init --only github",
    }
}

/// The `--only` word that names `setting`'s group alone ([`naming`]): the family of its items,
/// or the id of the one sign-out.
#[must_use]
pub fn only_word(setting: Setting) -> &'static str {
    let way = naming(setting);
    way.strip_prefix("aterm init --only ").unwrap_or(way)
}

/// The settings a run takes ([`picked`]), and the one plain line it says of those in its scope
/// that the person's choices in System State held ([`Holds::line`]).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Picked {
    /// The settings this run applies.
    runs: Selection,
    /// What it says of the ones it left as the person set them.
    held: Option<String>,
    /// The settings in its scope it left as the person set them ([`Self::held`]'s).
    kept: Selection,
    /// The System State row each Desktop-lane item was built for, by its id, tagged as the
    /// item is built (an id may carry more than one: a group that could not be built stands for
    /// each of its rows). [`converge`] reads the person's choices again once it holds System
    /// State's records, and leaves the items whose rows they hold by then.
    rows: Vec<(String, Setting)>,
    /// Original selectors represented by the existing background workstation row, with
    /// the precise private settings its cached observation must cover.
    relayed: Option<RelayedSelection>,
    /// The person's choices as the run read them when it began: a group the run names (a flag's,
    /// or every one `--only` words name) runs over these holds alone, never over a Stop pressed
    /// or a group turned off since ([`Holds::later`]).
    start: Holds,
}

impl Picked {
    /// Every setting, holding none: a run whose `--only` words name what it runs.
    const ALL: Self = Self {
        runs: Selection::all(),
        held: None,
        kept: Selection::none(),
        rows: Vec::new(),
        relayed: None,
        start: Holds::NONE,
    };

    /// No setting: a run that builds no workstation.
    const NONE: Self = Self {
        runs: Selection::none(),
        held: None,
        kept: Selection::none(),
        rows: Vec::new(),
        relayed: None,
        start: Holds::NONE,
    };

    /// Whether this run signs out of Messages or FaceTime.
    const fn signs_out(&self) -> bool {
        self.runs.contains(Setting::Messages) || self.runs.contains(Setting::FaceTime)
    }

    /// Whether this run applies every preference and both sign-outs: the whole profile, which
    /// the setup records record as verified ([`Records::hold`]).
    const fn whole(&self) -> bool {
        let mut each = 0;
        while each < PROFILE.len() {
            if !self.runs.contains(PROFILE[each]) {
                return false;
            }
            each += 1;
        }
        self.runs.contains(Setting::Messages) && self.runs.contains(Setting::FaceTime)
    }
}

/// The settings a run takes (design §7): with `--only` words, every one, since the words name
/// what runs; otherwise this Mac's scope (`alab`: every group on an ALab Mac, the base plan
/// elsewhere) with the groups `picks` names and less the ones it leaves out, and less each
/// setting the person's choices in System State hold (`holds`) unless a flag names its group.
/// The line it says of those names only ways back that run where it is printed (`at_console`,
/// [`Holds::line`]).
fn picked(
    picks: Picks,
    only: &[String],
    alab: &dyn Fn() -> bool,
    holds: &Holds,
    at_console: bool,
) -> Picked {
    if !only.is_empty() {
        if at_console {
            return Picked {
                start: holds.clone(),
                ..Picked::ALL
            };
        }
        // Away from the console `--only` narrows the run and names no group over the person's
        // choices: it is no person's pick there, as `--profile`, `--sign-out` and `--clone` are
        // not ([`converge`] refuses those).
        let mut runs = Selection::all();
        let mut kept = Selection::none();
        for setting in Setting::ALL.into_iter().filter(|s| holds.holds(*s)) {
            runs.set(setting, false);
            kept.set(setting, true);
        }
        return Picked {
            runs,
            held: None,
            kept,
            rows: Vec::new(),
            relayed: None,
            start: holds.clone(),
        };
    }
    let scope = picks.choice(alab()).selection();
    let mut runs = Selection::none();
    let mut kept = Selection::none();
    let mut held = Vec::new();
    for setting in Setting::ALL.into_iter().filter(|s| scope.contains(*s)) {
        if holds.holds(setting) && !picks.named.names(setting) {
            held.push(setting);
            kept.set(setting, true);
        } else {
            runs.set(setting, true);
        }
    }
    Picked {
        runs,
        held: holds.line(&held, at_console),
        kept,
        rows: Vec::new(),
        relayed: None,
        start: holds.clone(),
    }
}

/// What `--profile`, `--sign-out`, `--clone` and `--no-profile`, `--no-sign-out`, `--no-clone`
/// said (design §7): on an ALab Mac every group runs and a `--no-` flag leaves one out; elsewhere
/// none runs and a flag adds one. A group a flag names runs even where the person turned it off
/// in System State: naming it is their choice too ([`Holds`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Picks {
    /// The groups `--profile`, `--sign-out` and `--clone` named.
    named: Choice,
    /// The groups `--no-profile`, `--no-sign-out` and `--no-clone` left out.
    left: Choice,
}

impl Picks {
    /// Whether any of the six flags was given.
    fn any(self) -> bool {
        self.named != Choice::NONE || self.left != Choice::NONE
    }

    /// The groups a run takes on a Mac `alab` says is an ALab Mac ([`Host::alab`]): its scope's
    /// groups and the named ones, less the ones left out.
    const fn choice(self, alab: bool) -> Choice {
        let scope = if alab {
            Choice::ALL
        } else {
            Choice {
                clone: true,
                ..Choice::NONE
            }
        };
        Choice {
            profile: scope.profile || self.named.profile,
            sign_out: scope.sign_out || self.named.sign_out,
            clone: scope.clone || self.named.clone,
        }
        .without(self.left)
    }
}

/// Whether this invocation can apply setup, using the same option parser as the run.
/// Queries remain read-only; an invalid command still reaches the normal usage refusal.
#[must_use]
pub fn launches_setup(args: &[OsString]) -> bool {
    options(args).map_or(true, |options| {
        !options.help && options.action == Action::Apply
    })
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
        let request = match arg.to_str() {
            Some("--repository-request") => {
                Some(args.next().and_then(|word| word.to_str()).unwrap_or(""))
            }
            Some(arg) => arg.strip_prefix("--repository-request="),
            None => None,
        };
        if let Some(json) = request {
            if json.len() > 4096 {
                return Err("repository request is too large".into());
            }
            options.repository_request = Some(
                aterm_json::from_str(json).map_err(|_| "invalid repository scheduling request")?,
            );
            continue;
        }
        let work = match arg.to_str() {
            Some("--repository-work") => {
                Some(args.next().and_then(|word| word.to_str()).unwrap_or(""))
            }
            Some(arg) => arg.strip_prefix("--repository-work="),
            None => None,
        };
        if let Some(word) = work {
            options.repository_work = Some(match word {
                "repair" => aterm_secrets::converge::RepositoryWork::Repair,
                "refresh" => aterm_secrets::converge::RepositoryWork::Refresh,
                _ => return Err("--repository-work takes repair or refresh".into()),
            });
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
            Some(
                flag @ ("--profile" | "--sign-out" | "--clone" | "--no-profile" | "--no-sign-out"
                | "--no-clone"),
            ) => {
                let (named, group) = match flag.strip_prefix("--no-") {
                    Some(group) => (false, group),
                    None => (true, &flag[2..]),
                };
                let (on, off) = (&mut options.picks.named, &mut options.picks.left);
                let (this, other) = if named { (on, off) } else { (off, on) };
                let (mine, theirs) = match group {
                    "profile" => (&mut this.profile, other.profile),
                    "sign-out" => (&mut this.sign_out, other.sign_out),
                    _ => (&mut this.clone, other.clone),
                };
                if theirs {
                    return Err(format!("choose either --{group} or --no-{group}"));
                }
                *mine = true;
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
    if options.repository_work.is_some() && options.repository_request.is_some() {
        return Err("choose repository work or a retained repository request".into());
    }
    if (options.repository_work.is_some() || options.repository_request.is_some())
        && !options.converge
    {
        return Err("--repository-work describes one --converge job".into());
    }
    if options.converge
        && (options.action == Action::Plan
            || !options.only.is_empty()
            || options.vault.is_some()
            || options.picks.any()
            || options.develop.is_some()
            || options.no_wait)
    {
        return Err(
            "--converge takes only --check, --json, --verbose, --trigger and --repository-work"
                .into(),
        );
    }
    if options.develop.is_some() && options.only.iter().any(|word| word == "workstation") {
        return Err("--develop and --no-develop cannot be combined with --only workstation".into());
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

/// The store lock a run narrowed to `only` asks its base layer for, `lock` unless no item it
/// selects touches the store: `--only base.clt` (System State's Mac Setup tab, and a
/// person's own run of it) selects Apple's Command Line Tools alone, which run once the lock is
/// let go, so its base layer is composed as a read ([`BaseLock::Read`]) and never waits for an
/// install pass that holds the store.
#[must_use]
pub fn lock_for(only: &[String], lock: BaseLock) -> BaseLock {
    if !only.is_empty() && only.iter().all(|word| word == crate::init_clt::ID) {
        BaseLock::Read
    } else {
        lock
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

/// Who decided that a run of `aterm init` happens ([`Host::decided`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decider {
    /// Whoever ran `aterm init` by name: a person, or a program that runs it for them.
    Person,
    /// aterm by itself: nobody ran `aterm init`, and aterm finishes setup after a package
    /// install or update a person typed ([`PACKAGE_FOLLOW_UP`]). Such a run puts no question of
    /// macOS's on the screen: its vault step asks nothing ([`crate::init_secrets::Here::Unbidden`])
    /// and reaches the LAN only through the LAN broker, as every run does. Rust in Spotlight
    /// searches no folder of the
    /// person's for build folders ([`crate::init_spotlight::Projects::Unread`]). Apple's Command
    /// Line Tools share the durable account administrator-ask episode, sharing System State's Mac Setup
    /// tab's ask, and never under the person's Stop, read as they run
    /// ([`Self::compose`]).
    Aterm,
}

impl Decider {
    /// Share the durable administrator-ask record in both modes. Automatic runs additionally
    /// respect current setup holds and an already-consumed episode. Explicit person retries
    /// remain available and record their first attempt before sudo can ask.
    pub(crate) fn compose(
        self,
        mut base: Base,
        unbidden: impl FnOnce() -> crate::init_clt::Unbidden,
    ) -> Base {
        base.clt = base.clt.map(|clt| match self {
            Self::Aterm => clt.unbidden(unbidden()),
            Self::Person => clt.explicit(unbidden()),
        });
        base
    }
}

/// The words of `aterm init` that finishes setup after a person's package install or update
/// ([`Decider::Aterm`]). `--no-wait` waits behind no System State pass and stands each base item
/// aside for a pass that holds the store. `--no-sign-out` builds no Messages or FaceTime
/// sign-out, so nothing asks macOS for Accessibility or for Automation of System Events, and
/// the Permissions rows leave Accessibility out. `--no-profile` leaves the workstation
/// preferences to System State: they read and write other apps' defaults domains, which macOS
/// may answer with its dialog about access to other apps' data.
pub const PACKAGE_FOLLOW_UP: [&str; 3] = ["--no-wait", "--no-sign-out", "--no-profile"];

/// What `aterm init` takes from the `aterm` that runs it ([`main_entry`]).
pub struct Host<'a> {
    /// Who decided this run happens ([`Decider`]).
    pub decided: Decider,
    /// The base layer ([`Base`]), its store lock as [`BaseLock`] says.
    pub base: &'a dyn Fn(BaseLock) -> Base,
    /// Whether this process holds Full Disk Access, as the runner measures it.
    pub full_disk_access: &'a dyn Fn() -> Option<bool>,
    /// This Mac's setup records, shared with the first launch ([`Records`]): `'static`, since a
    /// run aterm decided reads their holds again as `base.clt` runs, long after the run composed
    /// it ([`crate::init_clt::Unbidden::here`]).
    pub records: &'static dyn Records,
    /// aterm's macOS grants, as the Permissions phase reads them ([`Grants`]).
    pub grants: Grants,
    /// The running window's already-published workstation observation. Only a read-only
    /// check away from a person's terminal asks this port for its selected private settings;
    /// it never probes other apps.
    pub workstation_check: &'a dyn Fn(Selection) -> WorkstationObservation,
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
    /// Whether this is an ALab Mac, by the one rule System State's window reads (a membership of
    /// the ALab vault opened under this Mac's own key): its workstation profile, clones and
    /// sign-outs run by default there, and elsewhere only when a flag names them (design §7).
    pub alab: &'a dyn Fn() -> bool,
    /// Whether a person is at this Mac's screen now, as the window's person stages read it: a
    /// keyboard, mouse or trackpad used within the last minute, and the screen neither locked
    /// nor switched to another login. Read on every run right before the Messages and FaceTime
    /// sign-outs, again by each of them right before its own clicks (the run's port,
    /// [`Live::with_presence`]), and with no terminal also as the run decides whether to build
    /// them and whether `--sign-out` runs (design §6.4).
    pub present: fn() -> bool,
}

/// What a person's own choices in System State hold of this Mac's setup ([`Records::holds`]):
/// `aterm init` and `aterm doctor --apply` leave each such group as it is, and only its switch on
/// the page, or the person naming it on the command line, turns it on again (design §3,
/// principle 4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Holds {
    /// The settings the person turned off (Leave as is) in a plan of their own, or that an older
    /// aterm's Cancel left as they were. A plan aterm chose by itself holds none.
    pub off: Selection,
    /// Why every group waits whatever its switch says: the person's Stop of setup, or System
    /// State's records that cannot be read now.
    pub stopped: Option<String>,
    /// The original revision of the stopped run, including one a Restore carries on. `None`
    /// when the records give no stopped run, as when they cannot be read.
    pub stopped_revision: Option<u64>,
    /// A newer aterm saved this Mac's setup records: every group waits, and this aterm, which
    /// leaves that setup alone, names no way to set one up ([`Self::line`]).
    pub newer: bool,
}

impl Default for Holds {
    fn default() -> Self {
        Self::NONE
    }
}

impl Holds {
    /// Nothing held: a Mac with no setup records, or a plan aterm chose by itself.
    pub const NONE: Self = Self {
        off: Selection::none(),
        stopped: None,
        stopped_revision: None,
        newer: false,
    };

    /// Whether `setting` is held as it is.
    #[must_use]
    pub fn holds(&self, setting: Setting) -> bool {
        self.newer || self.stopped.is_some() || self.off.contains(setting)
    }

    /// Whether these holds hold `setting` for a reason `earlier`, read before them, did not: a
    /// Stop, records a newer aterm saved, or the group turned off since. A run that names a group
    /// runs it over a hold already on record when the run began, never over a later one: a Stop
    /// pressed, or a group turned off, after `aterm init` started is the person's later choice,
    /// and holds what has not run yet (principle 4).
    #[must_use]
    pub fn later(&self, earlier: &Self, setting: Setting) -> bool {
        self.later_stop(earlier)
            || (self.newer && !earlier.newer)
            || (self.off.contains(setting) && !earlier.off.contains(setting))
    }

    /// Whether a Stop appeared, or names a different run, since `earlier` was read.
    fn later_stop(&self, earlier: &Self) -> bool {
        self.stopped.is_some()
            && (earlier.stopped.is_none() || self.stopped_revision != earlier.stopped_revision)
    }

    /// Why these holds hold `setting`, in the words a row it left ends with.
    #[must_use]
    pub fn why(&self, setting: Setting) -> String {
        if self.newer {
            "left as it is: a newer aterm set this Mac up, and this aterm leaves its settings \
             alone"
                .to_owned()
        } else if let Some(why) = &self.stopped {
            format!("left as it is because {why}")
        } else if self.off.contains(setting) {
            "left as you set it in System State".to_owned()
        } else {
            "left as it is".to_owned()
        }
    }

    /// The one plain line a run says of `held`, the settings these holds kept from it, naming
    /// how one comes back on, and only a way that runs where the line is printed: its switch in
    /// System State (Set up now after a Stop) anywhere, and for a person at this Mac's console
    /// (`at_console`) the narrowest `aterm init` that names it too ([`naming`]), which an
    /// agent's tab, an SSH shell or a run with no terminal refuses. Records a newer aterm saved
    /// name no way: this aterm leaves that setup alone. `None` when it kept none.
    #[must_use]
    pub fn line(&self, held: &[Setting], at_console: bool) -> Option<String> {
        if held.is_empty() {
            return None;
        }
        let names = held
            .iter()
            .map(|setting| setting.name())
            .collect::<Vec<_>>()
            .join(", ");
        if self.newer {
            return Some(format!(
                "left as they are: a newer aterm set this Mac up, and this aterm leaves its \
                 settings alone: {names}"
            ));
        }
        // One group's way back is its run; several are one `--only` with each one's word, in
        // the order the names are.
        let flags = match held {
            _ if !at_console => String::new(),
            [one] => format!(", or run {}", naming(*one)),
            several => format!(
                ", or run aterm init --only with each one's word ({})",
                several
                    .iter()
                    .map(|setting| only_word(*setting))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        };
        Some(match &self.stopped {
            Some(why) => format!(
                "left as they are because {why}: {names}. Set up now in System State goes \
                 on{flags}"
            ),
            None => format!(
                "left as you set them in System State: {names}. Turn one on with its switch \
                 there{flags}"
            ),
        })
    }
}

/// What a pass that holds System State's records sets ([`Records::hold`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pass {
    /// Part of the profile, or none of its preferences (the sign-outs alone): the hold records
    /// nothing of it as the plan, and no stage of the window's run as done.
    Part,
    /// Every preference and both sign-outs, recorded as the plan the way `aterm doctor --apply`
    /// records its pass.
    Whole {
        /// The pass clones the GitHub repositories too, so the plan it saves holds them.
        clones: bool,
    },
}

impl Pass {
    /// Whether the pass sets the whole profile.
    #[must_use]
    pub const fn whole(self) -> bool {
        matches!(self, Self::Whole { .. })
    }
}

/// This Mac's setup records (`profile.lock` and the initial-setup record), which `aterm init`
/// shares with the first launch's setup and `aterm doctor --apply`: a pass that changes the
/// workstation holds them, so two surfaces never relaunch the Dock over each other, and a whole
/// profile it verified is recorded, so the first launch neither runs nor offers it again.
pub trait Records: Sync {
    /// Hold the records for a workstation pass that sets `pass`, waiting a bounded while another
    /// setup holds them (`wait` hears one line as that wait starts; `None`, `--no-wait`, waits for
    /// nothing).
    ///
    /// # Errors
    /// [`Unheld::Busy`]: another setup held them past the wait (or at once, with no wait);
    /// [`Unheld::Failed`]: they cannot be held or written. Either way nothing was changed.
    fn hold(&self, pass: Pass, wait: Option<&dyn Fn(&str)>) -> Result<Box<dyn Held>, Unheld>;

    /// What the person's own choices in System State hold ([`Holds`]), read from these records
    /// without holding them. Records that cannot be read hold every group, saying why.
    fn holds(&self) -> Holds {
        Holds::default()
    }

    /// The items whose keys the person changed after aterm set them (design §6.6): `aterm init`
    /// leaves each as they set it ([`KEPT_BY_YOU`]), as the window's unattended pass does. Read
    /// while the records are held.
    ///
    /// # Errors
    /// The record of those keys cannot be read: init then sets none of the preferences
    /// ([`KEPT_UNREAD`]).
    fn kept(&self) -> Result<Vec<String>, String> {
        Ok(Vec::new())
    }
}

/// Why [`Records::hold`] (or [`Held::again`]) did not hold the setup records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unheld {
    /// Another setup held them past the wait, or at once for a run that waits for nothing: the
    /// Desktop lane's items stand aside for that lock, in these words, and the run goes on.
    Busy(String),
    /// They cannot be held or written: the run ends saying why.
    Failed(String),
}

impl From<String> for Unheld {
    fn from(why: String) -> Self {
        Self::Failed(why)
    }
}

impl From<Unheld> for String {
    fn from(unheld: Unheld) -> Self {
        match unheld {
            Unheld::Busy(why) | Unheld::Failed(why) => why,
        }
    }
}

/// A hold on the setup records ([`Records::hold`]), let go when dropped.
pub trait Held: Send {
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
    /// hold taken for a [`Pass::Whole`].
    ///
    /// # Errors
    /// The record cannot be written.
    fn verified(self: Box<Self>) -> Result<(), String>;

    /// Let the records go while the run's other lanes finish (the clones, the Command Line
    /// Tools), keeping what this hold records of the pass: System State's Stop, its switches,
    /// Restore and Set up now wait only while the preferences change, and [`Self::again`] holds
    /// the records once more for the sign-outs.
    fn release(&mut self) {}

    /// Hold the records again after [`Self::release`], waiting as [`Records::hold`] waits.
    ///
    /// # Errors
    /// As [`Records::hold`]; nothing was changed.
    fn again(&mut self, _wait: Option<&dyn Fn(&str)>) -> Result<(), Unheld> {
        Ok(())
    }
}

/// No setup records: a test's run, which shares nothing with a first launch.
pub struct NoRecords;

impl Records for NoRecords {
    fn hold(&self, _pass: Pass, _wait: Option<&dyn Fn(&str)>) -> Result<Box<dyn Held>, Unheld> {
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
    entry(
        args,
        host,
        std::env::var_os(aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB).as_deref(),
        &tab_entry,
    )
}

/// [`main_entry`] with the tab's variable as `tab_var` and the tab's own run as `tab_run`
/// ([`in_tab`], [`tab_entry`]).
fn entry(
    args: Vec<OsString>,
    host: &Host<'_>,
    tab_var: Option<&std::ffi::OsStr>,
    tab_run: &dyn Fn(&Options, &Host<'_>) -> u8,
) -> ExitCode {
    let mut options = match options(&args) {
        Ok(options) => options,
        Err(why) => {
            eprintln!("aterm init: {why}\n{}", usage());
            return ExitCode::from(2);
        }
    };
    if options.help {
        return answer(&usage()).map_or_else(|failed| failed, |()| ExitCode::SUCCESS);
    }
    if in_tab(tab_var, &options) {
        return ExitCode::from(tab_run(&options, host));
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
    // What the person turned off in System State, or their Stop, holds this run's groups as it
    // holds the window's; the line saying so follows the tally.
    let held: Mutex<Vec<ItemReport>> = Mutex::new(Vec::new());
    // The person's choices as the run read them when it began: the clone it owes is stamped
    // with them ([`note_owed`]).
    let start: Mutex<Holds> = Mutex::new(Holds::NONE);
    let groups = |sys: &dyn System, ctx: &Ctx, mode: Mode, reading: &Reading<'_>| {
        let standing = Standing {
            decided: host.decided,
            alab: host.alab,
            present: &host.present,
            holds: host.records.holds(),
            workstation_observation: observed_workstation(
                sys,
                options.action,
                reading.context_scope,
                host.workstation_check,
            ),
        };
        *start.lock().unwrap_or_else(PoisonError::into_inner) = standing.holds.clone();
        prepare_groups(
            sys,
            ctx,
            mode,
            options.vault.as_deref(),
            reading,
            &standing,
            host.grants,
        )
    };
    let sys = Live::new().with_presence(host.present);
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
                    base: &|applies| {
                        host.decided
                            .compose((host.base)(options.base_lock(applies)), || {
                                crate::init_clt::Unbidden::here(host.records)
                            })
                    },
                    groups: &groups,
                    full_disk_access: host.full_disk_access,
                    targo: composed.targo.clone(),
                    cwd: cwd.as_deref(),
                    records: host.records,
                    on_row: &|row| printer.row(row),
                    present: &host.present,
                    on_held: &|row| {
                        held.lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .push(row)
                    },
                },
                journal.as_mut(),
                &mut laps,
            )
        });
    printer.flush();
    let rows = match result {
        Ok(rows) => rows,
        Err(Refused::Usage(why)) => {
            say_after_run(format_args!("aterm init: {why}\n{}", usage()));
            return ExitCode::from(2);
        }
        Err(Refused::Failed(why)) => {
            aterm_log::info!("aterm init: no report; {laps}");
            say_after_run(format_args!("aterm init: {why}"));
            return ExitCode::FAILURE;
        }
    };
    let start = start.into_inner().unwrap_or_else(PoisonError::into_inner);
    let recorded = note_owed(
        &rows,
        options.action,
        host.converge.state_dir().as_deref(),
        &start,
    );
    if let Err(why) = &recorded {
        say_after_run(format_args!(
            "aterm init: {why}; once this Mac is let into the vault, {} runs the GitHub step",
            naming(Setting::Repositories)
        ));
    }
    let complete = complete(&rows, options.action);
    let code = status(&rows, options.action);
    aterm_log::info!("aterm init: {} rows, exit {code}; {laps}", rows.len());
    let held = held.into_inner().unwrap_or_else(PoisonError::into_inner);
    let answered = if options.json {
        let mut rows = rows;
        rows.extend(held);
        match report_json(options.action, complete, rows, &laps) {
            Ok(report) => answer(&format!("{report}\n")),
            Err(why) => {
                say_after_run(format_args!("aterm init: cannot encode report: {why}"));
                return ExitCode::FAILURE;
            }
        }
    } else {
        use std::fmt::Write as _;
        let mut summary = format!(
            "{}\n",
            completion_summary(&rows, options.action, recorded.is_ok())
        );
        for line in held.into_iter().filter_map(|row| row.why) {
            let _ = writeln!(summary, "aterm init: {line}");
        }
        let mut answered = answer(&summary);
        // The walk is handed over whatever became of the summary: it is the person's grants,
        // not part of the answer. Nothing written since the run unwinds ([`say_after_run`],
        // [`answer`]), so a lost line never keeps it from them.
        let missing = walk_grants(&rows);
        if options.action == Action::Apply
            && !missing.is_empty()
            && let Some(line) = (host.walk)(&missing)
            && answered.is_ok()
        {
            answered = answer(&format!("aterm init: {line}\n"));
        }
        answered
    };
    answered.map_or_else(|failed| failed, |()| ExitCode::from(code))
}

/// The id of the row `--json` carries for the line of what the person's choices in System
/// State held ([`held_row`]).
const HELD: &str = "init.system-state";

/// The id of the row `--json` carries for the line of the keys the person changed since aterm
/// set them ([`held_row`]).
const CHANGED: &str = "init.changed-by-you";

/// The row `--json` carries for `line` ([`Front::on_held`]), a line a run says of what the
/// person's choices held, which the text output prints after its tally: the groups System State
/// holds ([`HELD`], [`Holds::line`]) or the keys they changed ([`CHANGED`]). Each is left as
/// they set it, so not applicable to this run, and no failure.
fn held_row(id: &str, title: &str, line: String) -> ItemReport {
    ItemReport {
        id: id.into(),
        family: "init".into(),
        title: title.into(),
        outcome: Outcome::NotApplicable,
        from: None,
        to: None,
        why: Some(line),
        revert: None,
        effect: Effect::Now,
        detail: None,
        layer: Some(Layer::Workstation),
        class: None,
        explain: None,
        stood_aside: None,
        essential: false,
        elapsed_ms: Some(0),
    }
}

/// Whether this run is System State's Mac Setup tab's program: `--only base.clt` alone,
/// with [`aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB`] set (to `var`) and not empty
/// ([`tab`]). Anywhere else, `aterm init --only base.clt` speaks as it always does.
fn in_tab(var: Option<&std::ffi::OsStr>, options: &Options) -> bool {
    var.is_some_and(|value| !value.is_empty())
        && *options
            == Options {
                only: vec![crate::init_clt::ID.to_owned()],
                ..Options::default()
            }
}

/// What the tab's run exits with when the window's go line never came ([`tab`]): neither 0 nor
/// 1, which say the run printed its last line, so the window ends the tab itself if it still
/// shows ([`aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_NOT_STARTED`]).
const TAB_NOT_STARTED: u8 = aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_NOT_STARTED;

/// Clears the tab's screen and scrollback once its run starts, so nothing typed or echoed while
/// it waited for the go line stays in view.
const TAB_CLEAR: &str = "\x1b[H\x1b[2J\x1b[3J";

/// [`tab`] on this Mac: its words on this run's terminal, its install in this account.
fn tab_entry(options: &Options, host: &Host<'_>) -> u8 {
    let sys = Live::new().with_presence(host.present);
    let out = crate::init_clt::Tab::new(Arc::new(|text: &str| {
        use std::io::Write as _;
        let mut stdout = std::io::stdout().lock();
        let _ = stdout.write_all(text.as_bytes());
        let _ = stdout.flush();
    }));
    let cwd = std::env::current_dir().ok();
    let groups = |_: &dyn System, _: &Ctx, _: Mode, _: &Reading<'_>| Ok((Vec::new(), Picked::NONE));
    tab(&sys, &out, &crate::sessions::is_live_account, &|tab| {
        let in_tab = |applies: bool| {
            let mut composed = (host.base)(options.base_lock(applies));
            composed.clt = composed.clt.map(|clt| clt.in_tab(tab.clone()));
            composed
        };
        let mut laps = Laps::start();
        sys.journal()
            .map_err(Refused::Failed)
            .and_then(|mut journal| {
                converge(
                    &sys,
                    options,
                    &Front {
                        base: &in_tab,
                        groups: &groups,
                        full_disk_access: host.full_disk_access,
                        targo: None,
                        cwd: cwd.as_deref(),
                        records: host.records,
                        on_row: &|_| {},
                        present: &host.present,
                        on_held: &|_| {},
                    },
                    journal.as_mut(),
                    &mut laps,
                )
            })
    })
}

/// System State's Mac Setup tab's run of `aterm init --only base.clt` (design §3.3),
/// speaking through `out`: it waits for the window's go line
/// ([`aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_GO`], read with echo off and kept when
/// typed ahead of the read), which comes only once the tab landed, and else starts nothing,
/// says nothing and exits [`TAB_NOT_STARTED`]. Then it clears the screen, installs through
/// `converge` (in this person's own login, `live`), and ends on one line:
/// [`crate::init_clt::TAB_INSTALLED`] and 0, or why the install did not finish
/// ([`crate::init_clt::Unfinished::line`]) and that class's exit
/// ([`crate::init_clt::Unfinished::tab_exit`]: 1, or Low Data Mode's, a non-administrator's, a
/// stop's and Apple's window closed's own). None of init's own report is printed.
fn tab(
    sys: &dyn System,
    out: &crate::init_clt::Tab,
    live: &dyn Fn() -> bool,
    converge: &dyn Fn(&crate::init_clt::Tab) -> Result<Vec<ItemReport>, Refused>,
) -> u8 {
    // Read with echo off, keeping what was typed before the read: the window types the go line
    // as soon as the tab lands, often before this run reaches here, and a read that discarded
    // typed-ahead input would wait for a line that never comes again. The screen clear after
    // it removes the line's echo.
    let go = sys.ask_kept("");
    if !matches!(&go, Ok(Some(line))
        if line.bytes() == aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_GO.as_bytes())
    {
        return TAB_NOT_STARTED;
    }
    drop(go);
    out.write(TAB_CLEAR);
    let installed = if live() {
        converge(out).is_ok_and(|rows| {
            rows.iter().any(|row| {
                row.id == crate::init_clt::ID
                    && matches!(row.outcome, Outcome::Ok | Outcome::Changed)
            })
        })
    } else {
        out.note(crate::init_clt::Unfinished::NotOwnLogin);
        false
    };
    if installed {
        out.line(crate::init_clt::TAB_INSTALLED);
        0
    } else {
        let why = out
            .unfinished()
            .unwrap_or(crate::init_clt::Unfinished::Other);
        out.line(&why.line());
        out.exit(why)
    }
}

/// `aterm init --converge`: one converge pass ([`crate::sync::converge`]) over `ports`, which
/// it reaches only through them (no workstation item, question or setup record), at background
/// QoS. `--check` probes and writes nothing. Its rows print as init's do, then a tally (or the
/// JSON report). Exit 0 once admitted work settled (or a check ran without failure),
/// [`aterm_secrets::converge::UNFINISHED`] while an apply pass has work left, and
/// [`aterm_secrets::converge::HELD`] while another pass holds the lock; 1 for a failed row,
/// an unwritten record, a pass that cannot
/// run here or a tally standard output could not take ([`answer_to`]). A subscriber's pass
/// from a program that is not the installed release app (a held copy) is the installed app's
/// to run ([`hand_off`]).
fn converge_entry(options: &Options, ports: &dyn crate::sync::Ports) -> ExitCode {
    let trigger = options.trigger.unwrap_or(crate::sync::Trigger::Init);
    ports.background(trigger);
    let request = options.repository_request.unwrap_or_else(|| {
        let observed = if options.action == Action::Check {
            Default::default()
        } else {
            ports
                .state_dir()
                .map(|state| crate::sync::repository_refresh::observe(&state, ports.now_ms()))
                .unwrap_or_default()
        };
        crate::sync::repository_refresh::Job::from(
            options
                .repository_work
                .unwrap_or(aterm_secrets::converge::RepositoryWork::Refresh),
        )
        .request(observed)
    });
    if trigger == crate::sync::Trigger::Subscriber
        && let Some(program) = ports.installed_release_elsewhere()
        && let Some(code) = hand_off(options, ports, &program, request)
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
    let pass = crate::sync::converge(ports, trigger, mode, request, &mut |row| printer.row(row));
    printer.flush();
    let pass = match pass {
        Ok(pass) => pass,
        Err(crate::sync::Stop::Held) => {
            say_after_run(format_args!(
                "aterm init: another converge pass holds {}/{}; this one did not run",
                aterm_converge::FileJournal::DIR,
                crate::sync::LOCK
            ));
            return ExitCode::from(aterm_secrets::converge::HELD);
        }
        Err(crate::sync::Stop::Refused(why)) => {
            say_after_run(format_args!("aterm init: {why}"));
            return ExitCode::FAILURE;
        }
    };
    let failed = pass.rows.iter().any(|row| row.outcome.is_failure());
    let complete = pass.complete();
    let answered = if options.json {
        match report_json(action, complete, pass.rows, &laps) {
            Ok(report) => answer(&format!("{report}\n")),
            Err(why) => {
                say_after_run(format_args!("aterm init: cannot encode report: {why}"));
                return ExitCode::FAILURE;
            }
        }
    } else {
        answer(&format!(
            "{} \u{b7} converge {}/{}\n",
            completion_summary(&pass.rows, action, pass.unrecorded.is_none()),
            trigger.word(),
            pass.due.word()
        ))
    };
    if let Some(why) = &pass.unrecorded {
        say_after_run(format_args!(
            "aterm init: the pass's record was not written: {why}"
        ));
    }
    if failed || pass.unrecorded.is_some() {
        ExitCode::FAILURE
    } else {
        answered.map_or_else(
            |failed| failed,
            |()| {
                if mode == Mode::Apply && !complete {
                    ExitCode::from(aterm_secrets::converge::UNFINISHED)
                } else {
                    ExitCode::SUCCESS
                }
            },
        )
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
fn hand_off(
    options: &Options,
    ports: &dyn crate::sync::Ports,
    program: &Path,
    request: aterm_secrets::converge::RepositoryRequest,
) -> Option<ExitCode> {
    let mut args = ["init", "--converge", "--trigger"]
        .map(str::to_owned)
        .to_vec();
    args.push(crate::sync::Trigger::Subscriber.word().to_owned());
    let encoded = match aterm_json::to_string(&request) {
        Ok(encoded) => encoded,
        Err(_) => return None,
    };
    args.extend(["--repository-request".to_owned(), encoded]);
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
        later.push(rendered(row, self.verbose));
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

/// `row` as `aterm init` prints it: its word and title (with `--verbose` its id too), then what
/// changed or why.
fn rendered(row: &ItemReport, verbose: bool) -> String {
    use std::fmt::Write as _;
    let word = if waiting(row) {
        "waiting"
    } else {
        word(row.outcome)
    };
    let mut text = if verbose {
        format!("{word}: {} ({})\n", row.title, row.id)
    } else {
        format!("{word}: {}\n", row.title)
    };
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
        (count(owed), "to run once this Mac is let in"),
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

/// Announce success only after both the selected items and the pass's record are complete.
fn completion_summary(rows: &[ItemReport], action: Action, recorded: bool) -> String {
    let mut summary = tally(rows, action);
    if recorded
        && action != Action::Plan
        && !rows.is_empty()
        && rows.iter().all(|row| {
            matches!(
                row.outcome,
                Outcome::Ok | Outcome::Changed | Outcome::NotApplicable
            )
        })
    {
        summary.push_str(" — Successfully completed. No further action is needed.");
    }
    summary
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
    (matches!(row.outcome, Outcome::Person | Outcome::Deferred) && !owed(row))
        || (row.outcome == Outcome::Blocked && !waits_by_itself(row) && !waits_on_a_step(row))
}

/// What a row that waits for this Mac to be let into the vault says ([`owe`]): the run records
/// it owed ([`note_owed`]), and the converge pass clones then ([`clone_owed`]), unless the
/// person leaves GitHub repositories as they are in System State after this run began; a Stop
/// holds it until Set up now ([`owed_clone`]).
const OWED: &str = "runs by itself once this Mac is let into the vault: the vault's sync clones \
                    when the GitHub token lands, with no second aterm init, unless you leave \
                    GitHub repositories as they are in System State before then (a Stop there \
                    holds it until Set up now)";

/// The record an apply leaves under the account's converge state when its GitHub step waits
/// for this Mac to be let into the vault ([`owed`]): the converge pass the vault's subscriber
/// starts once the vault's files land runs that step then, once ([`owed_clone`],
/// [`clone_owed`]), so nobody runs `aterm init` again for it (design §7).
const CLONE_OWED: &str = "clone-owed";

/// What `sync.clone` reads of the clone an `aterm init` owes ([`owed_clone`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwedClone {
    /// No clone was requested and no vault inventory can be verified here.
    Nothing(String),
    /// The private vault inventory was read and every repository verified locally.
    Verified { present: usize, total: usize },
    /// One is owed, but init.toml turned the GitHub step off since: why.
    Off(String),
    /// One is owed, and the token it waits for, or the git every clone runs, is not on this Mac
    /// yet: why it waits.
    Waiting(String),
    /// One is owed, and its token is here: what the pass finds.
    Due(String),
    /// One is owed, but its last run stopped at a step only a person can take, and neither
    /// the token nor init.toml changed since: what they do, in plain words. The private
    /// inventory still rechecks preserved checkout work locally, so fixing it resumes setup.
    Person(String),
    /// One is owed, but the person left GitHub repositories as they are in System State since
    /// the run that owes it began: their held line ([`Holds::line`]). A hold already on record
    /// then, which the run went past because the person named the group (`aterm init --only
    /// github`), keeps it owed, and a Stop holds it owed ([`OwedClone::Waiting`]).
    Held(String),
}

/// The line of [`CLONE_OWED`] that records what held GitHub repositories when the run that owes
/// the clone began ([`Holds`]): `held`, then `off`, `stopped` and `newer`, each that held them.
const HELD_MARK: &str = "held";

/// [`HELD_MARK`]'s line for `holds`.
fn held_stamp(holds: &Holds) -> String {
    let mut line = HELD_MARK.to_owned();
    for (word, held) in [
        ("off", holds.off.contains(Setting::Repositories)),
        ("stopped", holds.stopped.is_some()),
        ("newer", holds.newer),
    ] {
        if held {
            line.push(' ');
            line.push_str(word);
        }
    }
    line
}

/// What held GitHub repositories when the run that owes the clone began, as `record` says
/// ([`held_stamp`]); nothing for a record that says none (an older aterm's).
fn stamped_holds(record: &str) -> Holds {
    let mut holds = Holds::NONE;
    let words = record
        .lines()
        .filter_map(|line| line.strip_prefix(HELD_MARK))
        .filter(|rest| rest.is_empty() || rest.starts_with(' '))
        .flat_map(str::split_whitespace);
    for word in words {
        match word {
            "off" => holds.off.set(Setting::Repositories, true),
            "stopped" => holds.stopped = Some(String::new()),
            "newer" => holds.newer = true,
            _ => {}
        }
    }
    holds
}

/// The account's converge state under `state`, where [`CLONE_OWED`] lives.
fn converge_state(state: &Path) -> PathBuf {
    state.join(aterm_converge::FileJournal::DIR)
}

/// Record, under the account's state directory `state`, that an apply's GitHub step waits for
/// this Mac to be let into the vault ([`CLONE_OWED`]), stamped with what the person's choices in
/// System State held of GitHub repositories when the run began (`start`, [`HELD_MARK`]);
/// nothing for a plan, a check or a run whose rows owe nothing.
///
/// # Errors
/// The record cannot be written, or there is no state directory to write it in.
fn note_owed(
    rows: &[ItemReport],
    action: Action,
    state: Option<&Path>,
    start: &Holds,
) -> Result<(), String> {
    if action != Action::Apply || !rows.iter().any(owed) {
        return Ok(());
    }
    owe_clone(
        state.ok_or("no state directory records the clone it owes")?,
        start,
    )
}

/// Write [`CLONE_OWED`] under the account's state directory `state`, stamped with `start`
/// ([`note_owed`]).
///
/// # Errors
/// The record cannot be written.
pub(crate) fn owe_clone(state: &Path, start: &Holds) -> Result<(), String> {
    let dir = converge_state(state);
    aterm_uds::private_file::ensure_private_dir(&dir)
        .map_err(|why| format!("{}: {why}", dir.display()))?;
    aterm_uds::private_file::write_private(
        &dir,
        CLONE_OWED,
        format!(
            "aterm init's GitHub step waits for this Mac to be let into the vault\n{}\n",
            held_stamp(start)
        )
        .as_bytes(),
    )
    .map_err(|why| format!("{}: {why}", dir.join(CLONE_OWED).display()))
}

/// The line [`CLONE_OWED`] carries when the step's last run stopped at a step only a person
/// can take: the step's mark then ([`crate::init_github::TokenRead::mark`]) follows it.
const PERSON_MARK: &str = "mark ";

/// `sync.clone`'s reading under the account's state directory `state`: whether an `aterm init`
/// owes its GitHub step ([`note_owed`]), whether the person's choices in System State (`now`)
/// came to hold GitHub repositories after the run that owes it began ([`OwedClone::Held`]), and
/// whether the token that step reads, and Apple's git, are on this Mac now. A step only a person
/// can finish waits for them until its source changes, or a local read of the private
/// inventory finds preserved work ready to resume ([`OwedClone::Person`]); a Stop holds the
/// step and keeps it owed ([`OwedClone::Waiting`]).
///
/// # Errors
/// The record, this login's context or its init.toml cannot be read.
pub fn owed_clone(sys: &dyn System, state: &Path, now: &Holds) -> Result<OwedClone, String> {
    owed_clone_with(sys, state, now, None)
}

/// Read default repository debt using inputs captured under the caller's ready vault.
/// All Git work runs after that capture released its lock. `current` must fully
/// validate the same materialization without waiting, immediately before publication.
/// This reads only: no clone, source change, or debt-record write is admitted here.
///
/// # Errors
/// As [`owed_clone`], or `current` fails. A changed account or default source waits.
pub fn owed_clone_captured(
    sys: &dyn System,
    state: &Path,
    now: &Holds,
    inputs: crate::init_github::InitialRepositoryInputs,
    current: impl FnOnce() -> Result<bool, String>,
) -> Result<OwedClone, String> {
    let ctx = repository_context(sys)?;
    if let Err(why) = inputs.validate_default_context(sys, &ctx) {
        return Ok(OwedClone::Waiting(why));
    }
    let value = owed_clone_with(sys, state, now, Some(&inputs))?;
    let syncing = || {
        OwedClone::Waiting(
            "repository inputs changed or are still syncing; waiting for a new ready snapshot"
                .into(),
        )
    };
    if !current()? {
        return Ok(syncing());
    }
    if let Err(why) = inputs.validate_default_context(sys, &ctx) {
        return Ok(OwedClone::Waiting(why));
    }
    if let Some(why) = inputs.local_blocker() {
        // The full vault fence is current, but the capture never admitted local
        // repository work. Preserve holds and unfinished dispositions; never promote
        // a later local observation through this blocked capture.
        if matches!(&value, OwedClone::Due(_) | OwedClone::Verified { .. }) {
            return Ok(OwedClone::Waiting(why.into()));
        }
    } else if !inputs.is_current_now(sys)? {
        return Ok(syncing());
    }
    Ok(value)
}

fn owed_clone_with(
    sys: &dyn System,
    state: &Path,
    now: &Holds,
    inputs: Option<&crate::init_github::InitialRepositoryInputs>,
) -> Result<OwedClone, String> {
    let dir = converge_state(state);
    let record = match aterm_uds::private_file::read_private(&dir, CLONE_OWED, 4096) {
        Ok(record) => String::from_utf8_lossy(&record).into_owned(),
        Err(aterm_uds::private_file::PrivateFileError::Missing) => {
            return automatic_clone(sys, now, inputs);
        }
        Err(why) => return Err(format!("{}: {why}", dir.join(CLONE_OWED).display())),
    };
    let ctx = repository_context(sys)?;
    let Some(read) = crate::init_github::token_read(sys, &ctx)? else {
        return Ok(OwedClone::Off(
            "off: init.toml's [github] enabled = false".into(),
        ));
    };
    // A Stop, or records a newer aterm saved, hold the clone and keep it owed: Set up now goes
    // on, the clone with it. Only GitHub repositories left as they are since the run began
    // forgets it, as turning the step off does.
    if now.stopped.is_some() || now.newer {
        return Ok(OwedClone::Waiting(now.why(Setting::Repositories)));
    }
    if now.later(&stamped_holds(&record), Setting::Repositories)
        && let Some(line) = now.line(&[Setting::Repositories], false)
    {
        return Ok(OwedClone::Held(line));
    }
    // The default toolset provisions managed Git independently of this pass. A ready managed
    // Git is preferred; developer tools or the distribution's Git are usable fallbacks. Keep
    // the clone owed while none is ready, then retry without another vault change.
    if crate::items::github::installed_git(sys).is_none() {
        return Ok(OwedClone::Waiting(if sys.macos() {
            "aterm init's GitHub step waits for Git to finish installing automatically; \
             Apple's Command Line Tools or Xcode also provide Git"
                .into()
        } else {
            format!(
                "aterm init's GitHub step waits for git ({}), which this system's package \
                 manager installs",
                crate::items::github::SYSTEM_GIT
            )
        }));
    }
    if let Some((how, _)) = record
        .lines()
        .zip(record.lines().skip(1))
        .find(|(_, mark)| mark.strip_prefix(PERSON_MARK) == Some(read.mark.as_str()))
    {
        if crate::init_github::uses_vault_inventory(sys, &ctx)? {
            // Old records describe only the source, not what blocked the checkout. Re-probe
            // the default inventory locally, without applying or enumerating GitHub, so a
            // corrected worktree can resume even with an untyped pre-upgrade Person mark.
            // Explicit profiles keep their auth cache and never repeat its network probe.
            let rows = match inputs {
                Some(inputs) => crate::init_github::run_initial_repositories_captured(
                    sys,
                    &ctx,
                    Mode::Plan,
                    aterm_secrets::converge::RepositoryWork::Repair,
                    inputs.clone(),
                    || Ok(true),
                )?,
                None => crate::init_github::run_initial_repositories(sys, &ctx, Mode::Plan)?,
            };
            for row in rows
                .iter()
                .filter(|row| !crate::init_github::keychain_row(row))
            {
                let Some(crate::init_observe::Reading::Repositories {
                    present,
                    count,
                    signed_in: true,
                }) = crate::init_observe::Reading::of(row)
                else {
                    continue;
                };
                match row.outcome {
                    Outcome::Ok if present == count => {
                        return Ok(OwedClone::Verified {
                            present,
                            total: count,
                        });
                    }
                    Outcome::Planned if present < count => {
                        return Ok(OwedClone::Due(format!(
                            "local checkout work now allows repository setup to finish ({present}/{count} ready)"
                        )));
                    }
                    _ => {}
                }
            }
        }
        return Ok(OwedClone::Person(how.to_owned()));
    }
    if let Some(why) = read.unusable {
        return Ok(OwedClone::Waiting(format!(
            "aterm init's GitHub step waits for a token it can use: {why}"
        )));
    }
    Ok(if read.landed {
        OwedClone::Due(
            "aterm init's GitHub step waited for the vault, whose GitHub token is here now".into(),
        )
    } else {
        OwedClone::Waiting(
            "aterm init's GitHub step waits for the vault's GitHub token to land on this Mac"
                .into(),
        )
    })
}

/// Repository work needs this process's own account, without workstation or HID facts.
fn repository_context(sys: &dyn System) -> Result<Ctx, String> {
    let identity = sys.identity()?;
    if identity.user.uid != sys.euid() || sys.euid() == 0 {
        return Err("repository setup belongs to this login alone".into());
    }
    Ok(Ctx {
        facts: HostFacts::default(),
        users: vec![identity.user],
        console: None,
        host_name: String::new(),
        cache_dir: PathBuf::new(),
        disrupt: false,
    })
}

/// A vault member needs no earlier init debt to acquire its repositories. The subscriber
/// reads the private inventory on every pass, so newly delivered entries and missing checkouts
/// are reconciled too. A person's holds still win, and this path never inventories GitHub.
fn automatic_clone(
    sys: &dyn System,
    now: &Holds,
    inputs: Option<&crate::init_github::InitialRepositoryInputs>,
) -> Result<OwedClone, String> {
    let nothing = || OwedClone::Nothing("aterm init owes no clone".into());
    let Ok(ctx) = repository_context(sys) else {
        return Ok(nothing());
    };
    let token = Path::new(&ctx.users[0].home).join(crate::init_github::TOKEN_FILE);
    if sys
        .stat(&token)
        .is_err_and(|why| why.kind() == std::io::ErrorKind::NotFound)
    {
        return Ok(nothing());
    }
    if now.holds(Setting::Repositories) {
        return Ok(OwedClone::Held(now.why(Setting::Repositories)));
    }
    let Some(read) = crate::init_github::token_read(sys, &ctx)? else {
        return Ok(OwedClone::Off(
            "off: init.toml's [github] enabled = false".into(),
        ));
    };
    if let Some(why) = read.unusable {
        return Ok(OwedClone::Waiting(why));
    }
    if !read.landed {
        return Ok(OwedClone::Waiting(
            "waiting for the vault's GitHub credential".into(),
        ));
    }
    if crate::items::github::installed_git(sys).is_none() {
        return Ok(OwedClone::Waiting(
            "waiting for an installed git to clone the vault's repositories".into(),
        ));
    }
    let counted = match inputs {
        Some(inputs) => crate::init_github::automatic_inventory_counts_captured(sys, &ctx, inputs),
        None => crate::init_github::automatic_inventory_counts(sys, &ctx),
    };
    match counted {
        Ok(Some((present, total))) if present == total => {
            Ok(OwedClone::Verified { present, total })
        }
        Ok(Some((present, total))) => Ok(OwedClone::Due(format!(
            "{present}/{total} repositories from the private vault are present; fleet setup clones the rest"
        ))),
        Ok(None) => Ok(nothing()),
        Err(why) => Ok(OwedClone::Waiting(why)),
    }
}

/// Forget the GitHub step an `aterm init` owes, under the account's state directory `state`:
/// its step was turned off (init.toml's `[github] enabled = false`, or GitHub repositories left
/// as they are in System State), so turning it on later clones nothing by itself.
///
/// # Errors
/// The record cannot be removed.
pub fn forget_owed_clone(state: &Path) -> Result<(), String> {
    let dir = converge_state(state);
    aterm_uds::private_file::remove_private(&dir, CLONE_OWED)
        .map(|_| ())
        .map_err(|why| format!("{}: {why}", dir.join(CLONE_OWED).display()))
}

/// `sync.clone`'s apply: the GitHub step an `aterm init` owes, run once as init runs it, in any
/// shell of the account; its record under `state` goes once every repository row is in place,
/// and stays for the next pass otherwise. The keychain row runs with them but is no repository
/// row: its retry is `sync.github`'s. A repository row only a person can finish marks the record
/// with the step's mark now ([`OwedClone::Person`]), so no pass runs the step again until the
/// token, init.toml or git's presence changes, or the person's own `aterm init` owes it afresh
/// ([`note_owed`]).
///
/// # Errors
/// A repository row of the step did not end in place (its words), or the step could not run.
pub fn clone_owed(
    sys: &dyn System,
    state: &Path,
    work: aterm_secrets::converge::RepositoryWork,
) -> Result<String, String> {
    clone_owed_in_pass(sys, state, work, None)
}

/// Reuse the enclosing converge pass's alignment lease; standalone callers own
/// their lease lazily at the default native effect boundary.
/// # Errors
/// As [`clone_owed`], including a changed source or an unfulfilled shared refresh.
pub fn clone_owed_in_pass(
    sys: &dyn System,
    state: &Path,
    work: aterm_secrets::converge::RepositoryWork,
    execution: Option<crate::sync::repository_refresh::Execution>,
) -> Result<String, String> {
    let ctx = repository_context(sys)?;
    let items = crate::init_github::items_for_execution(sys, &ctx, None, work, execution)?;
    let bound = crate::engine::bind(
        items
            .iter()
            .map(|item| (item.as_ref(), anywhere(Class::Install))),
        sys,
        &ctx,
    );
    let mut journal = sys.journal()?;
    let rows = aterm_converge::run(&bound, Mode::Apply, journal.as_mut())
        .map_err(|why| why.to_string())?;
    settle_owed_clone(sys, state, &ctx, &rows)
}

/// Clone from inputs captured under the selected vault's complete materialization. The
/// snapshot is checked again before effects and read-back; its lock never spans network work.
///
/// # Errors
/// As [`clone_owed`], or the captured materialization changed before the operation finished.
pub fn clone_owed_captured(
    sys: &dyn System,
    state: &Path,
    work: aterm_secrets::converge::RepositoryWork,
    inputs: crate::init_github::InitialRepositoryInputs,
    current: impl Fn() -> Result<bool, String> + Send + Sync + 'static,
) -> Result<String, String> {
    let ctx = repository_context(sys)?;
    let rows = crate::init_github::run_initial_repositories_captured(
        sys,
        &ctx,
        Mode::Apply,
        work,
        inputs,
        current,
    )?;
    settle_owed_clone(sys, state, &ctx, &rows)
}

/// Record one repository attempt after the native or captured runner has returned.
fn settle_owed_clone(
    sys: &dyn System,
    state: &Path,
    ctx: &Ctx,
    rows: &[ItemReport],
) -> Result<String, String> {
    let dir = converge_state(state);
    // The repositories' rows alone: the keychain row's own retry is `sync.github`'s, and its
    // failing must not leave the mark unwritten and rerun this whole step every pass.
    let repository_rows = || {
        rows.iter()
            .filter(|row| !crate::init_github::keychain_row(row))
    };
    if let Some(row) = repository_rows().find(|row| {
        !matches!(
            crate::init_observe::standing(row),
            aterm_types::macos_setup::Standing::InPlace
                | aterm_types::macos_setup::Standing::NotNeeded
        )
    }) {
        let why = format!(
            "{}: {}",
            row.title,
            row.why.as_deref().unwrap_or(word(row.outcome))
        );
        if row.outcome == Outcome::Person
            && let Some(read) = crate::init_github::token_read(sys, ctx)?
        {
            let how = format!(
                "{why}. Only you can fix this; the vault's sync runs the step again once its \
                 GitHub token or init.toml changes, or run {} once it is fixed",
                naming(Setting::Repositories)
            );
            // The stamp of the run that owes it stays ([`HELD_MARK`]).
            let start = aterm_uds::private_file::read_private(&dir, CLONE_OWED, 4096)
                .map(|record| stamped_holds(&String::from_utf8_lossy(&record)))
                .unwrap_or(Holds::NONE);
            aterm_uds::private_file::write_private(
                &dir,
                CLONE_OWED,
                format!(
                    "{}\n{PERSON_MARK}{}\n{}\n",
                    person_line(&how),
                    read.mark,
                    held_stamp(&start)
                )
                .as_bytes(),
            )
            .map_err(|why| format!("{}: {why}", dir.join(CLONE_OWED).display()))?;
        }
        return Err(why);
    }
    aterm_uds::private_file::remove_private(&dir, CLONE_OWED)
        .map_err(|why| format!("{}: {why}", dir.join(CLONE_OWED).display()))?;
    let changed = repository_rows()
        .filter(|row| row.outcome == Outcome::Changed)
        .count();
    Ok(format!(
        "ran the GitHub step aterm init owed: {changed} of its repository rows changed, the rest \
         in place"
    ))
}

/// The longest line of what the person does that [`CLONE_OWED`] keeps, in bytes: the record is
/// read whole within 4 KiB ([`owed_clone`]), so a long row's words never make it unreadable.
const PERSON_LINE_MAX: usize = 1024;

/// `how` as [`CLONE_OWED`]'s one line of it: on one line, and cut at a character boundary to
/// [`PERSON_LINE_MAX`] bytes, an ellipsis saying so.
fn person_line(how: &str) -> String {
    let line = how.replace(['\n', '\r'], " ");
    if line.len() <= PERSON_LINE_MAX {
        return line;
    }
    let mut end = PERSON_LINE_MAX - '\u{2026}'.len_utf8();
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\u{2026}", &line[..end])
}

/// `row`, deferred in plain words ([`OWED`]) when it waits for this Mac to be let into the vault
/// (GitHub's sign-in and the clones, design §7): the vault's join finishes by itself, and what
/// waits on it runs then, so it is no step of this run's. A sign-out that found nobody at the
/// screen right before its own clicks (the person left during the other's) is deferred as the
/// sign-outs are when nobody is there as they start ([`NOBODY_AT_THE_SCREEN`]).
/// A read-only relay retains the coordinator's scoped reason rather than claiming vault work.
fn owe(mut row: ItemReport) -> ItemReport {
    if crate::init_accounts::found_nobody(&row) {
        row.outcome = Outcome::Deferred;
        row.why = Some(NOBODY_AT_THE_SCREEN.into());
        row.stood_aside = None;
        return row;
    }
    if row.outcome == Outcome::Blocked && waiting(&row) && !waits_by_itself(&row) {
        row.outcome = Outcome::Deferred;
        if row.class != Some(Class::Relay) {
            row.why = Some(OWED.into());
        }
    }
    row
}

/// [`owe`], save a row a later hold left (a Stop pressed, or GitHub repositories turned off, while
/// its batch ran: [`HeldLate`], whose words `held` are): its line is the person's words and what
/// the batch did before it stopped, and it waits on no vault, so it is not deferred as owed.
fn owe_unless_held(row: ItemReport, held: Option<&str>) -> ItemReport {
    let theirs = held.is_some_and(|words| {
        waiting(&row) && row.why.as_deref().is_some_and(|why| why.starts_with(words))
    });
    if theirs { row } else { owe(row) }
}

/// Whether `row` waits for this Mac to be let into the vault, and runs once it is ([`owe`]).
fn owed(row: &ItemReport) -> bool {
    waiting(row) && !waits_by_itself(row)
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
    /// This login's clone tuning item ([`Self::clone_tuning`]), built once.
    tuning: std::sync::OnceLock<Result<Box<dyn Item>, String>>,
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
            tuning: std::sync::OnceLock::new(),
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

    /// This login's `init.github.tuning@<user>` item ([`crate::clone_tuning`]), its record under
    /// `state` and System State's holds as `holds` reads them at each probe and apply, bound as
    /// a relay-class repair of the account's own, for bare `aterm doctor` to apply to a vault
    /// clone that differs from the git tuning its repository declares: the context and the item
    /// are built only on this call, and the first call's `state` and `holds` are the item's.
    ///
    /// # Errors
    /// This login's context cannot be read, or it resolves no one user.
    pub fn clone_tuning(
        &self,
        state: &Path,
        holds: crate::clone_tuning::HoldsNow,
    ) -> Result<Vec<Box<dyn aterm_converge::Item + '_>>, String> {
        let ctx = self.ctx().as_ref().map_err(Clone::clone)?;
        let item = self
            .tuning
            .get_or_init(|| crate::clone_tuning::item(ctx, state, holds))
            .as_ref()
            .map_err(Clone::clone)?;
        Ok(crate::engine::bind(
            [(item.as_ref(), crate::clone_tuning::place())],
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
    /// Read only when this run's selected local keys consume the FDA reading.
    full_disk_access: &'a dyn Fn() -> Option<bool>,
    targo: Option<crate::init_develop::Targo>,
    cwd: Option<&'a Path>,
    /// This Mac's setup records, held over the Desktop lane of a pass that changes preferences.
    records: &'a dyn Records,
    /// Hears each row as its item finishes, from whichever lane ran it ([`run_layers`]).
    on_row: &'a (dyn Fn(&ItemReport) + Sync),
    /// Whether a person is at this Mac's screen now ([`Host::present`]), read again as the
    /// sign-outs start in a run with no terminal.
    present: &'a (dyn Fn() -> bool + Sync),
    /// Hears each line of what the person's choices held, once the run is over, as the row
    /// `--json` carries ([`held_row`]): the groups System State holds ([`Holds::line`]), and the
    /// keys they changed since aterm set them ([`Records::kept`]).
    on_held: &'a dyn Fn(ItemReport),
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
            Picked::ALL,
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
            full_disk_access: &|| sys.full_disk_access(),
            targo: None,
            cwd: None,
            records,
            on_row: &|_| {},
            present: &|| true,
            on_held: &|_| {},
        },
        &mut crate::engine::NoJournal,
        &mut Laps::start(),
    )
    .map_err(|refused| match refused {
        Refused::Usage(why) | Refused::Failed(why) => why,
    })
}

/// Why the sign-outs wait, read again as they start, on every run: nobody is at this Mac's
/// screen ([`Front::present`]), with a terminal or without one (the clones before them can take
/// half an hour, and the person who started the run may have gone).
const NOBODY_AT_THE_SCREEN: &str = "waits for a person at this Mac's screen: the sign-outs, \
                                    which nothing undoes, run only while someone is there, and \
                                    nobody was as they started; the next aterm init runs them";

/// Why `aterm init --sign-out` with no terminal is refused while nobody is at this Mac's screen:
/// the sign-outs are a person stage (design §6.4), so the flag runs only for a person there.
const SIGN_OUT_NEEDS_A_PERSON: &str = "--sign-out runs with no terminal only while a person is \
                                       at this Mac's screen, and nobody is there now (no input \
                                       within the last minute, or the screen is locked): run it \
                                       there, or leave it out";

/// Whether the Desktop-lane item `id` is left as it is because the person's choices in System
/// State, read as the records are held (`holds`), hold every row it was built for (`rows`,
/// [`Picked::rows`]). A group the run names, by a flag (`named`) or by its `--only` words
/// (`only`), runs over the holds already on record when the run began (`start`), as [`picked`]
/// runs it, but never over a later one ([`Holds::later`]): a Stop pressed, or a group turned off,
/// after init started holds the items not yet run, named or not. An item built for no row is
/// held only by a Stop (or records a newer aterm saved), and for a run whose `--only` words name
/// it only by one that came later.
fn leaves_held(
    start: &Holds,
    holds: &Holds,
    named: Choice,
    only: &[String],
    rows: &[(String, Setting)],
    id: &str,
) -> bool {
    let names = |setting: Setting| !only.is_empty() || named.names(setting);
    let mut built_for = rows
        .iter()
        .filter(|(each, _)| each == id)
        .map(|(_, setting)| *setting)
        .peekable();
    if built_for.peek().is_none() {
        let stopped = |holds: &Holds| holds.newer || holds.stopped.is_some();
        return stopped(holds)
            && (only.is_empty() || (holds.newer && !start.newer) || holds.later_stop(start));
    }
    built_for
        .all(|setting| holds.holds(setting) && (!names(setting) || holds.later(start, setting)))
}

/// Whether a way back that names `aterm init` runs where this process runs ([`Holds::line`]'s
/// `at_console`): for a person at this Mac's console, never in an agent's tab, an SSH shell or a
/// run with no terminal, where `aterm init` names no group over the person's choices.
#[must_use]
pub fn ways_back_run_here() -> bool {
    ways_back_run(&Live::new())
}

/// [`ways_back_run_here`] on `sys`.
fn ways_back_run(sys: &dyn System) -> bool {
    !sys.macos() || init_presence(sys).why().is_none()
}

/// Setup does not require a controlling terminal when invoked by an agent in the
/// console owner's graphic session. Agent ancestry is not a permission boundary.
fn init_presence(sys: &dyn System) -> Presence {
    match crate::console::presence(sys) {
        Presence::AgentTab => Presence::AtConsole,
        Presence::NoTerminal if sys.agent_ancestor().is_some() => Presence::AtConsole,
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
    // Local setup commands may come from the owner or their agent. Account sign-out also runs
    // without a terminal while a person is at the screen; a different login stays outside it.
    let presence = init_presence(sys);
    let named = options.picks.named;
    let restricted = named.profile
        || (named.sign_out && !matches!(presence, Presence::AtConsole | Presence::NoTerminal));
    if restricted
        && sys.macos()
        && let Some(why) = presence.why()
    {
        return Err(Refused::Usage(format!(
            "--profile and --sign-out run in this Mac's console session \
             ({why}): run them there, or leave them out"
        )));
    }
    // With no terminal `--sign-out` is a person's pick only while a person is at the screen.
    if named.sign_out && sys.macos() && presence == Presence::NoTerminal && !(front.present)() {
        return Err(Refused::Usage(SIGN_OUT_NEEDS_A_PERSON.into()));
    }
    let only = &options.only;
    let workstation = options.workstation();
    let (ctx, keyboards, context_scope) = if workstation {
        scoped_context(sys, only, options.picks, front.full_disk_access)?
    } else {
        (
            context_for_selected(sys, WorkstationGroup::All, Selection::none())?,
            KeyboardRead::default(),
            Selection::none(),
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
    let (groups, picked) = if workstation {
        (front.groups)(
            sys,
            &ctx,
            mode,
            &Reading {
                keyboards: &keyboards,
                context_scope,
                only,
                picks: options.picks,
            },
        )?
    } else {
        (Vec::new(), Picked::NONE)
    };
    // A workstation word names an item of the workstation this run built; away from a Mac's
    // console one row stands for every one of them.
    let unknown: Vec<&str> = options
        .station
        .iter()
        .filter(|word| {
            (ctx.console.is_some() || !sys.macos())
                && !picked
                    .relayed
                    .as_ref()
                    .is_some_and(|selection| selection.words.contains(word))
                && !groups.iter().any(|(items, place)| {
                    items.iter().any(|item| {
                        aterm_converge::selects(word, &item.id(), item.family(), Some(place.layer))
                    })
                })
        })
        .map(String::as_str)
        .collect();
    if !unknown.is_empty() {
        // A word may name a group the person's choices hold, which a run away from the console
        // never builds ([`picked`]): say so in their words.
        let kept: Vec<Setting> = Setting::ALL
            .into_iter()
            .filter(|setting| picked.kept.contains(*setting))
            .collect();
        return Err(Refused::Usage(match picked.start.line(&kept, false) {
            Some(line) => format!(
                "--only {} names no item this run takes: {line}",
                unknown.join(",")
            ),
            None => format!("--only {} names no item ({ONLY_WORDS})", unknown.join(",")),
        }));
    }
    // A pass that changes the preferences or signs out holds the setup records the first
    // launch holds for the same pass, over the Desktop lane's own changes alone: the base layer,
    // the vault, the clones and the Command Line Tools never wait behind a first-launch pass,
    // and System State's Stop, its switches, Restore and Set up now wait only while preferences
    // change. The records are let go once the preferences are set and held again for the
    // sign-outs, after every other lane; each time the person's choices are read again, so a
    // Stop or a group turned off while init waited holds what it covers. A whole profile the
    // lane verified is recorded for the first launch; a run that builds no preference (with no
    // terminal they wait for a person at the console) sets none, so it holds the records for its
    // sign-outs alone and rewrites neither the plan nor the window's finished stages.
    let selected = |item: &dyn Item, place: &Place| {
        only.is_empty()
            || only.iter().any(|word| {
                aterm_converge::selects(word, &item.id(), item.family(), Some(place.layer))
            })
    };
    let holds_for = |sign_outs: bool| {
        mode == Mode::Apply
            && groups.iter().any(|(items, place)| {
                place.class == Class::Preference
                    && items.iter().any(|item| {
                        signs_out(&item.id()) == sign_outs && selected(item.as_ref(), place)
                    })
            })
    };
    let (holds_preferences, holds_sign_outs) = (holds_for(false), holds_for(true));
    let pass = if picked.whole() && only.is_empty() && holds_preferences {
        Pass::Whole {
            clones: picked.runs.contains(Setting::Repositories),
        }
    } else {
        Pass::Part
    };
    let records = front.records;
    let flags_run = ways_back_run(sys);
    // The settings the person's choices held by the time the records were held, past the ones
    // the run's preparation already left ([`Picked::kept`]), and the holds that said so.
    let late: Mutex<(Selection, Option<Holds>)> = Mutex::new((Selection::none(), None));
    let leaves = |now: &Holds, id: &str| {
        leaves_held(
            &picked.start,
            now,
            options.picks.named,
            only,
            &picked.rows,
            id,
        )
    };
    let note_left = |now: &Holds, ids: &mut dyn Iterator<Item = String>| {
        let mut late = late.lock().unwrap_or_else(PoisonError::into_inner);
        for id in ids {
            for (_, setting) in picked.rows.iter().filter(|(each, _)| *each == id) {
                late.0.set(*setting, true);
            }
        }
        late.1 = Some(now.clone());
    };
    let left_of = |now: &Holds, sign_outs: bool| {
        groups
            .iter()
            .filter(|(_, place)| place.class == Class::Preference)
            .flat_map(|(items, _)| items.iter().map(|item| item.id()))
            .filter(|id| signs_out(id) == sign_outs && leaves(now, id))
            .collect::<Vec<String>>()
    };
    // A GitHub clone, which may start half an hour in, reads the person's choices again as it
    // starts: a Stop pressed, or the group turned off, since the run began holds it, as it holds
    // the Desktop lane's items (`--clone` runs over a hold on record when the run began alone).
    let held_late = |id: &str| {
        if mode != Mode::Apply || !aterm_converge::selects(GITHUB.id, id, "", None) {
            return None;
        }
        let now = records.holds();
        if !now.later(&picked.start, Setting::Repositories) {
            return None;
        }
        let why = now.why(Setting::Repositories);
        let mut late = late.lock().unwrap_or_else(PoisonError::into_inner);
        late.0.set(Setting::Repositories, true);
        late.1 = Some(now);
        Some(why)
    };
    // A whole profile the lane verified is recorded for the first launch, under the hold.
    // It keeps the hold unless it recorded the whole profile, which lets the hold go. A pass
    // that could not read which keys the person changed set no preference (each row reads not
    // applicable, [`KEPT_UNREAD`]), so it records nothing: the window's run sets them.
    let finish = |taken: Box<dyn Held>, rows: &[ItemReport], last: bool, unread: bool| {
        if !pass.whole() || unread {
            return Ok(Some(taken));
        }
        let verified = verified_mac_settings(&ctx, &groups, rows);
        if !verified.is_empty() {
            taken.checkpoint(verified)?;
        }
        if last
            && verified == Selection::all().workstation()
            && rows
                .iter()
                .filter(|row| {
                    row.layer == Some(Layer::Workstation) && row.class == Some(Class::Preference)
                })
                .all(|row| counts(row, Action::Apply))
        {
            taken.verified()?;
            return Ok(None);
        }
        Ok::<_, String>(Some(taken))
    };
    // The keys the person changed since aterm set them stay theirs ([`Records::kept`]), and
    // the words their rows end with; with no record of them readable, every one does.
    let read_kept = || {
        let kept = records.kept();
        let why = if kept.is_ok() {
            KEPT_BY_YOU
        } else {
            KEPT_UNREAD
        };
        (kept, why)
    };
    let desktop: &Around<'_> = &|stages| {
        let said = |line: &str| eprintln!("aterm init: {line}");
        let wait: Option<&dyn Fn(&str)> = (!options.no_wait).then_some(&said);
        let mut held: Option<Box<dyn Held>> = None;
        // Why the records were not held: another setup held them past the wait, or at once.
        let mut busy: Option<String> = None;
        // The record of the keys the person changed could not be read: no preference was set.
        let mut unread = false;
        let mut rows = if holds_preferences {
            match records.hold(pass, wait) {
                Err(Unheld::Failed(why)) => return Err(why),
                Err(Unheld::Busy(why)) => {
                    let rows = (stages.preferences)(Desk::instead(Instead::Lock(&why)))?;
                    busy = Some(why);
                    rows
                }
                Ok(taken) => {
                    let now = records.holds();
                    note_left(&now, &mut left_of(&now, false).into_iter());
                    let left = |id: &str| leaves(&now, id);
                    let (kept, kept_why) = read_kept();
                    unread = kept.is_err();
                    let keeps = |id: &str| keeps_key(&kept, id);
                    let withheld: Vec<String> = groups
                        .iter()
                        .filter(|(_, place)| {
                            place.layer == Layer::Workstation && place.class == Class::Preference
                        })
                        .flat_map(|(items, place)| items.iter().map(move |item| (item, place)))
                        .filter_map(|(item, place)| {
                            let id = item.id();
                            (item.withheld().is_some()
                                && !left(&id)
                                && !keeps(&id)
                                && selected(item.as_ref(), place))
                            .then_some(id)
                        })
                        .collect();
                    if !withheld.is_empty() {
                        taken.withheld(&withheld)?;
                    }
                    let rows = (stages.preferences)(Desk {
                        leaves: &left,
                        kept: &keeps,
                        kept_why,
                        ..Desk::ALL
                    })?;
                    if let Some(mut taken) =
                        finish(taken, &rows, stages.sign_outs.is_none(), unread)?
                    {
                        taken.release();
                        held = Some(taken);
                    }
                    rows
                }
            }
        } else {
            let (kept, kept_why) = read_kept();
            (stages.preferences)(Desk {
                kept: &|id| keeps_key(&kept, id),
                kept_why,
                ..Desk::ALL
            })?
        };
        let Some(sign_outs) = stages.sign_outs else {
            return Ok(rows);
        };
        // The sign-outs, which nothing undoes, run last of all: once every other lane is done.
        (stages.others)();
        if let Some(why) = &busy {
            rows.extend(sign_outs(Desk::instead(Instead::Lock(why)))?);
            return Ok(rows);
        }
        if !holds_sign_outs {
            let (kept, kept_why) = read_kept();
            rows.extend(sign_outs(Desk {
                kept: &|id| keeps_key(&kept, id),
                kept_why,
                ..Desk::ALL
            })?);
            return Ok(rows);
        }
        let again = match held.take() {
            Some(mut taken) => {
                let again = taken.again(wait);
                held = Some(taken);
                again
            }
            None => records.hold(pass, wait).map(|taken| held = Some(taken)),
        };
        match again {
            Err(Unheld::Failed(why)) => return Err(why),
            Err(Unheld::Busy(why)) => {
                rows.extend(sign_outs(Desk::instead(Instead::Lock(&why)))?);
                return Ok(rows);
            }
            Ok(()) => {}
        }
        let now = records.holds();
        note_left(&now, &mut left_of(&now, true).into_iter());
        // A sign-out the person undid since aterm made it (they signed back in) is theirs, as
        // a key they changed is (design §6.6): the record is read again under this hold.
        let (kept, kept_why) = read_kept();
        unread |= kept.is_err();
        rows.extend(sign_outs(Desk {
            leaves: &|id| leaves(&now, id),
            kept: &|id| keeps_key(&kept, id),
            kept_why,
            ..Desk::ALL
        })?);
        if let Some(taken) = held.take() {
            finish(taken, &rows, true, unread)?;
        }
        Ok(rows)
    };
    laps.lap("questions");
    // The words a later hold left GitHub's batch in, once one did ([`owe_unless_held`]).
    let late_words = || {
        let late = late.lock().unwrap_or_else(PoisonError::into_inner);
        late.1
            .as_ref()
            .filter(|_| late.0.contains(Setting::Repositories))
            .map(|now| now.why(Setting::Repositories))
    };
    let on_row =
        |row: &ItemReport| (front.on_row)(&owe_unless_held(row.clone(), late_words().as_deref()));
    let rows = run_layers(
        sys,
        &ctx,
        (front.base)(mode == Mode::Apply).narrowed(only),
        &groups,
        &develop.items,
        &Narrowing {
            only,
            record: options.develop.is_some() || develop.unreadable,
            relayed: picked
                .relayed
                .as_ref()
                .is_some_and(|selection| !selection.words.is_empty()),
        },
        mode,
        journal,
        laps,
        &Hooks {
            on_row: &on_row,
            desktop,
            held: &held_late,
        },
    )?;
    // The one line of what the person's choices held: the preparation's, or, where the records
    // read later held more, all of them in the words the later reading gives.
    let (left, now) = late.into_inner().unwrap_or_else(PoisonError::into_inner);
    let late_words = now
        .as_ref()
        .filter(|_| left.contains(Setting::Repositories))
        .map(|now| now.why(Setting::Repositories));
    let line = match now {
        Some(now) if !left.is_empty() => {
            let kept = left.union(picked.kept);
            let held: Vec<Setting> = Setting::ALL
                .into_iter()
                .filter(|setting| kept.contains(*setting))
                .collect();
            now.line(&held, flags_run)
        }
        _ => picked.held.clone(),
    };
    if let Some(line) = line {
        (front.on_held)(held_row(
            HELD,
            "Groups left as you set them in System State",
            line,
        ));
    }
    for line in kept_lines(&rows, &picked.rows) {
        (front.on_held)(held_row(
            CHANGED,
            "Settings you changed since aterm set them",
            line,
        ));
    }
    Ok(rows
        .into_iter()
        .map(|row| owe_unless_held(row, late_words.as_deref()))
        .collect())
}

/// The lines of the keys a run left as the person set them ([`Instead::Kept`]), by the System
/// State row each was built for (`tags`): the ones they changed since aterm set them, and the
/// ones left because the record of those cannot be read.
fn kept_lines(rows: &[ItemReport], tags: &[(String, Setting)]) -> Vec<String> {
    let names = |ends: &str| {
        let mut left = Selection::none();
        for row in rows.iter().filter(|row| {
            row.outcome == Outcome::NotApplicable
                && row.why.as_deref().is_some_and(|why| why.ends_with(ends))
        }) {
            for (_, setting) in tags.iter().filter(|(id, _)| *id == row.id) {
                left.set(*setting, true);
            }
        }
        Setting::ALL
            .into_iter()
            .filter(|setting| left.contains(*setting))
            .map(Setting::name)
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut lines = Vec::new();
    let changed = names(KEPT_BY_YOU);
    if !changed.is_empty() {
        lines.push(format!(
            "left as you changed them since aterm set them: {changed}. Set up now in System \
             State, or aterm doctor --apply, sets them again"
        ));
    }
    let unread = names(KEPT_UNREAD);
    if !unread.is_empty() {
        lines.push(format!(
            "aterm could not read which settings you changed, so it left these as they are: \
             {unread}. aterm doctor --apply sets them again"
        ));
    }
    lines
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
/// ([`counts`]). Under `--check` the vault's own approval wait maps to 77, a permission:
/// the check neither reads it as in place (0) nor as failed (1). Other typed stand-aside
/// reasons keep their status; an untyped row left to a person or the vault ([`left`]) maps to 77.
fn status(rows: &[ItemReport], action: Action) -> u8 {
    if complete(rows, action) {
        return 0;
    }
    let incomplete: Vec<&ItemReport> = rows.iter().filter(|row| !counts(row, action)).collect();
    let codes: Option<Vec<u8>> = incomplete
        .iter()
        .map(|row| match &row.stood_aside {
            _ if action == Action::Check && waits_by_itself(row) => Some(77),
            Some(aside) => Some(aside.exit_status()),
            None => (action == Action::Check && left(row)).then_some(77),
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
        if expected.as_deref().is_some_and(covered) {
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

/// Activate accepted Caps defaults on the new HID services only. The caller holds the setup
/// profile lock and checks its exact-run Stop before each item, including this process's Stop.
/// A requested service absent from the model or mapping census stays retryable; each one must
/// be observed, including services whose explicit settings are intentionally left alone.
/// `invalidate` retires an older whole-Caps reading before a setter, or when this
/// attachment-only pass cannot preserve it. It supplies no positive observation.
///
/// # Errors
/// Console identity, input observations or the local change journal cannot be resolved.
pub fn activate_keyboard_attachments(
    sys: &dyn System,
    registries: &[u64],
    kept: &dyn Fn(&str) -> bool,
    continues: &dyn Fn() -> bool,
    invalidate: &dyn Fn(),
) -> Result<bool, String> {
    struct BeforeChange<'a> {
        inner: &'a mut dyn Journal,
        invalidate: &'a dyn Fn(),
    }
    impl Journal for BeforeChange<'_> {
        fn before_change(
            &mut self,
            id: &str,
            from: &str,
            to: &str,
            revert: Option<&str>,
        ) -> Result<(), String> {
            self.inner.before_change(id, from, to, revert)?;
            (self.invalidate)();
            Ok(())
        }

        fn failed(&mut self, id: &str, why: &str) -> Result<(), String> {
            self.inner.failed(id, why)
        }
    }
    let invalidated = std::cell::Cell::new(false);
    let invalidate_once = || {
        if !invalidated.replace(true) {
            invalidate();
        }
    };
    let result = (|| {
        let mut selected = Selection::none();
        selected.set(Setting::CapsLock, true);
        let (ctx, unread) = context_reading_with(
            sys,
            WorkstationGroup::InputActivation,
            selected,
            KeyboardServices::Attachments,
        )?;
        if let Some(why) = unread.unread {
            return Err(why);
        }
        if ctx.console.is_none() {
            return Err(outside_console_session(sys));
        }
        let items = crate::init_preferences::attachment_items(&ctx, registries)?;
        let mut journal = sys.journal()?;
        let mut journal = BeforeChange {
            inner: journal.as_mut(),
            invalidate: &invalidate_once,
        };
        let mut ready = true;
        let mut covered = crate::facts::KeyboardCoverage::default();
        for item in &items {
            if !continues() {
                return Ok(false);
            }
            if kept(&item.id()) {
                let observed = item.inspect_kept(sys)?;
                if registries
                    .iter()
                    .any(|id| observed.covers(std::slice::from_ref(id)))
                {
                    invalidate_once();
                }
                covered.include(observed);
                continue;
            }
            let bound = crate::engine::bind(
                [(
                    item as &dyn Item,
                    crate::engine::Place::fleet(item.family(), aterm_converge::Remedy::None),
                )],
                sys,
                &ctx,
            );
            let rows = aterm_converge::run_with(&bound, Mode::Apply, &mut journal, &mut |row| {
                let observed = item.observed_services();
                let relevant = registries
                    .iter()
                    .any(|id| observed.covers(std::slice::from_ref(id)));
                if match row.outcome {
                    Outcome::Ok => false,
                    Outcome::NotApplicable => relevant,
                    _ => true,
                } {
                    invalidate_once();
                }
            })
            .map_err(|why| why.to_string())?;
            ready &= rows.iter().all(|row| {
                matches!(
                    row.outcome,
                    Outcome::Ok | Outcome::Changed | Outcome::NotApplicable
                )
            });
            covered.include(item.observed_services());
        }
        Ok(ready && covered.covers(registries))
    })();
    if !matches!(result, Ok(true)) {
        invalidate_once();
    }
    result
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
    /// Canonical preparation scope; manual item collections prove no complete setting.
    preparation: Option<(WorkstationGroup, Selection)>,
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
    /// The scope recorded by canonical workstation preparation. Manually assembled
    /// passes have none, so a caller cannot label a partial item set as complete.
    #[must_use]
    pub fn prepared_scope(&self) -> Option<(WorkstationGroup, Selection)> {
        self.preparation
    }

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
            preparation: None,
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
    let preparation = Some((group, selected));
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
            preparation,
        });
    }
    let (ctx, keyboards) = context_reading(sys, group, selected)?;
    if ctx.console.is_none() {
        return Err(outside_console_session(sys));
    }
    let mut selected = selected;
    if keyboards.unread.is_some() {
        selected.set(Setting::CapsLock, false);
    }
    let mut tagged = workstation_items_selected(&ctx, group, selected, &keyboards.cohorts)?;
    if let Some(why) = keyboards.unread {
        tagged.insert(
            0,
            (
                Setting::CapsLock,
                Box::new(Unbuilt {
                    id: "keyboard.caps-lock".into(),
                    family: "keyboard",
                    title: Setting::CapsLock.label(),
                    why,
                }),
            ),
        );
    }
    let (settings, items) = tagged
        .into_iter()
        .map(|(setting, item)| ((item.id(), setting), item))
        .unzip();
    Ok(Workstation {
        ctx,
        items,
        settings,
        snapshots: None,
        held: Vec::new(),
        preparation,
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
        items: crate::init_spotlight::items(ctx, crate::init_spotlight::Projects::Noted)?,
        settings: Vec::new(),
        snapshots: None,
        held: Vec::new(),
        preparation: None,
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

/// Apply the Secrets stage while the caller's admitted setup choices remain current.
/// The same check reaches each native proof publication, including a Stop during a handshake.
/// # Errors
/// The context or journal cannot be resolved, or setup cannot finish safely.
#[expect(
    clippy::too_many_arguments,
    reason = "the selected stage and its live authorization"
)]
pub fn run_initial_secrets_guarded(
    sys: &dyn System,
    ctx: &Ctx,
    ops: Arc<dyn crate::init_secrets::SecretsOps>,
    source: &str,
    replaces: crate::init_secrets::Replaces,
    password: &mut Option<aterm_secrets::SecretBytes>,
    wait: std::time::Duration,
    current: &(dyn Fn() -> Result<(), String> + Sync),
) -> Result<Vec<ItemReport>, String> {
    let mut journal = sys.journal()?;
    let vault = crate::init_secrets::selected(ctx, ops, source, wait, replaces)?;
    run_vault_item(vault, sys, ctx, password, journal.as_mut(), current)
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
    run_vault_item(vault, sys, ctx, password, journal, &|| Ok(()))
}

/// The converge pass's join of the ALab vault (`sync.peers`): [`run_initial_secrets_with`]
/// with no password, except that the subscriber its join puts in place is a repair, never a
/// person's On (`init_secrets::Vault::keeping_off`), so a turn-off recorded since the pass read
/// this Mac stays. `current` rechecks the background caller's setup authorization throughout
/// the native exchange and under the vault's state lock before publication.
///
/// # Errors
/// As [`run_initial_secrets`].
pub(crate) fn run_converge_join(
    sys: &dyn System,
    ctx: &Ctx,
    ops: Arc<dyn crate::init_secrets::SecretsOps>,
    source: &str,
    replaces: crate::init_secrets::Replaces,
    journal: &mut dyn Journal,
    current: &(dyn Fn() -> Result<(), String> + Sync),
) -> Result<Vec<ItemReport>, String> {
    let authority_ops = Arc::clone(&ops);
    let admitted = || {
        current()?;
        if authority_ops.turned_off() {
            return Err("the vault subscriber was turned off; background admission is held".into());
        }
        Ok(())
    };
    let vault =
        crate::init_secrets::selected(ctx, ops, source, std::time::Duration::ZERO, replaces)?
            .keeping_off();
    run_vault_item(vault, sys, ctx, &mut None, journal, &admitted)
}

/// Run `vault`, the Secrets stage's vault item, as an apply journaled in `journal`, holding
/// `password` for its join and handing back what the join did not spend.
fn run_vault_item(
    vault: crate::init_secrets::Vault,
    sys: &dyn System,
    ctx: &Ctx,
    password: &mut Option<aterm_secrets::SecretBytes>,
    journal: &mut dyn Journal,
    current: &(dyn Fn() -> Result<(), String> + Sync),
) -> Result<Vec<ItemReport>, String> {
    vault.hold_password(password.take());
    let held = |_: &str| current().err();
    let guarded = HeldLate {
        inner: &vault,
        held: &held,
        decided: std::sync::OnceLock::new(),
        late: std::sync::OnceLock::new(),
        said: std::sync::OnceLock::new(),
    };
    let bound = crate::engine::bind(
        [(&guarded as &dyn Item, workstation_place(Class::Install))],
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
    cohorts: &crate::facts::KeyboardCohorts,
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
            for (setting, item) in
                crate::init_preferences::selected_tagged_items(ctx, selected, cohorts)?
            {
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
                    crate::init_preferences::attached_keyboard_items(ctx, cohorts)?,
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

/// The id of GitHub's group, and the word that names each of its items ([`Setting::Repositories`]).
const GITHUB: SettingGroup = SettingGroup {
    id: "init.github",
    family: "github",
    setting: Setting::Repositories,
};

/// The identity and setting of a composed group, shared by its failure row and fact scope.
#[derive(Clone, Copy)]
struct SettingGroup {
    id: &'static str,
    family: &'static str,
    setting: Setting,
}

impl SettingGroup {
    fn build(self, items: Result<Vec<Box<dyn Item>>, String>) -> Vec<Box<dyn Item>> {
        built(self.id, self.family, self.setting.label(), items)
    }

    fn selects(self, word: &str) -> bool {
        aterm_converge::selects(word, self.id, self.family, Some(Layer::Workstation))
    }
}

/// Whether the item `id` is a Messages or FaceTime sign-out, or the row that stands for them
/// when they could not be built.
fn signs_out(id: &str) -> bool {
    id == ACCOUNTS_GROUP || crate::init_accounts::signs_out(id)
}

/// What [`converge`] read and was told before the workstation's groups are built.
struct Reading<'a> {
    /// The captured native assignments, or why the keyboard census could not be read
    /// ([`context_reading`]): only the Caps Lock rows go unread.
    keyboards: &'a KeyboardRead,
    /// Optional local facts the real selectors required, not effect authority.
    context_scope: Selection,
    /// The run's `--only` words.
    only: &'a [String],
    /// What `--profile`, `--sign-out`, `--clone` and their `--no-` forms said.
    picks: Picks,
}

/// What decides which groups a run takes besides its flags ([`picked`]), whether the Messages
/// and FaceTime sign-outs may run with no terminal, and whether the run may put a question of
/// macOS's on the screen ([`Decider`]): read from the `aterm` that runs it ([`Host`]) and from
/// System State's records.
struct Standing<'a> {
    /// Who decided this run happens ([`Host::decided`]).
    decided: Decider,
    /// Whether this is an ALab Mac ([`Host::alab`]).
    alab: &'a dyn Fn() -> bool,
    /// Whether a person is at this Mac's screen now ([`Host::present`]).
    present: &'a (dyn Fn() -> bool + Sync),
    /// What the person's own choices in System State hold ([`Records::holds`]).
    holds: Holds,
    /// A read-only check's prompt-free observation from the existing coordinator.
    workstation_observation: Option<WorkstationObservation>,
}

/// How a run prepares the workstation's groups ([`prepare_groups`], or a test's stand-in), with
/// the settings it picked.
type Prepare<'a> =
    dyn Fn(&dyn System, &Ctx, Mode, &Reading<'_>) -> Result<(Vec<Group>, Picked), String> + 'a;

/// Prepare workstation groups, reading optional profiles before probing; `vault` is
/// `--vault`'s. Default discovery waits for the authorized apply, after native admission.
/// Which settings run is this Mac's
/// scope, the flags and the person's choices in System State ([`picked`], `standing`); nothing
/// asks. Who is at this run ([`crate::console::presence`]) decides whether it may change the
/// person's own Mac: over SSH, or under another login's console, nothing of the workstation is
/// read, and only a remote shell of the console's own login joins the open ALab vault; from an
/// agent's tool call or with no terminal, the vault's join runs with no question (never the
/// person's join password, which nothing asks for) and [`groups`] leaves the preferences to
/// them, building the Messages and FaceTime sign-outs with no terminal only for a person at the
/// screen ([`Standing::present`]). The vault is read only when this run names its item. For a
/// person at the console the Permissions phase lists aterm's grants. A run aterm decided
/// ([`Decider::Aterm`]) puts no question of macOS's on the screen: its vault step asks nothing,
/// and Rust in Spotlight searches no folder of the person's. A group that cannot be built is one
/// row saying why ([`Unbuilt`]); the others run.
/// Read-only runs use the actual console presence: an agent relays the coordinator's evidence
/// for private preferences and accounts, and only a person's console run discovers projects.
fn prepare_groups(
    sys: &dyn System,
    ctx: &Ctx,
    mode: Mode,
    vault: Option<&str>,
    reading: &Reading<'_>,
    standing: &Standing<'_>,
    grants: Grants,
) -> Result<(Vec<Group>, Picked), String> {
    prepare_groups_with(sys, ctx, mode, vault, reading, standing, grants, |ctx| {
        crate::init_secrets::live(ctx)
    })
}

/// [`prepare_groups`] with the vault's port from `secrets` (a test's scripted one).
#[expect(
    clippy::too_many_arguments,
    reason = "one run's workstation inputs, each its own"
)]
fn prepare_groups_with(
    sys: &dyn System,
    ctx: &Ctx,
    mode: Mode,
    vault: Option<&str>,
    reading: &Reading<'_>,
    standing: &Standing<'_>,
    grants: Grants,
    secrets: impl FnOnce(&Ctx) -> Result<Arc<dyn crate::init_secrets::SecretsOps>, String>,
) -> Result<(Vec<Group>, Picked), String> {
    // Apply authority can include the person's agent; a read-only run must not turn that
    // authority into permission to query private preferences from an agent or headless tool.
    let console_presence = crate::console::presence(sys);
    let presence = if mode == Mode::Plan {
        console_presence
    } else {
        init_presence(sys)
    };
    let observation = (mode == Mode::Plan)
        .then(|| standing.workstation_observation.clone())
        .flatten();
    // Name command-line ways back wherever local setup commands can run, including agent tools.
    let flags_run = ways_back_run(sys);
    let mut picked = picked(
        reading.picks,
        reading.only,
        standing.alab,
        &standing.holds,
        flags_run,
    );
    let runs = picked.runs;
    if sys.macos() && presence != Presence::AtConsole && !reading.only.is_empty() {
        picked.relayed = Some(relayed_selection(
            ctx,
            reading.keyboards,
            runs,
            reading.only,
        ));
    }
    let requested = picked
        .relayed
        .as_ref()
        .map_or_else(|| runs.workstation(), |selection| selection.requested);
    let observation = observation.map(|observation| observation.for_scope(requested.workstation()));
    let decided = standing.decided;
    if !sys.macos() {
        return Ok((
            elsewhere(sys, ctx, mode, vault, reading, runs, decided, secrets)?,
            picked,
        ));
    }
    let projects = match (decided, console_presence) {
        (Decider::Person, Presence::AtConsole) => crate::init_spotlight::Projects::Noted,
        _ => crate::init_spotlight::Projects::Unread,
    };
    let none = || crate::init_secrets::Secrets {
        items: Vec::new(),
        waited_on: None,
    };
    let github = |secrets: &crate::init_secrets::Secrets| {
        if runs.contains(Setting::Repositories) {
            GITHUB.build(crate::init_github::items(
                sys,
                ctx,
                secrets.waited_on.as_deref(),
            ))
        } else {
            Vec::new()
        }
    };
    if ctx.console.is_none() {
        let mut groups = vec![away_from_console(presence, observation)];
        // A remote shell of the console's own login runs the background rows an agent's tab
        // runs (design §7): Rust in Spotlight, the open ALab vault's join and the GitHub
        // repositories, each in any shell of the account, asking nothing. Another login's
        // console, or nobody's, sets up nothing of this one.
        if presence == Presence::Remote {
            if runs.contains(Setting::Spotlight) {
                groups.push((spotlight(ctx, projects), anywhere(Class::Asked)));
            }
            let secrets = if runs.contains(Setting::Secrets) {
                vault_items(sys, ctx, mode, vault, reading, Here::Remote, secrets)?
            } else {
                none()
            };
            let github = github(&secrets);
            groups.push((secrets.items, anywhere(Class::Install)));
            if !github.is_empty() {
                groups.push((github, anywhere(Class::Install)));
            }
        }
        return Ok((groups, picked));
    }
    let person = presence == Presence::AtConsole;
    let here = match (person, decided) {
        (true, Decider::Person) => Here::Person,
        (true, Decider::Aterm) => Here::Unbidden,
        (false, _) => Here::Session,
    };
    let secrets = if runs.contains(Setting::Secrets) {
        vault_items(sys, ctx, mode, vault, reading, here, secrets)?
    } else {
        none()
    };
    let github = github(&secrets);
    // Off a terminal an apply's sign-outs run only for a person at the screen now, checked
    // again as they start. A read-only run never uses that apply authority to query another
    // app's account data from a headless tool.
    let signs_out = picked.signs_out()
        && (person
            || (mode == Mode::Apply && presence == Presence::NoTerminal && (standing.present)()));
    let (mut groups, rows) = groups(
        ctx,
        presence,
        projects,
        secrets,
        github,
        reading.keyboards,
        runs,
        reading.only,
        signs_out,
        observation,
    );
    picked.rows = rows;
    if person && runs.contains(Setting::Permissions) {
        groups.push((
            permissions(grants, picked.signs_out()),
            workstation_place(Class::Person),
        ));
    }
    Ok((groups, picked))
}

/// Rust in Spotlight's items ([`crate::init_spotlight::items`]), or the one row saying why
/// they could not be built: a background row, run wherever this login's shell is. Only a
/// person's own run at the console searches their project folders, as `projects` permits.
fn spotlight(ctx: &Ctx, projects: crate::init_spotlight::Projects) -> Vec<Box<dyn Item>> {
    built(
        crate::init_spotlight::ID,
        "spotlight",
        Setting::Spotlight.label(),
        crate::init_spotlight::items(ctx, projects),
    )
}

/// The vault's items for `here` ([`crate::init_secrets::items_at`]), which ask for no join
/// password anywhere; none when `--only` names neither the workstation nor the vault.
fn vault_items(
    sys: &dyn System,
    ctx: &Ctx,
    mode: Mode,
    vault: Option<&str>,
    reading: &Reading<'_>,
    here: Here,
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
        crate::init_secrets::items_at(sys, ctx, secrets(ctx)?, mode, vault, here)?
    } else {
        crate::init_secrets::Secrets {
            items: Vec::new(),
            waited_on: None,
        }
    })
}

/// [`workstation_place`] for a group that runs in any shell of the account: the vault's join
/// away from the console, and every group on a machine that is not a Mac.
fn anywhere(class: Class) -> Place {
    Place {
        requires: Requires::Account,
        ..workstation_place(class)
    }
}

/// The workstation layer on a machine that is not a Mac (Linux): one row per macOS group, each
/// reading "macOS only"; the vault, which asks nothing of anyone (`--vault`'s questions only of
/// a person at a terminal, an SSH session's included: nobody sits at a Linux box's console);
/// and the repositories where `runs` takes them. Each runs in any shell of the account.
#[expect(
    clippy::too_many_arguments,
    reason = "one run's workstation inputs, each its own"
)]
fn elsewhere(
    sys: &dyn System,
    ctx: &Ctx,
    mode: Mode,
    vault: Option<&str>,
    reading: &Reading<'_>,
    runs: Selection,
    decided: Decider,
    secrets: impl FnOnce(&Ctx) -> Result<Arc<dyn crate::init_secrets::SecretsOps>, String>,
) -> Result<Vec<Group>, String> {
    let person = sys.terminal() && sys.agent_ancestor().is_none();
    let here = match (person, decided) {
        (true, Decider::Person) => Here::Person,
        (true, Decider::Aterm) => Here::Unbidden,
        (false, _) => Here::Remote,
    };
    let secrets = if runs.contains(Setting::Secrets) {
        vault_items(sys, ctx, mode, vault, reading, here, secrets)?
    } else {
        crate::init_secrets::Secrets {
            items: Vec::new(),
            waited_on: None,
        }
    };
    let github = GITHUB.build(crate::init_github::items(
        sys,
        ctx,
        secrets.waited_on.as_deref(),
    ));
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
    if runs.contains(Setting::Repositories) {
        groups.push((github, anywhere(Class::Install)));
    }
    Ok(groups)
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

/// The aggregate workstation row when direct settings probes are withheld ([`AwayFromConsole`]).
const AWAY: &str = "init.workstation";

/// Selector provenance for a withheld local cohort, not permission to probe or apply it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RelayedSelection {
    words: Vec<String>,
    requested: Selection,
}

/// Match the real local item identities without running them. The same constructors
/// supply a person's pass, so device IDs, families and per-setting scope cannot drift
/// into a separate alias catalog for the aggregate row.
fn relayed_selection(
    ctx: &Ctx,
    keyboards: &KeyboardRead,
    runs: Selection,
    only: &[String],
) -> RelayedSelection {
    let (groups, rows) = local_groups(ctx, keyboards, runs.workstation());
    selection_of_groups(&groups, &rows, runs, only)
}

fn selection_of_groups(
    groups: &[Group],
    rows: &[(String, Setting)],
    runs: Selection,
    only: &[String],
) -> RelayedSelection {
    let mut selection = RelayedSelection {
        words: Vec::new(),
        requested: Selection::none(),
    };
    for word in only {
        let whole = aterm_converge::selects(word, AWAY, "init", Some(Layer::Workstation));
        let mut matched = whole;
        if whole {
            selection.requested = selection.requested.union(runs.workstation());
        }
        for (items, place) in groups {
            for item in items {
                let id = item.id();
                if aterm_converge::selects(word, &id, item.family(), Some(place.layer)) {
                    matched = true;
                    for (_, setting) in rows.iter().filter(|(each, _)| *each == id) {
                        selection.requested.set(*setting, true);
                    }
                }
            }
        }
        if matched && !selection.words.contains(word) {
            selection.words.push(word.clone());
        }
    }
    selection
}

/// Project the same item identities before optional local facts are collected. Selectors
/// unresolved here, including model-specific keyboard ids not discovered yet, retain full facts;
/// the normal built-item validation still decides whether those words name anything.
fn context_selection(sys: &dyn System, ctx: &Ctx, only: &[String], picks: Picks) -> Selection {
    if only.is_empty() {
        return Choice::ALL.without(picks.left).selection();
    }
    let runs = Selection::all();
    let (mut groups, mut rows) = local_groups(ctx, &KeyboardRead::default(), runs.workstation());
    let local = selection_of_groups(&groups, &rows, runs, only);
    if only.iter().all(|word| local.words.contains(word)) {
        return local.requested;
    }
    // Reading the local profile builds the real default or custom repository identities,
    // without probing their token, vault, checkout, helper or a preference. The group itself
    // still identifies a disabled or unread source without borrowing unrelated local facts.
    let github = GITHUB.build(crate::init_github::items(sys, ctx, None));
    rows.extend(github.iter().map(|item| (item.id(), GITHUB.setting)));
    groups.push((github, workstation_place(Class::Install)));
    let mut projected = selection_of_groups(&groups, &rows, runs, only);
    for word in only.iter().filter(|word| GITHUB.selects(word)) {
        if !projected.words.contains(word) {
            projected.words.push(word.clone());
        }
        projected.requested.set(GITHUB.setting, true);
    }
    if only.iter().all(|word| projected.words.contains(word)) {
        projected.requested
    } else {
        runs
    }
}

/// The existing coordinator's observation, without reading another app's settings or
/// authorizing any change. Missing evidence is distinct from a genuine permission boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkstationObservation {
    /// The coordinator reports completion, with these private rows currently verified as
    /// wanted. Its saved plan or a group left as is proves no unobserved value.
    Complete(Selection),
    /// Background work or an observation is still pending.
    Waiting(String),
    /// The coordinator observed a real step only a person can complete.
    Permission(String),
    /// No usable coordinator observation is available.
    Unavailable(String),
}

impl WorkstationObservation {
    fn for_scope(self, requested: Selection) -> Self {
        match self {
            Self::Complete(verified)
                if Setting::ALL
                    .into_iter()
                    .any(|setting| requested.contains(setting) && !verified.contains(setting)) =>
            {
                Self::Waiting(
                    "background setup has not verified all requested preference rows".into(),
                )
            }
            observation => observation,
        }
    }
}

/// Optional workstation inputs can raise privacy dialogs when read. Away from a person's
/// console, an apply therefore defers without reading them; a check may relay the existing
/// coordinator's observation instead.
struct AwayFromConsole {
    presence: Presence,
    observation: Option<WorkstationObservation>,
}

impl Item for AwayFromConsole {
    fn id(&self) -> String {
        AWAY.into()
    }
    fn family(&self) -> &'static str {
        "init"
    }
    fn title(&self) -> String {
        if self.observation.is_some() {
            "Workstation setup observed in the background".into()
        } else {
            "Workstation setup at this Mac's console".into()
        }
    }
    fn probe(&self, _sys: &dyn System, _ctx: &Ctx) -> Probe {
        match &self.observation {
            Some(WorkstationObservation::Complete(_)) => Probe::Ok {
                now: "background coordinator verified the requested preference rows".into(),
            },
            Some(
                WorkstationObservation::Waiting(why)
                | WorkstationObservation::Permission(why)
                | WorkstationObservation::Unavailable(why),
            ) => Probe::Unusable { why: why.clone() },
            None => Probe::Deferred { why: self.why() },
        }
    }
    fn read_aside(&self) -> Option<aterm_converge::StoodAside> {
        match &self.observation {
            Some(WorkstationObservation::Waiting(why)) => {
                Some(aterm_converge::StoodAside::Waiting(why.clone()))
            }
            Some(WorkstationObservation::Permission(why)) => {
                Some(aterm_converge::StoodAside::Permission(why.clone()))
            }
            _ => None,
        }
    }
    fn apply(&self, _sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        Err(self.why())
    }
}

impl AwayFromConsole {
    /// The presence's line; the engine's own where the console was lost after it was read.
    fn why(&self) -> String {
        self.presence
            .needs_person()
            .unwrap_or_else(|| aterm_converge::NOT_AT_CONSOLE.into())
    }
}

/// The console account's non-interactive contexts may read its existing coordinator.
fn observes_background(presence: Presence) -> bool {
    matches!(
        presence,
        Presence::AgentTab | Presence::NoTerminal | Presence::Remote
    )
}

fn observed_workstation(
    sys: &dyn System,
    action: Action,
    requested: Selection,
    check: &dyn Fn(Selection) -> WorkstationObservation,
) -> Option<WorkstationObservation> {
    (action == Action::Check
        && !requested.workstation().is_empty()
        && sys.macos()
        && observes_background(crate::console::presence(sys)))
    .then(|| check(requested.workstation()))
}

/// Read in any shell of the account rather than blocked by the engine's console gate;
/// applied by nobody, whether it defers or relays an observation.
fn away_from_console(presence: Presence, observation: Option<WorkstationObservation>) -> Group {
    let observation = observation.filter(|_| observes_background(presence));
    let class = if observation.is_some() {
        Class::Relay
    } else {
        Class::Person
    };
    let mut place = workstation_place(class);
    place.requires = Requires::Account;
    if class == Class::Relay {
        place.remedy = Remedy::None;
    }
    (
        vec![Box::new(AwayFromConsole {
            presence,
            observation,
        })],
        place,
    )
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
/// console (`presence`), the preferences `runs` takes and fseventsd's memory
/// ([`crate::init_fseventsd`], restarted only when the run's `only` words name it), with the
/// keyboard list unread (`keyboards`) the Caps Lock rows one row saying why; anywhere else the
/// preferences wait for that person ([`AwayFromConsole`]). Rust in Spotlight, a background row,
/// runs for anyone in the console's session. Then the vault, its GitHub profile (`github`,
/// already built, empty where `runs` takes none), and last the Messages and FaceTime sign-outs
/// `runs` takes, where `signs_out` lets them run ([`run_layers`] runs them after every other
/// row). With the groups, the System State row each Desktop-lane item was built for, by its id
/// ([`Picked::rows`]).
#[expect(
    clippy::too_many_arguments,
    reason = "one run's workstation composition, each part its own input"
)]
fn groups(
    ctx: &Ctx,
    presence: Presence,
    projects: crate::init_spotlight::Projects,
    secrets: crate::init_secrets::Secrets,
    github: Vec<Box<dyn Item>>,
    keyboards: &KeyboardRead,
    runs: Selection,
    only: &[String],
    signs_out: bool,
    observation: Option<WorkstationObservation>,
) -> (Vec<Group>, Vec<(String, Setting)>) {
    let person = presence == Presence::AtConsole;
    let (mut groups, mut rows) = if person {
        let mut preferences = runs;
        for setting in SIGN_OUT {
            preferences.set(setting, false);
        }
        local_groups(ctx, keyboards, preferences)
    } else {
        (vec![away_from_console(presence, observation)], Vec::new())
    };
    let place = workstation_place;
    // Background cache/toolchain maintenance searches no project folder; only the person's
    // console run may also discover and note their indexed projects.
    if runs.contains(Setting::Spotlight) {
        groups.push((spotlight(ctx, projects), place(Class::Asked)));
    }
    if person {
        let named = only.iter().any(|word| word == crate::init_fseventsd::ID);
        groups.push((
            vec![Box::new(crate::init_fseventsd::Fseventsd::new(named))],
            crate::init_fseventsd::place(),
        ));
    }
    groups.push((secrets.items, place(Class::Install)));
    if !github.is_empty() {
        groups.push((github, place(Class::Install)));
    }
    if signs_out {
        let mut accounts = Selection::none();
        for setting in SIGN_OUT {
            accounts.set(setting, runs.contains(setting));
        }
        let (account_groups, account_rows) = local_groups(ctx, keyboards, accounts);
        groups.extend(account_groups);
        rows.extend(account_rows);
    }
    (groups, rows)
}

/// Construct local preference and account items, tagged by their actual setting, without
/// probing any preference or account. Both a person's pass and the background selector
/// metadata use these same identities.
fn local_groups(
    ctx: &Ctx,
    keyboards: &KeyboardRead,
    runs: Selection,
) -> (Vec<Group>, Vec<(String, Setting)>) {
    let place = workstation_place;
    let mut groups = Vec::new();
    let mut rows: Vec<(String, Setting)> = Vec::new();
    // `items`, each tagged with every one of `settings`: a group's rows, or the one row that
    // says why it could not be built.
    let mut tagged = |settings: &[Setting], items: Vec<Box<dyn Item>>| {
        for item in &items {
            rows.extend(settings.iter().map(|setting| (item.id(), *setting)));
        }
        items
    };
    {
        let mut preferences = Selection::none();
        for setting in [
            Setting::CapsLock,
            Setting::Desktop,
            Setting::Widgets,
            Setting::Dock,
        ] {
            preferences.set(setting, runs.contains(setting));
        }
        let unread = keyboards
            .unread
            .as_deref()
            .filter(|_| preferences.contains(Setting::CapsLock));
        if unread.is_some() {
            preferences.set(Setting::CapsLock, false);
        }
        let mut items = if preferences.is_empty() {
            Vec::new()
        } else {
            match crate::init_preferences::selected_tagged_items(
                ctx,
                preferences,
                &keyboards.cohorts,
            ) {
                Ok(built) => built
                    .into_iter()
                    .map(|(setting, item)| tagged(&[setting], vec![item]).remove(0))
                    .collect(),
                Err(why) => {
                    let each: Vec<Setting> = Setting::ALL
                        .into_iter()
                        .filter(|setting| preferences.contains(*setting))
                        .collect();
                    tagged(
                        &each,
                        built(
                            "init.preferences",
                            "init",
                            "Workstation preferences",
                            Err(why),
                        ),
                    )
                }
            }
        };
        if let Some(why) = unread {
            items.insert(
                0,
                tagged(
                    &[Setting::CapsLock],
                    vec![Box::new(Unbuilt {
                        id: "keyboard.caps-lock".into(),
                        family: "keyboard",
                        title: Setting::CapsLock.label(),
                        why: why.to_owned(),
                    })],
                )
                .remove(0),
            );
        }
        if !items.is_empty() {
            groups.push((items, place(Class::Preference)));
        }
        if runs.contains(Setting::NaturalScrolling) {
            groups.push((
                tagged(
                    &[Setting::NaturalScrolling],
                    built(
                        "init.trackpad",
                        crate::init_trackpad::NATURAL,
                        Setting::NaturalScrolling.label(),
                        crate::init_trackpad::items(ctx),
                    ),
                ),
                place(Class::Preference),
            ));
        }
        if runs.contains(Setting::TrackpadScrolling) {
            groups.push((
                tagged(
                    &[Setting::TrackpadScrolling],
                    built(
                        "init.trackpad.behavior",
                        "trackpad",
                        Setting::TrackpadScrolling.label(),
                        crate::init_trackpad::behavior_items(ctx),
                    ),
                ),
                place(Class::Preference),
            ));
        }
    }
    if SIGN_OUT.into_iter().any(|setting| runs.contains(setting)) {
        let mut accounts = Selection::none();
        for setting in SIGN_OUT {
            accounts.set(setting, runs.contains(setting));
        }
        let items = match crate::init_accounts::selected_tagged_items(ctx, accounts) {
            Ok(built) => built
                .into_iter()
                .map(|(setting, item)| tagged(&[setting], vec![item]).remove(0))
                .collect(),
            Err(why) => {
                let each: Vec<Setting> = SIGN_OUT
                    .into_iter()
                    .filter(|setting| accounts.contains(*setting))
                    .collect();
                tagged(
                    &each,
                    built(
                        ACCOUNTS_GROUP,
                        "init",
                        "Sign out of Messages and FaceTime",
                        Err(why),
                    ),
                )
            }
        };
        groups.push((items, place(Class::Preference)));
    }
    (groups, rows)
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
            relayed: false,
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
    /// Keep the already-prepared aggregate when its original selectors matched.
    relayed: bool,
}

/// What a run's caller hears and wraps while its lanes go ([`run_layers`]).
struct Hooks<'a> {
    /// Hears each row as its item finishes, from whichever lane ran it.
    on_row: &'a (dyn Fn(&ItemReport) + Sync),
    /// Runs the Desktop lane's two stages, handed to it ([`Stages`]): [`converge`] holds the
    /// setup records around each ([`Records`]), so only they wait behind a first-launch pass.
    desktop: &'a Around<'a>,
    /// Why the person's choices hold the Network-lane item with this id as it starts, if they
    /// do ([`HeldLate`]): a Stop pressed, or a group turned off, after the run began.
    held: &'a (dyn Fn(&str) -> Option<String> + Sync),
}

impl Hooks<'_> {
    /// Hear nothing, and run the Desktop lane as it is.
    #[cfg(test)]
    const QUIET: Hooks<'static> = Hooks {
        on_row: &|_| {},
        desktop: &|stages| stages.plain(),
        held: &|_| None,
    };
}

/// One stage of the Desktop lane, run as `Desk` says ([`Stages`]).
type StageRun<'a> = dyn Fn(Desk<'_>) -> Result<Vec<ItemReport>, String> + 'a;

/// The Desktop lane's stages, handed to [`Hooks::desktop`] to run in order: the preferences,
/// then, once every other lane is done ([`Self::others`]), the sign-outs.
struct Stages<'s> {
    /// The preferences and the trackpad.
    preferences: &'s StageRun<'s>,
    /// The Messages and FaceTime sign-outs, which run last of all; `None` where the run has
    /// none.
    sign_outs: Option<&'s StageRun<'s>>,
    /// Returns once every other lane is done.
    others: &'s dyn Fn(),
}

#[cfg(test)]
impl Stages<'_> {
    /// Run every stage as it is, in order.
    fn plain(&self) -> Result<Vec<ItemReport>, String> {
        let mut rows = (self.preferences)(Desk::ALL)?;
        if let Some(sign_outs) = self.sign_outs {
            (self.others)();
            rows.extend(sign_outs(Desk::ALL)?);
        }
        Ok(rows)
    }
}

/// What runs the Desktop lane's stages ([`Hooks::desktop`]).
type Around<'a> = dyn Fn(&Stages<'_>) -> Result<Vec<ItemReport>, String> + Sync + 'a;

/// What one stage of the Desktop lane runs of its items ([`Stages`]), as [`converge`]'s hold
/// on System State's records decides.
#[derive(Clone, Copy)]
struct Desk<'d> {
    /// Whether the item with this id is left out: the person's choices hold the System State
    /// row it was built for by the time the records are held ([`leaves_held`]).
    leaves: &'d dyn Fn(&str) -> bool,
    /// What each item the stage runs does in place of a change, where it cannot make one now.
    instead: Option<Instead<'d>>,
    /// Whether the item with this id is a key the person changed after aterm set it (design
    /// §6.6, [`Records::kept`]): it is left as they set it, in [`Self::kept_why`]'s words.
    kept: &'d dyn Fn(&str) -> bool,
    /// Why a kept item is left as it is ([`Instead::Kept`]).
    kept_why: &'d str,
}

impl Desk<'static> {
    /// Every item, as it is.
    const ALL: Self = Self {
        leaves: &leaves_none,
        instead: None,
        kept: &leaves_none,
        kept_why: "",
    };
}

impl<'d> Desk<'d> {
    /// Every item, each doing `instead` in place of a change.
    const fn instead(instead: Instead<'d>) -> Self {
        Self {
            instead: Some(instead),
            ..Desk::ALL
        }
    }

    /// What the item with this id does in place of a change, if anything.
    fn instead_of(&self, id: &str) -> Option<Instead<'d>> {
        self.instead
            .or_else(|| (self.kept)(id).then_some(Instead::Kept(self.kept_why)))
    }
}

/// Whether `kept` ([`Records::kept`]) keeps the item with id `id` as the person set it: every
/// item, where the record could not be read.
fn keeps_key(kept: &Result<Vec<String>, String>, id: &str) -> bool {
    kept.as_ref()
        .map_or(true, |ids| ids.iter().any(|kept| kept == id))
}

/// [`Desk::leaves`] of a stage that leaves no item out, and [`Desk::kept`] of one that keeps
/// none.
const fn leaves_none(_id: &str) -> bool {
    false
}

/// How a row the person changed after aterm set it ends ([`Instead::Kept`]): `aterm init`
/// leaves it, and names the ways that set it again (design §6.6). It mirrors the words the
/// window's unattended pass uses.
const KEPT_BY_YOU: &str = "changed by you: aterm init leaves it as you set it, and Set up now in \
                           System State or aterm doctor --apply sets it again";

/// How a row ends when the record of the keys the person changed cannot be read: `aterm init`
/// sets none of them rather than one of theirs, and records none as set.
const KEPT_UNREAD: &str = "left as it is: aterm could not read which settings you changed, so \
                           it changed none of them; aterm doctor --apply sets them again";

/// What a Desktop-lane item does in place of a change it cannot make now ([`Unchanged`]).
#[derive(Clone, Copy, Debug)]
enum Instead<'d> {
    /// Stand aside for another setup's hold on System State's records, in these words: the row
    /// reads as it is and changes nothing (exit 75, "a lock").
    Lock(&'d str),
    /// Leave a key the person changed after aterm set it ([`KEPT_BY_YOU`]), in these words:
    /// the row reads as it is, and one that would change reads as left.
    Kept(&'d str),
}

/// A Desktop-lane item that cannot change anything now ([`Instead`]): it reads as it is and
/// stands aside where it would change, or waits for a person without reading at all. It
/// refreshes and relaunches nothing.
struct Unchanged<'a> {
    inner: &'a dyn Item,
    instead: Instead<'a>,
}

impl Item for Unchanged<'_> {
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
        match self.instead {
            Instead::Lock(_) => self.inner.probe(sys, ctx),
            Instead::Kept(why) => match self.inner.probe(sys, ctx) {
                Probe::Change { from, .. } => Probe::NotApplicable {
                    why: format!("{from}; {why}"),
                },
                probe => probe,
            },
        }
    }
    fn apply(&self, _sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        Err(match self.instead {
            Instead::Lock(why) | Instead::Kept(why) => why.into(),
        })
    }
    fn revert(&self) -> Option<String> {
        self.inner.revert()
    }
    fn effect(&self) -> Effect {
        self.inner.effect()
    }
    fn after(&self) -> Vec<String> {
        self.inner.after()
    }
    fn read_aside(&self) -> Option<aterm_converge::StoodAside> {
        self.inner.read_aside()
    }
    fn stands_aside(&self, _sys: &dyn System, _ctx: &Ctx) -> Option<aterm_converge::StoodAside> {
        match self.instead {
            Instead::Lock(why) => Some(aterm_converge::StoodAside::Lock(why.into())),
            Instead::Kept(_) => None,
        }
    }
}

/// A Network-lane item of a group the person's choices can hold after the run began (a GitHub
/// clone, which may start half an hour in): it reads those choices as it starts
/// ([`Hooks::held`]), and one they hold by then changes nothing and reads as left, in their
/// words. An admitted batch checks that same policy at each effect boundary; a later hold is
/// retained for the row, while ordinary read-back preserves the item's first admission. The row
/// of a batch a later hold stopped says the person's words, then what the batch said it did
/// before it stopped (a clone's hook installer, a clone that failed): nothing reads it back.
struct HeldLate<'a> {
    inner: &'a dyn Item,
    held: &'a (dyn Fn(&str) -> Option<String> + Sync),
    decided: std::sync::OnceLock<Option<String>>,
    late: std::sync::OnceLock<String>,
    /// What the batch answered when a later hold stopped it.
    said: std::sync::OnceLock<String>,
}

impl HeldLate<'_> {
    /// Why the person's choices hold this item, read once as it starts.
    fn held(&self) -> Option<&String> {
        self.late.get().or_else(|| {
            self.decided
                .get_or_init(|| (self.held)(&self.inner.id()))
                .as_ref()
        })
    }

    fn current(&self) -> Result<(), String> {
        if let Some(why) = self.held() {
            return Err(why.clone());
        }
        match (self.held)(&self.inner.id()) {
            Some(why) => Err(self.late.get_or_init(|| why).clone()),
            None => Ok(()),
        }
    }

    fn finished(&self, result: Result<(), String>) -> Result<(), String> {
        match (self.late.get(), result) {
            (Some(_), Err(said)) => {
                let _ = self.said.set(said);
                Ok(())
            }
            (Some(_), Ok(())) => Ok(()),
            (None, result) => result,
        }
    }
}

impl Item for HeldLate<'_> {
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
        match self.held() {
            Some(why) => Probe::NotApplicable { why: why.clone() },
            None => self.inner.probe(sys, ctx),
        }
    }
    fn apply(&self, sys: &dyn System, ctx: &Ctx) -> Result<(), String> {
        match self.held() {
            Some(why) => Err(why.clone()),
            None => self.finished(self.inner.apply_guarded(sys, ctx, &|| self.current())),
        }
    }
    fn preference_changes(
        &self,
        sys: &dyn System,
        ctx: &Ctx,
    ) -> Result<Vec<(Target, Read<PValue>)>, String> {
        self.inner.preference_changes(sys, ctx)
    }
    fn refresh(&self, sys: &dyn System, ctx: &Ctx) -> Option<Result<(), String>> {
        self.inner
            .refresh_guarded(sys, ctx, &|| self.current())
            .map(|result| self.finished(result))
    }
    fn refresh_changes(&self, sys: &dyn System, ctx: &Ctx) -> bool {
        self.inner.refresh_changes(sys, ctx)
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
        self.late
            .get()
            .map(|why| {
                aterm_converge::StoodAside::Waiting(match self.said.get() {
                    None => why.clone(),
                    Some(said) if said.starts_with(why.as_str()) => said.clone(),
                    Some(said) => format!("{why}; {said}"),
                })
            })
            .or_else(|| self.inner.stood_aside())
    }
    fn unfinished(&self) -> Option<crate::init_clt::Unfinished> {
        self.inner.unfinished()
    }
    fn read_aside(&self) -> Option<aterm_converge::StoodAside> {
        self.inner.read_aside()
    }
    fn withheld(&self) -> Option<String> {
        self.inner.withheld()
    }
    fn stands_aside(&self, sys: &dyn System, ctx: &Ctx) -> Option<aterm_converge::StoodAside> {
        if self.held().is_some() {
            return None;
        }
        self.inner.stands_aside(sys, ctx)
    }
}

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
/// start at once, the rest of the Network lane (the toolset, GitHub, `base.clt`, the develop layer) once
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
    // The Messages and FaceTime sign-outs run last of all, once every other lane is done: what
    // they change nothing undoes, so nothing else of the run is left to fail after them.
    let others = Latch::default();
    let (mut spotlight, mut network, mut desktop, mut later, mut last) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for (items, place) in groups {
        for item in items {
            let at = (item.as_ref(), place.clone());
            match lane(place.class) {
                Lane::Base => spotlight.push(at),
                Lane::Desktop if signs_out(&item.id()) => last.push(at),
                Lane::Desktop => desktop.push(at),
                // The vault's join starts at once; what may take the store lock waits behind
                // the gate.
                Lane::Network if item.family() == crate::init_secrets::FAMILY => network.push(at),
                Lane::Network => later.push(at),
            }
        }
    }
    // The workstation's Network-lane items each read the person's choices as they start.
    let held_late: Vec<HeldLate<'_>> = later
        .iter()
        .map(|(inner, _)| HeldLate {
            inner: *inner,
            held: hooks.held,
            decided: std::sync::OnceLock::new(),
            late: std::sync::OnceLock::new(),
            said: std::sync::OnceLock::new(),
        })
        .collect();
    // Downloaded tools and repository clones need no Apple compiler. Finish them before
    // an interactive Command Line Tools installation can wait for a Mac password.
    let gated: Vec<(&dyn Item, Place)> = toolset
        .as_ref()
        .map(|toolset| (toolset as &dyn Item, crate::init_toolset::place()))
        .into_iter()
        .chain(
            later
                .iter()
                .zip(&held_late)
                .map(|((_, place), item)| (item as &dyn Item, place.clone())),
        )
        .chain(
            clt.as_ref()
                .map(|clt| (clt as &dyn Item, crate::init_clt::place())),
        )
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
            if narrowing.relayed {
                words.push(AWAY.to_owned());
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
    // A Desktop-lane stage as its desk says: the items it leaves out dropped, and the rest each
    // doing what it says in place of a change.
    let staged = |lane: &[(&dyn Item, Place)], desk: Desk<'_>| {
        let ran: Vec<(&dyn Item, Place)> = lane
            .iter()
            .filter(|(item, _)| !(desk.leaves)(&item.id()))
            .cloned()
            .collect();
        let unchanged: Vec<Option<Unchanged<'_>>> = ran
            .iter()
            .map(|(inner, _)| {
                desk.instead_of(&inner.id()).map(|instead| Unchanged {
                    inner: *inner,
                    instead,
                })
            })
            .collect();
        run(&ran
            .iter()
            .zip(&unchanged)
            .map(|((inner, place), unchanged)| {
                (
                    unchanged.as_ref().map_or(*inner, |item| item as &dyn Item),
                    place.clone(),
                )
            })
            .collect::<Vec<_>>())
    };
    let preferences = |desk: Desk<'_>| staged(&desktop, desk);
    let sign_outs = |desk: Desk<'_>| staged(&last, desk);
    let desktop_lane = || {
        (hooks.desktop)(&Stages {
            preferences: &preferences,
            sign_outs: (!last.is_empty()).then_some(&sign_outs as &StageRun<'_>),
            others: &|| others.wait(),
        })
    };
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
        let done = Opens(&others);
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
        drop(done);
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
#[cfg(test)]
fn local_context(sys: &dyn System) -> Result<(Ctx, KeyboardRead), String> {
    context_reading(sys, WorkstationGroup::All, Selection::all())
}

/// Capture optional facts only after projecting the run's real selector identities.
fn scoped_context(
    sys: &dyn System,
    only: &[String],
    picks: Picks,
    full_disk_access: &dyn Fn() -> Option<bool>,
) -> Result<(Ctx, KeyboardRead, Selection), String> {
    let mut ctx = context_identity(sys)?;
    let selected = context_selection(sys, &ctx, only, picks);
    if ctx.console.is_some() && !crate::init_preferences::withheld_settings(selected).is_empty() {
        ctx.facts.full_disk_access = full_disk_access();
    }
    let (ctx, keyboards) = context_keyboards(
        sys,
        ctx,
        WorkstationGroup::All,
        selected,
        KeyboardServices::Native,
    )?;
    Ok((ctx, keyboards, selected))
}

fn context_for_selected(
    sys: &dyn System,
    group: WorkstationGroup,
    selected: Selection,
) -> Result<Ctx, String> {
    let (ctx, keyboards) = context_reading(sys, group, selected)?;
    keyboards.unread.map_or(Ok(ctx), Err)
}

/// [`context_for_selected`]'s reading, with why the keyboard list (`hidutil list`) could not
/// be read, if it could not, apart: the context then lists no keyboard.
fn context_reading(
    sys: &dyn System,
    group: WorkstationGroup,
    selected: Selection,
) -> Result<(Ctx, KeyboardRead), String> {
    context_reading_with(sys, group, selected, KeyboardServices::Native)
}

/// A normal read covers all native services; attachment activation owns only its captured IDs.
enum KeyboardServices {
    Native,
    Attachments,
}

#[derive(Debug, Default)]
struct KeyboardRead {
    unread: Option<String>,
    cohorts: crate::facts::KeyboardCohorts,
}

impl KeyboardRead {
    #[cfg(test)]
    const EMPTY: Self = Self {
        unread: None,
        cohorts: crate::facts::KeyboardCohorts::EMPTY,
    };
}

fn context_reading_with(
    sys: &dyn System,
    group: WorkstationGroup,
    selected: Selection,
    services: KeyboardServices,
) -> Result<(Ctx, KeyboardRead), String> {
    let ctx = context_identity(sys)?;
    context_keyboards(sys, ctx, group, selected, services)
}

/// The process's account and console identity; no optional HID or permission probe.
fn context_identity(sys: &dyn System) -> Result<Ctx, String> {
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
    Ok(Ctx {
        facts,
        users: vec![user.clone()],
        console: at_console.then_some(user),
        host_name: String::new(),
        cache_dir: PathBuf::new(),
        disrupt: true,
    })
}

fn context_keyboards(
    sys: &dyn System,
    mut ctx: Ctx,
    group: WorkstationGroup,
    selected: Selection,
    services: KeyboardServices,
) -> Result<(Ctx, KeyboardRead), String> {
    if ctx.console.is_none() {
        return Ok((ctx, KeyboardRead::default()));
    }
    let keyboards =
        if group == WorkstationGroup::Accounts || !selected.contains(Setting::CapsLock) {
            Ok((Vec::new(), crate::facts::KeyboardCohorts::default()))
        } else {
            let command = crate::facts::hidutil_keyboards();
            sys.run_complete_stdout(&command)
                .map_err(|e| format!("{}: {e}", command.display()))
                .and_then(|keyboard_list| {
                    if keyboard_list.success() {
                        crate::facts::parse_hidutil_keyboards_complete(&keyboard_list.out())
                            .and_then(|census| {
                                let native = match services {
                                    KeyboardServices::Native => Some(sys.keyboard_service_ids()?),
                                    KeyboardServices::Attachments => None,
                                };
                                census.into_parts(native.as_deref())
                            })
                    } else {
                        Err(format!(
                            "{}: {}",
                            command.display(),
                            keyboard_list.describe()
                        ))
                    }
                })
        };
    let (keyboards, read) = match keyboards {
        Ok((keyboards, cohorts)) => (
            keyboards,
            KeyboardRead {
                unread: None,
                cohorts,
            },
        ),
        Err(why) => (
            Vec::new(),
            KeyboardRead {
                unread: Some(why),
                ..KeyboardRead::default()
            },
        ),
    };
    ctx.facts.keyboards = keyboards;
    Ok((ctx, read))
}

fn complete(rows: &[ItemReport], action: Action) -> bool {
    !rows.is_empty() && rows.iter().all(|row| counts(row, action))
}

/// Whether `row` counts as done for `action`. For a run that is not `--check`, a vault request
/// its subscriber will finish after approval ([`waits_by_itself`]), a grant only the person switches on
/// ([`is_grant`]), a row deferred to a person ([`waits_for_a_person`]), one that runs once
/// this Mac is let into the vault ([`owed`]), and Apple's Command Line Tools deferred to an
/// install under way, to Apple's installer window open for them or to a person at this Mac's
/// console ([`crate::init_clt::deferred`]), and the vault's look deferred until the LAN broker
/// answers ([`crate::init_secrets::waits_for_lan_broker`]) are not this run's to finish, so an apply or a plan completes with them (exit 0), naming them;
/// `--check` still reads them as not verified.
fn counts(row: &ItemReport, action: Action) -> bool {
    matches!(
        row.outcome,
        Outcome::Ok | Outcome::Changed | Outcome::NotApplicable
    ) || (action == Action::Plan && row.outcome == Outcome::Planned)
        || (action != Action::Check && (waits_by_itself(row) || left(row)))
}

/// Whether `row` is not a run's to finish ([`counts`]): a grant only the person switches on
/// ([`is_grant`]), a row deferred to a person ([`waits_for_a_person`]), one owed once this Mac
/// is let into the vault ([`owed`]), the Command Line Tools deferred
/// ([`crate::init_clt::deferred`]), or the vault's look deferred until the LAN broker, which
/// setup's one elevation installs, answers ([`crate::init_secrets::waits_for_lan_broker`]).
/// `--check` reads such a row as not in place and exits 77 for it ([`status`]): what is missing
/// waits on a person's (or the vault's, or the elevation's) permission, which is neither done
/// nor a failure.
fn left(row: &ItemReport) -> bool {
    is_grant(row)
        || waits_for_a_person(row)
        || owed(row)
        || crate::init_clt::deferred(row)
        || (row.outcome == Outcome::Deferred && crate::init_secrets::waits_for_lan_broker(row))
}

/// Whether `row` is deferred to a person: the stand-in for the workstation rows a person at
/// this Mac's console makes ([`AwayFromConsole`]), or a sign-out that found nobody at the screen
/// as it started ([`NOBODY_AT_THE_SCREEN`]).
fn waits_for_a_person(row: &ItemReport) -> bool {
    row.outcome == Outcome::Deferred
        && (row.id == AWAY
            || (signs_out(&row.id) && row.why.as_deref() == Some(NOBODY_AT_THE_SCREEN)))
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
        family: &'static str,
        probes: Mutex<Vec<Probe>>,
    }

    impl Listed {
        fn boxed(id: impl Into<String>, probes: Vec<Probe>) -> Box<dyn Item> {
            Self::of("test", id, probes)
        }

        /// [`Self::boxed`], of `family`.
        fn of(family: &'static str, id: impl Into<String>, probes: Vec<Probe>) -> Box<dyn Item> {
            Box::new(Self {
                id: id.into(),
                family,
                probes: Mutex::new(probes),
            })
        }
    }

    impl Item for Listed {
        fn id(&self) -> String {
            self.id.clone()
        }
        fn family(&self) -> &'static str {
            self.family
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

    /// [`prepare_groups_with`] for an apply with no flag, no `--only` and no grant, on an ALab
    /// Mac ([`alab_standing`]), whose vault look (a scripted one: a test never reads the
    /// network) finds no vault.
    fn prepared(sys: &dyn System, ctx: &Ctx) -> Vec<Group> {
        let vault = crate::init_secrets::tests::Scripted::new(vec![Ok(
            crate::init_secrets::tests::unconfigured(),
        )]);
        prepare_groups_with(
            sys,
            ctx,
            Mode::Apply,
            None,
            &reading(&[]),
            &alab_standing(),
            no_grants,
            move |_| Ok(vault as Arc<dyn crate::init_secrets::SecretsOps>),
        )
        .unwrap()
        .0
    }

    /// A run with no flag and the `--only` words `only`.
    fn reading(only: &[String]) -> Reading<'_> {
        static KEYBOARDS: KeyboardRead = KeyboardRead::EMPTY;
        Reading {
            keyboards: &KEYBOARDS,
            context_scope: Selection::all(),
            only,
            picks: Picks::default(),
        }
    }

    /// An ALab Mac with a person at its screen, nothing held in System State.
    fn alab_standing() -> Standing<'static> {
        Standing {
            decided: Decider::Person,
            alab: &|| true,
            present: &|| true,
            holds: Holds::default(),
            workstation_observation: None,
        }
    }

    /// A Mac that is not ALab's, with a person at its screen, nothing held in System State.
    fn personal_standing() -> Standing<'static> {
        Standing {
            decided: Decider::Person,
            alab: &|| false,
            present: &|| true,
            holds: Holds::default(),
            workstation_observation: None,
        }
    }

    /// What `aterm init` answers on standard output fails the run only when the answer is lost:
    /// a reader gone ends the answer and keeps the run's own status, while a full disk under a
    /// redirect (or EIO) is said on stderr, once, and exits 1, so `aterm init --json > file`
    /// never reads a short report as a success; a stderr that fails too (`2>&1` onto the same
    /// full disk) still returns exit 1, never unwinding, so the walk after the summary is still
    /// handed over. CONTROL: an answer written in full is the run's, and says nothing on stderr.
    #[test]
    fn a_lost_answer_fails_the_run_and_a_gone_reader_does_not() {
        use std::io::ErrorKind;
        struct Refusing(ErrorKind);
        impl std::io::Write for Refusing {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(self.0.into())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let said = std::cell::RefCell::new(Vec::<String>::new());
        let say = |line: &str| {
            said.borrow_mut().push(line.to_owned());
            Ok(())
        };
        let mut written = Vec::new();
        assert_eq!(answer_to(&mut written, "{}\n", say), Ok(()));
        assert_eq!(written, b"{}\n");
        assert_eq!(
            answer_to(&mut Refusing(ErrorKind::BrokenPipe), "{}\n", say),
            Ok(())
        );
        assert!(said.borrow().is_empty(), "{:?}", said.borrow());
        for kind in [ErrorKind::StorageFull, ErrorKind::Other] {
            said.borrow_mut().clear();
            assert_eq!(
                answer_to(&mut Refusing(kind), "{}\n", say),
                Err(ExitCode::FAILURE),
                "{kind:?}"
            );
            assert_eq!(said.borrow().len(), 1, "{kind:?}: {:?}", said.borrow());
        }
        assert_eq!(
            answer_to(&mut Refusing(ErrorKind::StorageFull), "{}\n", |_| {
                Err(ErrorKind::StorageFull.into())
            }),
            Err(ExitCode::FAILURE),
            "stderr lost too"
        );
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
        let items = workstation_items_selected(
            &context,
            WorkstationGroup::All,
            Selection::all(),
            &crate::facts::KeyboardCohorts::EMPTY,
        )
        .unwrap();
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
        let items = workstation_items_selected(
            &context,
            WorkstationGroup::Preferences,
            Selection::all(),
            &crate::facts::KeyboardCohorts::EMPTY,
        )
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
            let items = workstation_items_selected(
                &context,
                WorkstationGroup::All,
                selected,
                &crate::facts::KeyboardCohorts::EMPTY,
            )
            .unwrap();
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
            preparation: None,
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
            preparation: None,
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
            preparation: None,
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
            preparation: None,
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
            preparation: None,
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
            crate::init_spotlight::Projects::Noted,
            crate::init_secrets::Secrets {
                items: Vec::new(),
                waited_on: None,
            },
            crate::init_github::items(&sys, &alice, None).unwrap(),
            &KeyboardRead::EMPTY,
            Selection::all(),
            &[],
            true,
            None,
        )
        .0;
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
            3,
            "the keychain block, the GitHub sign-in and checkouts: {github:?}"
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
                preparation: None,
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
            preparation: None,
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
            preparation: None,
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
        fn hold(&self, _pass: Pass, _wait: Option<&dyn Fn(&str)>) -> Result<Box<dyn Held>, Unheld> {
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
            decided: Decider::Person,
            base: &|_| panic!("--converge composed init's base layer"),
            full_disk_access: &|| panic!("--converge measured Full Disk Access"),
            records: &NoSetup,
            grants: grants_unread,
            workstation_check: &|_| panic!("--converge read workstation observations"),
            walk: &|_| panic!("--converge walked the grants"),
            converge: ports,
            alab: &|| panic!("--converge read the ALab scope"),
            present: || panic!("--converge read who is at the screen"),
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
            [crate::sync::Trigger::Init],
            "the runner lowers its QoS as it starts, as its trigger's pass"
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

    /// Pending admitted work has its own non-failure transport and an incomplete JSON report.
    /// A locally ready Git resumes the same clone; an explicit group hold is settled instead.
    #[test]
    fn converge_transports_unfinished_repository_work_without_claiming_completion() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let ports = crate::sync::tests::Inert::new(scratch.path());
        ports.clone_is(OwedClone::Waiting("Git is installing".into()));
        assert_eq!(
            converge_run(&["--converge", "--trigger", "subscriber", "--json"], &ports),
            exit(aterm_secrets::converge::UNFINISHED)
        );
        let pass = crate::sync::converge(
            &ports,
            crate::sync::Trigger::Subscriber,
            Mode::Plan,
            aterm_secrets::converge::RepositoryWork::Refresh,
            &mut |_| {},
        )
        .unwrap();
        let json = report_json(Action::Check, pass.complete(), pass.rows, &Laps::start()).unwrap();
        let report: aterm_json::Value = aterm_json::from_str(&json).unwrap();
        assert_eq!(report["complete"], aterm_json::Value::Bool(false));
        assert!(!ports.applies().contains(&"clone"));
        ports.clone_is(OwedClone::Due("managed Git is now ready".into()));
        assert_eq!(converge_run(&["--converge"], &ports), exit(0));
        assert!(ports.applies().contains(&"clone"));
        ports.clone_is(OwedClone::Held("repositories left as is".into()));
        assert_eq!(converge_run(&["--converge"], &ports), exit(0));
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
            [(app.clone(), {
                let mut args = words(&[
                    "init",
                    "--converge",
                    "--trigger",
                    "subscriber",
                    "--repository-request",
                ]);
                args.push(
                    aterm_json::to_string(&aterm_secrets::converge::RepositoryRequest::Refresh(
                        aterm_secrets::converge::RefreshRequest::after(None, Default::default()),
                    ))
                    .unwrap(),
                );
                args.push("--json".into());
                args
            })]
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
        assert_eq!(
            ports.backgrounds(),
            [
                crate::sync::Trigger::Subscriber,
                crate::sync::Trigger::Window,
                crate::sync::Trigger::Init
            ],
            "each runner lowers itself as its own trigger's pass, the hand-off's first"
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
        assert_eq!(untouched.backgrounds(), [], "a usage error reads nothing");
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
        let station = crate::init_spotlight::items(
            &crate::items::defaults::tests::ctx(&[alice]),
            crate::init_spotlight::Projects::Noted,
        )
        .unwrap();
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

    /// `base.clt` reads only once the base layer let its hold go, and off the console (no one
    /// logged in at its screen, no terminal) its install waits for a person there: Deferred,
    /// naming where to run it, with nothing run.
    #[test]
    fn base_clt_runs_after_the_base_hold_is_released_and_waits_for_a_person_off_the_console() {
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
        sys.running(&[]);
        let rows = run(&sys, &ctx, base, &[], Mode::Apply, &mut NoJournal).unwrap();
        assert_eq!(rows.len(), 1, "{rows:?}");
        let row = &rows[0];
        assert_eq!(row.id, crate::init_clt::ID);
        assert_eq!(row.outcome, Outcome::Deferred, "{row:?}");
        assert_eq!(row.layer, Some(Layer::Base));
        assert_eq!(row.class, Some(Class::Elevate));
        assert!(
            row.why.as_deref().is_some_and(|why| why
                .ends_with("console (nobody is logged in at its screen): run aterm init there")),
            "{row:?}"
        );
        let saw = saw.lock().unwrap();
        assert!(
            !saw.is_empty() && saw.iter().all(|&released| released),
            "{saw:?}"
        );
        assert!(sys.calls().is_empty(), "{:?}", sys.calls());
    }

    #[test]
    fn repository_setup_does_not_wait_for_the_apple_tools_install() {
        use std::sync::Arc;
        struct Repositories(Arc<Mutex<Vec<&'static str>>>);
        impl Item for Repositories {
            fn id(&self) -> String {
                "init.github.native@alice".into()
            }
            fn family(&self) -> &'static str {
                "github"
            }
            fn title(&self) -> String {
                "Repositories".into()
            }
            fn probe(&self, _: &dyn System, _: &Ctx) -> Probe {
                self.0.lock().unwrap().push("repositories");
                ok()
            }
            fn apply(&self, _: &dyn System, _: &Ctx) -> Result<(), String> {
                panic!("the repositories are already present")
            }
        }
        let seen = Arc::new(Mutex::new(Vec::new()));
        let toolset_seen = Arc::clone(&seen);
        let clt_seen = Arc::clone(&seen);
        let base = Base {
            items: Vec::new(),
            held: None,
            toolset: Some(crate::init_toolset::Toolset::new(
                Box::new(move || {
                    toolset_seen.lock().unwrap().push("toolset");
                    crate::init_toolset::Found::Complete("Git installed".into())
                }),
                Box::new(|_, _, _| panic!("the toolset is already installed")),
                None,
                crate::init_toolset::tests::ATPKG,
            )),
            clt: Some(crate::init_clt::CommandLineTools::new(
                Box::new(move |_| {
                    let mut seen = clt_seen.lock().unwrap();
                    assert!(seen.contains(&"repositories"), "{seen:?}");
                    seen.push("apple tools");
                    crate::init_clt::Found::Present("compiler installed".into())
                }),
                PathBuf::from("/Applications/aterm.app/Contents/MacOS/aterm"),
            )),
            targo: None,
        };
        let groups: Vec<Group> = vec![(
            vec![Box::new(Repositories(Arc::clone(&seen)))],
            place(Class::Install),
        )];
        run(
            &Fake::default(),
            &at_console(user("alice", 501)),
            base,
            &groups,
            Mode::Apply,
            &mut NoJournal,
        )
        .unwrap();
        let seen = seen.lock().unwrap();
        let at = |name| seen.iter().position(|step| *step == name).unwrap();
        assert!(at("toolset") < at("repositories"), "{seen:?}");
        assert!(at("repositories") < at("apple tools"), "{seen:?}");
    }

    /// `base.pkg.toolset` READS ONLY ONCE THE BASE LAYER LET THE STORE LOCK GO (its passes
    /// take it), before `base.clt` and the develop layer, whose tools run after it
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
                    Box::new(move |args: &[String], _, _: &dyn Fn() -> bool| {
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
                    relayed: false,
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
            // The machine sees nothing of the run but the stops the toolset's passes hold.
            let held: &[&str] = if passes > 0 {
                &["defer stops", "stops restored"]
            } else {
                &[]
            };
            assert_eq!(sys.calls(), held);
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
    fn cached_completion_of_held_scope_does_not_verify_an_explicit_profile_check() {
        let mut sys = alice_at(501);
        sys.terminal = false;
        sys.agent = Some("Codex");
        let (ctx, _) = local_context(&sys).unwrap();
        let options =
            options(&["--profile", "--check", "--no-sign-out", "--no-clone"].map(OsString::from))
                .unwrap();
        let mut partial = Selection::none();
        partial.set(Setting::CapsLock, true);
        for (verified, ready) in [
            (Selection::none(), false),
            (partial, false),
            (Selection::all().workstation(), true),
        ] {
            let mut standing = personal_standing();
            for setting in Setting::ALL
                .into_iter()
                .filter(|setting| setting.is_workstation())
            {
                standing.holds.off.set(setting, !verified.contains(setting));
            }
            // A coordinator whose groups are all left as is legitimately reports completion,
            // but has no observations proving the profile this command explicitly names.
            standing.workstation_observation = Some(WorkstationObservation::Complete(verified));
            let vault = crate::init_secrets::tests::Scripted::new(vec![Ok(
                crate::init_secrets::tests::unconfigured(),
            )]);
            let (groups, picked) = prepare_groups_with(
                &sys,
                &ctx,
                Mode::Plan,
                None,
                &Reading {
                    keyboards: &KeyboardRead::EMPTY,
                    context_scope: Selection::all(),
                    only: &[],
                    picks: options.picks,
                },
                &standing,
                no_grants,
                move |_| Ok(vault as Arc<dyn crate::init_secrets::SecretsOps>),
            )
            .unwrap();
            assert!(
                picked.runs.contains(Setting::Dock),
                "the explicit profile overrides its earlier hold"
            );
            let away = groups
                .into_iter()
                .filter(|(items, _)| items.iter().any(|item| item.id() == AWAY))
                .collect::<Vec<_>>();
            let before = sys.calls();
            let rows = run(&sys, &ctx, Base::none(), &away, Mode::Plan, &mut NoJournal).unwrap();
            assert_eq!(
                complete(&rows, Action::Check),
                ready,
                "another observation scope cannot verify these preferences"
            );
            assert_eq!(
                sys.calls(),
                before,
                "the check must not re-probe private preferences"
            );
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
        alice_with_keyboard_census(console_uid, "Services:\n\nDevices:\n")
    }

    fn alice_with_keyboard_census(console_uid: u32, census: &str) -> Fake {
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
            Ran::ok(census),
        );
        sys
    }

    #[test]
    fn attachment_activation_does_not_settle_an_unseen_requested_service() {
        let sys = alice_at(501);
        let invalidated = std::cell::Cell::new(0);
        assert!(
            !activate_keyboard_attachments(&sys, &[41], &|_| false, &|| true, &|| invalidated
                .set(invalidated.get() + 1))
            .unwrap(),
            "a native service missing from the separate model census remains retryable"
        );
        assert_eq!(
            invalidated.get(),
            1,
            "uncertain coverage retires an older reading once"
        );
        assert!(sys.keyboard_changes().is_empty());
    }

    #[test]
    fn saved_caps_preferences_do_not_verify_an_unseen_native_keyboard() {
        use aterm_types::macos_setup::{Observation, Standing as SettingStanding, Subject};

        let mut sys = alice_at(501);
        sys.keyboard_services = Some(Ok(vec![41]));
        let mapping = crate::items::keyboard::Model {
            vendor: 0,
            product: 0,
        }
        .key();
        let saved = crate::plist::document(&PValue::Dict(vec![(
            mapping,
            PValue::Array(vec![crate::items::keyboard::caps_to_control()]),
        )]));
        sys.on(
            "alice$ /usr/bin/defaults -currentHost export -g -",
            Ran::ok(&saved),
        );
        sys.on(
            "/usr/bin/defaults export /Library/Preferences/com.apple.keyboardtype -",
            export("<dict/>"),
        );
        assert_eq!(sys.keyboard_service_ids().unwrap(), [41]);
        assert!(
            !activate_keyboard_attachments(&sys, &[41], &|_| false, &|| true, &|| {})
                .unwrap_or(false),
            "attachment activation still owes the service absent from the model census"
        );

        let mut selected = Selection::none();
        selected.set(Setting::CapsLock, true);
        let observed = prepare_workstation_selected(&sys, WorkstationGroup::All, selected)
            .and_then(|pass| pass.run_tagged(&sys, Mode::Plan))
            .map_or_else(
                |_| Observation::unknown(Subject::Setting(Setting::CapsLock)),
                |rows| {
                    crate::init_observe::observation(
                        Setting::CapsLock,
                        &rows.iter().map(|(_, row)| row).collect::<Vec<_>>(),
                    )
                },
            );
        assert_ne!(
            observed.standing,
            SettingStanding::InPlace,
            "saved preferences cannot confirm a keyboard its live census did not observe"
        );
        assert_ne!(observed.now.as_deref(), Some("Control"));
        assert!(sys.keyboard_changes().is_empty());
        assert!(!sys.calls().iter().any(|call| call.contains(" write ")));
    }

    #[test]
    fn a_partial_live_mapping_read_cannot_verify_the_whole_keyboard_model() {
        use crate::items::keyboard::{CAPS_LOCK, LEFT_CONTROL, Model};
        use crate::port::Cmd;
        use aterm_types::macos_setup::Standing as SettingStanding;

        let census = format!(
            "Services:\n{:<12}{:<12}{:<16}{:<24}Built-In\n{:<12}{:<12}{:<16}{:<24}0\n{:<12}{:<12}{:<16}{:<24}0\n\nDevices:\n",
            "VendorID",
            "ProductID",
            "RegistryID",
            "Product",
            "0xfffe",
            "0xa9",
            "0x29",
            "Fixture keyboard",
            "0xfffe",
            "0xa9",
            "0x2a",
            "Fixture keyboard"
        );
        let mut sys = alice_with_keyboard_census(501, &census);
        sys.keyboard_services = Some(Ok(vec![41, 42]));
        let who = crate::items::defaults::tests::user("alice", 501);
        let models = [
            Model {
                vendor: 0,
                product: 0,
            },
            Model {
                vendor: 65534,
                product: 169,
            },
        ];
        let saved = crate::plist::document(&PValue::Dict(
            models
                .into_iter()
                .map(|model| {
                    (
                        model.key(),
                        PValue::Array(vec![crate::items::keyboard::caps_to_control()]),
                    )
                })
                .collect(),
        ));
        sys.on(
            "alice$ /usr/bin/defaults -currentHost export -g -",
            Ran::ok(&saved),
        );
        sys.on(
            "/usr/bin/defaults export /Library/Preferences/com.apple.keyboardtype -",
            export("<dict/>"),
        );
        let property = Cmd::new(aterm_uds::os_path!("hidutil"))
            .args([
                "property",
                "--matching",
                &crate::port::keyboard_usage::matching_model(65534, 169),
                "--get",
                "UserKeyMapping",
            ])
            .as_user(&who);
        sys.on(&property.display(), Ran::ok("RegistryID Key Value\n2a UserKeyMapping ( { HIDKeyboardModifierMappingSrc = 30064771129; HIDKeyboardModifierMappingDst = 30064771296; } )\n"));
        sys.on(&property.display(), Ran::ok("RegistryID Key Value\n29 UserKeyMapping ( { HIDKeyboardModifierMappingSrc = 30064771129; HIDKeyboardModifierMappingDst = 30064771296; } )\n2a UserKeyMapping ( { HIDKeyboardModifierMappingSrc = 30064771129; HIDKeyboardModifierMappingDst = 30064771296; } )\n"));
        let dump = Cmd::new(aterm_uds::os_path!("hidutil"))
            .args(["dump", "services", "-f", "xml"])
            .as_user(&who);
        sys.on(
            &dump.display(),
            Ran::ok(&crate::init_preferences::tests::hid_dump(&[(
                42,
                &[],
                &[(CAPS_LOCK, LEFT_CONTROL)],
            )])),
        );
        sys.on(
            &dump.display(),
            Ran::ok(&crate::init_preferences::tests::hid_dump(&[
                (41, &[], &[(CAPS_LOCK, LEFT_CONTROL)]),
                (42, &[], &[(CAPS_LOCK, LEFT_CONTROL)]),
            ])),
        );
        let mut selected = Selection::none();
        selected.set(Setting::CapsLock, true);
        let rows = prepare_workstation_selected(&sys, WorkstationGroup::All, selected)
            .unwrap()
            .run_tagged(&sys, Mode::Plan)
            .unwrap();
        let observed = crate::init_observe::observation(
            Setting::CapsLock,
            &rows.iter().map(|(_, row)| row).collect::<Vec<_>>(),
        );
        assert_ne!(
            observed.standing,
            SettingStanding::InPlace,
            "one mapped service cannot verify its model's other unread native service"
        );
        assert_ne!(observed.now.as_deref(), Some("Control"));
        let recovered = prepare_workstation_selected(&sys, WorkstationGroup::All, selected)
            .unwrap()
            .run_tagged(&sys, Mode::Plan)
            .unwrap();
        let recovered = crate::init_observe::observation(
            Setting::CapsLock,
            &recovered.iter().map(|(_, row)| row).collect::<Vec<_>>(),
        );
        assert_eq!(recovered.standing, SettingStanding::InPlace);
        assert_eq!(recovered.now.as_deref(), Some("Control"));
        assert!(sys.keyboard_changes().is_empty());
    }

    #[test]
    fn an_unconfirmed_keyboard_leaves_other_selected_settings_readable() {
        use aterm_types::macos_setup::Standing as SettingStanding;

        for native in [
            Ok(vec![41]),
            Err("native census is unread".into()),
            Ok(Vec::new()),
        ] {
            let mut sys = alice_at(501);
            sys.keyboard_services = Some(native.clone());
            saved_off(&sys);
            sys.scroll_state(false);
            let mapping = crate::items::keyboard::Model {
                vendor: 0,
                product: 0,
            }
            .key();
            sys.on(
                "alice$ /usr/bin/defaults -currentHost export -g -",
                Ran::ok(&crate::plist::document(&PValue::Dict(vec![(
                    mapping,
                    PValue::Array(vec![crate::items::keyboard::caps_to_control()]),
                )]))),
            );
            sys.on(
                "/usr/bin/defaults export /Library/Preferences/com.apple.keyboardtype -",
                export("<dict/>"),
            );
            let mut selected = Selection::none();
            selected.set(Setting::CapsLock, true);
            selected.set(Setting::NaturalScrolling, true);
            let rows = prepare_workstation_selected(&sys, WorkstationGroup::All, selected)
                .unwrap()
                .run_tagged(&sys, Mode::Plan)
                .unwrap();
            for setting in [Setting::CapsLock, Setting::NaturalScrolling] {
                let observed = crate::init_observe::observation(
                    setting,
                    &rows
                        .iter()
                        .filter(|(tag, _)| *tag == setting)
                        .map(|(_, row)| row)
                        .collect::<Vec<_>>(),
                );
                let confirmed = setting != Setting::CapsLock || native == Ok(Vec::new());
                assert_eq!(
                    observed.standing,
                    if confirmed {
                        SettingStanding::InPlace
                    } else {
                        SettingStanding::Unknown
                    },
                    "{setting:?}, {native:?}"
                );
            }
            assert!(sys.keyboard_changes().is_empty());
            assert!(!sys.calls().iter().any(|call| call.contains(" write ")));
        }

        let mut sys = alice_at(501);
        sys.keyboard_services = Some(Err("Caps is not selected; must not read".into()));
        saved_off(&sys);
        sys.scroll_state(false);
        let mut selected = Selection::none();
        selected.set(Setting::NaturalScrolling, true);
        prepare_workstation_selected(&sys, WorkstationGroup::All, selected)
            .unwrap()
            .run_tagged(&sys, Mode::Plan)
            .unwrap();
        assert!(
            sys.calls()
                .iter()
                .all(|call| { !call.contains("hidutil") && call != "keyboard-service-ids" })
        );
    }

    #[test]
    fn held_attachments_cover_only_matching_services_without_probing_preferences() {
        let census = format!(
            "Services:\n{:<12}{:<12}{:<16}{:<24}Built-In\n{:<12}{:<12}{:<16}{:<24}0\n{:<12}{:<12}{:<16}{:<24}0\n\nDevices:\n",
            "VendorID",
            "ProductID",
            "RegistryID",
            "Product",
            "0xfffe",
            "0xa9",
            "0x29",
            "Fixture keyboard",
            "0xfffe",
            "0xa9",
            "0x2a",
            "Fixture keyboard"
        );
        let mut sys = alice_with_keyboard_census(501, &census);
        sys.keyboard_services = Some(Ok(vec![41, 42, 43]));
        let who = crate::items::defaults::tests::user("alice", 501);
        let get = crate::port::Cmd::new(aterm_uds::os_path!("hidutil"))
            .args([
                "property",
                "--matching",
                &crate::port::keyboard_usage::matching_model(65534, 169),
                "--get",
                "UserKeyMapping",
            ])
            .as_user(&who);
        sys.on(
            &get.display(),
            Ran::ok("RegistryID Key Value\n29 UserKeyMapping (null)\n"),
        );
        let invalidated = std::cell::Cell::new(0);
        assert!(
            activate_keyboard_attachments(&sys, &[41], &|_| true, &|| true, &|| invalidated
                .set(invalidated.get() + 1))
            .unwrap()
        );
        assert_eq!(
            invalidated.get(),
            1,
            "kept requested coverage is no whole-Caps proof"
        );
        invalidated.set(0);
        assert!(
            !activate_keyboard_attachments(&sys, &[41, 42], &|_| true, &|| true, &|| invalidated
                .set(invalidated.get() + 1))
            .unwrap(),
            "an observed held model cannot cover an unseen requested service"
        );
        assert_eq!(invalidated.get(), 1, "kept and uncertain coverage coalesce");
        assert!(sys.keyboard_changes().is_empty());
        assert!(
            sys.calls()
                .iter()
                .all(|call| { !call.contains("defaults") && !call.contains("dump services") })
        );
    }

    #[test]
    fn a_noninteractive_check_relays_background_evidence_without_probing_preferences() {
        let mut sys = alice_at(501);
        sys.terminal = false;
        sys.agent = None;
        let (ctx, _) = local_context(&sys).unwrap();
        let mut standing = personal_standing();
        standing.workstation_observation = Some(WorkstationObservation::Complete(
            Selection::all().workstation(),
        ));
        let before = sys.calls();
        for (mode, outcome) in [(Mode::Plan, Outcome::Ok), (Mode::Apply, Outcome::Deferred)] {
            let vault = crate::init_secrets::tests::Scripted::new(vec![Ok(
                crate::init_secrets::tests::unconfigured(),
            )]);
            let (mut groups, _) = prepare_groups_with(
                &sys,
                &ctx,
                mode,
                None,
                &reading(&[]),
                &standing,
                no_grants,
                move |_| Ok(vault as Arc<dyn crate::init_secrets::SecretsOps>),
            )
            .unwrap();
            groups.retain(|(items, _)| items.iter().any(|item| item.id() == AWAY));
            let scratch = aterm_tempfile::tempdir().unwrap();
            let mut journal = FileJournal::under(scratch.path());
            let rows = run(&sys, &ctx, Base::none(), &groups, mode, &mut journal).unwrap();
            let row = rows.iter().find(|row| row.id == AWAY).unwrap();
            assert_eq!(row.outcome, outcome);
            if mode == Mode::Plan {
                assert_eq!(status(&rows, Action::Check), 0);
            }
        }
        assert_eq!(
            sys.calls(),
            before,
            "neither path reads or changes other apps"
        );
        for action in [Action::Apply, Action::Plan] {
            assert!(
                observed_workstation(&sys, action, Selection::all(), &|_| panic!(
                    "a non-check queried observations"
                ))
                .is_none()
            );
        }
        assert_eq!(
            observed_workstation(&sys, Action::Check, Selection::all(), &|_| {
                WorkstationObservation::Complete(Selection::all().workstation())
            }),
            Some(WorkstationObservation::Complete(
                Selection::all().workstation()
            ))
        );
        let other = alice_at(502);
        assert!(
            observed_workstation(&other, Action::Check, Selection::all(), &|_| panic!(
                "another login queried observations"
            ))
            .is_none()
        );
    }

    #[test]
    fn background_check_distinguishes_waiting_permission_and_missing_evidence() {
        let sys = alice_at(501);
        let (ctx, _) = local_context(&sys).unwrap();
        let before = sys.calls();
        for (observation, code) in [
            (WorkstationObservation::Waiting("work continues".into()), 75),
            (
                WorkstationObservation::Permission("a real grant is off".into()),
                77,
            ),
            (
                WorkstationObservation::Unavailable("not observed".into()),
                1,
            ),
        ] {
            let groups = vec![away_from_console(Presence::AgentTab, Some(observation))];
            let scratch = aterm_tempfile::tempdir().unwrap();
            let mut journal = FileJournal::under(scratch.path());
            let rows = run(&sys, &ctx, Base::none(), &groups, Mode::Plan, &mut journal).unwrap();
            assert_eq!(status(&rows, Action::Check), code);
            assert_eq!(rows[0].class, Some(Class::Relay));
        }
        assert_eq!(sys.calls(), before);
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
                assert_eq!(status(&rows, Action::Check), 77);
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
            let away = rows
                .iter()
                .find(|row| row.id == AWAY)
                .expect("the stand-in");
            assert_eq!(away.outcome, Outcome::Deferred);
            assert_eq!(away.why, Presence::Remote.needs_person());
            // Beside it run the background rows an agent's tab runs (design §7): Rust in
            // Spotlight, the vault's join (here no vault answers) and the GitHub repositories,
            // and nothing of the console session.
            let background = ["init.spotlight.", "init.secrets@", "init.github."];
            assert!(
                rows.iter()
                    .all(|row| row.id == AWAY
                        || background.iter().any(|lane| row.id.starts_with(lane))),
                "{rows:?}"
            );
            for lane in background {
                assert!(
                    rows.iter().any(|row| row.id.starts_with(lane)),
                    "{lane}: {rows:?}"
                );
            }
            let secrets = rows
                .iter()
                .find(|row| row.id.starts_with("init.secrets@"))
                .expect("the background vault join");
            assert_eq!(secrets.outcome, Outcome::Blocked, "{rows:?}");
            assert!(matches!(
                secrets.stood_aside,
                Some(aterm_converge::StoodAside::Offline(_))
            ));
            assert_eq!(status(&rows, Action::Apply), 69, "{rows:?}");
            assert!(
                !sys.calls().iter().any(|call| {
                    [
                        "hidutil",
                        "defaults",
                        "killall",
                        "osascript",
                        "accounts.data",
                    ]
                    .iter()
                    .any(|console| call.contains(console))
                }),
                "{:?}",
                sys.calls()
            );
        }
    }

    type BoundFixture = (
        Vec<(String, Class)>,
        Vec<Group>,
        Ctx,
        Arc<crate::init_secrets::tests::Scripted>,
    );

    /// The rows a run of `sys` binds, standing as `standing` says, with an unconfigured vault
    /// whose look answers as ALab's open vault when `alab`; the groups, context and vault port
    /// to read back.
    fn bound_for(sys: &Fake, standing: &Standing<'_>, alab: bool) -> BoundFixture {
        let (ctx, _) = local_context(sys).unwrap();
        let vault = crate::init_secrets::tests::Scripted::new(vec![Ok(
            crate::init_secrets::tests::unconfigured(),
        )]);
        if alab {
            vault.alab_answers();
        }
        let port = Arc::clone(&vault);
        let groups = prepare_groups_with(
            sys,
            &ctx,
            Mode::Apply,
            None,
            &reading(&[]),
            standing,
            no_grants,
            move |_| Ok(port as Arc<dyn crate::init_secrets::SecretsOps>),
        )
        .unwrap()
        .0;
        let bound = crate::engine::bind(
            groups.iter().flat_map(|(items, place)| {
                items.iter().map(move |item| (item.as_ref(), place.clone()))
            }),
            sys,
            &ctx,
        )
        .iter()
        .map(|b| (b.id(), b.class()))
        .collect::<Vec<_>>();
        (bound, groups, ctx, vault)
    }

    /// With no terminal (an agent's tool call, a launch agent, a script), in the console's own
    /// session, init runs the base layer and looks for the vault, and signs out of Messages and
    /// FaceTime, which nothing undoes, only for a person at the screen now (input within the
    /// last minute, the screen unlocked); with nobody there they wait. An agent's local setup
    /// runs the selected profile. Rust in Spotlight runs for each; no preference
    /// runs off the console: no `killall`, no `defaults write`. Control: a person at the console
    /// gets every row.
    #[test]
    fn with_no_terminal_the_sign_outs_wait_for_a_person_at_the_screen() {
        let personal = [Class::Preference];
        for (terminal, agent, presence, present) in [
            (false, None, Presence::NoTerminal, false),
            (false, None, Presence::NoTerminal, true),
            (true, Some("Claude Code"), Presence::AgentTab, true),
            (true, None, Presence::AtConsole, false),
        ] {
            let mut sys = alice_at(501);
            sys.terminal = terminal;
            sys.agent = agent;
            assert_eq!(crate::console::presence(&sys), presence);
            let standing = Standing {
                decided: Decider::Person,
                alab: &|| true,
                present: if present { &|| true } else { &|| false },
                holds: Holds::default(),
                workstation_observation: None,
            };
            let (bound, groups, ctx, vault) = bound_for(&sys, &standing, false);
            assert!(ctx.console.is_some(), "the console's own session");
            assert_eq!(
                vault.probes(),
                0,
                "{presence:?} prepares without looking for the ALab vault"
            );
            assert!(
                bound
                    .iter()
                    .any(|(id, _)| id == "init.spotlight.rustup-profile@alice"),
                "{presence:?}: {bound:?}"
            );
            if init_presence(&sys) == Presence::AtConsole {
                assert!(
                    bound.iter().any(|(_, class)| *class == Class::Preference),
                    "{bound:?}"
                );
                continue;
            }
            let personal_rows = bound
                .iter()
                .filter(|(_, class)| personal.contains(class))
                .collect::<Vec<_>>();
            if presence == Presence::NoTerminal && present {
                assert_eq!(personal_rows.len(), 2, "{bound:?}");
                assert!(
                    personal_rows
                        .iter()
                        .all(|(id, _)| crate::init_accounts::signs_out(id)),
                    "{bound:?}"
                );
            } else {
                assert!(personal_rows.is_empty(), "{presence:?}: {bound:?}");
            }
            let rows = run(
                &sys,
                &ctx,
                Base::none(),
                &groups,
                Mode::Apply,
                &mut NoJournal,
            )
            .unwrap();
            assert_eq!(vault.probes(), 1, "{presence:?} looks during apply");
            let away = rows
                .iter()
                .find(|row| row.id == AWAY)
                .expect("the stand-in");
            assert_eq!(away.outcome, Outcome::Deferred);
            assert_eq!(away.why, presence.needs_person());
            let calls = sys.calls();
            for change in ["killall", "defaults write"] {
                assert!(
                    !calls.iter().any(|call| call.contains(change)),
                    "{change}: {calls:?}"
                );
            }
            if !present || presence == Presence::AgentTab {
                assert!(
                    !calls.iter().any(|call| call.contains("osascript")),
                    "{calls:?}"
                );
            }
        }
    }

    /// On a Mac that is not ALab's, `aterm init` at the console with no flag runs the base
    /// plan (Permissions, the secrets, Spotlight) and the Sync rows; it changes no preference
    /// of the ALab profile and signs out of nothing, which take `--profile` and `--sign-out`.
    #[test]
    fn a_personal_macs_console_run_changes_no_profile_and_signs_out_of_nothing() {
        let mut sys = alice_at(501);
        sys.terminal = true;
        sys.agent = None;
        assert_eq!(init_presence(&sys), Presence::AtConsole);
        let (bound, _, _, vault) = bound_for(&sys, &personal_standing(), false);
        assert_eq!(vault.probes(), 0, "preparation leaves discovery to apply");
        assert!(
            bound
                .iter()
                .any(|(id, _)| id == "init.spotlight.rustup-profile@alice"),
            "the base plan's Spotlight runs: {bound:?}"
        );
        let profile: Vec<_> = bound
            .iter()
            .filter(|(id, class)| {
                *class == Class::Preference || crate::init_accounts::signs_out(id)
            })
            .collect();
        assert!(profile.is_empty(), "{profile:?}");
        let (bound, _, _, _) = bound_for(&sys, &alab_standing(), false);
        assert!(
            bound
                .iter()
                .any(|(id, _)| crate::init_accounts::signs_out(id))
                && bound.iter().any(|(_, class)| *class == Class::Preference),
            "an ALab Mac's run takes the profile: {bound:?}"
        );
    }

    /// Off the console the vault join is a background row: an agent's tab whose look finds
    /// ALab's open vault asks it to let this Mac in, with no join password and no code shown.
    #[test]
    fn an_agent_tab_joins_the_open_alab_vault() {
        let mut sys = alice_at(501);
        sys.terminal = true;
        sys.agent = Some("Claude Code");
        assert_eq!(init_presence(&sys), Presence::AtConsole);
        let (_, groups, ctx, vault) = bound_for(&sys, &alab_standing(), true);
        let _ = run(
            &sys,
            &ctx,
            Base::none(),
            &groups,
            Mode::Apply,
            &mut NoJournal,
        )
        .unwrap();
        assert_eq!(vault.joins().len(), 1, "probes {}", vault.probes());
        assert_eq!(vault.passwords(), [false], "no join password");
    }

    /// A run aterm decided at this Mac's console (the follow-up of a person's package install,
    /// [`Decider::Aterm`]) puts no question of macOS's on the screen: Rust in Spotlight asks
    /// Spotlight for none of the person's build folders. Without the LAN broker the vault's look
    /// never runs, whoever decided the run, and the row waits for the broker (deferred, the run
    /// exits 0). NEGATIVE CONTROL: the person's own run at the console searches for them.
    #[test]
    fn a_run_aterm_decided_raises_no_question_of_macos() {
        const MDFIND: &str =
            "alice$ /usr/bin/mdfind -onlyin /Users//alice -0 'kMDItemFSName == \"CACHEDIR.TAG\"'";
        for (decided, asks) in [(Decider::Aterm, false), (Decider::Person, true)] {
            let mut sys = alice_at(501);
            sys.terminal = true;
            sys.agent = None;
            assert_eq!(init_presence(&sys), Presence::AtConsole);
            sys.create_dir_all(Path::new("/Users//alice/Library/Caches"), 0o755)
                .unwrap();
            sys.file("/usr/bin/mdfind", b"").on(MDFIND, Ran::ok(""));
            let (ctx, _) = local_context(&sys).unwrap();
            let vault = crate::init_secrets::tests::Scripted::new(vec![Ok(
                crate::init_secrets::tests::unconfigured(),
            )]);
            vault.broker_absent();
            let port = Arc::clone(&vault);
            let standing = Standing {
                decided,
                ..personal_standing()
            };
            let groups = prepare_groups_with(
                &sys,
                &ctx,
                Mode::Apply,
                None,
                &reading(&[]),
                &standing,
                no_grants,
                move |_| Ok(port as Arc<dyn crate::init_secrets::SecretsOps>),
            )
            .unwrap()
            .0;
            let rows = run(
                &sys,
                &ctx,
                Base::none(),
                &groups,
                Mode::Apply,
                &mut NoJournal,
            )
            .unwrap();
            assert_eq!(vault.probes(), 0, "{decided:?}: no look");
            assert_eq!(
                sys.calls().iter().any(|call| call == MDFIND),
                asks,
                "{decided:?}: {:?}",
                sys.calls()
            );
            let row = rows
                .iter()
                .find(|row| row.id.starts_with("init.secrets@"))
                .expect("the vault row");
            assert!(
                crate::init_secrets::waits_for_lan_broker(row),
                "{decided:?}: {row:?}"
            );
            assert_eq!(row.outcome, Outcome::Deferred, "{row:?}");
            assert_eq!(status(&rows, Action::Apply), 0, "{rows:?}");
        }
    }

    #[test]
    fn a_scoped_agent_check_reaches_its_advertised_workstation_selector() {
        let unread = "selected preference has no current cached reading";
        for (selector, console, terminal, agent, held, dock_verified, waiting, expected) in [
            (
                "github",
                501,
                true,
                Some("Codex"),
                false,
                false,
                false,
                Some(0),
            ),
            (
                "widgets",
                501,
                true,
                Some("Codex"),
                false,
                false,
                false,
                Some(75),
            ),
            (
                "keyboard",
                501,
                true,
                Some("Codex"),
                false,
                false,
                false,
                Some(0),
            ),
            (
                "keyboard",
                501,
                true,
                Some("Codex"),
                false,
                false,
                true,
                Some(75),
            ),
            (
                "keyboard.caps-lock.seen@alice",
                501,
                false,
                Some("Codex"),
                false,
                false,
                false,
                Some(0),
            ),
            (
                "keyboard,dock",
                501,
                true,
                Some("Codex"),
                false,
                false,
                false,
                Some(75),
            ),
            (
                "keyboard,dock",
                501,
                true,
                Some("Codex"),
                false,
                true,
                false,
                Some(0),
            ),
            (
                "keybord",
                501,
                true,
                Some("Codex"),
                false,
                false,
                false,
                None,
            ),
            ("keyboard", 501, false, None, true, false, false, None),
            (
                "keyboard",
                502,
                true,
                Some("Codex"),
                false,
                false,
                false,
                Some(77),
            ),
        ] {
            let mut sys = alice_at(console);
            sys.terminal = terminal;
            sys.agent = agent;
            let mut requested =
                options(&["--check", "--only", selector].map(OsString::from)).unwrap();
            requested.station = unnamed(&Base::none(), &requested.only);
            let mut verified = Selection::none();
            verified.set(Setting::CapsLock, true);
            verified.set(Setting::Dock, dock_verified);
            let mut standing = alab_standing();
            standing.holds.off.set(Setting::CapsLock, held);
            let fda_reads = std::cell::Cell::new(0);
            let full_disk_access = || {
                fda_reads.set(fda_reads.get() + 1);
                Some(true)
            };
            let prepare = |sys: &dyn System, ctx: &Ctx, mode: Mode, reading: &Reading<'_>| {
                let standing = Standing {
                    workstation_observation: observed_workstation(
                        sys,
                        Action::Check,
                        reading.context_scope,
                        &|required| {
                            assert_ne!(
                                selector, "github",
                                "GitHub-only needs no window's cached preference observation"
                            );
                            if selector.starts_with("keyboard") {
                                assert!(required.contains(Setting::CapsLock));
                                assert_eq!(
                                    required.contains(Setting::Dock),
                                    selector.contains("dock")
                                );
                                assert!(!required.contains(Setting::Repositories));
                            }
                            if waiting {
                                WorkstationObservation::Waiting(unread.into())
                            } else {
                                WorkstationObservation::Complete(verified)
                            }
                        },
                    ),
                    holds: standing.holds.clone(),
                    ..standing
                };
                prepare_groups_with(sys, ctx, mode, None, reading, &standing, no_grants, |_| {
                    Ok(crate::init_secrets::tests::Scripted::new(vec![Ok(
                        crate::init_secrets::tests::unconfigured(),
                    )])
                        as Arc<dyn crate::init_secrets::SecretsOps>)
                })
                .inspect(|(_, picked)| {
                    if held {
                        assert!(picked.kept.contains(Setting::CapsLock));
                        assert!(!picked.runs.contains(Setting::CapsLock));
                    }
                })
            };
            let result = converge(
                &sys,
                &requested,
                &Front {
                    base: &|_| Base::none(),
                    groups: &prepare,
                    full_disk_access: &full_disk_access,
                    targo: None,
                    cwd: None,
                    records: &NoSetup,
                    on_row: &|_| {},
                    present: &|| {
                        assert_ne!(
                            selector, "github",
                            "GitHub-only needs no WindowServer presence read"
                        );
                        true
                    },
                    on_held: &|_| {},
                },
                &mut NoJournal,
                &mut Laps::start(),
            );
            match expected {
                Some(code) => {
                    let rows =
                        result.expect("a real selector reaches the coordinator's observation");
                    assert_eq!(status(&rows, Action::Check), code, "{selector}: {rows:?}");
                    if selector == "github" {
                        assert_eq!(fda_reads.get(), 0, "GitHub-only needs no FDA reading");
                        assert_eq!(rows.len(), 2);
                        assert!(rows.iter().all(|row| row.family == "github"));
                        assert!(
                            sys.calls()
                                .iter()
                                .all(|call| !call.contains("hidutil")
                                    && call != "keyboard-service-ids"),
                            "GitHub-only needs no keyboard census: {:?}",
                            sys.calls()
                        );
                    } else {
                        assert_eq!(
                            rows.len(),
                            1,
                            "{selector}: only the selected aggregate runs"
                        );
                        assert_eq!(rows[0].id, AWAY);
                        if waiting {
                            assert_eq!(rows[0].why.as_deref(), Some(unread));
                        }
                        if console == 501 {
                            assert!(
                                rows[0]
                                    .explain
                                    .as_ref()
                                    .is_none_or(|explain| explain.remedy == Remedy::None),
                                "a read-only cached observation starts no setup"
                            );
                        }
                        assert_eq!(
                            rows[0].class,
                            Some(if console == 501 {
                                Class::Relay
                            } else {
                                Class::Person
                            }),
                            "another login cannot relay this account's cached evidence"
                        );
                    }
                }
                None => {
                    let Refused::Usage(why) = result.expect_err("a typo or held group is refused")
                    else {
                        panic!("selector refusal must remain a usage error");
                    };
                    assert!(why.contains(selector), "{why}");
                }
            }
            assert!(
                sys.calls().iter().all(|call| {
                    !call.contains("defaults")
                        && !call.contains("dump services")
                        && !call.contains("mdfind")
                        && !call.contains("osascript")
                }),
                "{selector}: {:?}",
                sys.calls()
            );
            assert!(sys.keyboard_changes().is_empty());
            if selector == "widgets" {
                assert_eq!(
                    fda_reads.get(),
                    1,
                    "selected contained keys retain the FDA reading"
                );
            }
            if selector.starts_with("keyboard") && console == 501 {
                assert!(
                    sys.calls().iter().any(|call| call.contains("hidutil list")),
                    "keyboard selection still discovers its actual services"
                );
            }
        }
    }

    #[test]
    fn only_a_persons_console_run_searches_spotlight_project_folders() {
        const MDFIND: &str =
            "alice$ /usr/bin/mdfind -onlyin /Users//alice -0 'kMDItemFSName == \"CACHEDIR.TAG\"'";
        const TARGET: &str = "/Users//alice/Documents/demo/target";
        for (terminal, agent, remote, decided, searches) in [
            (true, None, false, Decider::Person, true),
            (true, None, false, Decider::Aterm, false),
            (true, Some("Codex"), false, Decider::Person, false),
            (false, Some("Codex"), false, Decider::Person, false),
            (false, None, false, Decider::Person, false),
            (true, None, true, Decider::Person, false),
        ] {
            for mode in [Mode::Plan, Mode::Apply] {
                let mut sys = alice_at(501);
                sys.terminal = terminal;
                sys.agent = agent;
                sys.remote_login = remote;
                for cache in [".ubd/cargo-build.noindex", "tooling.noindex"] {
                    sys.create_dir_all(
                        &Path::new("/Users//alice/Library/Caches/aterm").join(cache),
                        0o755,
                    )
                    .unwrap();
                }
                sys.create_dir_all(Path::new(&format!("{TARGET}/debug")), 0o755)
                    .unwrap();
                sys.file(
                    &format!("{TARGET}/CACHEDIR.TAG"),
                    b"Signature: 8a477f597d28d172789f06886806bc55\n# created by cargo\n",
                );
                sys.file("/usr/bin/mdfind", b"")
                    .on(MDFIND, Ran::ok(&format!("{TARGET}/CACHEDIR.TAG\0")));
                let (ctx, _) = local_context(&sys).unwrap();
                let standing = Standing {
                    decided,
                    ..personal_standing()
                };
                let only = ["spotlight".to_owned()];
                let (groups, _) = prepare_groups_with(
                    &sys,
                    &ctx,
                    mode,
                    None,
                    &reading(&only),
                    &standing,
                    no_grants,
                    |_| panic!("a Spotlight-only run needs no vault"),
                )
                .unwrap();
                let builds = groups
                    .iter()
                    .flat_map(|(items, _)| items)
                    .find(|item| item.id() == "init.spotlight.build-noindex@alice")
                    .expect("the real build-cache item");
                let probe = builds.probe(&sys, &ctx);
                assert!(matches!(probe, Probe::Ok { .. }), "{probe:?}");
                assert_eq!(
                    sys.calls().iter().any(|call| call == MDFIND),
                    searches,
                    "{terminal:?}/{agent:?}/{remote:?}/{decided:?}/{mode:?}: {probe:?}"
                );
                assert_eq!(
                    matches!(probe, Probe::Ok { now } if now.contains(TARGET)),
                    searches,
                    "only the person's run reads and notes the protected cache tag"
                );
            }
        }
    }

    /// The package follow-up's words ([`PACKAGE_FOLLOW_UP`]) are one run init accepts: an apply
    /// whose base items stand aside for a pass holding the store, that still joins the vault
    /// and, on an ALab Mac, clones the repositories, and that signs out of nothing and changes
    /// no workstation preference on any Mac. NEGATIVE CONTROL: without `--no-sign-out` an ALab
    /// Mac's run signs out.
    #[test]
    fn the_package_follow_up_signs_out_of_nothing_and_changes_no_preference() {
        let words = |words: &[&str]| words.iter().map(OsString::from).collect::<Vec<_>>();
        let follow_up = options(&words(&PACKAGE_FOLLOW_UP)).unwrap();
        assert_eq!(follow_up.action, Action::Apply);
        assert_eq!(follow_up.base_lock(true), BaseLock::Try);
        let alab = follow_up.picks.choice(true);
        assert_eq!(
            alab,
            Choice {
                profile: false,
                sign_out: false,
                clone: true,
            }
        );
        assert_eq!(
            follow_up.picks.choice(false),
            Choice {
                clone: true,
                ..Choice::NONE
            }
        );
        let runs = alab.selection();
        assert!(runs.contains(Setting::Repositories), "{runs:?}");
        for left in [
            Setting::Messages,
            Setting::FaceTime,
            Setting::CapsLock,
            Setting::NaturalScrolling,
            Setting::TrackpadScrolling,
            Setting::Desktop,
            Setting::Widgets,
            Setting::Dock,
        ] {
            assert!(!runs.contains(left), "{left:?}: {runs:?}");
        }
        let signs_out = options(&words(&["--no-wait", "--no-profile"])).unwrap();
        assert!(signs_out.picks.choice(true).sign_out);
    }

    /// Agent setup starts the vault look at once.
    #[test]
    fn an_agent_tab_starts_the_vault_look_without_waiting_for_a_second_command() {
        let mut sys = alice_at(501);
        sys.terminal = true;
        sys.agent = Some("Claude Code");
        let (ctx, _) = local_context(&sys).unwrap();
        let vault = crate::init_secrets::tests::Scripted::new(vec![Ok(
            crate::init_secrets::tests::unconfigured(),
        )]);
        vault.alab_answers();
        let port = Arc::clone(&vault);
        let groups = prepare_groups_with(
            &sys,
            &ctx,
            Mode::Apply,
            None,
            &reading(&[]),
            &alab_standing(),
            no_grants,
            move |_| Ok(port as Arc<dyn crate::init_secrets::SecretsOps>),
        )
        .unwrap()
        .0;
        let rows = run(
            &sys,
            &ctx,
            Base::none(),
            &groups,
            Mode::Apply,
            &mut NoJournal,
        )
        .unwrap();
        assert_eq!(vault.probes(), 1);
        assert_eq!(vault.joins().len(), 1);
        assert!(rows.iter().any(|row| row.id.starts_with("init.secrets@")));
    }

    #[test]
    fn local_agent_tools_can_run_explicit_setup_flags_without_a_terminal() {
        for flag in ["--clone", "--profile", "--sign-out"] {
            for terminal in [false, true] {
                let mut sys = alice_with_checkout(501, None);
                sys.terminal = terminal;
                sys.agent = Some("Codex");
                let options = options(&[OsString::from(flag)]).unwrap();
                let groups = |_: &Ctx| Vec::new();
                assert!(
                    converge_with(&sys, &options, &groups, &NoRecords).is_ok(),
                    "{flag}, terminal={terminal}"
                );
            }
        }
    }

    /// An apply whose GitHub step waits for this Mac to be let into the vault records the clone
    /// it owes; the converge pass waits for the vault's GitHub token, then runs that step once
    /// and forgets it, so nobody runs `aterm init` again. A plan, or a run that owes nothing,
    /// records nothing.
    #[test]
    fn the_clone_init_owes_runs_once_the_token_lands_with_no_second_init() {
        use aterm_converge::StoodAside;
        let state = aterm_tempfile::tempdir().unwrap();
        let waiting = ItemReport {
            stood_aside: Some(StoodAside::Waiting("waiting to be let in".into())),
            ..report("init.github.native@alice", Outcome::Blocked)
        };
        let sys = alice_at(501);
        let nothing = || OwedClone::Nothing("aterm init owes no clone".into());
        note_owed(
            std::slice::from_ref(&waiting),
            Action::Plan,
            Some(state.path()),
            &Holds::NONE,
        )
        .unwrap();
        note_owed(
            &[report("init.test", Outcome::Changed)],
            Action::Apply,
            Some(state.path()),
            &Holds::NONE,
        )
        .unwrap();
        assert_eq!(
            owed_clone(&sys, state.path(), &Holds::NONE).unwrap(),
            nothing()
        );
        note_owed(&[waiting], Action::Apply, Some(state.path()), &Holds::NONE).unwrap();
        let owed = owed_clone(&sys, state.path(), &Holds::NONE).unwrap();
        assert!(matches!(owed, OwedClone::Waiting(_)), "{owed:?}");
        crate::init_github::scripted::converged(&sys, &user("alice", 501), "notes");
        let owed = owed_clone(&sys, state.path(), &Holds::NONE).unwrap();
        assert!(matches!(owed, OwedClone::Due(_)), "{owed:?}");
        clone_owed(
            &sys,
            state.path(),
            aterm_secrets::converge::RepositoryWork::Refresh,
        )
        .unwrap();
        assert_eq!(
            owed_clone(&sys, state.path(), &Holds::NONE).unwrap(),
            OwedClone::Verified {
                present: 1,
                total: 1
            },
            "the next pass verifies the inventory instead of cloning again"
        );
        assert!(
            sys.calls()
                .iter()
                .all(|call| !call.contains("hidutil") && call != "keyboard-service-ids"),
            "repository retry needs no workstation census: {:?}",
            sys.calls()
        );
    }

    /// The keychain row's own trouble is `sync.github`'s to retry: with the repositories done, the
    /// owed step is done, so a settings read that fails does not rerun it every pass.
    #[test]
    fn the_keychain_rows_trouble_does_not_hold_the_owed_clone() {
        use aterm_converge::StoodAside;
        let state = aterm_tempfile::tempdir().unwrap();
        let waiting = ItemReport {
            stood_aside: Some(StoodAside::Waiting("waiting to be let in".into())),
            ..report("init.github.native@alice", Outcome::Blocked)
        };
        let sys = alice_at(501);
        let who = user("alice", 501);
        note_owed(&[waiting], Action::Apply, Some(state.path()), &Holds::NONE).unwrap();
        sys.on(
            &crate::git_credential::script::helpers(&who),
            crate::port::Ran::exit(128, "fatal: bad config line 1"),
        );
        crate::init_github::scripted::converged(&sys, &who, "notes");
        clone_owed(
            &sys,
            state.path(),
            aterm_secrets::converge::RepositoryWork::Refresh,
        )
        .unwrap();
        assert_eq!(
            owed_clone(&sys, state.path(), &Holds::NONE).unwrap(),
            OwedClone::Verified {
                present: 1,
                total: 1
            },
        );
    }

    /// Fleet materialization alone starts repository reconciliation: no earlier init or
    /// clone-owed file is needed. A complete local inventory settles; a new entry wakes it.
    #[test]
    fn fleet_inventory_clones_without_an_earlier_init_and_respects_holds() {
        let state = aterm_tempfile::tempdir().unwrap();
        let sys = alice_at(501);
        let who = user("alice", 501);
        crate::init_github::scripted::converged(&sys, &who, "notes");
        assert_eq!(
            owed_clone(&sys, state.path(), &Holds::NONE).unwrap(),
            OwedClone::Verified {
                present: 1,
                total: 1
            },
        );
        let path = Path::new(&who.home).join(".secrets/github-repositories.json");
        let bytes = aterm_json::to_vec(&aterm_json::json!({
            "schema":2,"account":"alice","repositories":[{
                "id":2,"name":"new-code","owner":{"login":"alice"},
                "full_name":"alice/new-code","private":true,
                "clone_url":"https://github.com/alice/new-code.git","default_branch":"main"
            }]
        }))
        .unwrap();
        sys.file(path.to_str().unwrap(), &bytes).stat_of(
            path.to_str().unwrap(),
            Stat {
                uid: 501,
                gid: 20,
                mode: 0o600,
                dir: false,
                symlink: false,
                len: bytes.len() as u64,
            },
        );
        assert!(matches!(
            owed_clone(&sys, state.path(), &Holds::NONE).unwrap(),
            OwedClone::Due(_)
        ));
        let mut held = Holds::NONE;
        held.off.set(Setting::Repositories, true);
        assert!(matches!(
            owed_clone(&sys, state.path(), &held).unwrap(),
            OwedClone::Held(_)
        ));
        let path = Path::new(&who.home).join(".config/aterm/init.toml");
        let profile = b"[github]\nenabled=false\n";
        sys.file(path.to_str().unwrap(), profile).stat_of(
            path.to_str().unwrap(),
            Stat {
                uid: 501,
                gid: 20,
                mode: 0o600,
                dir: false,
                symlink: false,
                len: profile.len() as u64,
            },
        );
        assert!(matches!(
            owed_clone(&sys, state.path(), &Holds::NONE).unwrap(),
            OwedClone::Off(_)
        ));
    }

    /// GitHub waiting for the vault to let this Mac in is owed, not blocked: its row reads
    /// deferred, saying when it runs, and init exits 0, not 75.
    #[test]
    fn github_waiting_on_the_vaults_approval_exits_0() {
        use aterm_converge::StoodAside;
        let row = |id: &str| ItemReport {
            id: id.into(),
            family: "test".into(),
            title: id.into(),
            outcome: Outcome::Blocked,
            from: None,
            to: None,
            why: Some("waits for init.secrets@alice: waiting to be let in".into()),
            revert: None,
            effect: Effect::Now,
            detail: None,
            layer: None,
            class: None,
            explain: None,
            stood_aside: Some(StoodAside::Waiting("waiting to be let in".into())),
            essential: false,
            elapsed_ms: None,
        };
        let rows: Vec<ItemReport> = [row("init.secrets@alice"), row("init.github.native@alice")]
            .into_iter()
            .map(owe)
            .collect();
        assert_eq!(status(&rows, Action::Apply), 0);
        assert_eq!(
            rows[0].outcome,
            Outcome::Blocked,
            "the join finishes by itself"
        );
        assert!(
            rows[1].outcome == Outcome::Deferred && rows[1].why.as_deref() == Some(OWED),
            "{rows:?}"
        );
        assert!(tally(&rows, Action::Apply).contains("to run once this Mac is let in"));
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
            let (groups, picked) = prepare_groups_with(
                &sys,
                &ctx,
                Mode::Apply,
                None,
                &reading(&[]),
                &alab_standing(),
                no_grants,
                move |_| Ok(port as Arc<dyn crate::init_secrets::SecretsOps>),
            )
            .unwrap();
            assert_eq!(picked, Picked::ALL);
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

    /// The ordinary `init --only github` producer shares the existing alignment
    /// lease too; it cannot slip past a subscriber's admitted repository wave.
    #[test]
    fn plain_github_apply_respects_the_existing_repository_pass_lease() {
        let mut sys = Fake::root();
        sys.euid = 501;
        sys.linux = true;
        sys.terminal = true;
        let alice = user("alice", 501);
        sys.logged_in(&alice);
        crate::init_github::scripted::converged(&sys, &alice, "notes");
        let mut picked = options(&["--only".into(), "github".into()]).unwrap();
        picked.station = vec!["github".into()];
        let groups = |sys: &dyn System, ctx: &Ctx, mode: Mode, reading: &Reading<'_>| {
            prepare_groups_with(
                sys,
                ctx,
                mode,
                None,
                reading,
                &alab_standing(),
                no_grants,
                |_| panic!("GitHub-only init does not open a Secrets stage"),
            )
        };
        let run = || {
            converge(
                &sys,
                &picked,
                &Front {
                    base: &|_| Base::none(),
                    groups: &groups,
                    full_disk_access: &|| panic!("repository init reads no workstation permission"),
                    targo: None,
                    cwd: None,
                    records: &NoRecords,
                    on_row: &|_| {},
                    present: &|| false,
                    on_held: &|_| {},
                },
                &mut NoJournal,
                &mut Laps::start(),
            )
            .unwrap()
        };
        let held = sys.repository_pass().unwrap().pass.unwrap();
        let rows = run();
        assert!(
            rows.iter()
                .any(|row| row.id == "init.github.native@alice" && row.outcome == Outcome::Failed),
            "{rows:?}"
        );
        assert!(
            sys.calls()
                .iter()
                .all(|call| !call.contains("--symref") && !call.contains(" fetch "))
        );
        drop(held);
        let rows = run();
        assert!(
            rows.iter()
                .any(|row| row.id == "init.github.native@alice" && row.outcome == Outcome::Ok),
            "{rows:?}"
        );
        let record = sys.repository_pass().unwrap().pass.unwrap().record();
        assert!(record.converged_ms.is_none());
        assert_eq!(
            crate::sync::due(Some(&record), "current", &[], 0),
            crate::sync::Due::NewBuild
        );
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
        // A vault join an admin has yet to approve is delegated for apply and plan:
        // the subscriber finishes it. A check still reports that approval is missing.
        for action in [Action::Apply, Action::Plan] {
            assert_eq!(status(&[done.clone(), vault()], action), 0);
        }
        assert_eq!(status(&[done.clone(), vault()], Action::Check), 77);
        // A row waiting for that join is owed, and runs once this Mac is let in: no second
        // run is needed for it.
        assert_eq!(
            status(&[vault(), row(Outcome::Blocked, waiting())], Action::Apply),
            0
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
            let why = unread
                .unread
                .as_ref()
                .expect("the list is not read")
                .clone();
            assert!(ctx.console.is_some() && ctx.facts.keyboards.is_empty());
            assert!(
                !sys.calls()
                    .iter()
                    .any(|call| call.contains("defaults") || call.contains("accounts.data"))
            );
            let groups = groups(
                &ctx,
                Presence::AtConsole,
                crate::init_spotlight::Projects::Noted,
                crate::init_secrets::Secrets {
                    items: Vec::new(),
                    waited_on: None,
                },
                Vec::new(),
                &unread,
                Selection::all(),
                &[],
                true,
                None,
            )
            .0;
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
        assert!(unread.unread.is_none() && ctx.facts.keyboards.is_empty());
        let sys = current_user();
        sys.on(&command.display(), Ran::ok("Services:\n"));
        sys.refuse_complete_stdout(&command.display(), "incomplete captured service list");
        assert!(local_context(&sys).unwrap().1.unread.is_some());
    }

    #[test]
    fn pending_vault_approval_is_not_a_complete_read_only_check() {
        use crate::init_secrets::SecretsOps as _;
        use crate::init_secrets::tests::{Scripted, configured, converged};
        use aterm_secrets::client::Membership;

        let mut sys = Fake::root();
        sys.euid = 501;
        let context = ctx(&[user("alice", 501)]);
        for pending in [true, false] {
            let vault = if pending {
                let vault = Scripted::new(vec![Ok(configured(Membership::Pending))]);
                // Arrange the subscriber that will finish this request after approval.
                // The scripted service manager writes no real unit or state.
                vault.install_subscriber().unwrap();
                vault
            } else {
                converged()
            };
            let installs = vault.installs();
            for action in [Action::Check, Action::Apply, Action::Plan] {
                let mode = if action == Action::Apply {
                    Mode::Apply
                } else {
                    Mode::Plan
                };
                let port = Arc::clone(&vault) as Arc<dyn crate::init_secrets::SecretsOps>;
                let step = crate::init_secrets::items(&sys, &context, port, mode, None).unwrap();
                let rows = crate::engine::run(&step.items, mode, &sys, &context, &mut NoJournal);
                assert_eq!(rows.len(), 1);
                if pending {
                    assert!(waits_by_itself(&rows[0]), "{rows:?}");
                    assert_eq!(rows[0].outcome, Outcome::Blocked);
                } else {
                    assert_eq!(rows[0].outcome, Outcome::Ok);
                }
                let ready = !pending || action != Action::Check;
                assert_eq!(complete(&rows, action), ready, "{action:?}: {rows:?}");
                assert_eq!(status(&rows, action), if ready { 0 } else { 77 });
                let json =
                    report_json(action, complete(&rows, action), rows, &Laps::start()).unwrap();
                let json: aterm_json::Value = aterm_json::from_str(&json).unwrap();
                assert_eq!(json["complete"], aterm_json::Value::Bool(ready));
                assert!(vault.joins().is_empty());
                assert_eq!(vault.installs(), installs);
                assert_eq!(vault.probes(), 0, "a configured vault needs no discovery");
            }
        }
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
                    Picked::ALL,
                ))
            };
            converge(
                sys,
                options,
                &Front {
                    base: &base,
                    groups: &groups,
                    full_disk_access: &|| sys.full_disk_access(),
                    targo: None,
                    cwd: Some(Path::new("/Users//alice/aterm")),
                    records: &NoRecords,
                    on_row: &|_| {},
                    present: &|| true,
                    on_held: &|_| {},
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
                    &crate::checkout::network(&sys, &alice, Some(Path::new("/Users//alice/aterm")))
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

    /// DOCTOR'S CLONE TUNING is the workstation item `init.github.tuning@alice` bound as a
    /// relay-class repair of the account's own: nothing is read before the call, and under System
    /// State's hold of GitHub repositories bare doctor's scope reads it not applicable, naming
    /// the hold, and runs no git. NEGATIVE CONTROL: with no hold the same Mac's row names what it
    /// read instead.
    #[test]
    fn doctor_s_clone_tuning_is_a_relay_item_of_the_account_s_own() {
        let mac = || {
            let sys = alice_with_checkout(501, None);
            let setup = sys.calls().len();
            (
                Doctor::over(sys, Some(PathBuf::from("/Users//alice")), None),
                setup,
            )
        };
        let run = |doctor: &Doctor<Fake>, holds: Holds| {
            let items = doctor
                .clone_tuning(Path::new("/state"), Box::new(move || holds.clone()))
                .expect("alice's context reads");
            aterm_converge::run_scoped(
                &items,
                aterm_converge::Mode::Apply,
                aterm_converge::Scope::REPAIR,
                &mut aterm_converge::NoJournal,
                &mut |_| {},
            )
            .unwrap()
        };
        let mut off = Selection::none();
        off.set(Setting::Repositories, true);
        let held = Holds { off, ..Holds::NONE };
        let (doctor, setup) = mac();
        assert_eq!(
            doctor.sys.calls().len(),
            setup,
            "nothing read before the call"
        );
        let rows = run(&doctor, held);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].id, "init.github.tuning@alice");
        assert_eq!(rows[0].class, Some(Class::Relay));
        assert_eq!(rows[0].layer, Some(Layer::Workstation));
        assert_eq!(rows[0].outcome, Outcome::NotApplicable, "{rows:?}");
        assert_eq!(
            rows[0].why.as_deref(),
            Some("left as you set it in System State")
        );
        assert!(
            !doctor
                .sys
                .calls()
                .iter()
                .any(|call| call.contains("rev-parse") || call.contains("config")),
            "{:?}",
            doctor.sys.calls()
        );

        let (doctor, _) = mac();
        let rows = run(&doctor, Holds::NONE);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_ne!(
            rows[0].why.as_deref(),
            Some("left as you set it in System State"),
            "{rows:?}"
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

    /// The Messages and FaceTime sign-outs, which nothing undoes, run after every other row,
    /// another lane's included: the vault's join here holds its lane for a second watching for
    /// a sign-out to start, and none starts until it ends.
    #[test]
    fn the_sign_outs_run_after_every_other_row() {
        type Log = Arc<(Mutex<Vec<&'static str>>, std::sync::Condvar)>;
        let log: Log = Arc::default();
        let note = |log: &Log, what: &'static str| {
            log.0.lock().unwrap().push(what);
            log.1.notify_all();
        };
        let (dock, sign_out, vault) = (Arc::clone(&log), Arc::clone(&log), Arc::clone(&log));
        let groups = vec![
            (
                vec![
                    then("init.test.dock", "dock", move || note(&dock, "dock")),
                    then("init.messages.signed-out@alice", "init", move || {
                        note(&sign_out, "sign-out");
                    }),
                ],
                place(Class::Preference),
            ),
            (
                vec![then(
                    "init.test.vault",
                    crate::init_secrets::FAMILY,
                    move || {
                        let (seen, started) = &*vault;
                        drop(
                            started
                                .wait_timeout_while(
                                    seen.lock().unwrap(),
                                    std::time::Duration::from_secs(1),
                                    |seen| !seen.contains(&"sign-out"),
                                )
                                .unwrap(),
                        );
                        note(&vault, "vault");
                    },
                )],
                place(Class::Install),
            ),
        ];
        let alice = at_console(user("alice", 501));
        run(
            &Fake::root(),
            &alice,
            Base::none(),
            &groups,
            Mode::Apply,
            &mut NoJournal,
        )
        .unwrap();
        let seen = log.0.lock().unwrap().clone();
        let at = |what| seen.iter().position(|seen| *seen == what).unwrap();
        assert!(at("vault") < at("sign-out"), "{seen:?}");
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
                relayed: false,
            },
            Mode::Apply,
            &mut NoJournal,
            &mut Laps::start(),
            &Hooks {
                on_row: &on_row,
                desktop: &|stages| stages.plain(),
                held: &|_| None,
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
        let mut text: String = rows
            .iter()
            .filter(|row| shown(row))
            .map(|row| rendered(row, false))
            .collect();
        text.push_str(&completion_summary(rows, action, true));
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
        assert_eq!(
            quiet,
            "aterm init: 45 ok — Successfully completed. No further action is needed.\n"
        );
        rows[3] = ItemReport {
            from: Some("on".into()),
            to: Some("off".into()),
            ..report("init.test.changed", Outcome::Changed)
        };
        let changed = printed(&rows, Action::Apply);
        assert_eq!(changed.lines().count(), 3, "{changed}");
        assert!(
            changed.ends_with("aterm init: 44 ok \u{b7} 1 changed — Successfully completed. No further action is needed.\n"),
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

    #[test]
    fn unfinished_or_planned_setup_never_claims_successful_completion() {
        for outcome in [
            Outcome::Failed,
            Outcome::Unknown,
            Outcome::Blocked,
            Outcome::Person,
            Outcome::Planned,
        ] {
            let rows = [report("base.clt", outcome)];
            assert!(
                !completion_summary(&rows, Action::Apply, true).contains("Successfully completed")
            );
        }
        assert!(!completion_summary(&[], Action::Apply, true).contains("Successfully completed"));
        assert!(
            !completion_summary(&[report("base.clt", Outcome::Ok)], Action::Apply, false)
                .contains("Successfully completed")
        );
        assert!(
            completion_summary(&[report("base.clt", Outcome::Ok)], Action::Plan, true)
                .ends_with("(nothing changed)")
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
        assert_eq!(String::from_utf8(after).unwrap(), rendered(&changed, false));
    }

    /// `aterm init --help` and `aterm help init` read one page: every System State setting by
    /// its label, with the same automatic first-launch behavior.
    #[test]
    fn the_usage_names_every_setting_by_its_label() {
        let usage = usage();
        for setting in Setting::ALL {
            assert!(usage.contains(setting.label()), "{setting:?}: {usage}");
        }
    }

    /// First-run setup is on by default (owner ruling 2026-10-25): a person at an ALab Mac's
    /// console with no flag gets every group, the preferences and both sign-outs included, and
    /// is asked nothing; `--no-profile` and `--no-clone` leave theirs out. On another Mac a flag
    /// adds its group to the base plan.
    #[test]
    fn a_person_at_the_console_gets_every_group_and_is_asked_nothing() {
        let prepared_with = |picks: Picks, standing: Standing<'static>| {
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
                    keyboards: &KeyboardRead::EMPTY,
                    context_scope: Selection::all(),
                    only: &[],
                    picks,
                },
                &standing,
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
        let signs_out = |id: &String| crate::init_accounts::signs_out(id);
        let (preferences, picked, asked) = prepared_with(Picks::default(), alab_standing());
        assert_eq!((picked.runs, picked.held), (Picked::ALL.runs, None));
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
        for (picks, standing) in [
            (
                Picks {
                    named: sign_out,
                    left: Choice::NONE,
                },
                personal_standing(),
            ),
            (
                Picks {
                    named: Choice::NONE,
                    left: Choice {
                        profile: true,
                        clone: true,
                        ..Choice::NONE
                    },
                },
                alab_standing(),
            ),
        ] {
            let (preferences, picked, asked) = prepared_with(picks, standing);
            let mut expected = sign_out.selection();
            expected.set(Setting::Repositories, !picks.left.clone);
            assert_eq!(picked.runs, expected);
            assert!(asked.is_empty(), "{asked:?}");
            assert!(
                preferences.len() == 2 && preferences.iter().all(signs_out),
                "{preferences:?}"
            );
        }
    }

    /// A group the person turned off in System State, or every group while a Stop there holds,
    /// is left as it is by a plain `aterm init`, which says so and how to set it up anyway; a
    /// flag that names the group runs it.
    #[test]
    fn init_keeps_a_group_the_person_turned_off_and_names_the_way_to_run_it() {
        let mut off = Selection::all();
        off.set(Setting::Messages, false);
        off.set(Setting::FaceTime, false);
        let mut kept = Selection::none();
        kept.set(Setting::Messages, true);
        kept.set(Setting::FaceTime, true);
        let turned_off = Holds {
            off: kept,
            ..Holds::NONE
        };
        let plain = picked(Picks::default(), &[], &|| true, &turned_off, true);
        assert_eq!(plain.runs, off, "{plain:?}");
        assert!(!plain.signs_out());
        let line = plain.held.expect("the held line");
        assert!(
            line.contains("left as you set them in System State")
                && line.contains(
                    "aterm init --only with each one's word (init.messages, init.facetime)"
                ),
            "{line}"
        );
        let named = Picks {
            named: Choice {
                sign_out: true,
                ..Choice::NONE
            },
            left: Choice::NONE,
        };
        let flagged = picked(named, &[], &|| true, &turned_off, true);
        assert!(flagged.signs_out() && flagged.held.is_none(), "{flagged:?}");
        let stopped = Holds {
            stopped: Some("you stopped setup in System State".into()),
            stopped_revision: Some(1),
            ..Holds::NONE
        };
        let plain = picked(Picks::default(), &[], &|| true, &stopped, true);
        assert!(plain.runs.is_empty(), "{plain:?}");
        let line = plain.held.expect("the held line");
        assert!(
            line.starts_with("left as they are because you stopped setup in System State"),
            "{line}"
        );
        let only = picked(
            Picks::default(),
            &["init.dock".into()],
            &|| true,
            &stopped,
            true,
        );
        assert_eq!(
            (only.runs, only.held, only.kept),
            (Picked::ALL.runs, None, Selection::none()),
            "--only runs what it names"
        );
        assert_eq!(only.start, stopped, "over the holds on record as it began");
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
        fn hold(&self, pass: Pass, _wait: Option<&dyn Fn(&str)>) -> Result<Box<dyn Held>, Unheld> {
            self.holds.lock().unwrap().push(pass.whole());
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
            Ok((
                groups(ctx),
                picked(
                    reading.picks,
                    reading.only,
                    &|| true,
                    &Holds::default(),
                    true,
                ),
            ))
        };
        converge(
            sys,
            options,
            &Front {
                base: &|_| Base::none(),
                groups: &prepare,
                full_disk_access: &|| sys.full_disk_access(),
                targo: None,
                cwd: None,
                records,
                on_row: &|_| {},
                present: &|| true,
                on_held: &|_| {},
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
            fn hold(
                &self,
                _pass: Pass,
                _wait: Option<&dyn Fn(&str)>,
            ) -> Result<Box<dyn Held>, Unheld> {
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
            |_: &dyn System, ctx: &Ctx, _: Mode, _: &Reading<'_>| Ok((groups(ctx), Picked::ALL));
        let sys = alice_with_checkout(501, None);
        let rows = converge(
            &sys,
            &Options::default(),
            &Front {
                base: &base,
                groups: &prepare,
                full_disk_access: &|| sys.full_disk_access(),
                targo: None,
                cwd: None,
                records: &records,
                on_row: &|_| {},
                present: &|| true,
                on_held: &|_| {},
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
        // On an ALab Mac `--no-sign-out` runs the profile without the sign-outs: a part.
        let part = Options {
            picks: Picks {
                named: Choice::NONE,
                left: Choice {
                    sign_out: true,
                    ..Choice::NONE
                },
            },
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
            (&part, &dock, [false], 0),
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
        fn hold(&self, _pass: Pass, _wait: Option<&dyn Fn(&str)>) -> Result<Box<dyn Held>, Unheld> {
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
        assert!(options.workstation());
        let sys = alice_with_checkout(501, None);
        let groups = |ctx: &Ctx| {
            vec![
                (
                    vec![Listed::boxed("init.test.dock", vec![change(), ok()])],
                    place(Class::Preference),
                ),
                (
                    crate::init_spotlight::items(ctx, crate::init_spotlight::Projects::Noted)
                        .unwrap(),
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
                crate::init_spotlight::Projects::Noted,
                crate::init_secrets::Secrets {
                    items: Vec::new(),
                    waited_on: None,
                },
                Vec::new(),
                &KeyboardRead::EMPTY,
                Selection::all(),
                &only,
                true,
                None,
            )
            .0;
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

    /// Explicit setup also runs from an agent's tool call in the console owner's session.
    #[test]
    fn sign_out_runs_without_a_terminal_including_from_an_agent_tab() {
        let options = Options {
            picks: Picks {
                named: Choice {
                    sign_out: true,
                    ..Choice::NONE
                },
                left: Choice::NONE,
            },
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
            (false, Some("Claude Code"), true),
            (false, Some("Codex"), true),
            (true, Some("Claude Code"), true),
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
            crate::init_spotlight::Projects::Unread,
            crate::init_secrets::Secrets {
                items: Vec::new(),
                waited_on: None,
            },
            Vec::new(),
            &KeyboardRead::EMPTY,
            Choice {
                sign_out: true,
                ..Choice::NONE
            }
            .selection(),
            &[],
            true,
            None,
        )
        .0;
        let ids: Vec<String> = accounts
            .iter()
            .flat_map(|(items, _)| items.iter().map(|item| item.id()))
            .collect();
        assert_eq!(
            ids,
            [
                AWAY,
                "init.spotlight.rustup-profile@alice",
                "init.spotlight.rust-docs@alice",
                "init.spotlight.build-noindex@alice",
                "init.messages.signed-out@alice",
                "init.facetime.signed-out@alice"
            ],
            "the preferences wait for a person at the console, Rust in Spotlight runs as a \
             background row, and the sign-outs run last"
        );
        let preferences = Options {
            picks: Picks {
                named: Choice {
                    profile: true,
                    ..Choice::NONE
                },
                left: Choice::NONE,
            },
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
            crate::init_spotlight::Projects::Noted,
            crate::init_secrets::Secrets {
                items: Vec::new(),
                waited_on: None,
            },
            github,
            &KeyboardRead::EMPTY,
            Selection::all(),
            &[],
            true,
            None,
        )
        .0;
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
        // `--check` reads the grant as not in place, and as a permission, not a failure.
        assert_eq!(status(&reported, Action::Check), 77, "{reported:?}");
        // CONTROL: the same rows with one that failed read as a failure under `--check`.
        let mut failed = reported.clone();
        failed.push(ItemReport {
            id: "base.failed".into(),
            outcome: Outcome::Failed,
            ..person.clone()
        });
        assert_eq!(status(&failed, Action::Check), 1, "{failed:?}");
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

    /// What one run of the Mac Setup tab's program did: its exit code, everything it
    /// wrote to the tab, and how many times it converged.
    struct TabRun {
        code: u8,
        said: Vec<String>,
        converged: u32,
    }

    const TAB_ATERM: &str = "/Applications/aterm.app/Contents/MacOS/aterm";

    const TAB_CC: &str = "/Library/Developer/CommandLineTools/usr/bin/cc";

    /// The `sudo` argv on `alice_with_checkout`, in the tab and outside it: `-k`, the plain prompt
    /// and the root half's `--plain`, which reads no file.
    fn tab_sudo() -> String {
        format!("tty /usr/bin/sudo -k -p 'Mac password: ' {TAB_ATERM} machines here apply --plain")
    }

    /// [`tab`] over `sys` (`live`: in the person's own login), composing `base.clt` alone as
    /// [`tab_entry`] does, its probe finding [`TAB_CC`] or nothing.
    fn tab_run(sys: &Fake, live: bool) -> TabRun {
        let said = Arc::new(Mutex::new(Vec::new()));
        let out = crate::init_clt::Tab::new({
            let said = Arc::clone(&said);
            Arc::new(move |text: &str| said.lock().unwrap().push(text.to_owned()))
        });
        let converged = std::sync::atomic::AtomicU32::new(0);
        let only = options(&["--only".into(), crate::init_clt::ID.into()]).unwrap();
        let code = tab(sys, &out, &|| live, &|tab| {
            converged.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let base = |_: bool| Base {
                clt: Some(
                    crate::init_clt::CommandLineTools::new(
                        Box::new(|sys: &dyn System| {
                            if sys.read_file(Path::new(TAB_CC)).is_ok() {
                                crate::init_clt::Found::Present(TAB_CC.into())
                            } else {
                                crate::init_clt::Found::Missing("no developer directory".into())
                            }
                        }),
                        PathBuf::from(TAB_ATERM),
                    )
                    .in_tab(tab.clone()),
                ),
                ..Base::none()
            };
            let groups =
                |_: &dyn System, _: &Ctx, _: Mode, _: &Reading<'_>| Ok((Vec::new(), Picked::NONE));
            converge(
                sys,
                &only,
                &Front {
                    base: &base,
                    groups: &groups,
                    full_disk_access: &|| sys.full_disk_access(),
                    targo: None,
                    cwd: Some(Path::new("/Users//alice/aterm")),
                    records: &NoRecords,
                    on_row: &|_| {},
                    present: &|| true,
                    on_held: &|_| {},
                },
                &mut NoJournal,
                &mut Laps::start(),
            )
        });
        let said = said.lock().unwrap().clone();
        TabRun {
            code,
            said,
            converged: converged.load(std::sync::atomic::Ordering::SeqCst),
        }
    }

    /// The tab's run is `--only base.clt` alone under the tab's variable; without the variable
    /// (or with it empty), or with any other word or flag, `aterm init` speaks as it always did.
    #[test]
    fn only_base_clt_under_the_tab_variable_is_the_tabs_program() {
        let tab_var = Some(std::ffi::OsStr::new("1"));
        let parse =
            |args: &[&str]| options(&args.iter().map(OsString::from).collect::<Vec<_>>()).unwrap();
        assert!(in_tab(tab_var, &parse(&["--only", "base.clt"])));
        assert!(in_tab(tab_var, &parse(&["--only=base.clt"])));
        assert!(!in_tab(None, &parse(&["--only", "base.clt"])));
        assert!(!in_tab(
            Some(std::ffi::OsStr::new("")),
            &parse(&["--only", "base.clt"])
        ));
        for other in [
            &["--only", "base.clt,shell"][..],
            &["--only", "base"],
            &["--only", "base.clt", "--json"],
            &["--plan", "--only", "base.clt"],
            &[],
        ] {
            assert!(!in_tab(tab_var, &parse(other)), "{other:?}");
        }
    }

    /// `aterm init --only base.clt` under the tab's variable is the tab's own run, whose code is
    /// the run's exit: none of `aterm init`'s ports (its base layer, Full Disk Access, the
    /// records, the grants, the walk), which here panic, is touched first.
    /// NEGATIVE CONTROL: an entry that never hands the tab its run composes init's base layer,
    /// whose port panics, and never exits 7.
    #[test]
    fn the_tabs_variable_hands_the_run_to_the_tab() {
        struct NoRecords;
        impl Records for NoRecords {
            fn hold(
                &self,
                _pass: Pass,
                _wait: Option<&dyn Fn(&str)>,
            ) -> Result<Box<dyn Held>, Unheld> {
                panic!("the tab's run held the setup records first")
            }
        }
        let scratch = aterm_tempfile::tempdir().unwrap();
        let ports = crate::sync::tests::Inert::new(scratch.path());
        let host = Host {
            decided: Decider::Person,
            base: &|_| panic!("the tab's run composed init's base layer first"),
            full_disk_access: &|| panic!("the tab's run measured Full Disk Access"),
            records: &NoRecords,
            grants: |_| panic!("the tab's run read the grants"),
            workstation_check: &|_| panic!("the tab's run read workstation observations"),
            walk: &|_| panic!("the tab's run walked the grants"),
            converge: &ports,
            alab: &|| panic!("the tab's run read the ALab scope first"),
            present: || panic!("the tab's run read who is at the screen first"),
        };
        let ran = std::cell::Cell::new(0);
        let code = entry(
            vec!["--only".into(), "base.clt".into()],
            &host,
            Some(std::ffi::OsStr::new("1")),
            &|options, _| {
                ran.set(ran.get() + 1);
                assert_eq!(options.only, [crate::init_clt::ID]);
                7
            },
        );
        assert_eq!(format!("{code:?}"), format!("{:?}", ExitCode::from(7)));
        assert_eq!(ran.get(), 1);
    }

    /// The tab's program starts nothing and says nothing until the window types its go line
    /// (read with echo off, keeping what was typed before the read), and ends on 2, so a tab
    /// whose go line never came is never taken for an install that ran.
    #[test]
    fn the_tab_starts_nothing_and_says_nothing_until_its_go_line() {
        for answer in [
            Some(Some("installx")),
            Some(Some("Install")),
            Some(Some("")),
            Some(None),
            None,
        ] {
            let mut sys = alice_with_checkout(501, None);
            sys.tty = true;
            if let Some(answer) = answer {
                sys.answer(answer);
            }
            let before = sys.calls();
            let run = tab_run(&sys, true);
            assert_eq!(run.code, TAB_NOT_STARTED, "{answer:?}");
            assert!(run.said.is_empty(), "{answer:?}: {:?}", run.said);
            assert_eq!(run.converged, 0, "{answer:?}");
            let calls = sys.calls();
            assert_eq!(
                calls[before.len()..],
                ["ask kept ".to_owned()],
                "{answer:?}: one read with echo off, then nothing"
            );
        }
    }

    /// Past its go line, the tab's program clears the screen, says its one intro line right
    /// before `sudo` asks for the Mac password with the plain prompt, runs the root half
    /// `--plain`, and ends on its one installed line and 0. Tools already there end on the same
    /// line with nothing asked.
    #[test]
    fn the_tab_installs_and_ends_on_its_one_installed_line() {
        let mut sys = alice_with_checkout(501, None);
        sys.running(&[]);
        sys.tty = true;
        sys.answer(Some(aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_GO));
        let sudo = tab_sudo();
        sys.on(&sudo, Ran::ok(""))
            .on_write(&sudo, Path::new(TAB_CC), b"");
        let run = tab_run(&sys, true);
        assert_eq!(
            run.said,
            [
                TAB_CLEAR.to_owned(),
                format!("{}\n", crate::init_clt::TAB_INTRO),
                format!("{}\n", crate::init_clt::TAB_INSTALLED),
            ],
            "{:?}",
            sys.calls()
        );
        assert_eq!(run.code, 0);
        assert_eq!(run.converged, 1);
        assert_eq!(
            sys.calls()
                .iter()
                .filter(|call| call.starts_with("tty "))
                .collect::<Vec<_>>(),
            [&sudo]
        );

        let mut there = alice_with_checkout(501, None);
        there.tty = true;
        there
            .answer(Some(aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_GO))
            .file(TAB_CC, b"");
        let run = tab_run(&there, true);
        assert_eq!(
            run.said,
            [
                TAB_CLEAR.to_owned(),
                format!("{}\n", crate::init_clt::TAB_INSTALLED),
            ]
        );
        assert_eq!(run.code, 0);
        assert!(!there.calls().iter().any(|call| call.starts_with("tty ")));
    }

    /// Every way the tab's install falls short, early ones included, ends on its one plain line
    /// `Mac Setup did not finish: <reason>.` and
    /// its class's exit (1, or a stop's and Apple's window closed's own): `sudo`'s own refusal, each
    /// class the root half exits with, a signal, a `sudo` that never ran, a run outside the
    /// person's own login, and a refused converge.
    #[test]
    fn the_tab_ends_on_one_line_saying_why_the_install_did_not_finish() {
        use crate::init_clt::Unfinished;
        let go = aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_GO;
        let ends_on = |run: &TabRun, why: Unfinished, intro: bool| {
            let mut said = vec![TAB_CLEAR.to_owned()];
            if intro {
                said.push(format!("{}\n", crate::init_clt::TAB_INTRO));
            }
            said.push(format!("{}\n", why.line()));
            assert_eq!(run.said, said, "{why:?}");
            assert_eq!(run.code, why.tab_exit(), "{why:?}");
        };
        let sudo = tab_sudo();
        for (ran, why) in [
            (Some(Ran::exit(1, "")), Unfinished::Password),
            (Some(Ran::exit(80, "")), Unfinished::Catalog),
            (Some(Ran::exit(81, "")), Unfinished::NoneOffered),
            (Some(Ran::exit(82, "")), Unfinished::Download),
            (Some(Ran::exit(83, "")), Unfinished::Installer),
            (Some(Ran::exit(84, "")), Unfinished::Busy),
            (Some(Ran::exit(85, "")), Unfinished::Stopped),
            (Some(Ran::exit(86, "")), Unfinished::AppleWindow),
            (Some(Ran::exit(87, "")), Unfinished::Other),
            (Some(Ran::exit(88, "")), Unfinished::WindowClosed),
            (Some(Ran::exit(127, "")), Unfinished::Other),
            (
                Some(Ran {
                    status: None,
                    ..Ran::default()
                }),
                Unfinished::Other,
            ),
            // Exit 0 with nothing installed after it.
            (Some(Ran::ok("")), Unfinished::Other),
            // `sudo` never ran.
            (None, Unfinished::Other),
        ] {
            let mut sys = alice_with_checkout(501, None);
            sys.running(&[]);
            sys.tty = true;
            sys.answer(Some(go));
            if let Some(ran) = ran {
                sys.on(&sudo, ran);
            }
            ends_on(&tab_run(&sys, true), why, true);
        }

        // Outside the person's own login nothing is converged.
        let mut sys = alice_with_checkout(501, None);
        sys.tty = true;
        sys.answer(Some(go));
        let run = tab_run(&sys, false);
        ends_on(&run, Unfinished::NotOwnLogin, false);
        assert_eq!(run.converged, 0);

        // A converge refused before the item (run as root) is the other class.
        let mut root = alice_with_checkout(501, None);
        root.tty = true;
        root.euid = 0;
        root.answer(Some(go));
        let run = tab_run(&root, true);
        ends_on(&run, Unfinished::Other, false);
        assert_eq!(run.converged, 1);

        // So is a probe with no terminal to ask in.
        let sys = alice_with_checkout(501, None);
        sys.running(&[]);
        sys.answer(Some(go));
        ends_on(&tab_run(&sys, true), Unfinished::Other, false);
    }

    /// In Low Data Mode the tab asks for no password and ends on its own line, naming Low Data
    /// Mode and how to go on, with its own exit, which the window reads to wait for the mode to
    /// end without spending its one automatic opening. An account that is no administrator of
    /// the Mac, which `sudo` would refuse, ends on its own line and exit too.
    #[test]
    fn low_data_mode_and_a_non_administrator_each_end_the_tab_on_their_own_line_and_exit() {
        use crate::init_clt::Unfinished;
        for (why, code, line) in [
            (
                Unfinished::LowData,
                aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_LOW_DATA,
                crate::init_clt::TAB_LOW_DATA,
            ),
            (
                Unfinished::NotAdmin,
                aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_NOT_ADMIN,
                crate::init_clt::TAB_NOT_ADMIN,
            ),
        ] {
            let mut sys = alice_with_checkout(501, None);
            sys.running(&[]);
            sys.tty = true;
            match why {
                Unfinished::LowData => sys.low_data = Some(true),
                _ => sys.admin = Some(false),
            }
            sys.answer(Some(aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_GO));
            let run = tab_run(&sys, true);
            assert_eq!(run.code, code, "{why:?}");
            assert_eq!(why.tab_exit(), code, "{why:?}");
            assert_eq!(
                run.said,
                [TAB_CLEAR.to_owned(), format!("{line}\n")],
                "{why:?}"
            );
            assert!(
                !sys.calls().iter().any(|call| call.starts_with("tty ")),
                "{why:?}: no password asked: {:?}",
                sys.calls()
            );
        }
    }

    /// The tab's program (`--only base.clt`) composes init's base layer as a read: Apple's
    /// Command Line Tools touch no store, so the tab never waits for an install pass that holds
    /// it before its password prompt. CONTROL: any other run that applies waits for the lock,
    /// and `--no-wait` only tries it.
    #[test]
    fn the_tab_never_asks_for_the_store_lock() {
        let parse =
            |args: &[&str]| options(&args.iter().map(OsString::from).collect::<Vec<_>>()).unwrap();
        assert_eq!(
            parse(&["--only", "base.clt"]).base_lock(true),
            BaseLock::Read
        );
        assert_eq!(parse(&["--only=base.clt"]).base_lock(true), BaseLock::Read);
        assert_eq!(parse(&[]).base_lock(true), BaseLock::Wait);
        assert_eq!(parse(&["--only", "base"]).base_lock(true), BaseLock::Wait);
        assert_eq!(
            parse(&["--only", "base.clt,base.pkg.toolset"]).base_lock(true),
            BaseLock::Wait
        );
        assert_eq!(parse(&["--no-wait"]).base_lock(true), BaseLock::Try);
        assert_eq!(parse(&[]).base_lock(false), BaseLock::Read);
    }

    /// `base.clt` with the tools missing, run over `sys` in its login's own context.
    fn clt_rows(sys: &Fake) -> Vec<ItemReport> {
        let (ctx, _) = local_context(sys).unwrap();
        let base = Base {
            clt: Some(
                crate::init_clt::CommandLineTools::new(
                    Box::new(|_: &dyn System| {
                        crate::init_clt::Found::Missing("no developer directory".into())
                    }),
                    PathBuf::from(TAB_ATERM),
                )
                .saying(Arc::new(|_: &str| {})),
            ),
            ..Base::none()
        };
        run(sys, &ctx, base, &[], Mode::Apply, &mut NoJournal).unwrap()
    }

    /// Who may be asked for the Mac password: installing Apple's Command Line Tools is a
    /// person's step at this Mac's console (design §6.3, §7). In an agent's tab, or with no
    /// terminal at all (an agent's tool call, a script), nothing asks for it, the row waits for
    /// a person there, and the run exits 0. CONTROL: a person at the console is asked once.
    #[test]
    fn off_the_console_init_asks_no_password_for_the_command_line_tools_and_exits_0() {
        let sudo = tab_sudo();
        for (terminal, agent) in [
            (true, Some("Claude Code")),
            (false, Some("Claude Code")),
            (false, None),
        ] {
            let mut sys = alice_at(501);
            sys.running(&[]);
            sys.terminal = terminal;
            sys.tty = terminal;
            sys.agent = agent;
            assert_ne!(crate::console::presence(&sys), Presence::AtConsole);
            let rows = clt_rows(&sys);
            let row = rows
                .iter()
                .find(|row| row.id == crate::init_clt::ID)
                .unwrap();
            assert!(
                !sys.calls().iter().any(|call| call.starts_with("tty ")),
                "{terminal} {agent:?}: asked for the Mac password: {:?}",
                sys.calls()
            );
            assert_eq!(row.outcome, Outcome::Deferred, "{row:?}");
            assert_eq!(status(&rows, Action::Apply), 0, "{row:?}");
            assert_eq!(status(&rows, Action::Check), 77, "{row:?}");
        }

        let mut sys = alice_at(501);
        sys.terminal = true;
        sys.tty = true;
        sys.agent = None;
        assert_eq!(init_presence(&sys), Presence::AtConsole);
        sys.running(&[]);
        sys.on(&sudo, Ran::exit(1, ""));
        let rows = clt_rows(&sys);
        assert_eq!(
            sys.calls()
                .iter()
                .filter(|call| call.starts_with("tty "))
                .collect::<Vec<_>>(),
            [&sudo]
        );
        assert_ne!(status(&rows, Action::Apply), 0);
    }

    /// `aterm init` at the console while an install of the tools runs (System State's tab, or
    /// another account's) asks for no password and fails nothing: the row waits for that
    /// install, and the run exits 0. CONTROL: a placeholder nothing holds is no install.
    #[test]
    fn during_a_running_clt_install_init_asks_no_password_and_fails_nothing() {
        let console = || {
            let mut sys = alice_at(501);
            sys.terminal = true;
            sys.tty = true;
            sys.agent = None;
            sys.file(crate::items::developer::PLACEHOLDER, b"");
            sys
        };
        let sys = console();
        sys.running(&["/usr/sbin/softwareupdate"]);
        let rows = clt_rows(&sys);
        let row = rows
            .iter()
            .find(|row| row.id == crate::init_clt::ID)
            .unwrap();
        assert!(
            !sys.calls().iter().any(|call| call.starts_with("tty ")),
            "an install is running: no password asked: {:?}",
            sys.calls()
        );
        assert_eq!(row.outcome, Outcome::Deferred, "{row:?}");
        assert!(!row.outcome.is_failure(), "{row:?}");
        assert_eq!(status(&rows, Action::Apply), 0, "{row:?}");

        let stale = console();
        stale.running(&["/tmp/installer"]);
        let _ = clt_rows(&stale);
        assert!(
            stale.calls().iter().any(|call| call.starts_with("tty ")),
            "{:?}",
            stale.calls()
        );
    }

    /// One converge of `sys` with `prepare`'s groups and `records`, a person at the screen as
    /// `present` says: its rows (or why it was refused), and the lines of what the person's
    /// choices held ([`Front::on_held`]).
    fn converge_held(
        sys: &Fake,
        options: &Options,
        prepare: &Prepare<'_>,
        records: &dyn Records,
        present: &(dyn Fn() -> bool + Sync),
    ) -> (Result<Vec<ItemReport>, Refused>, Vec<ItemReport>) {
        let held = Mutex::new(Vec::new());
        let rows = converge(
            sys,
            options,
            &Front {
                base: &|_| Base::none(),
                groups: prepare,
                full_disk_access: &|| sys.full_disk_access(),
                targo: None,
                cwd: None,
                records,
                on_row: &|_| {},
                present,
                on_held: &|row| held.lock().unwrap().push(row),
            },
            &mut NoJournal,
            &mut Laps::start(),
        );
        (rows, held.into_inner().unwrap())
    }

    /// [`desktop_rows`]'s pair: the items, and each one's tag.
    type DesktopRows = (Vec<Box<dyn Item>>, Vec<(String, Setting)>);

    /// The sign-outs for `ctx`'s one user and a Dock row, each a change, tagged with the System
    /// State row each is built for ([`Picked::rows`]).
    fn desktop_rows(ctx: &Ctx) -> DesktopRows {
        let mut items: Vec<Box<dyn Item>> = crate::init_accounts::items(ctx)
            .unwrap()
            .into_iter()
            .map(|item| Listed::boxed(item.id(), vec![change(), ok()]))
            .collect();
        items.push(Listed::boxed("init.test.dock", vec![change(), ok()]));
        let tags = items
            .iter()
            .map(|item| {
                let id = item.id();
                let setting = if id.starts_with("init.messages.") {
                    Setting::Messages
                } else if id.starts_with("init.facetime.") {
                    Setting::FaceTime
                } else {
                    Setting::Dock
                };
                (id, setting)
            })
            .collect();
        (items, tags)
    }

    /// System State's stopped revision before and after init waits for the window's pass.
    struct StoppedWhileWaiting {
        waited: std::sync::atomic::AtomicBool,
        before: Option<u64>,
        after: u64,
    }

    impl Records for StoppedWhileWaiting {
        fn hold(&self, _pass: Pass, wait: Option<&dyn Fn(&str)>) -> Result<Box<dyn Held>, Unheld> {
            if let Some(wait) = wait {
                wait("waiting while System State changes this Mac's settings");
            }
            self.waited.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(Box::new(NoRecords))
        }
        fn holds(&self) -> Holds {
            let stopped_revision = if self.waited.load(std::sync::atomic::Ordering::SeqCst) {
                Some(self.after)
            } else {
                self.before
            };
            Holds {
                stopped: stopped_revision.map(|_| "you stopped setup in System State".to_owned()),
                stopped_revision,
                ..Holds::NONE
            }
        }
    }

    /// A Stop the person presses in System State while `aterm init` waits for the records holds
    /// init's preferences and sign-outs, as a Stop pressed before init started does (principle
    /// 4): the holds are read again once the records are held, and the run says what it left.
    #[test]
    fn a_stop_pressed_while_init_waits_for_the_records_holds_its_desktop_lane() {
        let records = StoppedWhileWaiting {
            waited: std::sync::atomic::AtomicBool::new(false),
            before: None,
            after: 1,
        };
        let prepare = |_: &dyn System, ctx: &Ctx, _: Mode, reading: &Reading<'_>| {
            let (items, tags) = desktop_rows(ctx);
            let mut picked = picked(
                reading.picks,
                reading.only,
                &|| true,
                &records.holds(),
                true,
            );
            picked.rows = tags;
            Ok((vec![(items, place(Class::Preference))], picked))
        };
        let sys = alice_with_checkout(501, None);
        let (rows, held) = converge_held(&sys, &Options::default(), &prepare, &records, &|| true);
        let rows = rows.unwrap();
        let changed: Vec<&str> = rows
            .iter()
            .filter(|row| row.outcome == Outcome::Changed)
            .map(|row| row.id.as_str())
            .collect();
        assert!(
            changed.is_empty(),
            "the person's Stop landed before init changed anything, yet init changed {changed:?}"
        );
        let line = held
            .iter()
            .find(|row| row.id == HELD)
            .and_then(|row| row.why.clone())
            .expect("the held line");
        assert!(
            line.starts_with("left as they are because you stopped setup in System State")
                && line.contains("Dock")
                && line.contains("Messages"),
            "{line}"
        );
    }

    /// System State's records, stopped once `stop` is set (a Stop pressed in System State).
    struct StoppedLater(Arc<std::sync::atomic::AtomicBool>);

    impl Records for StoppedLater {
        fn hold(&self, _pass: Pass, _wait: Option<&dyn Fn(&str)>) -> Result<Box<dyn Held>, Unheld> {
            Ok(Box::new(NoRecords))
        }
        fn holds(&self) -> Holds {
            if self.0.load(std::sync::atomic::Ordering::SeqCst) {
                Holds {
                    stopped: Some("you stopped setup in System State".to_owned()),
                    stopped_revision: Some(1),
                    ..Holds::NONE
                }
            } else {
                Holds::NONE
            }
        }
    }

    /// A GitHub batch the person stops after a clone ran its hook installer: its row says their
    /// words and what the installer did, both printed live and returned, and is not deferred as
    /// a clone owed until this Mac is let into the vault, which it is not waiting for.
    /// NEGATIVE CONTROL: deferring every waiting row as owed answers `OWED` here, the person's
    /// words and the installer's line gone.
    #[test]
    fn a_late_stop_keeps_the_persons_words_and_the_installer_line_on_the_github_row() {
        use std::sync::atomic::{AtomicBool, Ordering};
        struct Batch(Arc<AtomicBool>);
        impl Item for Batch {
            fn id(&self) -> String {
                "init.github.batch@alice".into()
            }
            fn family(&self) -> &'static str {
                "github"
            }
            fn title(&self) -> String {
                self.id()
            }
            fn probe(&self, _: &dyn System, _: &Ctx) -> Probe {
                change()
            }
            fn apply(&self, _: &dyn System, _: &Ctx) -> Result<(), String> {
                panic!("the wrapper forwards its hold into the batch")
            }
            fn apply_guarded(
                &self,
                _: &dyn System,
                _: &Ctx,
                current: &(dyn Fn() -> Result<(), String> + Sync),
            ) -> Result<(), String> {
                current()?;
                // The clone is published and its installer ran; the person presses Stop.
                self.0.store(true, Ordering::SeqCst);
                current().map_err(|why| {
                    format!("{why}; $HOME/trust: ran its scripts/install-git-hooks.sh: installed")
                })
            }
        }
        let stop = Arc::new(AtomicBool::new(false));
        let records = StoppedLater(Arc::clone(&stop));
        let batch = Arc::clone(&stop);
        let prepare = move |_: &dyn System, ctx: &Ctx, _: Mode, reading: &Reading<'_>| {
            let picked = picked(reading.picks, reading.only, &|| true, &Holds::NONE, true);
            let items: Vec<Box<dyn Item>> = vec![Box::new(Batch(Arc::clone(&batch)))];
            let _ = ctx;
            Ok((vec![(items, anywhere(Class::Install))], picked))
        };
        let sys = alice_with_checkout(501, None);
        let printed = Mutex::new(Vec::new());
        let rows = converge(
            &sys,
            &Options::default(),
            &Front {
                base: &|_| Base::none(),
                groups: &prepare,
                full_disk_access: &|| sys.full_disk_access(),
                targo: None,
                cwd: None,
                records: &records,
                on_row: &|row| printed.lock().unwrap().push(row.clone()),
                present: &|| true,
                on_held: &|_| {},
            },
            &mut NoJournal,
            &mut Laps::start(),
        )
        .unwrap();
        assert!(stop.load(Ordering::SeqCst), "the batch ran");
        let printed = printed.into_inner().unwrap();
        for rows in [&rows, &printed] {
            let row = rows
                .iter()
                .find(|row| row.id == "init.github.batch@alice")
                .expect("the batch's row");
            assert_eq!(row.outcome, Outcome::Blocked, "{row:?}");
            assert_eq!(
                row.why.as_deref(),
                Some(
                    "left as it is because you stopped setup in System State; $HOME/trust: ran its \
                     scripts/install-git-hooks.sh: installed"
                ),
                "{row:?}"
            );
        }
    }

    /// System State's records as init holds them: `taken` once held, `held` while held, as the
    /// profile lock is.
    struct Watched {
        taken: Arc<std::sync::atomic::AtomicBool>,
        held: Arc<std::sync::atomic::AtomicBool>,
    }

    impl Records for Watched {
        fn hold(&self, _pass: Pass, _wait: Option<&dyn Fn(&str)>) -> Result<Box<dyn Held>, Unheld> {
            self.held.store(true, std::sync::atomic::Ordering::SeqCst);
            self.taken.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(Box::new(Releases(Arc::clone(&self.held))))
        }
    }

    /// The hold [`Watched`] hands out: it lets the records go when released or dropped.
    struct Releases(Arc<std::sync::atomic::AtomicBool>);

    impl Held for Releases {
        fn verified(self: Box<Self>) -> Result<(), String> {
            Ok(())
        }
        fn release(&mut self) {
            self.0.store(false, std::sync::atomic::Ordering::SeqCst);
        }
        fn again(&mut self, _wait: Option<&dyn Fn(&str)>) -> Result<(), Unheld> {
            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
    }

    impl Drop for Releases {
        fn drop(&mut self) {
            self.0.store(false, std::sync::atomic::Ordering::SeqCst);
        }
    }

    /// init holds System State's records (profile.lock) only over its Desktop lane's own
    /// changes: the preferences, then the sign-outs, which run last. While its clones (or the
    /// Command Line Tools) run in the Network lane, the window's Stop, a group turned off,
    /// Restore and Set up now never wait behind it. Here the clone waits (a one-minute hang
    /// detector) for the records to be let go after the Dock changed.
    #[test]
    fn init_lets_system_states_records_go_while_its_clones_run() {
        use std::sync::atomic::Ordering;
        let (taken, held) = (
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        );
        let saw = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let records = Watched {
            taken: Arc::clone(&taken),
            held: Arc::clone(&held),
        };
        let clone = {
            let (taken, held, saw) = (Arc::clone(&taken), Arc::clone(&held), Arc::clone(&saw));
            move || {
                let start = std::time::Instant::now();
                while start.elapsed() < std::time::Duration::from_secs(60) {
                    if taken.load(Ordering::SeqCst) && !held.load(Ordering::SeqCst) {
                        saw.store(true, Ordering::SeqCst);
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
            }
        };
        let prepare = move |_: &dyn System, ctx: &Ctx, _: Mode, _: &Reading<'_>| {
            let (items, _) = desktop_rows(ctx);
            Ok((
                vec![
                    (items, place(Class::Preference)),
                    (
                        vec![then("init.test.clone", "github", clone.clone())],
                        place(Class::Install),
                    ),
                ],
                Picked::ALL,
            ))
        };
        let sys = alice_with_checkout(501, None);
        let (rows, _) = converge_held(&sys, &Options::default(), &prepare, &records, &|| true);
        let rows = rows.unwrap();
        assert!(
            saw.load(Ordering::SeqCst),
            "init kept System State's records held for the whole minute its clone ran"
        );
        assert!(
            rows.iter()
                .filter(|row| crate::init_accounts::signs_out(&row.id))
                .all(|row| row.outcome == Outcome::Changed),
            "the sign-outs ran, under a fresh hold: {rows:?}"
        );
        assert!(
            !held.load(Ordering::SeqCst),
            "the records are let go at the end"
        );
    }

    /// With no terminal, the Messages and FaceTime sign-outs, which nothing undoes, run only
    /// while a person is at the screen. They run last, after every other lane (the clones
    /// included), so the person who was there as the run began may have gone: the sign-outs
    /// read the screen again when they start, and wait for the next `aterm init` if nobody is
    /// there (exit 0, nothing failed). Control: a person still there gets them.
    #[test]
    fn with_no_terminal_the_sign_outs_read_the_person_again_when_they_start() {
        use std::sync::atomic::Ordering;
        for walks_away in [true, false] {
            let here = Arc::new(std::sync::atomic::AtomicBool::new(true));
            let mut sys = alice_at(501);
            sys.terminal = false;
            sys.agent = None;
            assert_eq!(crate::console::presence(&sys), Presence::NoTerminal);
            let gone = Arc::clone(&here);
            let prepare = move |_: &dyn System, ctx: &Ctx, _: Mode, _: &Reading<'_>| {
                let gone = Arc::clone(&gone);
                let items: Vec<Box<dyn Item>> = crate::init_accounts::items(ctx)
                    .unwrap()
                    .into_iter()
                    .map(|item| Listed::boxed(item.id(), vec![change(), ok()]))
                    .collect();
                // A clone that takes a while; the person walks away meanwhile.
                let clone = then("init.test.clone", "github", move || {
                    if walks_away {
                        gone.store(false, Ordering::SeqCst);
                    }
                });
                Ok((
                    vec![
                        (items, place(Class::Preference)),
                        (vec![clone], anywhere(Class::Install)),
                    ],
                    Picked::ALL,
                ))
            };
            let present = || here.load(Ordering::SeqCst);
            let (rows, _) =
                converge_held(&sys, &Options::default(), &prepare, &NoRecords, &present);
            let rows = rows.unwrap();
            let sign_outs: Vec<&ItemReport> = rows
                .iter()
                .filter(|row| crate::init_accounts::signs_out(&row.id))
                .collect();
            assert_eq!(sign_outs.len(), 2, "{rows:?}");
            assert!(
                sign_outs.iter().all(|row| row.outcome == Outcome::Changed),
                "walks_away={walks_away}: {sign_outs:#?}"
            );
        }
    }

    /// The held line names a working command for both a console terminal and an agent tool.
    #[test]
    fn the_held_line_off_the_console_names_a_way_back_that_runs_there() {
        for (agent, flags) in [(Some("Claude Code"), true), (None, true)] {
            let mut sys = alice_at(501);
            sys.terminal = true;
            sys.agent = agent;
            let (ctx, _) = local_context(&sys).unwrap();
            let mut off = Selection::none();
            off.set(Setting::Dock, true);
            let standing = Standing {
                decided: Decider::Person,
                alab: &|| true,
                present: &|| true,
                holds: Holds { off, ..Holds::NONE },
                workstation_observation: None,
            };
            let vault = crate::init_secrets::tests::Scripted::new(vec![Ok(
                crate::init_secrets::tests::unconfigured(),
            )]);
            let (_, picked) = prepare_groups_with(
                &sys,
                &ctx,
                Mode::Apply,
                None,
                &Reading {
                    keyboards: &KeyboardRead::EMPTY,
                    context_scope: Selection::all(),
                    only: &[],
                    picks: Picks::default(),
                },
                &standing,
                no_grants,
                move |_| Ok(vault as Arc<dyn crate::init_secrets::SecretsOps>),
            )
            .unwrap();
            let line = picked.held.expect("the held line");
            assert!(line.contains("Turn one on with its switch there"), "{line}");
            assert_eq!(
                line.contains(naming(Setting::Dock)),
                flags,
                "{agent:?}: {line}"
            );
            // The way back itself: it sets the Dock at the console, and in an agent's tab names
            // no item the run takes, saying the person left it as is.
            let words: Vec<OsString> = naming(Setting::Dock)
                .split_whitespace()
                .skip(2)
                .map(OsString::from)
                .collect();
            let mut way = options(&words).unwrap();
            way.station = way.only.clone();
            let holds = standing.holds.clone();
            let prepare = |_: &dyn System, _: &Ctx, _: Mode, reading: &Reading<'_>| {
                let picked = super::picked(reading.picks, reading.only, &|| true, &holds, flags);
                let items: Vec<Box<dyn Item>> = if picked.runs.contains(Setting::Dock) {
                    vec![Listed::of("dock", "init.dock@alice", vec![change(), ok()])]
                } else {
                    Vec::new()
                };
                Ok((vec![(items, place(Class::Preference))], picked))
            };
            let (ran, _) = converge_held(&sys, &way, &prepare, &NoRecords, &|| true);
            if flags {
                let ran = ran.unwrap();
                assert!(
                    ran.iter()
                        .any(|row| row.id == "init.dock@alice" && row.outcome == Outcome::Changed),
                    "{ran:?}"
                );
            } else {
                assert!(
                    matches!(&ran, Err(Refused::Usage(why))
                        if why.contains("left as you set them in System State: Dock")),
                    "{agent:?}: {ran:?}"
                );
            }
            let named = Options {
                picks: Picks {
                    named: Choice {
                        profile: true,
                        ..Choice::NONE
                    },
                    left: Choice::NONE,
                },
                ..Options::default()
            };
            let none =
                |_: &dyn System, _: &Ctx, _: Mode, _: &Reading<'_>| Ok((Vec::new(), Picked::NONE));
            let (typed, _) = converge_held(&sys, &named, &none, &NoRecords, &|| true);
            assert_eq!(
                matches!(typed, Err(Refused::Usage(_))),
                !flags,
                "{agent:?}: {typed:?}"
            );
        }
    }

    /// The way back init names for the GitHub repositories ([`naming`], also the owed clone's
    /// words when its record cannot be written) runs the repositories alone: on an ALab Mac
    /// `--clone` adds to the scope's groups, which would also reset the Dock and sign out of
    /// Messages and FaceTime.
    #[test]
    fn the_narrowest_way_back_for_the_repositories_runs_only_them() {
        let words: Vec<OsString> = naming(Setting::Repositories)
            .split_whitespace()
            .skip(2)
            .map(OsString::from)
            .collect();
        let mut options = options(&words).unwrap();
        // As `entry` reads it: a word no base item names is the workstation's.
        options.station = options.only.clone();
        let groups = |ctx: &Ctx| {
            let (items, _) = desktop_rows(ctx);
            vec![
                (items, place(Class::Preference)),
                (
                    vec![then("init.github.native@alice", "github", || {})],
                    place(Class::Install),
                ),
            ]
        };
        let sys = alice_with_checkout(501, None);
        let rows = converge_with(&sys, &options, &groups, &NoRecords).unwrap();
        let ran: Vec<&str> = rows.iter().map(|row| row.id.as_str()).collect();
        assert_eq!(
            ran,
            ["init.github.native@alice"],
            "`{}` ran more than the repositories",
            naming(Setting::Repositories)
        );
    }

    /// Records another setup holds past init's bounded wait.
    struct NeverLetGo;

    impl Records for NeverLetGo {
        fn hold(&self, _pass: Pass, wait: Option<&dyn Fn(&str)>) -> Result<Box<dyn Held>, Unheld> {
            if let Some(wait) = wait {
                wait("waiting while System State changes this Mac's settings");
            }
            Err(Unheld::Busy(
                "left as it is: System State was changing this Mac's settings for 10 minutes"
                    .into(),
            ))
        }
    }

    /// When init's 10-minute wait for the records ends, the run still reports what its other
    /// lanes did (the vault's join, the clones, the base layer) and exits 75, "a lock", as its
    /// usage says: the Desktop lane's rows stand aside for that lock in plain words, nothing of
    /// them changed. It does not end as a bare failure with every row lost.
    #[test]
    fn inits_bounded_wait_reports_the_other_lanes_and_exits_75() {
        let prepare = |_: &dyn System, ctx: &Ctx, _: Mode, _: &Reading<'_>| {
            let (items, _) = desktop_rows(ctx);
            Ok((
                vec![
                    (items, place(Class::Preference)),
                    (
                        vec![then("init.test.vault", crate::init_secrets::FAMILY, || {})],
                        place(Class::Install),
                    ),
                ],
                Picked::ALL,
            ))
        };
        let sys = alice_with_checkout(501, None);
        let (result, _) = converge_held(&sys, &Options::default(), &prepare, &NeverLetGo, &|| true);
        let rows = match result {
            Ok(rows) => rows,
            Err(refused) => panic!("the wait's end discards the run's report: {refused:?}"),
        };
        assert!(
            rows.iter().any(|row| row.id == "init.test.vault"),
            "{rows:?}"
        );
        let desk: Vec<&ItemReport> = rows
            .iter()
            .filter(|row| {
                row.id.starts_with("init.test.dock") || crate::init_accounts::signs_out(&row.id)
            })
            .collect();
        assert_eq!(desk.len(), 3, "{rows:?}");
        assert!(
            desk.iter().all(|row| row.outcome != Outcome::Changed
                && row
                    .stood_aside
                    .as_ref()
                    .is_some_and(|aside| aside.why().contains("10 minutes"))),
            "{desk:#?}"
        );
        assert_eq!(status(&rows, Action::Apply), 75, "{rows:?}");
    }

    /// A key the person changed after aterm set it (design §6.6) is theirs: `aterm init` leaves
    /// it as they set it, says so, and names the ways that set it again; a key of theirs that
    /// reads as aterm wants is in place. With the record of those keys unreadable, it sets none.
    #[test]
    fn init_leaves_a_key_the_person_changed_and_names_the_way_to_set_it_again() {
        struct Kept(Result<Vec<String>, String>);
        impl Records for Kept {
            fn hold(
                &self,
                _pass: Pass,
                _wait: Option<&dyn Fn(&str)>,
            ) -> Result<Box<dyn Held>, Unheld> {
                Ok(Box::new(NoRecords))
            }
            fn kept(&self) -> Result<Vec<String>, String> {
                self.0.clone()
            }
        }
        let prepare = |_: &dyn System, _: &Ctx, _: Mode, reading: &Reading<'_>| {
            let mut picked = picked(
                reading.picks,
                reading.only,
                &|| true,
                &Holds::default(),
                true,
            );
            picked.rows = vec![
                ("init.test.dock".to_owned(), Setting::Dock),
                ("init.test.widgets".to_owned(), Setting::Widgets),
            ];
            Ok((
                vec![(
                    vec![
                        Listed::boxed("init.test.dock", vec![change(), ok()]),
                        Listed::boxed("init.test.widgets", vec![change(), ok()]),
                    ],
                    place(Class::Preference),
                )],
                picked,
            ))
        };
        let sys = alice_with_checkout(501, None);
        let outcome = |rows: &[ItemReport], id: &str| {
            rows.iter().find(|row| row.id == id).map(|row| row.outcome)
        };
        let kept = Kept(Ok(vec!["init.test.dock".to_owned()]));
        let (rows, held) = converge_held(&sys, &Options::default(), &prepare, &kept, &|| true);
        let rows = rows.unwrap();
        assert_eq!(
            outcome(&rows, "init.test.dock"),
            Some(Outcome::NotApplicable)
        );
        assert_eq!(outcome(&rows, "init.test.widgets"), Some(Outcome::Changed));
        let line = held
            .iter()
            .find(|row| row.id == CHANGED)
            .and_then(|row| row.why.clone())
            .expect("the line of the keys the person changed");
        assert!(
            line.contains("Dock")
                && !line.contains("Widgets")
                && line.contains("Set up now in System State, or aterm doctor --apply"),
            "{line}"
        );
        let unread = Kept(Err("setup-kept.json: not JSON".to_owned()));
        let (rows, _) = converge_held(&sys, &Options::default(), &prepare, &unread, &|| true);
        let rows = rows.unwrap();
        assert!(
            rows.iter().all(|row| row.outcome == Outcome::NotApplicable),
            "{rows:?}"
        );
    }

    /// A run with no terminal builds no preference (they wait for a person at the console), so
    /// its Desktop lane holds the records for its sign-outs alone: a part of the profile, which
    /// saves no plan and clears none of the window's finished stages ([`Pass::Part`]).
    #[test]
    fn a_run_that_sets_no_preference_takes_no_whole_hold() {
        struct Passes(Mutex<Vec<Pass>>);
        impl Records for Passes {
            fn hold(
                &self,
                pass: Pass,
                _wait: Option<&dyn Fn(&str)>,
            ) -> Result<Box<dyn Held>, Unheld> {
                self.0.lock().unwrap().push(pass);
                Ok(Box::new(NoRecords))
            }
        }
        let mut sys = alice_at(501);
        sys.terminal = false;
        sys.agent = None;
        assert_eq!(crate::console::presence(&sys), Presence::NoTerminal);
        let prepare = |_: &dyn System, ctx: &Ctx, _: Mode, reading: &Reading<'_>| {
            let (mut items, mut tags) = desktop_rows(ctx);
            // No preference: only the sign-outs.
            items.retain(|item| crate::init_accounts::signs_out(&item.id()));
            tags.retain(|(id, _)| crate::init_accounts::signs_out(id));
            let mut picked = picked(
                reading.picks,
                reading.only,
                &|| true,
                &Holds::default(),
                false,
            );
            picked.rows = tags;
            Ok((vec![(items, place(Class::Preference))], picked))
        };
        let records = Passes(Mutex::new(Vec::new()));
        let (rows, _) = converge_held(&sys, &Options::default(), &prepare, &records, &|| true);
        let rows = rows.unwrap();
        assert!(
            rows.iter()
                .filter(|row| crate::init_accounts::signs_out(&row.id))
                .all(|row| row.outcome == Outcome::Changed),
            "{rows:?}"
        );
        assert_eq!(
            records.0.into_inner().unwrap(),
            [Pass::Part],
            "a run that set no preference held the records for a whole-profile pass"
        );
    }

    /// The person typed `aterm init --sign-out`, then pressed Stop in System State while init
    /// waited for the records: the Stop is their later choice, and holds the sign-outs, which
    /// nothing undoes, though a flag named them. A flag goes past only a hold on record when the
    /// run began: an unchanged Stop, never another stopped revision with the same words.
    #[test]
    fn a_stop_pressed_after_init_started_holds_a_named_sign_out() {
        for (before, after, blocked) in [(None, 1, true), (Some(1), 1, false), (Some(1), 2, true)] {
            let records = StoppedWhileWaiting {
                waited: std::sync::atomic::AtomicBool::new(false),
                before,
                after,
            };
            let prepare = |_: &dyn System, ctx: &Ctx, _: Mode, reading: &Reading<'_>| {
                let (items, tags) = desktop_rows(ctx);
                let mut picked = picked(
                    reading.picks,
                    reading.only,
                    &|| true,
                    &records.holds(),
                    true,
                );
                picked.rows = tags;
                Ok((vec![(items, place(Class::Preference))], picked))
            };
            let sys = alice_with_checkout(501, None);
            assert_eq!(crate::console::presence(&sys), Presence::AtConsole);
            let named = Options {
                picks: Picks {
                    named: Choice {
                        sign_out: true,
                        ..Choice::NONE
                    },
                    left: Choice::NONE,
                },
                ..Options::default()
            };
            let (rows, held) = converge_held(&sys, &named, &prepare, &records, &|| true);
            let rows = rows.unwrap();
            let signed_out: Vec<&str> = rows
                .iter()
                .filter(|row| {
                    crate::init_accounts::signs_out(&row.id) && row.outcome == Outcome::Changed
                })
                .map(|row| row.id.as_str())
                .collect();
            assert_eq!(
                signed_out.len(),
                if blocked { 0 } else { 2 },
                "stopped revision {before:?} -> {after}: {signed_out:?}"
            );
            assert_eq!(
                held.iter().any(|row| row.id == HELD
                    && row
                        .why
                        .as_deref()
                        .is_some_and(|why| why.contains("Messages, FaceTime"))),
                blocked,
                "{held:?}"
            );
        }
    }

    /// With the record of the keys the person changed unreadable, init sets no preference
    /// ([`KEPT_UNREAD`]), so it records no Mac setting verified for the first launch: the
    /// window's run then sets them.
    #[test]
    fn a_pass_that_could_not_read_the_kept_record_records_no_verified_settings() {
        type Log = Arc<Mutex<Vec<(bool, bool)>>>;
        struct Logged(Log);
        impl Held for Logged {
            fn checkpoint(&self, settings: Selection) -> Result<(), String> {
                self.0
                    .lock()
                    .unwrap()
                    .push((settings.contains(Setting::Dock), false));
                Ok(())
            }
            fn verified(self: Box<Self>) -> Result<(), String> {
                self.0.lock().unwrap().push((true, true));
                Ok(())
            }
        }
        struct Unread(Log);
        impl Records for Unread {
            fn hold(
                &self,
                _pass: Pass,
                _wait: Option<&dyn Fn(&str)>,
            ) -> Result<Box<dyn Held>, Unheld> {
                Ok(Box::new(Logged(Arc::clone(&self.0))))
            }
            fn kept(&self) -> Result<Vec<String>, String> {
                Err("setup-kept.json: not JSON".to_owned())
            }
        }
        let prepare = |_: &dyn System, ctx: &Ctx, _: Mode, reading: &Reading<'_>| {
            let (items, tags) = desktop_rows(ctx);
            let mut picked = picked(
                reading.picks,
                reading.only,
                &|| true,
                &Holds::default(),
                true,
            );
            picked.rows = tags;
            Ok((vec![(items, place(Class::Preference))], picked))
        };
        let sys = alice_with_checkout(501, None);
        let log: Log = Arc::new(Mutex::new(Vec::new()));
        let records = Unread(Arc::clone(&log));
        let (rows, _) = converge_held(&sys, &Options::default(), &prepare, &records, &|| true);
        let rows = rows.unwrap();
        assert!(
            rows.iter().all(|row| row.outcome != Outcome::Changed),
            "{rows:?}"
        );
        let log = log.lock().unwrap().clone();
        assert!(
            log.iter().all(|(dock, verified)| !dock && !verified),
            "init set no preference, yet recorded the Dock or the whole profile verified: \
             {log:?}"
        );
    }

    /// A sign-out the person undid after aterm made it (they signed back in) is in the record
    /// of the keys they changed (design §6.6), and the page reads "Changed by you" on its row:
    /// `aterm init` leaves it as they set it, as it leaves a preference they changed.
    #[test]
    fn init_leaves_a_sign_out_the_person_undid_as_they_set_it() {
        struct KeptSignOut(Vec<String>);
        impl Records for KeptSignOut {
            fn hold(
                &self,
                _pass: Pass,
                _wait: Option<&dyn Fn(&str)>,
            ) -> Result<Box<dyn Held>, Unheld> {
                Ok(Box::new(NoRecords))
            }
            fn kept(&self) -> Result<Vec<String>, String> {
                Ok(self.0.clone())
            }
        }
        let sys = alice_with_checkout(501, None);
        let (ctx, _) = local_context(&sys).unwrap();
        let (_, tags) = desktop_rows(&ctx);
        let messages = tags
            .iter()
            .find(|(_, setting)| *setting == Setting::Messages)
            .map(|(id, _)| id.clone())
            .expect("a Messages sign-out");
        let prepare = |_: &dyn System, ctx: &Ctx, _: Mode, reading: &Reading<'_>| {
            let (items, tags) = desktop_rows(ctx);
            let mut picked = picked(
                reading.picks,
                reading.only,
                &|| true,
                &Holds::default(),
                true,
            );
            picked.rows = tags;
            Ok((vec![(items, place(Class::Preference))], picked))
        };
        let records = KeptSignOut(vec![messages.clone()]);
        let (rows, held) = converge_held(&sys, &Options::default(), &prepare, &records, &|| true);
        let rows = rows.unwrap();
        let outcome = |id: &str| rows.iter().find(|row| row.id == id).map(|row| row.outcome);
        assert_eq!(outcome(&messages), Some(Outcome::NotApplicable), "{rows:?}");
        let facetime = tags
            .iter()
            .find(|(_, setting)| *setting == Setting::FaceTime)
            .map(|(id, _)| id.as_str())
            .expect("a FaceTime sign-out");
        assert_eq!(outcome(facetime), Some(Outcome::Changed), "{rows:?}");
        assert!(
            held.iter().any(|row| row.id == CHANGED
                && row
                    .why
                    .as_deref()
                    .is_some_and(|why| why.contains("Messages"))),
            "{held:?}"
        );
    }

    /// `aterm init --help`: "A group off in System State (all after a Stop) stays as is unless a
    /// flag names it; one turned off mid-run stays as is". A Stop pressed while a clone runs, which may take half an
    /// hour, holds the clones that have not started; one the run named over a hold already on
    /// record when it began still runs.
    #[test]
    fn a_stop_pressed_mid_run_holds_the_clones_not_yet_started() {
        use std::sync::atomic::{AtomicBool, Ordering};
        struct StopLater(Arc<AtomicBool>, Holds);
        impl Records for StopLater {
            fn hold(
                &self,
                _pass: Pass,
                _wait: Option<&dyn Fn(&str)>,
            ) -> Result<Box<dyn Held>, Unheld> {
                Ok(Box::new(NoRecords))
            }
            fn holds(&self) -> Holds {
                Holds {
                    stopped: self
                        .0
                        .load(Ordering::SeqCst)
                        .then(|| STOPPED.to_owned())
                        .or_else(|| self.1.stopped.clone()),
                    stopped_revision: if self.0.load(Ordering::SeqCst) {
                        Some(2)
                    } else {
                        self.1.stopped_revision
                    },
                    ..self.1.clone()
                }
            }
        }
        const STOPPED: &str = "you stopped setup in System State";
        let mut left = Selection::none();
        left.set(Setting::Repositories, true);
        for (start, named) in [
            (Holds::NONE, false),
            (
                Holds {
                    off: left,
                    ..Holds::NONE
                },
                true,
            ),
            (
                Holds {
                    stopped: Some(STOPPED.to_owned()),
                    stopped_revision: Some(1),
                    ..Holds::NONE
                },
                true,
            ),
        ] {
            let stop = Arc::new(AtomicBool::new(false));
            let records = StopLater(Arc::clone(&stop), start.clone());
            let pressed = Arc::clone(&stop);
            let prepare = move |_: &dyn System, _: &Ctx, _: Mode, reading: &Reading<'_>| {
                let pressed = Arc::clone(&pressed);
                let picked = picked(reading.picks, reading.only, &|| true, &start, true);
                Ok((
                    vec![(
                        vec![
                            // The first repository: the person presses Stop while it clones.
                            then("init.github.first@alice", "github", move || {
                                pressed.store(true, Ordering::SeqCst);
                            }),
                            Listed::of("github", "init.github.second@alice", vec![change(), ok()]),
                        ],
                        place(Class::Install),
                    )],
                    picked,
                ))
            };
            let sys = alice_with_checkout(501, None);
            let options = if named {
                Options {
                    picks: Picks {
                        named: Choice {
                            clone: true,
                            ..Choice::NONE
                        },
                        left: Choice::NONE,
                    },
                    ..Options::default()
                }
            } else {
                Options::default()
            };
            let (rows, held) = converge_held(&sys, &options, &prepare, &records, &|| true);
            let rows = rows.unwrap();
            assert!(stop.load(Ordering::SeqCst), "the Stop was pressed mid-run");
            let second = rows
                .iter()
                .find(|row| row.id == "init.github.second@alice")
                .expect("the second clone's row");
            assert_eq!(second.outcome, Outcome::NotApplicable, "{second:?}");
            assert!(
                second
                    .why
                    .as_deref()
                    .is_some_and(|why| why.contains(STOPPED)),
                "{second:?}"
            );
            assert!(
                held.iter().any(|row| row.id == HELD
                    && row
                        .why
                        .as_deref()
                        .is_some_and(|why| why.contains("GitHub repositories"))),
                "{held:?}"
            );
        }
        // A hold on record when the run began, which `--clone` names, holds nothing it runs.
        for start in [
            Holds {
                off: left,
                ..Holds::NONE
            },
            Holds {
                stopped: Some(STOPPED.to_owned()),
                stopped_revision: Some(1),
                ..Holds::NONE
            },
        ] {
            let records = StopLater(Arc::new(AtomicBool::new(false)), start);
            let start = records.holds();
            let prepare = move |_: &dyn System, _: &Ctx, _: Mode, reading: &Reading<'_>| {
                let picked = picked(reading.picks, reading.only, &|| true, &start, true);
                Ok((
                    vec![(
                        vec![Listed::of(
                            "github",
                            "init.github.second@alice",
                            vec![change(), ok()],
                        )],
                        place(Class::Install),
                    )],
                    picked,
                ))
            };
            let named = Options {
                picks: Picks {
                    named: Choice {
                        clone: true,
                        ..Choice::NONE
                    },
                    left: Choice::NONE,
                },
                ..Options::default()
            };
            let sys = alice_with_checkout(501, None);
            let (rows, _) = converge_held(&sys, &named, &prepare, &records, &|| true);
            let rows = rows.unwrap();
            assert!(
                rows.iter()
                    .any(|row| row.id == "init.github.second@alice"
                        && row.outcome == Outcome::Changed),
                "{rows:?}"
            );
        }
    }

    #[test]
    fn held_late_forwards_new_choices_to_each_effect_of_one_admitted_batch() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        struct Batch {
            effects: AtomicUsize,
            stopped: AtomicBool,
            stop_late: bool,
        }
        impl Item for Batch {
            fn id(&self) -> String {
                "init.github.batch@alice".into()
            }
            fn family(&self) -> &'static str {
                "github"
            }
            fn title(&self) -> String {
                self.id()
            }
            fn probe(&self, _: &dyn System, _: &Ctx) -> Probe {
                if self.effects.load(Ordering::SeqCst) == 2 {
                    ok()
                } else {
                    change()
                }
            }
            fn apply(&self, _: &dyn System, _: &Ctx) -> Result<(), String> {
                panic!("the wrapper must forward current authorization into the batch")
            }
            fn apply_guarded(
                &self,
                _: &dyn System,
                _: &Ctx,
                current: &(dyn Fn() -> Result<(), String> + Sync),
            ) -> Result<(), String> {
                for at in 0..2 {
                    // What its first effect did, which only its own answer can say.
                    current().map_err(|why| {
                        format!("{why}; ~/batch: ran its scripts/install-git-hooks.sh")
                    })?;
                    self.effects.fetch_add(1, Ordering::SeqCst);
                    if at == 0 && self.stop_late {
                        self.stopped.store(true, Ordering::SeqCst);
                    }
                }
                Ok(())
            }
        }
        for stop_late in [false, true] {
            let batch = Batch {
                effects: AtomicUsize::new(0),
                stopped: AtomicBool::new(false),
                stop_late,
            };
            let policy = |_: &str| {
                batch
                    .stopped
                    .load(Ordering::SeqCst)
                    .then(|| "late Stop".to_owned())
            };
            let wrapper = HeldLate {
                inner: &batch,
                held: &policy,
                decided: std::sync::OnceLock::new(),
                late: std::sync::OnceLock::new(),
                said: std::sync::OnceLock::new(),
            };
            let sys = crate::port::Fake::root();
            let ctx = crate::items::defaults::tests::ctx(&[]);
            assert!(matches!(wrapper.probe(&sys, &ctx), Probe::Change { .. }));
            assert!(
                wrapper.apply(&sys, &ctx).is_ok(),
                "a held batch is left rather than failed"
            );
            assert_eq!(
                batch.effects.load(Ordering::SeqCst),
                if stop_late { 1 } else { 2 }
            );
            // NEGATIVE CONTROL: a wrapper that answers the hold's words alone loses the line.
            assert_eq!(
                wrapper.stood_aside().map(|aside| aside.why().to_owned()),
                stop_late
                    .then(|| "late Stop; ~/batch: ran its scripts/install-git-hooks.sh".to_owned())
            );
            if stop_late {
                assert!(matches!(
                    wrapper.probe(&sys, &ctx),
                    Probe::NotApplicable { .. }
                ));
            }
        }
    }

    /// `aterm init --help`: "sign-outs run last, with you at the screen". With a terminal at the
    /// console the person may walk away (screen locked or idle) during a long clone: the
    /// sign-outs read presence again as they start, terminal or not, and wait for a person.
    #[test]
    fn a_terminal_runs_admitted_sign_outs_without_a_presence_gate() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let here = Arc::new(AtomicBool::new(true));
        let mut sys = alice_at(501);
        sys.terminal = true;
        sys.agent = None;
        assert_eq!(crate::console::presence(&sys), Presence::AtConsole);
        let gone = Arc::clone(&here);
        let prepare = move |_: &dyn System, ctx: &Ctx, _: Mode, _: &Reading<'_>| {
            let gone = Arc::clone(&gone);
            let items: Vec<Box<dyn Item>> = crate::init_accounts::items(ctx)
                .unwrap()
                .into_iter()
                .map(|item| Listed::boxed(item.id(), vec![change(), ok()]))
                .collect();
            // A clone that takes half an hour; the person walks away meanwhile.
            let clone = then("init.test.clone", "github", move || {
                gone.store(false, Ordering::SeqCst);
            });
            Ok((
                vec![
                    (items, place(Class::Preference)),
                    (vec![clone], anywhere(Class::Install)),
                ],
                Picked::ALL,
            ))
        };
        let present = || here.load(Ordering::SeqCst);
        let (rows, _) = converge_held(&sys, &Options::default(), &prepare, &NoRecords, &present);
        let rows = rows.unwrap();
        let sign_outs: Vec<&ItemReport> = rows
            .iter()
            .filter(|row| crate::init_accounts::signs_out(&row.id))
            .collect();
        assert!(!sign_outs.is_empty());
        assert!(
            sign_outs.iter().all(|row| row.outcome == Outcome::Changed),
            "admitted work continues after a long clone: {sign_outs:#?}"
        );
    }

    /// A sign-out as it finds nobody at this Mac right before its own clicks
    /// ([`crate::init_accounts::found_nobody`]): it clicks nothing and stands aside.
    struct FoundNobody(String);

    impl Item for FoundNobody {
        fn id(&self) -> String {
            self.0.clone()
        }
        fn family(&self) -> &'static str {
            "init.messages"
        }
        fn title(&self) -> String {
            self.0.clone()
        }
        fn probe(&self, _: &dyn System, _: &Ctx) -> Probe {
            change()
        }
        fn apply(&self, _: &dyn System, _: &Ctx) -> Result<(), String> {
            Ok(())
        }
        fn revert(&self) -> Option<String> {
            None
        }
        fn stood_aside(&self) -> Option<aterm_converge::StoodAside> {
            Some(aterm_converge::StoodAside::Waiting(
                crate::init_accounts::away(),
            ))
        }
    }

    /// Each sign-out reads presence again right before its own clicks, through the run's port
    /// (design §6.4, round 23): a person at the screen as the sign-outs start who leaves during
    /// Messages' minute of clicks gets no FaceTime clicks. The run defers that sign-out as it
    /// defers the sign-outs when nobody is there as they start, for the next aterm init and as
    /// no failure of this one. Before, its stand-aside read as owed to the vault's join.
    #[test]
    fn a_sign_out_that_finds_nobody_before_its_clicks_waits_for_the_next_run() {
        let mut sys = alice_at(501);
        sys.terminal = true;
        sys.agent = None;
        let prepare = |_: &dyn System, ctx: &Ctx, _: Mode, _: &Reading<'_>| {
            let items: Vec<Box<dyn Item>> = crate::init_accounts::items(ctx)
                .unwrap()
                .into_iter()
                .map(|item| Box::new(FoundNobody(item.id())) as Box<dyn Item>)
                .collect();
            Ok((vec![(items, place(Class::Preference))], Picked::ALL))
        };
        let (rows, _) = converge_held(&sys, &Options::default(), &prepare, &NoRecords, &|| true);
        let rows = rows.unwrap();
        let sign_outs: Vec<&ItemReport> = rows
            .iter()
            .filter(|row| crate::init_accounts::signs_out(&row.id))
            .collect();
        assert!(!sign_outs.is_empty());
        assert!(
            sign_outs.iter().all(|row| row.outcome == Outcome::Deferred
                && row.why.as_deref() == Some(NOBODY_AT_THE_SCREEN)
                && waits_for_a_person(row)
                && counts(row, Action::Apply)),
            "a sign-out that found nobody before its clicks: {sign_outs:#?}"
        );
    }

    /// [`naming`] promises the narrowest run that sets one group up and nothing else, and the
    /// held line offers it to turn one group back on: on an ALab Mac the Dock's way back sets
    /// the Dock alone, not the Widgets the person also left as is (`--profile` set both).
    #[test]
    fn the_way_back_for_one_group_left_as_is_runs_only_that_group() {
        let mut off = Selection::none();
        off.set(Setting::Dock, true);
        off.set(Setting::Widgets, true);
        let holds = Holds { off, ..Holds::NONE };
        let words: Vec<OsString> = naming(Setting::Dock)
            .split_whitespace()
            .skip(2)
            .map(OsString::from)
            .collect();
        let mut options = options(&words).unwrap();
        options.station = options.only.clone();
        let prepare = |_: &dyn System, _: &Ctx, _: Mode, reading: &Reading<'_>| {
            let picked = picked(reading.picks, reading.only, &|| true, &holds, true);
            let mut items: Vec<Box<dyn Item>> = Vec::new();
            if picked.runs.contains(Setting::Dock) {
                items.push(Listed::of("dock", "init.dock@alice", vec![change(), ok()]));
            }
            if picked.runs.contains(Setting::Widgets) {
                items.push(Listed::of(
                    "widgets",
                    "consumer.widgets.test@alice",
                    vec![change(), ok()],
                ));
            }
            Ok((vec![(items, place(Class::Preference))], picked))
        };
        let sys = alice_with_checkout(501, None);
        let (rows, _) = converge_held(&sys, &options, &prepare, &NoRecords, &|| true);
        let changed: Vec<String> = rows
            .unwrap()
            .iter()
            .filter(|row| row.outcome == Outcome::Changed)
            .map(|row| row.id.clone())
            .collect();
        assert_eq!(
            changed,
            ["init.dock@alice"],
            "`{}`, the Dock's way back, set more than the Dock",
            naming(Setting::Dock)
        );
    }

    /// Each Mac setting's way back ([`naming`]) names the items of its own System State row
    /// and no other, as this build builds them: the preferences, the trackpad's two rows and
    /// each sign-out.
    #[test]
    fn the_way_back_for_each_group_names_only_that_groups_items() {
        let sys = alice_with_checkout(501, None);
        let (ctx, keyboards) = local_context(&sys).unwrap();
        let mut items: Vec<(Setting, Box<dyn Item>)> =
            crate::init_preferences::selected_tagged_items(
                &ctx,
                Selection::all(),
                &keyboards.cohorts,
            )
            .unwrap();
        items.extend(
            crate::init_trackpad::items(&ctx)
                .unwrap()
                .into_iter()
                .map(|item| (Setting::NaturalScrolling, item)),
        );
        items.extend(
            crate::init_trackpad::behavior_items(&ctx)
                .unwrap()
                .into_iter()
                .map(|item| (Setting::TrackpadScrolling, item)),
        );
        items.extend(crate::init_accounts::selected_tagged_items(&ctx, Selection::all()).unwrap());
        for setting in Setting::ALL.into_iter().filter(|s| s.is_workstation()) {
            let word = only_word(setting);
            assert_eq!(naming(setting), format!("aterm init --only {word}"));
            let named: Vec<(Setting, String)> = items
                .iter()
                .filter(|(_, item)| aterm_converge::selects(word, &item.id(), item.family(), None))
                .map(|(each, item)| (*each, item.id()))
                .collect();
            let own = items.iter().filter(|(each, _)| *each == setting).count();
            assert!(
                named.len() == own && named.iter().all(|(each, _)| *each == setting),
                "--only {word} names {named:?} for {setting:?}"
            );
        }
    }

    /// The line of the keys the person changed names each by the short name System State's
    /// row shows ([`Setting::name`]), as the held line does.
    #[test]
    fn the_changed_by_you_line_names_rows_as_the_page_does() {
        let rows = vec![
            held_row("init.test.dock", "Dock", format!("a; {KEPT_BY_YOU}")),
            held_row("init.test.caps", "Caps", format!("a; {KEPT_BY_YOU}")),
        ];
        let tags = vec![
            ("init.test.dock".to_owned(), Setting::Dock),
            ("init.test.caps".to_owned(), Setting::CapsLock),
        ];
        let lines = kept_lines(&rows, &tags);
        let line = lines.first().expect("the changed-by-you line");
        assert!(
            line.contains("Caps Lock key, Dock.") && !line.contains("only aterm"),
            "{line}"
        );
    }

    /// The line of what a person does that the owed clone's record keeps stays on one line and
    /// within what its reader reads whole, however long the row's words.
    #[test]
    fn the_owed_clones_person_line_stays_within_what_its_reader_reads() {
        let long = format!("{}\nsecond line", "é".repeat(4000));
        let line = person_line(&long);
        assert!(line.len() <= PERSON_LINE_MAX, "{}", line.len());
        assert!(!line.contains('\n') && line.ends_with('\u{2026}'));
        assert_eq!(person_line("short"), "short");
    }

    /// A person who left GitHub repositories as is and then names them (`aterm init --clone`,
    /// principle 4: naming a group is their choice too) is told the clone their run owes "runs by
    /// itself once this Mac is let into the vault". The record their run leaves is stamped with
    /// the hold they went past, so the vault's sync clones once the token lands, though System
    /// State still holds the group.
    #[test]
    fn a_clone_the_person_named_is_not_forgotten_by_the_vaults_sync() {
        use crate::sync::tests::owed_clone::{Hooked, alice};
        use std::sync::atomic::{AtomicBool, Ordering};
        let mut left = Selection::none();
        left.set(Setting::Repositories, true);
        let holds = Holds {
            off: left,
            ..Holds::NONE
        };
        let named = Picks {
            named: Choice {
                clone: true,
                ..Choice::NONE
            },
            left: Choice::NONE,
        };
        let picked = picked(named, &[], &|| true, &holds, true);
        assert!(picked.runs.contains(Setting::Repositories), "{picked:?}");
        // The run's GitHub step waits for this Mac to be let into the vault: owed.
        let waiting = ItemReport {
            stood_aside: Some(aterm_converge::StoodAside::Waiting(
                "waiting to be let in".into(),
            )),
            ..held_row(
                "init.github.native@alice",
                "GitHub repositories",
                "waits for init.secrets@alice: waiting to be let in".into(),
            )
        };
        let waiting = ItemReport {
            outcome: Outcome::Blocked,
            ..waiting
        };
        assert_eq!(owe(waiting.clone()).why.as_deref(), Some(OWED));
        let scratch = aterm_tempfile::tempdir().unwrap();
        let state = scratch.path().to_path_buf();
        note_owed(&[waiting], Action::Apply, Some(&state), &picked.start).unwrap();
        // The vault's token lands; System State still holds the group, as it did.
        let (sys, _) = alice();
        let token = "/Users//alice/.secrets/gh_access_token";
        sys.file(token, b"fixture_token_only").stat_of(
            token,
            Stat {
                uid: 501,
                gid: 20,
                mode: 0o600,
                dir: false,
                symlink: false,
                len: 18,
            },
        );
        let cloned = AtomicBool::new(false);
        let owed = || owed_clone(&sys, &state, &holds);
        let clone = || {
            cloned.store(true, Ordering::SeqCst);
            Ok("cloned".to_owned())
        };
        let ports = Hooked {
            inner: crate::sync::tests::Inert::new(&state),
            held: &|| None,
            owed: &owed,
            clone: &clone,
            forget: &|| forget_owed_clone(&state),
        };
        assert!(matches!(owed(), Ok(OwedClone::Due(_))), "{:?}", owed());
        let _ = crate::sync::converge(
            &ports,
            crate::sync::Trigger::Subscriber,
            Mode::Apply,
            aterm_secrets::converge::RepositoryWork::Refresh,
            &mut |_| {},
        );
        assert!(
            cloned.load(Ordering::SeqCst),
            "the clone the person's own `aterm init --clone` owed was forgotten: {:?}",
            owed()
        );
    }
}
