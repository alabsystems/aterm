// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for window landing-hold display ([`HeldGlass`], [`hold_words`], [`App::shown_hold`]).

use std::sync::Arc;

use aterm_agent::harness::acts::{HoldLeg, LandHeld, OpenWord, TaskFace};

use super::{HeldGlass, HoldForm, hold_words, on_land_held};
use crate::claude_lights::{ChipGlass, Clock, Stamp};
use crate::harness_glass::PauseGlass;
use crate::harness_screen::HarnessSessionRow;
use crate::{App, WindowId};

const TODAY: i64 = 20_007;

const SINCE_S: i64 = 1_792_159_320;

fn at_1402(day: i64) -> Option<Stamp> {
    Some(Stamp {
        clock: Clock::of_minute(14 * 60 + 2),
        day,
    })
}

/// The hold §12.3.1 reads: a land act's yield, read, held by a need, two
/// unpushed commits and two open words, three nudges since the act.
pub(crate) fn held() -> LandHeld {
    LandHeld {
        since_ms: Some(SINCE_S * 1000),
        nudges: 3,
        read: true,
        legs: HoldLeg::Needs.bit() | HoldLeg::Repo.bit() | HoldLeg::Open.bit(),
        open: [
            OpenWord::of("follow-up"),
            OpenWord::of("not pushed"),
            None,
            None,
        ],
        unpushed: 2,
        ..LandHeld::default()
    }
}

/// The same hold before any yield was read: Land's instruction typed,
/// nothing judged since.
pub(crate) fn unread() -> LandHeld {
    LandHeld {
        read: false,
        legs: 0,
        open: [None; 4],
        unpushed: 0,
        ..held()
    }
}

/// `held`'s glass, its `since` at 14:02 on `day`.
pub(crate) fn glass(held: LandHeld, day: i64) -> HeldGlass {
    HeldGlass {
        held,
        since: at_1402(day),
    }
}

/// The need a hold's first leg names, as the needs store holds it: a
/// worker's words with the separators a title cuts at.
pub(crate) fn ask() -> crate::needs::HarnessAsk {
    crate::needs::HarnessAsk {
        first: "sign 0.48.0 \u{b7} the release credential \u{2014} on this Mac".to_string(),
        more: 1,
        others: Vec::new(),
        program: aterm_phase::Program::Claude,
    }
}

