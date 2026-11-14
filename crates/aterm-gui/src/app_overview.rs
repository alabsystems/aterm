// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `App` glue for the Overview ([`crate::overview`]) in the modal [`crate::overlay::Overlay`] slot.

use aterm_session::SessionId;

use crate::App;
use crate::WindowId;
use crate::overview::{
    ChipMark, Driver, Manages, OverviewAct, OverviewChip, OverviewGroup, OverviewState,
};

/// How often an open card's coarse ages are read again at paint time ([`App::overview_prepaint`]).
pub(crate) const OVERVIEW_AGE_STEP: std::time::Duration = std::time::Duration::from_secs(60);

/// A coarse age, so a card left open does not repaint every second.
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

/// A result's size in words.
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

fn short_sid(sid: &str) -> String {
    aterm_messages::presence::short_sid(sid)
}

/// The Stop Worker sheet's words for the worker labelled `label`: its title, its body and its
/// proceed button (`objc_confirm_drive` raises the same sheet on a real `NSAlert`).
pub(crate) fn stop_worker_sheet(label: &str) -> (&'static str, String, &'static str) {
    (
        "Stop Worker",
        format!("Stop {label}? Its agent gets SIGTERM and its task reads exited."),
        "Stop",
    )
}

impl App {
    pub(crate) fn overview_chip(
        &self,
        h: &crate::session_store::SessionHandle,
        hosting: &crate::harness_reach::HarnessHosting,
    ) -> OverviewChip {
        let ctx = &h.ctx;
        let autopilot = crate::autopilot::word_of(ctx);
        let (verdict, agent_word, agent_age, program, task) = {
            let tl = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            let (word, age) = tl.shown_agent_word();
            (
                tl.need().clone(),
                word,
                age,
                crate::harness_host::agent_of(tl.agent()),
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
            (Some(need), _) => need.harness.as_ref().map_or_else(
                || need.what.clone(),
                |ask| ask.cued_line(crate::needs::ROW_WHAT_MAX),
            ),
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
        // A record that reads `done why=landed`: its task words say `landed`, so the driver's
        // words do not say it again (§14.6).
        let landed = task.as_ref().is_some_and(|t| {
            t.state == crate::task_record::TaskState::Done
                && t.why == Some(crate::task_record::WHY_LANDED)
        });
        let task_words = task.as_ref().map(|t| {
            let age = coarse_age(t.since_ms());
            let mut words = if landed {
                format!("task landed {age}")
            } else {
                format!("task {} {age}", t.state.as_str())
            };
            if let Some(why) = t.why.filter(|_| !landed) {
                words.push_str(&format!(" ({why})"));
            }
            if t.state.marks_its_tab() && t.result_bytes > 0 {
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
        // A session the host admits is driven only while aterm's Autopilot carries it
        // (`autopilot::carried`, the hover's `driven_by_aterm` rule too): a plain codex gets no line.
        let (driven, held) = program
            .filter(|agent| hosting.hosts(h.sid.as_str(), autopilot, *agent))
            .and_then(|agent| {
                let (_, view, supervised, self_proven) = self.session_autopilot_for(
                    h.local_id,
                    self.session_agent(h.local_id),
                    &crate::autopilot::ClaimClock::default(),
                );
                crate::autopilot::carried(agent, supervised, self_proven).then_some(view)
            })
            .map_or((None, None), |view| {
                let glass = self.chip_glass(h.local_id);
                let face = crate::claude_lights::ChipFace::of(view, &glass);
                let held = self
                    .shown_hold(h.local_id)
                    .filter(|hold| hold.hold.read())
                    .and_then(|hold| {
                        hold.words(None, crate::harness_glass::HoldForm::Tip, None, None)
                    });
                let stopped = self.shown_stop(h.local_id);
                (
                    crate::app_presence::driven_words(face, &glass, None, landed, stopped),
                    held,
                )
            });
        let driven = driven.map(|words| self.autopilot_driver(h.local_id, words));
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
        let has_result = task
            .as_ref()
            .is_some_and(crate::native_workers::result_to_read);
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
            stoppable: live_task || crate::native_act::renewal_pending(ctx),
        }
    }

    /// The Overview's window groups of chips, and who manages whom, as a refresh lays them out.
    pub(crate) fn overview_parts(&self) -> (Vec<OverviewGroup>, Vec<Manages>) {
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
                tab.root.visit(&mut |&view| {
                    let Some(local) = self
                        .view_store
                        .get(view)
                        .copied()
                        .and_then(crate::tab_model::View::terminal_session)
                    else {
                        return;
                    };
                    let Some(h) = by_local.get(&local) else {
                        return; // an unregistered stub labels no chip
                    };
                    if placed.insert(local) {
                        chips.push(self.overview_chip(h, &hosting));
                    }
                });
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

    /// Open the Overview (host+raise).
    pub(crate) fn open_overview(&mut self) -> Result<(), String> {
        let Some(wid) = self.frontmost_window else {
            return Err("no front window to open the overview on".to_string());
        };
        self.open_overview_on(wid)
    }

    /// The tab menu's Stop Worker… where no sheet can ask the person (headless, off macOS).
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

    /// Open the Overview at manager `session`'s manages line (its Team chip's entrance), by
    /// its local id, raising the window.
    pub(crate) fn open_overview_raised(
        &mut self,
        wid: WindowId,
        session: u64,
    ) -> Result<(), String> {
        self.open_overview_on(wid)?;
        if let Some(card) = self.windows.get_mut(&wid).and_then(|ws| ws.overview_mut())
            && card.select_session(session)
        {
            self.repaint_overview_pointer(wid, true);
        }
        self.raise_window(wid);
        Ok(())
    }

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

    /// Re-snapshot every open card, selection and confirm surviving by identity.
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

    /// Re-read the words into an open card on `wid` from the redraw path.
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

    /// Esc: cancel an armed confirm first, close second.
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

    fn overview_run(&mut self, wid: WindowId, act: OverviewAct) {
        match act {
            OverviewAct::Raise(local) => {
                self.overview_exit(wid);
                if let Err(e) = self.raise_session_by_id(local) {
                    aterm_log::info!("overview raise {local}: {e}");
                }
            }
            OverviewAct::TakeOverOrHandBack(local) => {
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

    /// ⏎ / click: go there (a chip's session, a manages line's manager).
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

    /// Why Stop Worker would stop nothing on `session` right now, before any signal.
    pub(crate) fn stop_worker_refusal(&self, session: u64) -> Option<StopRefused> {
        let Some(ctx) = self.pool.get(session).map(|s| s.ctx.clone()) else {
            return Some(StopRefused::Gone);
        };
        stop_worker_refusal_in(&ctx, self.person_gesture_refused())
    }

    /// Say why stop worker stopped nothing, to whom it concerns.
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
                crate::message_reporters::stop_worker_nothing(session, &label, &why.words())
            }
            StopRefused::NoAgent | StopRefused::Signal(_) => {
                crate::message_reporters::stop_worker_failed(session, &label, &why.words())
            }
        };
        self.post_message(msg);
    }

    /// Stop worker: SIGTERM to `session`'s agent, in process, through the `signal` verb's own body.
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
        // A person's Stop, or a control client's gesture standing in for one: the run it ends
        // closes `out=signal` by whoever asked.
        let by = if self.person_gesture_refused() {
            aterm_slog::Actor::Ctl
        } else {
            aterm_slog::Actor::Human
        };
        stop_worker_in(master, &ctx, by)
    }

    /// Read result: open a done worker's result file as a Markdown document tab on `wid`, a live
    /// worker's or one whose tab aterm closed ([`crate::retired_workers`]).
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
                    .filter(|t| crate::native_workers::result_to_read(t))
                    .and_then(|t| t.result_path.clone())
            })
            .or_else(|| {
                self.finished_row(session)
                    .map(|row| row.record)
                    .filter(crate::native_workers::result_to_read)
                    .and_then(|record| record.result_path)
            })
            .ok_or_else(|| "no result to read".to_string())?;
        self.overview_exit(wid);
        let _ = self.open_menu_document(wid, path);
        Ok(())
    }

    /// While the card is open on `wid`, drive it from the keyboard and swallow every key.
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

    /// The engine-neutral twin of [`Self::on_key_overview_mode`].
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

    /// Modal pointer boundary.
    pub(crate) fn overview_claims_pointer(&mut self, wid: WindowId) -> bool {
        self.overlay_card_claims_pointer(
            wid,
            |ws| ws.overview().is_some(),
            Self::sync_overview_pointer_cursor,
        )
    }

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

    /// Settle a left release: same-item press+release activates through the same seam as Enter.
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

    /// Scroll the band (wheel).
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
    /// Its task is not working or stalled.
    NotRunning,
    /// A control client's gesture on a held tab.
    Halted(String),
    /// No agent runs in the tab.
    NoAgent,
    /// The `signal` verb's own refusal (no foreground program, a platform with no signals).
    Signal(String),
}

/// The production Stop Worker preconditions on an already resolved session.
/// Human owner gestures may stop a held tab; control clients retain the fabric refusal.
pub(crate) fn stop_worker_refusal_in(
    ctx: &crate::SessionCtx,
    controller: bool,
) -> Option<StopRefused> {
    let refusal = stop_worker_signal_refusal_in(ctx, controller);
    if !controller
        && matches!(
            refusal,
            Some(StopRefused::NotRunning | StopRefused::NoAgent)
        )
        && crate::native_act::renewal_pending(ctx)
    {
        None
    } else {
        refusal
    }
}

fn stop_worker_signal_refusal_in(ctx: &crate::SessionCtx, controller: bool) -> Option<StopRefused> {
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
    if controller && let Some(refusal) = crate::fabric::halt_refusal(ctx, "signal") {
        return Some(StopRefused::Halted(refusal.trim_end().to_string()));
    }
    if shell_in_front {
        return Some(StopRefused::NoAgent);
    }
    None
}

/// Run the confirmed production Stop Worker operation, rechecking its live session.
/// The caller owns the UI confirmation and records the actual person gesture first.
pub(crate) fn stop_worker_in(
    master: i32,
    ctx: &crate::SessionCtx,
    by: aterm_slog::Actor,
) -> Result<String, StopRefused> {
    if let Some(refused) = stop_worker_refusal_in(ctx, !matches!(by, aterm_slog::Actor::Human)) {
        return Err(refused);
    }
    let cancelled = if matches!(by, aterm_slog::Actor::Human) {
        crate::native_act::cancel_renewal_owner(ctx)
            .map_err(|why| StopRefused::Signal(why.into()))?
    } else {
        false
    };
    if cancelled && crate::native_act::cancelled_native_shell_front(ctx, master) {
        return Ok("recovery cancelled".into());
    }
    if let Some(refused) =
        stop_worker_signal_refusal_in(ctx, !matches!(by, aterm_slog::Actor::Human))
    {
        return if cancelled && matches!(refused, StopRefused::NotRunning | StopRefused::NoAgent) {
            Ok("recovery cancelled".into())
        } else {
            Err(refused)
        };
    }
    let reply = crate::control::signal_verb(master, "term", ctx, by);
    let reply = reply.trim_end().to_string();
    match reply.strip_prefix("OK ") {
        Some(ok) => Ok(ok.to_string()),
        None => Err(StopRefused::Signal(
            reply.strip_prefix("ERR ").unwrap_or(&reply).to_string(),
        )),
    }
}

/// Whether `publication` says the tab's foreground program is its shell and no agent.
pub(crate) fn shell_in_front(publication: &crate::session_timeline::AgentPublication) -> bool {
    crate::harness_host::agent_of(publication).is_none()
        && publication
            .program
            .as_deref()
            .is_some_and(crate::quit_safety::is_shell_name)
}

impl StopRefused {
    /// The refusal in words: the record's line.
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
#[path = "app_overview_tests.rs"]
mod tests;
