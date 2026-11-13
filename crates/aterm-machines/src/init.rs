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
    Disable Spotlight indexing on the data volume (file-search updates stop; same password)
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
  Preferences need you at the console; sign-outs run last, with you at the screen. A
  group off in System State (all after a Stop) stays as is unless a flag names it; one
  turned off mid-run stays as is. A setting changed since aterm set it stays as is till
  Set up now or aterm doctor --apply. --plan reports, --check only reads, --json prints
  JSON, --verbose every row. Agent and headless preference checks relay what System State
  last read back, with its age: set up as last read once over 10 min old; a row it has
  not read yet is 77.
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
/// person turns a part off later (AGENTS.md).
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
    /// Whether a person is at this Mac's screen now: a keyboard, mouse or trackpad used within
    /// the last minute, and the screen neither locked nor switched to another login. Read with
    /// no terminal as the run decides whether to build the Messages and FaceTime sign-outs and
    /// whether `--sign-out` runs (design §6.4); each sign-out reads its own session again right
    /// before its clicks ([`crate::port::System::session_reach`]).
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

    /// The items whose keys changed after aterm set them (design §6.6): `aterm init` leaves each
    /// as it is ([`KEPT_CHANGED`]), as the window's unattended pass does. Read while the records
    /// are held.
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
    entry_with_system(
        args,
        host,
        tab_var,
        tab_run,
        &crate::sessions::is_live_account,
        &|| Box::new(Live::new()),
        &|current| host.converge.init_name(current),
    )
}

type NameCompletion<'a> =
    dyn Fn(&dyn Fn() -> Result<(), String>) -> Result<Option<String>, String> + 'a;

