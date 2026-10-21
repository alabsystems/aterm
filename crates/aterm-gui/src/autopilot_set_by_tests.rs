// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHO SET A SESSION'S AUTOPILOT (`docs/DESIGN-harness-autopilot-2026-10-15.md`
//! §11.7.5) on the App: the wire's write recorded with its writer, every
//! window surface naming itself, `meta`'s two fields, the carry across a
//! restore, and the SESSIONS row's setting line. Each test says its negative
//! control; the mutations that must fail them are recorded in the landing
//! commit.

use super::*;
use crate::autopilot_timers::{TimerChoice, set_fake_clocks};
use crate::session_timeline::{AutopilotSetBy, apply_autopilot};
use aterm_slog::Actor;
use std::time::{Duration, Instant};

/// 2026-10-16T19:00:00Z — a Friday — in unix seconds.
const FRI_19Z: i64 = 1_792_177_200;

/// A test App with a host that supervises nothing, session 0 running Claude
/// Code: what the window's surfaces need to write a word.
fn hosted() -> (App, crate::harness_host::HostHandle) {
    let mut app = App::headless_for_test();
    run_claude(&app, 0);
    let host = crate::harness_host::HostHandle::for_supervised_test(
        Vec::new(),
        std::sync::Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
    );
    app.harness = Some(host.clone());
    (app, host)
}

/// Session `session` runs Claude Code (its published program).
fn run_claude(app: &App, session: u64) {
    let ctx = app.pool.get(session).unwrap().ctx.clone();
    let mut tl = ctx.timeline.lock().unwrap();
    tl.note_foreground_group(7);
    tl.set_program(7, Some("claude".into()));
}

/// Session `s`'s keyed Autopilot records, oldest first.
fn records(app: &App, s: u64) -> Vec<(String, Vec<String>)> {
    let key = format!(
        "harness.autopilot.{}",
        app.pool.get(s).unwrap().ctx.self_id.as_str()
    );
    app.messages
        .log()
        .records()
        .filter(|r| r.key.as_deref() == Some(key.as_str()))
        .map(|r| (r.title.clone(), r.detail.clone()))
        .collect()
}

/// The last line of session `s`'s newest Autopilot record.
fn last_line(app: &App, s: u64) -> Option<String> {
    records(app, s).last()?.1.last().cloned()
}

/// Session `s`'s stored setter.
fn setter(app: &App, s: u64) -> Option<AutopilotSetBy> {
    app.pool
        .get(s)
        .unwrap()
        .ctx
        .meta
        .lock()
        .unwrap()
        .autopilot_set_by
}

/// The wire's `meta` on session `s` by `by` (an Owner client is `Ctl`), as
/// the dispatch arm runs it: the reply, and whether the stored value moved —
/// its `Wake::MetaChanged`, applied here as the event loop applies it.
fn wire_by(app: &mut App, s: u64, rest: &str, by: Actor) -> (String, bool) {
    let session = app.pool.get(s).unwrap();
    let (term, ctx) = (session.term.clone(), session.ctx.clone());
    let (reply, changed) = crate::control::cmd_meta_by(&term, &app.store, s, &ctx, rest, by);
    if changed {
        app.on_meta_changed(s);
    }
    (reply, changed)
}

/// [`wire_by`] over an Owner connection (`aterm ctl`).
fn wire(app: &mut App, s: u64, rest: &str) -> (String, bool) {
    wire_by(app, s, rest, Actor::Ctl)
}

/// The bare `meta` reply's value of `key`.
fn field(reply: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}=");
    reply
        .split_whitespace()
        .find_map(|w| w.strip_prefix(prefix.as_str()))
        .map(str::to_owned)
}