#[test]
fn a_holds_words_come_in_five_forms_from_one_function() {
    let hold = glass(held(), TODAY);
    let need = ask();
    let words = |form| hold_words(&hold, Some(&need), form, Some(TODAY));
    let legs = "2 unpushed commits; open work in its report: follow-up, not pushed";
    assert_eq!(
        words(HoldForm::Line).as_deref(),
        Some(
            format!(
                "not landed yet: needs you: {} (+1 more); {legs} \u{b7} landing since 14:02 \u{b7} \
                 3 nudges",
                need.first
            )
            .as_str()
        )
    );
    let title = words(HoldForm::Title).expect("a title");
    assert_eq!(
        title.split(" \u{b7} ").count(),
        2,
        "the legs, then the clock: {title}"
    );
    assert!(!title.contains(" \u{2014} "), "{title}");
    assert!(
        title.starts_with("not landed yet: needs you: sign 0.48.0, the release credential, on"),
        "{title}"
    );
    assert!(
        title.ends_with(" \u{b7} landing since 14:02, 3 nudges"),
        "{title}"
    );
    assert_eq!(
        words(HoldForm::Hover).as_deref(),
        Some(format!("not landed yet, needs you; {legs}").as_str()),
        "no clock and no need's words on the hover"
    );
    assert_eq!(
        words(HoldForm::Tip).as_deref(),
        Some(format!("not landed yet: needs you; {legs} \u{b7} 3 nudges").as_str())
    );
    let spoken = words(HoldForm::Spoken).expect("spoken");
    assert!(
        spoken
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, ' ' | '.' | ':' | ',')),
        "glyph-free: {spoken}"
    );
    assert!(spoken.starts_with("Not landed yet. Needs you: sign 0.48.0 the release credential on this Mac. And 1 more. 2 unpushed commits. Open work in its report: follow up, not pushed."), "{spoken}");
    assert!(
        spoken.ends_with(" Landing since 14:02, 3 nudges."),
        "{spoken}"
    );
    for form in [HoldForm::Title, HoldForm::Hover, HoldForm::Tip] {
        let said = words(form).expect("said");
        for foreign in [" \u{2014} ", " \u{b7} last: ", "pausing until"] {
            assert!(!said.contains(foreign), "{form:?}: {said}");
        }
    }

    let before = glass(held(), TODAY - 6);
    let day = crate::claude_lights::Weekday::of_days(TODAY - 6);
    let line = hold_words(&before, Some(&need), HoldForm::Line, Some(TODAY)).expect("a line");
    assert!(
        line.ends_with(&format!(
            " \u{b7} landing since {} 14:02 \u{b7} 3 nudges",
            day.short()
        )),
        "{line}"
    );
    let spoken = hold_words(&before, None, HoldForm::Spoken, Some(TODAY)).expect("spoken");
    assert!(
        spoken.ends_with(&format!(
            " Landing since {} at 14:02, 3 nudges.",
            day.long()
        )),
        "{spoken}"
    );

    let fresh = glass(unread(), TODAY);
    let said = |form| hold_words(&fresh, Some(&need), form, Some(TODAY));
    assert_eq!(
        said(HoldForm::Line).as_deref(),
        Some("landing since 14:02 \u{b7} 3 nudges")
    );
    assert_eq!(
        said(HoldForm::Title).as_deref(),
        Some("landing since 14:02, 3 nudges")
    );
    assert_eq!(said(HoldForm::Hover), None, "main's `land`");
    assert_eq!(said(HoldForm::Tip).as_deref(), Some("3 nudges"));
    assert_eq!(
        said(HoldForm::Spoken).as_deref(),
        Some("Landing since 14:02, 3 nudges.")
    );
    let quiet = glass(
        LandHeld {
            nudges: 0,
            ..unread()
        },
        TODAY,
    );
    assert_eq!(
        hold_words(&quiet, None, HoldForm::Line, Some(TODAY)).as_deref(),
        Some("landing since 14:02")
    );
    assert_eq!(hold_words(&quiet, None, HoldForm::Tip, Some(TODAY)), None);

    let every = glass(
        LandHeld {
            legs: every_leg(),
            changed: 1,
            stashes: 2,
            unpushed: 0,
            unlanded: 4,
            open: [None; 4],
            nudges: 1,
            ..held()
        },
        TODAY,
    );
    assert_eq!(
        hold_words(&every, None, HoldForm::Line, Some(TODAY)).as_deref(),
        Some(
            "not landed yet: a need named earlier in its reply; 1 changed file, 2 stashes, 4 \
             commits not on the remote main; open work in its report; no \u{201c}Nothing \
             left:\u{201d} defense yet; a reply too tall to read whole \u{b7} landing since 14:02 \
             \u{b7} 1 nudge"
        )
    );
    let unread = glass(
        LandHeld {
            legs: HoldLeg::Unread.bit() | HoldLeg::Defense.bit(),
            nudges: 1,
            ..held()
        },
        TODAY,
    );
    assert_eq!(
        hold_words(&unread, None, HoldForm::Line, Some(TODAY)).as_deref(),
        Some(
            "not landed yet: a repository that could not be read; no \u{201c}Nothing \
             left:\u{201d} defense yet \u{b7} landing since 14:02 \u{b7} 1 nudge"
        )
    );
    let spoken = hold_words(&unread, None, HoldForm::Spoken, Some(TODAY)).expect("spoken");
    assert!(
        spoken.contains("A repository that could not be read."),
        "{spoken}"
    );
}

