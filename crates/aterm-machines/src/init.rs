// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The local, per-user macOS setup, sharing the machines probe/apply/read-back engine.

use crate::engine::{Journal, Place};
use crate::items::defaults::Target;
use crate::items::{Ctx, Item};
use crate::model::{Effect, HostFacts, ItemReport, Mode, Outcome, PValue, Probe, Read};
use crate::port::{Cmd, Live, System};
use aterm_converge::{Class, Layer, Remedy, Report, ReportMode, Requires};
use aterm_types::macos_setup::{Selection, Setting};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex, PoisonError};

/// `aterm init` usage, printed by `--help` and after a usage error.
pub const USAGE: &str = "\
aterm init [--plan | --check] [--json] [--develop | --no-develop]
           [--only <item,...>] [--vault <url|path>]
    Set up this Mac for this account, applying and reading back each item;
    safe to run again. Run as the logged-in user, without sudo.

    First the base items, over SSH too: what bare `aterm doctor` restores (the
    installed commands and their ~/.local/bin links, the shell integration, the
    reroute stubs, the rustup link, exec roots), the AI CLI account roster
    (when there is a fleet accounts source), and Apple's Command Line Tools
    when missing (base.clt: one sudo password prompt in this terminal).

    Then, only when this account is the one logged in at this Mac's console
    (over SSH too), this login's settings: Caps Lock as Control; natural
    scrolling off, trackpad scrolling on; desktop icons shown; widgets hidden
    (Notification Center's only with Full Disk Access); a Dock of only aterm,
    System Settings and Safari; Messages and FaceTime signed out (which may need
    Accessibility and Automation permission for aterm); Rust kept out of
    Spotlight (rustup's minimal profile, no rust-docs, build caches in .noindex
    folders). It joins the secrets vault (aterm help secrets): the ALab vault
    when it answers on this network (asking only its join password), else the
    one --vault names; with neither, it asks nothing. A [github] table in
    ~/.config/aterm/init.toml keeps its token in the login keychain (unless
    git's own helpers, such as the secrets vault's, already hand it back),
    clones the owner's repositories into ~ and fast-forwards each clean
    checkout that is behind (aterm help init).

    Last, on a develop machine (one `aterm init --develop` recorded), the
    develop items.

    --plan    Report the changes without making them.
    --check   Read only; exit nonzero if anything still needs attention.
    --json    Print the per-item report as JSON.
    --develop Record this Mac as a develop machine ([develop] in init.toml)
              and run base and develop only. Each recorded aterm checkout is
              fetched and fast-forwarded when it is clean, on its default
              branch and has no commit of its own (else left as it is, saying
              why; never a reset, stash, push or hook), with its vendor/astream
              submodule where that loses nothing; its submodule.recurse is set
              true. In each checkout not left as it is, init then runs the
              checkout's own `targo --unverified --locked verify provision`. Run
              inside a checkout to record it (a linked worktree is refused);
              elsewhere it records the [github] owner's aterm checkouts in ~.
    --no-develop  Remove the [develop] table and run base only; the tools stay
              as cache.
    --only    Run only the items named: a base id such as base.clt or
              base.shell.rc, a family such as shell or pkg, or base;
              workstation (every console-only step); a develop id such as
              develop.checkout or develop.checkout@~/aterm, a family (profile,
              checkout, tooling), or develop.
    --vault   Join this secrets vault (a URL, #<signer> pin allowed, or a
              path) when this Mac has none: read once at once, and asked for
              again while nothing answers there.
    --help    Show this help without inspecting or changing the Mac.

    Exit: 0 done, a vault join an admin has yet to approve included (the
    subscriber finishes it); when only items that stood aside are left, 75 if
    one waits on a lock (such as an install pass's store lock) or on another
    step (GitHub's sign-in on that join: run init again once it is approved),
    else 77 if one waits on a permission you grant, else 69 (the network); 1
    anything else; 2 usage.
";

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
}

impl Options {
    /// Whether this run converges the workstation layer.
    fn workstation(&self) -> bool {
        (self.only.is_empty() && self.develop.is_none())
            || self.only.iter().any(|word| word == "workstation")
    }
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
    if options.develop.is_some() && options.only.iter().any(|word| word == "workstation") {
        return Err("--develop and --no-develop cannot be combined with --only workstation".into());
    }
    if options.vault.is_some() && !options.workstation() {
        return Err("--vault joins in the workstation layer, which this run leaves out".into());
    }
    Ok(options)
}

/// Base layer `aterm init` runs before its own items.
pub struct Base {
    /// The items, in run order.
    pub items: Vec<Box<dyn aterm_converge::Item>>,
    /// Store lock held while base items run and released before `base.clt` and workstation items.
    pub held: Option<Box<dyn std::any::Any>>,
    /// `base.clt` ([`crate::init_clt`]), run once the lock is released.
    pub clt: Option<crate::init_clt::CommandLineTools>,
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
            targo,
        } = self;
        Self {
            items: aterm_converge::select(items, only).0,
            held,
            clt: clt.filter(|_| only.iter().any(|word| crate::init_clt::selected(word))),
            targo,
        }
    }
}