/// THE CONTROL LANE (§11.7.5): `meta set autopilot off` over an Owner
/// connection leaves the session's keyed record — none was left before
/// slice 7 — with the title a window write has and its last line `set over
/// aterm ctl`; `meta unset autopilot` records the hand back to the default
/// with the same line; aterm's harness and the fabric name themselves. A
/// session's edge connection is refused the write (the Owner rule, §11.7.6),
/// so nothing is stored or recorded from it; the setter words an edge
/// writer's sid would read are `from tab 2 (<its label>)`, and `from session
/// <short sid>` once no live session has it. NEGATIVE CONTROLS: `meta set
/// title x`, a same-word write and a second wake for the same write leave no
/// new record.
#[test]
fn a_write_over_the_wire_is_recorded_with_its_writer() {
    let (mut app, host) = hosted();
    assert!(records(&app, 0).is_empty());
    let (reply, changed) = wire(&mut app, 0, "set autopilot off");
    assert_eq!((reply.as_str(), changed), ("OK\n", true));
    let recs = records(&app, 0);
    assert_eq!(recs.len(), 1, "{recs:?}");
    assert_eq!(recs[0].0, "Autopilot set to off", "the title is unchanged");
    assert_eq!(
        recs[0].1,
        [
            "Tab 1: the harness keeps its hands off",
            "this session's own setting",
            "set over aterm ctl",
        ]
    );
    assert_eq!(
        setter(&app, 0).map(|s| (s.actor, s.surface)),
        Some((Actor::Ctl, None))
    );

    // NEGATIVE CONTROLS: another field, the same word, the same wake again.
    let (_, changed) = wire(&mut app, 0, "set title x");
    assert!(changed, "the title moved");
    assert_eq!(records(&app, 0).len(), 1, "no harness record for a title");
    let (reply, changed) = wire(&mut app, 0, "set autopilot off");
    assert_eq!((reply.as_str(), changed), ("OK\n", false));
    app.on_meta_changed(0);
    assert_eq!(records(&app, 0).len(), 1, "a same-word write records none");

    // The hand back to the default, by whoever wrote it.
    let (_, changed) = wire(&mut app, 0, "unset autopilot");
    assert!(changed);
    let (title, detail) = records(&app, 0).last().cloned().unwrap();
    assert_eq!(title, "Autopilot set to cruise");
    assert_eq!(
        detail[1..],
        [
            "back to the default, [harness] autopilot",
            "set over aterm ctl"
        ]
    );

    // aterm's harness and the fabric over the wire.
    wire_by(&mut app, 0, "set autopilot land", Actor::Harness);
    assert_eq!(
        last_line(&app, 0).as_deref(),
        Some("set by aterm's harness")
    );
    wire_by(&mut app, 0, "set autopilot off", Actor::Bridge);
    assert_eq!(last_line(&app, 0).as_deref(), Some("set over the fabric"));

    // A session's edge connection: refused, so nothing stored or recorded.
    let tok = {
        let ctx = app.pool.get(0).unwrap().ctx.clone();
        let mut edges = ctx.edges.lock().unwrap();
        edges.grant(
            aterm_session::SessionId::new("s-test-controller"),
            ctx.self_id.clone(),
            aterm_session::Op::WriteInput,
            ctx.nonce,
        )
    };
    let before = records(&app, 0).len();
    assert_eq!(
        crate::control::meta_questions_denied(
            crate::control::Scope::Edge(tok),
            "set autopilot cruise"
        ),
        Some(("ERR denied\n".to_string(), false))
    );
    assert_eq!(records(&app, 0).len(), before);

    // An edge writer's words: the second tab, by its tab and label.
    let wid = WindowId(0);
    let second = app.next_session_id;
    app.push_stub_tab(wid, crate::stub_session(second));
    let sid = app
        .pool
        .get(second)
        .unwrap()
        .ctx
        .self_id
        .as_str()
        .to_string();
    let edge = crate::session_log::sid_actor(&sid).expect("a minted sid");
    let ctx = app.pool.get(0).unwrap().ctx.clone();
    assert!(apply_autopilot(&ctx, Some("land".into()), None, edge, None).moved);
    app.on_meta_changed(0);
    let label = app.tab_label_of_session(second);
    let want = if label == "tab 2" {
        "set from tab 2".to_string()
    } else {
        format!("set from tab 2 ({label})")
    };
    assert_eq!(last_line(&app, 0), Some(want));
    // Gone: its short sid.
    let gone = aterm_session::SessionId::generate();
    let gone_actor = crate::session_log::sid_actor(gone.as_str()).expect("minted");
    assert!(apply_autopilot(&ctx, Some("off".into()), None, gone_actor, None).moved);
    app.on_meta_changed(0);
    assert_eq!(
        last_line(&app, 0),
        Some(format!(
            "set from session {}",
            aterm_messages::presence::short_sid(gone.as_str())
        ))
    );
    host.shutdown_and_join();
}

