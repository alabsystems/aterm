// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One bounded account-menu read, with a latest pending request and owner fencing.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aterm_agent::harness::{accounts, footer::FactsOwner};

use crate::autopilot_menu::MenuOwner;
use crate::claude_lights::Fact;
use crate::fact_menu::{AccountMenuRow, SessionWeek};
use crate::session_timeline::{ForegroundStamp, SessionTimeline};
use crate::{App, Wake, WindowId};

pub(crate) type ResultRows = Result<(Vec<AccountMenuRow>, bool), String>;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Key {
    session: u64,
    timeline: usize,
    foreground: Option<ForegroundStamp>,
    pgid: i32,
    birth: Option<u64>,
    owner: Option<FactsOwner>,
    cli: accounts::Cli,
    account: Option<String>,
    email: Option<String>,
    week: SessionWeek,
    config: Option<PathBuf>,
    state: Option<PathBuf>,
    home: Option<PathBuf>,
}

#[derive(Clone)]
struct Input {
    key: Key,
    // Keep this allocation alive so an old request cannot match a reused address.
    _owner: Arc<Mutex<SessionTimeline>>,
}

struct Active {
    generation: u64,
    wid: WindowId,
    input: Input,
    rows: Option<Arc<ResultRows>>,
    read_at: Option<Instant>,
}

#[derive(Default)]
pub(crate) struct State {
    generation: u64,
    active: Option<Active>,
    in_flight: Option<u64>,
    pending: Option<(u64, Input)>,
    deferred_keyboard_selection: bool,
    #[cfg(test)]
    hold_for_test: bool,
}

#[derive(Debug)]
pub(crate) struct Completion {
    generation: u64,
    rows: ResultRows,
}

impl State {
    pub(crate) fn close(&mut self) {
        self.generation = self.generation.saturating_add(1);
        self.active = None;
        self.pending = None;
        self.deferred_keyboard_selection = false;
    }

    pub(crate) fn defer_keyboard_selection(&mut self, defer: bool) {
        self.deferred_keyboard_selection = defer;
    }

    pub(crate) fn cancel_keyboard_selection(&mut self) {
        self.deferred_keyboard_selection = false;
    }

    fn request(&mut self, wid: WindowId, input: Input, now: Instant) {
        let same = self
            .active
            .as_ref()
            .filter(|active| active.wid == wid && active.input.key == input.key);
        if let Some(active) = same
            && (self.in_flight == Some(active.generation)
                || self
                    .pending
                    .as_ref()
                    .is_some_and(|(generation, _)| *generation == active.generation)
                || active
                    .read_at
                    .is_some_and(|at| now.saturating_duration_since(at) < Duration::from_secs(2)))
        {
            return;
        }
        let (rows, read_at) =
            same.map_or((None, None), |active| (active.rows.clone(), active.read_at));
        if self
            .active
            .as_ref()
            .is_some_and(|active| active.input.key != input.key)
        {
            self.deferred_keyboard_selection = false;
        }
        self.generation = self.generation.saturating_add(1);
        self.active = Some(Active {
            generation: self.generation,
            wid,
            input: input.clone(),
            rows,
            read_at,
        });
        self.pending = Some((self.generation, input));
    }
}

impl App {
    pub(crate) fn account_menu_read_deadline(&self) -> Option<Instant> {
        let menu = self.autopilot_menu.as_ref()?;
        if menu.owner != MenuOwner::Fact(Fact::Account) {
            return None;
        }
        if self
            .windows
            .get(&menu.wid)
            .is_none_or(|window| window.occluded)
        {
            return None;
        }
        let active = self.account_menu_read.active.as_ref()?;
        if active.wid != menu.wid
            || active.input.key.session != menu.session
            || self.account_menu_read.in_flight == Some(active.generation)
            || self
                .account_menu_read
                .pending
                .as_ref()
                .is_some_and(|(generation, _)| *generation == active.generation)
        {
            return None;
        }
        active.read_at.map(|at| at + Duration::from_secs(2))
    }