/// Run `aterm init`.
pub fn main_entry(
    args: Vec<OsString>,
    base: &dyn Fn(bool) -> Base,
    full_disk_access: &dyn Fn() -> Option<bool>,
) -> ExitCode {
    let options = match options(&args) {
        Ok(options) => options,
        Err(why) => {
            eprintln!("aterm init: {why}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    if options.help {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let composed = base(false);
    let unnamed = unnamed(&composed, &options.only);
    if !unnamed.is_empty() {
        eprintln!(
            "aterm init: --only {} names no item ({ONLY_WORDS})\n{USAGE}",
            unnamed.join(",")
        );
        return ExitCode::from(2);
    }
    if !crate::sessions::is_live_account() {
        eprintln!("aterm init: run in your own macOS login and home directory");
        return ExitCode::FAILURE;
    }
    // The look for the ALab vault overlaps reading the Mac (its context and the develop
    // layer); the secrets step takes its answer as the workstation groups are prepared, which
    // is before the base layer runs, so every question comes before any change.
    let ahead =
        (options.action == Action::Apply && options.workstation() && options.vault.is_none())
            .then(crate::init_secrets::look_ahead)
            .flatten();
    let groups = |sys: &dyn System, ctx: &Ctx, mode: Mode| {
        prepare_groups(sys, ctx, mode, ahead.clone(), options.vault.as_deref())
    };
    let sys = Live::new().with_full_disk_access(if options.workstation() {
        full_disk_access()
    } else {
        None
    });
    let cwd = std::env::current_dir().ok();
    let result = sys
        .journal()
        .map_err(Refused::Failed)
        .and_then(|mut journal| {
            converge(
                &sys,
                &options,
                &Front {
                    base: &|applies| base(applies),
                    groups: &groups,
                    targo: composed.targo.clone(),
                    cwd: cwd.as_deref(),
                },
                journal.as_mut(),
            )
        });
    let rows = match result {
        Ok(rows) => rows,
        Err(Refused::Usage(why)) => {
            eprintln!("aterm init: {why}\n{USAGE}");
            return ExitCode::from(2);
        }
        Err(Refused::Failed(why)) => {
            eprintln!("aterm init: {why}");
            return ExitCode::FAILURE;
        }
    };
    let complete = complete(&rows, options.action);
    let code = status(&rows, options.action);
    if options.json {
        let mode = match options.action {
            Action::Apply => ReportMode::Apply,
            Action::Plan => ReportMode::Plan,
            Action::Check => ReportMode::Check,
        };
        match aterm_json::to_string(&Report::new(mode, complete, rows)) {
            Ok(report) => println!("{report}"),
            Err(why) => {
                eprintln!("aterm init: cannot encode report: {why}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        for row in &rows {
            let word = if waiting(row) {
                "waiting"
            } else {
                word(row.outcome)
            };
            println!("{word}: {}", row.title);
            if let (Some(from), Some(to)) = (&row.from, &row.to) {
                println!("    {from} → {to}");
            } else if let (Outcome::Ok, Some(Layer::Develop), Some(now)) =
                (row.outcome, row.layer, &row.to)
            {
                println!("    {now}");
            }
            if let Some(why) = &row.why {
                println!("    {why}");
            }
        }
        let summary = match (options.action, complete) {
            (Action::Plan, true) => "plan ready; no changes applied",
            (_, true) if rows.iter().any(waits_by_itself) => {
                "done; the vault join finishes by itself once an admin approves it"
            }
            (_, true) => "all requested settings verified",
            (_, false) => "settings still need attention; see the rows above",
        };
        println!("aterm init: {summary}");
    }
    ExitCode::from(code)
}

/// The develop layer as `aterm doctor` reads it on this Mac ([`crate::init_develop::doctor`]),
/// for the user `aterm init --develop` would converge, in the context init builds when it leaves
/// the workstation out (root refused). The context is built only once the account's own
/// `init.toml` has a `[develop]` table or cannot be read, or an `--only` word asks
/// ([`crate::init_develop::may_record`]), so on a Mac that records no develop layer doctor runs
/// nothing for it (no `id -un`, no `dscl`) and shows no row. The caller asks only in the
/// account's own shell.
pub struct Doctor<S: System = Live> {
    sys: S,
    /// The account's home as its account record names it (`getpwuid`), where its `init.toml`
    /// is read first; `None` builds the context straight away.
    home: Option<PathBuf>,
    targo: Option<crate::init_develop::Targo>,
    ctx: std::sync::OnceLock<Result<Ctx, String>>,
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
        }
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
                          or pkg, or base; workstation; a develop id such as \
                          develop.checkout@~/aterm, a family such as checkout, or develop";

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
}

/// Converge the requested base, workstation, and develop layers.
fn converge(
    sys: &dyn System,
    options: &Options,
    front: &Front<'_>,
    journal: &mut dyn Journal,
) -> Result<Vec<ItemReport>, Refused> {
    let mode = if options.action == Action::Apply {
        Mode::Apply
    } else {
        Mode::Plan
    };
    let only = &options.only;
    let workstation = options.workstation();
    let ctx = if workstation {
        local_context(sys)?
    } else {
        context_for_selected(sys, WorkstationGroup::All, Selection::none())?
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
    let groups = if workstation {
        (front.groups)(sys, &ctx, mode)?
    } else {
        Vec::new()
    };
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
    )?)
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
        return Err("run as the user currently logged in at this Mac's console".into());
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
        return Err("run as the user currently logged in at this Mac's console".into());
    }
    Ok(ctx)
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

/// How a run prepares the workstation's groups ([`prepare_groups`], or a test's stand-in).
type Prepare<'a> = dyn Fn(&dyn System, &Ctx, Mode) -> Result<Vec<Group>, String> + 'a;

/// Prepare workstation groups, reading optional profiles and prompting for vault credentials
/// before probing; the look for the ALab vault answers with `ahead`'s, and `vault` is
/// `--vault`'s.
fn prepare_groups(
    sys: &dyn System,
    ctx: &Ctx,
    mode: Mode,
    ahead: Option<Arc<crate::init_secrets::Lookahead>>,
    vault: Option<&str>,
) -> Result<Vec<Group>, String> {
    if ctx.console.is_none() {
        return Ok(vec![(
            vec![Box::new(WorkstationAtConsole)],
            workstation_place(Class::Person),
        )]);
    }
    let secrets = crate::init_secrets::items(
        sys,
        ctx,
        crate::init_secrets::live(ctx, ahead)?,
        mode,
        vault,
    )?;
    groups(sys, ctx, secrets)
}

/// The optional workstation items cannot be enumerated without reading their inputs.
struct WorkstationAtConsole;

impl Item for WorkstationAtConsole {
    fn id(&self) -> String {
        "init.workstation".into()
    }
    fn family(&self) -> &'static str {
        "init"
    }
    fn title(&self) -> String {
        "Workstation setup at this Mac's console".into()
    }
    fn probe(&self, _sys: &dyn System, _ctx: &Ctx) -> Probe {
        Probe::Blocked {
            why: aterm_converge::NOT_AT_CONSOLE.into(),
        }
    }
    fn apply(&self, _sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        Err(aterm_converge::NOT_AT_CONSOLE.into())
    }
}

/// Workstation item groups in run order, each paired with its [`Place`].
fn groups(
    sys: &dyn System,
    ctx: &Ctx,
    secrets: crate::init_secrets::Secrets,
) -> Result<Vec<Group>, String> {
    let place = workstation_place;
    Ok(vec![
        (
            crate::init_preferences::items(ctx)?,
            place(Class::Preference),
        ),
        (crate::init_trackpad::items(ctx)?, place(Class::Preference)),
        (
            crate::init_trackpad::behavior_items(ctx)?,
            place(Class::Preference),
        ),
        (crate::init_spotlight::items(ctx)?, place(Class::Asked)),
        (crate::init_accounts::items(ctx)?, place(Class::Preference)),
        (secrets.items, place(Class::Install)),
        (
            crate::init_github::items(sys, ctx, secrets.waited_on.as_deref())?,
            place(Class::Install),
        ),
    ])
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
    journal: &mut dyn Journal,
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
    )
}

/// Second-pass narrowing: `--only` selectors and whether `develop.record` is always kept.
struct Narrowing<'a> {
    only: &'a [String],
    record: bool,
}

