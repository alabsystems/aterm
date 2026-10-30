// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `aterm doctor` entry point, convergence runner, and macOS base init items (design:
//! docs/DESIGN-init-system-2026-10-20.md). Every run is timed: each row carries the engine's
//! time on it, the JSON's `phases` the run's, and one aterm.log line says the same totals.

use std::ffi::OsString;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

use aterm_cli::Mark;
use aterm_cli::doctor::{self as cli, Args, Gathered, Section};
use aterm_converge::{
    Effect, Item, ItemReport, Journal, Laps, Layer, Mode, Outcome, Readiness, Remedy, Report,
    ReportMode, Scope, StoodAside,
};
use atpkg::converge::repair::{self, Ground, Pass};

/// What bare doctor says when an install pass holds the store and the wait for it ran out
/// (design §3.2).
pub(crate) const CONTENDED: &str =
    "an install pass holds the store; nothing that needs it was repaired";

/// Why the pass's own items are left to it: a run nobody waits in, beside an install pass
/// that holds the store (each row's `why` ends with it, and it leads the `repair:` note).
pub(crate) const PASS_LAYS: &str = "the running install pass lays these";

/// What a [`Wait`] answers: the store lock, or why it was not had.
type Waited = Result<atpkg::lock::StoreLock, atpkg::lock::StoreLockError>;

/// The wait behind an install pass holding the store, for a person at the terminal
/// ([`wait_for_pass`]; a test's own).
pub(crate) type Wait = dyn Fn(&atpkg::store::Layout) -> Waited;

/// Whether a person is at this terminal, who waits for an install pass holding the store: stdin
/// and stderr, where the wait's one line goes, are both terminals.
fn person_at_terminal() -> bool {
    use std::io::IsTerminal as _;
    std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}

/// [`Wait`] for `aterm <verb>`: up to [`atpkg::lock::TYPED_WAIT`], the pass's own progress on one
/// line ([`atpkg::lock::lock_store_waiting_shown`]).
fn wait_for_pass(verb: &'static str) -> impl Fn(&atpkg::store::Layout) -> Waited {
    move |layout| {
        atpkg::lock::lock_store_waiting_shown(
            layout,
            atpkg::lock::TYPED_WAIT,
            &format!("aterm {verb}"),
        )
    }
}

/// `aterm doctor [--read-only] [--json] [--only <id,...>] | (--check | --apply | --restore)
/// [--json]` over this machine.
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
        let (text, code) = cli::settings(action, args.json, args.verbose);
        return cli::answer(&text, code, true, None);
    }
    let ground = atpkg::store::resolve_configured()
        .map(|layout| Arc::new(Ground::of_process(layout, Pass::Doctor)));
    let packages = || {
        ground
            .as_ref()
            // The repair items walk the same exec root on their own thread: one walk for both.
            .map(|ground| atpkg::doctor::packages_rows_sharing(&ground.layout, &ground.reads))
            .unwrap_or_default()
    };
    let state = aterm_types::dirs::account_state_dir();
    // The run's phase totals go to aterm.log ([`run`]), never to this terminal.
    aterm_gui::install_session_log();
    #[cfg(target_os = "macos")]
    let reading = aterm_machines::init::Doctor::new(Some(Arc::new(targo)));
    #[cfg(target_os = "macos")]
    let compose = |asked| develop_layer(&reading, asked);
    #[cfg(target_os = "macos")]
    let develop = Some(Develop {
        names: &aterm_machines::init_develop::names,
        compose: &compose,
    });
    #[cfg(not(target_os = "macos"))]
    let develop = None;
    #[cfg(target_os = "macos")]
    let rust_docs = || reading.rust_docs();
    #[cfg(target_os = "macos")]
    let docs: Option<&DocsRepair<'_>> = Some(&rust_docs);
    #[cfg(not(target_os = "macos"))]
    let docs = None;
    let wait = wait_for_pass("doctor");
    let ran = run(
        &args,
        &Ports {
            gather: &cli::gather,
            ground: ground.as_ref(),
            packages: &packages,
            state: state.as_deref(),
            not_own_account: aterm_types::dirs::not_own_account(),
            develop,
            docs,
            wait: person_at_terminal().then_some(&wait as &Wait),
        },
    );
    cli::answer(&ran.text, ran.code, ran.stdout, ran.hand_over)
}

/// The develop layer as `reading` composes it, for [`Develop::compose`].
#[cfg(target_os = "macos")]
fn develop_layer(reading: &aterm_machines::init::Doctor, asked: bool) -> DevelopLayer<'_> {
    let composed = reading.compose(asked);
    DevelopLayer {
        items: composed
            .items
            .iter()
            .map(|(item, _)| (item.id(), item.family().to_owned()))
            .collect(),
        unreadable: composed.unreadable,
        rows: Box::new(move |only| reading.rows(&composed, only)),
    }
}

/// The develop layer doctor reads on a develop machine (design §5, S9): macOS's
/// [`aterm_machines::init::Doctor`], a stand-in in tests, none elsewhere.
pub(crate) struct Develop<'a> {
    /// Whether an `--only` word can name a develop item at all (an id with or without its
    /// `@<path>`, a develop family, or `develop`: [`aterm_machines::init_develop::names`]).
    pub(crate) names: &'a dyn Fn(&str) -> bool,
    /// The layer, its profile read and nothing else; `true` when an `--only` word asked for
    /// it, so a Mac that records none reads its record row.
    pub(crate) compose: &'a dyn Fn(bool) -> DevelopLayer<'a>,
}

/// One run's develop layer, composed.
pub(crate) struct DevelopLayer<'a> {
    /// Each item's id and family.
    pub(crate) items: Vec<(String, String)>,
    /// The profile could not be read: its one row says why, whatever `--only` says.
    pub(crate) unreadable: bool,
    /// The rows, each item read and none applied, narrowed to the `--only` words.
    pub(crate) rows: DevelopRows<'a>,
}

/// The repair that keeps this login's `rust-docs` out of Spotlight, its items bound for the
/// engine, or why they cannot be built: macOS's `aterm_machines::init::Doctor::rust_docs`, a
/// stand-in in tests, none elsewhere. Called only once the gather found Spotlight indexing them.
pub(crate) type DocsRepair<'a> = dyn Fn() -> Result<Vec<Box<dyn Item + 'a>>, String> + 'a;

/// The id of the workstation item [`DocsRepair`] binds, without its `@<user>`.
const RUST_DOCS: &str = "init.spotlight.rust-docs";

/// [`DevelopLayer::rows`]: the `--only` words in, the rows or why they cannot be ordered out.
pub(crate) type DevelopRows<'a> =
    Box<dyn FnOnce(&[String]) -> Result<Vec<ItemReport>, String> + 'a>;

