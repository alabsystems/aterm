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
//! its passes would wait on the lock init holds. A stop (a Ctrl-C, a closed tab) while they run
//! ends the running pass with its process group, says so on this run's standard error, and then
//! ends the run, as the stop would have.

use crate::items::{Ctx, Item};
use crate::model::Probe;
use crate::port::{Ran, RunFailed, System};
use aterm_converge::StoodAside;
use std::sync::{Arc, Mutex};
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
/// environment, its standard input closed and its output kept, ended (it and its process group)
/// at the budget ([`RunFailed::TimedOut`]) or once the stop reading (the third argument) reads
/// true, an end the caller tells by its own stop.
pub type Pkg =
    Box<dyn Fn(&[String], Duration, &dyn Fn() -> bool) -> Result<Ran, RunFailed> + Send + Sync>;

/// What the line a stop of the passes says starts with ([`Toolset::say_stopped`]).
const STOPPED_LEAD: &str = "aterm init: the ALab toolset: ";

/// `base.pkg.toolset`: `aterm pkg seed`, then `aterm pkg update`, when the toolset is missing
/// programs atpkg may install.
pub struct Toolset {
    found: ReadToolset,
    pkg: Pkg,
    /// `--wait-lock`'s bound for each pass; `None` (`--no-wait`) stands aside for a pass that
    /// holds the store.
    wait: Option<Duration>,
    atpkg: Atpkg,
    selected: Option<&'static str>,
    aside: Mutex<Option<StoodAside>>,
    /// Where a stop of the passes is said, before the stop is sent again ends the run: this
    /// run's standard error.
    say: Arc<dyn Fn(&str) + Send + Sync>,
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
            selected: None,
            aside: Mutex::new(None),
            say: Arc::new(|text: &str| {
                use std::io::Write as _;
                let _ = std::io::stderr().lock().write_all(text.as_bytes());
            }),
        }
    }

    /// Run only this signed default-selected unit through the same unattended
    /// package policy and process custody. It does not seed or complete the toolset.
    #[must_use]
    pub fn default_selected(self, program: &'static str) -> Self {
        Self {
            selected: Some(program),
            ..self
        }
    }

    /// This item saying a stop of its passes through `say` (a test's).
    #[cfg(test)]
    #[must_use]
    pub(crate) fn saying(self, say: Arc<dyn Fn(&str) + Send + Sync>) -> Self {
        Self { say, ..self }
    }

    /// The arguments after `aterm pkg` for `verb`'s pass: `update` carries what every
    /// unattended lane's whole update carries ([`Atpkg::update_flags`]).
    #[must_use]
    pub fn args(&self, verb: &str) -> Vec<String> {
        let mut args = vec![verb.to_owned()];
        if verb == "update" {
            args.extend(self.atpkg.update_flags.iter().map(|&flag| flag.to_owned()));
            if let Some(program) = self.selected {
                args.extend(["--default-selected".to_owned(), program.to_owned()]);
            }
        }
        if let Some(wait) = self.wait {
            args.push("--wait-lock".to_owned());
            args.push(wait.as_secs().to_string());
        }
        args
    }

    /// Run the package passes with full admission at each boundary and a cancellation
    /// check while the child runs. `continuing` must not wait for locks or repeat full
    /// input-integrity proofs; it cannot replace `current` before or after either pass.
    /// # Errors
    /// Admission is withdrawn, a pass fails, or the run is stopped.
    pub fn apply_with_guards(
        &self,
        sys: &dyn System,
        current: &(dyn Fn() -> Result<(), String> + Sync),
        continuing: &(dyn Fn() -> Result<(), String> + Sync),
    ) -> Result<(), String> {
        current()?;
        self.set_aside(None);
        // Each pass runs in a process group of its own, which a Ctrl-C at the terminal or a
        // closed tab never reaches: held while the passes run, a stop ends the running pass's
        // group (the store keeps what it finished), starts no other, is said on standard error
        // ([`Toolset::say_stopped`]), and is sent again as this returns, so the run stops as it
        // would have and leaves no pass behind it writing to pipes nobody reads.
        let _stops = sys.defer_stops();
        let stopped = || sys.stopped().is_some() || continuing().is_err();
        let waited = self.wait.unwrap_or_default();
        let passes = if self.selected.is_some() {
            &[("update", UPDATE_BUDGET)][..]
        } else {
            &[("seed", SEED_BUDGET), ("update", UPDATE_BUDGET)][..]
        };
        let named = |at: usize| {
            passes
                .get(at)
                .map(|&(verb, _)| format!("aterm pkg {}", self.args(verb).join(" ")))
        };
        for (at, &(verb, budget)) in passes.iter().enumerate() {
            current()?;
            let args = self.args(verb);
            let shown = format!("aterm pkg {}", args.join(" "));
            if let Some(sig) = sys.stopped() {
                let when = at.checked_sub(1).and_then(named).map_or_else(
                    || format!("before `{shown}`"),
                    |done| after(&done, Some(&shown)),
                );
                return Err(self.say_stopped(sig, &when));
            }
            let ran = (self.pkg)(&args, budget.saturating_add(waited), &stopped);
            if let Some(sig) = sys.stopped() {
                // A pass that did not return its run was ended (the stop it read ends its group
                // at once); one that returned it had finished before the stop was read.
                let when = if matches!(ran, Err(RunFailed::TimedOut(_))) {
                    format!("during `{shown}`, which was ended")
                } else {
                    after(&shown, named(at + 1).as_deref())
                };
                return Err(self.say_stopped(sig, &when));
            }
            current()?;
            let ran = ran.map_err(|failed| {
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

    fn set_aside(&self, aside: Option<StoodAside>) {
        *self
            .aside
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = aside;
    }

    /// The row's failure for stop `sig`, read `when` among the passes, said first on this run's
    /// standard error: the stop sent again as the apply lets the stops go ends the run before
    /// any row is reported, so the said line is what the person sees of it.
    fn say_stopped(&self, sig: i32, when: &str) -> String {
        let why = format!(
            "stopped ({}) {when}; the store keeps what it finished",
            crate::port::stop_name(sig)
        );
        (self.say)(&format!("{STOPPED_LEAD}{why}\n"));
        why
    }
}

/// Where a stop read once the pass `done` finished came: after it, and before the pass `next`
/// where one follows.
fn after(done: &str, next: Option<&str>) -> String {
    match next {
        Some(next) => format!("after `{done}`, before `{next}`"),
        None => format!("after `{done}`"),
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

    fn apply(&self, sys: &dyn System, ctx: &Ctx) -> Result<(), String> {
        self.apply_guarded(sys, ctx, &|| Ok(()))
    }

    fn apply_guarded(
        &self,
        sys: &dyn System,
        _ctx: &Ctx,
        current: &(dyn Fn() -> Result<(), String> + Sync),
    ) -> Result<(), String> {
        self.apply_with_guards(sys, current, current)
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
#[path = "init_toolset_tests.rs"]
pub(crate) mod tests;