/// Every leg a yield can hold at once: the repository's work or its probe
/// unread, never both ([`HoldLeg::Unread`] left out).
fn every_leg() -> u8 {
    HoldLeg::ALL
        .iter()
        .filter(|&&leg| leg != HoldLeg::Unread)
        .fold(0, |bits, leg| bits | leg.bit())
}

#[test]
fn the_open_leg_names_what_the_nudge_named_never_as_a_quote() {
    use aterm_agent::supervise::policy::turn_end::open_word_at;
    let every: Vec<&str> = (1..=u16::MAX)
        .map_while(|i| std::num::NonZeroU16::new(i).and_then(open_word_at))
        .collect();
    for want in ["unchecked box", "%", "#n", "a part", "follow-up"] {
        assert!(every.contains(&want), "{want} is one of {every:?}");
    }
    for word in every {
        let hold = glass(
            LandHeld {
                legs: HoldLeg::Open.bit(),
                open: [OpenWord::of(word), None, None, None],
                ..held()
            },
            TODAY,
        );
        for form in [
            HoldForm::Title,
            HoldForm::Line,
            HoldForm::Hover,
            HoldForm::Tip,
        ] {
            let said = hold_words(&hold, None, form, Some(TODAY)).expect("said");
            assert!(
                said.contains(&format!("open work in its report: {word}"))
                    && !said.contains(&format!("\u{201c}{word}\u{201d}")),
                "{form:?} of {word:?}: {said}"
            );
        }
        let spoken = hold_words(&hold, None, HoldForm::Spoken, Some(TODAY)).expect("spoken");
        let named = spoken
            .split_once("Open work in its report: ")
            .and_then(|(_, rest)| rest.split_once('.'))
            .map(|(named, _)| named);
        assert!(
            named.is_some_and(|n| n.chars().any(char::is_alphanumeric)),
            "{word:?} said: {spoken}"
        );
        assert!(!spoken.contains("  "), "{word:?} said: {spoken}");
    }
}

#[test]
fn the_glass_stamps_a_holds_since_once() {
    let mut app = App::headless_for_test();
    assert!(on_land_held(&mut app, 0, Some(held())));
    let first = app.harness_glass[&0];
    let stamped = first.hold().expect("held").since;
    assert!(stamped.is_some(), "stamped where it arrived");
    assert!(!on_land_held(&mut app, 0, Some(held())), "told again");
    assert_eq!(app.harness_glass[&0].trail_gen(), first.trail_gen());
    let nudged = LandHeld {
        nudges: 4,
        ..held()
    };
    let kept = HeldGlass::of(
        nudged,
        Some(HeldGlass {
            held: held(),
            since: at_1402(TODAY),
        }),
    );
    assert_eq!(kept.since, at_1402(TODAY), "the clock it was stamped with");
    assert!(on_land_held(&mut app, 0, Some(nudged)));
    assert_ne!(app.harness_glass[&0].trail_gen(), first.trail_gen());
    assert_eq!(app.harness_glass[&0].hold().expect("held").since, stamped);
    let later = LandHeld {
        since_ms: Some((SINCE_S + 3_600) * 1000),
        ..held()
    };
    let restamped = HeldGlass::of(
        later,
        Some(HeldGlass {
            held: held(),
            since: at_1402(TODAY),
        }),
    );
    assert_ne!(restamped.since, at_1402(TODAY), "read again");
    assert!(on_land_held(&mut app, 0, None));
    assert_eq!(app.harness_glass[&0].hold(), None);
    assert!(!on_land_held(&mut app, 0, None), "none stands: no move");
}

fn row(
    view: crate::autopilot::AutopilotView,
    glass: ChipGlass,
    need: Option<crate::needs::HarnessAsk>,
    hold: Option<HeldGlass>,
) -> HarnessSessionRow {
    HarnessSessionRow {
        local_id: 7,
        title: "fix the build".to_string(),
        name: "fix the build".to_string(),
        day_line: None,
        detail: String::new(),
        mode: view.mode(),
        default: view.mode(),
        view,
        glass,
        armed_at_ms: None,
        today: TODAY,
        setting: "follows the default".to_string(),
        need,
        hold,
    }
}