/// What a doctor run reads and writes, injected so a test runs it over a scratch home
/// and prefix.
pub(crate) struct Ports<'a> {
    /// Reads aterm-cli's facts the `--only` words name, `true` leaving the Developer Tools
    /// hand-over due after the report ([`cli::gather`]); called once, after every word is
    /// known to name something.
    pub(crate) gather: &'a dyn Fn(bool, &[String]) -> Gathered,
    /// The repair items' ground; `None` when no home folder, and so no store, can be found
    /// ([`aterm_types::dirs::home_dir`]).
    pub(crate) ground: Option<&'a Arc<Ground>>,
    /// `aterm pkg doctor`'s rows, read on a thread of their own beside the gather, and
    /// again after a repair that changed something; never when no word names one of them.
    pub(crate) packages: &'a (dyn Fn() -> Vec<ItemReport> + Sync),
    /// The account's state root, whose journal records each change; `None` refuses
    /// every change (nothing is changed unrecorded).
    pub(crate) state: Option<&'a Path>,
    /// Why this is not a shell of the account's own (root, another `$HOME`, a private
    /// state root: [`aterm_types::dirs::not_own_account`]): every item is read and none
    /// changed, the store is not locked and nothing is journaled (design §3.2).
    pub(crate) not_own_account: Option<&'static str>,
    /// The develop layer ([`Develop`]), read only in the account's own shell; `None` where
    /// aterm has none (not macOS).
    pub(crate) develop: Option<Develop<'a>>,
    /// The `rust-docs` repair ([`DocsRepair`]), built and run in the account's own shell when
    /// the gather found Spotlight indexing rust-docs pages; `None` where aterm has none.
    pub(crate) docs: Option<&'a DocsRepair<'a>>,
    /// The wait behind an install pass holding the store, for a person at the terminal, taken
    /// only when a repair needs the store ([`needs_the_store`]); `None` with nobody there, and
    /// the pass's own items are left to it ([`How::LeftToPass`]).
    pub(crate) wait: Option<&'a Wait>,
}

/// A run's answer: the text (the report, the JSON, or the refusal), its exit status,
/// whether it is a report (stdout whatever the code; a refusal goes to stderr), and the
/// Developer Tools hand-over due after it.
#[derive(Debug)]
pub(crate) struct Ran {
    pub(crate) text: String,
    pub(crate) code: u8,
    pub(crate) stdout: bool,
    pub(crate) hand_over: Option<cli::HandOver>,
}

/// How the repair items ran.
#[derive(Debug, Clone, PartialEq, Eq)]
enum How {
    /// Applied under the store lock.
    Applied,
    /// `--read-only`, or a shell not the account's own: probed and planned, unlocked.
    Planned,
    /// An install pass held the lock and the wait for it ran out: what touches nothing of the
    /// store runs unlocked and the rest stands aside ([`beside_the_pass`]), and this line says
    /// so (a note).
    StoodAside(String),
    /// An install pass held the lock and nobody waits for it, or no repair needs the lock
    /// ([`needs_the_store`]): its own items are left to it
    /// ([`PASS_LAYS`]), what touches nothing of the store runs unlocked, and the rest stands
    /// aside ([`beside_the_pass`]); this line says so (a note).
    LeftToPass(String),
    /// The lock could not be taken for another reason: each change is held back, and
    /// this line says why (a warning: the store is not in order).
    HeldBack(String),
}

/// Run `aterm doctor` as `args` asks over `ports`, each step a phase of the run: the early
/// and the final packages read, the develop layer, the gather, the store lock, the census
/// (the fact items) and the repairs.
pub(crate) fn run(args: &Args, ports: &Ports<'_>) -> Ran {
    let mut laps = Laps::start();
    let tokens = &args.only;
    let develop_word = |token: &str| {
        ports
            .develop
            .as_ref()
            .is_some_and(|develop| (develop.names)(token))
    };
    let (repairs, missed) = match ports.ground {
        Some(ground) => aterm_converge::select(repair::items(ground), tokens),
        None => (Vec::new(), tokens.clone()),
    };
    // Every word is judged against the names first, before anything is read; only a word no
    // name covers has the packages' rows read to tell whether one of them answers to it.
    let mut unmatched: Vec<String> = missed
        .into_iter()
        .filter(|token| {
            !cli::names(token) && !develop_word(token) && !atpkg::converge::names(token)
        })
        .collect();
    let mut read_early = None;
    if !unmatched.is_empty() {
        let rows = (ports.packages)();
        laps.lap("packages");
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
            stdout: false,
            hand_over: None,
        };
    }

    let own = ports.not_own_account.is_none();
    // The develop layer (design §5, S9), composed in the account's own shell when no word
    // narrows the run or one names a develop item, its profile read and nothing more (a
    // shell that is not the account's reads none: its checkouts and their code are the
    // account's). A develop word that names no item of this Mac's layer is the usage
    // refusal, as `aterm init`'s is, before anything else is read or handed over.
    let asked = tokens.iter().any(|token| develop_word(token));
    let develop = match &ports.develop {
        Some(develop) if own && (tokens.is_empty() || asked) => Some((develop.compose)(asked)),
        _ => None,
    };
    if let Some(layer) = develop.as_ref().filter(|layer| !layer.unreadable) {
        let nothing: Vec<&str> = tokens
            .iter()
            .filter(|token| develop_word(token))
            .filter(|token| {
                !layer.items.iter().any(|(id, family)| {
                    aterm_converge::selects(token, id, family, Some(Layer::Develop))
                })
            })
            .map(String::as_str)
            .collect();
        if !nothing.is_empty() {
            let checkouts: Vec<&str> = layer
                .items
                .iter()
                .map(|(id, _)| id.as_str())
                .filter(|id| id.starts_with("develop.checkout@"))
                .collect();
            return Ran {
                text: format!(
                    "aterm doctor: --only {}: names no develop item of this Mac ({}) — usage: \
                     aterm doctor {}\n",
                    nothing.join(","),
                    if checkouts.is_empty() {
                        "init.toml's [develop] records no checkout; `aterm init --develop` \
                         records one"
                            .to_owned()
                    } else {
                        format!("its checkouts: {}", checkouts.join(", "))
                    },
                    cli::GRAMMAR
                ),
                code: 2,
                stdout: false,
                hand_over: None,
            };
        }
    }
    laps.lap("develop");
    // The packages' rows read from the start, beside everything else, when no word narrows
    // the run or one names a packages row.
    let packages = tokens.is_empty() || tokens.iter().any(|token| atpkg::converge::names(token));
    std::thread::scope(|scope| {
        let reading = (read_early.is_none() && packages).then(|| scope.spawn(ports.packages));
        converge(args, ports, repairs, develop, laps, || {
            read_early.or_else(|| {
                reading.map(|read| {
                    read.join()
                        .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
                })
            })
        })
    })
}