    pub(crate) fn service_account_menu_read(&mut self, now: Instant) {
        if self
            .account_menu_read_deadline()
            .is_some_and(|at| now >= at)
            && let Some(session) = self.autopilot_menu.as_ref().map(|menu| menu.session)
        {
            self.refresh_fact_menu(session);
        }
    }

    fn account_menu_input(&self, session: u64) -> Option<Input> {
        let entry = self.pool.get(session)?;
        let owner = entry.ctx.timeline.clone();
        let timeline = owner.lock().unwrap_or_else(|p| p.into_inner());
        let cli = timeline
            .agent()
            .reader
            .and_then(crate::harness_host::pool_cli)?;
        let pgid = timeline.agent().program_pgid;
        let birth = timeline
            .claude_footer_identity()
            .filter(|(pid, _)| *pid == pgid)
            .and_then(|(_, birth)| birth);
        let facts = timeline.claude_footer();
        let mut week = SessionWeek::default();
        let now = aterm_time::unix_secs()
            .and_then(|v| i64::try_from(v).ok())
            .unwrap_or(0);
        if cli == accounts::Cli::Claude
            && let Some(sub) = facts.and_then(|f| f.subscription.as_ref())
        {
            let walled = facts
                .and_then(|f| f.usage.as_ref())
                .and_then(|u| u.wall.as_ref())
                .is_some_and(|w| w.label == "7d");
            week.left = crate::fact_menu::weekly_facts(sub, walled, now).0;
            week.figure = crate::fact_menu::session_figure(sub, now);
            week.label.clone_from(&sub.label);
        }
        let home = aterm_primer::home_dir();
        // Only environment-to-path projection here. The platform fallback can read passwd,
        // and the test-root fallback can create directories, so both belong to the worker.
        let config = aterm_types::dirs::resolve_aterm_config_path(
            std::env::var_os("XDG_CONFIG_HOME").as_deref(),
            std::env::var_os("APPDATA").as_deref(),
            std::env::var_os("HOME").as_deref(),
        );
        let state = aterm_types::dirs::resolve_state_dir(
            aterm_types::dev_seam!("ATERM_STATE_HOME").map(PathBuf::from),
            aterm_types::dirs::StatePlatform {
                home: home.clone().filter(|path| path.is_absolute()),
                xdg_state_home: std::env::var_os("XDG_STATE_HOME").map(PathBuf::from),
                local_app_data: std::env::var_os("LOCALAPPDATA").map(PathBuf::from),
            },
        )
        .map(|root| {
            root.join("harness")
                .join(aterm_agent::harness::cli::HARNESS)
        });
        let key = Key {
            session,
            timeline: Arc::as_ptr(&owner) as usize,
            foreground: timeline.foreground_stamp(),
            pgid,
            birth,
            owner: facts.and_then(|f| f.owner.clone()),
            cli,
            account: (cli == accounts::Cli::Claude)
                .then(|| facts.and_then(|f| f.account.clone()))
                .flatten(),
            email: (cli == accounts::Cli::Claude)
                .then(|| {
                    facts
                        .and_then(|f| f.subscription.as_ref())
                        .and_then(|s| s.email.clone())
                })
                .flatten(),
            week,
            config,
            state,
            home,
        };
        drop(timeline);
        Some(Input { key, _owner: owner })
    }

    pub(crate) fn prepare_account_menu_read(&mut self, wid: WindowId, session: u64) {
        let Some(input) = self.account_menu_input(session) else {
            self.account_menu_read.close();
            return;
        };
        self.account_menu_read.request(wid, input, Instant::now());
        self.dispatch_account_menu_read();
    }

    pub(crate) fn account_menu_rows_cached(&self, session: u64) -> Option<&ResultRows> {
        let input = self.account_menu_input(session)?;
        self.account_menu_read
            .active
            .as_ref()
            .filter(|active| active.input.key == input.key)
            .and_then(|active| active.rows.as_deref())
    }