#[test]
fn state_line_says_why_land_holds() {
    use crate::autopilot::AutopilotView;
    let stamp = |h: u16, m: u16| Stamp {
        clock: Clock::of_minute(h * 60 + m),
        day: TODAY,
    };
    let pausing = ChipGlass {
        pause: Some(PauseGlass {
            until: stamp(18, 41),
            short: 3,
        }),
        ..ChipGlass::default()
    };
    let pause = "pausing until 18:41 (3 short turns in a row)";
    let hold = Some(glass(held(), TODAY));
    assert_eq!(
        row(AutopilotView::Land, pausing, Some(ask()), hold)
            .state_line()
            .as_deref(),
        Some(
            format!(
                "not landed yet: needs you: {} (+1 more); 2 unpushed commits; open work in \
                 its report: follow-up, not pushed \u{b7} landing since 14:02 \u{b7} 3 nudges \
                 \u{b7} {pause}",
                ask().first
            )
            .as_str()
        )
    );
    assert_eq!(
        row(
            AutopilotView::Land,
            pausing,
            Some(ask()),
            Some(glass(unread(), TODAY))
        )
        .state_line()
        .as_deref(),
        Some(
            format!(
                "needs you: {} (+1 more); carrying on \u{b7} landing since 14:02 \u{b7} 3 nudges \
                 \u{b7} {pause}",
                ask().first
            )
            .as_str()
        )
    );
    assert_eq!(
        row(AutopilotView::Land, ChipGlass::default(), None, hold)
            .state_line()
            .as_deref(),
        Some(
            "not landed yet: a need named earlier in its reply; 2 unpushed commits; open work \
             in its report: follow-up, not pushed \u{b7} landing since 14:02 \u{b7} 3 nudges"
        )
    );
    assert_eq!(
        row(AutopilotView::Cruise, pausing, None, hold)
            .state_line()
            .as_deref(),
        Some(pause)
    );
    let landed = ChipGlass {
        task: Some(TaskFace::Landed),
        task_at: Some(stamp(17, 42)),
        ..pausing
    };
    assert_eq!(
        row(AutopilotView::Land, landed, None, hold)
            .state_line()
            .as_deref(),
        Some("landed 17:42")
    );
    let theirs = ChipGlass {
        task: Some(TaskFace::Theirs),
        ..pausing
    };
    assert_eq!(
        row(AutopilotView::Land, theirs, None, hold)
            .state_line()
            .as_deref(),
        Some("quiet: a person's own exchange")
    );
}

/// The Settings ▸ Harness fixture with landings, its landing session held:
/// every leg, each count and nudges, so the state line is as tall as a hold
/// makes it (the narrow sweeps read it at 22 to 42 columns, 1× and 2×).
pub(crate) fn held_session(session: &mut crate::harness_screen::HarnessSession) {
    session.hold = Some(glass(
        LandHeld {
            legs: every_leg(),
            changed: 3,
            untracked: 1,
            stashes: 1,
            unlanded: 4,
            ..held()
        },
        0,
    ));
}

struct TestHost(crate::harness_host::HostHandle);

impl Drop for TestHost {
    fn drop(&mut self) {
        self.0.shutdown_and_join();
    }
}

fn desk(word: &str) -> (App, String, TestHost) {
    let mut app = App::headless_for_test();
    let host = TestHost(crate::harness_host::HostHandle::for_supervised_test(
        Vec::new(),
        Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
    ));
    app.harness = Some(host.0.clone());
    let ctx = app.pool.get(0).expect("session 0").ctx.clone();
    {
        let mut tl = ctx.timeline.lock().expect("timeline");
        tl.note_foreground_group(7);
        tl.set_program(7, Some("claude".into()));
    }
    ctx.meta
        .lock()
        .expect("meta")
        .set("autopilot", Some(word.into()));
    crate::session_timeline::claim_supervisor(
        &ctx,
        &crate::harness_host::holder(),
        None,
        None,
        crate::metrics::now_us(),
    )
    .expect("claimed");
    let id = app.next_session_id;
    app.push_stub_tab(WindowId(0), crate::stub_session(id));
    let sid = ctx.self_id.as_str().to_string();
    app.apply_harness_fact(&sid, crate::HarnessFact::Task(Some(TaskFace::Open)));
    (app, sid, host)
}