/// The rest of [`run`] once every word names something: the facts gathered, the repairs run
/// (under the store lock when they may change anything), the develop rows read, and the
/// report rendered, each step a lap of `laps`. `early` is the packages' rows read before or
/// beside it all, `None` when no word names one of them; its lap is only what is left of that
/// read once the repairs are done.
fn converge<'a>(
    args: &Args,
    ports: &Ports<'a>,
    repairs: Vec<Box<dyn Item>>,
    develop: Option<DevelopLayer<'a>>,
    mut laps: Laps,
    early: impl FnOnce() -> Option<Vec<ItemReport>>,
) -> Ran {
    let tokens = &args.only;
    let own = ports.not_own_account.is_none();
    // A document's reader is no person to bring to a pane: `--json` never hands over.
    let gathered = (ports.gather)(
        own && !args.read_only && !args.json && cli::hands_over(tokens),
        tokens,
    );
    laps.lap("gather");
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
    // This login's rust-docs, kept out of Spotlight once the gather found it indexing them (the
    // workstation item, a relay-class repair of the account's own): built only then, so a Mac
    // whose docs are out of Spotlight runs nothing for it, and never from another account's
    // shell, whose rustup is not its own.
    let mut docs_unbuilt = None;
    let docs: Vec<Box<dyn Item + 'a>> = match ports.docs {
        Some(docs) if own && gathered.rust_docs_indexed() => docs().unwrap_or_else(|why| {
            docs_unbuilt = Some(why);
            Vec::new()
        }),
        _ => Vec::new(),
    };

    let scope = Scope {
        not_own_account: ports.not_own_account,
        ..Scope::REPAIR
    };
    let mut lock = None;
    // Whether this run waited for an install pass: what it read of the store before is stale.
    let mut waited = false;
    let how = match ports.ground {
        _ if args.read_only || !own => How::Planned,
        Some(ground) if !repairs.is_empty() => {
            // A person at the terminal waits for an install pass holding the store when a repair
            // needs it, so one run is enough; with nobody there, or nothing for the lock to do,
            // the pass's own items are left to it and the report comes at once.
            let taken = match (atpkg::lock::try_lock_store(&ground.layout), ports.wait) {
                (Err(atpkg::lock::StoreLockError::Contended(_)), Some(wait))
                    if needs_the_store(&repairs, scope) =>
                {
                    waited = true;
                    let taken = wait(&ground.layout);
                    ground.reads.forget();
                    taken
                }
                (taken, _) => taken,
            };
            match taken {
                Ok(held) => {
                    lock = Some(held);
                    How::Applied
                }
                Err(atpkg::lock::StoreLockError::Contended(_)) if !waited => {
                    How::LeftToPass(format!("an install pass holds the store; {PASS_LAYS}"))
                }
                Err(atpkg::lock::StoreLockError::Contended(_)) => {
                    How::StoodAside(CONTENDED.to_owned())
                }
                Err(why) => How::HeldBack(format!(
                    "the store lock could not be taken ({why}); nothing was repaired"
                )),
            }
        }
        _ => How::Applied,
    };
    laps.lap("lock");
    let (facts, repairs, docs) = match &how {
        // The facts and the rust-docs touch nothing of the store (the stale copies are the
        // facts' one change; the rust-docs repair sets aside rustup's own toolchains' HTML).
        How::StoodAside(line) => (
            facts,
            beside_the_pass(repairs, &StoodAside::Lock(line.clone()), false),
            docs,
        ),
        How::HeldBack(line) => (
            aterm_converge::held_back(facts, line),
            aterm_converge::held_back(repairs, line),
            aterm_converge::held_back(docs, line),
        ),
        How::LeftToPass(_) => (
            facts,
            beside_the_pass(repairs, &StoodAside::Lock(CONTENDED.to_owned()), true),
            docs,
        ),
        How::Applied | How::Planned => (facts, repairs, docs),
    };
    let mode = if how == How::Planned {
        Mode::Plan
    } else {
        Mode::Apply
    };
    let mut journal: Box<dyn Journal> = match ports.state {
        Some(state) if matches!(how, How::Applied | How::LeftToPass(_) | How::StoodAside(_)) => {
            Box::new(aterm_converge::FileJournal::under(state))
        }
        _ => Box::new(aterm_converge::ReadOnly),
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
    laps.lap("census");
    let repair_rows = ran(&repairs, "doctor.repairs");
    let mut docs_rows = ran(&docs, "doctor.docs");
    for row in &mut docs_rows {
        if row.outcome == Outcome::Changed {
            row.detail = Some(format!(
                "{} \u{2192} {}",
                row.from.as_deref().unwrap_or("rust-docs in Spotlight"),
                row.to.as_deref().unwrap_or("kept out")
            ));
        }
    }
    if let Some(why) = docs_unbuilt {
        refused.push(unmeasured(
            RUST_DOCS,
            "spotlight",
            Some(Layer::Workstation),
            format!("rust-docs stay in Spotlight: {why}"),
        ));
    }
    laps.lap("repairs");
    if !fact_rows.iter().any(|row| row.id == cli::SHELL) {
        refused.push(unmeasured(
            cli::SHELL,
            "shell",
            Some(Layer::Base),
            "shell usability was not measured".to_owned(),
        ));
    }
    drop(lock);

    // An apply that changed something, or failed partway, may have changed what the early
    // read saw, as an install pass this run waited for did: the packages are read again after
    // it. Either read is in before the develop rows start, so a build a checkout's `--check`
    // runs is never counted as a lease or a live build.
    let rewrote = waited
        || fact_rows
            .iter()
            .chain(&repair_rows)
            .any(|row| matches!(row.outcome, Outcome::Changed | Outcome::Failed));
    let mut packages = match early() {
        Some(rows) if !rewrote => rows,
        Some(_) => (ports.packages)(),
        None => Vec::new(),
    };
    laps.lap("packages");
    let ran_ids: Vec<&str> = fact_rows
        .iter()
        .chain(&repair_rows)
        .map(|row| row.id.as_str())
        .collect();
    packages.retain(|row| {
        !ran_ids.contains(&row.id.as_str())
            && (tokens.is_empty() || tokens.iter().any(|token| named_by(token, row)))
    });
    if matches!(how, How::LeftToPass(_)) {
        packages.iter_mut().for_each(left_to_the_pass);
    }
    // `aterm pkg doctor`'s own policy, judged before the rows join doctor's: only a row that
    // fails that surface is unresolved here; its other warnings count as notes.
    let pkg_judged: Vec<ItemReport> = packages.iter().map(as_pkg_doctor_judges).collect();
    let pkg_failing = packages.iter().any(ItemReport::fails_surface);
    for row in &mut packages {
        row.essential = false;
    }

    // The develop rows, read once the store is free again and the packages are read (a
    // checkout's `--check` may build its verifier first): each item probed, none applied,
    // nothing journaled.
    let develop_rows = develop.map_or_else(Vec::new, |layer| {
        (layer.rows)(tokens).unwrap_or_else(|refusal| {
            refused.push(unmeasured(
                "doctor.develop",
                "develop",
                Some(Layer::Develop),
                refusal,
            ));
            Vec::new()
        })
    });
    laps.lap("develop");

    let missing_store = unmeasured(
        "base.pkg.store",
        "pkg",
        Some(Layer::Base),
        "the home folder cannot be found, so neither can the package store".to_owned(),
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
            .chain(&develop_rows)
            .chain(&packages)
            .chain(&refused)
            .any(|row| named_by(token, row))
        {
            refused.push(unmeasured(
                &format!("doctor.selection.{token}"),
                "doctor",
                None,
                format!("--only {token} selected no row in this run"),
            ));
        }
    }
    let rows: Vec<ItemReport> = fact_rows
        .iter()
        .chain(&repair_rows)
        .chain(&docs_rows)
        .chain(&develop_rows)
        .chain(&packages)
        .chain(&refused)
        .cloned()
        .collect();
    let judged: Vec<ItemReport> = fact_rows
        .iter()
        .chain(&repair_rows)
        .chain(&docs_rows)
        .chain(&develop_rows)
        .chain(&pkg_judged)
        .chain(&refused)
        .cloned()
        .collect();
    let readiness = Readiness::of(&judged, tokens);
    let code = u8::from(!readiness.all_green);
    aterm_log::info!(
        "aterm doctor{}: {} rows, exit {code}; {laps}",
        if mode == Mode::Plan {
            " (read only)"
        } else {
            ""
        },
        rows.len()
    );
    if args.json {
        let mode = if mode == Mode::Plan {
            ReportMode::Check
        } else {
            ReportMode::Apply
        };
        let mut report = Report::new(mode, readiness.all_green, rows);
        report.readiness = Some(readiness);
        report.phases = laps.phases();
        return match aterm_json::to_string(&report) {
            Ok(text) => Ran {
                text: format!("{text}\n"),
                code,
                stdout: true,
                hand_over: None,
            },
            Err(why) => Ran {
                text: format!("aterm doctor: cannot encode the report: {why}\n"),
                code: 70,
                stdout: false,
                hand_over: None,
            },
        };
    }
    let refused_rows = refused.clone();
    let mut sections: Vec<Section> = refused
        .into_iter()
        .map(|row| Section {
            label: match (row.family.as_str(), row.id.as_str()) {
                ("pkg", _) => "packages:",
                ("develop", _) => "develop:",
                ("shell", _) => "shell:",
                (_, "doctor.repairs") => "repair:",
                _ => "doctor:",
            },
            mark: Mark::Warn,
            fact: row.why.unwrap_or_default(),
        })
        .collect();
    let hold = ports.not_own_account.map(aterm_converge::not_own_account);
    sections.extend(repair_section(&repair_rows, &how, hold.as_deref()));
    sections.extend(docs_section(&docs_rows));
    sections.extend(develop_section(&develop_rows));
    if !packages.is_empty() {
        let (healthy, lines) = atpkg::doctor::packages_view(&packages);
        sections.push(Section {
            label: "packages:",
            mark: match (healthy, pkg_failing) {
                (true, _) => Mark::Ok,
                (false, false) => Mark::Note,
                (false, true) => Mark::Warn,
            },
            fact: lines.join("\n"),
        });
    }
    let text = if args.verbose {
        cli::render_readiness(&gathered, &fact_rows, &sections, &readiness)
    } else {
        let (more, changes) = brief_rows(
            &readiness,
            &fact_rows,
            [
                ("repair:", repair_rows.as_slice()),
                ("rust-docs:", docs_rows.as_slice()),
                ("develop:", develop_rows.as_slice()),
                ("packages:", packages.as_slice()),
                ("doctor:", refused_rows.as_slice()),
            ],
            &how,
        );
        cli::render_brief(&gathered, &fact_rows, &more, &readiness, &changes)
    };
    Ran {
        text,
        code,
        stdout: true,
        hand_over: gathered.hand_over(),
    }
}