    fn dispatch_account_menu_read(&mut self) {
        if self.account_menu_read.in_flight.is_some() {
            return;
        }
        let Some((generation, input)) = self.account_menu_read.pending.take() else {
            return;
        };
        self.account_menu_read.in_flight = Some(generation);
        #[cfg(test)]
        if self.account_menu_read.hold_for_test {
            return;
        }
        let Some(proxy) = self.proxy.clone() else {
            self.on_account_menu_read(Completion {
                generation,
                rows: Err("background account delivery is unavailable".into()),
            });
            return;
        };
        let spawned = crate::qos::spawn_lane_thread(
            "aterm-account-menu",
            crate::qos::Role::Background,
            move || {
                let done = complete_read(generation, || read_rows(input));
                let _ = proxy.send_event(Wake::AccountMenuRead(done));
            },
        );
        if let Err(error) = spawned {
            self.on_account_menu_read(Completion {
                generation,
                rows: Err(format!("account reader could not start: {error}")),
            });
        }
    }

    pub(crate) fn on_account_menu_read(&mut self, completion: Completion) {
        if self.account_menu_read.in_flight != Some(completion.generation) {
            return;
        }
        self.account_menu_read.in_flight = None;
        let matching = self.account_menu_read.active.as_ref().and_then(|active| {
            let menu = self.autopilot_menu.as_ref()?;
            (active.generation == completion.generation
                && menu.wid == active.wid
                && self.windows.contains_key(&menu.wid)
                && menu.session == active.input.key.session
                && menu.owner == MenuOwner::Fact(Fact::Account))
            .then_some((
                active.wid,
                active.input.key.session,
                active.input.key.clone(),
            ))
        });
        let mut accepted = false;
        if let Some((_, session, key)) = matching
            && self
                .account_menu_input(session)
                .is_some_and(|input| input.key == key)
            && let Some(active) = self.account_menu_read.active.as_mut()
        {
            active.rows = Some(Arc::new(completion.rows));
            active.read_at = Some(Instant::now());
            accepted = true;
        }
        if let Some((wid, session)) = self.autopilot_menu.as_ref().and_then(|menu| {
            (menu.owner == MenuOwner::Fact(Fact::Account) && self.windows.contains_key(&menu.wid))
                .then_some((menu.wid, menu.session))
        }) {
            let changed = self.account_menu_read.active.as_ref().is_none_or(|active| {
                self.account_menu_input(session)
                    .is_none_or(|input| active.input.key != input.key)
            });
            if changed {
                self.prepare_account_menu_read(wid, session);
            }
            self.rebuild_fact_menu_rows(session);
            if accepted
                && std::mem::take(&mut self.account_menu_read.deferred_keyboard_selection)
                && let Some(menu) = self
                    .autopilot_menu
                    .as_mut()
                    .filter(|menu| menu.highlight.is_none())
            {
                let shown = menu
                    .rect
                    .map_or(menu.rows.len(), |rect| rect.h.saturating_sub(2));
                menu.highlight = crate::autopilot_menu::step(&menu.rows, None, true, shown);
                self.request_menu_frame(wid);
            }
        } else {
            self.account_menu_read.close();
        }
        self.dispatch_account_menu_read();
    }
}

