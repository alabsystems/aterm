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

/// The local setup's command-line help.
pub const USAGE: &str = "aterm init [--plan | --check] [--json] [--only <item,...>]\n\
    First restore what aterm lays for this account (the installed commands and\n\
    their ~/.local/bin links, the shell integration, the reroute stubs, the\n\
    rustup link, exec roots), as bare `aterm doctor` does, and install Apple's\n\
    Command Line Tools when they are missing (base.clt: one sudo password prompt\n\
    in this terminal); this part runs over SSH too. Then, at this Mac's console\n\
    only,\n\
    configure this macOS login: Caps Lock as Control, Natural scrolling off,\n\
    no widgets, only aterm, System Settings and Safari pinned in the Dock,\n\
    sign out of Messages and FaceTime, and keep rustup's offline docs (rust-docs,\n\
    tens of thousands of pages Spotlight indexes) off this login: the minimal\n\
    profile, and rust-docs removed from the toolchains rustup installed. Applies\n\
    and reads back each setting; safe to run again.\n\
    Joins the secrets vault (aterm help secrets): the ALab vault when it answers\n\
    on this network (asking only for its join password), else the one you name.\n\
    Optional ~/.config/aterm/init.toml [github] config authenticates gh and clones\n\
    all accessible owned repositories into your home; see aterm help init.\n\
    Run as the logged-in user, without sudo. Account sign-out may need\n\
    Accessibility and Automation permission for aterm in System Settings.\n\
\n\
    --plan    Report the changes without applying them.\n\
    --check   Read only; exit nonzero if anything still needs attention.\n\
    --json    Print the per-setting report as JSON.\n\
    --only    Run only the base items named (an id such as base.clt or\n\
              base.shell.rc, a family such as shell or pkg, or base), and no\n\
              setting of this login.\n\
    --help    Show this help without inspecting or changing the Mac.\n\
\n\
    Exit: 0 done; 75 only what an install pass's store lock held back is left;\n\
    77 only what waits for a permission you grant; 69 only what waits for the\n\
    network; 1 anything else; 2 usage.\n";

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
    /// `--only` words: the base items kept ([`aterm_converge::select`]); empty runs all.
    only: Vec<String>,
}

