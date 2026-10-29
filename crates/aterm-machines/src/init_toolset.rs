// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `base.pkg.toolset`: the ALab toolset (the default set here, every agent CLI included),
//! installed by the passes a first window launch runs, `aterm pkg seed` then `aterm pkg update`,
//! so a Mac set up only by `aterm init` (over SSH, with no window) gets it too. Those passes
//! honour what a person said: a decline (`aterm pkg uninstall --all`), Automatic updates off
//! (`[packages].enabled = false`), `[packages].auto_install = false`, `[packages].exclude` and
//! each program they uninstalled.
//!
//! It runs once the base layer let the store lock go ([`crate::init::Base::toolset`]): before,
//! its passes would wait on the lock init holds.

use crate::items::{Ctx, Item};
use crate::model::Probe;
use crate::port::{Ran, RunFailed, System};
use aterm_converge::StoodAside;
use std::sync::Mutex;
use std::time::Duration;

/// The item's id: `aterm pkg doctor`'s toolset row. The develop layer's tools run after it in
/// the lane, whatever it ends as: their own probe reads the `targo` they need.
pub const ID: &str = "base.pkg.toolset";

/// The item's family, which `--only pkg` names with the store's repair items.
pub const FAMILY: &str = "pkg";

/// How long each pass waits for another atpkg run holding the store (the window's own
/// `--wait-lock`), unless `--no-wait` makes it stand aside at once.
pub const WAIT_LOCK: Duration = Duration::from_secs(1800);

/// How long `aterm pkg seed` may run past its wait for the lock: it lays stubs and links and
/// downloads nothing.
const SEED_BUDGET: Duration = Duration::from_secs(10 * 60);

/// How long `aterm pkg update` may run past its wait for the lock: the whole first install.
const UPDATE_BUDGET: Duration = Duration::from_secs(3 * 3600);

/// atpkg's exit when another run holds the store lock past its wait (or at once without one).
pub const CONTENDED: i32 = 75;

/// atpkg's exit when neither the index nor any vendor channel answered.
pub const OFFLINE: i32 = 69;

/// `aterm pkg update`'s exit when the pass installed nothing on an empty store (an unserved
/// platform, an exclude or uninstalls that leave nothing): the toolset row says why.
const NOTHING_INSTALLED: i32 = 2;

/// How many of a failed pass's last lines on stderr its row carries: atpkg's failure line and
/// the lines it points back to.
const SAID_LINES: usize = 3;

/// What the item takes from atpkg's command line, which this crate does not link: the host
/// fills it from `atpkg::cli`.
#[derive(Clone, Copy)]
pub struct Atpkg {
    /// The words every whole `update` an unattended lane runs carries after its verb (a window's
    /// package loop, a terminal session's pass): `--defer-busy-flip`, so a toolchain flip waits
    /// while a build runs from the toolchain it replaces.
    pub update_flags: &'static [&'static str],
    /// The lines of a pass's output (stdout and stderr) that leave a program for the next
    /// pass, a vendor landing whose signature check did not finish
    /// (`atpkg::cli::left_for_next_pass`): the pass exits 0 with that program not installed.
    pub left_for_next_pass: fn(&str) -> Vec<String>,
}

/// What the toolset reads as, in the item's terms: `aterm pkg doctor`'s toolset row (the one
/// default set: the index's programs and every vendor CLI that serves this machine), beside
/// whether atpkg may install anything unattended here and Automatic updates are on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Found {
    /// Every program of the default set here is installed, or left out on purpose: what is
    /// active (the row's words).
    Complete(String),
    /// Programs of the default set are missing and atpkg may install them: which (the row's
    /// words).
    Missing(String),
    /// atpkg installs nothing unattended here (a decline, Automatic updates off,
    /// `[packages].auto_install = false`, an ignored prefix, a build that pins no root key), and
    /// the toolset is not complete: why.
    Off(String),
    /// The store could not be read: why.
    Unread(String),
}

/// The read [`Toolset`] probes through.
pub type ReadToolset = Box<dyn Fn() -> Found + Send + Sync>;

