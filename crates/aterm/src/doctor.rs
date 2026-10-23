// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `aterm doctor` entry point, convergence runner, and macOS base init items.

use std::ffi::OsString;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

use aterm_cli::Mark;
use aterm_cli::doctor::{self as cli, Args, Gathered, Section};
use aterm_converge::{
    Effect, Item, ItemReport, Journal, Layer, Mode, Outcome, Readiness, Remedy, Report, ReportMode,
    Scope, StoodAside,
};
use atpkg::converge::repair::{self, Ground, Pass};

/// What bare doctor says when an install pass holds the store (design §3.2).
pub(crate) const CONTENDED: &str = "an install pass holds the store; nothing was repaired";

/// `aterm doctor [--read-only | --check | --apply | --restore] [--json] [--only <id,...>]`
/// over this machine.
pub(crate) fn main_entry(rest: &[OsString]) -> ExitCode {
    let rest: Vec<String> = rest
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let args = match cli::args(&rest) {
        Ok(args) => args,
        Err(usage) => {
            eprintln!("{usage}");
            return ExitCode::from(2);
        }
    };
    if let Some(action) = args.settings {
        let (text, code) = cli::settings(action, args.json);
        return cli::answer(&text, code, true);
    }
    let ground = atpkg::store::resolve_configured()
        .map(|layout| Arc::new(Ground::of_process(layout, Pass::Doctor)));
    let packages = || {
        ground
            .as_ref()
            .map(|ground| atpkg::doctor::packages_rows(&ground.layout))
            .unwrap_or_default()
    };
    let state = aterm_types::dirs::account_state_dir();
    let ran = run(
        &args,
        &Ports {
            gather: &cli::gather,
            ground: ground.as_ref(),
            packages: &packages,
            state: state.as_deref(),
            not_own_account: aterm_types::dirs::not_own_account(),
        },
    );
    cli::answer(&ran.text, ran.code, ran.json)
}

/// What a doctor run reads and writes, injected so a test runs it over a scratch home
/// and prefix.
pub(crate) struct Ports<'a> {
    /// Reads aterm-cli's facts, `true` handing the Developer Tools over first
    /// ([`cli::gather`]); called once, after every `--only` word is known to name
    /// something.
    pub(crate) gather: &'a dyn Fn(bool) -> Gathered,
    /// The repair items' ground; `None` when no store can be found ($HOME unset).
    pub(crate) ground: Option<&'a Arc<Ground>>,
    /// `aterm pkg doctor`'s rows, read after the repair.
    pub(crate) packages: &'a dyn Fn() -> Vec<ItemReport>,
    /// The account's state root, whose journal records each change; `None` refuses
    /// every change (nothing is changed unrecorded).
    pub(crate) state: Option<&'a Path>,
    /// Why this is not a shell of the account's own (root, another `$HOME`, a private
    /// state root: [`aterm_types::dirs::not_own_account`]): every item is read and none
    /// changed, the store is not locked and nothing is journaled (design §3.2).
    pub(crate) not_own_account: Option<&'static str>,
}

/// A run's answer: the text (the report, the JSON, or the refusal), its exit status, and
/// whether it is the JSON (stdout whatever the code).
#[derive(Debug)]
pub(crate) struct Ran {
    pub(crate) text: String,
    pub(crate) code: u8,
    pub(crate) json: bool,
}

/// How the repair items ran.
#[derive(Debug, Clone, PartialEq, Eq)]
enum How {
    /// Applied under the store lock.
    Applied,
    /// `--read-only`, or a shell not the account's own: probed and planned, unlocked.
    Planned,
    /// An install pass held the lock: each stood aside, and this line says so (a note).
    StoodAside(String),
    /// The lock could not be taken for another reason: each change is held back, and
    /// this line says why (a warning: the store is not in order).
    HeldBack(String),
}