fn options(args: &[OsString]) -> Result<Options, String> {
    let mut options = Options::default();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let only = match arg.to_str() {
            Some("--only") => Some(
                args.next()
                    .and_then(|words| words.to_str())
                    .ok_or("--only takes a comma-separated list of base items")?,
            ),
            Some(arg) => arg.strip_prefix("--only="),
            None => None,
        };
        if let Some(words) = only {
            let words: Vec<String> = words
                .split(',')
                .map(str::trim)
                .filter(|word| !word.is_empty())
                .map(str::to_owned)
                .collect();
            if words.is_empty() {
                return Err("--only takes a comma-separated list of base items".into());
            }
            options.only.extend(words);
            continue;
        }
        match arg.to_str() {
            Some("--help" | "-h") => options.help = true,
            Some("--json") => options.json = true,
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
    Ok(options)
}

/// The base layer `aterm init` runs before its own items (design §4.2, S5): the
/// packages' repair items, composed by the front door (`crates/aterm`), which links the
/// crate that defines them, and `base.clt` over atpkg's C toolchain probe. Base items
/// require only the account, so they run over SSH too; the workstation items need this
/// Mac's console.
pub struct Base {
    /// The items, in run order.
    pub items: Vec<Box<dyn aterm_converge::Item>>,
    /// What keeps them safe to apply (the store lock), held while they run and released
    /// before `base.clt` and the workstation items.
    pub held: Option<Box<dyn std::any::Any>>,
    /// `base.clt` ([`crate::init_clt`]), run once the lock is released: its apply waits on
    /// a person's password.
    pub clt: Option<crate::init_clt::CommandLineTools>,
}

impl Base {
    /// No base items.
    #[must_use]
    pub fn none() -> Self {
        Self {
            items: Vec::new(),
            held: None,
            clt: None,
        }
    }

    /// The `--only` words that name none of these items (`aterm_converge::selects`: an
    /// id, a family, a layer), for the surface to refuse before anything is read.
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

    /// These items narrowed to `only` ([`aterm_converge::select`]); every item when it is
    /// empty.
    #[must_use]
    pub fn narrowed(self, only: &[String]) -> Self {
        if only.is_empty() {
            return self;
        }
        let Self { items, held, clt } = self;
        Self {
            items: aterm_converge::select(items, only).0,
            held,
            clt: clt.filter(|_| only.iter().any(|word| crate::init_clt::selected(word))),
        }
    }
}

/// Run `aterm init`. Parsing and help precede every live probe, including HOME checks.
/// `base` gives the base layer's items for a run that applies (`true`) or only reads.
pub fn main_entry(args: Vec<OsString>, base: &dyn Fn(bool) -> Base) -> ExitCode {
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
    // Every `--only` word names a base item, or nothing is read (the items are composed,
    // not probed).
    let unnamed = base(false).unnamed(&options.only);
    if !unnamed.is_empty() {
        eprintln!(
            "aterm init: --only {} names no base item (an id such as base.clt or base.shell.rc, \
             a family such as shell or pkg, or base)\n{USAGE}",
            unnamed.join(",")
        );
        return ExitCode::from(2);
    }
    if !crate::sessions::is_live_account() {
        eprintln!("aterm init: run in your own macOS login and home directory");
        return ExitCode::FAILURE;
    }
    let sys = Live::new();
    let result = sys.journal().and_then(|mut journal| {
        let mode = if options.action == Action::Apply {
            Mode::Apply
        } else {
            Mode::Plan
        };
        // `--only` runs base items alone: no setting of this login is read or asked about,
        // and no keyboard list either.
        let (ctx, groups) = if options.only.is_empty() {
            let ctx = local_context(&sys)?;
            let groups = prepare_groups(&sys, &ctx, mode)?;
            (ctx, groups)
        } else {
            (context_for(&sys, WorkstationGroup::Accounts)?, Vec::new())
        };
        run(
            &sys,
            &ctx,
            base(mode == Mode::Apply).narrowed(&options.only),
            &groups,
            mode,
            journal.as_mut(),
        )
    });
    let rows = match result {
        Ok(rows) => rows,
        Err(why) => {
            eprintln!("aterm init: {why}");
            return ExitCode::FAILURE;
        }
    };
    let complete = complete(&rows, options.action);
    let code = status(&rows, options.action);
    if options.json {
        // The one envelope every `--json` surface prints (`aterm pkg doctor --json` too).
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
            println!("{}: {}", word(row.outcome), row.title);
            if let (Some(from), Some(to)) = (&row.from, &row.to) {
                println!("    {from} → {to}");
            }
            if let Some(why) = &row.why {
                println!("    {why}");
            }
        }
        let summary = match (options.action, complete) {
            (Action::Plan, true) => "plan ready; no changes applied",
            (_, true) => "all requested settings verified",
            (_, false) => "settings still need attention; see the rows above",
        };
        println!("aterm init: {summary}");
    }
    ExitCode::from(code)
}

/// The exit status (design §3.3): 0 when complete; when the only incomplete rows stood
/// aside, 75 on a lock, 77 on a permission not granted, 69 offline (in that order when
/// mixed: what a lock held back was not attempted at all); else 1.
fn status(rows: &[ItemReport], action: Action) -> u8 {
    use aterm_converge::StoodAside;
    if complete(rows, action) {
        return 0;
    }
    let incomplete: Vec<&ItemReport> = rows.iter().filter(|row| !counts(row, action)).collect();
    if incomplete.is_empty() || incomplete.iter().any(|row| row.stood_aside.is_none()) {
        return 1;
    }
    let any = |kind: fn(&StoodAside) -> bool| {
        incomplete
            .iter()
            .any(|row| row.stood_aside.as_ref().is_some_and(kind))
    };
    if any(|aside| matches!(aside, StoodAside::Lock(_))) {
        75
    } else if any(|aside| matches!(aside, StoodAside::Permission(_))) {
        77
    } else {
        69
    }
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
}