/// THE WAKE READS THE DROPPED TIMER FIRST (§11.7.3, item 3): a timed Cruise
/// armed over the wire, then `meta set autopilot land` over the wire — the
/// record says the timer it dropped is cancelled, as a window write's does,
/// and who set it. NEGATIVE CONTROL: indexing before the read (the order
/// before slice 7) loses the cancel line.
#[test]
fn a_wire_write_says_the_timer_it_dropped() {
    let (mut app, host) = hosted();
    let wall = FRI_19Z * 1000;
    set_fake_clocks(Some((Instant::now(), wall)));
    let (_, changed) = wire(&mut app, 0, "set autopilot cruise until=+1h");
    assert!(changed);
    let (title, detail) = records(&app, 0).last().cloned().unwrap();
    assert!(title.starts_with("Autopilot cruises until "), "{title}");
    assert_eq!(
        detail.last().map(String::as_str),
        Some("set over aterm ctl")
    );
    let until = app.autopilot_timers.glass(0).expect("indexed");
    let (_, changed) = wire(&mut app, 0, "set autopilot land");
    assert!(changed);
    let (title, detail) = records(&app, 0).last().cloned().unwrap();
    assert_eq!(title, "Autopilot set to land");
    assert_eq!(
        detail[2..],
        [
            format!(
                "the {} landing time is cancelled",
                crate::autopilot_menu::when(until)
            ),
            "set over aterm ctl".to_string(),
        ]
    );
    set_fake_clocks(None);
    host.shutdown_and_join();
}

/// THE SURFACES, A TABLE (§11.7.5): the chip, the composer border, the
/// Autopilot menu (a mode and its cancel), a SESSIONS row (a mode and its
/// Timer), the `Every session` row and the timer's fire each store their own
/// setter — `meta`'s word for it — and end the record with their own words.
/// NEGATIVE CONTROL: the bulk row reporting `Settings` fails its row.
#[test]
fn every_surface_names_itself() {
    use crate::autopilot_menu::{MenuAct, MenuRow};
    use crate::harness_screen::EveryPress;
    let (mut app, host) = hosted();
    let mono = Instant::now();
    let wall = FRI_19Z * 1000;
    set_fake_clocks(Some((mono, wall)));
    let wid = WindowId(0);
    // A menu row chosen as a person chooses it: opened over the chip's
    // cell, the row that acts `act`.
    let choose = move |app: &mut App, act: MenuAct| {
        app.open_autopilot_menu(wid, 0, (20, 2), false);
        let row = app
            .autopilot_menu
            .as_ref()
            .expect("open")
            .rows
            .iter()
            .position(|r| matches!(r, MenuRow::Item { act: a, .. } if *a == act))
            .unwrap_or_else(|| panic!("no {act:?} row"));
        app.choose_autopilot_menu_row(row);
    };
    // Each step: what it does, the setter's `meta` word, the record's line.
    type Step = Box<dyn Fn(&mut App)>;
    let steps: Vec<(&str, Step, &str, String)> = vec![
        (
            "the chip",
            Box::new(|app: &mut App| {
                app.cycle_session_autopilot(0);
            }),
            "human:chip",
            "set from the chip".into(),
        ),
        (
            "the composer border",
            Box::new(|app: &mut App| {
                app.toggle_session_harness(0);
            }),
            "human:border",
            "set from the composer border".into(),
        ),
        (
            "the menu's mode",
            Box::new(move |app: &mut App| choose(app, MenuAct::Mode(Autopilot::Cruise))),
            "human:menu",
            "set from the Autopilot menu".into(),
        ),
        (
            "a SESSIONS row's mode",
            Box::new(|app: &mut App| {
                app.press_page_mode(0, Autopilot::Land);
            }),
            "human:settings",
            "set from Settings \u{25b8} Harness".into(),
        ),
        (
            "the Every session row",
            Box::new(|app: &mut App| {
                app.press_autopilot_every(EveryPress::Mode(Autopilot::Cruise));
            }),
            "human:every",
            "set from Settings \u{25b8} Harness, every session".into(),
        ),
        (
            "a SESSIONS row's Timer",
            Box::new(|app: &mut App| {
                app.execute_harness_timer(crate::native_app::HarnessTimerRequest {
                    session: 0,
                    choice: TimerChoice::In(60),
                });
            }),
            "human:settings",
            "set from Settings \u{25b8} Harness".into(),
        ),
        (
            "the menu's cancel",
            Box::new(move |app: &mut App| choose(app, MenuAct::Timer(TimerChoice::Cancel))),
            "human:menu",
            "set from the Autopilot menu".into(),
        ),
    ];
    let revision = |app: &App| {
        app.pool
            .get(0)
            .unwrap()
            .ctx
            .meta
            .lock()
            .unwrap()
            .autopilot_revision
    };
    for (name, step, word, line) in &steps {
        let before = revision(&app);
        step(&mut app);
        assert_eq!(revision(&app), before + 1, "{name}: the pair moved");
        let (reply, _) = wire(&mut app, 0, "");
        assert_eq!(
            field(&reply, "autopilot_by").as_deref(),
            Some(*word),
            "{name}"
        );
        assert_eq!(
            field(&reply, "autopilot_since").as_deref(),
            Some(FRI_19Z.to_string().as_str()),
            "{name}"
        );
        assert_eq!(last_line(&app, 0).as_ref(), Some(line), "{name}");
    }

    // The timer's fire: armed from the menu, landed at its clock.
    choose(&mut app, MenuAct::Timer(TimerChoice::In(60)));
    let until = app.autopilot_timers.glass(0).expect("armed");
    let land_at = app.autopilot_timers.entry(0).unwrap().land_at_ms;
    set_fake_clocks(Some((mono + Duration::from_secs(3_600), land_at)));
    app.expire_autopilot_timers();
    let (reply, _) = wire(&mut app, 0, "");
    assert_eq!(field(&reply, "autopilot").as_deref(), Some("land"));
    assert_eq!(field(&reply, "autopilot_by").as_deref(), Some("aterm"));
    assert_eq!(
        field(&reply, "autopilot_since"),
        Some(land_at.div_euclid(1000).to_string())
    );
    assert_eq!(
        last_line(&app, 0),
        Some(format!("set at the {} timer", until.clock.as_str()))
    );
    // The menu's note and the SESSIONS row say when the word was written,
    // by its timer (§11.7.7): the fire ran at the timer's own minute here.
    assert_eq!(
        app.autopilot_set_by_words(0)
            .map(|words| words.line(None))
            .as_deref(),
        Some(format!("set {} by its timer", until.clock.as_str()).as_str())
    );
    assert_eq!(
        app.harness_state().projection().sessions[0].setting,
        format!("its own setting, set {} by its timer", until.clock.as_str())
    );
    set_fake_clocks(None);
    host.shutdown_and_join();
}