/// Run `aterm doctor` as `args` asks over `ports`.
pub(crate) fn run(args: &Args, ports: &Ports<'_>) -> Ran {
    let tokens = &args.only;
    let (repairs, missed) = match ports.ground {
        Some(ground) => aterm_converge::select(repair::items(ground), tokens),
        None => (Vec::new(), tokens.clone()),
    };
    let mut unmatched: Vec<String> = missed
        .into_iter()
        .filter(|token| !cli::names(token))
        .collect();
    let mut read_early = None;
    if !unmatched.is_empty() {
        let rows = (ports.packages)();
        unmatched.retain(|token| !rows.iter().any(|row| named_by(token, row)));
        read_early = Some(rows);
    }
    if !unmatched.is_empty() {
        return Ran {
            text: format!(
                "aterm doctor: --only {}: no item, family or layer has that name — usage: \
                 aterm doctor {}\n",
                unmatched.join(","),
                cli::GRAMMAR
            ),
            code: 2,
            json: false,
        };
    }

    let own = ports.not_own_account.is_none();
    let gathered = (ports.gather)(own && !args.read_only && cli::hands_over(tokens));
    let fact_tokens: Vec<String> = if tokens.is_empty() {
        Vec::new()
    } else {
        tokens
            .iter()
            .cloned()
            .chain([cli::SHELL.to_owned()])
            .collect()
    };
    let (facts, _) = aterm_converge::select(gathered.items(), &fact_tokens);

    let mut lock = None;
    let how = match ports.ground {
        _ if args.read_only || !own => How::Planned,
        Some(ground) if !repairs.is_empty() => match atpkg::lock::try_lock_store(&ground.layout) {
            Ok(held) => {
                lock = Some(held);
                How::Applied
            }
            Err(atpkg::lock::StoreLockError::Contended(_)) => How::StoodAside(CONTENDED.to_owned()),
            Err(why) => How::HeldBack(format!(
                "the store lock could not be taken ({why}); nothing was repaired"
            )),
        },
        _ => How::Applied,
    };
    let (facts, repairs) = match &how {
        How::StoodAside(line) => {
            let aside = StoodAside::Lock(line.clone());
            (
                aterm_converge::standing_aside(facts, &aside),
                aterm_converge::standing_aside(repairs, &aside),
            )
        }
        How::HeldBack(line) => (
            aterm_converge::held_back(facts, line),
            aterm_converge::held_back(repairs, line),
        ),
        How::Applied | How::Planned => (facts, repairs),
    };
    let mode = if how == How::Planned {
        Mode::Plan
    } else {
        Mode::Apply
    };
    let mut journal: Box<dyn Journal> = match ports.state {
        Some(state) if how == How::Applied => Box::new(aterm_converge::FileJournal::under(state)),
        _ => Box::new(aterm_converge::ReadOnly),
    };
    let scope = Scope {
        not_own_account: ports.not_own_account,
        ..Scope::REPAIR
    };
    let mut refused = Vec::new();
    let mut ran = |items: &[Box<dyn Item + '_>], id| {
        aterm_converge::run_scoped(items, mode, scope, journal.as_mut(), &mut |_| {})
            .unwrap_or_else(|refusal| {
                refused.push(unmeasured(id, "doctor", None, refusal.to_string()));
                Vec::new()
            })
    };
    let fact_rows = ran(&facts, "doctor.facts");
    let repair_rows = ran(&repairs, "doctor.repairs");
    if !fact_rows.iter().any(|row| row.id == cli::SHELL) {
        refused.push(unmeasured(
            cli::SHELL,
            "shell",
            Some(Layer::Base),
            "shell usability was not measured".to_owned(),
        ));
    }
    drop(lock);

    let changed = fact_rows
        .iter()
        .chain(&repair_rows)
        .any(|row| row.outcome == Outcome::Changed);
    let mut packages = match read_early {
        Some(rows) if !changed => rows,
        _ => (ports.packages)(),
    };
    let ran_ids: Vec<&str> = fact_rows
        .iter()
        .chain(&repair_rows)
        .map(|row| row.id.as_str())
        .collect();
    packages.retain(|row| {
        !ran_ids.contains(&row.id.as_str())
            && (tokens.is_empty() || tokens.iter().any(|token| named_by(token, row)))
    });
    for row in &mut packages {
        row.essential = false;
    }

    let missing_store = unmeasured(
        "base.pkg.store",
        "pkg",
        Some(Layer::Base),
        "$HOME is unset, so the package store cannot be found".to_owned(),
    );
    if ports.ground.is_none()
        && (tokens.is_empty() || tokens.iter().any(|token| named_by(token, &missing_store)))
    {
        refused.push(missing_store);
    }
    for token in tokens {
        if !fact_rows
            .iter()
            .chain(&repair_rows)
            .chain(&packages)
            .chain(&refused)
            .any(|row| named_by(token, row))
        {
            refused.push(unmeasured(
                &format!("doctor.selection.{token}"),
                "doctor",
                None,
                format!("requested selector {token} produced no check result on this host"),
            ));
        }
    }
    let rows: Vec<ItemReport> = fact_rows
        .iter()
        .chain(&repair_rows)
        .chain(&packages)
        .chain(&refused)
        .cloned()
        .collect();
    let readiness = Readiness::of(&rows, tokens);
    let code = u8::from(!readiness.all_green);
    if args.json {
        let mode = if mode == Mode::Plan {
            ReportMode::Check
        } else {
            ReportMode::Apply
        };
        let mut report = Report::new(mode, readiness.all_green, rows);
        report.readiness = Some(readiness);
        return match aterm_json::to_string(&report) {
            Ok(text) => Ran {
                text: format!("{text}\n"),
                code,
                json: true,
            },
            Err(why) => Ran {
                text: format!("aterm doctor: cannot encode the report: {why}\n"),
                code: 70,
                json: false,
            },
        };
    }
    let mut sections: Vec<Section> = refused
        .into_iter()
        .map(|row| Section {
            label: if row.family == "pkg" {
                "packages:"
            } else {
                "repair:"
            },
            mark: Mark::Warn,
            fact: row.why.unwrap_or_default(),
        })
        .collect();
    let hold = ports.not_own_account.map(aterm_converge::not_own_account);
    sections.extend(repair_section(&repair_rows, &how, hold.as_deref()));
    if !packages.is_empty() {
        let (healthy, lines) = atpkg::doctor::packages_view(&packages);
        sections.push(Section {
            label: "packages:",
            mark: if healthy { Mark::Ok } else { Mark::Warn },
            fact: lines.join("\n"),
        });
    }
    let text = cli::render_readiness(&gathered, &fact_rows, &sections, &readiness);
    Ran {
        text,
        code,
        json: false,
    }
}