/// A packages row the install pass holding the store lays (relay or install) and has not laid
/// yet, read as left to it ([`PASS_LAYS`]), as [`beside_the_pass`] leaves the repairs: the
/// toolset that pass is installing reads `NotApplicable`, not planned. A failure stays one.
fn left_to_the_pass(row: &mut ItemReport) {
    use aterm_converge::{Class, Explain};
    if matches!(row.class, Some(Class::Relay | Class::Install))
        && matches!(
            row.outcome,
            Outcome::Planned | Outcome::Blocked | Outcome::Unknown
        )
    {
        let why = match row.why.take().or_else(|| row.from.clone()) {
            Some(said) => format!("{said}; left as it is: {PASS_LAYS}"),
            None => format!("left as it is: {PASS_LAYS}"),
        };
        row.outcome = Outcome::NotApplicable;
        row.explain = Some(Explain {
            why: why.clone(),
            remedy: Remedy::None,
        });
        row.why = Some(why);
    }
}

/// A packages row as readiness counts it: as `aterm pkg doctor` judges it, so a row that does
/// not fail that surface ([`ItemReport::fails_surface`], judged while the row still says whether
/// it is essential) counts as a note, not unresolved. The JSON's row stays as it read.
fn as_pkg_doctor_judges(row: &ItemReport) -> ItemReport {
    let mut judged = row.clone();
    let settled = matches!(
        row.outcome,
        Outcome::Ok | Outcome::Changed | Outcome::NotApplicable
    );
    if !settled && !row.fails_surface() {
        judged.outcome = Outcome::NotApplicable;
    }
    judged
}

/// The rows bare doctor shows besides its facts' (which [`cli::render_brief`] reads itself):
/// one per unresolved row of the repairs, the develop layer, the packages and the refusals,
/// each under its section's label (a repair that stood aside for an install pass is a note: it
/// waits, nothing is wrong); and what the repairs changed, one phrase each.
fn brief_rows<'r>(
    readiness: &Readiness,
    fact_rows: &[ItemReport],
    sections: impl IntoIterator<Item = (&'static str, &'r [ItemReport])>,
    how: &How,
) -> (Vec<cli::Shown>, Vec<String>) {
    use cli::{Kind, Shown};
    let aside = match how {
        How::StoodAside(line) | How::HeldBack(line) => Some(line.as_str()),
        How::LeftToPass(_) | How::Applied | How::Planned => None,
    };
    let mut shown = Vec::new();
    let mut changes = Vec::new();
    for (label, rows) in sections {
        for row in rows {
            if row.outcome == Outcome::Changed {
                match row.detail.as_deref() {
                    Some(detail) => changes.extend(detail.lines().map(str::to_owned)),
                    None => changes.push(format!("restored {}", row.title)),
                }
                continue;
            }
            if !readiness.unresolved.contains(&row.id) {
                continue;
            }
            let why = row.why.as_deref().unwrap_or("not done");
            let text = match label {
                "packages:" => atpkg::doctor::sentence_and_fix(row),
                "rust-docs:" if row.outcome == Outcome::Planned => format!(
                    "{} \u{2014} fix: {DOCS_FIX}",
                    row.from.as_deref().unwrap_or("rust-docs in Spotlight")
                ),
                "rust-docs:" => format!("{why}{}", remedy(row)),
                "develop:" => format!("{}: {why}{}", row.id, remedy(row)),
                "doctor:" => why.to_owned(),
                _ => match (row.outcome, aside) {
                    (Outcome::Planned, _) => format!(
                        "{}: {} \u{2014} fix: bare `aterm doctor` restores it",
                        row.title,
                        row.from.as_deref().unwrap_or("not as laid")
                    ),
                    (Outcome::Blocked, _) if row.stood_aside.is_some() => format!(
                        "{}: not repaired, an install pass holds the store \u{2014} fix: run \
                         `aterm doctor` again once it ends",
                        row.title
                    ),
                    (Outcome::Blocked, Some(aside)) => format!(
                        "{}: {} \u{2014} {aside}",
                        row.title,
                        row.from.as_deref().unwrap_or("not as laid")
                    ),
                    (Outcome::Failed, _) => {
                        format!("{}: NOT done \u{2014} {why}{}", row.title, remedy(row))
                    }
                    _ => format!("{}: {why}{}", row.title, remedy(row)),
                },
            };
            let kind = if row.outcome == Outcome::Unknown {
                Kind::Unread
            } else if row.stood_aside.is_some()
                && matches!(how, How::StoodAside(_) | How::LeftToPass(_))
            {
                Kind::Note
            } else {
                Kind::NeedsYou
            };
            shown.push(Shown::new(row.id.clone(), label, kind, &text));
        }
    }
    // Every unresolved row is shown: one no section above holds and no fact row is, by its id.
    for id in &readiness.unresolved {
        if !shown.iter().any(|row| &row.id == id) && !fact_rows.iter().any(|row| &row.id == id) {
            shown.push(Shown::new(id.clone(), "doctor:", Kind::NeedsYou, id));
        }
    }
    (shown, changes)
}

/// Missing evidence as a row, not an empty successful report. No probe ran for it, so it took
/// none of the run's time.
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
        elapsed_ms: Some(0),
    }
}

