// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `App` glue for the Overview ([`crate::overview`],
//! `docs/DESIGN-owner-surfaces-2026-10-16.md` §6, was the Connection Map):
//! snapshot every session's words — its published need verdict, else what its
//! agent is doing and for how long, its task, and who drives it — grouped by
//! window, then who manages whom from the task records' `from=`; open the card
//! on the frontmost window WITH raise/focus, or from one of its entrances (the
//! status item, the needs pill's `+N`, a manager's Team chip) on its window,
//! at the session it names, the window raised; keep an OPEN card fresh (the
//! status glance's rebuild, the connection and close funnels, and at most one
//! re-read a minute at paint time, so its coarse ages advance); and settle the
//! acts: ⏎ raises through the SAME body as the `raise` wire verb, `t` is Take
//! Over / Hand Back, `r` opens a done worker's result as a document tab, ⌫
//! stops a worker behind the inline confirm. A control client's key into the
//! card is marked (`App::person_gesture_refused`): its `t` is refused, and its
//! ⌫ on a held tab is refused as the wire `signal` verb is.
//!
//! The words come from the values every other reader reads (the one need
//! publication, the agent publication, the task record, the holds), under the
//! same leaf locks, never a second classification. The state lives in the
//! one modal [`crate::overlay::Overlay`] slot (keys structurally gated before
//! the terminal), every mutator repaints and refreshes the a11y tree, and the
//! pointer follows the palette's claim/hover/arm discipline.

use aterm_session::SessionId;

use crate::App;
use crate::WindowId;
use crate::overview::{
    ChipMark, Driver, Manages, OverviewAct, OverviewChip, OverviewGroup, OverviewState,
};

/// Where an entrance opens the Overview ([`App::open_overview_raised`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OverviewAt {
    /// The card's first line (the status item's Overview…).
    Top,
    /// A session's chip, by local id (the pill's `+N`: the next need).
    Session(u64),
    /// A manager's manages line, by its local id, else its chip (its Team
    /// chip).
    Manager(u64),
}

/// How often an open card's coarse ages are read again at paint time
/// ([`App::overview_prepaint`]): the finest unit [`coarse_age`] prints.
pub(crate) const OVERVIEW_AGE_STEP: std::time::Duration = std::time::Duration::from_secs(60);

/// A coarse age, so a card left open does not repaint every second: `just
/// now`, `4m`, `2h`, `3d`.
pub(crate) fn coarse_age(ms: u64) -> String {
    const MIN: u64 = 60_000;
    const HOUR: u64 = 60 * MIN;
    const DAY: u64 = 24 * HOUR;
    if ms < MIN {
        "just now".to_string()
    } else if ms < HOUR {
        format!("{}m", ms / MIN)
    } else if ms < DAY {
        format!("{}h", ms / HOUR)
    } else {
        format!("{}d", ms / DAY)
    }
}

/// A result's size in words: `812 B`, `2.1 KB`, `1.4 MB`.
pub(crate) fn size_words(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    if bytes < KB {
        format!("{bytes} B")
    } else if bytes < MB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    }
}

/// The short form of a sid a person reads (`s-1e91`).
fn short_sid(sid: &str) -> String {
    aterm_messages::presence::short_sid(sid)
}

impl App {
    /// One session's line: its need, else what it is doing; its task; who
    /// drives it. Leaf locks only, each released before the next: the
    /// timeline's (verdict, agent publication, task record), the fabric's
    /// (holds), the meta's (Autopilot word, inside `autopilot::word_of`).
    fn overview_chip(
        &self,
        h: &crate::session_store::SessionHandle,
        hosting: &crate::harness_reach::HarnessHosting,
    ) -> OverviewChip {
        let ctx = &h.ctx;
        let autopilot = crate::autopilot::word_of(ctx);
        let (verdict, agent_word, agent_age, program, task) = {
            let tl = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            let a = tl.agent();
            (
                tl.need().clone(),
                a.word,
                a.age_ms(),
                crate::harness_host::agent_of(a),
                tl.task().cloned(),
            )
        };
        let person = ctx.fabric.holds().person().is_some();
        let need_word = verdict.need.as_ref().map(|need| need.word.word());
        let mark = match verdict.need.as_ref().map(|need| need.word.severity()) {
            Some(aterm_types::needs::Severity::Stuck) => ChipMark::Stuck,
            Some(aterm_types::needs::Severity::Decide) => ChipMark::Decide,
            None if person => ChipMark::Yours,
            None if task
                .as_ref()
                .is_some_and(|t| t.state == crate::task_record::TaskState::Done) =>
            {
                ChipMark::Done
            }
            None if agent_word == "busy"
                || task
                    .as_ref()
                    .is_some_and(|t| t.state == crate::task_record::TaskState::Working) =>
            {
                ChipMark::Working
            }
            None => ChipMark::Quiet,
        };
        let doing = match (&verdict.need, &verdict.waiting) {
            // A harness's need, whose `what` has no frame (§12.2.3), says it
            // is a need: no header above the line does (§12.2.9).
            (Some(need), _) => need.harness.as_ref().map_or_else(
                || need.what.clone(),
                |ask| ask.cued_line(crate::needs::ROW_WHAT_MAX),
            ),
            // A person's words, as ❯ ▸ Waiting says them, never the wire word.
            (None, Some(wait)) => {
                let words = crate::needs::wait_words(wait, verdict.recheck_unix.is_some());
                match &wait.reset {
                    Some(reset) => format!("\u{29d7} {words} \u{00b7} resets {reset}"),
                    None => format!("\u{29d7} {words}"),
                }
            }
            (None, None) if agent_word == "-" => "shell".to_string(),
            (None, None) => format!("{agent_word} {}", coarse_age(agent_age)),
        };
        let task_words = task.as_ref().map(|t| {
            let age = coarse_age(t.since_ms());
            // A landing says `landed` (§12.4.1): `task landed 4m`, never
            // `task done 4m (landed)`; an older build's done keeps its why.
            let landed = t.state == crate::task_record::TaskState::Done
                && t.why == Some(crate::task_record::WHY_LANDED);
            let mut words = if landed {
                format!("task landed {age}")
            } else {
                format!("task {} {age}", t.state.as_str())
            };
            if let Some(why) = t.why.filter(|_| !landed) {
                words.push_str(&format!(" ({why})"));
            }
            if t.state == crate::task_record::TaskState::Done && t.result_bytes > 0 {
                words.push_str(&format!(" \u{00b7} {}", size_words(t.result_bytes)));
            }
            words
        });
        let worker_of = task
            .as_ref()
            .filter(|t| t.from != crate::task_record::FROM_CTL)
            .map(|t| {
                let local = self
                    .store
                    .read()
                    .unwrap_or_else(|p| p.into_inner())
                    .by_sid(&SessionId::new(&t.from))
                    .map(|h| h.local_id);
                match local {
                    Some(manager) => Driver::WorkerOf(self.tab_label_of_session(manager)),
                    None => Driver::WorkerOfElsewhere(short_sid(&t.from)),
                }
            });
        // Who drives, in the chip's own words (`crate::app_presence::
        // driven_words`: `land, landed`, §12.4.1), and why a landing holds
        // (§12.3.1) on a line of its own under the chip's: the hold's words
        // with no clock, as the menu bar's landing worker's tip says them
        // (`hold_words`' tip form), only where a land act's yield was read.
        // On the chip's own line the hover's long words would push `typed
        // by` past the card's width.
        let (driven, held) = program
            .filter(|agent| hosting.hosts(h.sid.as_str(), autopilot, *agent))
            .map_or((None, None), |_| {
                let glass = self.chip_glass(h.local_id);
                let view = self.session_autopilot(h.local_id).1;
                let face = crate::claude_lights::ChipFace::of(view, &glass);
                let held = self
                    .shown_hold(h.local_id)
                    .filter(|hold| hold.read())
                    .and_then(|hold| {
                        crate::harness_glass::hold_words(
                            &hold,
                            None,
                            crate::harness_glass::HoldForm::Tip,
                            None,
                        )
                    });
                (crate::app_presence::driven_words(face, &glass, None), held)
            });
        let driven = driven.map(Driver::Aterm);
        let is_worker = worker_of.is_some();
        let driver = if person {
            Some(Driver::You)
        } else {
            worker_of.or(driven)
        };
        let live_task = task.as_ref().is_some_and(|t| {
            matches!(
                t.state,
                crate::task_record::TaskState::Working | crate::task_record::TaskState::Stalled
            )
        });
        let has_result = task.as_ref().is_some_and(|t| {
            t.state == crate::task_record::TaskState::Done
                && t.result_bytes > 0
                && t.result_path.is_some()
        });
        OverviewChip {
            sid: h.sid.clone(),
            local_id: h.local_id,
            label: self.tab_label_of_session(h.local_id),
            mark,
            need: need_word,
            doing,
            task: task.as_ref().map(|t| t.state.as_str()),
            task_words,
            driver,
            held,
            typed: self.typed_by(ctx).map(|typed| {
                let ago = aterm_time::unix_millis()
                    .unwrap_or(0)
                    .saturating_sub(typed.at_unix_ms);
                let age = coarse_age(ago);
                // `just now`, else `4m ago`.
                let when = if ago < 60_000 {
                    age
                } else {
                    format!("{age} ago")
                };
                (typed.by.words(), when)
            }),
            worker_of: is_worker,
            held_by_person: person,
            has_result,
            stoppable: live_task,
        }
    }