fn unmeasured(id: &str, family: &str, layer: Option<Layer>, why: String) -> ItemReport {
    ItemReport {
        id: id.to_owned(),
        family: family.to_owned(),
        title: "checks could not be completed".to_owned(),
        outcome: Outcome::Unknown,
        from: None,
        to: None,
        why: Some(why),
        revert: None,
        effect: Effect::Now,
        detail: None,
        layer,
        class: None,
        explain: None,
        stood_aside: None,
        essential: false,
    }
}

/// The base items `aterm init` runs before its workstation items (design §4.2, S5): the
/// same repair items, the doctor's pass (an opted-out rc stays as the person left it),
/// `base.ai.accounts`, and `base.clt` over atpkg's C toolchain probe (S6).
#[cfg(target_os = "macos")]
pub(crate) fn init_base(applies: bool) -> aterm_machines::init::Base {
    let mut base = match atpkg::store::resolve_configured() {
        Some(layout) => {
            init_base_over(&Arc::new(Ground::of_process(layout, Pass::Doctor)), applies)
        }
        None => aterm_machines::init::Base::none(),
    };
    base.items.push(Box::new(AiAccountsItem));
    aterm_machines::init::Base {
        clt: command_line_tools(),
        targo: Some(Arc::new(targo)),
        ..base
    }
}