/// The base items `aterm init` runs before its workstation items (design §4.2, S5): the
/// same repair items, the doctor's pass (an opted-out rc stays as the person left it),
/// `base.ai.accounts`, on a Mac `base.clt` over atpkg's C toolchain probe (S6), and
/// `base.pkg.toolset` ([`toolset`]), which installs the toolset once the lock is let go.
#[cfg(unix)]
pub(crate) fn init_base(lock: aterm_machines::init::BaseLock) -> aterm_machines::init::Base {
    let wait = wait_for_pass("init");
    let mut base = match atpkg::store::resolve_configured() {
        Some(layout) => init_base_over(
            &Arc::new(Ground::of_process(layout, Pass::Doctor)),
            lock,
            person_at_terminal().then_some(&wait as &Wait),
        ),
        None => aterm_machines::init::Base::none(),
    };
    base.items.push(Box::new(AiAccountsItem));
    // Apple's Command Line Tools exist on a Mac alone.
    #[cfg(target_os = "macos")]
    let clt = command_line_tools();
    #[cfg(not(target_os = "macos"))]
    let clt = None;
    aterm_machines::init::Base {
        clt,
        toolset: Some(toolset(lock)),
        targo: Some(Arc::new(targo)),
        ..base
    }
}

/// `base.pkg.toolset` ([`aterm_machines::init_toolset`]): its check side is `aterm pkg
/// doctor`'s toolset row beside atpkg's install consent ([`toolset_found`]), and its passes are
/// this executable's `aterm pkg seed` then `aterm pkg update` ([`run_pkg`]), the passes a first
/// window launch runs (update with the unattended lanes' `--defer-busy-flip`), each waiting for
/// the store lock unless `--no-wait` made `lock` [`aterm_machines::init::BaseLock::Try`].
#[cfg(unix)]
fn toolset(lock: aterm_machines::init::BaseLock) -> aterm_machines::init_toolset::Toolset {
    use aterm_machines::init_toolset::{Atpkg, Toolset, WAIT_LOCK};
    Toolset::new(
        Box::new(toolset_found),
        Box::new(run_pkg),
        (lock != aterm_machines::init::BaseLock::Try).then_some(WAIT_LOCK),
        Atpkg {
            update_flags: &[atpkg::cli::DEFER_BUSY_FLIP_FLAG],
            left_for_next_pass: atpkg::cli::left_for_next_pass,
        },
    )
}

/// What `base.pkg.toolset` reads: the toolset row of this account's store
/// ([`atpkg::doctor::toolset_row`]), whether atpkg may install unattended here
/// ([`atpkg::adoption::adoption`]) and whether Automatic updates are on
/// ([`atpkg::config::PackagesConfig::enabled`], what a window's launch lane stands by on).
#[cfg(unix)]
fn toolset_found() -> aterm_machines::init_toolset::Found {
    let Some(layout) = atpkg::store::resolve_configured() else {
        return aterm_machines::init_toolset::Found::Unread(
            "no home folder, so no atpkg store to read".to_owned(),
        );
    };
    let cfg = atpkg::config::cached();
    toolset_reads(
        atpkg::doctor::toolset_row(&layout).as_ref(),
        atpkg::adoption::adoption(&layout, cfg),
        cfg.enabled(),
        cfg.ignored_prefix.as_deref(),
    )
}

/// [`toolset_found`]'s verdict over the toolset `row`, atpkg's `adoption` answer, whether
/// Automatic updates are `enabled` and the `[packages] prefix` this build ignores: a row that
/// reads `Ok` is complete whatever the consent; a row that wants programs is missing them where
/// atpkg may install them and Automatic updates are on, and otherwise off, saying why first
/// (init never undoes a decline, Automatic updates off or `auto_install = false`).
#[cfg(unix)]
fn toolset_reads(
    row: Option<&ItemReport>,
    adoption: atpkg::Adoption,
    enabled: bool,
    ignored_prefix: Option<&Path>,
) -> aterm_machines::init_toolset::Found {
    use aterm_machines::init_toolset::Found;
    use atpkg::Adoption;
    let Some(row) = row else {
        return Found::Unread("`aterm pkg doctor` reads no toolset row".to_owned());
    };
    let said = row
        .explain
        .as_ref()
        .map(|explain| explain.why.clone())
        .or_else(|| row.why.clone())
        .or_else(|| row.from.clone())
        .unwrap_or_else(|| row.title.clone());
    match row.outcome {
        Outcome::Ok => return Found::Complete(row.to.clone().unwrap_or(said)),
        Outcome::Unknown => return Found::Unread(said),
        _ => {}
    }
    let off = match adoption {
        Adoption::Consented if row.outcome == Outcome::NotApplicable => return Found::Off(said),
        // A window's launch lane stands by while the switch is off, and so does init.
        Adoption::Consented if !enabled => "Automatic updates are off ([packages].enabled = \
                                            false in aterm.toml, or the retired auto_update = \
                                            false), so init installs no toolset; Settings \u{25b8} \
                                            Packages turns them on, or `aterm pkg install \
                                            --default-set` installs it"
            .to_owned(),
        Adoption::Consented => return Found::Missing(said),
        // The row says the decline, and how to lift it.
        Adoption::Declined => return Found::Off(said),
        Adoption::AutoInstallOff => "[packages].auto_install = false in aterm.toml (or a \
                                     [packages] table too broken to read), so init installs no \
                                     toolset; `aterm pkg install --default-set` installs it"
            .to_owned(),
        Adoption::IgnoredPrefix => ignored_prefix.map_or_else(
            || "aterm.toml names a [packages] prefix this build ignores".to_owned(),
            atpkg::config::ignored_prefix_note,
        ),
        Adoption::ManagerDisabled => {
            "this aterm pins no root key, so atpkg installs nothing".to_owned()
        }
    };
    Found::Off(format!("{off}: {said}"))
}

/// Run this executable's `aterm pkg <args>` as `base.pkg.toolset`'s pass: a child that
/// inherits this process's environment (the store, the config and the network it reads are
/// init's), standard input closed and its output kept, ended at `budget`. Under a private
/// state root it carries the window's lead ([`atpkg::cli::PRIVATE_STATE_ROOT_FLAG`]), as the
/// window's passes there do.
#[cfg(unix)]
fn run_pkg(
    args: &[String],
    budget: std::time::Duration,
) -> Result<aterm_machines::port::Ran, aterm_machines::port::RunFailed> {
    use aterm_machines::port::{Ran, RunFailed};
    let exe = std::env::current_exe()
        .and_then(|exe| exe.canonicalize())
        .map_err(|e| RunFailed::Spawn(format!("this aterm cannot name itself: {e}")))?;
    let mut child = aterm_uds::spawnfd::command(&exe);
    child.arg("pkg");
    if aterm_types::dirs::runs_under_private_state_root() {
        child.arg(atpkg::cli::PRIVATE_STATE_ROOT_FLAG);
    }
    child.args(args);
    let now = std::time::Instant::now();
    let deadline = now
        .checked_add(budget)
        .unwrap_or_else(|| now + std::time::Duration::from_secs(86_400 * 365));
    match aterm_uds::deadline::output_until_from(
        &mut child,
        aterm_uds::spawnfd::Stdio::null(),
        deadline,
    ) {
        Ok(out) => Ok(Ran {
            status: out.status.code(),
            stdout: out.stdout,
            stderr: out.stderr,
        }),
        Err(aterm_uds::deadline::RunError::TimedOut) => Err(RunFailed::TimedOut(budget)),
        Err(aterm_uds::deadline::RunError::Spawn(e) | aterm_uds::deadline::RunError::Wait(e)) => {
            Err(RunFailed::Spawn(e.to_string()))
        }
    }
}

