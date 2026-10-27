// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Conformance test binding `SpareLifecycle` (`aterm_models_gui::spare_lifecycle_model`)
//! to the spare slot transitions, state projection, and negative control (`Buggy = 1`).

use super::*;
use crate::tab_spawn::SpawnAsk;
use aterm_models_gui::spare_lifecycle_model;
use aterm_spec::interp;

const GEN: u64 = 0x5EED;

/// Test rig with one tab in `/tmp` and spare-slot state.
struct Rig {
    app: App,
    id: u64,
    _dir: aterm_tempfile::TempDir,
    fifo_at: PathBuf,
}

impl Rig {
    /// Named `rigged` rather than `new` so the lock-order census does not treat every `new` as acquiring `term`.
    fn rigged() -> Self {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let s = crate::stub_session(app.next_session_id);
        crate::term_lock(&s.term).process(b"\x1b]7;file://localhost/tmp\x07");
        app.push_stub_tab(wid, s);
        app.spares.captured = Some(Vec::new());
        app.spares.power = || false;
        app.config.spare_shell_needs_person = Some(false);
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        Self {
            app,
            id: u64::MAX,
            fifo_at: dir.path().join("fifo"),
            _dir: dir,
        }
    }

    fn link(&self) -> Arc<SpareLink> {
        let slot = self.app.spares.slot.as_ref();
        let transit = self.app.spares.transit.iter().find(|t| t.id == self.id);
        slot.map(|s| Arc::clone(&s.link))
            .or_else(|| transit.map(|t| Arc::clone(&t.link)))
            .expect("a spare")
    }

    /// Project the code's spare state into the model's variables.
    fn project(&self) -> Vec<(&'static str, i64)> {
        let spares = &self.app.spares;
        let in_transit = spares.transit.iter().any(|t| t.id == self.id);
        let state = match spares.slot.as_ref().map(|s| s.state) {
            None if in_transit => 7,
            None => 0,
            Some(SlotState::Booked) => 1,
            Some(SlotState::Spawned) => 2,
            Some(SlotState::Parked) => 3,
            Some(SlotState::Warming) => 4,
            Some(SlotState::Verifying) => 5,
            Some(SlotState::Ready) => 6,
        };
        let in_store = self
            .app
            .store
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .by_local(self.id)
            .is_some();
        assert_eq!(
            in_store,
            self.app.pool.get(self.id).is_some(),
            "a spare is in the store exactly when it is in the pool"
        );
        let registered = in_store;
        let link = (state != 0 || registered).then(|| self.link_any());
        let live = link.as_ref().is_some_and(|l| l.is_live());
        let exited = link.as_ref().is_some_and(|l| l.exited());
        let delivered = link
            .as_ref()
            .map_or(0, |l| l.witness.host_effects.load(Ordering::Acquire));
        let stale = spares.slot.as_ref().is_some_and(|s| {
            let warm_booked = s.state == SlotState::Booked
                && !aterm_core::shell_integration::has_parked_hold(s.inputs.shell);
            (s.state.warm() || warm_booked)
                && spares.shared.generation.load(Ordering::Acquire) != s.generation
        });
        vec![
            ("state", state),
            (
                "warm",
                i64::from(spares.slot.as_ref().is_some_and(|s| {
                    !aterm_core::shell_integration::has_parked_hold(s.inputs.shell)
                })),
            ),
            (
                "spares",
                i64::from(spares.slot.is_some()) + spares.transit.len() as i64,
            ),
            ("registered", i64::from(registered)),
            (
                "eligible",
                i64::from(spares.shared.eligible.load(Ordering::Acquire)),
            ),
            ("exited", i64::from(exited)),
            ("live", i64::from(live)),
            ("delivered", i64::from(delivered)),
            ("stale", i64::from(stale)),
        ]
    }

    /// Complete the latest [`Job::Freshen`] request by storing its generation into `shared.fresh`.
    fn answer_freshen(&mut self) {
        let asked = self.app.spares.captured.as_ref().and_then(|jobs| {
            jobs.iter().rev().find_map(|j| match j {
                Job::Freshen(n) => Some(*n),
                _ => None,
            })
        });
        if let Some(n) = asked {
            self.app.spares.shared.fresh.store(n, Ordering::Release);
        }
    }

    /// Return the current spare's link across slot, transit, or landed state.
    fn link_any(&self) -> Arc<SpareLink> {
        LANDED.with(|landed| {
            landed
                .borrow()
                .clone()
                .filter(|l| l.id == self.id)
                .unwrap_or_else(|| self.link())
        })
    }