/// The toolset's `targo` for `develop.workspace-tools@<path>`: the `bin/targo` shim of the
/// atpkg store (the shim, so its own exports apply), once it resolves into the store.
#[cfg(target_os = "macos")]
fn targo() -> Result<std::path::PathBuf, String> {
    const INSTALL: &str = "the toolset's targo is not installed: `aterm pkg install \
                           --default-set` installs it";
    let layout = atpkg::store::resolve_configured().ok_or(INSTALL)?;
    atpkg::ops::which(&layout, "targo").ok_or(INSTALL)?;
    Ok(layout.bin_dir().join("targo"))
}

/// `base.ai.accounts`: converges `accounts.toml` and `fleet-accounts.toml`
/// during `aterm init` when a fleet accounts source is configured or discoverable.
#[cfg(target_os = "macos")]
struct AiAccountsItem;

#[cfg(target_os = "macos")]
impl Item for AiAccountsItem {
    fn id(&self) -> String {
        "base.ai.accounts".to_owned()
    }

    fn family(&self) -> &'static str {
        "accounts"
    }

    fn layer(&self) -> Layer {
        Layer::Base
    }

    fn class(&self) -> aterm_converge::Class {
        aterm_converge::Class::Preference
    }

    fn title(&self) -> String {
        "AI CLI account rotation".to_owned()
    }

    fn probe(&self) -> aterm_converge::Probe {
        use aterm_agent::harness::{
            accounts,
            accounts_sync::{FLEET_ROSTER_FILE, configured_source, has_init_accounts_source},
            cli::Env,
        };
        use aterm_converge::Probe;
        let Ok(env) = Env::from_process() else {
            return Probe::NotApplicable {
                why: "no user home directory is configured".to_owned(),
            };
        };
        let Some(home) = env.home.as_deref() else {
            return Probe::NotApplicable {
                why: "no user home directory is configured".to_owned(),
            };
        };
        let Some(roster_path) = env.accounts_roster() else {
            return Probe::NotApplicable {
                why: "no config directory is configured".to_owned(),
            };
        };
        let store = env.accounts_store();
        let fleet_path = roster_path
            .parent()
            .map(|p| p.join(FLEET_ROSTER_FILE))
            .unwrap_or_default();
        let config_path = roster_path
            .parent()
            .map(|p| p.join("aterm.toml"))
            .unwrap_or_else(|| std::path::PathBuf::from("aterm.toml"));
        if let Ok(raw) = accounts::Roster::load(&roster_path)
            && !raw.rows.is_empty()
            && fleet_path.is_file()
            && configured_source(Some(&config_path), Some(&roster_path)).is_some()
        {
            let bound = store.bound(&raw, env.now).unwrap_or(raw);
            let enrolled = accounts::enrolled(&bound);
            let ready = bound
                .rows
                .iter()
                .filter(|r| r.dir.is_none() || enrolled.contains(&r.label))
                .count();
            let state = store.load(env.now).map(|(s, _)| s).unwrap_or_default();
            let own_label = bound.row_for_login(None).map(|r| r.label.as_str());
            let next_hint = accounts::renew_next(&bound, &state, &enrolled, own_label, env.now)
                .filter(|_| ready < bound.rows.len())
                .map(|label| format!("; next: `aterm harness accounts renew {label}`"))
                .unwrap_or_default();
            return Probe::Ok {
                now: format!(
                    "{} accounts synced ({ready} enrolled{next_hint})",
                    bound.rows.len()
                ),
            };
        }
        if !has_init_accounts_source(home, &env.state, &roster_path, env.now) {
            return Probe::NotApplicable {
                why: "no fleet accounts source is configured".to_owned(),
            };
        }
        Probe::Change {
            from: "absent".to_owned(),
            to: "accounts.toml + fleet-accounts.toml synced".to_owned(),
        }
    }

    fn apply(&self) -> Result<(), String> {
        use aterm_agent::harness::{accounts_sync::sync_accounts_for_init, cli::Env};
        let env = Env::from_process()?;
        let home = env
            .home
            .as_deref()
            .ok_or_else(|| "no user home directory".to_owned())?;
        let roster_path = env
            .accounts_roster()
            .ok_or_else(|| "no config directory".to_owned())?;
        sync_accounts_for_init(home, &env.state, &roster_path, env.now).map(|_| ())
    }

    fn explain(&self, probe: &aterm_converge::Probe) -> aterm_converge::Explain {
        use aterm_converge::{Explain, Probe, Remedy};
        let why = match probe {
            Probe::Ok { now } => now.clone(),
            _ => "AI CLI account rotation roster (`accounts.toml`) is not synced yet".to_owned(),
        };
        Explain {
            why,
            remedy: Remedy::Init {
                only: "base.ai.accounts".to_owned(),
            },
        }
    }
}