    /// Snapshot the card: sessions grouped by window (tab order, pane leaves
    /// included), a trailing `elsewhere` group for registered sessions no
    /// window shows, and one manages line per manager a task's `from=` names
    /// (a manager not on this aterm is named by its sid, `(not here)`).
    /// Registry snapshot first; each session's leaf locks inside
    /// [`Self::overview_chip`], no store lock held across them. Exited
    /// sessions are not listed.
    fn overview_parts(&self) -> (Vec<OverviewGroup>, Vec<Manages>) {
        let hosting = self.harness_hosting();
        let handles: Vec<crate::session_store::SessionHandle> = {
            let g = self.store.read().unwrap_or_else(|p| p.into_inner());
            g.snapshot()
                .into_iter()
                .filter(|h| !matches!(h.state, crate::session_store::SessionState::Exited))
                .collect()
        };
        let by_local: std::collections::HashMap<u64, &crate::session_store::SessionHandle> =
            handles.iter().map(|h| (h.local_id, h)).collect();
        let mut wids: Vec<WindowId> = self.windows.keys().copied().collect();
        wids.sort();
        let mut placed: std::collections::HashSet<u64> = std::collections::HashSet::new();
        let mut groups: Vec<OverviewGroup> = Vec::new();
        for (ordinal, wid) in wids.iter().enumerate() {
            let Some(ws) = self.windows.get(wid) else {
                continue;
            };
            let mut chips: Vec<OverviewChip> = Vec::new();
            for tab in ws.tab_set.tabs() {
                for view in tab.root.leaves() {
                    let Some(local) = self
                        .view_store
                        .get(view)
                        .copied()
                        .and_then(crate::tab_model::View::terminal_session)
                    else {
                        continue;
                    };
                    let Some(h) = by_local.get(&local) else {
                        continue; // an unregistered stub labels no chip
                    };
                    if placed.insert(local) {
                        chips.push(self.overview_chip(h, &hosting));
                    }
                }
            }
            if chips.is_empty() {
                continue; // an all-native window has no session lines to head
            }
            let active_title = ws
                .tab_set
                .active_index()
                .and_then(|i| ws.tab_set.tabs().get(i))
                .map(|t| {
                    crate::session_timeline::sanitize_presentation_line(&t.presentation.title, 40)
                })
                .filter(|t| !t.is_empty());
            let label = match active_title {
                Some(t) => format!("Window {} \u{2014} {t}", ordinal + 1),
                None => format!("Window {}", ordinal + 1),
            };
            groups.push(OverviewGroup { label, chips });
        }
        let elsewhere: Vec<OverviewChip> = handles
            .iter()
            .filter(|h| !placed.contains(&h.local_id))
            .map(|h| self.overview_chip(h, &hosting))
            .collect();
        if !elsewhere.is_empty() {
            groups.push(OverviewGroup {
                label: "elsewhere".to_string(),
                chips: elsewhere,
            });
        }
        // Who manages whom: every task record a manager started, by manager,
        // in session order.
        let mut manages: Vec<Manages> = Vec::new();
        for h in &handles {
            let from = h
                .ctx
                .timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .task()
                .map(|t| t.from.clone());
            let Some(from) = from.filter(|from| from != crate::task_record::FROM_CTL) else {
                continue;
            };
            let worker = (h.sid.clone(), self.tab_label_of_session(h.local_id));
            if let Some(line) = manages.iter_mut().find(|m| m.manager.as_str() == from) {
                line.workers.push(worker);
                continue;
            }
            let manager = SessionId::new(&from);
            let manager_local = handles
                .iter()
                .find(|other| other.sid == manager)
                .map(|other| other.local_id);
            let manager_label = match manager_local {
                Some(local) => self.tab_label_of_session(local),
                None => format!("{} (not here)", short_sid(&from)),
            };
            manages.push(Manages {
                manager,
                manager_local,
                manager_label,
                workers: vec![worker],
            });
        }
        (groups, manages)
    }

    /// Open the Overview (host+raise): on the FRONTMOST window, which comes
    /// forward with it. `Err` (nothing opens) with no window resolvable, so the
    /// wire caller reports honestly; the menu path logs and moves on.
    pub(crate) fn open_overview(&mut self) -> Result<(), String> {
        let Some(wid) = self.frontmost_window else {
            return Err("no front window to open the overview on".to_string());
        };
        self.open_overview_on(wid)
    }

    /// The tab menu's Stop Worker… where no sheet can ask the person
    /// (headless, off macOS): the Overview opens on `wid` with `session`'s
    /// chip selected and its inline confirm armed for the person's hand, so
    /// their second ⌫ stops it and anything else keeps it. Nothing is
    /// stopped here. A control client's choice never comes here: it is the
    /// `signal` verb's act, with no card (`App::dispatch_tab_menu_connection`).
    pub(crate) fn overview_arm_stop(&mut self, wid: WindowId, session: u64) {
        let controller = self.person_gesture_refused();
        if let Err(e) = self.open_overview_on(wid) {
            aterm_log::info!("stop worker {session}: no overview to confirm on: {e}");
            return;
        }
        let armed = self
            .windows
            .get_mut(&wid)
            .and_then(|ws| ws.overview_mut())
            .is_some_and(|card| card.arm_stop_on(session, controller));
        if !armed {
            aterm_log::info!("stop worker {session}: the overview lists no worker to stop");
        }
        self.repaint_overview_pointer(wid, true);
    }

    /// THE OVERVIEW'S OWN ENTRANCES (§3, §5): the status item's Overview… on
    /// the frontmost window, the needs pill's `+N` on its window at the
    /// window's next need, and a manager's Team chip on the window clicked
    /// at that manager. Each opens the card on `wid` with its cursor `at`
    /// (where the card lists it), then RAISES the window
    /// ([`Self::raise_window`]: de-miniaturize, then focus): a status item
    /// click never activates aterm, and the pill and the chip may sit in a
    /// window behind. The Agents menu's row and the wire's `open overview`
    /// keep [`Self::open_overview`], whose card comes forward without one.
    pub(crate) fn open_overview_raised(
        &mut self,
        wid: WindowId,
        at: OverviewAt,
    ) -> Result<(), String> {
        self.open_overview_on(wid)?;
        let (session, as_manager) = match at {
            OverviewAt::Top => (None, false),
            OverviewAt::Session(session) => (Some(session), false),
            OverviewAt::Manager(session) => (Some(session), true),
        };
        if let Some(session) = session
            && let Some(card) = self.windows.get_mut(&wid).and_then(|ws| ws.overview_mut())
            && card.select_session(session, as_manager)
        {
            self.repaint_overview_pointer(wid, true);
        }
        self.raise_window(wid);
        Ok(())
    }

    /// [`Self::open_overview`] on `wid`, which comes forward with it. Where
    /// this aterm's fabric is configured, the card lists what the last read
    /// of the fabric's log said ("On the fabric") and kicks the
    /// next read ([`Self::kick_fabric_read`]), whose landing re-renders it.
    fn open_overview_on(&mut self, wid: WindowId) -> Result<(), String> {
        let (groups, manages) = self.overview_parts();
        self.agents.overview_read_at = Some(std::time::Instant::now());
        let mut state = OverviewState::new(wid, groups, manages);
        let _ = state.set_elsewhere(self.sessions_elsewhere());
        let _ = state.set_fabric(self.overview_fabric_group());
        if self.install_overlay_card(
            wid,
            crate::overlay::Overlay::Overview(state),
            true,
            Self::overview_claims_pointer,
            Self::overview_pointer_motion,
        ) {
            self.kick_fabric_read();
            Ok(())
        } else {
            Err("the front window is gone".to_string())
        }
    }

    /// Close the card on `wid` (no-op unless it is the open variant).
    pub(crate) fn overview_exit(&mut self, wid: WindowId) {
        self.close_overlay_card(wid, |ws| ws.overview().is_some());
    }