impl WorkstationGroup {
    /// The selected rows belonging to this part of the pass.
    #[must_use]
    pub fn selected(self, mut selection: Selection) -> Selection {
        for setting in Setting::ALL {
            let belongs = match self {
                Self::All => setting.is_workstation(),
                Self::Accounts => matches!(setting, Setting::Messages | Setting::FaceTime),
                Self::Preferences => {
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
/// The caller decides plan/apply and, before applying, enforces its own isolated
/// instance and first-launch policy. Plan only probes; it never opens either app.
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

/// A workstation pass whose before-images and apply share one resolved account
/// and item set. This lets doctor save exact plist values before the first write.
pub struct Workstation {
    ctx: Ctx,
    items: Vec<Box<dyn Item>>,
    snapshots: Option<Vec<PreferenceSnapshot>>,
    /// The items this pass leaves alone ([`Workstation::hold`]), by id.
    held: Vec<(String, Hold)>,
}

type PreferenceSnapshot = Vec<(Target, Read<PValue>)>;

/// How a pass leaves an item alone ([`Workstation::hold`]). Either way an item that already
/// reads as wanted stays `Ok`, and nothing of a held item is applied, refreshed or journaled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hold {
    /// Its change is not made in this pass: one that would change reads `Blocked`, naming
    /// what it reads now and this line ([`aterm_converge::held_back`]).
    Back(String),
    /// It made its change once and was changed since: one that would change reads
    /// `NotApplicable`, naming what it reads now and this line, so nothing is wrong and
    /// nothing is to do ([`aterm_converge::left_as_is`]).
    AsIs(String),
}

/// The shared engine's rows and the items whose change it reached.
#[derive(Debug)]
pub struct WorkstationRun {
    /// The common probe/apply/read-back results.
    pub rows: Vec<ItemReport>,
    /// Each item whose apply or refresh ran, in run order. A refused plan, a held item
    /// and an apply that stood aside contribute nothing: none of them changed anything.
    pub attempts: Vec<Attempt>,
}

/// One item whose apply or refresh this pass ran.
#[derive(Debug, Clone)]
pub struct Attempt {
    /// The item's id, as its row has it.
    pub id: String,
    /// Whether its read-back verified the change: the row ended `Ok`, `Changed` or
    /// `NotApplicable`. An attempt that did not verify may still have changed the machine
    /// (a write that failed after it took, a consumer that rewrote what was written).
    pub verified: bool,
    /// The before-images the snapshot guard admitted for it immediately before its
    /// mutation, including a write that failed after partially taking effect. Empty
    /// for an item with no preference keys and for a pass that took no snapshot.
    pub preferences: Vec<(Target, Read<PValue>)>,
}

impl Workstation {
    /// Typed before-images of precisely the preference keys this pass would change.
    /// The pass checks these again immediately before each mutation; a changed
    /// target or value requires a fresh snapshot instead of an unbacked write.
    ///
    /// # Errors
    /// A preference cannot be read safely.
    pub fn preference_changes(&mut self, sys: &dyn System) -> Result<PreferenceSnapshot, String> {
        let mut changes = Vec::new();
        let mut snapshots = Vec::new();
        for item in &self.items {
            // A held item changes nothing in this pass ([`Workstation::hold`]): no key of
            // it is backed up for, or activated after, a change nobody makes.
            if self.held.iter().any(|(id, _)| *id == item.id()) {
                snapshots.push(Vec::new());
                continue;
            }
            let snapshot = item.preference_changes(sys, &self.ctx)?;
            for (target, read) in &snapshot {
                if changes.iter().any(|(saved, _)| saved == target) {
                    return Err(
                        "workstation items overlap a preference key; no changes applied".into(),
                    );
                }
                if matches!(read, Read::Unusable(_)) {
                    return Err(
                        "a workstation recovery value could not be read; no changes applied".into(),
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

    /// Run the shared engine and retain exactly which snapshotted items reached
    /// mutation, including a write that failed after partially taking effect.
    ///
    /// # Errors
    /// The journal cannot be resolved or the items' dependency order is invalid.
    pub fn run_recording(self, sys: &dyn System, mode: Mode) -> Result<WorkstationRun, String> {
        // The journal of the account `sys` acts as: the items' own port.
        let mut journal = sys.journal()?;
        self.run_recording_with(sys, mode, journal.as_mut())
    }

    /// Leave the items `held` names alone in this pass, each as its [`Hold`] says: nothing
    /// of one is applied, refreshed, backed up ([`Workstation::preference_changes`]) or
    /// journaled, and one that already reads as wanted stays `Ok`. Call it before
    /// [`Workstation::preference_changes`]. An unattended caller names the items whose
    /// own earlier attempt it will not repeat, so a retry on its timer is not the same
    /// change, relaunch and rollback again.
    pub fn hold(&mut self, held: &[(String, Hold)]) {
        self.held = held.to_vec();
    }

    /// A pass over `items` at a context of no account and no host facts: for a caller's
    /// tests of what it does with a pass's rows and attempts.
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
                // An apply that stood aside changed nothing (`Item::stood_aside`).
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
        Ok(WorkstationRun { rows, attempts })
    }
}

/// The shared item still owns its policy and mutation. This wrapper checks that the
/// durable recovery record describes the values it is about to replace, and records which
/// items reached mutation.
struct Guard {
    inner: Box<dyn Item>,
    /// The before-images saved for recovery; `None` when the pass took no snapshot.
    before: Option<PreferenceSnapshot>,
    attempted: Arc<Mutex<Vec<(String, PreferenceSnapshot)>>>,
}

impl Guard {
    fn validate(&self, sys: &dyn System, ctx: &Ctx) -> Result<(), String> {
        let Some(before) = &self.before else {
            return Ok(());
        };
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
            Err("workstation preferences changed after the recovery snapshot; run doctor --apply again".into())
        }
    }

    fn record_attempt(&self) {
        let id = self.inner.id();
        let mut attempted = self
            .attempted
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if !attempted.iter().any(|(saved, _)| *saved == id) {
            attempted.push((id, self.before.clone().unwrap_or_default()));
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

/// Prepare only the chosen rows. An empty group reads no host facts.
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
            snapshots: None,
            held: Vec::new(),
        });
    }
    let ctx = context_for_selected(sys, group, selected)?;
    // Doctor and first launch run these items unscoped, so they need the console here;
    // `aterm init` scopes them instead (`Requires::Console`).
    if ctx.console.is_none() {
        return Err("run as the user currently logged in at this Mac's console".into());
    }
    let items = workstation_items_selected(&ctx, group, selected)?;
    Ok(Workstation {
        ctx,
        items,
        snapshots: None,
        held: Vec::new(),
    })
}

/// Resolve this login's console context for independent initial-setup stages,
/// without inventorying keyboards or reading any preference/account service.
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
        snapshots: None,
        held: Vec::new(),
    }
    .run(sys, mode)
}

fn workstation_items_selected(
    ctx: &Ctx,
    group: WorkstationGroup,
    selected: Selection,
) -> Result<Vec<Box<dyn Item>>, String> {
    let mut items = Vec::new();
    match group {
        WorkstationGroup::All | WorkstationGroup::Preferences => {
            items.extend(crate::init_preferences::selected_items(ctx, selected)?);
            if selected.contains(Setting::NaturalScrolling) {
                items.extend(crate::init_trackpad::items(ctx)?);
            }
            if selected.contains(Setting::TrackpadScrolling) {
                items.extend(crate::init_trackpad::behavior_items(ctx)?);
            }
            if group == WorkstationGroup::All {
                items.extend(crate::init_accounts::selected_items(ctx, selected)?);
            }
        }
        WorkstationGroup::Accounts => {
            items.extend(crate::init_accounts::selected_items(ctx, selected)?)
        }
        WorkstationGroup::InputActivation => {
            if selected.contains(Setting::CapsLock) {
                items.extend(crate::init_preferences::attached_keyboard_items(ctx)?);
            }
            if selected.contains(Setting::NaturalScrolling) {
                items.extend(crate::init_trackpad::items(ctx)?);
            }
        }
    }
    Ok(items)
}

/// Items that share one place in the desired state.
type Group = (Vec<Box<dyn Item>>, Place);

/// Preparation can read optional profiles and ask vault questions before an item is
/// probed. Away from the console, defer the whole workstation preparation as well as
/// its applies: a broken local profile must not prevent the base repairs over SSH.
fn prepare_groups(sys: &dyn System, ctx: &Ctx, mode: Mode) -> Result<Vec<Group>, String> {
    if ctx.console.is_none() {
        return Ok(vec![(
            vec![Box::new(WorkstationAtConsole)],
            workstation_place(Class::Person),
        )]);
    }
    // Questions precede every change, including the base layer's.
    let secrets = crate::init_secrets::items(sys, ctx, crate::init_secrets::live(ctx)?, mode)?;
    groups(sys, ctx, secrets)
}

/// The optional workstation items cannot be enumerated without reading their inputs.
/// Keep one honest blocked row until this login owns the console.
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

/// What init converges after the base layer, in run order, each list with its place: all
/// of it the workstation layer, which needs this Mac's console (design §3.2, §4.3): over
/// SSH, or under another login's console, each is `Blocked` and nothing of it is read. The
/// secrets vault's join (`secrets`, from `init_secrets::items`), the GitHub sign-in (whose
/// keychain write needs the console session's unlocked keychain) and checkouts reach the
/// network.
fn groups(sys: &dyn System, ctx: &Ctx, secrets: Vec<Box<dyn Item>>) -> Result<Vec<Group>, String> {
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
        // rustup's profile and the rust-docs its toolchains hold: an act on the
        // person's own toolchains that only a run they started makes (§3.2 `asked`).
        (crate::init_spotlight::items(ctx)?, place(Class::Asked)),
        (crate::init_accounts::items(ctx)?, place(Class::Preference)),
        // Before GitHub: the vault carries the token files its profile may name.
        (secrets, place(Class::Install)),
        (crate::init_github::items(sys, ctx)?, place(Class::Install)),
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

/// Run the `base` items, then `groups` against `sys`, each change journaled in `journal`
/// before it is made. Every class applies; an item that needs the console runs only when
/// this login holds it (`ctx.console`). What the base layer holds (the store lock) is
/// released once its items have run: a workstation item can wait on a person.
fn run<'a>(
    sys: &'a dyn System,
    ctx: &'a Ctx,
    base: Base,
    groups: &'a [Group],
    mode: Mode,
    journal: &mut dyn Journal,
) -> Result<Vec<ItemReport>, String> {
    // The front gate has refused root and any other `$HOME` (`main_entry`, `context_for`):
    // this is the account's own shell.
    let scope = aterm_converge::Scope {
        at_console: ctx.console.is_some(),
        ..aterm_converge::Scope::EVERY
    };
    let Base { items, held, clt } = base;
    let mut rows = aterm_converge::run_scoped(&items, mode, scope, journal, &mut |_| {})
        .map_err(|refused| refused.to_string())?;
    drop(held);
    // `base.clt` once the lock is gone: its apply waits on a person's password.
    if let Some(clt) = &clt {
        let bound = crate::engine::bind([(clt as &dyn Item, crate::init_clt::place())], sys, ctx);
        rows.extend(
            aterm_converge::run_scoped(&bound, mode, scope, journal, &mut |_| {})
                .map_err(|refused| refused.to_string())?,
        );
    }
    // No workstation item waits on a base item (they name none), so the two passes order
    // as the one did.
    let bound = crate::engine::bind(
        groups.iter().flat_map(|(items, place)| {
            items.iter().map(move |item| (item.as_ref(), place.clone()))
        }),
        sys,
        ctx,
    );
    rows.extend(
        aterm_converge::run_scoped(&bound, mode, scope, journal, &mut |_| {})
            .map_err(|refused| refused.to_string())?,
    );
    Ok(rows)
}

/// Collect only what this setup uses, avoiding fleet, TCC database and unrelated app probes.
/// Root is refused; another login holding the console (or none, over SSH) is not: the
/// context then has no console user, so the workstation items are `Blocked` and the
/// console session's keyboards are not read (design §3.2: `Requires::Console`).
fn local_context(sys: &dyn System) -> Result<Ctx, String> {
    context_for(sys, WorkstationGroup::All)
}

fn context_for(sys: &dyn System, group: WorkstationGroup) -> Result<Ctx, String> {
    context_for_selected(sys, group, Selection::all())
}

fn context_for_selected(
    sys: &dyn System,
    _group: WorkstationGroup,
    _selected: Selection,
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
    // The console's owner: this login's, or another's (then nothing of the console
    // session is read), or nobody's to read.
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
    // Live keyboard inventory is deliberately not read here. Searching HID
    // devices can stall behind the device/Bluetooth stack or its system UI,
    // and startup must never wait on it. Caps Lock preferences for listed
    // models still apply; live attached-keyboard activation is not part of
    // this pass.
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

/// Whether `row` counts as done for `action`.
fn counts(row: &ItemReport, action: Action) -> bool {
    matches!(
        row.outcome,
        Outcome::Ok | Outcome::Changed | Outcome::NotApplicable
    ) || (action == Action::Plan && row.outcome == Outcome::Planned)
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

    #[test]
    fn selected_widgets_do_not_read_keyboard_dock_trackpad_or_accounts() {
        let who = user("alice", 501);
        let context = ctx(&[who]);
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
        let items = workstation_items_selected(&context, WorkstationGroup::All, selected).unwrap();
        assert_eq!(items.len(), 1);
        let snapshots = items[0].preference_changes(&sys, &context).unwrap();
        assert_eq!(snapshots.len(), 3);
        assert!(matches!(
            items[0].probe(&sys, &context),
            Probe::Change { .. } | Probe::Deferred { .. }
        ));
        for call in sys.calls() {
            assert!(
                call.contains("com.apple.WindowManager")
                    || call.contains("com.apple.notificationcenterui")
                    || call.contains("/init-activation/widgets.pending"),
                "unchecked setting queried: {call}"
            );
        }
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

    /// The trackpad item's preference read, answering `com.apple.swipescrolldirection`
    /// saved as off.
    fn saved_off(sys: &Fake) {
        sys.on(
            "alice$ /usr/bin/defaults export -g -",
            export("<dict><key>com.apple.swipescrolldirection</key><false/></dict>"),
        );
    }

    /// A context whose one user holds this Mac's console.
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

    /// The workstation pass (`aterm doctor --apply`, a first launch's setup) journals each
    /// change in the journal of the system it runs on: the fake's files hold the changed
    /// item's line, and the item already as wanted writes none. Negative control: the
    /// journal this process would resolve for itself is never asked, so a mutant that
    /// asks it leaves the fake's journal empty.
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

    /// An item whose probes answer from a list (the last one repeats), counting its applies
    /// and its runtime refreshes; with `aside`, each apply stands aside on it.
    struct Counted {
        id: &'static str,
        probes: Mutex<Vec<Probe>>,
        applies: Arc<std::sync::atomic::AtomicU32>,
        refreshes: Arc<std::sync::atomic::AtomicU32>,
        aside: Option<aterm_converge::StoodAside>,
        /// The preference keys it would change ([`Item::preference_changes`]).
        keys: Vec<(Target, Read<PValue>)>,
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
                keys: Vec::new(),
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
            Ok(self.keys.clone())
        }
    }

    /// A held item's keys are no before-images of the pass: nothing of it changes, so
    /// nothing of it is backed up for, or relaunched after, the pass, and a pass that
    /// dies part way leaves no record naming a key nobody wrote. Negative control:
    /// dropping the held skip of `Workstation::preference_changes` lists the Dock key.
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
                keys: vec![(Target::user(domain, "k", &alice), Read::Absent)],
            }) as Box<dyn Item>
        };
        let mut pass = Workstation {
            items: vec![
                item("init.test.dock", "com.apple.dock"),
                item("init.test.widgets", "com.apple.WindowManager"),
            ],
            ctx: ctx(std::slice::from_ref(&alice)),
            snapshots: None,
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

    /// A held item that would change probes `Blocked` (held back) or `NotApplicable` (left
    /// as it is), naming what it reads now and the caller's line, and is neither applied,
    /// refreshed nor journaled, and one that already reads correct stays `Ok`, while the
    /// items not held run as before; only the items whose apply or refresh ran are
    /// attempts, each saying whether it verified. Negative control: not wrapping a held
    /// item in `run_recording_with` applies and journals it.
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

    /// An apply that stood aside (a permission not granted yet: the account sign-out
    /// without Accessibility) changed nothing, so it is no attempt: an unattended caller
    /// that holds what did not verify must not hold it, and retries it once the grant
    /// lands. Negative control: dropping the `stood_aside` filter of `run_recording_with`
    /// reports it an unverified attempt.
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
        // Natural scrolling is saved off but still on in the session: init's trackpad item
        // changes it, and its read-back reads off.
        let sys = Fake::root();
        sys.scroll_state(true);
        saved_off(&sys);
        let groups = vec![
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
        for (line, row) in lines
            .iter()
            .zip(rows.iter().filter(|r| r.outcome == Outcome::Changed))
        {
            assert_eq!(line["from"].as_str(), row.from.as_deref());
            assert_eq!(line["to"].as_str(), row.to.as_deref());
            assert_eq!(line["revert"].as_str(), row.revert.as_deref());
        }
        assert!(rows.iter().all(|r| r.layer == Some(Layer::Workstation)));
    }

    /// WHAT THE BASE LAYER HOLDS (the store lock) IS RELEASED once its items have run,
    /// before the first workstation item is read: a workstation item can wait on a person,
    /// or run `aterm pkg install` itself, and an install pass must not wait on it. The
    /// base item still runs under it, and the rows keep the base layer first.
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
        /// A workstation item that notes, at each probe, whether the hold was released.
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
        // The negative control: the same run with nothing to change, the trackpad's
        // native refresh included, writes no line and makes no journal.
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
        let groups = groups(&sys, &alice, Vec::new()).unwrap();
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
        // rustup's profile and rust-docs: acts on the person's toolchains, asked by the run.
        let asked: Vec<String> = bound
            .iter()
            .filter(|b| b.class() == Class::Asked)
            .map(|b| b.id())
            .collect();
        assert_eq!(
            asked,
            [
                "init.spotlight.rustup-profile@alice",
                "init.spotlight.rust-docs@alice"
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
            }
        );
        // `--only` takes a list, in either spelling, and refuses an empty one.
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

    /// `--only` keeps the base items its words name, `base.clt` among them, and a word that
    /// names none is the surface's to refuse (`unnamed`) before anything runs.
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

    /// `base.clt` runs in init's base pass after what the base layer holds is released (its
    /// apply waits on a password), as an elevated base item; with no terminal it is
    /// `Blocked` on `aterm init --only base.clt` and nothing runs.
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

    /// THE CONSOLE GATE IS PER ITEM (S5): with another uid at this Mac's console (or none,
    /// over SSH), init is not refused — a base item applies and is read back, and each
    /// workstation item is `Blocked` with nothing of the console session read (no
    /// `defaults`, no `hidutil`, no accounts). The same items with this login at the
    /// console apply both: the negative control.
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
        // Doctor's and first launch's workstation pass runs its items unscoped: away from
        // the console it is refused, never run on a context without the console user.
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
            };
            let groups = if at_console {
                vec![(
                    vec![Listed::boxed("init.test.dock", vec![change(), ok()])],
                    place(Class::Preference),
                )]
            } else {
                let before = sys.calls();
                let groups = prepare_groups(&sys, &ctx, Mode::Apply).unwrap();
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
        // Root is still refused at the front gate.
        assert!(
            local_context(&Fake::root())
                .unwrap_err()
                .contains("without sudo")
        );
    }

    /// 75 when the only incomplete rows stood aside on a lock, 69 offline, 75 when both,
    /// 1 when anything else is left, 0 when complete.
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
        let done = row(Outcome::Changed, None);
        assert_eq!(status(std::slice::from_ref(&done), Action::Apply), 0);
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
    fn local_context_never_inventories_live_keyboards() {
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
        // No hidutil expectation is installed: startup must not search live
        // devices or wait on the HID/Bluetooth stack to answer.
        let ctx = local_context(&sys).unwrap();
        assert!(ctx.facts.keyboards.is_empty());
        assert!(
            !sys.calls().iter().any(|call| call.contains("hidutil")),
            "calls: {:?}",
            sys.calls()
        );
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
}