/// `base.clt` ([`aterm_machines::init_clt`]): its probe is atpkg's C toolchain probe over
/// this process's `PATH`, which raises no Apple dialog, and its root half runs from this
/// executable. `None` when this executable cannot name itself.
#[cfg(target_os = "macos")]
fn command_line_tools() -> Option<aterm_machines::init_clt::CommandLineTools> {
    let aterm = std::env::current_exe()
        .and_then(|exe| exe.canonicalize())
        .ok()?;
    Some(aterm_machines::init_clt::CommandLineTools::new(
        Box::new(|_: &dyn aterm_machines::port::System| {
            clt_found(&atpkg::prereq::probe(std::env::var_os("PATH").as_deref()))
        }),
        aterm,
    ))
}

/// What `base.clt` reads in a C toolchain verdict ([`atpkg::prereq::CcVerdict`]): a
/// missing developer directory or `cc` is what the install supplies; a compiler that
/// refuses is installed already (an unaccepted Xcode license, a PATH-first `cc` without an
/// SDK); a compiler that did not answer is unknown.
#[cfg(target_os = "macos")]
fn clt_found(verdict: &atpkg::prereq::CcVerdict) -> aterm_machines::init_clt::Found {
    use aterm_machines::init_clt::Found;
    use atpkg::prereq::CcVerdict;
    match verdict {
        CcVerdict::Ready { .. } => Found::Present(verdict.line()),
        CcVerdict::NoDeveloperDir => Found::Missing(
            "not installed: no developer directory holds a clang (`/usr/bin/cc` is macOS's \
             install shim)"
                .to_owned(),
        ),
        CcVerdict::NoDriver => Found::Missing("no C compiler (`cc`) on PATH".to_owned()),
        CcVerdict::Broken { .. } => Found::Refuses(verdict.line()),
        CcVerdict::Unanswered { .. } | CcVerdict::Unproven { .. } => Found::Unknown(verdict.line()),
        CcVerdict::NotProbed => Found::NotHere(verdict.line()),
    }
}

/// [`init_base`] over `ground`. A run that applies tries the store lock, which init holds
/// over these items alone (never while a workstation item waits on a person). An install
/// pass holding it makes each item stand aside, and init exits 75 when they are all that
/// is left; a lock that cannot be taken otherwise holds each change back, a failure
/// (exit 1), since no retry takes it.
#[cfg(target_os = "macos")]
fn init_base_over(ground: &Arc<Ground>, applies: bool) -> aterm_machines::init::Base {
    use aterm_machines::init::Base;
    let items = repair::items(ground);
    if !applies {
        return Base {
            items,
            held: None,
            clt: None,
            targo: None,
        };
    }
    match atpkg::lock::try_lock_store(&ground.layout) {
        Ok(lock) => Base {
            items,
            held: Some(Box::new(lock)),
            clt: None,
            targo: None,
        },
        Err(why @ atpkg::lock::StoreLockError::Contended(_)) => Base {
            items: aterm_converge::standing_aside(items, &StoodAside::Lock(why.to_string())),
            held: None,
            clt: None,
            targo: None,
        },
        Err(why) => Base {
            items: aterm_converge::held_back(
                items,
                &format!("the store lock could not be taken ({why})"),
            ),
            held: None,
            clt: None,
            targo: None,
        },
    }
}