/// Runs `aterm pkg <args>` (this executable) as a child that inherits this process's
/// environment, its standard input closed and its output kept, ended at the budget
/// ([`RunFailed::TimedOut`]).
pub type Pkg = Box<dyn Fn(&[String], Duration) -> Result<Ran, RunFailed> + Send + Sync>;

/// `base.pkg.toolset`: `aterm pkg seed`, then `aterm pkg update`, when the toolset is missing
/// programs atpkg may install.
pub struct Toolset {
    found: ReadToolset,
    pkg: Pkg,
    /// `--wait-lock`'s bound for each pass; `None` (`--no-wait`) stands aside for a pass that
    /// holds the store.
    wait: Option<Duration>,
    atpkg: Atpkg,
    aside: Mutex<Option<StoodAside>>,
}

impl Toolset {
    /// The item over `found`, its passes run by `pkg` in `atpkg`'s words, each waiting `wait`
    /// for the store lock.
    #[must_use]
    pub fn new(found: ReadToolset, pkg: Pkg, wait: Option<Duration>, atpkg: Atpkg) -> Self {
        Self {
            found,
            pkg,
            wait,
            atpkg,
            aside: Mutex::new(None),
        }
    }

    /// The arguments after `aterm pkg` for `verb`'s pass: `update` carries what every
    /// unattended lane's whole update carries ([`Atpkg::update_flags`]).
    #[must_use]
    pub fn args(&self, verb: &str) -> Vec<String> {
        let mut args = vec![verb.to_owned()];
        if verb == "update" {
            args.extend(self.atpkg.update_flags.iter().map(|&flag| flag.to_owned()));
        }
        if let Some(wait) = self.wait {
            args.push("--wait-lock".to_owned());
            args.push(wait.as_secs().to_string());
        }
        args
    }

    fn set_aside(&self, aside: Option<StoodAside>) {
        *self
            .aside
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = aside;
    }
}

/// Where `aterm init` places the item.
#[must_use]
pub fn place() -> crate::engine::Place {
    crate::engine::Place {
        layer: aterm_converge::Layer::Base,
        class: aterm_converge::Class::Install,
        requires: aterm_converge::Requires::Account,
        remedy: aterm_converge::Remedy::Init {
            only: ID.to_owned(),
        },
    }
}

/// Whether the `--only` word `word` names the item ([`aterm_converge::selects`]).
#[must_use]
pub fn selected(word: &str) -> bool {
    aterm_converge::selects(word, ID, FAMILY, Some(aterm_converge::Layer::Base))
}

/// What a pass said last: its last lines on stderr ([`SAID_LINES`], in order, joined by `; `),
/// else its last line on stdout.
fn said(ran: &Ran) -> String {
    let last = |text: &str, n: usize| -> Vec<String> {
        let lines: Vec<&str> = text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect();
        lines[lines.len().saturating_sub(n)..]
            .iter()
            .map(|&line| line.to_owned())
            .collect()
    };
    let mut lines = last(&ran.err(), SAID_LINES);
    if lines.is_empty() {
        lines = last(&ran.out(), 1);
    }
    if lines.is_empty() {
        return "it said nothing".to_owned();
    }
    lines.join("; ")
}

impl Item for Toolset {
    fn id(&self) -> String {
        ID.to_owned()
    }

