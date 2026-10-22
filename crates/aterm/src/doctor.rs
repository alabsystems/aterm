// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `aterm doctor`, composed here (docs/DESIGN-init-system-2026-10-20.md §3.4, §5, S5):
//! aterm-cli's rows as items ([`aterm_cli::doctor`]), the packages' repair items
//! ([`atpkg::converge::repair`], the doctor's pass) and `aterm pkg doctor`'s rows, its
//! packages view.
//!
//! - Bare `aterm doctor` applies the `relay` and `asked` items (the 2026-10-14
//!   fix-by-default ruling) under a TRY-lock of the store. An install pass holding the
//!   lock is a note ("an install pass holds the store; nothing was repaired"): the items
//!   stand aside, no journal line is written, and unresolved work exits 1 (never 75).
//!   A lock that cannot be taken for another reason holds every change back: a warning.
//!   Every change is journaled ([`aterm_converge::FileJournal`] under the account's state
//!   root).
//! - `--read-only` probes and plans: nothing is written, the lock is not taken. So does a
//!   shell that is not the account's own (root, another `$HOME`, a private state root:
//!   `Requires::Account`), whose rows name each change it left.
//! - `--json` prints every row in the one report schema; `--only <id,...>` narrows the
//!   items and the packages' rows to those ids, families or layers (the shell is always
//!   read as a prerequisite), and a word that names nothing is the usage refusal, said
//!   before anything is read or handed over.
//! - Exit 0 means the selected checks are satisfied or explicitly not applicable;
//!   planned, manual, blocked, deferred, unknown and failed checks exit 1. JSON carries
//!   the shared readiness summary as well as the separate shell-usability row. Use
//!   `--read-only --only base.shell.starts` to check only whether the shell can start.
//! - `--check`, `--apply` and `--restore` are the macOS fleet settings profile alone
//!   ([`cli::settings`], docs/DESIGN-macos-doctor-2026-10-19.md): no item runs, and the
//!   report (or its `--json` document) is on stdout whatever the exit.

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
        // A fleet collector parses every answer, drift and failure included: stdout.
        let (text, code) = cli::settings(action, args.json);
        return cli::answer(&text, code, true);
    }
    let ground = atpkg::store::resolve_configured()
        .map(|layout| Arc::new(Ground::of_process(layout, Pass::Doctor)));
    // The Developer Tools row is aterm-cli's, read once: the packages view never reads it.
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
    // Every word is checked before anything is read or handed over: one that names no
    // doctor row (on any platform), no repair item and no packages row is the refusal.
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
    // The shell is always read, even when the requested scope is narrower.
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

    // The store lock: tried, never waited on (design §3.2), and not taken where nothing
    // is changed. An install pass holding it makes every item stand aside; a lock that
    // cannot be taken otherwise holds every change back.
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
        // Planned, stood aside, held back, or no state root: nothing is changed unrecorded.
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

    // The packages view, read after the repair: a row the items already ran is theirs
    // (the Developer Tools row is aterm-cli's). Readiness includes every remaining row,
    // without changing the essential/advisory policy used by `aterm pkg doctor`.
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
    // A recognized selector is not evidence that its check ran. This includes a
    // platform-specific row omitted by the gatherer; only an explicit NotApplicable
    // result can make it green.
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

/// Missing evidence must be a row, not an empty successful report.
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
/// and `base.clt` over atpkg's C toolchain probe (S6).
#[cfg(target_os = "macos")]
pub(crate) fn init_base(applies: bool) -> aterm_machines::init::Base {
    let base = match atpkg::store::resolve_configured() {
        Some(layout) => {
            init_base_over(&Arc::new(Ground::of_process(layout, Pass::Doctor)), applies)
        }
        None => aterm_machines::init::Base::none(),
    };
    aterm_machines::init::Base {
        clt: command_line_tools(),
        ..base
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
        };
    }
    match atpkg::lock::try_lock_store(&ground.layout) {
        Ok(lock) => Base {
            items,
            held: Some(Box::new(lock)),
            clt: None,
        },
        Err(why @ atpkg::lock::StoreLockError::Contended(_)) => Base {
            items: aterm_converge::standing_aside(items, &StoodAside::Lock(why.to_string())),
            held: None,
            clt: None,
        },
        Err(why) => Base {
            items: aterm_converge::held_back(
                items,
                &format!("the store lock could not be taken ({why})"),
            ),
            held: None,
            clt: None,
        },
    }
}

/// Whether `token` names `row` ([`aterm_converge::selects`]).
fn named_by(token: &str, row: &ItemReport) -> bool {
    aterm_converge::selects(token, &row.id, &row.family, row.layer)
}

/// The repair items' row: what was restored, what would be, what could not be and why.
/// A run that stood aside or held its changes back says why first, and then what each
/// item would have changed; so does a run outside the account's own shell (`hold`, the
/// line its held rows carry). `None` when no repair item ran.
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
    // A held-back change's why: what is not as laid, then this suffix.
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
            // What it would have changed, or (waiting on an item that stood aside) what
            // it waits for.
            Outcome::Blocked if row.stood_aside.is_some() => lines.push(format!(
                "{}: {}",
                row.title,
                row.from.as_deref().unwrap_or_else(|| lead
                    .and_then(|lead| why.strip_suffix(lead))
                    .unwrap_or(why)
                    .trim_end_matches([':', ' ']))
            )),
            // Named, never made, from a shell not the account's own.
            Outcome::Blocked if hold == Some(why) => {
                held = true;
                warn = true;
                let from = row.from.as_deref().unwrap_or("not as laid");
                lines.push(format!("{}: {from}", row.title));
            }
            // Named, never made, past a lock that could not be taken.
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
    // The first line is joined in front, never inserted: `Vec::insert` is a call whose
    // panic-freedom the verifier cannot establish, even at index 0.
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