/// Run `base` items, release the store lock, and then run `base.clt`, `groups`, and `develop`.
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
    journal: &mut dyn Journal,
) -> Result<Vec<ItemReport>, String> {
    let scope = aterm_converge::Scope {
        at_console: ctx.console.is_some(),
        ..aterm_converge::Scope::EVERY
    };
    let Base {
        items, held, clt, ..
    } = base;
    let mut rows = aterm_converge::run_scoped(&items, mode, scope, journal, &mut |_| {})
        .map_err(|refused| refused.to_string())?;
    drop(held);
    let clt = clt
        .as_ref()
        .map(|clt| (clt as &dyn Item, crate::init_clt::place()));
    let bound = crate::engine::bind(
        clt.into_iter()
            .chain(groups.iter().flat_map(|(items, place)| {
                items.iter().map(move |item| (item.as_ref(), place.clone()))
            }))
            .chain(
                develop
                    .iter()
                    .map(|(item, place)| (item.as_ref(), place.clone())),
            ),
        sys,
        ctx,
    );
    let bound = if narrowing.only.is_empty() {
        bound
    } else {
        let mut words = narrowing.only.to_vec();
        if narrowing.record {
            words.push(crate::init_develop::RECORD.to_owned());
        }
        aterm_converge::select(bound, &words).0
    };
    rows.extend(
        aterm_converge::run_scoped(&bound, mode, scope, journal, &mut |_| {})
            .map_err(|refused| refused.to_string())?,
    );
    Ok(rows)
}