    fn family(&self) -> &'static str {
        FAMILY
    }

    fn title(&self) -> String {
        "the ALab toolset (every agent CLI included)".to_owned()
    }

    fn probe(&self, _sys: &dyn System, _ctx: &Ctx) -> Probe {
        match (self.found)() {
            Found::Complete(now) => Probe::Ok { now },
            Found::Missing(from) => Probe::Change {
                from,
                to: "the ALab default set, installed by `aterm pkg seed` then `aterm pkg \
                     update` (a first window launch's passes)"
                    .to_owned(),
            },
            Found::Off(why) => Probe::NotApplicable { why },
            Found::Unread(why) => Probe::Unusable { why },
        }
    }

    fn apply(&self, _sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        self.set_aside(None);
        let waited = self.wait.unwrap_or_default();
        for (verb, budget) in [("seed", SEED_BUDGET), ("update", UPDATE_BUDGET)] {
            let args = self.args(verb);
            let shown = format!("aterm pkg {}", args.join(" "));
            let ran = (self.pkg)(&args, budget.saturating_add(waited)).map_err(|failed| {
                format!("`{shown}`: {failed}; the store keeps what it finished")
            })?;
            match ran.status {
                Some(0) if verb == "update" => {
                    // No fault, but a program left for the next pass (its signature check did
                    // not finish) is still missing: the next run installs it.
                    let left =
                        (self.atpkg.left_for_next_pass)(&format!("{}\n{}", ran.out(), ran.err()));
                    if !left.is_empty() && matches!((self.found)(), Found::Missing(_)) {
                        self.set_aside(Some(StoodAside::Lock(format!(
                            "{}; the next `aterm init` installs the rest",
                            left.join("; ")
                        ))));
                        return Ok(());
                    }
                }
                Some(0) => {}
                // The row read back says why nothing was installed.
                Some(NOTHING_INSTALLED) if verb == "update" => {}
                Some(CONTENDED) => {
                    self.set_aside(Some(StoodAside::Lock(format!(
                        "another atpkg run holds the store (`{shown}` exited {CONTENDED}: {}); \
                         the next `aterm init` installs the toolset",
                        said(&ran)
                    ))));
                    return Ok(());
                }
                Some(OFFLINE) => {
                    self.set_aside(Some(StoodAside::Offline(format!(
                        "the ALab index could not be reached (`{shown}` exited {OFFLINE}: {}); \
                         the next `aterm init` installs the toolset",
                        said(&ran)
                    ))));
                    return Ok(());
                }
                _ => {
                    return Err(format!(
                        "`{shown}` {}: {}",
                        ran.status.map_or_else(
                            || "ended on a signal".to_owned(),
                            |code| format!("exited {code}")
                        ),
                        said(&ran)
                    ));
                }
            }
        }
        Ok(())
    }

    fn revert(&self) -> Option<String> {
        Some("aterm pkg uninstall --all".to_owned())
    }

    fn stood_aside(&self) -> Option<StoodAside> {
        self.aside
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::engine::bind;
    use crate::items::defaults::tests::ctx;
    use crate::model::{Mode, Outcome};
    use crate::port::Fake;
    use aterm_converge::{Class, Layer, NoJournal, Scope};
    use std::sync::Arc;

    /// A line the fake atpkg leaves for its next pass by.
    const LEFT: &str = "left for the next pass";

    fn left_for_next_pass(output: &str) -> Vec<String> {
        output
            .lines()
            .filter(|line| line.contains(LEFT))
            .map(str::to_owned)
            .collect()
    }

    /// atpkg's words as the tests speak them: one update flag, and [`LEFT`].
    pub(crate) const ATPKG: Atpkg = Atpkg {
        update_flags: &["--defer-busy-flip"],
        left_for_next_pass,
    };

    /// A store the passes change: what the read finds before and after them, and each pass's
    /// arguments and exit.
    #[derive(Default)]
    struct Store {
        found: Mutex<Vec<Found>>,
        ran: Mutex<Vec<Vec<String>>>,
        exits: Mutex<Vec<Ran>>,
    }

    fn item(store: &Arc<Store>, wait: Option<Duration>) -> Toolset {
        let (read, run) = (Arc::clone(store), Arc::clone(store));
        Toolset::new(
            Box::new(move || {
                let mut found = read.found.lock().unwrap();
                if found.len() > 1 {
                    found.remove(0)
                } else {
                    found[0].clone()
                }
            }),
            Box::new(move |args: &[String], _budget| {
                run.ran.lock().unwrap().push(args.to_vec());
                let mut exits = run.exits.lock().unwrap();
                Ok(if exits.is_empty() {
                    Ran::ok("")
                } else {
                    exits.remove(0)
                })
            }),
            wait,
            ATPKG,
        )
    }

    fn store(found: Vec<Found>, exits: Vec<Ran>) -> Arc<Store> {
        Arc::new(Store {
            found: Mutex::new(found),
            exits: Mutex::new(exits),
            ..Store::default()
        })
    }

    fn run(item: &Toolset, mode: Mode) -> aterm_converge::ItemReport {
        let sys = Fake::default();
        let ctx = ctx(&[]);
        let bound = bind([(item as &dyn Item, place())], &sys, &ctx);
        let mut rows =
            aterm_converge::run_scoped(&bound, mode, Scope::EVERY, &mut NoJournal, &mut |_| {})
                .unwrap();
        assert!(sys.calls().is_empty(), "{:?}", sys.calls());
        rows.remove(0)
    }

    fn missing() -> Found {
        Found::Missing("2 of 7 missing (claude, muse)".into())
    }

    fn complete() -> Found {
        Found::Complete("7 active".into())
    }

    /// The passes a window's launch lane runs: seed, then update with the unattended lanes'
    /// flag, each waiting for the store lock.
    #[test]
    fn a_missing_program_is_installed_by_seed_then_update_and_read_back() {
        let store = store(vec![missing(), complete()], Vec::new());
        let item = item(&store, Some(WAIT_LOCK));
        let row = run(&item, Mode::Apply);
        assert_eq!(row.outcome, Outcome::Changed, "{row:?}");
        assert_eq!(row.to.as_deref(), Some("7 active"));
        assert_eq!(
            (row.layer, row.class),
            (Some(Layer::Base), Some(Class::Install))
        );
        assert_eq!(
            *store.ran.lock().unwrap(),
            [
                &["seed", "--wait-lock", "1800"][..],
                &["update", "--defer-busy-flip", "--wait-lock", "1800"]
            ]
        );
    }

    /// The second run finds the set installed and runs no pass; a plan never runs one.
    #[test]
    fn a_complete_toolset_runs_nothing_and_a_plan_runs_nothing() {
        let done = store(vec![complete()], Vec::new());
        let row = run(&item(&done, Some(WAIT_LOCK)), Mode::Apply);
        assert_eq!(row.outcome, Outcome::Ok, "{row:?}");
        assert!(done.ran.lock().unwrap().is_empty());

        let wanted = store(vec![missing()], Vec::new());
        let row = run(&item(&wanted, Some(WAIT_LOCK)), Mode::Plan);
        assert_eq!(row.outcome, Outcome::Planned, "{row:?}");
        assert!(wanted.ran.lock().unwrap().is_empty());
    }

    /// A decline, Automatic updates off, `auto_install = false` and the rest of atpkg's
    /// refusals read as not applicable, carrying why, and run nothing: init never undoes a
    /// person's answer.
    #[test]
    fn what_atpkg_may_not_install_is_said_and_left_alone() {
        let off = store(vec![Found::Off("switched off".into())], Vec::new());
        let row = run(&item(&off, Some(WAIT_LOCK)), Mode::Apply);
        assert_eq!(row.outcome, Outcome::NotApplicable, "{row:?}");
        assert_eq!(row.why.as_deref(), Some("switched off"));
        assert!(off.ran.lock().unwrap().is_empty());

        let unread = store(vec![Found::Unread("no store".into())], Vec::new());
        let row = run(&item(&unread, Some(WAIT_LOCK)), Mode::Apply);
        assert_eq!(row.outcome, Outcome::Unknown, "{row:?}");
        assert!(unread.ran.lock().unwrap().is_empty());
    }

    /// A pass that fails ends the row Failed carrying atpkg's words (the line it points back to
    /// too), and update never runs after a failed seed; an update that installs part of the set
    /// and fails the rest (exit 1) is Failed the same way.
    #[test]
    fn a_failed_or_partial_install_is_the_rows_outcome_in_atpkgs_words() {
        let failed = store(
            vec![missing()],
            vec![Ran::exit(1, "atpkg: seed: the store is not writable\n")],
        );
        let row = run(&item(&failed, Some(WAIT_LOCK)), Mode::Apply);
        assert_eq!(row.outcome, Outcome::Failed, "{row:?}");
        let why = row.why.unwrap_or_default();
        assert!(
            why.contains("atpkg: seed: the store is not writable"),
            "{why}"
        );
        assert_eq!(failed.ran.lock().unwrap().len(), 1);

        let partial = store(
            vec![missing()],
            vec![
                Ran::ok(""),
                Ran::exit(
                    1,
                    "atpkg: muse: download refused\natpkg: default set incomplete: muse\n",
                ),
            ],
        );
        let row = run(&item(&partial, Some(WAIT_LOCK)), Mode::Apply);
        assert_eq!(row.outcome, Outcome::Failed, "{row:?}");
        let why = row.why.unwrap_or_default();
        for line in [
            "atpkg: muse: download refused",
            "atpkg: default set incomplete: muse",
        ] {
            assert!(why.contains(line), "{why}");
        }
        assert_eq!(partial.ran.lock().unwrap().len(), 2);
    }

    /// An update that exits 0 having left a program for its next pass (a signature check that
    /// did not finish) stands the row aside in atpkg's words, never Failed. CONTROL: the same
    /// line beside a set the read finds complete (an upgrade left for later) is Changed.
    #[test]
    fn a_program_left_for_the_next_pass_stands_the_row_aside() {
        let said = format!("atpkg: claude 2.1.300 {LEFT}");
        let deferred = store(
            vec![missing(), missing()],
            vec![
                Ran::ok(""),
                Ran::ok(&format!("atpkg: codex current\n{said}\n")),
            ],
        );
        let row = run(&item(&deferred, Some(WAIT_LOCK)), Mode::Apply);
        assert_eq!(row.outcome, Outcome::Blocked, "{row:?}");
        assert!(
            matches!(&row.stood_aside, Some(StoodAside::Lock(why)) if why.contains(&said)),
            "{row:?}"
        );

        let upgraded = store(
            vec![missing(), complete()],
            vec![Ran::ok(""), Ran::ok(&format!("{said}\n"))],
        );
        let row = run(&item(&upgraded, Some(WAIT_LOCK)), Mode::Apply);
        assert_eq!(row.outcome, Outcome::Changed, "{row:?}");
        assert_eq!(row.stood_aside, None);
    }

    /// `--no-wait` passes no `--wait-lock`, and a store another pass holds stands the row
    /// aside on the lock (exit 75), as no network stands it aside offline (exit 69).
    #[test]
    fn a_held_store_and_no_network_stand_the_row_aside() {
        for (exit, wait, aside_lock) in [(CONTENDED, None, true), (OFFLINE, Some(WAIT_LOCK), false)]
        {
            let held = store(
                vec![missing()],
                vec![Ran::ok(""), Ran::exit(exit, "atpkg: busy or offline\n")],
            );
            let row = run(&item(&held, wait), Mode::Apply);
            assert_eq!(row.outcome, Outcome::Blocked, "{row:?}");
            assert_eq!(
                matches!(row.stood_aside, Some(StoodAside::Lock(_))),
                aside_lock,
                "{row:?}"
            );
            assert_eq!(
                matches!(row.stood_aside, Some(StoodAside::Offline(_))),
                !aside_lock,
                "{row:?}"
            );
            let ran = held.ran.lock().unwrap();
            assert_eq!(ran[0][0], "seed");
            assert_eq!(ran[1][0], "update");
            assert_eq!(
                ran[1].iter().any(|arg| arg == "--wait-lock"),
                wait.is_some(),
                "{ran:?}"
            );
        }
    }

    #[test]
    fn the_item_is_named_by_its_id_its_family_and_the_base_layer() {
        for word in [ID, FAMILY, "base"] {
            assert!(selected(word), "{word}");
        }
        for word in ["base.pkg.toolse", "workstation", "base.clt", "develop"] {
            assert!(!selected(word), "{word}");
        }
    }
}