/// The base items the converge pass re-lays after an update (`aterm init --converge`,
/// [`aterm_machines::sync`]): the doctor's repair items alone, the store lock as `lock` says and
/// never waited for, so a held store stands each item that needs it aside. Not
/// `base.ai.accounts` (the pass's `sync.accounts` row) or `base.clt` (a person's).
#[cfg(unix)]
pub(crate) fn sync_base(lock: aterm_machines::init::BaseLock) -> aterm_machines::init::Base {
    atpkg::store::resolve_configured().map_or_else(aterm_machines::init::Base::none, |layout| {
        init_base_over(
            &Arc::new(Ground::of_process(layout, Pass::Doctor)),
            lock,
            None,
        )
    })
}

/// The toolset's `targo` for `develop.workspace-tools@<path>`: the `bin/targo` shim of the
/// atpkg store (the shim, so its own exports apply), once it resolves into the store.
#[cfg(unix)]
fn targo() -> Result<std::path::PathBuf, String> {
    const INSTALL: &str = "the toolset's targo is not installed: `aterm pkg install \
                           --default-set` installs it";
    let layout = atpkg::store::resolve_configured().ok_or(INSTALL)?;
    atpkg::ops::which(&layout, "targo").ok_or(INSTALL)?;
    Ok(layout.bin_dir().join("targo"))
}

/// `base.ai.accounts`: converges `accounts.toml` and `fleet-accounts.toml`
/// during `aterm init` when a fleet accounts source is configured or discoverable.
#[cfg(unix)]
struct AiAccountsItem;