/// The actual entry with its local process ports supplied; production constructs the
/// live system only after the parser and tab/query routing have admitted ordinary setup.
fn entry_with_system(
    args: Vec<OsString>,
    host: &Host<'_>,
    tab_var: Option<&std::ffi::OsStr>,
    tab_run: &dyn Fn(&Options, &Host<'_>) -> u8,
    own_account: &dyn Fn() -> bool,
    system: &dyn Fn() -> Box<dyn System>,
    publish: &NameCompletion<'_>,
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
    if !own_account() && !options.toolset_alone() {
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
    let sys = system();
    let cwd = std::env::current_dir().ok();
    let printer = Printer::new(options.verbose, options.json);
    let result = sys
        .journal()
        .map_err(Refused::Failed)
        .and_then(|mut journal| {
            converge(
                sys.as_ref(),
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
    let mut rows = match result {
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
    if let Some(row) = finish_init_name(
        &options,
        host.decided,
        sys.as_ref(),
        host.records,
        &rows,
        publish,
    ) {
        printer.row(&row);
        printer.flush();
        rows.push(row);
    }
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

/// Only a person's ordinary apply that selected and completed base.clt may align names.
/// The completion port captures its target now, after root returned, and rechecks this
/// Stop/record fence alongside its saved-name/policy fence before each OS effect.
fn finish_init_name(
    options: &Options,
    decided: Decider,
    sys: &dyn System,
    records: &dyn Records,
    rows: &[ItemReport],
    publish: &NameCompletion<'_>,
) -> Option<ItemReport> {
    let mut root = rows.iter().filter(|row| row.id == crate::init_clt::ID);
    if decided != Decider::Person
        || options.action != Action::Apply
        || options.help
        || options.converge
        || (!options.only.is_empty()
            && !options
                .only
                .iter()
                .any(|word| crate::init_clt::selected(word)))
        || !root
            .next()
            .is_some_and(|row| matches!(row.outcome, Outcome::Ok | Outcome::Changed))
        || root.next().is_some()
    {
        return None;
    }
    let current = || name_current(sys, &|| records.holds());
    let began = std::time::Instant::now();
    let result = current().and_then(|()| publish(&current));
    let mut row = match result {
        Ok(None) => return None,
        Ok(Some(now)) => {
            let mut row = held_row("sync.identity", "Mac names", String::new());
            row.outcome = Outcome::Ok;
            row.to = Some(now);
            row.why = None;
            row
        }
        Err(why) => {
            let mut row = held_row("sync.identity", "Mac names", why);
            row.outcome = Outcome::Blocked;
            row.essential = true;
            row
        }
    };
    row.family = "identity".into();
    row.layer = Some(Layer::Base);
    row.class = Some(Class::Preference);
    row.elapsed_ms = Some(u64::try_from(began.elapsed().as_millis()).unwrap_or(u64::MAX));
    Some(row)
}

/// The global Stop/record fence shared by init completion and explicit saved-name sync.
/// Individual setup selections do not grant or hold naming; its source and policy are
/// checked by the caller immediately before each OS effect.
/// # Errors
/// This process was stopped, setup is stopped/unread, or a newer image owns its records.
pub fn name_current(sys: &dyn System, holds: &dyn Fn() -> Holds) -> Result<(), String> {
    if sys.stopped().is_some() {
        return Err("setup was stopped before its machine names finished".into());
    }
    let holds = holds();
    if holds.newer {
        return Err("a newer setup record holds machine naming unchanged".into());
    }
    if let Some(why) = holds.stopped {
        return Err(why);
    }
    Ok(())
}

/// The id of the row `--json` carries for the line of what the person's choices in System
/// State held ([`held_row`]), which a batch their later hold stopped names as what it waits on
/// ([`held_late`]).
const HELD: &str = "init.system-state";

/// The id of the row `--json` carries for the line of the keys that changed since aterm set
/// them ([`held_row`]).
const CHANGED: &str = "init.changed-since";

/// The row `--json` carries for `line` ([`Front::on_held`]), a line a run says of what the
/// person's choices held, which the text output prints after its tally: the groups System State
/// holds ([`HELD`], [`Holds::line`]) or the keys that changed since aterm set them
/// ([`CHANGED`]). Each is left as it is, so not applicable to this run, and no failure.
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
        waits_on: None,
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

/// [`tab`] on this Mac: its words on this run's terminal, its install in this account.
fn tab_entry(options: &Options, host: &Host<'_>) -> u8 {
    let sys = Live::new();
    let out = crate::init_clt::Tab::new(Arc::new(|text: &str| {
        use std::io::Write as _;
        let mut stdout = std::io::stdout().lock();
        let _ = stdout.write_all(text.as_bytes());
        let _ = stdout.flush();
    }));
    let cwd = std::env::current_dir().ok();
    let groups = |_: &dyn System, _: &Ctx, _: Mode, _: &Reading<'_>| Ok((Vec::new(), Picked::NONE));
    tab(&sys, &out, &crate::sessions::is_live_account, &|tab| {
        let leave =
            std::env::var_os(aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_LEAVE_DEVELOPER_MODE)
                .is_some_and(|value| !value.is_empty());
        let in_tab = |applies: bool| {
            let mut composed = (host.base)(options.base_lock(applies));
            composed.clt = composed
                .clt
                .map(|clt| clt.in_tab(tab.clone()).leaving_developer_mode(leave));
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
/// [`crate::init_clt::TAB_INSTALLED`] and 0 where the tools' row read them in place and no root
/// step beside them reads owed ([`crate::init_clt::RootSteps`]), or why the install did not finish
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
    if !crate::init_clt::await_tab_go(sys, out) {
        return TAB_NOT_STARTED;
    }
    // Installed: the tools' row read them in place, and no root step beside them reads owed.
    let installed = if live() {
        converge(out).is_ok_and(|rows| {
            rows.iter().any(|row| {
                row.id == crate::init_clt::ID
                    && matches!(row.outcome, Outcome::Ok | Outcome::Changed)
            }) && rows
                .iter()
                .all(|row| row.id != crate::init_clt::ROOT_STEPS_ID || row.outcome == Outcome::Ok)
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
        (count(held_late), "held by your choices in System State"),
        (
            count(waits_on_background_setup),
            "waiting on background setup",
        ),
        (
            count(|row| waits_on_a_step(row) && !waits_on_background_setup(row) && !held_late(row)),
            "waiting on another step",
        ),
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

/// Whether `row` relays the background coordinator's wait ([`AwayFromConsole`]): the window's
/// own setup is on the Mac settings, and the row says so in the coordinator's words.
fn waits_on_background_setup(row: &ItemReport) -> bool {
    row.id == AWAY && waiting(row)
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
/// for this Mac to be let into the vault ([`owed`]), or a later hold of the person's stopped it
/// ([`held_late`]): the converge pass the vault's subscriber starts once the vault's files land
/// runs that step then, once ([`owed_clone`], [`clone_owed`]), so nobody runs `aterm init` again
/// for it (design §7); a Stop keeps it owed until Set up now.
const CLONE_OWED: &str = "clone-owed";

/// What `sync.clone` reads of the clone an `aterm init` owes ([`owed_clone`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwedClone {
    /// No clone was requested, no inventory can be verified here, or its selection is empty.
    Nothing(String),
    /// The private vault inventory was read and every repository verified locally.
    Verified { present: usize, total: usize },
    /// Every repository of the private vault inventory has a matching checkout here, but
    /// `unborn` of them have no commit and no current proof that their remote is empty: only a
    /// read of each remote, which acquires any commits it finds, can verify them.
    Unobserved {
        present: usize,
        unborn: usize,
        total: usize,
    },
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

fn later_clone_hold(record: &str, now: &Holds) -> Option<String> {
    now.later(&stamped_holds(record), Setting::Repositories)
        .then(|| now.line(&[Setting::Repositories], false))
        .flatten()
}

/// The account's converge state under `state`, where [`CLONE_OWED`] lives.
fn converge_state(state: &Path) -> PathBuf {
    state.join(aterm_converge::FileJournal::DIR)
}

/// Record, under the account's state directory `state`, that an apply's GitHub step is owed
/// ([`CLONE_OWED`]): it waits for this Mac to be let into the vault ([`owed`]), or a later hold
/// of the person's stopped it ([`held_late`]). The record is stamped with what the person's
/// choices in System State held of GitHub repositories when the run began (`start`,
/// [`HELD_MARK`]), so `sync.clone` keeps a Stop's clone owed until Set up now and forgets one
/// whose group the person left as is since; nothing for a plan, a check or a run whose rows owe
/// nothing.
///
/// # Errors
/// The record cannot be written, or there is no state directory to write it in.
fn note_owed(
    rows: &[ItemReport],
    action: Action,
    state: Option<&Path>,
    start: &Holds,
) -> Result<(), String> {
    if action != Action::Apply || !rows.iter().any(|row| owed(row) || held_late(row)) {
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
        format!("aterm init owes its GitHub step\n{}\n", held_stamp(start)).as_bytes(),
    )
    .map_err(|why| format!("{}: {why}", dir.join(CLONE_OWED).display()))
}

/// The line [`CLONE_OWED`] carries when the step's last run stopped at a step only a person
/// can take: the step's mark then ([`crate::init_github::TokenRead::mark`]) follows it.
const PERSON_MARK: &str = "mark ";

/// The fresh control check before a subscriber starts repository work. This checks
/// current policy and the bounded debt record, never Git or checkout readiness.
/// A pending typed credential withdrawal retains its ordinary source admission;
/// this reading alone authorizes no repository effect or completion.
///
/// # Errors
/// The local account, profile, debt record or applicable withdrawal cannot be read safely.
pub fn owed_clone_control(
    sys: &dyn System,
    state: &Path,
    now: &Holds,
) -> Result<Option<String>, String> {
    let ctx = repository_context(sys)?;
    // Explicit opt-out may still retire its existing credential request. Preserve
    // this canonical local-only path before Off, while the person's holds still win.
    if !now.holds(Setting::Repositories)
        && crate::init_github::RepositoryCredentialWithdrawal::capture(sys, &ctx)?.is_some()
    {
        return Ok(None);
    }
    let dir = converge_state(state);
    let record = match aterm_uds::private_file::read_private(&dir, CLONE_OWED, 4096) {
        Ok(record) => Some(record),
        Err(aterm_uds::private_file::PrivateFileError::Missing) => None,
        Err(why) => return Err(format!("{}: {why}", dir.join(CLONE_OWED).display())),
    };
    clone_control_hold(sys, &ctx, now, record.as_deref())
}

fn clone_control_hold(
    sys: &dyn System,
    ctx: &Ctx,
    now: &Holds,
    record: Option<&[u8]>,
) -> Result<Option<String>, String> {
    if !crate::init_github::settings_managed(sys, &ctx.users[0])? {
        return Ok(Some("off: init.toml's [github] enabled = false".into()));
    }
    if now.stopped.is_some() || now.newer {
        return Ok(Some(now.why(Setting::Repositories)));
    }
    Ok(match record {
        Some(record) => later_clone_hold(&String::from_utf8_lossy(record), now),
        None => now
            .holds(Setting::Repositories)
            .then(|| now.why(Setting::Repositories)),
    })
}

/// The bounded control intent of one repository apply, retaining the original
/// debt file so a later request cannot grant an older captured operation.
/// Credential, source, inventory and checkout admission remain independent.
pub struct OwedCloneIntent {
    state: PathBuf,
    user: crate::model::User,
    holds: Holds,
    debt: Option<CloneDebt>,
    withdrawal: bool,
}

struct CloneDebt {
    file: std::fs::File,
    bytes: Vec<u8>,
}

impl std::fmt::Debug for OwedCloneIntent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OwedCloneIntent(..)")
    }
}

impl CloneDebt {
    fn read(state: &Path) -> Result<Option<Self>, String> {
        use std::io::Read as _;
        let dir = converge_state(state);
        let (file, metadata) = match aterm_uds::private_file::open_private(&dir, CLONE_OWED) {
            Ok(opened) => opened,
            Err(aterm_uds::private_file::PrivateFileError::Missing) => return Ok(None),
            Err(why) => return Err(format!("{}: {why}", dir.join(CLONE_OWED).display())),
        };
        if metadata.len() > 4096 {
            return Err("the repository debt record exceeds its 4096-byte bound".into());
        }
        let mut bytes = Vec::new();
        (&file)
            .take(4097)
            .read_to_end(&mut bytes)
            .map_err(|why| format!("{}: {why}", dir.join(CLONE_OWED).display()))?;
        if bytes.len() > 4096 {
            return Err("the repository debt record exceeds its 4096-byte bound".into());
        }
        Ok(Some(Self { file, bytes }))
    }

    fn same(&self, other: &Self) -> Result<bool, String> {
        use std::os::unix::fs::MetadataExt as _;
        let before = self.file.metadata().map_err(|why| why.to_string())?;
        let now = other.file.metadata().map_err(|why| why.to_string())?;
        Ok((before.dev(), before.ino()) == (now.dev(), now.ino()) && self.bytes == other.bytes)
    }
}

impl OwedCloneIntent {
    /// Capture the current control policy and exact optional private debt record.
    /// An older repository switch may be overridden only by that named request.
    /// # Errors
    /// Current policy holds the work, or account, profile or debt cannot be read safely.
    pub fn capture(sys: &dyn System, state: &Path, now: &Holds) -> Result<Self, String> {
        let ctx = repository_context(sys)?;
        let debt = CloneDebt::read(state)?;
        let held = now.holds(Setting::Repositories);
        if held
            && let Some(why) = clone_control_hold(
                sys,
                &ctx,
                now,
                debt.as_ref().map(|debt| debt.bytes.as_slice()),
            )?
        {
            return Err(why);
        }
        // Capture this purpose before effects. A callback inside retirement must
        // not discover new ledger work or invalidate its own completed cleanup.
        let withdrawal =
            crate::init_github::RepositoryCredentialWithdrawal::capture(sys, &ctx)?.is_some();
        if withdrawal {
            if held {
                return Err(now.why(Setting::Repositories));
            }
        } else if !held
            && let Some(why) = clone_control_hold(
                sys,
                &ctx,
                now,
                debt.as_ref().map(|debt| debt.bytes.as_slice()),
            )?
        {
            return Err(why);
        }
        Ok(Self {
            state: state.to_path_buf(),
            user: ctx.users[0].clone(),
            holds: now.clone(),
            debt,
            withdrawal,
        })
    }

    /// Whether this same account, state root, debt and policy permit the next effect.
    /// A new debt cannot supply an override to an operation captured without it.
    /// # Errors
    /// A current account, profile or private debt reading is unsafe or unavailable.
    pub fn current(&self, sys: &dyn System, state: &Path, now: &Holds) -> Result<bool, String> {
        if state != self.state.as_path() || now.later(&self.holds, Setting::Repositories) {
            return Ok(false);
        }
        let ctx = repository_context(sys)?;
        if ctx.users[0] != self.user {
            return Ok(false);
        }
        let debt = CloneDebt::read(state)?;
        let same = match (&self.debt, &debt) {
            (None, None) => true,
            (Some(before), Some(now)) => before.same(now)?,
            _ => false,
        };
        if !same {
            return Ok(false);
        }
        if self.withdrawal {
            // The separately captured withdrawal still validates its own source,
            // request and account. This is only the original unheld caller guard.
            return Ok(!now.holds(Setting::Repositories));
        }
        Ok(clone_control_hold(
            sys,
            &ctx,
            now,
            debt.as_ref().map(|debt| debt.bytes.as_slice()),
        )?
        .is_none())
    }

    /// Recheck one ordinary apply's captured source as well as its control intent.
    /// An empty selection cannot turn a named clone into cleanup over a held group.
    /// # Errors
    /// The current local account, policy, debt or captured source cannot be read safely.
    pub fn current_inputs(
        &self,
        sys: &dyn System,
        state: &Path,
        now: &Holds,
        inputs: &crate::init_github::InitialRepositoryInputs,
    ) -> Result<bool, String> {
        if !self.current(sys, state, now)?
            || (inputs.selects_no_repositories() && now.holds(Setting::Repositories))
        {
            return Ok(false);
        }
        inputs.is_current_now(sys)
    }
}

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
pub fn owed_clone(
    sys: &dyn System,
    state: &Path,
    now: &Holds,
    readback: Option<&crate::init_github::RepositoryReadback>,
) -> Result<OwedClone, String> {
    owed_clone_in_pass(
        sys,
        state,
        now,
        readback,
        aterm_secrets::converge::RepositoryWork::Repair,
    )
}

/// An admitted pass can refresh a preserved default checkout's remote observation.
/// Ordinary status readers use [`owed_clone`] and remain local.
/// # Errors
/// As [`owed_clone`], including a failed bounded remote observation during Refresh.
pub fn owed_clone_in_pass(
    sys: &dyn System,
    state: &Path,
    now: &Holds,
    readback: Option<&crate::init_github::RepositoryReadback>,
    work: aterm_secrets::converge::RepositoryWork,
) -> Result<OwedClone, String> {
    if !now.holds(Setting::Repositories)
        && let Ok(ctx) = repository_context(sys)
        && crate::init_github::RepositoryCredentialWithdrawal::capture(sys, &ctx)?.is_some()
    {
        return Ok(OwedClone::Due(
            "the default repository credential request is no longer needed".into(),
        ));
    }
    owed_clone_with(sys, state, now, None, readback, work)
}

/// Run only local credential-request retirement, retaining fresh caller holds at every
/// ledger boundary. Ordinary repository effects still require the full paired capture.
/// # Errors
/// The captured local intent or caller authority changed, or the ledger cannot be saved.
pub fn withdraw_repository_credential(
    sys: &dyn System,
    state: &Path,
    withdrawal: crate::init_github::RepositoryCredentialWithdrawal,
    current: impl Fn() -> Result<bool, String> + Send + Sync + 'static,
) -> Result<String, String> {
    let ctx = repository_context(sys)?;
    let rows = crate::init_github::run_repository_credential_withdrawal(
        sys,
        &ctx,
        Mode::Apply,
        withdrawal,
        current,
    )?;
    settle_owed_clone(sys, state, &ctx, &rows)
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
    readback: Option<&crate::init_github::RepositoryReadback>,
    work: aterm_secrets::converge::RepositoryWork,
) -> Result<OwedClone, String> {
    let ctx = repository_context(sys)?;
    if let Err(why) = inputs.validate_default_context(sys, &ctx) {
        return Ok(OwedClone::Waiting(why));
    }
    let value = owed_clone_with(sys, state, now, Some(&inputs), readback, work)?;
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
        if matches!(
            &value,
            OwedClone::Due(_) | OwedClone::Verified { .. } | OwedClone::Unobserved { .. }
        ) {
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
    readback: Option<&crate::init_github::RepositoryReadback>,
    work: aterm_secrets::converge::RepositoryWork,
) -> Result<OwedClone, String> {
    let dir = converge_state(state);
    let record = match aterm_uds::private_file::read_private(&dir, CLONE_OWED, 4096) {
        Ok(record) => String::from_utf8_lossy(&record).into_owned(),
        Err(aterm_uds::private_file::PrivateFileError::Missing) => {
            return automatic_clone(sys, now, inputs, readback);
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
    if let Some(line) = later_clone_hold(&record, now) {
        return Ok(OwedClone::Held(line));
    }
    // Live callers already captured the default source. A standalone legacy
    // read captures it once here and reuses it in its ordinary Person readback.
    let owned = if inputs.is_none() && crate::init_github::uses_vault_inventory(sys, &ctx)? {
        Some(crate::init_github::InitialRepositoryInputs::capture_ready(
            sys, &ctx,
        )?)
    } else {
        None
    };
    let inputs = inputs.or(owned.as_ref());
    if let Some(inputs) = inputs.filter(|inputs| inputs.local_blocker().is_none())
        && let Err(why) = inputs.validate_selection(sys, &ctx)
    {
        return Ok(OwedClone::Waiting(why));
    }
    if inputs.is_none_or(|inputs| inputs.local_blocker().is_some()) {
        match crate::init_github::RepositorySelection::capture(sys, &ctx) {
            Ok(Some(selection)) => {
                if let Some(inputs) = inputs
                    && let Err(why) = inputs.validate_selection(sys, &ctx)
                {
                    return Ok(OwedClone::Waiting(why));
                }
                if selection.count() == 0 {
                    selection.ensure_current(sys)?;
                    return Ok(OwedClone::Nothing(
                        "the private inventory selects no repositories".into(),
                    ));
                }
                if selection.needs_credential(sys)? {
                    return Ok(OwedClone::Waiting(selection.waiting()));
                }
            }
            Err(why) => return Ok(OwedClone::Waiting(why)),
            _ => {}
        }
    }
    if let Some(inputs) = inputs.filter(|inputs| inputs.selects_no_repositories()) {
        return Ok(if inputs.is_current_now(sys)? {
            OwedClone::Nothing("the private inventory selects no repositories".into())
        } else {
            OwedClone::Waiting(
                "repository inputs changed while the empty selection was read".into(),
            )
        });
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
            // the default inventory without applying or enumerating GitHub, so a
            // corrected worktree can resume even with an untyped pre-upgrade Person mark.
            // Explicit profiles keep their auth cache and never repeat its network probe.
            let rows = match inputs {
                Some(inputs) => crate::init_github::run_initial_repositories_captured(
                    sys,
                    &ctx,
                    Mode::Plan,
                    work,
                    inputs.clone(),
                    || Ok(true),
                )?,
                None => crate::init_github::run_initial_repositories_captured(
                    sys,
                    &ctx,
                    Mode::Plan,
                    work,
                    crate::init_github::InitialRepositoryInputs::capture_ready(sys, &ctx)?,
                    || Ok(true),
                )?,
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
                    Outcome::Planned => {
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
    readback: Option<&crate::init_github::RepositoryReadback>,
) -> Result<OwedClone, String> {
    let nothing = || OwedClone::Nothing("aterm init owes no clone".into());
    let Ok(ctx) = repository_context(sys) else {
        return Ok(nothing());
    };
    let owned = if inputs.is_none() && crate::init_github::uses_vault_inventory(sys, &ctx)? {
        Some(crate::init_github::InitialRepositoryInputs::capture_ready(
            sys, &ctx,
        )?)
    } else {
        None
    };
    let inputs = inputs.or(owned.as_ref());
    if let Some(inputs) = inputs.filter(|inputs| inputs.local_blocker().is_none())
        && let Err(why) = inputs.validate_selection(sys, &ctx)
    {
        return Ok(OwedClone::Waiting(why));
    }
    if inputs.is_none_or(|inputs| inputs.local_blocker().is_some()) {
        match crate::init_github::RepositorySelection::capture(sys, &ctx) {
            Ok(Some(selection)) => {
                if let Some(inputs) = inputs
                    && let Err(why) = inputs.validate_selection(sys, &ctx)
                {
                    return Ok(OwedClone::Waiting(why));
                }
                if now.holds(Setting::Repositories) {
                    return Ok(OwedClone::Held(now.why(Setting::Repositories)));
                }
                if selection.count() == 0 {
                    selection.ensure_current(sys)?;
                    return Ok(OwedClone::Nothing(
                        "the private inventory selects no repositories".into(),
                    ));
                }
                if selection.needs_credential(sys)? {
                    return Ok(OwedClone::Waiting(selection.waiting()));
                }
            }
            Err(why) => return Ok(OwedClone::Waiting(why)),
            Ok(None) => {}
        }
    }
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
    let counted = match inputs {
        Some(inputs) => {
            crate::init_github::automatic_inventory_counts_captured(sys, &ctx, inputs, readback)
        }
        None => crate::init_github::automatic_inventory_counts(sys, &ctx),
    };
    match counted {
        Ok(Some(counts)) if counts.total == 0 => Ok(OwedClone::Nothing(
            "the private inventory selects no repositories".into(),
        )),
        Ok(Some(counts)) if counts.all_here() => {
            if crate::init_github::RepositorySelection::acknowledgment_due(sys, &ctx.users[0])? {
                Ok(OwedClone::Due(
                    "confirm the delivered repository credential locally".into(),
                ))
            } else if counts.unborn == 0 {
                Ok(OwedClone::Verified {
                    present: counts.present,
                    total: counts.total,
                })
            } else {
                Ok(OwedClone::Unobserved {
                    present: counts.present,
                    unborn: counts.unborn,
                    total: counts.total,
                })
            }
        }
        Ok(Some(counts)) => Ok(OwedClone::Due(format!(
            "{}/{} repositories from the private vault are present; fleet setup clones the rest",
            counts.present + counts.unborn,
            counts.total
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
) -> Result<CloneOwedOutcome, String> {
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
) -> Result<CloneOwedOutcome, String> {
    clone_owed_in_pass_with_withdrawal_guard(sys, state, work, execution, None)
}

/// As [`clone_owed_in_pass`], retaining a caller's independent current-holds check for
/// any local credential withdrawal discovered while the items are constructed.
/// # Errors
/// As [`clone_owed_in_pass`], or the caller withholds a local withdrawal.
pub fn clone_owed_in_pass_with_withdrawal_guard(
    sys: &dyn System,
    state: &Path,
    work: aterm_secrets::converge::RepositoryWork,
    execution: Option<crate::sync::repository_refresh::Execution>,
    withdrawal_current: Option<Box<dyn Fn() -> Result<bool, String> + Send + Sync>>,
) -> Result<CloneOwedOutcome, String> {
    let ctx = repository_context(sys)?;
    if crate::init_github::uses_vault_inventory(sys, &ctx)? {
        let inputs = crate::init_github::InitialRepositoryInputs::capture_ready(sys, &ctx)?
            .with_repository_execution(execution);
        return clone_owed_captured(sys, state, work, inputs, move || {
            withdrawal_current
                .as_ref()
                .map_or(Ok(true), |current| current())
        });
    }
    let items = crate::init_github::items_for_execution_with_withdrawal_guard(
        sys,
        &ctx,
        None,
        work,
        execution,
        withdrawal_current,
    )?;
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
    settle_owed_clone(sys, state, &ctx, &rows).map(|said| CloneOwedOutcome {
        said,
        readback: None,
    })
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
) -> Result<CloneOwedOutcome, String> {
    let ctx = repository_context(sys)?;
    let rows = crate::init_github::run_initial_repositories_captured(
        sys,
        &ctx,
        Mode::Apply,
        work,
        inputs,
        current,
    )?;
    let said = settle_owed_clone(sys, state, &ctx, &rows)?;
    Ok(CloneOwedOutcome {
        said,
        readback: rows.readback,
    })
}

/// One completed native repository attempt and its pass-owned empty-origin readback.
#[derive(Debug)]
pub struct CloneOwedOutcome {
    pub said: String,
    pub readback: Option<crate::init_github::RepositoryReadback>,
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
/// (GitHub's sign-in and the clones, design §7, [`owed`]): the vault's join finishes by itself,
/// and what waits on it runs then, so it is no step of this run's. A row waiting on anything else
/// keeps its own outcome and words, and a read-only relay keeps the coordinator's scoped reason
/// rather than claiming vault work.
fn owe(mut row: ItemReport) -> ItemReport {
    if row.outcome == Outcome::Blocked && owed(&row) {
        row.outcome = Outcome::Deferred;
        if row.class != Some(Class::Relay) {
            row.why = Some(OWED.into());
        }
    }
    row
}

/// [`owe`], save a row a later hold left (a Stop pressed, or GitHub repositories turned off, while
/// its batch ran: [`HeldLate`], whose words `held` are): its line is the person's words and what
/// the batch did before it stopped, and it waits on no vault, so it is not deferred as owed. It
/// waits on the person's choices in System State instead, and names their row ([`HELD`], its
/// [`ItemReport::waits_on`]): [`held_late`].
fn owe_unless_held(mut row: ItemReport, held: Option<&str>) -> ItemReport {
    let theirs = held.is_some_and(|words| {
        waiting(&row) && row.why.as_deref().is_some_and(|why| why.starts_with(words))
    });
    if theirs {
        row.waits_on = Some(HELD.to_owned());
        row
    } else {
        owe(row)
    }
}

/// Whether `row` is a batch a later hold of the person's stopped ([`owe_unless_held`]): their
/// choice, as a hold read when the run began is, so no failure of this run's ([`left`]), and its
/// GitHub step stays owed ([`note_owed`]): `sync.clone` goes on with it once Set up now lifts a
/// Stop, and forgets it where they left GitHub repositories as they are.
fn held_late(row: &ItemReport) -> bool {
    waiting(row) && row.waits_on.as_deref() == Some(HELD)
}

/// Whether `row` waits for this Mac to be let into the vault, and runs once it is ([`owe`]): a
/// GitHub row that did not run because the vault's own row, which it runs after, stood aside
/// waiting for an admin's approval ([`ItemReport::waits_on`], [`waits_by_itself`]); what the
/// converge pass clones once the vault's token lands ([`note_owed`]). A row waiting on anything
/// else (another macOS dialog, the background coordinator, the person's later hold
/// ([`held_late`]), a wait of its own) is not owed to the vault: its join moves none of them.
fn owed(row: &ItemReport) -> bool {
    waiting(row)
        && !waits_by_itself(row)
        && row.family == "github"
        && row
            .waits_on
            .as_deref()
            .is_some_and(crate::init_secrets::is_vault_row)
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
    /// keys kept as they changed since aterm set them ([`Records::kept`]).
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
    // that could not read which keys changed since aterm set them set no preference (each row
    // reads not applicable, [`KEPT_UNREAD`]), so it records nothing: the window's run sets them.
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
                .all(verified_row)
        {
            taken.verified()?;
            return Ok(None);
        }
        Ok::<_, String>(Some(taken))
    };
    // The keys that changed since aterm set them stay as they are ([`Records::kept`]), and
    // the words their rows end with; with no record of them readable, every one does.
    let read_kept = || {
        let kept = records.kept();
        let why = if kept.is_ok() {
            KEPT_CHANGED
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
        // The record of the keys changed since aterm set them could not be read: no preference
        // was set.
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
        // A sign-out signed in again since aterm made it, which takes a person's Apple Account,
        // is left as it is, as a key kept as it changed is (design §6.6): the record is read
        // again under this hold.
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
            "Settings changed since aterm set them",
            line,
        ));
    }
    Ok(rows
        .into_iter()
        .map(|row| owe_unless_held(row, late_words.as_deref()))
        .collect())
}

/// The lines of the keys a run left as they are ([`Instead::Kept`]), by the System State row
/// each was built for (`tags`): the ones that changed since aterm set them, and the ones left
/// because the record of those cannot be read.
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
    let changed = names(KEPT_CHANGED);
    if !changed.is_empty() {
        lines.push(format!(
            "left as they are, changed since aterm set them: {changed}. Set up now in System \
             State, or aterm doctor --apply, sets them again"
        ));
    }
    let unread = names(KEPT_UNREAD);
    if !unread.is_empty() {
        lines.push(format!(
            "aterm could not read which settings changed since it set them, so it left these as \
             they are: {unread}. aterm doctor --apply sets them again"
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

/// Whether `row` stood aside waiting ([`aterm_converge::StoodAside::Waiting`]), of its own accord
/// or behind a prerequisite that did: on an admin, a dialog, the background coordinator, or
/// anything else it names. [`owed`] says which of these wait on the vault.
fn waiting(row: &ItemReport) -> bool {
    matches!(
        row.stood_aside,
        Some(aterm_converge::StoodAside::Waiting(_))
    )
}

/// Fully covered Mac stages in a whole raw init. Keep the prepared preference
/// IDs and the native services' expected IDs, so absent or unbuilt rows cannot
/// earn completion merely because every row that happened to return succeeded,
/// and count only rows read as wanted ([`verified_row`]), so a row an apply may end
/// with yet did not set (stood aside, deferred, left to a person) earns none.
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
                matching.peek().is_some() && matching.all(verified_row)
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

pub use crate::init_preferences::KeyboardAttachmentReceipt;

/// Whether the requested service cohort reached an actual final probe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyboardAttachmentActivation {
    /// A service or its current setup authority remains unverified.
    Unfinished,
    /// Complete final probe states, including explicit unowned choices and held services.
    Verified(KeyboardAttachmentReceipt),
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
) -> Result<KeyboardAttachmentActivation, String> {
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
        let mut receipt = KeyboardAttachmentReceipt::empty();
        for item in &items {
            if !continues() {
                return Ok(KeyboardAttachmentActivation::Unfinished);
            }
            if kept(&item.id()) {
                let observed = item.inspect_kept(sys)?;
                if registries
                    .iter()
                    .any(|id| observed.covers(std::slice::from_ref(id)))
                {
                    invalidate_once();
                }
                receipt.merge(item.kept_receipt(&observed));
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
            if let Some(verified) = item.verified() {
                receipt.merge(verified);
            } else {
                ready = false;
            }
        }
        Ok(if ready && covered.covers(registries) && continues() {
            KeyboardAttachmentActivation::Verified(receipt)
        } else {
            KeyboardAttachmentActivation::Unfinished
        })
    })();
    if !matches!(result, Ok(KeyboardAttachmentActivation::Verified(_))) {
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
    /// The items this pass holds ([`Workstation::hold`]), by id.
    held: Vec<(String, Hold)>,
    /// Canonical preparation scope; manual item collections prove no complete setting.
    preparation: Option<(WorkstationGroup, Selection)>,
}

type PreferenceSnapshot = Vec<(Target, Read<PValue>)>;

/// How a pass holds an item ([`Workstation::hold`]): leaves it alone, or makes its change only as
/// what it read back allows ([`Hold::ReadBack`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hold {
    /// Its change waits for a later pass ([`aterm_converge::put_off`]): its row reads
    /// `Deferred`, neither made nor failed, and the caller's own timer brings it back.
    Later(String),
    /// It is left as it is should it read otherwise ([`aterm_converge::left_as_is`]): its row
    /// reads what it reads now and `why`, as `NotApplicable`.
    AsIs(String),
    /// aterm set it, or found it as wanted, and read it back as `held` (its row's words, where
    /// the caller kept them). Should it read otherwise now, nothing says who changed it: a
    /// change from `held` itself (aterm's want moved since) or only to targets `held` does not
    /// name ([`Item::new_targets`]) is made, and any other is made once more while `again` is
    /// false; once aterm made it again (`again`), it is left as it is in `why`'s words, as
    /// [`Hold::AsIs`] leaves it.
    ReadBack {
        /// What it read back, in its row's words.
        held: Option<String>,
        /// Whether aterm already set it again since it first read back so.
        again: bool,
        /// Why it is left as it is, once it is.
        why: String,
    },
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
    /// The items held as read back ([`Hold::ReadBack`]) whose change this pass made only to
    /// targets their read-back did not name ([`Item::new_targets`]), by id.
    pub new_targets: Vec<String>,
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
        self.resolve_read_backs(sys);
        let mut changes = Vec::new();
        let mut snapshots = Vec::new();
        for item in &self.items {
            // A held item changes nothing in this pass ([`Workstation::hold`]): no key of
            // it is backed up for, or activated after, a change nobody makes.
            if self
                .held
                .iter()
                .any(|(id, hold)| *id == item.id() && holds_back(hold))
            {
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

    /// Decide, once and before the pass backs anything up, what each item aterm set again
    /// since it read back ([`Hold::ReadBack`] with `again`) does: one whose change is to what it
    /// read back (it reads as aterm left it, and aterm's want moved since), or only to targets
    /// its read-back did not name, is made as any change is; any other is left as it is
    /// ([`Hold::AsIs`]), whatever it reads, since a change it reads now or later is not made.
    /// What it read back stays with each one made, which the run's [`WorkstationRun::new_targets`]
    /// reads.
    fn resolve_read_backs(&mut self, sys: &dyn System) {
        let (items, ctx) = (&self.items, &self.ctx);
        for (id, hold) in &mut self.held {
            let Hold::ReadBack {
                held,
                again: true,
                why,
            } = hold
            else {
                continue;
            };
            let made = items
                .iter()
                .find(|item| item.id() == *id)
                .is_some_and(|item| {
                    matches!(item.probe(sys, ctx), Probe::Change { from, .. }
                    if held.as_deref().is_some_and(|held| {
                        held == from || item.new_targets(held, &from)
                    }))
                });
            *hold = if made {
                Hold::ReadBack {
                    held: held.take(),
                    again: false,
                    why: std::mem::take(why),
                }
            } else {
                Hold::AsIs(std::mem::take(why))
            };
        }
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

    /// [`Self::of`] at `ctx`: its accounts and console.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn of_in(ctx: Ctx, items: Vec<Box<dyn Item>>) -> Self {
        Self {
            ctx,
            ..Self::of(items)
        }
    }

    fn run_recording_with(
        mut self,
        sys: &dyn System,
        mode: Mode,
        journal: &mut dyn Journal,
    ) -> Result<WorkstationRun, String> {
        self.resolve_read_backs(sys);
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
                // One set again or never: its change is made as any change is
                // ([`Workstation::resolve_read_backs`]).
                None | Some((_, Hold::ReadBack { .. })) => vec![item],
                Some((_, Hold::Later(why))) => aterm_converge::put_off(vec![item], why),
                Some((_, Hold::AsIs(why))) => aterm_converge::left_as_is(vec![item], why),
            }
        })
        .collect();
        let rows =
            aterm_converge::run(&bound, mode, journal).map_err(|refused| refused.to_string())?;
        let new_targets = rows
            .iter()
            .filter(|row| row.outcome == Outcome::Changed)
            .filter_map(|row| {
                let held = self.held.iter().find_map(|(id, hold)| match hold {
                    Hold::ReadBack {
                        held: Some(held), ..
                    } if *id == row.id => Some(held),
                    _ => None,
                })?;
                let from = row.from.as_deref()?;
                items
                    .iter()
                    .find(|item| item.id() == row.id)
                    .is_some_and(|item| item.new_targets(held, from))
                    .then(|| row.id.clone())
            })
            .collect();
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
            new_targets,
        })
    }
}

/// Whether `hold` leaves its item's change unmade in the pass ([`Workstation::hold`]): one held as
/// read back is made as any change is, by then ([`Workstation::resolve_read_backs`]).
const fn holds_back(hold: &Hold) -> bool {
    matches!(hold, Hold::Later(_) | Hold::AsIs(_))
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
        self.apply_guarded(sys, ctx, &|| Ok(()))
    }
    fn apply_guarded(
        &self,
        sys: &dyn System,
        ctx: &Ctx,
        current: &(dyn Fn() -> Result<(), String> + Sync),
    ) -> Result<(), String> {
        current()?;
        self.validate(sys, ctx)?;
        current()?;
        self.record_attempt();
        self.inner.apply_guarded(sys, ctx, current)
    }
    fn preference_changes(
        &self,
        sys: &dyn System,
        ctx: &Ctx,
    ) -> Result<PreferenceSnapshot, String> {
        self.inner.preference_changes(sys, ctx)
    }
    fn refresh_guarded(
        &self,
        sys: &dyn System,
        ctx: &Ctx,
        current: &(dyn Fn() -> Result<(), String> + Sync),
    ) -> Option<Result<(), String>> {
        if let Err(why) = self.validate(sys, ctx) {
            return Some(Err(why));
        }
        self.record_attempt();
        let refreshed = self.inner.refresh_guarded(sys, ctx, current);
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
    fn new_targets(&self, held: &str, from: &str) -> bool {
        self.inner.new_targets(held, from)
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
    // With no terminal, an apply builds its sign-outs only for a person at the screen now, read
    // this once as the run is prepared: nothing reads presence again before the sign-outs or
    // their clicks. A read-only run never uses that apply authority to query another app's
    // account data from a headless tool.
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

/// What the running coordinator read back settled of the rows a reader asks about (`setup
/// status`' `verified`, `verified_at` and `rows=`, schema
/// [`aterm_types::macos_setup::SETUP_STATUS_SCHEMA`]): a reading of the past, whose age the
/// reader judges at its own clock ([`Self::stands_at`]), with the grants on offer beside it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadBack {
    /// The private rows read back settled, whatever their age: in place, not needed, set for
    /// macOS to take up in full at the next login, or changed since aterm set them ([`Self::changed`]).
    pub rows: Selection,
    /// When the oldest of `rows` was read (Unix seconds); `None` exactly when `rows` is empty.
    pub read_at: Option<u64>,
    /// Of `rows`, those set for macOS to take up in full at the next login (`at-login`).
    pub at_login: Selection,
    /// Of `rows`, those that changed since aterm set them and read them back, and again after
    /// aterm set them once more, or a sign-out signed in again (`differs` and verified, design
    /// §6.6): no unattended pass sets them again, and they are not in place. Nothing tells who
    /// changed them, so nothing says they are the person's.
    pub changed: Selection,
    /// The grants on offer, where the reader asked about the Permissions rows: neither granted
    /// nor a person's step, and nothing waits on them.
    pub offered: Vec<Access>,
}

/// A read-back of no row: nothing read back, so nothing to date. Not derived, since a
/// [`Selection`]'s own default selects every setting (the default plan), which would claim rows
/// nothing read.
impl Default for ReadBack {
    fn default() -> Self {
        Self {
            rows: Selection::none(),
            read_at: None,
            at_login: Selection::none(),
            changed: Selection::none(),
            offered: Vec::new(),
        }
    }
}

impl ReadBack {
    /// This read-back of the rows `requested` names alone: their oldest reading is no younger
    /// than all rows' oldest, so the date is kept while one of them is left. The grants on offer
    /// stay only where `requested` asks about the Permissions rows.
    #[must_use]
    pub fn scoped(self, requested: Selection) -> Self {
        let rows = self.rows.intersection(requested);
        Self {
            rows,
            read_at: self.read_at.filter(|_| !rows.is_empty()),
            at_login: self.at_login.intersection(requested),
            changed: self.changed.intersection(requested),
            offered: if requested.contains(Setting::Permissions) {
                self.offered
            } else {
                Vec::new()
            },
        }
    }

    /// Whether this read-back stands as a reading of now at `now` (Unix seconds): its oldest
    /// reading was taken no later than `now` and no more than
    /// [`aterm_types::macos_setup::READING_STANDS_SECS`] before it
    /// ([`aterm_types::macos_setup::Read::current_at`]). One with no row to date stands. Past
    /// that it says how the rows were when last read, never how they are now: nothing reads a
    /// private row again without a person there.
    #[must_use]
    pub fn stands_at(&self, now: u64) -> bool {
        self.read_at
            .is_none_or(|at| aterm_types::macos_setup::Read::At(at).current_at(now))
    }

    /// Whether this read-back's age can be told at `now` (Unix seconds; `None` where this Mac's
    /// clock could not be read): it has no row to date, or its oldest reading is dated no later
    /// than `now`. One dated after the clock (the clock moved back since) is a reading of no
    /// time this Mac can name, so it never passes as set up: a reader says it unread.
    #[must_use]
    pub fn dated_at(&self, now: Option<u64>) -> bool {
        self.read_at
            .is_none_or(|at| now.is_some_and(|now| at <= now))
    }

    /// `rows` (a noun phrase: `the requested preference rows`) as this read-back tells them at
    /// `now` (Unix seconds; `None` where this Mac's clock could not be read): read as wanted that
    /// long ago while the reading stands ([`Self::stands_at`]), else set up as last read, with
    /// its age and who reads them again, never as in place now (design §3.5), or at a time this
    /// Mac's clock cannot date ([`Self::dated_at`]); then its [`Self::notes`]. The rows changed
    /// since aterm set them ([`Self::changed`]) are left out of `rows` by name (`the requested
    /// preference rows other than Dock`), so they are never said read as wanted or set up, and
    /// the notes say them; where they are every row read back, the reading names them alone
    /// (`System State read Dock 1 min ago`), as wanted or set up never. Only the notes for a
    /// read-back with no row to date.
    #[must_use]
    pub fn said(&self, rows: &str, now: Option<u64>) -> String {
        let notes = self.notes();
        let Some(at) = self.read_at else {
            return notes.trim_start_matches("; ").to_owned();
        };
        let changed = names_of(self.changed);
        if !changed.is_empty() && names_of(self.rows).iter().all(|row| changed.contains(row)) {
            let rows = changed.join(", ");
            return match now {
                Some(now) if self.stands_at(now) => {
                    format!(
                        "System State read {rows} {} ago{notes}",
                        age_words(now - at)
                    )
                }
                Some(now) if at <= now => format!(
                    "System State last read {rows}, {} ago; not read since: \
                     {READ_THEM_NOW}{notes}",
                    age_words(now - at)
                ),
                _ => format!(
                    "System State last read {rows} at a time this Mac's clock cannot date; not \
                     read since: {READ_THEM_NOW}{notes}"
                ),
            };
        }
        let rows = if changed.is_empty() {
            rows.to_owned()
        } else {
            format!("{rows} other than {}", changed.join(", "))
        };
        match now {
            Some(now) if self.stands_at(now) => format!(
                "System State read {rows} as wanted {} ago{notes}",
                age_words(now - at)
            ),
            Some(now) if at <= now => format!(
                "set up as System State last read {rows}, {} ago; not read since: \
                 {READ_THEM_NOW}{notes}",
                age_words(now - at)
            ),
            // The clock moved back since, or could not be read: the reading's age is unknown.
            _ => format!(
                "System State last read {rows} at a time this Mac's clock cannot date; not read \
                 since: {READ_THEM_NOW}{notes}"
            ),
        }
    }

    /// What the rows read back say beyond `as wanted`, in the rows' names: `; Trackpad scrolling
    /// takes full effect at your next login; Dock changed since aterm set it, which setup leaves
    /// as it is; Full Disk Access on offer, which nothing waits for`. Empty when nothing more.
    #[must_use]
    pub fn notes(&self) -> String {
        let mut notes = String::new();
        match names_of(self.at_login).as_slice() {
            [] => {}
            [one] => notes.push_str(&format!("; {one} takes full effect at your next login")),
            many => notes.push_str(&format!(
                "; {} take full effect at your next login",
                many.join(", ")
            )),
        }
        match names_of(self.changed).as_slice() {
            [] => {}
            [one] => notes.push_str(&format!(
                "; {one} changed since aterm set it, which setup leaves as it is"
            )),
            many => notes.push_str(&format!(
                "; {} changed since aterm set them, which setup leaves as they are",
                many.join(", ")
            )),
        }
        if !self.offered.is_empty() {
            let offered: Vec<&str> = self.offered.iter().map(|access| access.name()).collect();
            notes.push_str(&format!(
                "; {} on offer, which nothing waits for",
                offered.join(", ")
            ));
        }
        notes
    }
}

/// The names of the rows `rows` selects, in the page's order.
fn names_of(rows: Selection) -> Vec<&'static str> {
    Setting::ALL
        .into_iter()
        .filter(|setting| rows.contains(*setting))
        .map(Setting::name)
        .collect()
}

/// The existing coordinator's observation, without reading another app's settings or
/// authorizing any change. Missing evidence is distinct from a genuine permission boundary,
/// and a row nothing read is distinct from both.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkstationObservation {
    /// The coordinator reads every row the reader asked about as wanted: these private rows
    /// read back so, each at some time ([`ReadBack`]), a reading of now only while it stands
    /// ([`ReadBack::stands_at`]), else how they were when last read; and the grants asked about
    /// read as wanted or on offer. Its saved plan or a group left as is proves no unobserved
    /// value.
    Complete(ReadBack),
    /// Background work or an observation is still pending for a row asked about.
    Waiting(String),
    /// The coordinator observed a real step only a person can complete for a row asked about.
    Permission(String),
    /// No row asked about waits or needs a person, and these (comma-separated `setup=` words,
    /// [`aterm_types::macos_setup::Subject::setup_word`]) have no usable reading in the
    /// coordinator: a private row's read runs only while System State is in front with a person
    /// and none ran (or none since a stage that could change it), or a read ran here and said
    /// nothing (a grant this process cannot read). Nothing waits on them. `read_back` is the
    /// coordinator's as for [`Self::Complete`], which a check that asks about none of `rows` is.
    Unread { rows: String, read_back: ReadBack },
    /// No usable coordinator observation is available.
    Unavailable(String),
}

impl WorkstationObservation {
    /// This observation of the Mac settings `requested` names: a completion that has not
    /// read back every one of them reads as still waiting, never complete.
    #[must_use]
    pub fn for_scope(self, requested: Selection) -> Self {
        match self {
            Self::Complete(read_back)
                if Setting::ALL.into_iter().any(|setting| {
                    requested.private().contains(setting) && !read_back.rows.contains(setting)
                }) =>
            {
                Self::Waiting(
                    "background setup has not verified all requested preference rows".into(),
                )
            }
            // The rows this check does not ask about, and their age, say nothing of it.
            Self::Complete(read_back) => Self::Complete(read_back.scoped(requested)),
            // A row this check does not ask about says nothing of it, read or not: with none of
            // its rows left unread, what the coordinator read back of them decides.
            Self::Unread { rows, read_back } => {
                let asked: Vec<&str> = rows
                    .split(',')
                    .filter(|word| {
                        Setting::ALL.into_iter().any(|setting| {
                            requested.contains(setting)
                                && aterm_types::macos_setup::Subject::Setting(setting).setup_word()
                                    == *word
                        })
                    })
                    .collect();
                if asked.is_empty() {
                    Self::Complete(read_back).for_scope(requested)
                } else {
                    Self::Unread {
                        rows: asked.join(","),
                        read_back: read_back.scoped(requested),
                    }
                }
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
    fn probe(&self, sys: &dyn System, _ctx: &Ctx) -> Probe {
        match &self.observation {
            Some(WorkstationObservation::Complete(read_back)) if read_back.read_at.is_none() => {
                Probe::Ok {
                    now: "this check requests no preference row the running aterm reads back"
                        .into(),
                }
            }
            // A reading dated after this Mac's clock (it moved back since) has no age to tell:
            // it is unread, never a pass (design §3.5), and its next reading dates it.
            Some(WorkstationObservation::Complete(read_back))
                if !read_back.dated_at(Some(sys.now())) =>
            {
                Probe::Deferred {
                    why: read_back.said("the requested preference rows", Some(sys.now())),
                }
            }
            // A row changed since aterm set it is left as it is, not in place: the check reads
            // it as at the console it reads a key left as it is, never as ok.
            Some(WorkstationObservation::Complete(read_back)) if !read_back.changed.is_empty() => {
                Probe::NotApplicable {
                    why: read_back.said("the requested preference rows", Some(sys.now())),
                }
            }
            // Read back as wanted: while the reading stands, as now; past that, set up as last
            // read, which only a person at System State, or this check at this Mac's console,
            // reads again. Told with its age, never as a reading of now (design §3.5).
            Some(WorkstationObservation::Complete(read_back)) => Probe::Ok {
                now: read_back.said("the requested preference rows", Some(sys.now())),
            },
            // Neither a wait nor a failure: nothing in the coordinator read these rows, and a
            // check from here reads no other app's settings.
            Some(WorkstationObservation::Unread { rows, .. }) => Probe::Deferred {
                why: format!("not read yet: {rows}; {READ_THEM_NOW}"),
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

/// Who reads the private rows a relay cannot (design §3.5): no read of another app's data runs
/// without a person there to answer a dialog it may raise.
pub const READ_THEM_NOW: &str = "System State reads them while it is in front with a person, \
                                 and `aterm init --check` does at this Mac's console";

/// The unit a relayed reading's age is told in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AgeUnit {
    Secs,
    Mins,
    Hours,
}

impl AgeUnit {
    const fn secs(self) -> u64 {
        match self {
            Self::Secs => 1,
            Self::Mins => 60,
            Self::Hours => 3600,
        }
    }

    const fn word(self) -> &'static str {
        match self {
            Self::Secs => "s",
            Self::Mins => "min",
            Self::Hours => "h",
        }
    }
}

/// `secs` as the count and unit a relayed reading's age is told in: seconds under two minutes,
/// minutes under two hours, else hours, the count rounded up so a reading is never told as newer
/// than it is.
const fn age_told(secs: u64) -> (u64, AgeUnit) {
    let unit = match secs {
        0..120 => AgeUnit::Secs,
        120..7200 => AgeUnit::Mins,
        _ => AgeUnit::Hours,
    };
    (secs.div_ceil(unit.secs()), unit)
}

/// `secs` as the few words a relayed reading's age is told in ([`age_told`]).
#[must_use]
pub fn age_words(secs: u64) -> String {
    let (count, unit) = age_told(secs);
    format!("{count} {}", unit.word())
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
    /// Whether the item with this id is a key that changed after aterm set it (design §6.6,
    /// [`Records::kept`]): it is left as it is, in [`Self::kept_why`]'s words.
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

/// Whether `kept` ([`Records::kept`]) keeps the item with id `id` as it is: every item, where
/// the record could not be read.
fn keeps_key(kept: &Result<Vec<String>, String>, id: &str) -> bool {
    kept.as_ref()
        .map_or(true, |ids| ids.iter().any(|kept| kept == id))
}

/// [`Desk::leaves`] of a stage that leaves no item out, and [`Desk::kept`] of one that keeps
/// none.
const fn leaves_none(_id: &str) -> bool {
    false
}

/// How a row that changed after aterm set it ends ([`Instead::Kept`]): `aterm init` leaves it,
/// never says whose change it is (nothing tells), and names the ways that set it again (design
/// §6.6). It mirrors the words the window's unattended pass uses.
const KEPT_CHANGED: &str = "changed since aterm set it: aterm init leaves it as it is, and Set up \
                            now in System State or aterm doctor --apply sets it again";

/// How a row ends when the record of the keys that changed since aterm set them cannot be read:
/// `aterm init` sets none of them rather than one of those, and records none as set.
const KEPT_UNREAD: &str = "left as it is: aterm could not read which settings changed since it \
                           set them, so it changed none of them; aterm doctor --apply sets them \
                           again";

/// What a Desktop-lane item does in place of a change it cannot make now ([`Unchanged`]).
#[derive(Clone, Copy, Debug)]
enum Instead<'d> {
    /// Stand aside for another setup's hold on System State's records, in these words: the row
    /// reads as it is and changes nothing (exit 75, "a lock").
    Lock(&'d str),
    /// Leave a key that changed after aterm set it ([`KEPT_CHANGED`]), in these words:
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
        self.apply_guarded(sys, ctx, &|| Ok(()))
    }
    fn apply_guarded(
        &self,
        sys: &dyn System,
        ctx: &Ctx,
        current: &(dyn Fn() -> Result<(), String> + Sync),
    ) -> Result<(), String> {
        match self.held() {
            Some(why) => Err(why.clone()),
            None => self.finished(self.inner.apply_guarded(sys, ctx, &|| {
                current()?;
                self.current()
            })),
        }
    }
    fn preference_changes(
        &self,
        sys: &dyn System,
        ctx: &Ctx,
    ) -> Result<Vec<(Target, Read<PValue>)>, String> {
        self.inner.preference_changes(sys, ctx)
    }
    fn refresh_guarded(
        &self,
        sys: &dyn System,
        ctx: &Ctx,
        current: &(dyn Fn() -> Result<(), String> + Sync),
    ) -> Option<Result<(), String>> {
        self.inner
            .refresh_guarded(sys, ctx, &|| {
                current()?;
                self.current()
            })
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
    /// The vault, downloaded tools and GitHub: installs and the network. The root transaction
    /// has its own lane, and develop follows both.
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

/// Holds root work and the Network lane after the vault's join until the base layer lets the
/// store lock go ([`Latch`]): the root transaction and package passes may take it.
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

/// An already-run predecessor, used only to preserve the develop stage's existing `after`
/// edges. It performs no probe, effect, refresh or journal write on the original item, and
/// its synthetic row is never published. Only the original pass's successful outcomes admit
/// a dependent apply; a plan still probes its dependents as usual.
struct Completed<'a> {
    inner: &'a dyn Item,
    row: &'a ItemReport,
}

impl Item for Completed<'_> {
    fn id(&self) -> String {
        self.row.id.clone()
    }
    fn family(&self) -> &'static str {
        self.inner.family()
    }
    fn title(&self) -> String {
        self.row.title.clone()
    }
    fn probe(&self, _sys: &dyn System, _ctx: &Ctx) -> Probe {
        if matches!(self.row.outcome, Outcome::Ok | Outcome::Changed) {
            Probe::Ok {
                now: self.row.to.clone().unwrap_or_else(|| self.title()),
            }
        } else {
            Probe::Unusable {
                why: self.row.why.clone().unwrap_or_else(|| self.title()),
            }
        }
    }
    fn apply(&self, _sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        Err("a completed predecessor changes nothing".into())
    }
    fn read_aside(&self) -> Option<aterm_converge::StoodAside> {
        self.row.stood_aside.clone()
    }
}

/// Run `base` items, release the store lock, and then run `base.clt`, `groups`, and `develop`
/// independently: the Desktop lane and the vault's join in the Network lane start at once;
/// the root lane and the rest of the Network lane (the toolset and GitHub) once the base layer
/// let the store lock go ([`AfterBase`]), and the Base lane follows the base layer. Develop
/// runs once both root and network work finish, preserving its predecessor outcomes. Each
/// row reaches `hooks.on_row` as its item finishes, and the Desktop lane runs
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
    // What setup's one elevation still owes beside the tools, read after `base.clt`'s own row.
    let steps = clt.as_ref().map(crate::init_clt::CommandLineTools::steps);
    // The last inner deferral must not resend a stop while another lane still owns a child.
    // Keep one owner until every lane and the develop stage have returned and been joined.
    let _stops = (mode == Mode::Apply && clt.is_some()).then(|| sys.defer_stops());
    let order: Vec<String> = clt
        .iter()
        .map(Item::id)
        .chain(steps.iter().map(Item::id))
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
    // Downloaded tools and repository clones need no Apple compiler. They proceed beside
    // the one root transaction, rather than delaying its LAN helper or waiting on its CLT
    // download or password.
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
        .collect();
    if !gated.is_empty() {
        network.push((&gate as &dyn Item, crate::init_clt::place()));
        network.extend(gated);
    }
    // The root steps' read-only row follows `base.clt` in its lane: it reads what setup's one
    // elevation still owes once the tools' own row has run.
    let root: Vec<(&dyn Item, Place)> = clt.as_ref().map_or_else(Vec::new, |clt| {
        let mut root = vec![
            (&gate as &dyn Item, crate::init_clt::place()),
            (clt as &dyn Item, crate::init_clt::place()),
        ];
        root.extend(
            steps
                .as_ref()
                .map(|steps| (steps as &dyn Item, crate::init_clt::place())),
        );
        root
    });
    let develop: Vec<(&dyn Item, Place)> = develop
        .iter()
        .map(|(item, place)| (item.as_ref(), place.clone()))
        .collect();
    let journal = Mutex::new(journal);
    let run = |lane: &[(&dyn Item, Place)],
               prior: &[(&dyn Item, Place)]|
     -> Result<Vec<ItemReport>, String> {
        if lane.is_empty() {
            return Ok(Vec::new());
        }
        let completed: std::collections::BTreeSet<String> =
            prior.iter().map(|(item, _)| item.id()).collect();
        let bound = crate::engine::bind(prior.iter().chain(lane).cloned(), sys, ctx);
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
                if row.id != AFTER_BASE && !completed.contains(&row.id) {
                    on_row(row);
                }
            })
            .map_err(|refused| refused.to_string())?;
        rows.retain(|row| row.id != AFTER_BASE && !completed.contains(&row.id));
        Ok(rows)
    };
    let network_lane = || run(&network, &[]);
    let root_lane = || run(&root, &[]);
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
        run(
            &ran.iter()
                .zip(&unchanged)
                .map(|((inner, place), unchanged)| {
                    (
                        unchanged.as_ref().map_or(*inner, |item| item as &dyn Item),
                        place.clone(),
                    )
                })
                .collect::<Vec<_>>(),
            &[],
        )
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
        let root_thread = (!root.is_empty())
            .then(|| {
                std::thread::Builder::new()
                    .name("aterm-init-root".into())
                    .spawn_scoped(threads, root_lane)
                    .ok()
            })
            .flatten();
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
        // Start a refused spawn inline while its independent peer is already running. Never
        // join a worker before the refused peer has had a chance to begin.
        let root_inline = root_thread.is_none().then(root_lane);
        let network_inline = network_thread.is_none().then(network_lane);
        let spotlight_rows = run(&spotlight, &[]);
        let joined = |thread: Option<std::thread::ScopedJoinHandle<'_, _>>,
                      inline: &dyn Fn() -> Result<Vec<ItemReport>, String>| {
            match thread {
                Some(thread) => thread
                    .join()
                    .unwrap_or_else(|_| Err("a setup lane stopped unexpectedly".into())),
                None => inline(),
            }
        };
        let root_rows = match root_thread {
            Some(thread) => joined(Some(thread), &root_lane),
            None => root_inline.expect("the refused root spawn ran inline"),
        };
        let network_rows = match network_thread {
            Some(thread) => joined(Some(thread), &network_lane),
            None => network_inline.expect("the refused network spawn ran inline"),
        };
        let develop_rows = match (&root_rows, &network_rows) {
            (Ok(root_rows), Ok(network_rows)) => {
                let predecessors: Vec<_> = root_rows
                    .iter()
                    .chain(network_rows)
                    .filter_map(|row| {
                        root.iter()
                            .chain(&network)
                            .find(|(item, _)| item.id() == row.id)
                            .map(|(inner, place)| (Completed { inner: *inner, row }, place.clone()))
                    })
                    .collect();
                let prior: Vec<_> = predecessors
                    .iter()
                    .map(|(item, place)| (item as &dyn Item, place.clone()))
                    .collect();
                run(&develop, &prior)
            }
            _ => Ok(Vec::new()),
        };
        drop(done);
        let desktop_rows = joined(desktop_thread, &desktop_lane);
        (
            base_rows,
            [
                spotlight_rows,
                root_rows,
                network_rows,
                develop_rows,
                desktop_rows,
            ],
        )
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

/// Whether `row` is evidence its setting is as wanted now: in place, changed with the read-back
/// confirming it, or not applicable here. What a setup record says is set or verified
/// ([`verified_mac_settings`], [`Held::verified`]) rests on these alone; a row [`left`] to
/// someone else completes an apply's exit ([`counts`]), never that record.
fn verified_row(row: &ItemReport) -> bool {
    matches!(
        row.outcome,
        Outcome::Ok | Outcome::Changed | Outcome::NotApplicable
    )
}

/// Whether `row` is not a run's to finish ([`counts`]): a grant only the person switches on
/// ([`is_grant`]), a row deferred to a person ([`waits_for_a_person`]), one owed once this Mac
/// is let into the vault ([`owed`]), a batch the person's later hold stopped ([`held_late`]),
/// the Command Line Tools deferred ([`crate::init_clt::deferred`]), or the vault's look deferred
/// until the LAN broker, which setup's one elevation installs, answers
/// ([`crate::init_secrets::waits_for_lan_broker`]).
/// `--check` reads such a row as not in place and exits 77 for it ([`status`]): what is missing
/// waits on a person's (or the vault's, or the elevation's) permission, which is neither done
/// nor a failure.
fn left(row: &ItemReport) -> bool {
    is_grant(row)
        || waits_for_a_person(row)
        || owed(row)
        || held_late(row)
        || crate::init_clt::deferred(row)
        || (row.outcome == Outcome::Deferred && crate::init_secrets::waits_for_lan_broker(row))
}

/// Whether `row` is deferred to a person: the stand-in for the workstation rows a person at
/// this Mac's console makes ([`AwayFromConsole`]).
fn waits_for_a_person(row: &ItemReport) -> bool {
    row.outcome == Outcome::Deferred && row.id == AWAY
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
#[path = "init_tests.rs"]
mod tests;