    /// Execute `action` on the rig, returning `false` when refused.
    fn act(&mut self, action: &str) -> bool {
        let wid = WindowId(0);
        let now = Instant::now();
        match action {
            "Book" => {
                if self.app.spares.slot.is_some() || !self.app.spares.transit.is_empty() {
                    return false;
                }
                self.id = self.app.next_session_id;
                self.app.next_session_id += 1;
                let mut slot = super::tests::parked_slot_for(&self.app, self.id, Some("/tmp"));
                slot.state = SlotState::Booked;
                let _ = std::fs::remove_file(&self.fifo_at);
                slot.link
                    .witness
                    .hold_fifo(SpareFifo::make_at(self.fifo_at.clone()).expect("fifo"));
                slot.session = None;
                self.app.spares.slot = Some(slot);
                LANDED.with(|l| *l.borrow_mut() = None);
                true
            }
            "BookWarm" => {
                if self.app.spares.slot.is_some() || !self.app.spares.transit.is_empty() {
                    return false;
                }
                if self.app.spare_warm_gates(now, now).is_some() {
                    return false;
                }
                self.id = self.app.next_session_id;
                self.app.next_session_id += 1;
                let mut slot = super::tests::parked_slot_for(&self.app, self.id, Some("/tmp"));
                slot.state = SlotState::Booked;
                slot.inputs.shell = aterm_core::shell_integration::ShellType::Fish;
                slot.generation = self.app.spares.shared.generation.load(Ordering::Acquire);
                let _ = std::fs::remove_file(&self.fifo_at);
                slot.link
                    .witness
                    .hold_fifo(SpareFifo::make_at(self.fifo_at.clone()).expect("fifo"));
                slot.session = None;
                self.app.spares.slot = Some(slot);
                LANDED.with(|l| *l.borrow_mut() = None);
                true
            }
            "Spawned" | "SpawnedWarm" => {
                let warm = action == "SpawnedWarm";
                if !self.app.spares.slot.as_ref().is_some_and(|s| {
                    s.state == SlotState::Booked
                        && warm != aterm_core::shell_integration::has_parked_hold(s.inputs.shell)
                }) {
                    return false;
                }
                let link = self.link();
                let mut session = crate::stub_session(self.id);
                Arc::get_mut(&mut session.ctx).expect("one ctx").witness =
                    Arc::clone(&link.witness);
                wire_spare_taint(
                    &session.term,
                    &link,
                    crate::spawn::HostReach::Platform,
                    false,
                );
                self.app.on_spare_spawned(SpareSpawned {
                    id: self.id,
                    session: Ok(session),
                });
                true
            }
            "ParkMark" => {
                let Some(slot) = self.app.spares.slot.as_ref() else {
                    return false;
                };
                if slot.state != SlotState::Spawned {
                    return false;
                }
                let _ = slot.link.witness.parked_seen.set(now);
                self.app.on_spare_note(self.id, SpareNote::Parked);
                true
            }
            "GateOpens" => {
                let shared = &self.app.spares.shared;
                if shared.eligible.load(Ordering::Acquire) {
                    return false;
                }
                if shared.generation.load(Ordering::Acquire) == 0 {
                    shared.generation.store(GEN, Ordering::Release);
                } else {
                    shared.generation.fetch_add(1, Ordering::AcqRel);
                }
                shared.eligible.store(true, Ordering::Release);
                true
            }
            "GenerationMoves" => {
                let shared = &self.app.spares.shared;
                if !shared.eligible.load(Ordering::Acquire) {
                    return false;
                }
                shared.generation.fetch_add(1, Ordering::AcqRel);
                shared.eligible.store(false, Ordering::Release);
                true
            }
            "Promote" => {
                let before = self.app.spares.slot.as_ref().map(|s| s.state);
                let at = now + Duration::from_secs(3_600);
                self.app.spare_promote(at);
                self.answer_freshen();
                self.app.spare_promote(at);
                before == Some(SlotState::Parked)
                    && self.app.spares.slot.as_ref().map(|s| s.state) == Some(SlotState::Warming)
            }
            "StartMark" => {
                let Some(slot) = self.app.spares.slot.as_ref() else {
                    return false;
                };
                if slot.state != SlotState::Warming {
                    return false;
                }
                let _ = slot
                    .link
                    .witness
                    .start_seen
                    .set(super::tests::a_start_seen_of(StartKind::Spare));
                self.app.on_spare_note(self.id, SpareNote::AtStart);
                true
            }
            "Verified" => {
                let Some(slot) = self.app.spares.slot.as_ref() else {
                    return false;
                };
                if slot.state != SlotState::Verifying {
                    return false;
                }
                let stale =
                    self.app.spares.shared.generation.load(Ordering::Acquire) != slot.generation;
                let verdict = if stale || slot.link.exited() {
                    Verdict::Backoff("stale")
                } else {
                    Verdict::Ok
                };
                self.app
                    .on_spare_note(self.id, SpareNote::Verified(verdict));
                verdict == Verdict::Ok
            }
            "Exits" => {
                let Some(slot) = self.app.spares.slot.as_ref() else {
                    return false;
                };
                if matches!(slot.state, SlotState::Booked) || slot.link.exited() {
                    return false;
                }
                slot.link.decide(Wake::Exit {
                    session: self.id,
                    window: wid,
                });
                true
            }
            "Adopt" => {
                let grid = self.app.spawn_grid(wid);
                match self
                    .app
                    .try_adopt_spare(wid, Some(wid), grid, &SpawnAsk::new(None))
                {
                    Ok((slot, session)) => {
                        self.app.spares.transit.push(Transit {
                            id: slot.id,
                            link: Arc::clone(&slot.link),
                            tier: slot.state,
                            grid: slot.grid,
                            age: Duration::ZERO,
                            rc_ms: None,
                            inputs: slot.inputs.clone(),
                            miss: Arc::default(),
                            retarget: Arc::default(),
                        });
                        PENDING.with(|p| *p.borrow_mut() = Some(session));
                        true
                    }
                    Err(_) => false,
                }
            }
            "Land" => {
                let Some(mut session) = PENDING.with(|p| p.borrow_mut().take()) else {
                    return false;
                };
                let link = self.link();
                self.app.prepare_adopted_spare(&mut session, wid);
                self.app.install_tab_session(wid, session, None);
                self.app.adopt_live(self.id, wid);
                LANDED.with(|l| *l.borrow_mut() = Some(link));
                true
            }
            "Retire" => {
                let retirable = self
                    .app
                    .spares
                    .slot
                    .as_ref()
                    .is_some_and(|s| s.id == self.id);
                if retirable {
                    self.app.retire_spare("conformance", None);
                }
                retirable
            }
            "HostEffect" => {
                let Some(session) = self
                    .app
                    .spares
                    .slot
                    .as_ref()
                    .and_then(|s| s.session.as_ref())
                else {
                    return false;
                };
                crate::term_lock(&session.term).process(b"\x07");
                true
            }
            other => panic!("unbound action {other}"),
        }
    }
}