#[cfg(unix)]
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
        use aterm_agent::harness::cli::Env;
        match Env::from_process() {
            Ok(env) => ai_accounts_probe(&env),
            Err(why) => aterm_converge::Probe::NotApplicable { why },
        }
    }

    fn apply(&self) -> Result<(), String> {
        use aterm_agent::harness::{
            accounts_sync::{sync_accounts_for_init, sync_cli_accounts_for_init},
            cli::Env,
        };
        let env = Env::from_process()?;
        let home = env
            .home
            .as_deref()
            .ok_or_else(|| "no user home directory".to_owned())?;
        let roster_path = env
            .accounts_roster()
            .ok_or_else(|| "no config directory".to_owned())?;
        let others = sync_cli_accounts_for_init(home, &env.state, &roster_path, env.now);
        let claude = sync_accounts_for_init(home, &env.state, &roster_path, env.now).map(|_| ());
        let failed: Vec<String> = others
            .into_iter()
            .filter_map(|(cli, synced)| synced.err().map(|why| format!("{}: {why}", cli.id())))
            .collect();
        match claude {
            Err(why) if !ai_accounts_others_only(&env) => Err(why),
            _ if failed.is_empty() => Ok(()),
            _ => Err(failed.join("; ")),
        }
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

/// Whether `env`'s machine keeps another CLI's pool, or a pool syncing by default is due here
/// ([`ai_accounts_due`]), and no source of Claude Code's: then `base.ai.accounts` is the other
/// pools' alone, Claude Code's sync having nothing to read is no failure of it, and a failed
/// sync of theirs is what the item reports.
#[cfg(unix)]
fn ai_accounts_others_only(env: &aterm_agent::harness::cli::Env) -> bool {
    let (Some(home), Some(roster_path)) = (env.home.as_deref(), env.accounts_roster()) else {
        return false;
    };
    !aterm_agent::harness::accounts_sync::has_init_accounts_source(
        home,
        &env.state,
        &roster_path,
        env.now,
    ) && (ai_accounts_pools(env).is_some() || ai_accounts_due(env).is_some())
}

/// The other CLIs' pools beside Claude Code's roster in one phrase (`codex 1, kimi 1`,
/// `accounts_sync::cli_pools_words`); `None` where this machine keeps none.
#[cfg(unix)]
fn ai_accounts_pools(env: &aterm_agent::harness::cli::Env) -> Option<Result<String, String>> {
    use aterm_agent::harness::accounts_sync::{cli_pools, cli_pools_words};
    let (config, home) = (env.config.as_deref()?, env.home.as_deref()?);
    let pools = cli_pools(config, &env.state, home, env.now);
    if let Some(why) = pools.iter().find_map(|pool| pool.rows.as_ref().err()) {
        return Some(Err(why.clone()));
    }
    cli_pools_words(&pools).map(Ok)
}

/// The pools syncing by default whose rows of this machine's the vault's fleet lists while no
/// roster of theirs stands here (`accounts_sync::cli_pools_due`), in one phrase (`codex`);
/// `None` where none is due.
#[cfg(unix)]
fn ai_accounts_due(env: &aterm_agent::harness::cli::Env) -> Option<String> {
    let (config, home) = (env.config.as_deref()?, env.home.as_deref()?);
    let due = aterm_agent::harness::accounts_sync::cli_pools_due(config, &env.state, home, env.now);
    (!due.is_empty()).then(|| {
        due.iter()
            .map(|cli| cli.id())
            .collect::<Vec<_>>()
            .join(", ")
    })
}

/// `base.ai.accounts`' probe over `env`: not applicable where `[harness] accounts = "off"`
/// turns the accounts' sync off (`accounts_sync::accounts_sync_off`, which the item's apply
/// would refuse), else Claude Code's pool, as it always read
/// ([`ai_accounts_claude_probe`]), a change where a pool syncing by default has rows of this
/// machine's in the vault's fleet and no roster here yet ([`ai_accounts_due`]), and each other
/// CLI's whose roster this machine keeps after it (`other CLIs: codex 1, kimi 1`). A machine
/// with Claude Code's alone reads as it always did.
#[cfg(unix)]
fn ai_accounts_probe(env: &aterm_agent::harness::cli::Env) -> aterm_converge::Probe {
    use aterm_agent::harness::accounts_sync::{ACCOUNTS_SYNC_OFF, accounts_sync_off};
    use aterm_converge::Probe;
    if let Some(config) = env.config.as_deref() {
        match aterm_secrets::swarm::enabled_at(config) {
            Ok(false) => {
                return Probe::NotApplicable {
                    why: aterm_secrets::swarm::SWARM_OFF.to_owned(),
                };
            }
            Ok(true) => {}
            Err(why) => {
                return Probe::Unusable {
                    why: format!("{}: {why}", config.display()),
                };
            }
        }
    }
    if accounts_sync_off(env.config.as_deref()) {
        return Probe::NotApplicable {
            why: ACCOUNTS_SYNC_OFF.to_owned(),
        };
    }
    let claude = match (ai_accounts_claude_probe(env), ai_accounts_due(env)) {
        (claude, None) => claude,
        (Probe::Ok { now }, Some(due)) => Probe::Change {
            from: now,
            to: format!("{due} rows synced from the vault"),
        },
        (Probe::NotApplicable { .. }, Some(due)) => Probe::Change {
            from: format!("no {due} roster"),
            to: format!("{due} rows synced from the vault"),
        },
        (Probe::Change { from, to }, Some(due)) => Probe::Change {
            from,
            to: format!("{to}; {due} rows synced from the vault"),
        },
        (other, Some(_)) => other,
    };
    let pools = match ai_accounts_pools(env) {
        Some(Ok(pools)) => pools,
        Some(Err(why)) => return Probe::Unusable { why },
        None => return claude,
    };
    let others = format!("other CLIs: {pools}");
    match claude {
        Probe::Ok { now } => Probe::Ok {
            now: format!("{now}; {others}"),
        },
        Probe::NotApplicable { .. } => Probe::Ok { now: others },
        Probe::Change { from, to } => Probe::Change {
            from,
            to: format!("{to}; {others}"),
        },
        other => other,
    }
}

/// Claude Code's half of [`ai_accounts_probe`].
#[cfg(unix)]
fn ai_accounts_claude_probe(env: &aterm_agent::harness::cli::Env) -> aterm_converge::Probe {
    use aterm_agent::harness::{
        accounts,
        accounts_sync::{
            FLEET_ROSTER_FILE, configured_source, has_init_accounts_source,
            needs_init_accounts_sync,
        },
    };
    use aterm_converge::Probe;
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
        && !needs_init_accounts_sync(home, &env.state, &roster_path, env.now)
    {
        let (bound, state) = match store
            .bound(&raw, env.now)
            .and_then(|bound| store.load(env.now).map(|(state, _)| (bound, state)))
        {
            Ok(read) => read,
            Err(why) => return Probe::Unusable { why },
        };
        let enrolled = accounts::enrolled(&bound);
        let ready = bound
            .rows
            .iter()
            .filter(|r| {
                enrolled.contains(&r.label)
                    && state
                        .accounts
                        .get(&r.label)
                        .is_none_or(|a| a.auth != accounts::Auth::SignedOut)
            })
            .count();
        let own_label = bound.row_for_login(None).map(|r| r.label.as_str());
        let next_hint = accounts::renew_next(&bound, &state, &enrolled, own_label, env.now)
            .map(|label| format!("; next: `aterm harness accounts renew {label}`"))
            .unwrap_or_default();
        return Probe::Ok {
            now: format!(
                "{} accounts synced ({ready} enrolled and not signed out{next_hint})",
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

/// `base.clt` ([`aterm_machines::init_clt`]): its probe is atpkg's C toolchain probe over
/// this process's `PATH`, which raises no Apple dialog, beside whether the git init runs is
/// there (the tools', else Xcode's); its root half runs from this executable. `None` when this executable cannot name
/// itself.
#[cfg(target_os = "macos")]
fn command_line_tools() -> Option<aterm_machines::init_clt::CommandLineTools> {
    let aterm = std::env::current_exe()
        .and_then(|exe| exe.canonicalize())
        .ok()?;
    Some(aterm_machines::init_clt::CommandLineTools::new(
        Box::new(|sys: &dyn aterm_machines::port::System| {
            clt_found(
                &atpkg::prereq::probe(std::env::var_os("PATH").as_deref()),
                aterm_machines::init_github::git_installed(sys),
            )
        }),
        aterm,
    ))
}

/// What `base.clt` reads in a C toolchain verdict ([`atpkg::prereq::CcVerdict`]) and whether
/// the git init runs is there (`clt_git`: [`aterm_machines::items::github::installed_git`],
/// the tools' own, else Xcode's): a missing developer directory or `cc` is what the install
/// supplies, and so is git beside a compiler that answers (a developer directory without a
/// git, which the GitHub setup's clones need); an Xcode with its git is no install's to make;
/// a compiler that refuses with the tools installed is theirs already (an unaccepted Xcode
/// license, a PATH-first `cc` without an SDK); a compiler that did not answer is unknown.
#[cfg(target_os = "macos")]
fn clt_found(verdict: &atpkg::prereq::CcVerdict, clt_git: bool) -> aterm_machines::init_clt::Found {
    use aterm_machines::init_clt::Found;
    use atpkg::prereq::CcVerdict;
    match verdict {
        CcVerdict::Ready { .. } | CcVerdict::Broken { .. } if !clt_git => Found::Missing(format!(
            "no git for init to run, neither the Command Line Tools' nor Xcode's ({})",
            verdict.line()
        )),
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

/// [`init_base`] over `ground`, its store lock as `lock` says. An apply takes the store lock,
/// which init holds over these items alone (never while a workstation item waits on a person).
/// An install pass holding it is waited for with `wait` (a person at the terminal) when an item
/// would change the store ([`needs_the_store`]), or else its own items are left to it
/// ([`beside_the_pass`]), so init needs no second run; `--no-wait`
/// ([`aterm_machines::init::BaseLock::Try`]), or a wait that ran out, makes each item that
/// touches the store stand aside instead (exit 75). A lock that cannot be taken for another
/// reason holds each change back, a failure (exit 1), since no retry takes it.
#[cfg(unix)]
fn init_base_over(
    ground: &Arc<Ground>,
    lock: aterm_machines::init::BaseLock,
    wait: Option<&Wait>,
) -> aterm_machines::init::Base {
    use aterm_machines::init::{Base, BaseLock};
    use atpkg::lock::StoreLockError::Contended;
    let items = repair::items(ground);
    let base = |items, held: Option<Box<dyn std::any::Any>>| Base {
        items,
        held,
        clt: None,
        toolset: None,
        targo: None,
    };
    if lock == BaseLock::Read {
        return base(items, None);
    }
    let mut waited = false;
    let taken = match (atpkg::lock::try_lock_store(&ground.layout), wait) {
        (Err(Contended(_)), Some(wait))
            if lock == BaseLock::Wait && needs_the_store(&items, Scope::EVERY) =>
        {
            waited = true;
            let taken = wait(&ground.layout);
            ground.reads.forget();
            taken
        }
        (taken, _) => taken,
    };
    match taken {
        Ok(held) => base(items, Some(Box::new(held))),
        Err(why @ Contended(_)) => base(
            beside_the_pass(
                items,
                &StoodAside::Lock(why.to_string()),
                lock == BaseLock::Wait && !waited,
            ),
            None,
        ),
        Err(why) => base(
            aterm_converge::held_back(items, &format!("the store lock could not be taken ({why})")),
            None,
        ),
    }
}

/// `repairs` beside an install pass that holds the store, in their run order: the copied
/// commands, which touch nothing of the store, run without its lock; with `laid` (a run nobody
/// waits in) the pass's own items (relay and install) are left to it, each reading
/// `NotApplicable` ([`PASS_LAYS`], [`aterm_converge::left_to_another`]); and the rest (with
/// `laid`, the stale rustup trust link, which rewrites the store's records) stand aside on
/// `aside`.
fn beside_the_pass<'a>(
    repairs: Vec<Box<dyn Item + 'a>>,
    aside: &StoodAside,
    laid: bool,
) -> Vec<Box<dyn Item + 'a>> {
    use aterm_converge::Class;
    repairs
        .into_iter()
        .flat_map(|item| match item.class() {
            _ if item.id() == repair::COPIED_COMMANDS => vec![item],
            Class::Relay | Class::Install if laid => {
                aterm_converge::left_to_another(vec![item], PASS_LAYS)
            }
            _ => aterm_converge::standing_aside(vec![item], aside),
        })
        .collect()
}

/// Whether an apply of `items` under `scope` would change something of the store, as a read
/// without its lock finds them: a change in a class `scope` applies, other than the copied
/// commands, which run without the lock. A run the engine refuses is read as one that would.
fn needs_the_store(items: &[Box<dyn Item + '_>], scope: Scope) -> bool {
    let mut read = aterm_converge::ReadOnly;
    aterm_converge::run_scoped(items, Mode::Plan, scope, &mut read, &mut |_| {}).map_or(
        true,
        |rows| {
            rows.iter().any(|row| {
                row.outcome == Outcome::Planned
                    && row.id != repair::COPIED_COMMANDS
                    && row.class.is_some_and(|class| scope.applies(class))
            })
        },
    )
}

/// The asked repairs a launch of the release app makes by itself once its seed is done (design
/// §3.2), each journaled under the account's state root: the stale copies of aterm moved to the
/// Trash, the copied commands in `~/.local/bin` moved aside, and a stale rustup trust link
/// re-pointed when the store lock is free (else the next launch does). What moved, one line each;
/// nothing from another executable or a shell that is not the account's own.
#[cfg(target_os = "macos")]
pub(crate) fn launch_repairs() -> Vec<String> {
    if aterm_types::dirs::not_own_account().is_some() {
        return Vec::new();
    }
    let (Some(layout), Some(state)) = (
        atpkg::store::resolve_configured(),
        aterm_types::dirs::account_state_dir(),
    ) else {
        return Vec::new();
    };
    launch_repairs_over(
        &Arc::new(Ground::of_process(layout, Pass::Doctor)),
        &state,
        &cli::StaleCopies::new(atpkg::copies::prune, Some(atpkg::copies::prune_after)),
    )
}

/// [`launch_repairs`] over `ground`, journaled under `state`, the stale copies through `copies`:
/// only when `ground`'s executable is the installed release app's.
#[cfg(any(target_os = "macos", all(test, unix)))]
fn launch_repairs_over(
    ground: &Arc<Ground>,
    state: &Path,
    copies: &cli::StaleCopies,
) -> Vec<String> {
    if !ground
        .exe
        .as_deref()
        .is_some_and(atpkg::hooks::is_installed_release_exe)
    {
        return Vec::new();
    }
    // The stale copies and the copied commands touch nothing of the store, so they run without
    // its lock: the install pass that follows the seed starts beside them and never waits behind
    // a census or a Trash move. The rustup trust link rewrites the store's records, so only it
    // takes the lock, and only once a read without it finds the link stale.
    let mut free: Vec<Box<dyn Item + '_>> = vec![copies.item()];
    let mut store = Vec::new();
    for item in repair::items(ground) {
        match item.id().as_str() {
            repair::COPIED_COMMANDS => free.push(item),
            repair::RUSTUP_LOCAL => store.push(item),
            _ => {}
        }
    }
    let scope = Scope {
        applies: &[aterm_converge::Class::Asked],
        ..Scope::REPAIR
    };
    let (mut journal, mut read) = (
        aterm_converge::FileJournal::under(state),
        aterm_converge::ReadOnly,
    );
    let mut run = |items: &[Box<dyn Item + '_>], mode: Mode| {
        let journal: &mut dyn Journal = match mode {
            Mode::Apply => &mut journal,
            Mode::Plan => &mut read,
        };
        aterm_converge::run_scoped(items, mode, scope, journal, &mut |_| {})
            .inspect_err(|refusal| aterm_log::warn!("launch repairs: {refusal}"))
            .unwrap_or_default()
    };
    let mut rows = run(&free, Mode::Apply);
    if run(&store, Mode::Plan)
        .iter()
        .any(|row| row.outcome == Outcome::Planned)
        && let Ok(lock) = atpkg::lock::try_lock_store(&ground.layout)
    {
        rows.extend(run(&store, Mode::Apply));
        drop(lock);
    }
    let mut moved = Vec::new();
    for row in &rows {
        match row.outcome {
            Outcome::Changed if row.id == cli::COPIES => moved.push(copies.said()),
            Outcome::Changed => moved.extend(
                row.detail
                    .iter()
                    .flat_map(|detail| detail.lines().map(str::to_owned)),
            ),
            Outcome::Failed | Outcome::Blocked | Outcome::Unknown => aterm_log::warn!(
                "launch repairs: {}: {}",
                row.id,
                row.why.as_deref().unwrap_or("not done")
            ),
            _ => {}
        }
    }
    moved
}

fn named_by(token: &str, row: &ItemReport) -> bool {
    aterm_converge::selects(token, &row.id, &row.family, row.layer)
}

/// The act a row's explanation names, as a line's tail: an em dash and the command in
/// backticks, or nothing when it names none.
fn remedy(row: &ItemReport) -> String {
    match row.explain.as_ref().map(|e| &e.remedy) {
        Some(
            remedy @ (Remedy::Command(_) | Remedy::Repair | Remedy::Init { .. } | Remedy::Develop),
        ) => remedy
            .command()
            .map(|command| format!(" — `{command}`"))
            .unwrap_or_default(),
        _ => String::new(),
    }
}

/// What keeps rust-docs out of Spotlight, as a planned row's fix says it.
const DOCS_FIX: &str = "bare `aterm doctor` keeps them out of Spotlight";

/// The rust-docs repair's section: what it did, or what is left and why; `None` when it did
/// not run (Spotlight indexes no rust-docs page, or the run is not in the account's own shell).
fn docs_section(rows: &[ItemReport]) -> Option<Section> {
    let row = rows.first()?;
    let why = row.why.as_deref().unwrap_or("not done");
    let (mark, fact) = match row.outcome {
        Outcome::Ok | Outcome::NotApplicable => (Mark::Ok, row.to.clone().unwrap_or_default()),
        Outcome::Changed => (Mark::Ok, row.detail.clone().unwrap_or_default()),
        Outcome::Planned => (
            Mark::Warn,
            format!(
                "{} \u{2014} {DOCS_FIX}",
                row.from.as_deref().unwrap_or("rust-docs in Spotlight")
            ),
        ),
        Outcome::Blocked if row.stood_aside.is_some() => (Mark::Note, why.to_owned()),
        Outcome::Failed => (
            Mark::Warn,
            format!("NOT done \u{2014} {why}{}", remedy(row)),
        ),
        _ => (Mark::Warn, format!("{why}{}", remedy(row))),
    };
    Some(Section {
        label: "rust-docs:",
        mark,
        fact,
    })
}

/// The develop rows' section (design §5, S9): one line per item of the recorded checkouts,
/// by its id, saying what it read and, where something is to do, the act that does it.
/// `None` when no develop row was read.
fn develop_section(rows: &[ItemReport]) -> Option<Section> {
    if rows.is_empty() {
        return None;
    }
    let mut warn = false;
    let mut lines = Vec::with_capacity(rows.len());
    for row in rows {
        let why = row.why.as_deref().unwrap_or("not read");
        let said = match row.outcome {
            Outcome::Ok | Outcome::Changed => row.to.clone().unwrap_or_else(|| "ok".to_owned()),
            Outcome::NotApplicable => format!("not applicable: {why}"),
            Outcome::Planned => {
                warn = true;
                format!(
                    "{} → {}{}",
                    row.from.as_deref().unwrap_or("not as wanted"),
                    row.to.as_deref().unwrap_or("as wanted"),
                    remedy(row)
                )
            }
            Outcome::Person
            | Outcome::Blocked
            | Outcome::Deferred
            | Outcome::Unknown
            | Outcome::Failed => {
                warn = true;
                format!("{why}{}", remedy(row))
            }
        };
        lines.push(format!("{}: {said}", row.id));
    }
    Some(Section {
        label: "develop:",
        mark: if warn { Mark::Warn } else { Mark::Ok },
        fact: lines.join("\n"),
    })
}

fn repair_section(rows: &[ItemReport], how: &How, hold: Option<&str>) -> Option<Section> {
    if rows.is_empty() {
        return None;
    }
    let (lead, lead_warns) = match how {
        How::StoodAside(line) | How::LeftToPass(line) => (Some(line.as_str()), false),
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