fn tell_hold(app: &mut App, sid: &str, held: Option<LandHeld>) {
    if crate::harness_host::store_land_held(&app.store, sid, held) {
        app.apply_harness_fact(sid, crate::HarnessFact::LandHeld);
    }
}

fn driven(app: &App) -> Option<String> {
    app.presence_hover(0, None).and_then(|hover| {
        hover
            .split(crate::native_ui::FACT_SEPARATOR)
            .flat_map(|part| part.lines())
            .find(|part| part.starts_with("driven by aterm"))
            .map(str::to_string)
    })
}

fn pushed() -> String {
    let tips = crate::work_counts::last_tooltips_pushed(WindowId(0)).expect("a stamp");
    tips.first().cloned().flatten().unwrap_or_default()
}

fn status(app: &App, key: &str) -> String {
    let line = app.session_status_record(0).expect("status");
    let prefix = format!("{key}=");
    line.split_whitespace()
        .find_map(|t| t.strip_prefix(prefix.as_str()))
        .unwrap_or_else(|| panic!("{key} in {line}"))
        .to_string()
}

fn land_fields(app: &App) -> [String; 3] {
    ["land_held", "land_nudges", "land_since"].map(|key| status(app, key))
}

fn meta(app: &mut App, rest: &str) {
    let session = app.pool.get(0).expect("session 0");
    let (term, ctx) = (session.term.clone(), session.ctx.clone());
    let (reply, changed) = crate::control::cmd_meta(&term, &app.store, 0, &ctx, rest);
    assert!(reply.starts_with("OK"), "{reply}");
    if changed {
        app.on_meta_changed(0);
    }
}

const WHY: &str = "driven by aterm (land: not landed yet, needs you; 2 unpushed commits; open \
                   work in its report: follow-up, not pushed)";

#[test]
fn who_drives_says_why_a_landing_holds() {
    let (mut app, sid, _host) = desk("land");
    assert_eq!(driven(&app).as_deref(), Some("driven by aterm (land)"));
    tell_hold(&mut app, &sid, Some(unread()));
    assert_eq!(
        driven(&app).as_deref(),
        Some("driven by aterm (land)"),
        "no yield read yet"
    );
    tell_hold(&mut app, &sid, Some(held()));
    assert_eq!(driven(&app).as_deref(), Some(WHY));
    assert!(!WHY.contains("14:"), "no clock on the hover");
    tell_hold(&mut app, &sid, None);
    assert_eq!(driven(&app).as_deref(), Some("driven by aterm (land)"));
}

#[test]
fn the_hover_follows_the_hold_and_the_face() {
    let (mut app, sid, _host) = desk("land");
    let _ = app.refresh_window_tabs(WindowId(0));
    assert!(pushed().contains("driven by aterm (land)"), "{}", pushed());
    tell_hold(&mut app, &sid, Some(held()));
    assert!(pushed().contains(WHY), "{}", pushed());
    let line = app.stripless_tab_chrome().expect("the tab line");
    let (_, as_pushed) = line
        .split_once(" pushed_tooltips=")
        .expect("a dev build prints the hovers pushed");
    assert!(as_pushed.contains(WHY), "{line}");
    let nudged = LandHeld {
        nudges: 4,
        ..held()
    };
    tell_hold(&mut app, &sid, Some(nudged));
    assert!(pushed().contains(WHY), "the same words: {}", pushed());
    app.apply_harness_fact(&sid, crate::HarnessFact::Task(Some(TaskFace::Landed)));
    assert!(
        pushed().contains("driven by aterm (land, landed)"),
        "{}",
        pushed()
    );
    assert!(!pushed().contains("not landed yet"), "{}", pushed());
}