/// THE `meta` REPLY (§11.7.2): `autopilot_by=` and `autopilot_since=`
/// appended after `land_at=` — `-` and `-` for a word never set, the writer
/// and its unix second once one is, `restored` and `-` for a word a leaf
/// carried with no setter, and the setter kept beside an unset word (who
/// handed it back). Every in-tree reader keys by name: the loop's reader is
/// held to it in `aterm-agent` (`the_autopilot_word_is_read_past_who_set_it`).
/// NEGATIVE CONTROL: an `autopilot_since=` printed for a word never set
/// fails the first assertion.
#[test]
fn meta_says_who_set_the_word_and_when() {
    let (mut app, host) = hosted();
    set_fake_clocks(Some((Instant::now(), FRI_19Z * 1000 + 999)));
    let (reply, _) = wire(&mut app, 0, "");
    assert!(
        reply.ends_with(" autopilot=- land_at=- autopilot_by=- autopilot_since=-\n"),
        "{reply}"
    );
    wire(&mut app, 0, "set autopilot land");
    let (reply, _) = wire(&mut app, 0, "");
    assert!(
        reply.ends_with(&format!(
            " autopilot=land land_at=- autopilot_by=ctl autopilot_since={FRI_19Z}\n"
        )),
        "{reply}"
    );
    wire(&mut app, 0, "unset autopilot");
    let (reply, _) = wire(&mut app, 0, "");
    assert!(
        reply.ends_with(&format!(
            " autopilot=- land_at=- autopilot_by=ctl autopilot_since={FRI_19Z}\n"
        )),
        "{reply}"
    );
    // A word with no setter: carried by a leaf from before slice 7.
    {
        let ctx = app.pool.get(0).unwrap().ctx.clone();
        let mut meta = ctx.meta.lock().unwrap();
        let _ = meta.set_autopilot(Some("off".into()), None);
        meta.autopilot_set_by = None;
    }
    let (reply, _) = wire(&mut app, 0, "");
    assert!(
        reply.ends_with(" autopilot=off land_at=- autopilot_by=restored autopilot_since=-\n"),
        "{reply}"
    );
    set_fake_clocks(None);
    host.shutdown_and_join();
}