/// Collect only what this setup uses, avoiding fleet, TCC database and unrelated app probes.
fn local_context(sys: &dyn System) -> Result<Ctx, String> {
    context_for(sys, WorkstationGroup::All)
}

fn context_for(sys: &dyn System, group: WorkstationGroup) -> Result<Ctx, String> {
    context_for_selected(sys, group, Selection::all())
}

fn context_for_selected(
    sys: &dyn System,
    group: WorkstationGroup,
    selected: Selection,
) -> Result<Ctx, String> {
    if sys.euid() == 0 {
        return Err("run as the logged-in user, without sudo".into());
    }
    let name = read(sys, &Cmd::new("/usr/bin/id").arg("-un"))?;
    let user = crate::facts::user(sys, name.trim())?;
    if user.uid != sys.euid() {
        return Err(format!(
            "`id -un` names {} (uid {}), not this process's uid {}",
            user.name,
            user.uid,
            sys.euid()
        ));
    }
    let at_console = sys
        .stat(Path::new("/dev/console"))
        .is_ok_and(|console| console.uid == user.uid);
    let facts = HostFacts {
        arch: read(sys, &Cmd::new("/usr/bin/uname").arg("-m"))?
            .trim()
            .into(),
        os_version: read(sys, &Cmd::new("/usr/bin/sw_vers").arg("-productVersion"))?
            .trim()
            .into(),
        console_user: at_console.then(|| user.name.clone()),
        users: vec![user.clone()],
        full_disk_access: sys.full_disk_access(),
        ..HostFacts::default()
    };
    if !at_console {
        return Ok(Ctx {
            facts,
            users: vec![user],
            console: None,
            host_name: String::new(),
            cache_dir: PathBuf::new(),
            disrupt: true,
        });
    }
    let keyboards = if group == WorkstationGroup::Accounts || !selected.contains(Setting::CapsLock)
    {
        Vec::new()
    } else {
        let command = crate::facts::hidutil_keyboards();
        let keyboard_list = sys
            .run_complete_stdout(&command)
            .map_err(|e| format!("{}: {e}", command.display()))?;
        if !keyboard_list.success() {
            return Err(format!(
                "{}: {}",
                command.display(),
                keyboard_list.describe()
            ));
        }
        crate::facts::parse_hidutil_keyboards_complete(&keyboard_list.out())?
    };
    let facts = HostFacts { keyboards, ..facts };
    Ok(Ctx {
        facts,
        users: vec![user.clone()],
        console: Some(user),
        host_name: String::new(),
        cache_dir: PathBuf::new(),
        disrupt: true,
    })
}