    /// Re-snapshot every OPEN card, selection and confirm surviving by
    /// identity, and repaint those whose words moved: the status glance's
    /// rebuild calls it on the way to the wait (the same changes the menu bar
    /// shows), and so do the connection and close funnels. Nothing open costs
    /// one scan of the windows.
    pub(crate) fn overview_refresh_all(&mut self) {
        let open: Vec<WindowId> = self
            .windows
            .iter()
            .filter(|(_, ws)| ws.overview().is_some())
            .map(|(wid, _)| *wid)
            .collect();
        if open.is_empty() {
            return;
        }
        let (groups, manages) = self.overview_parts();
        let elsewhere = self.sessions_elsewhere();
        let fabric = self.overview_fabric_group();
        self.agents.overview_read_at = Some(std::time::Instant::now());
        let mut moved = false;
        for wid in open {
            if let Some(card) = self.windows.get_mut(&wid).and_then(|ws| ws.overview_mut())
                && (card.retarget(groups.clone(), manages.clone())
                    | card.set_elsewhere(elsewhere)
                    | card.set_fabric(fabric.clone()))
            {
                moved = true;
                if let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
                    w.request_redraw();
                }
            }
        }
        if moved {
            self.overlay_a11y_update();
        }
    }

    /// Re-read the words into an open card on `wid` from the redraw path, at
    /// most once a [`OVERVIEW_AGE_STEP`]: its ages are coarse (minutes), and
    /// they move with no wake, so a card open on a quiet aterm still advances
    /// them at its next paint past the step. Every other frame costs one
    /// clock compare; what moves with a wake re-snapshots where the glance is
    /// rebuilt ([`Self::overview_refresh_all`]).
    pub(crate) fn overview_prepaint(&mut self, wid: WindowId) {
        if self
            .windows
            .get(&wid)
            .is_none_or(|ws| ws.overview().is_none())
        {
            return;
        }
        let now = std::time::Instant::now();
        if self
            .agents
            .overview_read_at
            .is_some_and(|at| now.saturating_duration_since(at) < OVERVIEW_AGE_STEP)
        {
            return;
        }
        self.agents.overview_read_at = Some(now);
        let (groups, manages) = self.overview_parts();
        let elsewhere = self.sessions_elsewhere();
        let fabric = self.overview_fabric_group();
        if let Some(card) = self.windows.get_mut(&wid).and_then(|ws| ws.overview_mut()) {
            let _ = card.retarget(groups, manages);
            let _ = card.set_elsewhere(elsewhere);
            let _ = card.set_fabric(fabric);
        }
    }

    /// How many sessions the other aterms on this Mac hold, as the last
    /// sibling scan counted them: the figure ❯'s This Mac line names
    /// (`status_item::Here::elsewhere`), so the Overview's title says the same.
    fn sessions_elsewhere(&self) -> usize {
        self.siblings.iter().map(|s| s.sessions).sum()
    }

    /// Move the cursor by `delta` over the items.
    pub(crate) fn overview_move(&mut self, wid: WindowId, delta: isize) {
        if let Some(card) = self.windows.get_mut(&wid).and_then(|ws| ws.overview_mut()) {
            card.move_selection(delta);
        }
        self.repaint_overview_pointer(wid, true);
    }

    /// esc: cancel an armed confirm first, close second.
    pub(crate) fn overview_escape(&mut self, wid: WindowId) {
        let close = self
            .windows
            .get_mut(&wid)
            .and_then(|ws| ws.overview_mut())
            .is_some_and(OverviewState::escape);
        if close {
            self.overview_exit(wid);
        } else {
            self.repaint_overview_pointer(wid, true);
        }
    }

    /// Run one resolved act ([`OverviewAct`]).
    fn overview_run(&mut self, wid: WindowId, act: OverviewAct) {
        match act {
            OverviewAct::Raise(local) => {
                self.overview_exit(wid);
                if let Err(e) = self.raise_session_by_id(local) {
                    aterm_log::info!("overview raise {local}: {e}");
                }
            }
            OverviewAct::TakeOverOrHandBack(local) => {
                // Hand Back where it has a hold to lift: the person's, or an
                // Owner client's where no person's stands.
                let held = self
                    .pool
                    .get(local)
                    .is_some_and(|s| s.ctx.fabric.holds().hand_back_origin().is_some());
                let done = if held {
                    self.hand_back_session(local)
                } else {
                    self.take_over_session(local)
                };
                if let Err(e) = done {
                    aterm_log::info!("overview take over {local}: {e}");
                }
                self.overview_refresh_all();
                self.repaint_overview_pointer(wid, true);
            }
            OverviewAct::ReadResult(local) => {
                if let Err(e) = self.read_result(wid, local) {
                    aterm_log::info!("overview read result {local}: {e}");
                }
            }
            OverviewAct::StopWorker(local) => {
                if let Err(why) = self.stop_worker(local) {
                    self.stop_worker_refused(local, why);
                }
                self.overview_refresh_all();
                self.repaint_overview_pointer(wid, true);
            }
            // A CONTROL CLIENT'S ⌫ ON A HELD TAB ARMS NOTHING: the stop it
            // would confirm is refused (the wire `signal` verb's halt), so no
            // confirm is left standing for any key to resolve.
            OverviewAct::Armed(local) => {
                if let Some(why) = self
                    .stop_worker_refusal(local)
                    .filter(|why| matches!(why, StopRefused::Halted(_)))
                {
                    if let Some(card) = self.windows.get_mut(&wid).and_then(|ws| ws.overview_mut())
                    {
                        card.cancel_confirm();
                    }
                    self.stop_worker_refused(local, why);
                }
                self.repaint_overview_pointer(wid, true);
            }
            OverviewAct::None => {}
        }
    }

    /// ⏎ / click: go there (a chip's session, a manages line's manager); an
    /// armed Stop Worker confirm is dropped, never resolved.
    pub(crate) fn overview_activate(&mut self, wid: WindowId) {
        let act = self
            .windows
            .get_mut(&wid)
            .and_then(|ws| ws.overview_mut())
            .map(OverviewState::activate);
        if let Some(act) = act {
            self.overview_run(wid, act);
        }
    }

    /// One key's act on the open card: `t`, `r` or ⌫. A ⌫ carries whose
    /// hand pressed it ([`Self::person_gesture_refused`]), so a confirm one
    /// hand armed is resolved by no other's.
    fn overview_key(&mut self, wid: WindowId, key: OverviewKey) {
        let controller = self.person_gesture_refused();
        let act = self
            .windows
            .get_mut(&wid)
            .and_then(|ws| ws.overview_mut())
            .map(|card| match key {
                OverviewKey::TakeOver => card.take_over_pressed(),
                OverviewKey::ReadResult => card.read_result_pressed(),
                OverviewKey::Stop => card.stop_pressed(controller),
            });
        if let Some(act) = act {
            self.overview_run(wid, act);
        }
    }

    /// Why Stop Worker would stop nothing on `session` right now, before any
    /// signal: no such session, a task that is not working or stalled (the
    /// card or the menu was drawn a moment ago), a CONTROL CLIENT'S gesture
    /// on a held tab ([`Self::person_gesture_refused`]): a control client's
    /// ⌫ into the Overview is the wire `signal` verb's act, so the same halt
    /// refuses it, word for word, and a refusal under the person's hold is
    /// counted toward it as the verb's is — or NO AGENT IN FRONT
    /// ([`StopRefused::NoAgent`]): the tab's foreground program is its
    /// shell (a worker that never started its agent, one whose agent
    /// returned to its prompt), which ignores the SIGTERM `killpg` would
    /// still report sent, so the stop would say done and end nothing.
    pub(crate) fn stop_worker_refusal(&self, session: u64) -> Option<StopRefused> {
        let Some(ctx) = self.pool.get(session).map(|s| s.ctx.clone()) else {
            return Some(StopRefused::Gone);
        };
        let (live, shell_in_front) = {
            let tl = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            let live = tl.task().is_some_and(|t| {
                matches!(
                    t.state,
                    crate::task_record::TaskState::Working | crate::task_record::TaskState::Stalled
                )
            });
            (live, shell_in_front(tl.agent()))
        };
        if !live {
            return Some(StopRefused::NotRunning);
        }
        if self.person_gesture_refused()
            && let Some(refusal) = crate::fabric::halt_refusal(&ctx, "signal")
        {
            return Some(StopRefused::Halted(refusal.trim_end().to_string()));
        }
        if shell_in_front {
            return Some(StopRefused::NoAgent);
        }
        None
    }

    /// SAY WHY STOP WORKER STOPPED NOTHING, to whom it concerns
    /// (`docs/DESIGN-owner-surfaces-2026-10-16.md` §1's alert rule):
    ///
    /// * A CONTROL CLIENT'S gesture ([`Self::person_gesture_refused`]): the
    ///   person did nothing and aterm settled it (a held tab's refusal is
    ///   already counted toward the hold, as the wire `signal` verb's is), so
    ///   nothing reaches the person's glass: one log line.
    /// * The person asked for what already is — the task ended, the tab is
    ///   gone — or a hold refused it: a RECORD (R10's rule), no step.
    /// * No agent runs in the tab (its shell is in front), or the `signal`
    ///   verb refused the person's own stop (no foreground program, a
    ///   platform with no signals): their gesture failed, said for the
    ///   gesture's short hold, with the act that still ends it (close its
    ///   tab).
    pub(crate) fn stop_worker_refused(&mut self, session: u64, why: StopRefused) {
        #[cfg(test)]
        {
            self.agents.last_stop_refused = Some(why.clone());
        }
        if self.person_gesture_refused() {
            aterm_log::info!(
                "stop worker {session}: a control client's gesture, refused: {}",
                why.words()
            );
            return;
        }
        let label = self.tab_label_of_session(session);
        let msg = match &why {
            StopRefused::Gone | StopRefused::NotRunning | StopRefused::Halted(_) => {
                crate::message_reporters::stop_worker_nothing(&label, &why.words())
            }
            StopRefused::NoAgent | StopRefused::Signal(_) => {
                crate::message_reporters::stop_worker_failed(&label, &why.words())
            }
        };
        self.post_message(msg);
    }

    /// STOP WORKER: SIGTERM to `session`'s agent, in process, through the
    /// `signal` verb's own body (it drops the input the program left unread
    /// first); the task reads `exited` when the agent ends. The person's own
    /// act at the window — the Overview's confirmed ⌫, the tab menu's sheet —
    /// is refused by no hold: Take Over stops the robots, not the person. A
    /// control client's gesture into the same card is refused where the wire
    /// `signal` verb would be ([`Self::stop_worker_refusal`], re-read here
    /// when it acts). `Err` says why nothing was signalled.
    pub(crate) fn stop_worker(&mut self, session: u64) -> Result<String, StopRefused> {
        let refused = self.stop_worker_refusal(session);
        #[cfg(test)]
        {
            self.agents.last_stop_refused = refused.clone();
        }
        if let Some(refused) = refused {
            return Err(refused);
        }
        let (master, ctx) = {
            let g = self.store.read().unwrap_or_else(|p| p.into_inner());
            let h = g.by_local(session).ok_or(StopRefused::Gone)?;
            (h.master, h.ctx.clone())
        };
        #[cfg(test)]
        {
            self.agents.stop_signals += 1;
        }
        let reply = crate::control::signal_verb(master, "term", &ctx);
        let reply = reply.trim_end().to_string();
        match reply.strip_prefix("OK ") {
            Some(ok) => Ok(ok.to_string()),
            None => Err(StopRefused::Signal(
                reply.strip_prefix("ERR ").unwrap_or(&reply).to_string(),
            )),
        }
    }

    /// READ RESULT: a done worker's result file as a Markdown document tab on
    /// `wid` (the admission every Open Markdown… takes). `Err` when the task
    /// has no result to read.
    pub(crate) fn read_result(&mut self, wid: WindowId, session: u64) -> Result<(), String> {
        let path = self
            .pool
            .get(session)
            .and_then(|s| {
                s.ctx
                    .timeline
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .task()
                    .filter(|t| t.result_bytes > 0)
                    .and_then(|t| t.result_path.clone())
            })
            .ok_or_else(|| "no result to read".to_string())?;
        self.overview_exit(wid);
        let _ = self.open_menu_document(wid, path);
        Ok(())
    }

    /// While the card is open on `wid`, drive it from the keyboard and
    /// SWALLOW every key (the modal-overlay gate contract).
    pub(crate) fn on_key_overview_mode(
        &mut self,
        wid: WindowId,
        ev: &aterm_window::event::KeyEvent,
    ) -> bool {
        use aterm_window::keyboard::{Key, NamedKey};
        if self
            .windows
            .get(&wid)
            .and_then(|ws| ws.overview())
            .is_none()
        {
            return false;
        }
        match &ev.logical_key {
            Key::Named(NamedKey::Escape) => self.overview_escape(wid),
            Key::Named(NamedKey::ArrowUp) => self.overview_move(wid, -1),
            Key::Named(NamedKey::ArrowDown) => self.overview_move(wid, 1),
            Key::Named(NamedKey::Enter) => self.overview_activate(wid),
            Key::Named(NamedKey::Delete | NamedKey::Backspace) => {
                self.overview_key(wid, OverviewKey::Stop);
            }
            Key::Character(c) if c.eq_ignore_ascii_case("t") => {
                self.overview_key(wid, OverviewKey::TakeOver);
            }
            Key::Character(c) if c.eq_ignore_ascii_case("r") => {
                self.overview_key(wid, OverviewKey::ReadResult);
            }
            _ => {}
        }
        true
    }

    /// The ENGINE-NEUTRAL twin of [`Self::on_key_overview_mode`] — reached by
    /// controller `key`/`text` verbs. The caller still swallows the event from
    /// the PTY, and marks it a controller's gesture: Take Over and Hand Back
    /// refuse under it (`App::person_gesture_refused`).
    pub(crate) fn overview_input_event(&mut self, wid: WindowId, ev: &crate::input::InputEvent) {
        use crate::input::InputEvent;
        use aterm_types::keyboard::{Key as TKey, KeyEventType, NamedKey as TNamed};
        if self
            .windows
            .get(&wid)
            .and_then(|ws| ws.overview())
            .is_none()
        {
            return;
        }
        let folded = ev.keypad_folded();
        let ev = folded.as_ref().unwrap_or(ev);
        match ev {
            InputEvent::Key {
                key, event_type, ..
            } => {
                if matches!(event_type, KeyEventType::Release) {
                    return;
                }
                match key {
                    TKey::Named(TNamed::Escape) => self.overview_escape(wid),
                    TKey::Named(TNamed::ArrowUp) => self.overview_move(wid, -1),
                    TKey::Named(TNamed::ArrowDown) => self.overview_move(wid, 1),
                    TKey::Named(TNamed::Enter) => self.overview_activate(wid),
                    TKey::Named(TNamed::Delete | TNamed::Backspace) => {
                        self.overview_key(wid, OverviewKey::Stop);
                    }
                    TKey::Character(c) if c.eq_ignore_ascii_case(&'t') => {
                        self.overview_key(wid, OverviewKey::TakeOver);
                    }
                    TKey::Character(c) if c.eq_ignore_ascii_case(&'r') => {
                        self.overview_key(wid, OverviewKey::ReadResult);
                    }
                    _ => {}
                }
            }
            InputEvent::Text(t) => {
                for c in t.chars() {
                    match c.to_ascii_lowercase() {
                        't' => self.overview_key(wid, OverviewKey::TakeOver),
                        'r' => self.overview_key(wid, OverviewKey::ReadResult),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    // ---- Pointer boundary (the palette_claims_pointer discipline) ----------

    /// Modal pointer boundary: whether the open card owns the gesture on
    /// `wid`.
    pub(crate) fn overview_claims_pointer(&mut self, wid: WindowId) -> bool {
        self.overlay_card_claims_pointer(
            wid,
            |ws| ws.overview().is_some(),
            Self::sync_overview_pointer_cursor,
        )
    }

    /// The item index under a window-space point, through the same zoom-aware
    /// transform the card was composited with.
    fn overview_item_at_pointer(&self, wid: WindowId, x: f64, y: f64) -> Option<usize> {
        let card = self.windows.get(&wid)?.overview()?;
        let (transform, x, y) = self.overlay_card_point(wid, x, y)?;
        crate::overview::overview_item_hit(card, &transform.geom, x, y)
    }

    /// Keep the OS cursor aligned with the card's item hover.
    pub(crate) fn sync_overview_pointer_cursor(&mut self, wid: WindowId) {
        let pointer = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.overview())
            .is_some_and(OverviewState::pointer_over_item);
        self.set_overlay_card_cursor(wid, pointer);
    }

    fn repaint_overview_pointer(&mut self, wid: WindowId, changed: bool) {
        self.sync_overview_pointer_cursor(wid);
        self.redraw_overlay_card(wid, changed);
    }

    /// Hover-select the item under a pointer motion.
    pub(crate) fn overview_pointer_motion(&mut self, wid: WindowId, x: f64, y: f64) {
        let hit = self.overview_item_at_pointer(wid, x, y);
        let changed = self
            .windows
            .get_mut(&wid)
            .and_then(|ws| ws.overview_mut())
            .is_some_and(|card| card.pointer_hover(hit));
        self.repaint_overview_pointer(wid, changed);
    }

    pub(crate) fn overview_pointer_press(&mut self, wid: WindowId, x: f64, y: f64) {
        let hit = self.overview_item_at_pointer(wid, x, y);
        let changed = self
            .windows
            .get_mut(&wid)
            .and_then(|ws| ws.overview_mut())
            .is_some_and(|card| card.pointer_press(hit));
        self.repaint_overview_pointer(wid, changed);
    }

    /// Settle a left release: same-item press+release activates through the
    /// SAME seam as Enter.
    pub(crate) fn overview_pointer_release(&mut self, wid: WindowId, x: f64, y: f64) {
        let hit = self.overview_item_at_pointer(wid, x, y);
        let (changed, activate) = self
            .windows
            .get_mut(&wid)
            .and_then(|ws| ws.overview_mut())
            .map_or((false, false), |card| card.pointer_release(hit));
        self.repaint_overview_pointer(wid, changed);
        if activate {
            self.overview_activate(wid);
        }
    }

    /// Scroll the band (wheel) — the cursor stays put.
    pub(crate) fn overview_pointer_wheel(&mut self, wid: WindowId, delta: isize) {
        let changed = self
            .windows
            .get_mut(&wid)
            .and_then(|ws| ws.overview_mut())
            .is_some_and(|card| card.scroll_by(delta));
        self.repaint_overview_pointer(wid, changed);
    }
}

/// The card's letter keys and ⌫.
#[derive(Clone, Copy)]
enum OverviewKey {
    TakeOver,
    ReadResult,
    Stop,
}

/// Why Stop Worker signalled nothing ([`App::stop_worker`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum StopRefused {
    /// The session is gone.
    Gone,
    /// Its task is not working or stalled: it ended since the row was drawn,
    /// or never ran.
    NotRunning,
    /// A control client's gesture on a held tab: the `signal` verb's own
    /// `ERR halted …`, word for word.
    Halted(String),
    /// No agent runs in the tab: its foreground program is its shell, which
    /// a SIGTERM would not end ([`shell_in_front`]). Nothing is signalled.
    NoAgent,
    /// The `signal` verb's own refusal (no foreground program, a platform
    /// with no signals).
    Signal(String),
}

/// Whether `publication` says the tab's foreground program is its shell and
/// no agent: the program is a shell by name
/// ([`crate::quit_safety::is_shell_name`], the survey's `shell` word) and no
/// agent is identified in front ([`crate::harness_host::agent_of`]). An
/// unresolved program is not a shell: the stop is sent as before, since
/// nothing says it would end nothing.
pub(crate) fn shell_in_front(publication: &crate::session_timeline::AgentPublication) -> bool {
    crate::harness_host::agent_of(publication).is_none()
        && publication
            .program
            .as_deref()
            .is_some_and(crate::quit_safety::is_shell_name)
}

impl StopRefused {
    /// The refusal in words: the record's line, the log's, or (for
    /// [`Self::Signal`]) the failed gesture's detail.
    pub(crate) fn words(&self) -> String {
        match self {
            Self::Gone => "the tab is gone".to_string(),
            Self::NotRunning => "its task is not running: nothing was stopped".to_string(),
            Self::NoAgent => "no agent runs in it: only its shell is in front".to_string(),
            Self::Halted(refusal) => {
                format!("the tab is held, so a control client cannot stop it ({refusal})")
            }
            Self::Signal(why) => why.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::App;
    use crate::WindowId;
    use crate::app_introspect::AuxTarget;
    use crate::input::InputEvent;
    use crate::overlay::OverlayKind;

    fn key(named: aterm_types::keyboard::NamedKey) -> InputEvent {
        InputEvent::Key {
            key: aterm_types::keyboard::Key::Named(named),
            mods: aterm_types::keyboard::Modifiers::empty(),
            base_layout: None,
            layout_text: None,
            event_type: aterm_types::keyboard::KeyEventType::Press,
        }
    }

    /// Three registered stub sessions in one window; returns their sids in
    /// registration order (tab 0 is the harness session).
    fn app_with_three() -> (App, WindowId, Vec<aterm_session::SessionId>) {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.tab_strip_rows = 1;
        app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
        app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
        app.splice_tab_strip(wid);
        let sids = {
            let g = app.store.read().unwrap();
            let mut handles = g.snapshot();
            handles.sort_by_key(|h| h.local_id);
            handles.into_iter().map(|h| h.sid).collect()
        };
        (app, wid, sids)
    }

    fn ctx_of(app: &App, sid: &aterm_session::SessionId) -> std::sync::Arc<crate::SessionCtx> {
        app.store.read().unwrap().by_sid(sid).unwrap().ctx.clone()
    }

    fn local_of(app: &App, sid: &aterm_session::SessionId) -> u64 {
        app.store.read().unwrap().by_sid(sid).unwrap().local_id
    }

    /// Mark `worker`'s task working, started by `from` (a sid, or `ctl`).
    fn give_task(app: &App, worker: &aterm_session::SessionId, from: &str) {
        use crate::task_record::{TaskMark, TaskState, mark_session_task};
        let ctx = ctx_of(app, worker);
        let mark = TaskMark {
            from: Some(from.to_string()),
            ..TaskMark::new(TaskState::Working)
        };
        mark_session_task(&ctx, local_of(app, worker), &mark).expect("unfenced");
    }

    /// The open card's item ordinal of the chip for `sid`.
    fn chip_ordinal(app: &App, wid: WindowId, sid: &aterm_session::SessionId) -> usize {
        app.windows[&wid]
            .overview()
            .expect("open")
            .controls_lines()
            .iter()
            .filter(|l| l.contains(" chip ") || l.contains(" manages "))
            .position(|l| l.contains(&format!("sid={} ", sid.as_str())))
            .expect("the session is listed")
    }

    fn chip_line(app: &App, sid: &aterm_session::SessionId) -> String {
        app.read_aux_controls(AuxTarget::Overview)
            .into_iter()
            .find(|l| l.contains(" chip ") && l.contains(&format!("sid={} ", sid.as_str())))
            .expect("the chip")
    }

    /// WHO TYPED (S9), named on the main thread where the person and a
    /// poller look: a client types into a worker over an edge minted for
    /// another tab, and the worker's `status`, its hover's last line and its
    /// Overview line name the client on that tab's edge, the tab by its role.
    /// CONTROL: before the stamp none says it.
    #[test]
    fn status_the_hover_and_the_card_name_who_typed() {
        use crate::typed_by::{Caller, Who};
        let (mut app, _wid, sids) = app_with_three();
        let (manager, worker) = (&sids[1], &sids[2]);
        ctx_of(&app, manager).meta.lock().unwrap().role = Some("lab-sweep".into());
        let local = local_of(&app, worker);
        let status = |app: &App| app.session_status_record(local).expect("live");
        assert!(status(&app).contains(" typed_by=- typed_ms=- "));
        assert!(app.presence_hover(local, None).is_none());

        let by = Caller::new(Who::Edge {
            src: manager.clone(),
            client: Box::new(Who::Word("a script".into())),
        });
        crate::typed_by::stamp(&ctx_of(&app, worker), by);
        app.on_typed(worker);
        assert!(
            status(&app).contains(" typed_by=a%20script%20on%20lab-sweep"),
            "{}",
            status(&app)
        );
        let hover = app.presence_hover(local, None).expect("the typist is said");
        assert!(
            hover.starts_with("last typed into by a script on lab-sweep's edge"),
            "{hover}"
        );
        assert!(app.open_overview().is_ok());
        let line = chip_line(&app, worker);
        assert!(
            line.contains(" typed_by=a%20script%20on%20lab-sweep"),
            "{line}"
        );
        assert!(chip_line(&app, manager).contains(" typed_by=- "));
    }

    /// A TAB NAMES WHO TYPED FROM IT BY WHAT SOMEONE SET ON IT (S9): its
    /// role, else the person's name for it, else its session id; the
    /// survey's facts get the role or the id, never the person's name.
    /// NEGATIVE CONTROL: a title the program sets, and a role that is a word
    /// aterm credits by, name nothing: the tab reads as its id.
    #[test]
    fn a_typist_tab_is_named_by_its_role_its_pin_or_its_id() {
        let (app, _wid, sids) = app_with_three();
        let tab = &sids[1];
        let local = local_of(&app, tab);
        let id = aterm_messages::presence::short_sid(tab.as_str());
        let named = |app: &App| {
            let named = app.typist_tab(local).expect("pooled");
            (named.words, named.facts)
        };
        let term = app.pool.get(local).expect("pooled").term.clone();
        crate::term_lock(&term).process(b"\x1b]2;aterm\x07");
        assert_eq!(app.tab_label_of_session(local), "aterm", "its label");
        assert_eq!(
            named(&app),
            (id.clone(), Some(id.clone())),
            "never its title"
        );
        let ctx = ctx_of(&app, tab);
        let pinned = ctx
            .meta
            .lock()
            .unwrap()
            .set("title", Some("acme-renewal".to_string()));
        assert_eq!(pinned, Some(true), "the pin stored");
        assert_eq!(
            named(&app),
            ("acme-renewal".to_string(), Some(id.clone())),
            "the person's name, which facts withhold"
        );
        ctx.meta.lock().unwrap().role = Some("aterm".to_string());
        assert_eq!(named(&app).0, "acme-renewal", "no role takes aterm's word");
        // Nor another Mac's: a name that ends as a drive's credit would read
        // as that Mac's typing (the step-in), however it is cased or spaced.
        for posing in [
            "mac-03 over the fleet",
            "mac-03 (node 1a2b3c4d)  Over The  Fleet",
        ] {
            ctx.meta.lock().unwrap().role = Some(posing.to_string());
            assert_eq!(named(&app).0, "acme-renewal", "{posing}");
        }
        // NEGATIVE CONTROL: a role that only names the fleet is a role.
        ctx.meta.lock().unwrap().role = Some("fleet-sweeper".to_string());
        assert_eq!(named(&app).0, "fleet-sweeper");
        ctx.meta.lock().unwrap().role = Some("lab-sweep".to_string());
        assert_eq!(
            named(&app),
            ("lab-sweep".to_string(), Some("lab-sweep".to_string()))
        );
    }

    /// `open overview`: the card opens on the front window, lists every
    /// session under its window, counts the workers a task's `from=` names
    /// and lists the manager with them; `controls overview` mirrors it, and
    /// the map's old `connections` keyword still reaches it. Closed, it says
    /// so.
    #[test]
    fn open_lists_sessions_workers_and_who_manages_them() {
        let (mut app, wid, sids) = app_with_three();
        give_task(&app, &sids[1], sids[0].as_str());
        give_task(&app, &sids[2], sids[0].as_str());
        assert_eq!(
            app.read_aux_controls(AuxTarget::Overview),
            vec!["overview open=false".to_string()]
        );
        assert!(app.open_overview().is_ok());
        assert_eq!(
            app.windows[&wid].overlay().map(|o| o.kind()),
            Some(OverlayKind::Overview)
        );
        assert_eq!(AuxTarget::parse("overview"), Some(AuxTarget::Overview));
        assert_eq!(AuxTarget::parse("connections"), Some(AuxTarget::Overview));
        assert_eq!(
            app.windows[&wid].overlay().unwrap().kind().keyword(),
            "overview"
        );
        let lines = app.read_aux_controls(AuxTarget::Overview);
        assert!(
            lines[0].contains("sessions=3")
                && lines[0].contains("workers=2")
                && lines[0].contains("manages=1"),
            "{lines:?}"
        );
        for worker in &sids[1..] {
            let line = chip_line(&app, worker);
            assert!(
                line.contains("task_state=working") && line.contains("driver=worker"),
                "{line}"
            );
            assert!(line.contains("stoppable=1"), "{line}");
        }
        assert!(
            lines.iter().any(|l| l.starts_with(&format!(
                "overview manages manager={} local={}",
                sids[0].as_str(),
                local_of(&app, &sids[0])
            )) && l.contains(sids[1].as_str())
                && l.contains(sids[2].as_str())),
            "{lines:?}"
        );
        assert!(app.close_aux_overlay(AuxTarget::Overview).is_ok());
        assert!(app.windows[&wid].overview().is_none());
    }

    /// THE CARD NAMES A BACKGROUND WORKER AS THE STRIP DOES: its chip, its
    /// manager's word on its line and the manages line all read the tabs'
    /// OSC titles (no role, pin or registry title stands for either), and an
    /// untitled tab reads its position. CONTROL: the third tab, with no
    /// title, is not named by either title.
    #[test]
    fn the_card_names_background_tabs_by_their_osc_titles() {
        use crate::overview::Driver;
        let (mut app, wid, sids) = app_with_three();
        app.switch_tab_in(wid, 0);
        for (sid, title) in [(&sids[0], "lab-sweep"), (&sids[1], "bench")] {
            let local = local_of(&app, sid);
            let term = app.pool.get(local).expect("pooled").term.clone();
            crate::term_lock(&term).process(format!("\x1b]2;{title}\x07").as_bytes());
        }
        give_task(&app, &sids[1], sids[0].as_str());
        give_task(&app, &sids[2], sids[0].as_str());
        let (groups, manages) = app.overview_parts();
        let chip = |sid: &aterm_session::SessionId| {
            groups
                .iter()
                .flat_map(|g| g.chips.iter())
                .find(|c| c.sid == *sid)
                .cloned()
                .expect("listed")
        };
        assert_eq!(chip(&sids[0]).label, "lab-sweep");
        assert_eq!(chip(&sids[1]).label, "bench");
        assert_eq!(
            chip(&sids[1]).driver,
            Some(Driver::WorkerOf("lab-sweep".to_string()))
        );
        assert_eq!(chip(&sids[2]).label, "tab 3", "the untitled tab");
        assert_eq!(manages.len(), 1);
        assert_eq!(manages[0].manager_label, "lab-sweep");
        assert_eq!(
            manages[0]
                .workers
                .iter()
                .map(|(_, label)| label.as_str())
                .collect::<Vec<_>>(),
            ["bench", "tab 3"]
        );
    }

    /// ⏎ on a chip raises through the `raise` body and closes the card.
    #[test]
    fn enter_on_a_chip_raises_and_closes() {
        let (mut app, wid, sids) = app_with_three();
        app.switch_tab_in(wid, 0);
        assert!(app.open_overview().is_ok());
        let target = chip_ordinal(&app, wid, &sids[2]);
        app.overview_move(wid, target as isize);
        app.overview_input_event(wid, &key(aterm_types::keyboard::NamedKey::Enter));
        assert!(app.windows[&wid].overview().is_none(), "closed");
        assert_eq!(app.windows[&wid].tab_set.active_index(), Some(2));
    }

    /// `t` takes the selected tab over and, pressed again, hands it back —
    /// the person's key; the chip says `yours`. A control client's `t` (the
    /// overlay gate's controller path) moves nothing.
    #[test]
    fn t_takes_the_selected_tab_over_and_a_controllers_t_does_not() {
        use crate::input::Source;
        let (mut app, wid, sids) = app_with_three();
        assert!(app.open_overview().is_ok());
        let target = chip_ordinal(&app, wid, &sids[1]);
        app.overview_move(wid, target as isize);
        let ctx = ctx_of(&app, &sids[1]);
        let t = InputEvent::Text("t".to_string());
        // A control client's `t`, through the overlay gate: refused.
        let _ = app.input_to_session(
            wid,
            t.clone(),
            Source::Controller,
            None,
            crate::app_input::PressPhase::Initial,
        );
        assert!(ctx.fabric.holds().person().is_none(), "a controller's t");
        // The person's `t`: taken over, and the chip says so.
        app.overview_input_event(wid, &t);
        assert!(ctx.fabric.holds().person().is_some());
        let line = chip_line(&app, &sids[1]);
        assert!(
            line.contains("mark=yours") && line.contains("driver=person"),
            "{line}"
        );
        app.overview_input_event(wid, &t);
        assert!(ctx.fabric.holds().person().is_none(), "handed back");
    }

    /// ⌫ arms the inline confirm on a worker and only the second press stops
    /// it; esc cancels first. A session with no live task is never armed.
    /// The stop is the `signal` verb's body on the session's own PTY: a stub
    /// has no foreground group, so the refusal is said on the band.
    #[test]
    fn delete_confirms_inline_before_it_stops_a_worker() {
        let (mut app, wid, sids) = app_with_three();
        give_task(&app, &sids[1], crate::task_record::FROM_CTL);
        assert!(app.open_overview().is_ok());
        // No task: ⌫ does nothing.
        let quiet = chip_ordinal(&app, wid, &sids[2]);
        app.overview_move(wid, quiet as isize);
        app.overview_input_event(wid, &key(aterm_types::keyboard::NamedKey::Backspace));
        assert!(app.read_aux_controls(AuxTarget::Overview)[0].contains("confirm=-"));
        // The worker: the first ⌫ arms, esc keeps, the card stays open.
        let worker = chip_ordinal(&app, wid, &sids[1]);
        app.overview_move(wid, worker as isize - quiet as isize);
        app.overview_input_event(wid, &key(aterm_types::keyboard::NamedKey::Backspace));
        assert!(
            app.read_aux_controls(AuxTarget::Overview)[0].contains(&format!("confirm={worker}"))
        );
        app.overview_input_event(wid, &key(aterm_types::keyboard::NamedKey::Escape));
        assert!(app.windows[&wid].overview().is_some());
        assert!(app.read_aux_controls(AuxTarget::Overview)[0].contains("confirm=-"));
        // Armed, ⏎ goes there and stops nothing.
        app.overview_input_event(wid, &key(aterm_types::keyboard::NamedKey::Backspace));
        app.overview_input_event(wid, &key(aterm_types::keyboard::NamedKey::Enter));
        assert!(app.windows[&wid].overview().is_none(), "⏎ went there");
        assert_eq!(app.agents.stop_signals, 0, "⏎ never confirms a stop");
        // Armed and confirmed by a second ⌫: the stop runs, and the signal's
        // refusal (a stub has no foreground group) is the person's row.
        assert!(app.open_overview().is_ok());
        let worker = chip_ordinal(&app, wid, &sids[1]);
        app.overview_move(wid, worker as isize);
        app.overview_input_event(wid, &key(aterm_types::keyboard::NamedKey::Backspace));
        app.overview_input_event(wid, &key(aterm_types::keyboard::NamedKey::Backspace));
        assert_eq!(app.agents.stop_signals, 1, "the confirmed stop signals");
        assert!(
            matches!(
                app.agents.last_stop_refused,
                Some(crate::app_overview::StopRefused::Signal(_))
            ),
            "{:?}",
            app.agents.last_stop_refused
        );
        assert!(stop_row_on_glass(&app), "the person's failed stop is said");
        assert!(app.read_aux_controls(AuxTarget::Overview)[0].contains("confirm=-"));
    }

    /// Whether a Stop Worker row stands on the band (the person's glass).
    fn stop_row_on_glass(app: &App) -> bool {
        app.messages.live_rows().any(|l| {
            l.msg
                .key
                .as_deref()
                .is_some_and(|k| k.starts_with(crate::message_reporters::KEY_STOP_WORKER))
        })
    }

    /// Arm ⌫ on `sid`'s chip and confirm it with a second ⌫, both from `src`
    /// (`Source::Controller` is the `key` verb's road through the overlay
    /// gate).
    fn stop_twice(
        app: &mut App,
        wid: WindowId,
        sid: &aterm_session::SessionId,
        src: crate::input::Source,
    ) {
        let target = chip_ordinal(app, wid, sid);
        let at = app.windows[&wid].overview().expect("open").controls_lines()[0].clone();
        let selected: isize = at
            .split(' ')
            .find_map(|kv| kv.strip_prefix("selected="))
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        app.overview_move(wid, target as isize - selected);
        for _ in 0..2 {
            let _ = app.input_to_session(
                wid,
                key(aterm_types::keyboard::NamedKey::Backspace),
                src,
                None,
                crate::app_input::PressPhase::Initial,
            );
        }
    }

    /// STOP WORKER READS THE TASK AGAIN WHEN IT ACTS: armed on a working
    /// worker whose task then ends, the confirming ⌫ stops nothing and says
    /// it is not running — no signal is sent. NEGATIVE CONTROL: a task still
    /// working at the confirm reaches the signal.
    #[test]
    fn stop_worker_reads_the_task_again_when_it_acts() {
        use crate::app_overview::StopRefused;
        use crate::task_record::{TaskMark, TaskState, mark_session_task};
        for ends in [true, false] {
            let (mut app, wid, sids) = app_with_three();
            give_task(&app, &sids[1], crate::task_record::FROM_CTL);
            assert!(app.open_overview().is_ok());
            let worker = chip_ordinal(&app, wid, &sids[1]);
            app.overview_move(wid, worker as isize);
            app.overview_input_event(wid, &key(aterm_types::keyboard::NamedKey::Backspace));
            assert!(
                app.read_aux_controls(AuxTarget::Overview)[0]
                    .contains(&format!("confirm={worker}")),
                "armed"
            );
            if ends {
                let ctx = ctx_of(&app, &sids[1]);
                mark_session_task(
                    &ctx,
                    local_of(&app, &sids[1]),
                    &TaskMark::new(TaskState::Done),
                )
                .expect("unfenced");
            }
            app.overview_input_event(wid, &key(aterm_types::keyboard::NamedKey::Backspace));
            if ends {
                assert_eq!(app.agents.last_stop_refused, Some(StopRefused::NotRunning));
                assert_eq!(app.agents.stop_signals, 0, "an ended task is not signalled");
                assert!(
                    !stop_row_on_glass(&app),
                    "asked for what already is: a record"
                );
            } else {
                // It reached the signal (a stub has no foreground group, so
                // the verb's own refusal is what comes back).
                assert!(
                    matches!(
                        app.agents.last_stop_refused,
                        None | Some(StopRefused::Signal(_))
                    ),
                    "{:?}",
                    app.agents.last_stop_refused
                );
                assert_eq!(app.agents.stop_signals, 1, "a working task is");
            }
        }
    }

    /// A WORKER WITH ONLY ITS SHELL IN FRONT IS NOT "STOPPED". Its SIGTERM
    /// would go to the interactive shell, which ignores it while `killpg`
    /// reports it sent, so the stop would say done and end nothing: the
    /// confirmed ⌫⌫ and the tab menu's Stop are refused before any signal
    /// (no sheet is asked), and the person's failed gesture is one row on
    /// the glass with the act that still ends it. NEGATIVE CONTROL: the same
    /// worker with its agent in front reaches the signal.
    #[test]
    fn a_worker_with_only_its_shell_in_front_is_refused() {
        use crate::app_overview::StopRefused;
        use crate::session_chrome::ConnVerb;
        for program in ["zsh", "claude"] {
            let (mut app, wid, sids) = app_with_three();
            give_task(&app, &sids[1], sids[0].as_str());
            {
                let ctx = ctx_of(&app, &sids[1]);
                let mut tl = ctx.timeline.lock().unwrap();
                tl.note_foreground_group(7);
                tl.set_program(7, Some(program.to_string()));
            }
            let shell = program == "zsh";
            assert!(app.open_overview().is_ok());
            stop_twice(&mut app, wid, &sids[1], crate::input::Source::Human);
            if shell {
                assert_eq!(app.agents.last_stop_refused, Some(StopRefused::NoAgent));
                assert_eq!(app.agents.stop_signals, 0, "the shell is not signalled");
                let rows = app
                    .messages
                    .live_rows()
                    .filter(|l| {
                        l.msg.key.as_deref().is_some_and(|k| {
                            k.starts_with(crate::message_reporters::KEY_STOP_WORKER)
                        })
                    })
                    .count();
                assert_eq!(rows, 1, "one gesture row");
                // The tab menu's row refuses before any sheet is asked.
                app.agents.owner_sheet_for_test = Some(Some(true));
                let tab = app.windows[&wid].tab_set.tabs()[1].id;
                app.dispatch_tab_menu_connection(wid, tab, &sids[1], ConnVerb::StopWorker);
                assert_eq!(app.agents.stop_signals, 0, "the tab menu's row too");
            } else {
                assert_eq!(app.agents.stop_signals, 1, "an agent in front is signalled");
                assert_ne!(app.agents.last_stop_refused, Some(StopRefused::NoAgent));
            }
        }
    }

    /// A CONTROL CLIENT'S TAB-MENU STOP PUTS NOTHING ON THE PERSON'S GLASS.
    /// Its choice of Stop Worker… (keys into the tab menu, under the gesture
    /// mark) is the wire `signal` verb's act on an unheld worker: the stop
    /// goes out, and no sheet, no card and no row appear. NEGATIVE CONTROL:
    /// the person's choice with no sheet to ask on (headless) opens the
    /// Overview with the stop armed for their hand and sends nothing.
    #[test]
    fn a_control_clients_tab_menu_stop_opens_no_card() {
        use crate::session_chrome::ConnVerb;
        for controller in [true, false] {
            let (mut app, wid, sids) = app_with_three();
            give_task(&app, &sids[1], sids[0].as_str());
            let tab = app.windows[&wid].tab_set.tabs()[1].id;
            app.with_gesture_source(controller, |app| {
                app.dispatch_tab_menu_connection(wid, tab, &sids[1], ConnVerb::StopWorker);
            });
            if controller {
                assert_eq!(app.agents.stop_signals, 1, "the signal verb's act");
                assert!(app.windows[&wid].overview().is_none(), "no card");
                assert!(!stop_row_on_glass(&app), "no row");
            } else {
                assert_eq!(app.agents.stop_signals, 0, "nothing unconfirmed");
                let line = chip_line(&app, &sids[1]);
                assert!(line.contains("confirm=true"), "{line}");
                assert!(
                    app.read_aux_controls(AuxTarget::Overview)[0].contains("confirm_by=person"),
                    "armed for the person's hand"
                );
            }
        }
    }

    /// A CONTROL CLIENT CANNOT STOP A HELD WORKER THROUGH THE OVERVIEW OR THE
    /// TAB MENU, AND ITS REFUSAL IS NOT THE PERSON'S NEWS. The wire `signal`
    /// verb to a held tab answers `ERR halted`; a control client's ⌫ into the
    /// open card (the `key` verb's road) on that tab's chip is the same act
    /// and is refused the same way, under an Owner's hold and under the
    /// person's Take Over alike: each ⌫ arms nothing (no confirm is left for
    /// the person's next key), no signal is sent, the hold stands, a refusal
    /// under the person's hold is counted, and nothing reaches the person's
    /// glass. The tab menu's Stop Worker under the same mark is refused the
    /// same way. NEGATIVE CONTROL: the person's own ⌫⌫ on the same chip,
    /// under the same hold, reaches the signal (Take Over stops the robots,
    /// not the person); and a control client's ⌫⌫ on an unheld worker does
    /// too, as the wire verb would.
    #[test]
    fn a_controller_cannot_stop_a_held_worker_and_the_person_can() {
        use crate::app_overview::StopRefused;
        use crate::input::Source;
        use crate::session_chrome::ConnVerb;
        for person in [false, true] {
            let (mut app, wid, sids) = app_with_three();
            give_task(&app, &sids[1], sids[0].as_str());
            give_task(&app, &sids[2], sids[0].as_str());
            let held = ctx_of(&app, &sids[1]);
            if person {
                assert!(app.take_over_session(local_of(&app, &sids[1])).is_ok());
            } else {
                let on = format!("{} on reason=mine", sids[1].as_str());
                assert_eq!(
                    crate::fabric::cmd_hold(&app.store, &on, crate::fabric::HoldIssuer::Owner),
                    "OK hold=1\n"
                );
            }
            let wire = crate::fabric::halt_refusal(&held, "signal").expect("the wire is halted");
            assert!(wire.starts_with("ERR halted "), "{wire}");
            let counted = held.fabric.person_refusals();
            assert!(app.open_overview().is_ok());

            stop_twice(&mut app, wid, &sids[1], Source::Controller);
            assert!(
                matches!(app.agents.last_stop_refused, Some(StopRefused::Halted(_))),
                "person={person}: {:?}",
                app.agents.last_stop_refused
            );
            assert_eq!(app.agents.stop_signals, 0, "person={person}: no signal");
            assert!(held.fabric.holds().is_held(), "the hold stands");
            assert!(
                app.read_aux_controls(AuxTarget::Overview)[0].contains("confirm=-"),
                "no confirm left armed"
            );
            assert_eq!(
                held.fabric.person_refusals(),
                counted + 2 * u64::from(person),
                "each refused ⌫, counted only under the person's hold"
            );
            assert!(!stop_row_on_glass(&app), "person={person}: no row");
            assert!(
                !app.person_gesture_refused(),
                "the mark ends with the gesture"
            );
            // The tab menu's row, chosen under the same mark.
            app.agents.last_stop_refused = None;
            let tab = app.windows[&wid].tab_set.tabs()[0].id;
            app.with_gesture_source(true, |app| {
                app.dispatch_tab_menu_connection(wid, tab, &sids[1], ConnVerb::StopWorker);
            });
            assert!(
                matches!(app.agents.last_stop_refused, Some(StopRefused::Halted(_))),
                "person={person}: the tab menu's row"
            );
            assert_eq!(app.agents.stop_signals, 0);
            assert!(!stop_row_on_glass(&app), "person={person}: no row");

            // CONTROL: the person's own ⌫⌫ on the same chip is signalled.
            stop_twice(&mut app, wid, &sids[1], Source::Human);
            assert_eq!(
                app.agents.stop_signals, 1,
                "person={person}: the person stops it"
            );
            // CONTROL: a control client's ⌫⌫ on an unheld worker is too.
            stop_twice(&mut app, wid, &sids[2], Source::Controller);
            assert_eq!(
                app.agents.stop_signals, 2,
                "an unheld worker, as the wire verb"
            );
        }
    }

    /// A session close while the card is open re-snapshots it: the closed
    /// session's line leaves now.
    #[test]
    fn session_close_retargets_the_open_card() {
        let (mut app, _wid, sids) = app_with_three();
        assert!(app.open_overview().is_ok());
        assert!(app.read_aux_controls(AuxTarget::Overview)[0].contains("sessions=3"));
        let local = local_of(&app, &sids[2]);
        app.retire_session_registration(local);
        let lines = app.read_aux_controls(AuxTarget::Overview);
        assert!(lines[0].contains("sessions=2"), "{lines:?}");
        assert!(!lines.iter().any(|l| l.contains(sids[2].as_str())));
    }

    /// THE TITLE COUNTS THE OTHER ATERMS' SESSIONS APART (§6; ❯'s This Mac
    /// line says the same figure): a sibling scan that finds another aterm
    /// with three sessions reaches an open card's title as "(+3 in another
    /// aterm)" beside this aterm's own count. NEGATIVE CONTROL: with no
    /// sibling the title names this aterm alone.
    #[test]
    fn the_title_counts_another_aterms_sessions_apart() {
        let (mut app, wid, _sids) = app_with_three();
        assert!(app.open_overview().is_ok());
        let title = |app: &App| {
            app.windows
                .get(&wid)
                .and_then(|ws| ws.overview())
                .map(crate::overview::OverviewState::title)
                .expect("open")
        };
        assert!(title(&app).starts_with("This aterm \u{2014} 3 sessions"));
        assert!(!title(&app).contains("another aterm"), "{}", title(&app));
        app.siblings = vec![crate::status_item::SiblingInstance {
            pid: 900,
            sessions: 3,
            ..crate::status_item::SiblingInstance::default()
        }];
        app.overview_refresh_all();
        assert!(
            title(&app).starts_with("This aterm \u{2014} 3 sessions (+3 in another aterm)"),
            "{}",
            title(&app)
        );
        app.siblings.clear();
        app.overview_refresh_all();
        assert!(!title(&app).contains("another aterm"), "{}", title(&app));
    }

    /// THE OVERVIEW NAMES WHO DRIVES AS THE HOVER DOES (§12.4.1; one helper,
    /// `app_presence::driven_words`): a Claude Code under Land this host
    /// supervises reads `driven by aterm (land)`, then `(land, landed)` once
    /// its loop tells the landing; a worker Land landed reads `task landed
    /// …`, never `task done … (landed)` nor `task landed … (landed)`.
    /// NEGATIVE CONTROLS (as run): main's mode word alone reads `driven by
    /// aterm (land)` after the landing, and its task words `task done just
    /// now (landed)`; the why kept beside `landed` reads `task landed just
    /// now (landed)`.
    #[test]
    fn the_overview_names_the_chips_face_and_a_landed_task() {
        use crate::overview::Driver;
        use aterm_agent::harness::acts::TaskFace;
        let mut app = App::headless_for_test();
        let host = crate::harness_host::HostHandle::for_supervised_test(
            Vec::new(),
            std::sync::Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
        );
        app.harness = Some(host.clone());
        let ctx = app.pool.get(0).expect("session 0").ctx.clone();
        {
            let mut tl = ctx.timeline.lock().expect("timeline");
            tl.note_foreground_group(7);
            tl.set_program(7, Some("claude".into()));
            tl.publish_agent(
                "idle",
                None,
                None,
                Some(aterm_phase::Program::Claude),
                crate::session_timeline::AgentStamp {
                    generation: crate::control::ScreenGen { epoch: 1, seq: 1 },
                    fp: 1,
                },
            );
        }
        ctx.meta
            .lock()
            .expect("meta")
            .set("autopilot", Some("land".into()));
        crate::session_timeline::claim_supervisor(
            &ctx,
            &crate::harness_host::holder(),
            None,
            None,
            crate::metrics::now_us(),
        )
        .expect("claimed");
        let sid = ctx.self_id.clone();
        let chip = |app: &App| {
            let (groups, _) = app.overview_parts();
            groups
                .iter()
                .flat_map(|g| g.chips.iter())
                .find(|c| c.sid == sid)
                .cloned()
                .expect("listed")
        };
        assert_eq!(chip(&app).driver, Some(Driver::Aterm("land".to_string())));
        app.apply_harness_fact(sid.as_str(), crate::HarnessFact::Task(Some(TaskFace::Open)));
        app.apply_harness_fact(
            sid.as_str(),
            crate::HarnessFact::Task(Some(TaskFace::Landed)),
        );
        assert_eq!(
            chip(&app).driver,
            Some(Driver::Aterm("land, landed".to_string()))
        );
        assert_eq!(
            chip(&app).driver.map(|d| d.words()).as_deref(),
            Some("driven by aterm (land, landed)")
        );
        // A worker Land landed.
        let mut mark = crate::task_record::TaskMark::new(crate::task_record::TaskState::Done);
        mark.why = Some(crate::task_record::WHY_LANDED);
        crate::task_record::mark_session_task(&ctx, 0, &mark).expect("marked");
        let words = chip(&app).task_words.expect("a task");
        assert!(words.starts_with("task landed "), "{words}");
        assert!(!words.contains("done"), "{words}");
        assert_eq!(words.matches("landed").count(), 1, "said once: {words}");
        host.shutdown_and_join();
    }

    /// THE OVERVIEW SAYS WHY A LANDING HOLDS, ON A LINE OF ITS OWN (Autopilot
    /// v5 §12.3.1): a Claude Code under Land this
    /// host supervises reads `driven by aterm (land)` on its chip's line
    /// throughout — the hover's long words there pushed `typed by` past the
    /// card's width — and, once its loop tells a hold whose yield was read,
    /// the hold's words with no clock on the chip's held line (`not landed
    /// yet: 2 unpushed commits; open work in its report: follow-up · 3
    /// nudges`, the menu's tip form); none before a yield is read, none under
    /// Cruise, none once the hold is taken down. NEGATIVE CONTROLS (as run):
    /// the builder passing the hold to `driven_words` (3c's first cut) puts
    /// the hold on the chip's line; the builder with no held line reads
    /// `None` with the hold standing.
    #[test]
    fn the_overview_says_why_a_landing_holds() {
        use aterm_agent::harness::acts::{HoldLeg, LandHeld, OpenWord, TaskFace};
        let mut app = App::headless_for_test();
        let host = crate::harness_host::HostHandle::for_supervised_test(
            Vec::new(),
            std::sync::Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
        );
        app.harness = Some(host.clone());
        let ctx = app.pool.get(0).expect("session 0").ctx.clone();
        {
            let mut tl = ctx.timeline.lock().expect("timeline");
            tl.note_foreground_group(7);
            tl.set_program(7, Some("claude".into()));
        }
        ctx.meta
            .lock()
            .expect("meta")
            .set("autopilot", Some("land".into()));
        crate::session_timeline::claim_supervisor(
            &ctx,
            &crate::harness_host::holder(),
            None,
            None,
            crate::metrics::now_us(),
        )
        .expect("claimed");
        let sid = ctx.self_id.clone();
        let chip = |app: &App| {
            let (groups, _) = app.overview_parts();
            groups
                .iter()
                .flat_map(|g| g.chips.iter())
                .find(|c| c.sid == sid)
                .map(|c| {
                    (
                        c.driver.as_ref().map(crate::overview::Driver::words),
                        c.held.clone(),
                    )
                })
                .expect("its chip")
        };
        let tell = |app: &mut App, held: Option<LandHeld>| {
            if crate::harness_host::store_land_held(&app.store, sid.as_str(), held) {
                app.apply_harness_fact(sid.as_str(), crate::HarnessFact::LandHeld);
            }
        };
        let land = Some("driven by aterm (land)".to_string());
        app.apply_harness_fact(sid.as_str(), crate::HarnessFact::Task(Some(TaskFace::Open)));
        assert_eq!(chip(&app), (land.clone(), None));
        let held = LandHeld {
            since_ms: Some(1_792_159_320_000),
            nudges: 3,
            read: true,
            legs: HoldLeg::Repo.bit() | HoldLeg::Open.bit(),
            open: [OpenWord::of("follow-up"), None, None, None],
            unpushed: 2,
            ..LandHeld::default()
        };
        tell(
            &mut app,
            Some(LandHeld {
                read: false,
                legs: 0,
                ..held
            }),
        );
        assert_eq!(chip(&app), (land.clone(), None), "no yield read yet");
        tell(&mut app, Some(held));
        assert_eq!(
            chip(&app),
            (
                land.clone(),
                Some(
                    "not landed yet: 2 unpushed commits; open work in its report: follow-up \
                     \u{b7} 3 nudges"
                        .to_string()
                )
            )
        );
        ctx.meta
            .lock()
            .expect("meta")
            .set("autopilot", Some("cruise".into()));
        assert_eq!(chip(&app).1, None, "none under Cruise");
        ctx.meta
            .lock()
            .expect("meta")
            .set("autopilot", Some("land".into()));
        tell(&mut app, None);
        assert_eq!(chip(&app), (land, None));
        host.shutdown_and_join();
    }

    /// A HARNESS'S NEED SAYS IT IS ONE ON THE OVERVIEW (Autopilot v5
    /// §12.2.9): no header above an Overview line says "needs you", and a
    /// harness's `what` now carries the need without aterm's frame, so the
    /// chip's doing (painted and spoken alike) leads with `needs you: ` and
    /// keeps ` (+1 more)` whole within main's 72 bytes. Another owner's ask
    /// keeps its own words. NEGATIVE CONTROL (run for the landing): the
    /// doing arm reading `need.what` drops the lead.
    #[test]
    fn a_harness_need_says_needs_you_on_the_overview() {
        let (mut app, _wid, sids) = app_with_three();
        let (harness, person) = (&sids[1], &sids[2]);
        let need = "grant Screen Recording to com.aterm.aterm in System Settings \u{25b8} \
                    Privacy & Security \u{25b8} Screen Recording";
        let ask = |sid: &aterm_session::SessionId, owner: &str, text: String| {
            ctx_of(&app, sid)
                .meta
                .lock()
                .unwrap()
                .set_attention_kind(
                    owner,
                    Some(text),
                    Some(aterm_types::needs::AttentionKind::Ask),
                )
                .expect("a slot");
        };
        ask(
            harness,
            crate::harness_host::NEEDS_ATTENTION_OWNER,
            format!("claude needs you: {need} (+1 more) \u{2014} carrying on"),
        );
        ask(person, "person", "sign the release notes".to_string());
        for sid in [harness, person] {
            app.publish_need(local_of(&app, sid));
        }
        let doing = |sid: &aterm_session::SessionId| {
            let (groups, _) = app.overview_parts();
            groups
                .iter()
                .flat_map(|g| g.chips.iter())
                .find(|c| &c.sid == sid)
                .map(|c| c.doing.clone())
                .expect("listed")
        };
        let words = doing(harness);
        assert!(
            words.starts_with("needs you: grant Screen Recording")
                && words.ends_with("\u{2026} (+1 more)"),
            "{words}"
        );
        assert!(!words.contains("claude"), "{words}");
        assert!(words.len() <= crate::needs::ROW_WHAT_MAX, "{words}");
        assert_eq!(doing(person), "sign the release notes");
    }

    /// The words are coarse, so a card left open does not repaint each
    /// second, and a result's size reads in units.
    #[test]
    fn ages_and_sizes_read_in_coarse_units() {
        assert_eq!(super::coarse_age(59_000), "just now");
        assert_eq!(super::coarse_age(4 * 60_000 + 59_000), "4m");
        assert_eq!(super::coarse_age(2 * 3_600_000), "2h");
        assert_eq!(super::size_words(812), "812 B");
        assert_eq!(super::size_words(2150), "2.1 KB");
    }
}