fn read_rows(input: Input) -> ResultRows {
    let now = aterm_time::unix_secs()
        .and_then(|value| i64::try_from(value).ok())
        .ok_or("account clock is unavailable")?;
    let Key {
        cli,
        config,
        state,
        home,
        account,
        email,
        week,
        ..
    } = input.key;
    let config = config
        .or_else(aterm_types::dirs::aterm_config_path)
        .ok_or("account configuration path is unavailable")?;
    let state_dir = state
        .or_else(crate::harness_host::harness_state)
        .ok_or("account state directory is unavailable")?;
    if cli != accounts::Cli::Claude {
        let home = home.ok_or("account home is unavailable")?;
        return crate::fact_menu::pool_menu_rows(
            cli,
            (&config, &state_dir, &home),
            (account.as_deref(), email.as_deref()),
            now,
        );
    }
    let path = accounts::roster_beside(&config).ok_or("account roster path is unavailable")?;
    let roster = accounts::Roster::load(&path)?;
    let store = accounts::Store::at(&state_dir);
    let roster = store.bound(&roster, now)?;
    let (state, _) = store.load(now)?;
    let enrolled = accounts::enrolled(&roster);
    Ok(crate::fact_menu::account_menu_rows_from(
        &roster,
        &state,
        &enrolled,
        account.as_deref(),
        email.as_deref(),
        week,
        now,
    ))
}