#[test]
fn a_nudge_the_hover_does_not_say_still_rebuilds_the_glance() {
    let (mut app, sid, _host) = desk("land");
    tell_hold(&mut app, &sid, Some(held()));
    let _ = app.take_status_rebuild();
    let _ = crate::work_counts::take_hover_redraws();
    let hover = driven(&app);
    let nudged = LandHeld {
        nudges: 4,
        ..held()
    };
    tell_hold(&mut app, &sid, Some(nudged));
    assert_eq!(driven(&app), hover, "the hover says no nudge");
    assert_eq!(
        crate::work_counts::take_hover_redraws(),
        0,
        "no strip stamped"
    );
    assert!(app.take_status_rebuild(), "the glance owes a rebuild");
    let handle = app.store.read().unwrap().by_local(0).cloned().expect("0");
    assert!(
        app.status_session_row(&handle)
            .landing_tip
            .is_some_and(|tip| tip.ends_with(" \u{b7} 4 nudges")),
        "{:?}",
        app.status_session_row(&handle).landing_tip
    );
    assert!(!app.take_status_rebuild(), "and only once");
}

#[test]
fn a_hold_is_shown_only_while_the_word_is_land() {
    let (mut app, sid, _host) = desk("land");
    tell_hold(&mut app, &sid, Some(held()));
    assert!(app.shown_hold(0).is_some());
    assert_eq!(
        land_fields(&app),
        ["needs,repo,open", "3", &SINCE_S.to_string()]
    );
    let handle = app.store.read().unwrap().by_local(0).cloned().expect("0");
    assert!(app.status_session_row(&handle).landing);
    meta(&mut app, "set autopilot cruise");
    assert_eq!(app.shown_hold(0), None, "hidden under Cruise");
    assert_eq!(driven(&app).as_deref(), Some("driven by aterm (cruise)"));
    assert_eq!(land_fields(&app), ["-", "-", "-"]);
    assert!(!app.status_session_row(&handle).landing);
    meta(&mut app, "set autopilot land");
    assert!(app.shown_hold(0).is_some(), "the glass kept it");
    assert_eq!(driven(&app).as_deref(), Some(WHY));
}

fn on_timeline(app: &App) -> Option<LandHeld> {
    app.pool
        .get(0)
        .expect("0")
        .ctx
        .timeline
        .lock()
        .expect("timeline")
        .land_held()
}

#[test]
fn a_persons_own_exchange_clears_the_hold() {
    let (mut app, sid, _host) = desk("land");
    tell_hold(&mut app, &sid, Some(held()));
    assert_eq!(on_timeline(&app), Some(held()));
    app.apply_harness_fact(&sid, crate::HarnessFact::Task(Some(TaskFace::Theirs)));
    assert_eq!(app.harness_glass[&0].hold(), None, "off the glass");
    assert_eq!(on_timeline(&app), None, "off the timeline");
}

fn prove_self_claim(app: &App) {
    let ctx = app.pool.get(0).expect("0").ctx.clone();
    assert!(crate::session_timeline::release_supervisor(
        &ctx,
        None,
        crate::metrics::now_us(),
        aterm_slog::Actor::Ctl
    ));
    crate::session_timeline::claim_supervisor_as(
        &ctx,
        &format!("codex-alab@{}", std::process::id()),
        Some(1),
        None,
        Some(crate::self_claim::stand_in::running()),
        crate::metrics::now_us(),
        aterm_slog::Actor::Ctl,
    )
    .expect("a proof takes a free session");
    assert!(app.beside_proven_self_claim(0));
}