fn read(sys: &dyn System, cmd: &Cmd) -> Result<String, String> {
    let ran = sys
        .run(cmd)
        .map_err(|e| format!("{}: {e}", cmd.display()))?;
    if !ran.success() {
        return Err(format!("{}: {}", cmd.display(), ran.describe()));
    }
    Ok(ran.out())
}

fn complete(rows: &[ItemReport], action: Action) -> bool {
    !rows.is_empty() && rows.iter().all(|row| counts(row, action))
}

/// Whether `row` counts as done for `action`: a row that waits on an admin's approval does
/// ([`waits_by_itself`]).
fn counts(row: &ItemReport, action: Action) -> bool {
    matches!(
        row.outcome,
        Outcome::Ok | Outcome::Changed | Outcome::NotApplicable
    ) || (action == Action::Plan && row.outcome == Outcome::Planned)
        || waits_by_itself(row)
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
        id: &'static str,
        probes: Mutex<Vec<Probe>>,
    }

    impl Listed {
        fn boxed(id: &'static str, probes: Vec<Probe>) -> Box<dyn Item> {
            Box::new(Self {
                id,
                probes: Mutex::new(probes),
            })
        }
    }

    impl Item for Listed {
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
            sys.on("/usr/bin/id -un", Ran::ok("alice\n"));
            sys.on(
                "/usr/bin/dscl . -read /Groups/admin GroupMembership",
                Ran::ok("GroupMembership: alice\n"),
            );
            sys.on("/usr/bin/dscl . -read /Users//alice UniqueID PrimaryGroupID NFSHomeDirectory UserShell", Ran::ok("UniqueID: 501\nPrimaryGroupID: 20\nNFSHomeDirectory: /Users//alice\nUserShell: /bin/zsh\n"));
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
            sys.on("/usr/bin/uname -m", Ran::ok("arm64\n"));
            sys.on("/usr/bin/sw_vers -productVersion", Ran::ok("27.0\n"));
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
            sys.on("/usr/bin/id -un", Ran::ok("alice\n"));
            sys.on(
                "/usr/bin/dscl . -read /Groups/admin GroupMembership",
                Ran::ok("GroupMembership: alice\n"),
            );
            sys.on("/usr/bin/dscl . -read /Users//alice UniqueID PrimaryGroupID NFSHomeDirectory UserShell", Ran::ok("UniqueID: 501\nPrimaryGroupID: 20\nNFSHomeDirectory: /Users//alice\nUserShell: /bin/zsh\n"));
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
            sys.on("/usr/bin/uname -m", Ran::ok("arm64\n"));
            sys.on("/usr/bin/sw_vers -productVersion", Ran::ok("27.0\n"));
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
        sys.on("/usr/bin/id -un", Ran::ok("alice\n"));
        sys.on(
            "/usr/bin/dscl . -read /Groups/admin GroupMembership",
            Ran::ok("GroupMembership: alice\n"),
        );
        sys.on("/usr/bin/dscl . -read /Users//alice UniqueID PrimaryGroupID NFSHomeDirectory UserShell", Ran::ok("UniqueID: 501\nPrimaryGroupID: 20\nNFSHomeDirectory: /Users//alice\nUserShell: /bin/zsh\n"));
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
        sys.on("/usr/bin/uname -m", Ran::ok("arm64\n"));
        sys.on("/usr/bin/sw_vers -productVersion", Ran::ok("27.0\n"));
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
    fn what_the_base_layer_holds_is_released_before_a_workstation_item_is_read() {
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
            targo: None,
        };
        let groups = vec![(
            vec![Box::new(Sees {
                released: Arc::clone(&released),
                saw: Arc::clone(&saw),
            }) as Box<dyn Item>],
            place(Class::Preference),
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
            &sys,
            &alice,
            crate::init_secrets::Secrets {
                items: Vec::new(),
                waited_on: None,
            },
        )
        .unwrap();
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
                matches!(b.class(), Class::Preference | Class::Install | Class::Asked),
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

    #[test]
    fn modes_are_exclusive_and_invalid_arguments_fail_before_probes() {
        assert!(options(&["--plan".into(), "--check".into()]).is_err());
        assert!(options(&["--unknown".into(), "--help".into()]).is_err());
        assert_eq!(
            options(&["--check".into(), "--json".into()]).unwrap(),
            Options {
                action: Action::Check,
                json: true,
                help: false,
                only: Vec::new(),
                develop: None,
                vault: None,
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
            vault(&["--vault=~/vault.txt", "--only", "base"]).is_err(),
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

    #[test]
    fn root_is_refused_without_running_any_command() {
        let sys = Fake::root();
        assert!(local_context(&sys).unwrap_err().contains("without sudo"));
        assert!(sys.calls().is_empty());
    }

    #[test]
    fn under_another_console_owner_a_base_item_applies_and_a_workstation_item_is_blocked() {
        let alice_at = |console_uid: u32| {
            let mut sys = Fake::root();
            sys.euid = 501;
            sys.on("/usr/bin/id -un", Ran::ok("alice\n"));
            sys.on(
                "/usr/bin/dscl . -read /Groups/admin GroupMembership",
                Ran::ok("GroupMembership: alice\n"),
            );
            sys.on("/usr/bin/dscl . -read /Users//alice UniqueID PrimaryGroupID NFSHomeDirectory UserShell", Ran::ok("UniqueID: 501\nPrimaryGroupID: 20\nNFSHomeDirectory: /Users//alice\nUserShell: /bin/zsh\n"));
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
            sys.on("/usr/bin/uname -m", Ran::ok("arm64\n"));
            sys.on("/usr/bin/sw_vers -productVersion", Ran::ok("27.0\n"));
            sys.on(
                &crate::facts::hidutil_keyboards().display(),
                Ran::ok("Services:\n\nDevices:\n"),
            );
            sys
        };
        let refused = prepare_workstation(&alice_at(502), WorkstationGroup::Preferences)
            .err()
            .expect("refused away from the console");
        assert!(refused.contains("at this Mac's console"), "{refused}");
        for (console_uid, at_console) in [(502, false), (501, true)] {
            let sys = alice_at(console_uid);
            let ctx = local_context(&sys).unwrap();
            assert_eq!(ctx.console.is_some(), at_console);
            let applied = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
            let base = Base {
                items: vec![Box::new(BaseItem {
                    probes: Mutex::new(vec![change(), ok()]),
                    applied: std::sync::Arc::clone(&applied),
                })],
                held: None,
                clt: None,
                targo: None,
            };
            let groups = if at_console {
                vec![(
                    vec![Listed::boxed("init.test.dock", vec![change(), ok()])],
                    place(Class::Preference),
                )]
            } else {
                let before = sys.calls();
                let groups = prepare_groups(&sys, &ctx, Mode::Apply, None, None).unwrap();
                assert_eq!(sys.calls(), before, "no workstation preparation over SSH");
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
                assert_eq!(rows[1].outcome, Outcome::Blocked, "{rows:?}");
                assert_eq!(rows[1].why.as_deref(), Some(aterm_converge::NOT_AT_CONSOLE));
                assert!(!sys.calls().iter().any(|call| call.contains("defaults")
                    || call.contains("hidutil")
                    || call.contains("accounts.data")));
                assert_eq!(status(&rows, Action::Apply), 1);
            }
        }
        assert!(
            local_context(&Fake::root())
                .unwrap_err()
                .contains("without sudo")
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
        };
        let lock = || Some(StoodAside::Lock("held".into()));
        let offline = || Some(StoodAside::Offline("down".into()));
        let permission = || Some(StoodAside::Permission("not granted".into()));
        let waiting = || Some(StoodAside::Waiting("waiting for an admin".into()));
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
    fn an_unreadable_keyboard_list_at_the_console_is_refused_before_preferences_or_accounts_are_read()
     {
        let current_user = || {
            let mut sys = Fake::root();
            sys.euid = 501;
            sys.on("/usr/bin/id -un", Ran::ok("alice\n"));
            sys.on(
                "/usr/bin/dscl . -read /Groups/admin GroupMembership",
                Ran::ok("GroupMembership: alice\n"),
            );
            sys.on("/usr/bin/dscl . -read /Users//alice UniqueID PrimaryGroupID NFSHomeDirectory UserShell", Ran::ok("UniqueID: 501\nPrimaryGroupID: 20\nNFSHomeDirectory: /Users//alice\nUserShell: /bin/zsh\n"));
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
            sys.on("/usr/bin/uname -m", Ran::ok("arm64\n"));
            sys.on("/usr/bin/sw_vers -productVersion", Ran::ok("27.0\n"));
            sys
        };
        let command = crate::facts::hidutil_keyboards();
        for invalid in [
            "Services:\nUnknownHeader\nnonempty unparsed row\n",
            "Devices:\n",
        ] {
            let sys = current_user();
            sys.on(&command.display(), Ran::ok(invalid));
            assert!(local_context(&sys).is_err());
            assert!(
                !sys.calls()
                    .iter()
                    .any(|call| call.contains("defaults") || call.contains("accounts.data"))
            );
        }
        let sys = current_user();
        sys.on(&command.display(), Ran::ok("Services:\n\nDevices:\n"));
        assert!(local_context(&sys).unwrap().facts.keyboards.is_empty());
        let sys = current_user();
        sys.on(&command.display(), Ran::ok("Services:\n"));
        sys.refuse_complete_stdout(&command.display(), "incomplete captured service list");
        assert!(local_context(&sys).is_err());
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
        sys.on("/usr/bin/id -un", Ran::ok("alice\n"));
        sys.on(
            "/usr/bin/dscl . -read /Groups/admin GroupMembership",
            Ran::ok("GroupMembership: alice\n"),
        );
        sys.on("/usr/bin/dscl . -read /Users//alice UniqueID PrimaryGroupID NFSHomeDirectory UserShell", Ran::ok("UniqueID: 501\nPrimaryGroupID: 20\nNFSHomeDirectory: /Users//alice\nUserShell: /bin/zsh\n"));
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
        sys.on("/usr/bin/uname -m", Ran::ok("arm64\n"));
        sys.on("/usr/bin/sw_vers -productVersion", Ran::ok("27.0\n"));
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
            let groups = |_: &dyn System, _: &Ctx, _: Mode| {
                self.workstations.fetch_add(1, Ordering::SeqCst);
                Ok(vec![(
                    vec![Listed::boxed("init.test.dock", vec![change(), ok()])],
                    place(Class::Preference),
                )])
            };
            converge(
                sys,
                options,
                &Front {
                    base: &base,
                    groups: &groups,
                    targo: None,
                    cwd: Some(Path::new("/Users//alice/aterm")),
                },
                &mut NoJournal,
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
                let ctx = local_context(&sys).unwrap();
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
                    &crate::checkout::git(&alice, Some(Path::new("/Users//alice/aterm")))
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
    /// when given; the account's context cannot be built (no `id -un` answer is scripted, as
    /// for an account the local directory node does not hold).
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
    /// develop row and never builds the account's context (no `id -un`, no `dscl`). A profile
    /// that records a checkout, an `--only` develop word, or no home from the account record
    /// (the controls) build it, and its failure here is the one `develop.record` row.
    #[test]
    fn doctor_runs_nothing_for_the_develop_layer_of_a_mac_that_records_none() {
        let id = Cmd::new("/usr/bin/id").arg("-un").display();
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
}