thread_local! {
    /// Adopted spare session awaiting `Land`.
    static PENDING: std::cell::RefCell<Option<Session>> = const { std::cell::RefCell::new(None) };
    /// Link of the most recently landed spare.
    static LANDED: std::cell::RefCell<Option<Arc<SpareLink>>> = const { std::cell::RefCell::new(None) };
}

/// Drive `trace` on the model and rig in lockstep, verifying projection equality and invariants at each step.
fn lockstep(trace: &[&str]) {
    let model = spare_lifecycle_model();
    let mut state = model.init_state();
    let mut rig = Rig::rigged();
    for (step, action) in trace.iter().enumerate() {
        let mut next = state.clone();
        let fired = model.fire(action, &mut next);
        let done = rig.act(action);
        assert_eq!(
            done,
            fired,
            "step {step} {action}: the code {} where the model {}",
            if done { "acted" } else { "refused" },
            if fired { "acts" } else { "refuses" }
        );
        if fired {
            state = next;
        }
        for (var, value) in rig.project() {
            assert_eq!(
                state[var], value,
                "step {step} {action}: `{var}` is {value} in the code, {} in the model",
                state[var]
            );
        }
        for inv in model.invariants.iter().map(|i| i.name) {
            assert!(model.check_invariant(inv, &state), "{inv} after {action}");
        }
    }
}

#[test]
fn the_spare_lifecycle_conforms_to_its_model() {
    let model = spare_lifecycle_model();
    interp::prove_and_catch(&model);
    let caught = interp::violated_invariants(&interp::with_buggy(&model, 1));
    for inv in [
        "NeverSlotAndRegistered",
        "RunOnlyWhenEligible",
        "AdoptOnlyValid",
        "NoEffectBeforeAdoption",
    ] {
        assert!(caught.contains(inv), "Buggy=1 must break {inv}: {caught:?}");
    }

    lockstep(&["Book", "Spawned", "ParkMark", "Adopt", "Land"]);
    lockstep(&[
        "Book",
        "Spawned",
        "ParkMark",
        "Promote",
        "GateOpens",
        "Promote",
        "StartMark",
        "Verified",
        "Adopt",
        "Land",
    ]);
    lockstep(&[
        "Book",
        "Spawned",
        "ParkMark",
        "GateOpens",
        "Promote",
        "StartMark",
        "Adopt",
        "Verified",
        "Land",
    ]);
    lockstep(&[
        "Book",
        "Spawned",
        "ParkMark",
        "GateOpens",
        "Promote",
        "StartMark",
        "GenerationMoves",
        "Adopt",
        "Retire",
    ]);
    lockstep(&[
        "Book",
        "Spawned",
        "ParkMark",
        "GateOpens",
        "Promote",
        "StartMark",
        "Verified",
        "GenerationMoves",
        "Adopt",
        "Retire",
        "Book",
    ]);
    lockstep(&[
        "Book",
        "Spawned",
        "ParkMark",
        "GateOpens",
        "Promote",
        "StartMark",
        "Verified",
        "GenerationMoves",
        "GateOpens",
        "Adopt",
        "Retire",
    ]);
    lockstep(&["Book", "Spawned", "ParkMark", "Exits", "Adopt", "Retire"]);
    lockstep(&["Book", "Spawned", "ParkMark", "HostEffect", "Retire"]);
    lockstep(&[
        "BookWarm",
        "GateOpens",
        "BookWarm",
        "Spawned",
        "SpawnedWarm",
        "StartMark",
        "Verified",
        "Adopt",
        "Land",
    ]);
    lockstep(&[
        "GateOpens",
        "BookWarm",
        "GenerationMoves",
        "SpawnedWarm",
        "Adopt",
        "Retire",
    ]);
}
