// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 of `TrickFlashLifecycle` (`aterm_spec::derive::trick_flash_lifecycle_model`):
//! the real `WordDecorations` over a real terminal, driven the way the split
//! host drives it, its `trick_flash_phase` checked against the model after
//! every call. A call the model names no step for (a bind between panes, a
//! rescan with nothing to find) must leave the phase where it was.
//!
//! The negative control is the bug the bind's `found_in_a_pane` fixed: closing
//! the bound sibling left the engine unbound, and the next bind's pane-space
//! reset took pane 1's flash with it.

use std::collections::{BTreeMap, BTreeSet};

use aterm_core::terminal::Terminal;
use aterm_effects::word_decorations::{DecoConfig, TrickFlashPhase, WordDecorations};
use aterm_lexicon::Lexicon;
use aterm_spec::derive::{Model, trick_flash_lifecycle_model};
use aterm_spec::{interp, verify, xref};
use aterm_time::Instant;

type State = BTreeMap<&'static str, i64>;

/// The projection `#[refines]` names.
fn project(wd: &WordDecorations, now: Instant) -> i64 {
    match wd.trick_flash_phase(now) {
        TrickFlashPhase::Idle => 0,
        TrickFlashPhase::Pending => 1,
        TrickFlashPhase::Live => 2,
        TrickFlashPhase::Revoked => 3,
    }
}

/// The step `action` takes from `state`, with the phase the engine shows.
fn observed(model: &Model, state: &State, action: &str, phase: i64) -> State {
    let mut next = state.clone();
    assert!(
        model.fire(action, &mut next),
        "{action} is enabled at {state:?}"
    );
    next.insert("phase", phase);
    next
}

/// One engine, one pane's terminal holding `$ sit`, and the model beside them.
struct Rig {
    wd: WordDecorations,
    term: Terminal,
    lex: Lexicon,
    cfg: DecoConfig,
    now: Instant,
    epoch: u64,
    model: Model,
    state: State,
    fired: BTreeSet<&'static str>,
}

impl Rig {
    fn new() -> Self {
        let model = trick_flash_lifecycle_model();
        let mut term = Terminal::new(4, 40);
        term.process(b"$ sit");
        Rig {
            wd: WordDecorations::default(),
            term,
            lex: Lexicon::with_languages(&["en"]),
            cfg: DecoConfig::default(),
            now: Instant::now(),
            epoch: 0,
            state: model.init_state(),
            model,
            fired: BTreeSet::new(),
        }
    }

    fn rescan(&mut self) {
        self.epoch += 1;
        let (term, lex, cfg) = (&self.term, &self.lex, &self.cfg);
        self.wd.rescan(term, 4, 40, lex, cfg, self.epoch, self.now);
    }

    /// `drive` is one real call; `action` the model step it is, if any.
    fn step(&mut self, action: Option<&'static str>, drive: impl FnOnce(&mut Self)) {
        drive(self);
        let phase = project(&self.wd, self.now);
        let Some(action) = action else {
            assert_eq!(phase, self.state["phase"], "no step, no change");
            return;
        };
        let next = observed(&self.model, &self.state, action, phase);
        let (ok, why) = verify::validate_transition_tiered(
            &self.model,
            &[],
            &self.state,
            &next,
            Some(action),
            "trick flash",
        );
        assert!(ok, "{action}: {why}");
        self.state = next;
        self.fired.insert(action);
    }
}

#[test]
fn the_real_flash_refines_the_lifecycle_model() {
    // A split: the key goes to pane 1, then the bound pane 2 closes.
    let mut r = Rig::new();
    r.step(Some("Rebind"), |r| r.wd.bind_pane(1, (0, 0)));
    r.step(None, Rig::rescan);
    r.step(Some("NotePane"), |r| {
        r.wd.note_trick_typed(r.now, Some(1), "sit", 0);
    });
    r.step(Some("Find"), |r| {
        r.term.process(b" ");
        r.rescan();
    });
    r.step(None, |r| r.wd.bind_pane(2, (400, 0)));
    r.step(Some("CloseOther"), |r| r.wd.retain_panes(|pane| pane != 2));
    // The engine before the fix showed Idle after the next bind: refused, and
    // admitted by the mutant.
    let lost = observed(&r.model, &r.state, "Rebind", 0);
    let (ok, why) =
        verify::validate_transition_tiered(&r.model, &[], &r.state, &lost, None, "lost");
    assert!(!ok, "{why}");
    let buggy = interp::with_buggy(&r.model, 1);
    assert!(buggy.successors("Rebind", &r.state).contains(&lost));
    r.step(Some("Rebind"), |r| r.wd.bind_pane(1, (0, 0)));
    assert_eq!(
        r.state["phase"], 2,
        "the sibling's close left pane 1's flash"
    );
    r.step(Some("Revoke"), |r| r.wd.revoke_trick_flash(r.now));
    r.step(Some("Revoke"), |r| r.wd.revoke_trick_flash(r.now));
    r.step(Some("CloseOwn"), |r| r.wd.retain_panes(|pane| pane != 1));
    let mut fired = r.fired;

    // One grid: a revoke before the echo drops the word; the window's own
    // flash goes with the bind that splits the window.
    let mut r = Rig::new();
    r.step(None, Rig::rescan);
    r.step(Some("NoteWindow"), |r| {
        r.wd.note_trick_typed(r.now, None, "sit", 0);
    });
    r.step(Some("Revoke"), |r| r.wd.revoke_trick_flash(r.now));
    r.step(Some("NoteWindow"), |r| {
        r.wd.note_trick_typed(r.now, None, "sit", 0);
    });
    r.step(Some("Find"), |r| {
        r.term.process(b" ");
        r.rescan();
    });
    r.step(Some("Rebind"), |r| r.wd.bind_pane(1, (0, 0)));
    fired.extend(r.fired);

    // The unsplit host names its session: a word noted there and not yet
    // found has a caret in the grid the split changes, and goes with it.
    let mut r = Rig::new();
    r.step(None, |r| {
        r.wd.set_scan_session(Some(1));
        r.rescan();
    });
    r.step(Some("NotePane"), |r| {
        r.wd.note_trick_typed(r.now, Some(1), "sit", 0);
    });
    r.step(Some("Rebind"), |r| r.wd.bind_pane(1, (0, 0)));
    assert_eq!(r.state["phase"], 0, "unfound: retired");

    let model = trick_flash_lifecycle_model();
    let actions: BTreeSet<&str> = model.actions.iter().map(|a| a.name).collect();
    assert_eq!(fired, actions, "the trace drives every action");
    let anchored: BTreeSet<&str> = xref::refinements()
        .filter(|a| a.machine == model.name)
        .map(|a| a.action)
        .collect();
    assert_eq!(anchored, actions, "the anchors name the model's actions");
}