#[test]
fn a_proven_self_claim_clears_a_standing_hold() {
    let (mut app, sid, _host) = desk("land");
    tell_hold(&mut app, &sid, Some(held()));
    prove_self_claim(&app);
    let late = LandHeld {
        nudges: 4,
        ..held()
    };
    tell_hold(&mut app, &sid, Some(late));
    assert_eq!(app.harness_glass[&0].hold(), None, "never shown beside it");
    assert_eq!(on_timeline(&app), None, "nor kept for `status`");

    let (mut app, sid, _host) = desk("land");
    tell_hold(&mut app, &sid, Some(held()));
    prove_self_claim(&app);
    app.apply_harness_fact(&sid, crate::HarnessFact::Task(Some(TaskFace::Open)));
    assert_eq!(app.harness_glass[&0].hold(), None, "the hosted loop's face");
    assert_eq!(on_timeline(&app), None);

    let (mut app, sid, _host) = desk("land");
    tell_hold(&mut app, &sid, Some(held()));
    prove_self_claim(&app);
    app.pool
        .get(0)
        .expect("0")
        .ctx
        .timeline
        .lock()
        .unwrap()
        .queue_self_task(TaskFace::Open);
    app.apply_harness_fact(&sid, crate::HarnessFact::SelfTask(TaskFace::Open));
    assert_eq!(app.harness_glass[&0].hold(), None, "the proof's own act");
    assert_eq!(on_timeline(&app), None);
}

#[test]
fn status_prints_land_fields_only_where_claimed_here() {
    let (mut app, sid, _host) = desk("land");
    assert_eq!(land_fields(&app), ["-", "-", "-"]);
    tell_hold(&mut app, &sid, Some(unread()));
    assert_eq!(land_fields(&app), ["-", "3", &SINCE_S.to_string()]);
    tell_hold(&mut app, &sid, Some(held()));
    assert_eq!(
        land_fields(&app),
        ["needs,repo,open", "3", &SINCE_S.to_string()]
    );
    assert_eq!(status(&app, "pause"), "-");
    assert_eq!(status(&app, "task"), "open");
    let ctx = app.pool.get(0).expect("0").ctx.clone();
    assert!(crate::session_timeline::release_supervisor(
        &ctx,
        None,
        crate::metrics::now_us(),
        aterm_slog::Actor::Ctl
    ));
    assert_eq!(land_fields(&app), ["-", "-", "-"], "no claim here");
    crate::session_timeline::claim_supervisor(
        &ctx,
        "aterm-harness@1",
        None,
        None,
        crate::metrics::now_us(),
    )
    .expect("another process claims it");
    assert_eq!(land_fields(&app), ["-", "-", "-"], "another's claim");

    let (mut app, sid, _host) = desk("land");
    tell_hold(&mut app, &sid, Some(held()));
    app.apply_harness_fact(&sid, crate::HarnessFact::Task(Some(TaskFace::Landed)));
    assert_eq!(land_fields(&app), ["-", "-", "-"], "landed");
}

