// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-converge`: one desired state, one engine (docs/DESIGN-init-system-2026-10-20.md
//! §3).
//!
//! Every setting a surface converges is one [`Item`]: it probes (changing nothing),
//! applies, and explains itself. [`run`] orders the items by their [`Item::after`] edges,
//! takes each through probe, plan, apply and read-back, journals every change before it is
//! made ([`Journal`], [`FileJournal`]), and returns one [`ItemReport`] per item. Items whose
//! changes one consumer reads share an [`Activation`] (its relaunch): the engine applies
//! them all, runs it once, and only then reads them back.
//!
//! An item owns its ports (an `Arc<dyn System>`, a store layout, a parsed config), so one
//! run can mix items from crates that do not depend on each other, and this crate holds no
//! platform code. `aterm_machines::Bound` wraps the machines families, whose items take
//! their `System` and context per call.

mod engine;
mod journal;
mod model;
mod scope;

#[cfg(any(test, debug_assertions))]
pub use engine::NoJournal;
pub use engine::{Journal, ReadOnly, Refusal, named_by, order, run, run_scoped, run_with};
pub use journal::FileJournal;
pub use model::{
    Class, Effect, Explain, ItemReport, Layer, Mode, Outcome, Probe, Read, Readiness, Remedy,
    Report, ReportMode, Requires, SCHEMA, StoodAside,
};
pub use scope::{
    NOT_AT_CONSOLE, Scope, held_back, left_as_is, not_own_account, select, selects, standing_aside,
};
use std::sync::Arc;

/// What makes the changes of several items felt at once: the relaunch of a consumer that
/// reads what each of them writes (the Dock reads `init.desktop`'s keys and `init.dock`'s).
/// A consumer relaunched between two writes it reads can write what it read at its start
/// back over the second, so the writes all land first, the consumer relaunches once, and
/// each item is read back after it is up (design §3.3).
///
/// Items name one by [`Item::activation`]; two are the same activation when their ids are.
pub trait Activation: Send + Sync {
    /// Stable id, unique within a run: its journal line's (`init.relaunch@alice`).
    fn id(&self) -> String;

    /// What awaits activation, as its journal line's `from` and `to`, or `None` when
    /// nothing does. Must change nothing.
    ///
    /// # Errors
    /// One line when what awaits it cannot be read: every item it activates fails with it.
    fn pending(&self) -> Result<Option<(String, String)>, String>;

    /// Make the applied changes felt, and return only once they are (each relaunched
    /// consumer up again and past its start), so the read-backs after it see what the
    /// consumer settled on. Each item settles alone: the answer names, by [`Item::id`],
    /// each item whose part of it did not take effect (a consumer of its keys that did not
    /// start again), with why; that item fails with it, and every other one is read back.
    ///
    /// # Errors
    /// One line when none of it can be said to have taken effect: every item it activates
    /// fails with it, and what awaits it stays for the next run.
    fn activate(&self) -> Result<Vec<(String, String)>, String>;
}

/// One setting the engine probes, plans, applies and verifies.
pub trait Item: Send + Sync {
    /// Stable id, unique within a run (`base.shell.rc`, `keyboard.caps-lock.0-0-0@alice`).
    fn id(&self) -> String;

    /// The family: the `--only <table>` merge key a machines report keeps.
    fn family(&self) -> &'static str;

    /// The layer of the desired state the item belongs to.
    fn layer(&self) -> Layer;

    /// What its apply may do, and so which surfaces apply it.
    fn class(&self) -> Class;

    /// Where it may run.
    fn requires(&self) -> Requires {
        Requires::Account
    }

    /// One human line for the report.
    fn title(&self) -> String;

    /// Read the host and decide. Must change nothing.
    fn probe(&self) -> Probe;

    /// Make the change the last [`Item::probe`] planned. The engine probes again
    /// afterwards, and only a matching read-back counts.
    ///
    /// # Errors
    /// One line saying what did not work.
    fn apply(&self) -> Result<(), String>;

    /// Reapply an idempotent runtime setting whose saved preferences already match.
    /// Called only in apply mode after an `Ok` probe. `Some` requires another matching
    /// probe; `None` keeps the ordinary already-correct path without another read. It
    /// must not change the user's saved choice or install background work.
    fn refresh(&self) -> Option<Result<(), String>> {
        None
    }

    /// Whether [`Item::refresh`] changes the machine (a re-lay of files the item owns,
    /// not an activation of a saved setting): the engine then journals it before it runs,
    /// as it journals an apply, with the reading it re-lays as both `from` and `to`, and
    /// a journal that refuses the record leaves it unrun. `false`, the default: a
    /// refresh that only activates what is saved writes no line.
    fn refresh_changes(&self) -> bool {
        false
    }

    /// Why `probe` reads as it does, and the one remedy.
    fn explain(&self, probe: &Probe) -> Explain;

    /// Whether a probe that is not `Ok` fails the surface that runs the item.
    fn essential(&self) -> bool {
        false
    }

    /// The [`Activation`] this item's change shares with the other items whose writes the
    /// same consumer reads. In an apply run the engine reads an applied item back only
    /// after that activation ran: once, journaled first, after every item of the run that
    /// names it has been through its apply. An item whose [`Item::after`] names one of
    /// them runs after all of them. `None`, the default: the item's own apply makes its
    /// change felt, and it is read back at once.
    fn activation(&self) -> Option<Arc<dyn Activation + '_>> {
        None
    }

    /// Ids of items that must end `Ok` or `Changed` before this one may apply (the
    /// Command Line Tools before Homebrew). An id ending in `*` names every other item
    /// with that prefix. The run is ordered by these edges, and an edge that names no item
    /// of the run refuses it ([`Refusal`]).
    fn after(&self) -> Vec<String> {
        Vec::new()
    }

    /// Item-specific output a caller acts on, read after the run (a public key, a
    /// version).
    fn detail(&self) -> Option<String> {
        None
    }

    /// The command that undoes `apply`, if one exists.
    fn revert(&self) -> Option<String> {
        None
    }

    /// When the change is felt.
    fn effect(&self) -> Effect {
        Effect::Now
    }

    /// Why the last [`Item::apply`] stood aside without changing anything and without
    /// failing: a lock another run holds, a network the change needs that is down
    /// (atpkg's exits 75 and 69), or a required permission has not been granted (77).
    /// The engine then reports the item `Blocked` with this
    /// line rather than probing it again. `None`, the default, when the apply did its
    /// work or returned an error.
    fn stood_aside(&self) -> Option<StoodAside> {
        None
    }

    /// Why this item stands aside before it acts: the run lacks what its change needs (a
    /// store lock another pass holds, [`standing_aside`]). The engine then neither
    /// journals nor applies it, reports it `Blocked` with this line, and an item waiting
    /// on it stands aside with it. `None`, the default. It is asked before the apply,
    /// unlike [`Item::stood_aside`], so an item reused across runs is never held back by
    /// what its last apply met.
    fn stands_aside(&self) -> Option<StoodAside> {
        None
    }
}