fn named_by(token: &str, row: &ItemReport) -> bool {
    aterm_converge::selects(token, &row.id, &row.family, row.layer)
}

fn repair_section(rows: &[ItemReport], how: &How, hold: Option<&str>) -> Option<Section> {
    if rows.is_empty() {
        return None;
    }
    let remedy = |row: &ItemReport| match row.explain.as_ref().map(|e| &e.remedy) {
        Some(Remedy::Command(command)) => format!(" — `{command}`"),
        Some(Remedy::Repair) => " — `aterm pkg repair`".to_owned(),
        Some(Remedy::Init { only }) => format!(" — `aterm init --only {only}`"),
        _ => String::new(),
    };
    let (lead, lead_warns) = match how {
        How::StoodAside(line) => (Some(line.as_str()), false),
        How::HeldBack(line) => (Some(line.as_str()), true),
        How::Applied | How::Planned => (None, false),
    };
    let held_back = match how {
        How::HeldBack(line) => format!("; not changed: {line}"),
        _ => String::new(),
    };
    let mut lines: Vec<String> = Vec::new();
    let mut warn = lead_warns;
    let mut held = false;
    for row in rows {
        let why = row.why.as_deref().unwrap_or("not done");
        match row.outcome {
            Outcome::Ok | Outcome::NotApplicable | Outcome::Deferred => {}
            Outcome::Changed => match row.detail.as_deref() {
                Some(detail) => lines.extend(detail.lines().map(str::to_owned)),
                None => lines.push(format!("restored {}", row.title)),
            },
            Outcome::Planned => {
                warn = true;
                lines.push(format!(
                    "{}: {} — bare `aterm doctor` restores it",
                    row.title,
                    row.from.as_deref().unwrap_or("not as laid")
                ));
            }
            Outcome::Blocked if row.stood_aside.is_some() => lines.push(format!(
                "{}: {}",
                row.title,
                row.from.as_deref().unwrap_or_else(|| lead
                    .and_then(|lead| why.strip_suffix(lead))
                    .unwrap_or(why)
                    .trim_end_matches([':', ' ']))
            )),
            Outcome::Blocked if hold == Some(why) => {
                held = true;
                warn = true;
                let from = row.from.as_deref().unwrap_or("not as laid");
                lines.push(format!("{}: {from}", row.title));
            }
            Outcome::Blocked if !held_back.is_empty() && why.ends_with(&held_back) => {
                warn = true;
                lines.push(format!(
                    "{}: {}",
                    row.title,
                    why.strip_suffix(&held_back).unwrap_or(why)
                ));
            }
            Outcome::Failed => {
                warn = true;
                lines.push(format!("{}: NOT done — {why}{}", row.title, remedy(row)));
            }
            Outcome::Blocked | Outcome::Person | Outcome::Unknown => {
                warn = true;
                lines.push(format!("{}: {why}{}", row.title, remedy(row)));
            }
        }
    }
    let first = lead.or_else(|| hold.filter(|_| held));
    if first.is_none() && lines.is_empty() {
        lines.push("nothing to restore".to_owned());
    }
    let mark = match (warn, lead.is_some()) {
        (true, _) => Mark::Warn,
        (false, true) => Mark::Note,
        (false, false) => Mark::Ok,
    };
    let fact = match first {
        Some(first) if lines.is_empty() => first.to_owned(),
        Some(first) => format!("{first}\n{}", lines.join("\n")),
        None => lines.join("\n"),
    };
    Some(Section {
        label: "repair:",
        mark,
        fact,
    })
}

#[cfg(all(test, unix))]
#[path = "doctor/tests.rs"]
mod tests;