/// THE SETTER'S CARRIED FORM (§11.7.3, item 5): `<setter> <unix ms>`, read
/// back to the setter it was written from, in its one form; a setter this
/// build cannot read — an unknown surface or word, no time — is none, so
/// the word reads `restored`, never as a writer it was not. The image every
/// capture takes ([`crate::session_timeline::SessionMeta::sanitized`]) keeps
/// the setter beside a word, drops it with the word, and carries no
/// revision; a leaf's carried meta ([`crate::restore::TerminalLeafRestore`])
/// keeps it only beside a word too. The App's paths — cold, reopen, graft,
/// a driver's write winning — are `app_restore_tests`'
/// `the_setter_rides_the_cold_restore_the_reopen_and_the_graft`. NEGATIVE
/// CONTROL: an unknown word read as a sid fails the `nonsense` row.
#[test]
fn the_setter_is_carried_in_one_form_and_dropped_with_its_word() {
    use crate::autopilot::Surface;
    let since = FRI_19Z * 1000 + 250;
    for (text, want) in [
        (
            format!("human:every {since}"),
            Some((Actor::Human, Some(Surface::EverySession))),
        ),
        (
            format!("human:chip {since}"),
            Some((Actor::Human, Some(Surface::Chip))),
        ),
        (
            format!("human:border {since}"),
            Some((Actor::Human, Some(Surface::Border))),
        ),
        (
            format!("human:menu {since}"),
            Some((Actor::Human, Some(Surface::Menu))),
        ),
        (
            format!("human:settings {since}"),
            Some((Actor::Human, Some(Surface::Settings))),
        ),
        (format!("ctl {since}"), Some((Actor::Ctl, None))),
        (format!("aterm {since}"), Some((Actor::Aterm, None))),
        (format!("bridge {since}"), Some((Actor::Bridge, None))),
        (format!("harness {since}"), Some((Actor::Harness, None))),
        (format!("- {since}"), Some((Actor::Nobody, None))),
        (format!("human:pad {since}"), None),
        (format!("nonsense {since}"), None),
        ("ctl".to_string(), None),
        ("ctl soon".to_string(), None),
    ] {
        let got = AutopilotSetBy::parse_carried(&text);
        assert_eq!(got.map(|s| (s.actor, s.surface)), want, "{text}");
        if let Some(got) = got {
            assert_eq!(got.since_ms, since, "{text}");
            assert_eq!(got.carried(), text, "{text}: its one form");
        }
    }
    let sid = aterm_session::SessionId::generate();
    let carried = AutopilotSetBy::parse_carried(&format!("{} {since}", sid.as_str()))
        .expect("an edge writer's sid");
    assert_eq!(carried.word(), sid.as_str());

    // The capture's image: beside the word, dropped with it.
    let mut meta = crate::session_timeline::SessionMeta::default();
    let _ = meta.set_autopilot(Some("off".into()), None);
    meta.note_autopilot_set_by(AutopilotSetBy {
        actor: Actor::Human,
        surface: Some(Surface::Menu),
        since_ms: since,
    });
    assert_eq!(meta.autopilot_revision, 1);
    let image = meta.sanitized();
    assert_eq!(image.autopilot_set_by, meta.autopilot_set_by);
    assert_eq!(image.autopilot_revision, 0, "a revision is this process's");
    assert_eq!(image, meta, "provenance is outside equality");
    let _ = meta.set_autopilot(None, None);
    assert_eq!(
        meta.sanitized().autopilot_set_by,
        None,
        "no word, no setter"
    );

    // A leaf's carried meta: beside a word only, and only where it parses.
    let leaf = |autopilot: Option<&str>, by: &str| crate::restore::TerminalLeafRestore {
        sid: None,
        lineage: None,
        log_ep: None,
        cwd: None,
        title: String::new(),
        profile: None,
        local_id: Some(0),
        user_title: None,
        description: None,
        icon: None,
        role: None,
        attention: None,
        questions: None,
        autopilot: autopilot.map(str::to_owned),
        land_at: None,
        autopilot_by: Some(by.to_owned()),
        identity: None,
        theme: None,
        agent: None,
        supervised: None,
        lost: None,
        held: false,
        holds: Vec::new(),
    };
    assert_eq!(
        leaf(Some("off"), &format!("human:menu {since}"))
            .carried_user_meta()
            .autopilot_set_by
            .map(|s| s.word()),
        Some("human:menu".to_string())
    );
    assert_eq!(leaf(None, &format!("ctl {since}")).carried_set_by(), None);
    assert_eq!(leaf(Some("land"), "human:pad 1").carried_set_by(), None);
}