#[test]
fn a_worker_reads_landing_only_after_its_instruction() {
    use crate::status_item::{Glance, SessionRow, TaskRow, compose_status_menu, menu_lines};
    use crate::task_record::TaskState;
    const NOW: i64 = 1_800_000_000;
    let worker = |id: u64, label: &str, landing: bool| SessionRow {
        id,
        sid: format!("s-{id:020}"),
        title: label.to_string(),
        agent: "busy",
        task: Some(TaskRow {
            state: TaskState::Working,
            why: None,
            from: "s-manager".to_string(),
            entered: 7,
            since_unix_ms: u64::try_from(NOW - 4 * 60).expect("positive") * 1000,
            result_bytes: 0,
            result_path: None,
            seen: false,
        }),
        landing,
        landing_tip: landing.then(|| "not landed yet: 2 unpushed commits \u{b7} 3 nudges".into()),
        ..SessionRow::default()
    };
    let manager = SessionRow {
        id: 1,
        sid: "s-manager".to_string(),
        title: "lab-sweep".to_string(),
        agent: "busy",
        ..SessionRow::default()
    };
    let rows = [manager, worker(2, "bench", true), worker(3, "lint", false)];
    let glance = Glance::build(
        &rows,
        &[],
        None,
        u64::try_from(NOW).expect("positive") * 1000,
        None,
    );
    let lines = menu_lines(&compose_status_menu(&glance, NOW), &[]);
    let line = |words: &str| {
        let found: Vec<&String> = lines.iter().filter(|l| l.contains(words)).collect();
        assert_eq!(found.len(), 1, "{words}: {lines:#?}");
        found[0].clone()
    };
    let bench = line("bench \u{2014}");
    assert!(
        bench.contains("\u{25cf} bench \u{2014} landing 4m"),
        "{bench}"
    );
    assert!(
        bench.contains("tip=\"not landed yet: 2 unpushed commits \u{b7} 3 nudges\""),
        "{bench}"
    );
    let lint = line("lint \u{2014}");
    assert!(lint.contains("\u{25cf} lint \u{2014} working 4m"), "{lint}");
    assert!(!lint.contains("tip="), "{lint}");
    assert!(
        line("lab-sweep \u{2014}").contains("2 workers: 1 landing, 1 working"),
        "{lines:#?}"
    );

    let (mut app, sid, _host) = desk("land");
    let handle = app.store.read().unwrap().by_local(0).cloned().expect("0");
    let row = app.status_session_row(&handle);
    assert!(!row.landing, "before Land's instruction");
    tell_hold(&mut app, &sid, Some(held()));
    let row = app.status_session_row(&handle);
    assert!(row.landing);
    assert_eq!(
        row.landing_tip.as_deref(),
        Some(
            "not landed yet: needs you; 2 unpushed commits; open work in its report: \
             follow-up, not pushed \u{b7} 3 nudges"
        )
    );
}

#[test]
fn the_page_says_why_as_the_hold_moves() {
    let (mut app, sid, _host) = desk("land");
    let line = |app: &App| {
        let instance = app
            .native_runtime
            .instance_by_kind(crate::native_app::AppKind::Settings)
            .expect("the Settings controller");
        let Some(crate::native_app::NativeApp::Settings(settings)) =
            app.native_runtime.app(instance)
        else {
            unreachable!("a Settings instance")
        };
        settings.harness_snapshot().projection().sessions[0].state_line()
    };
    assert!(app.open_harness_settings(WindowId(0), crate::harness_screen::HarnessFocus::Top));
    assert!(app.harness_page_shown());
    assert!(
        !line(&app).unwrap_or_default().contains("landing since"),
        "{:?}",
        line(&app)
    );
    tell_hold(&mut app, &sid, Some(held()));
    let said = line(&app).expect("a line");
    assert!(
        said.starts_with("not landed yet: a need named earlier in its reply; 2 unpushed"),
        "{said}"
    );
    assert!(said.contains(" \u{b7} landing since "), "{said}");
    assert!(said.ends_with(" \u{b7} 3 nudges"), "{said}");
}

#[test]
fn a_gone_sessions_hold_goes_with_its_glass() {
    let (mut app, _sid, _host) = desk("land");
    let second = app.next_session_id - 1;
    assert!(on_land_held(&mut app, second, Some(held())));
    app.retire_session_registration(second);
    assert!(
        !app.harness_glass.contains_key(&second),
        "the glass and its hold went"
    );
}

#[test]
fn a_land_hold_does_not_grow_the_wake() {
    const MAIN_FACT_BYTES: usize = 24;
    /// Windows' own `Wake` variants make it 4160 bytes there (the Windows lane, aarch64).
    const MAIN_WAKE_BYTES: usize = if cfg!(windows) { 4160 } else { 4144 };
    fn copy<T: Copy>() {}
    copy::<crate::HarnessFact>();
    assert_eq!(std::mem::size_of::<crate::HarnessFact>(), MAIN_FACT_BYTES);
    assert!(std::mem::size_of::<crate::Wake>() <= MAIN_WAKE_BYTES);
}