fn complete_read(generation: u64, read: impl FnOnce() -> ResultRows) -> Completion {
    let rows = std::panic::catch_unwind(std::panic::AssertUnwindSafe(read))
        .unwrap_or_else(|_| Err("account reader stopped before completing the read".into()));
    Completion { generation, rows }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_agent::harness::footer::FooterFacts;

    fn rig() -> (App, WindowId, u64) {
        let mut app = App::headless_for_test();
        app.account_menu_read.hold_for_test = true;
        let wid = *app.windows.keys().next().unwrap();
        let session = app.focused_session_id(wid).unwrap();
        publish(&app, session, 1, "first", aterm_phase::Program::Claude);
        (app, wid, session)
    }

    fn publish(
        app: &App,
        session: u64,
        birth: u64,
        conversation: &str,
        program: aterm_phase::Program,
    ) {
        let entry = app.pool.get(session).unwrap();
        let mut timeline = entry.ctx.timeline.lock().unwrap();
        timeline.note_foreground_group(4242);
        timeline.set_agent_reader_program_for_test(Some(program));
        timeline.set_claude_footer(
            4242,
            Some(birth),
            Some(FooterFacts {
                owner: Some(FactsOwner {
                    floor: Some(birth),
                    session_id: conversation.into(),
                }),
                account: Some("fixture-account".into()),
                ..Default::default()
            }),
        );
    }

    fn open(app: &mut App, wid: WindowId, session: u64) -> u64 {
        app.open_fact_menu(wid, session, Fact::Account, (0, 0), true);
        app.account_menu_read.in_flight.unwrap()
    }

    #[test]
    fn opening_and_repainting_coalesce_until_a_controlled_completion() {
        let (mut app, wid, session) = rig();
        let generation = open(&mut app, wid, session);
        assert!(app.account_menu_rows_cached(session).is_none());
        for _ in 0..8 {
            app.refresh_fact_menu(session);
        }
        assert_eq!(app.account_menu_read.in_flight, Some(generation));
        assert!(app.account_menu_read.pending.is_none());
        app.on_account_menu_read(Completion {
            generation,
            rows: Err("fixture refusal".into()),
        });
        assert!(matches!(
            app.account_menu_rows_cached(session),
            Some(Err(_))
        ));
        assert!(app.account_menu_read.in_flight.is_none());
        assert!(app.account_menu_read_deadline().is_some());
        app.close_fact_menu();
        assert!(app.account_menu_read_deadline().is_none());
    }

    #[test]
    fn an_immediate_start_failure_is_visible_after_opening_the_menu() {
        let (mut app, wid, session) = rig();
        app.account_menu_read.hold_for_test = false;
        app.open_fact_menu(wid, session, Fact::Account, (0, 0), true);
        assert!(matches!(
            app.account_menu_rows_cached(session),
            Some(Err(_))
        ));
        assert!(app.account_menu_read.in_flight.is_none());
        assert!(app.account_menu_read_deadline().is_some());
        assert!(app.autopilot_menu.is_some());
    }

    #[test]
    fn a_closed_window_cannot_receive_a_completed_account_read() {
        let (mut app, wid, session) = rig();
        let generation = open(&mut app, wid, session);
        app.windows.remove(&wid);
        app.on_account_menu_read(Completion {
            generation,
            rows: Ok((Vec::new(), true)),
        });
        assert!(app.account_menu_read.active.is_none());
        assert!(app.account_menu_read.in_flight.is_none());
        assert!(app.account_menu_read_deadline().is_none());
    }

    #[test]
    fn closing_reopening_and_retargeting_reject_old_completions() {
        let (mut app, wid, session) = rig();
        let old = open(&mut app, wid, session);
        app.close_fact_menu();
        open(&mut app, wid, session);
        let new = app.account_menu_read.active.as_ref().unwrap().generation;
        assert_ne!(old, new);
        app.on_account_menu_read(Completion {
            generation: old,
            rows: Ok((Vec::new(), true)),
        });
        assert!(app.account_menu_rows_cached(session).is_none());
        assert_eq!(app.account_menu_read.in_flight, Some(new));
        app.open_fact_menu(wid, session, Fact::Model, (0, 0), true);
        app.on_account_menu_read(Completion {
            generation: new,
            rows: Ok((Vec::new(), true)),
        });
        assert_eq!(
            app.autopilot_menu.as_ref().unwrap().owner,
            MenuOwner::Fact(Fact::Model)
        );
        assert!(app.account_menu_read.active.is_none());
    }

    #[test]
    fn changed_process_conversation_and_provider_invalidate_pending_rows() {
        let (mut app, wid, session) = rig();
        for (birth, conversation, program) in [
            (2, "first", aterm_phase::Program::Claude),
            (2, "second", aterm_phase::Program::Claude),
            (2, "second", aterm_phase::Program::Codex),
        ] {
            let old = open(&mut app, wid, session);
            publish(&app, session, birth, conversation, program);
            app.on_account_menu_read(Completion {
                generation: old,
                rows: Ok((Vec::new(), true)),
            });
            assert!(app.account_menu_rows_cached(session).is_none());
            let current = app.account_menu_read.in_flight.unwrap();
            assert_ne!(current, old);
            app.on_account_menu_read(Completion {
                generation: current,
                rows: Ok((Vec::new(), false)),
            });
            assert!(app.autopilot_menu.as_ref().unwrap().highlight.is_none());
            assert!(matches!(
                app.account_menu_rows_cached(session),
                Some(Ok((_, false)))
            ));
        }
    }

    #[test]
    fn unchanged_owner_gets_periodic_refresh_without_discarding_usable_rows() {
        let (mut app, wid, session) = rig();
        let generation = open(&mut app, wid, session);
        app.on_account_menu_read(Completion {
            generation,
            rows: Ok((Vec::new(), true)),
        });
        let at = app
            .account_menu_read
            .active
            .as_ref()
            .unwrap()
            .read_at
            .unwrap();
        let input = app.account_menu_input(session).unwrap();
        app.account_menu_read
            .request(wid, input.clone(), at + Duration::from_secs(1));
        assert!(app.account_menu_read.pending.is_none());
        app.account_menu_read
            .request(wid, input, at + Duration::from_secs(2));
        assert!(app.account_menu_read.pending.is_some());
        assert!(app.account_menu_rows_cached(session).is_some());
    }

    #[test]
    fn account_identity_change_hides_old_choices_before_the_new_read_completes() {
        let (mut app, wid, session) = rig();
        let old = open(&mut app, wid, session);
        app.on_account_menu_read(Completion {
            generation: old,
            rows: Ok((Vec::new(), true)),
        });
        {
            let entry = app.pool.get(session).unwrap();
            let mut timeline = entry.ctx.timeline.lock().unwrap();
            let mut facts = timeline.claude_footer().unwrap().clone();
            facts.account = Some("different-account".into());
            timeline.set_claude_footer(4242, Some(1), Some(facts));
        }
        assert!(app.account_menu_rows_cached(session).is_none());
        app.refresh_fact_menu(session);
        assert!(app.account_menu_read.in_flight.is_some());
        assert!(
            app.account_menu_read
                .active
                .as_ref()
                .unwrap()
                .rows
                .is_none()
        );
        assert!(app.account_menu_read_deadline().is_none());
    }

    #[test]
    fn reader_panic_completes_the_pending_slot_and_occlusion_disarms_polling() {
        let (mut app, wid, session) = rig();
        let generation = open(&mut app, wid, session);
        app.on_account_menu_read(complete_read(generation, || {
            panic!("synthetic reader failure")
        }));
        assert!(matches!(
            app.account_menu_rows_cached(session),
            Some(Err(_))
        ));
        assert!(app.account_menu_read.in_flight.is_none());
        assert!(app.account_menu_read_deadline().is_some());
        app.windows.get_mut(&wid).unwrap().occluded = true;
        assert!(app.account_menu_read_deadline().is_none());
        app.windows.get_mut(&wid).unwrap().occluded = false;
        assert!(app.account_menu_read_deadline().is_some());
    }

    #[test]
    fn first_result_selects_for_keyboard_open_only_and_later_results_preserve_choice() {
        for keyboard in [false, true] {
            let (mut app, wid, session) = rig();
            app.open_fact_menu(wid, session, Fact::Account, (0, 0), keyboard);
            let generation = app.account_menu_read.in_flight.unwrap();
            assert!(app.autopilot_menu.as_ref().unwrap().highlight.is_none());
            app.on_account_menu_read(Completion {
                generation,
                rows: Ok((Vec::new(), true)),
            });
            let menu = app.autopilot_menu.as_mut().unwrap();
            let first = crate::autopilot_menu::step(&menu.rows, None, true, menu.rows.len());
            assert!(first.is_some());
            assert_eq!(menu.highlight, keyboard.then_some(first).flatten());
            assert!(!app.account_menu_read.deferred_keyboard_selection);
            let last = crate::autopilot_menu::step(&menu.rows, None, false, menu.rows.len());
            menu.highlight = last;
            let at = app
                .account_menu_read
                .active
                .as_ref()
                .unwrap()
                .read_at
                .unwrap();
            let input = app.account_menu_input(session).unwrap();
            app.account_menu_read
                .request(wid, input, at + Duration::from_secs(2));
            app.dispatch_account_menu_read();
            let generation = app.account_menu_read.in_flight.unwrap();
            app.on_account_menu_read(Completion {
                generation,
                rows: Ok((Vec::new(), true)),
            });
            assert_eq!(app.autopilot_menu.as_ref().unwrap().highlight, last);
        }
    }

    #[test]
    fn pointer_or_navigation_control_cancels_deferred_keyboard_selection() {
        for pointer in [false, true] {
            let (mut app, wid, session) = rig();
            let generation = open(&mut app, wid, session);
            if pointer {
                app.autopilot_menu_motion(wid, 10.0, 20.0);
            } else {
                app.autopilot_menu_nav(wid, crate::autopilot_menu::MenuKey::Down);
            }
            app.on_account_menu_read(Completion {
                generation,
                rows: Ok((Vec::new(), true)),
            });
            assert!(app.autopilot_menu.as_ref().unwrap().highlight.is_none());
            assert!(!app.account_menu_read.deferred_keyboard_selection);
        }
    }

    #[test]
    fn inert_key_and_stationary_pointer_do_not_take_keyboard_control() {
        let (mut app, wid, session) = rig();
        let generation = open(&mut app, wid, session);
        app.autopilot_menu_nav(wid, crate::autopilot_menu::MenuKey::Inert);
        app.autopilot_menu.as_mut().unwrap().rest =
            crate::autopilot_menu::PointerRest::at_open(Some((10.0, 20.0)));
        app.autopilot_menu_motion(wid, 10.0, 20.0);
        app.on_account_menu_read(Completion {
            generation,
            rows: Ok((Vec::new(), true)),
        });
        assert!(app.autopilot_menu.as_ref().unwrap().highlight.is_some());
    }
}