/// THE SESSIONS ROW SAYS WHO SET ITS WORD (§11.7.1, §11.7.7): on its
/// setting line under the row — `its own setting, set 14:05 over aterm
/// ctl`, the clock time, never an age, its weekday from a day before —
/// while the detail says only where it is and how the harness holds it; a
/// session handed back to the default says `follows the default` on the same
/// line. NEGATIVE CONTROL: the row with no setter composed reads `its own
/// setting` alone and fails the first assertion.
#[test]
fn a_sessions_row_says_who_set_its_word_and_when() {
    let (mut app, host) = hosted();
    let at = FRI_19Z + 5 * 60;
    set_fake_clocks(Some((Instant::now(), at * 1000)));
    wire(&mut app, 0, "set autopilot off");
    // The local clock and day, read here from the zone, not the stamp.
    let offset = crate::autopilot_timers::local_offset_at(at).expect("a zone");
    let local = at + offset;
    let clock = format!(
        "{:02}:{:02}",
        local.rem_euclid(86_400) / 3_600,
        local.rem_euclid(3_600) / 60
    );
    let row = |app: &App| app.harness_state().projection().sessions[0].clone();
    assert_eq!(
        row(&app).setting,
        format!("its own setting, set {clock} over aterm ctl"),
        "{:?}",
        row(&app)
    );
    assert!(
        row(&app).detail.ends_with("autopilot off"),
        "{}",
        row(&app).detail
    );
    // A day later the line names the day it was set.
    set_fake_clocks(Some((Instant::now(), (at + 86_400) * 1000)));
    let weekday = crate::claude_lights::Weekday::of_days(local.div_euclid(86_400)).short();
    assert_eq!(
        row(&app).setting,
        format!("its own setting, set {weekday} {clock} over aterm ctl")
    );
    // Under the default: no setter said.
    wire(&mut app, 0, "unset autopilot");
    assert_eq!(row(&app).setting, "follows the default");
    assert!(
        row(&app).detail.ends_with("autopilot cruise"),
        "{}",
        row(&app).detail
    );
    set_fake_clocks(None);
    host.shutdown_and_join();
}

/// A LATER WAKE NEVER REPLACES THE FIRE'S RECORD (§11.7.7, the review of
/// slice 7): a timer armed from the menu fires — its record says what the
/// fire found and `set at the 13:00 timer` — and the session's next wake
/// for another field (`meta set title x` over the wire) records nothing,
/// since the fire noted its revision as recorded. NEGATIVE CONTROL: the fire
/// not noting its revision lets that wake record the session again, `Autopilot
/// set to land` … `set by its timer`, over the fire's own words.
#[test]
fn a_later_wake_never_replaces_the_fires_record() {
    use crate::autopilot_menu::{MenuAct, MenuRow};
    let (mut app, host) = hosted();
    let mono = Instant::now();
    set_fake_clocks(Some((mono, FRI_19Z * 1000)));
    app.open_autopilot_menu(WindowId(0), 0, (20, 2), false);
    let row = app
        .autopilot_menu
        .as_ref()
        .expect("open")
        .rows
        .iter()
        .position(|r| {
            matches!(r, MenuRow::Item { act, .. } if *act == MenuAct::Timer(TimerChoice::In(60)))
        })
        .expect("the 1 hour row");
    app.choose_autopilot_menu_row(row);
    let until = app.autopilot_timers.glass(0).expect("armed");
    let land_at = app.autopilot_timers.entry(0).unwrap().land_at_ms;
    set_fake_clocks(Some((mono + Duration::from_secs(3_600), land_at)));
    app.expire_autopilot_timers();
    let fired = records(&app, 0).last().cloned().expect("the fire's record");
    assert_eq!(
        fired.1.last().cloned(),
        Some(format!("set at the {} timer", until.clock.as_str()))
    );
    let count = records(&app, 0).len();
    let (_, changed) = wire(&mut app, 0, "set title x");
    assert!(changed, "the title moved");
    assert_eq!(records(&app, 0).len(), count, "the wake records nothing");
    assert_eq!(
        records(&app, 0).last(),
        Some(&fired),
        "the fire's record stands"
    );
    set_fake_clocks(None);
    host.shutdown_and_join();
}
