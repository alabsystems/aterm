// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for window landing-hold display ([`HeldGlass`], [`hold_words`], [`App::shown_hold`]),
//! and for the stop and the outage wait beside it ([`App::shown_stop`], [`App::shown_wait`]).

use std::sync::Arc;

use aterm_agent::harness::acts::{HoldLeg, LandHeld, OpenWord, TaskFace};

use super::{HeldGlass, HoldForm, ShownHold, hold_words, on_land_held};
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
        tail: crate::needs::NeedTail::CarryingOn,
        entry_first: None,
    }
}

#[test]
fn a_holds_words_come_in_five_forms_from_one_function() {
    let hold = glass(held(), TODAY);
    let need = ask();
    let words = |form| hold_words(&hold, Some(&need), form, Some(TODAY), None);
    let legs = "2 unpushed commits; open in its report: follow-up, not pushed";
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
        Some(format!("not landed yet, {legs}").as_str()),
        "no clock and no need on the hover: the need's own line stands above it"
    );
    assert_eq!(
        words(HoldForm::Tip).as_deref(),
        Some(format!("not landed yet: {legs} \u{b7} 3 nudges").as_str()),
        "no need on a tip: the need's own line stands above it"
    );
    let spoken = words(HoldForm::Spoken).expect("spoken");
    assert!(
        spoken
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, ' ' | '.' | ':' | ',')),
        "glyph-free: {spoken}"
    );
    assert!(spoken.starts_with("Not landed yet. Needs you: sign 0.48.0 the release credential on this Mac. And 1 more. 2 unpushed commits. Open in its report: follow up, not pushed."), "{spoken}");
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
    let line = hold_words(&before, Some(&need), HoldForm::Line, Some(TODAY), None).expect("a line");
    assert!(
        line.ends_with(&format!(
            " \u{b7} landing since {} 14:02 \u{b7} 3 nudges",
            day.short()
        )),
        "{line}"
    );
    let spoken = hold_words(&before, None, HoldForm::Spoken, Some(TODAY), None).expect("spoken");
    assert!(
        spoken.ends_with(&format!(
            " Landing since {} at 14:02, 3 nudges.",
            day.long()
        )),
        "{spoken}"
    );

    let fresh = glass(unread(), TODAY);
    let said = |form| hold_words(&fresh, Some(&need), form, Some(TODAY), None);
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
        hold_words(&quiet, None, HoldForm::Line, Some(TODAY), None).as_deref(),
        Some("landing since 14:02")
    );
    assert_eq!(
        hold_words(&quiet, None, HoldForm::Tip, Some(TODAY), None),
        None
    );

    let every = glass(
        LandHeld {
            legs: every_leg(),
            changed: 1,
            stashes: 2,
            unpushed: 0,
            unlanded: 4,
            left: 2,
            open: [None; 4],
            nudges: 1,
            ..held()
        },
        TODAY,
    );
    assert_eq!(
        hold_words(&every, None, HoldForm::Line, Some(TODAY), None).as_deref(),
        Some(
            "not landed yet: a need named earlier in its reply; 1 changed file, 2 stashes, 4 \
             commits not on the remote main; 2 processes left running; what it listed as left; \
             no \u{201c}Nothing left:\u{201d} defense yet; a reply too tall to read whole \u{b7} \
             landing since 14:02 \u{b7} 1 nudge"
        )
    );
    let spoken = hold_words(&every, None, HoldForm::Spoken, Some(TODAY), None).expect("spoken");
    assert!(spoken.contains("2 processes left running."), "{spoken}");
    let unread = glass(
        LandHeld {
            legs: HoldLeg::Unread.bit() | HoldLeg::Defense.bit(),
            nudges: 1,
            ..held()
        },
        TODAY,
    );
    assert_eq!(
        hold_words(&unread, None, HoldForm::Line, Some(TODAY), None).as_deref(),
        Some(
            "not landed yet: a repository that could not be read; no \u{201c}Nothing \
             left:\u{201d} defense yet \u{b7} landing since 14:02 \u{b7} 1 nudge"
        )
    );
    let spoken = hold_words(&unread, None, HoldForm::Spoken, Some(TODAY), None).expect("spoken");
    assert!(
        spoken.contains("A repository that could not be read."),
        "{spoken}"
    );
}

/// A HOLD ON WORK OFF `HEAD` NAMES ITS COUNTS (R-7): a repository leg held only by a
/// submodule's commits on no remote or by commits parked on another branch the session used says
/// those counts. CONTROL: a repository leg with no count says `work no remote main has`.
#[test]
fn a_hold_on_work_off_head_names_its_counts() {
    let line = |sub_unpushed: u16, parked: u16| {
        let only = glass(
            LandHeld {
                legs: HoldLeg::Repo.bit(),
                unpushed: 0,
                sub_unpushed,
                parked,
                ..held()
            },
            TODAY,
        );
        hold_words(&only, None, HoldForm::Line, Some(TODAY), None).expect("a line")
    };
    let tail = " \u{b7} landing since 14:02 \u{b7} 3 nudges";
    assert_eq!(
        line(1, 2),
        format!(
            "not landed yet: 1 submodule commit on no remote, 2 commits parked on other \
             branches{tail}"
        )
    );
    assert_eq!(
        line(3, 1),
        format!(
            "not landed yet: 3 submodule commits on no remote, 1 commit parked on another \
             branch{tail}"
        )
    );
    assert_eq!(
        line(0, 0),
        format!("not landed yet: work no remote main has{tail}")
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
            let said = hold_words(&hold, None, form, Some(TODAY), None).expect("said");
            assert!(
                said.contains(&format!("open in its report: {word}"))
                    && !said.contains(&format!("\u{201c}{word}\u{201d}")),
                "{form:?} of {word:?}: {said}"
            );
        }
        let spoken = hold_words(&hold, None, HoldForm::Spoken, Some(TODAY), None).expect("spoken");
        let named = spoken
            .split_once("Open in its report: ")
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
        stopped: None,
        waiting: None,
        switch_asked: false,
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
        hold: hold.map(|hold| ShownHold {
            hold,
            by: None,
            said: None,
        }),
        receipt: None,
        token: None,
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
                "not landed yet: needs you: {} (+1 more); 2 unpushed commits; open in its \
                 report: follow-up, not pushed \u{b7} landing since 14:02 \u{b7} 3 nudges \
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
            "not landed yet: a need named earlier in its reply; 2 unpushed commits; open in \
             its report: follow-up, not pushed \u{b7} landing since 14:02 \u{b7} 3 nudges"
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
        row(AutopilotView::Land, landed, None, hold).state_line(),
        Some(format!(
            "landed 17:42{}{}",
            crate::native_ui::FACT_SEPARATOR,
            aterm_agent::supervise::mode_words::NO_RECORD
        ))
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
    session.hold = Some(ShownHold {
        hold: glass(
            LandHeld {
                legs: every_leg(),
                changed: 3,
                untracked: 1,
                stashes: 1,
                unlanded: 4,
                ..held()
            },
            0,
        ),
        by: None,
        said: None,
    });
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
            .find(|part| {
                part.starts_with("driven by aterm") || part.starts_with("codex-alab autopilot")
            })
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

/// The hover of [`held`]: its need is said once, by the need's own line, not again here.
const WHY: &str = "driven by aterm (land: not landed yet, 2 unpushed commits; open in its report: follow-up, \
     not pushed)";

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
            finished: false,
            own_report: false,
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
            "not landed yet: 2 unpushed commits; open in its report: follow-up, not pushed \
             \u{b7} 3 nudges"
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

/// The words a leg that names nothing of its reply says, and a read hold's tip, are the ones
/// beside `HoldLeg` in aterm-agent, where the forever lab reads them to judge a hold: every
/// form the glass draws says them, so the lab never holds a copy of its own.
#[test]
fn a_fixed_legs_words_and_the_tip_are_the_ones_beside_the_leg() {
    use aterm_agent::harness::acts::{NOT_LANDED_YET, held_tip};
    for leg in [HoldLeg::Defense, HoldLeg::Unread, HoldLeg::Cut] {
        let words = leg.fixed_words().expect("a fixed leg's words");
        for nudges in [0, 1, 3] {
            let hold = glass(
                LandHeld {
                    legs: leg.bit(),
                    nudges,
                    ..held()
                },
                TODAY,
            );
            let said = |form| hold_words(&hold, None, form, Some(TODAY), None);
            assert_eq!(
                said(HoldForm::Tip),
                Some(held_tip(words, nudges)),
                "{leg:?}"
            );
            assert_eq!(
                said(HoldForm::Hover),
                Some(format!("{NOT_LANDED_YET}, {words}")),
                "{leg:?}"
            );
            for form in [HoldForm::Line, HoldForm::Title] {
                let line = said(form).expect("a read hold's line");
                assert!(
                    line.starts_with(&format!("{NOT_LANDED_YET}: {words}")),
                    "{leg:?} {form:?}: {line}"
                );
            }
        }
    }
    for leg in [HoldLeg::Needs, HoldLeg::Repo, HoldLeg::Open] {
        assert_eq!(leg.fixed_words(), None, "{leg:?} names what its reply held");
    }
}

/// Tab `sid`'s loop tells the exception standing at its point (`None`: its hold lifted).
fn tell_stop(
    app: &mut App,
    sid: &str,
    standing: Option<aterm_agent::supervise::policy::exception::Exception>,
) {
    app.note_harness_host(crate::harness_host::HostNote::ResumeException {
        sid: sid.to_string(),
        agent: aterm_phase::Program::Claude,
        standing,
    });
}

/// Session 0's chip title as its hover draws it under `view`, its spoken description, the
/// Settings page's state line, the tab's hover, the worker row's tip and `status held=`.
fn stop_surfaces(app: &mut App, view: crate::autopilot::AutopilotView) -> [String; 6] {
    let chip = ChipGlass::default();
    let title = crate::claude_lights::title_parts(
        &app.harness_glass,
        &app.hold_said,
        &app.pool,
        0,
        &(view, chip),
    )
    .whole;
    let (_, spoken) = app.autopilot_spoken(0, view, &chip);
    if !app.harness_page_shown() {
        assert!(app.open_harness_settings(WindowId(0), crate::harness_screen::HarnessFocus::Top));
    }
    let instance = app
        .native_runtime
        .instance_by_kind(crate::native_app::AppKind::Settings)
        .expect("the Settings controller");
    let Some(crate::native_app::NativeApp::Settings(settings)) = app.native_runtime.app(instance)
    else {
        unreachable!("a Settings instance")
    };
    let line = settings.harness_snapshot().projection().sessions[0].state_line();
    let handle = app.store.read().unwrap().by_local(0).cloned().expect("0");
    let tip = app.status_session_row(&handle).landing_tip;
    [
        title,
        spoken.unwrap_or_default(),
        line.unwrap_or_default(),
        driven(app).unwrap_or_default(),
        tip.unwrap_or_default(),
        status(app, "held"),
    ]
}

#[test]
fn a_stopped_tab_says_so_wherever_its_autopilot_is_read_until_the_hold_lifts() {
    use crate::autopilot::AutopilotView;
    use crate::harness_glass::stop_words;
    use aterm_agent::harness::acts::{HeldBy, STOPPED_TYPING};
    use aterm_agent::supervise::policy::exception::Exception;
    let sep = crate::native_ui::FACT_SEPARATOR;
    for held_by in HeldBy::ALL {
        {
            let standing = Exception::repeats(held_by);
            let case = format!("{held_by:?} {:?}", standing.when);
            let (mut app, sid, _host) = desk("land");
            tell_hold(&mut app, &sid, Some(held()));
            app.apply_harness_fact(
                &sid,
                crate::HarnessFact::Pause(Some(crate::PausePlan {
                    at_unix: SINCE_S + 240,
                    short: 2,
                })),
            );
            let pause = app
                .harness_glass
                .get(&0)
                .and_then(|g| g.pause)
                .expect("CONTROL: the pause on the glass");
            let before = stop_surfaces(&mut app, AutopilotView::Land);
            let paused = pause.state_words(Some(crate::harness_glass::wall_today().1));
            assert!(before[2].contains(&paused), "{case}: CONTROL: {before:#?}");
            assert!(
                before.iter().all(|s| !s.contains(STOPPED_TYPING)),
                "{case}: CONTROL: {before:#?}"
            );
            assert_eq!(before[5], "-", "{case}");
            tell_stop(&mut app, &sid, Some(standing));
            let [title, spoken, line, hover, tip, field] =
                stop_surfaces(&mut app, AutopilotView::Land);
            assert!(
                title.starts_with(&format!(
                    "autopilot: land \u{2014} {}{sep}not landed yet",
                    stop_words(held_by, HoldForm::Title, false)
                )),
                "{case}: {title}"
            );
            assert!(
                !title.contains("last: "),
                "{case}: the trail gives way: {title}"
            );
            assert!(
                spoken.starts_with(&stop_words(held_by, HoldForm::Spoken, false)),
                "{case}: {spoken}"
            );
            assert!(
                line.starts_with(&format!(
                    "{}{sep}not landed yet: ",
                    stop_words(held_by, HoldForm::Line, false)
                )),
                "{case}: {line}"
            );
            assert!(
                !line.contains(&paused),
                "{case}: the lapsed pause goes: {line}"
            );
            assert!(
                hover.starts_with(&format!(
                    "driven by aterm (land: {}; not landed yet, ",
                    stop_words(held_by, HoldForm::Hover, false)
                )),
                "{case}: {hover}"
            );
            assert!(
                tip.starts_with(&format!(
                    "{}{sep}not landed yet: ",
                    stop_words(held_by, HoldForm::Tip, false)
                )),
                "{case}: {tip}"
            );
            assert_eq!(field, held_by.word(), "{case}");
            tell_stop(&mut app, &sid, None);
            let after = stop_surfaces(&mut app, AutopilotView::Land);
            assert!(
                after.iter().all(|s| !s.contains(STOPPED_TYPING)),
                "{case}: the lift takes every stop word: {after:#?}"
            );
            assert_eq!(after[5], "-", "{case}");
        }
    }
}

#[test]
fn a_stopped_cruise_says_the_stop_in_place_of_its_promise() {
    use crate::autopilot::{AutopilotView, mode_words};
    use crate::harness_glass::stop_words;
    use aterm_agent::harness::acts::{HeldBy, STOPPED_TYPING};
    use aterm_agent::supervise::Autopilot;
    use aterm_agent::supervise::policy::exception::Exception;
    let (mut app, sid, _host) = desk("cruise");
    let promise = mode_words(Autopilot::Cruise).chip;
    let before = stop_surfaces(&mut app, AutopilotView::Cruise);
    assert!(before[0].contains(promise), "CONTROL: {before:#?}");
    tell_stop(
        &mut app,
        &sid,
        Some(Exception::repeats_by(HeldBy::SameReply, true)),
    );
    let [title, _, line, hover, _, field] = stop_surfaces(&mut app, AutopilotView::Cruise);
    assert!(
        title.starts_with(&format!(
            "autopilot: cruise \u{2014} {}",
            stop_words(HeldBy::SameReply, HoldForm::Title, true)
        )),
        "{title}"
    );
    assert!(!title.contains(promise), "{title}");
    assert!(
        line.starts_with(&stop_words(HeldBy::SameReply, HoldForm::Line, true)),
        "{line}"
    );
    assert_eq!(
        hover,
        format!(
            "driven by aterm (cruise: {})",
            stop_words(HeldBy::SameReply, HoldForm::Hover, true)
        )
    );
    assert_eq!(field, HeldBy::SameReply.word());
    // A Cruise worker is never landing, and its row in the menu bar says the stop all the same.
    {
        use crate::status_item::{Glance, SessionRow, TaskRow, compose_status_menu, menu_lines};
        const NOW: i64 = 1_800_000_000;
        let handle = app.store.read().unwrap().by_local(0).cloned().expect("0");
        let worker = SessionRow {
            task: Some(TaskRow {
                state: crate::task_record::TaskState::Working,
                why: None,
                from: "s-manager".to_string(),
                entered: 7,
                since_unix_ms: u64::try_from(NOW - 4 * 60).expect("positive") * 1000,
                result_bytes: 0,
                result_path: None,
                seen: false,
                finished: false,
                own_report: false,
            }),
            ..app.status_session_row(&handle)
        };
        assert!(!worker.landing, "CONTROL: Cruise does not land");
        let manager = SessionRow {
            id: 99,
            sid: "s-manager".to_string(),
            title: "lab-sweep".to_string(),
            agent: "busy",
            ..SessionRow::default()
        };
        let glance = Glance::build(
            &[manager, worker],
            &[],
            None,
            u64::try_from(NOW).expect("positive") * 1000,
            None,
        );
        let lines = menu_lines(&compose_status_menu(&glance, NOW), &[]);
        let tip = format!(
            "tip=\"{}\"",
            stop_words(HeldBy::SameReply, HoldForm::Tip, true)
        );
        assert_eq!(
            lines.iter().filter(|l| l.contains(&tip)).count(),
            1,
            "the worker's row says the stop: {lines:#?}"
        );
    }
    meta(&mut app, "set autopilot off");
    let off = stop_surfaces(&mut app, AutopilotView::Off);
    assert!(
        off[..4].iter().all(|s| !s.contains(STOPPED_TYPING)),
        "Off types nothing, so no stop is said: {off:#?}"
    );
}

/// THE STOP NAMES THE PERSON'S SWITCH TO LAND ONLY WHERE IT ASKS (v6 §14.17 item 3): under
/// Cruise, over a hold whose exception the switch clears (`Exception::lifted_by_switch`), the
/// chip's title and description and the state line say `your switch to Land`. NEGATIVE CONTROLS:
/// the same hold under Land, where there is no switch to make, and under Cruise a hold whose
/// exception the switch does not clear (a wall's, or one whose ask was spent), say only the
/// person's turn and the agent's work.
#[test]
fn the_stop_names_the_switch_to_land_only_under_cruise_where_it_asks() {
    use crate::autopilot::AutopilotView;
    use aterm_agent::harness::acts::{HeldBy, TYPES_AGAIN, TYPES_AGAIN_OR_SWITCH};
    use aterm_agent::supervise::policy::exception::Exception;
    let switch = "your switch to Land";
    let (mut app, sid, _host) = desk("cruise");
    tell_stop(
        &mut app,
        &sid,
        Some(Exception::repeats_by(HeldBy::SameReply, true)),
    );
    let [title, spoken, line, ..] = stop_surfaces(&mut app, AutopilotView::Cruise);
    for said in [&title, &spoken, &line] {
        assert!(said.contains(TYPES_AGAIN_OR_SWITCH), "{said}");
    }
    tell_stop(&mut app, &sid, Some(Exception::repeats(HeldBy::SameReply)));
    let [title, spoken, line, ..] = stop_surfaces(&mut app, AutopilotView::Cruise);
    for said in [&title, &spoken, &line] {
        assert!(
            said.contains(TYPES_AGAIN) && !said.contains(switch),
            "CONTROL: no switch clears it: {said}"
        );
    }
    tell_stop(
        &mut app,
        &sid,
        Some(Exception::repeats_by(HeldBy::SameReply, true)),
    );
    meta(&mut app, "set autopilot land");
    let [title, spoken, line, ..] = stop_surfaces(&mut app, AutopilotView::Land);
    for said in [&title, &spoken, &line] {
        assert!(
            said.contains(TYPES_AGAIN) && !said.contains(switch),
            "CONTROL: under Land: {said}"
        );
    }
}

/// An outage wait (§14.17.5.2) is said by Settings ▸ Harness's state line and `status waiting=`
/// only: the chip's title and description, the hover, the worker's tip, the needs and the
/// Messages stay as they were, and the wait's end or the supervisor's release takes it down.
#[test]
fn an_outage_wait_is_said_on_the_state_line_and_in_status_alone() {
    use crate::autopilot::AutopilotView;
    use crate::harness_glass::{Stamp, wall_today};
    use aterm_agent::supervise::Outage;
    let sep = crate::native_ui::FACT_SEPARATOR;
    let next = SINCE_S + 600;
    let (mut app, sid, _host) = desk("cruise");
    let quiet = |app: &App| {
        (
            app.messages.live_rows().count(),
            app.messages.digest().revision,
            app.presence.slot(0).and_then(|slot| slot.need.clone()),
            status(app, "needs"),
            app.chip_glass(0),
        )
    };
    let before = stop_surfaces(&mut app, AutopilotView::Cruise);
    let was = quiet(&app);
    assert!(
        before.iter().all(|s| !s.contains("waiting:")),
        "CONTROL: {before:#?}"
    );
    assert_eq!(status(&app, "waiting"), "-", "CONTROL: no wait");
    let tell = |app: &mut App, wall, tries| {
        app.apply_harness_fact(
            &sid,
            crate::HarnessFact::Waiting(Some(crate::OutageWait {
                wall,
                tries,
                at_unix: next,
            })),
        );
    };
    tell(&mut app, Outage::Overload, true);
    let after = stop_surfaces(&mut app, AutopilotView::Cruise);
    let at = Stamp::at_or_utc(next).ahead_words(Some(wall_today().1));
    assert!(
        after[2].starts_with(&format!("waiting: API overload{sep}next try {at}")),
        "the state line leads with it: {}",
        after[2]
    );
    assert_eq!(status(&app, "waiting"), "overload");
    assert_eq!(
        [&after[0], &after[1], &after[3], &after[4]],
        [&before[0], &before[1], &before[3], &before[4]],
        "no chip title, description, hover or tip says it"
    );
    assert_eq!(quiet(&app), was, "no Messages row, no need, no chip glass");
    tell(&mut app, Outage::Connection, false);
    let down = stop_surfaces(&mut app, AutopilotView::Cruise);
    assert!(
        down[2].starts_with("waiting: connection error") && !down[2].contains("next try"),
        "measured down, no try: {}",
        down[2]
    );
    assert_eq!(status(&app, "waiting"), "connection");
    app.apply_harness_fact(&sid, crate::HarnessFact::Waiting(None));
    let ended = stop_surfaces(&mut app, AutopilotView::Cruise);
    assert_eq!(ended, before, "its end takes it down");
    assert_eq!(status(&app, "waiting"), "-");
    tell(&mut app, Outage::ApiError, true);
    assert_eq!(status(&app, "waiting"), "api-error", "CONTROL: told again");
    let ctx = app.pool.get(0).expect("session 0").ctx.clone();
    assert!(crate::session_timeline::release_supervisor(
        &ctx,
        None,
        crate::metrics::now_us(),
        aterm_slog::Actor::Ctl
    ));
    assert_eq!(
        status(&app, "waiting"),
        "-",
        "a session this window no longer supervises waits on nothing it says"
    );
    app.harness_screen_moved();
    let released = stop_surfaces(&mut app, AutopilotView::Cruise);
    assert!(!released[2].contains("waiting:"), "{}", released[2]);
}

/// Land's one ask at the person's switch over a held tab (v6 §14.17 item 3) is said by Settings ▸
/// Harness's state line and `status land_asked=` while its answer is unread, and only while the
/// chip reads Land's `⤓`: the chip's title and description, the hover, the worker's tip and `held=`
/// stay as they were, and its answer or the supervisor's release takes it down.
#[test]
fn the_switchs_ask_is_said_on_the_state_line_and_in_status_while_it_stands() {
    use crate::autopilot::AutopilotView;
    use aterm_agent::harness::acts::SWITCH_ASKED;
    let (mut app, sid, _host) = desk("land");
    let before = stop_surfaces(&mut app, AutopilotView::Land);
    assert!(
        before.iter().all(|s| !s.contains(SWITCH_ASKED)),
        "CONTROL: {before:#?}"
    );
    assert_eq!(status(&app, "land_asked"), "-", "CONTROL: no ask");
    app.apply_harness_fact(&sid, crate::HarnessFact::SwitchAsked(true));
    let after = stop_surfaces(&mut app, AutopilotView::Land);
    assert!(
        after[2].starts_with(SWITCH_ASKED),
        "the state line leads with it: {}",
        after[2]
    );
    assert_eq!(status(&app, "land_asked"), "switch");
    assert_eq!(
        [&after[0], &after[1], &after[3], &after[4], &after[5]],
        [&before[0], &before[1], &before[3], &before[4], &before[5]],
        "no chip title, description, hover, tip or held= says it"
    );
    meta(&mut app, "set autopilot cruise");
    let cruise = stop_surfaces(&mut app, AutopilotView::Cruise);
    assert!(
        !cruise[2].contains(SWITCH_ASKED),
        "not under Cruise: {}",
        cruise[2]
    );
    assert_eq!(status(&app, "land_asked"), "-", "not under Cruise");
    meta(&mut app, "set autopilot land");
    assert_eq!(status(&app, "land_asked"), "switch", "CONTROL: Land again");
    app.apply_harness_fact(&sid, crate::HarnessFact::SwitchAsked(false));
    let answered = stop_surfaces(&mut app, AutopilotView::Land);
    assert_eq!(answered, before, "its answer takes it down");
    assert_eq!(status(&app, "land_asked"), "-");
    app.apply_harness_fact(&sid, crate::HarnessFact::SwitchAsked(true));
    assert_eq!(status(&app, "land_asked"), "switch", "CONTROL: told again");
    let ctx = app.pool.get(0).expect("session 0").ctx.clone();
    assert!(crate::session_timeline::release_supervisor(
        &ctx,
        None,
        crate::metrics::now_us(),
        aterm_slog::Actor::Ctl
    ));
    assert_eq!(
        status(&app, "land_asked"),
        "-",
        "a session this window no longer supervises is asked nothing it says"
    );
}

/// Session 0 as a codex-alab tab under `word`, no claim standing yet: codex in front, its reader
/// naming no state, as [`desk`] stands a Claude tab up but with no claim of aterm's.
fn codex_desk(word: &str) -> (App, String, TestHost) {
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
        tl.set_program(7, Some("codex".into()));
        tl.publish_agent(
            "unknown",
            None,
            None,
            Some(aterm_phase::Program::Codex),
            crate::session_timeline::AgentStamp {
                generation: crate::control::ScreenGen { epoch: 1, seq: 1 },
                fp: 1,
            },
        );
    }
    ctx.meta
        .lock()
        .expect("meta")
        .set("autopilot", Some(word.into()));
    let id = app.next_session_id;
    app.push_stub_tab(WindowId(0), crate::stub_session(id));
    (app, ctx.self_id.as_str().to_string(), host)
}

/// `meta <rest>` on session 0 through the real dispatch, from connection `conn` of the process
/// [`crate::self_claim::stand_in`] proves, the window woken as its wakes say.
fn self_meta(app: &mut App, rest: &str, conn: u64) -> String {
    let session = app.pool.get(0).expect("session 0");
    let (term, ctx) = (session.term.clone(), session.ctx.clone());
    let (reply, wakes) = crate::control::meta_wakes_proving_for_test(
        &app.store,
        &term,
        0,
        &ctx,
        rest,
        Some(conn),
        &|_| Some(crate::self_claim::stand_in::running()),
    );
    if wakes
        .iter()
        .any(|wake| matches!(wake, crate::Wake::MetaChanged { .. }))
    {
        app.on_meta_changed(0);
    }
    reply
}

/// Where session 0's stop is said of a tab without a chip: `status held=`, its Overview line's
/// driver, the tab's hover, the Settings page's state line and the worker row's tip.
fn codex_stop_surfaces(app: &mut App, sid: &str) -> [String; 5] {
    let overview = app
        .overview_parts()
        .0
        .iter()
        .flat_map(|group| group.chips.iter())
        .find(|chip| chip.sid.as_str() == sid)
        .and_then(|chip| chip.driver.as_ref().map(crate::overview::Driver::words))
        .unwrap_or_default();
    if !app.harness_page_shown() {
        assert!(app.open_harness_settings(WindowId(0), crate::harness_screen::HarnessFocus::Top));
    }
    app.harness_screen_moved();
    let instance = app
        .native_runtime
        .instance_by_kind(crate::native_app::AppKind::Settings)
        .expect("the Settings controller");
    let Some(crate::native_app::NativeApp::Settings(settings)) = app.native_runtime.app(instance)
    else {
        unreachable!("a Settings instance")
    };
    let line = settings
        .harness_snapshot()
        .projection()
        .sessions
        .iter()
        .find(|row| row.local_id == 0)
        .and_then(HarnessSessionRow::state_line);
    let handle = app.store.read().unwrap().by_local(0).cloned().expect("0");
    let tip = app.status_session_row(&handle).landing_tip;
    [
        status(app, "held"),
        overview,
        driven(app).unwrap_or_default(),
        line.unwrap_or_default(),
        tip.unwrap_or_default(),
    ]
}

/// CODEX-ALAB'S OWN STOP IS SAID WHERE ATERM'S IS, WHILE ITS PROVEN CLAIM STANDS (§14.3.2): a stop
/// codex-alab tells on its claim (`meta set harness_held`) is `status held=`'s arm, leads the
/// Settings ▸ Harness state line and the worker row's tip in a self-claim's stop words led by its
/// own name (`codex-alab stopped typing: …`, whose restart leaves out aterm's ask at a switch to
/// Land), so neither reads as aterm's stop, and is said after the
/// mode on the Overview and the hover, which name the native codex-alab autopilot
/// (`codex-alab autopilot (land: codex-alab stopped typing)`); its unset, and the claim's end, take every one of
/// them down. NEGATIVE CONTROLS: the same tab before its claim and after it says no stop, a stop
/// aterm's own loop put on its glass is never said under codex-alab's claim, and aterm's own stop
/// is said only while aterm holds the claim, in its own words with no name before them.
#[test]
fn a_codex_alabs_own_stop_is_said_where_aterms_is_while_its_claim_stands() {
    use crate::harness_glass::stop_words;
    use aterm_agent::harness::acts::{HeldBy, STOPPED_TYPING};
    use aterm_agent::supervise::policy::exception::Exception;
    let quiet = |surfaces: &[String; 5], case: &str| {
        assert_eq!(surfaces[0], "-", "{case}: {surfaces:#?}");
        assert!(
            surfaces.iter().all(|s| !s.contains(STOPPED_TYPING)),
            "{case}: {surfaces:#?}"
        );
    };
    let (mut app, sid, _host) = codex_desk("land");
    quiet(&codex_stop_surfaces(&mut app, &sid), "CONTROL: no claim");
    assert_eq!(
        self_meta(&mut app, "set harness_held same-reply", 4),
        "ERR denied\n",
        "CONTROL: no claim to tell it on"
    );
    let me = format!("codex-alab@{}", crate::self_claim::stand_in::pid());
    assert_eq!(
        self_meta(&mut app, &format!("set supervisor {me}"), 4),
        "OK\n"
    );
    let claimed = codex_stop_surfaces(&mut app, &sid);
    quiet(&claimed, "CONTROL: claimed, nothing told");
    assert_eq!(
        claimed[1], "codex-alab autopilot (land)",
        "CONTROL: the Overview names the claim's driver"
    );
    assert!(super::on_stopped(&mut app, 0, Some(HeldBy::NoWork), true));
    assert_eq!(
        app.harness_glass[&0].stopped(),
        Some(HeldBy::NoWork),
        "CONTROL: on aterm's glass"
    );
    quiet(
        &codex_stop_surfaces(&mut app, &sid),
        "aterm's own glass under codex-alab's claim",
    );
    assert!(super::on_stopped(&mut app, 0, None, false));

    for held_by in HeldBy::ALL {
        let case = format!("{held_by:?}");
        assert_eq!(
            self_meta(&mut app, &format!("set harness_held {}", held_by.word()), 4),
            "OK\n"
        );
        let [field, overview, hover, line, tip] = codex_stop_surfaces(&mut app, &sid);
        assert_eq!(field, held_by.word(), "{case}");
        let named = |form| format!("codex-alab {}", stop_words(held_by, form, false));
        let driven = format!("codex-alab autopilot (land: {})", named(HoldForm::Hover));
        assert_eq!(overview, driven, "{case}");
        assert_eq!(hover, driven, "{case}");
        assert!(line.starts_with(&named(HoldForm::Line)), "{case}: {line}");
        assert!(
            !line.contains(&stop_words(held_by, HoldForm::Line, true)),
            "{case}: aterm's restart, its switch to Land's ask, is not codex-alab's: {line}"
        );
        assert_eq!(tip, named(HoldForm::Tip), "{case}");
    }
    assert_eq!(self_meta(&mut app, "unset harness_held", 4), "OK\n");
    quiet(&codex_stop_surfaces(&mut app, &sid), "its unset");
    assert_eq!(
        self_meta(&mut app, "set harness_held same-reply", 4),
        "OK\n"
    );
    assert_eq!(status(&app, "held"), "same-reply", "CONTROL: told again");
    assert_eq!(self_meta(&mut app, "unset supervisor", 4), "OK\n");
    quiet(&codex_stop_surfaces(&mut app, &sid), "the claim's end");

    let (mut app, sid, _host) = desk("cruise");
    tell_stop(&mut app, &sid, Some(Exception::repeats(HeldBy::SameReply)));
    let handle = app.store.read().unwrap().by_local(0).cloned().expect("0");
    assert_eq!(status(&app, "held"), "same-reply", "CONTROL: aterm's claim");
    assert_eq!(
        app.status_session_row(&handle).landing_tip.as_deref(),
        Some(stop_words(HeldBy::SameReply, HoldForm::Tip, false).as_str()),
        "CONTROL: aterm's claim"
    );
    let ctx = app.pool.get(0).expect("session 0").ctx.clone();
    assert!(crate::session_timeline::release_supervisor(
        &ctx,
        None,
        crate::metrics::now_us(),
        aterm_slog::Actor::Ctl
    ));
    assert_eq!(status(&app, "held"), "-", "no claim of aterm's");
    assert_eq!(
        app.status_session_row(&handle).landing_tip,
        None,
        "no claim of aterm's"
    );
}

/// CODEX-ALAB'S STOP GIVES WAY WHERE ATERM'S DOES, ON EVERY SURFACE AT ONCE (§14.3.2): its told
/// stop is said only under a face that carries a mode, as aterm's own is, so once codex-alab tells
/// its own landing (`landed@v1`) or leaves the turn to a person (`left-to-person@v1`) none of
/// `status held=`, the worker row's tip, the Overview, the hover and the Settings state line says
/// it, and once its task is open again every one does, the claim and the kept stop standing
/// throughout. NEGATIVE CONTROL: under the open face before each, every surface says it.
#[test]
fn a_codex_alabs_stop_gives_way_to_its_landed_or_quiet_face_on_every_surface() {
    use aterm_agent::harness::acts::{HeldBy, STOPPED_TYPING};
    let (mut app, sid, _host) = codex_desk("land");
    let me = format!("codex-alab@{}", crate::self_claim::stand_in::pid());
    assert_eq!(
        self_meta(&mut app, &format!("set supervisor {me}"), 4),
        "OK\n"
    );
    assert_eq!(
        self_meta(&mut app, "set harness_held same-reply", 4),
        "OK\n"
    );
    let said = |app: &mut App, case: &str| {
        let surfaces = codex_stop_surfaces(app, &sid);
        assert_eq!(
            surfaces[0],
            HeldBy::SameReply.word(),
            "{case}: {surfaces:#?}"
        );
        assert!(
            surfaces[1..].iter().all(|s| s.contains(STOPPED_TYPING)),
            "{case}: {surfaces:#?}"
        );
    };
    let quiet = |app: &mut App, case: &str| {
        let surfaces = codex_stop_surfaces(app, &sid);
        assert_eq!(surfaces[0], "-", "{case}: {surfaces:#?}");
        assert!(
            surfaces.iter().all(|s| !s.contains(STOPPED_TYPING)),
            "{case}: {surfaces:#?}"
        );
    };
    let tell = |app: &mut App, face: TaskFace| {
        let ctx = app.pool.get(0).expect("session 0").ctx.clone();
        ctx.timeline.lock().expect("timeline").queue_self_task(face);
        app.apply_harness_fact(&sid, crate::HarnessFact::SelfTask(face));
        assert_eq!(app.chip_glass(0).task, Some(face), "codex-alab's face");
        ctx.meta
            .lock()
            .expect("meta")
            .self_held(crate::metrics::now_us())
    };
    for face in [TaskFace::Landed, TaskFace::Theirs] {
        said(&mut app, "CONTROL: the open face");
        assert_eq!(tell(&mut app, face), Some(HeldBy::SameReply), "{face:?}");
        quiet(&mut app, &format!("{face:?}"));
        assert_eq!(
            tell(&mut app, TaskFace::Open),
            Some(HeldBy::SameReply),
            "{face:?}"
        );
    }
    said(&mut app, "the open face again");
}

/// Where session 0's Land hold is shown of a tab without a chip, as [`codex_stop_surfaces`] reads
/// its stop.
#[derive(Debug, Default, PartialEq, Eq)]
struct HoldSurfaces {
    /// `status land_held= land_nudges= land_since=`.
    status: [String; 3],
    /// Whether the menu bar's row reads it landing, and This Mac's `landing` count.
    landing: (bool, usize),
    /// The worker row's tip.
    tip: String,
    /// The Overview's held line and its driver.
    overview: (String, String),
    /// The tab's hover.
    hover: String,
    /// The Settings ▸ Harness state line.
    line: String,
}

impl HoldSurfaces {
    /// Nothing of a Land hold anywhere: `-` in `status`, no landing, and no hold words on any
    /// line.
    fn assert_none(&self, case: &str) {
        assert_eq!(self.status, ["-", "-", "-"], "{case}: {self:#?}");
        assert_eq!(self.landing, (false, 0), "{case}: {self:#?}");
        assert_eq!(self.overview.0, "", "{case}: {self:#?}");
        for words in [&self.tip, &self.hover, &self.line] {
            assert!(
                !words.contains("not landed yet")
                    && !words.contains("landing since")
                    && !words.contains("nudge"),
                "{case}: {self:#?}"
            );
        }
    }
}

fn codex_hold_surfaces(app: &mut App, sid: &str) -> HoldSurfaces {
    let overview = app
        .overview_parts()
        .0
        .iter()
        .flat_map(|group| group.chips.iter())
        .find(|chip| chip.sid.as_str() == sid)
        .map(|chip| {
            (
                chip.held.clone().unwrap_or_default(),
                chip.driver
                    .as_ref()
                    .map(crate::overview::Driver::words)
                    .unwrap_or_default(),
            )
        })
        .unwrap_or_default();
    if !app.harness_page_shown() {
        assert!(app.open_harness_settings(WindowId(0), crate::harness_screen::HarnessFocus::Top));
    }
    app.harness_screen_moved();
    let instance = app
        .native_runtime
        .instance_by_kind(crate::native_app::AppKind::Settings)
        .expect("the Settings controller");
    let Some(crate::native_app::NativeApp::Settings(settings)) = app.native_runtime.app(instance)
    else {
        unreachable!("a Settings instance")
    };
    let line = settings
        .harness_snapshot()
        .projection()
        .sessions
        .iter()
        .find(|row| row.local_id == 0)
        .and_then(HarnessSessionRow::state_line);
    let handle = app.store.read().unwrap().by_local(0).cloned().expect("0");
    let row = app.status_session_row(&handle);
    HoldSurfaces {
        status: land_fields(app),
        landing: (row.landing, app.status_glance().here.landing),
        tip: row.landing_tip.unwrap_or_default(),
        overview,
        hover: driven(app).unwrap_or_default(),
        line: line.unwrap_or_default(),
    }
}

/// [`held`] as codex-alab's `meta set harness_land` spells it.
fn held_line() -> String {
    format!(
        "set harness_land since={} nudges=3 read=1 legs=needs,repo,open \
         open=follow-up,not%20pushed unpushed=2",
        SINCE_S * 1000
    )
}

/// CODEX-ALAB'S OWN LAND HOLD IS SHOWN WHERE ATERM'S IS, WHILE ITS PROVEN CLAIM STANDS (§14.3.3): a
/// hold codex-alab tells on its claim (`meta set harness_land`) is `status land_held= land_nudges=
/// land_since=`, makes the tab landing on the menu bar (its row and This Mac's count), and is said
/// on the worker row's tip, the Overview's held line, the hover and the Settings ▸ Harness state
/// line in aterm's hold words led by its own name (`codex-alab: not landed yet: …`), as aterm's
/// own is said there; a stop it told beside it names it once (`codex-alab stopped typing: … · not
/// landed yet: …`); its nudges and its land act's instant move as told; its own landing, a
/// person's own exchange and a mode other than Land hide it, and it comes back with the open face
/// and Land; its unset and the claim's end take it down. NEGATIVE CONTROLS: the same tab before
/// its claim and after it shows no hold and is denied the verb, a hold aterm's own loop put on its
/// glass is never shown under codex-alab's claim, and a Claude tab's own hold is shown in aterm's
/// own words with no name, only while aterm holds the claim.
#[test]
fn a_codex_alabs_own_land_hold_is_shown_where_aterms_is_while_its_claim_stands() {
    use crate::harness_glass::stop_words;
    use aterm_agent::harness::acts::HeldBy;
    let since = SINCE_S.to_string();
    let tip = hold_words(&glass(held(), TODAY), None, HoldForm::Tip, None, None).expect("a tip");
    let hover =
        hold_words(&glass(held(), TODAY), None, HoldForm::Hover, None, None).expect("a hover");
    let shown = |surfaces: &HoldSurfaces, case: &str| {
        assert_eq!(
            surfaces.status,
            ["needs,repo,open", "3", since.as_str()],
            "{case}: {surfaces:#?}"
        );
        assert_eq!(surfaces.landing, (true, 1), "{case}: {surfaces:#?}");
        assert_eq!(surfaces.tip, format!("codex-alab: {tip}"), "{case}");
        assert_eq!(surfaces.overview.0, format!("codex-alab: {tip}"), "{case}");
        assert_eq!(
            surfaces.overview.1, "codex-alab autopilot (land)",
            "{case}: the driver says no hold"
        );
        assert_eq!(
            surfaces.hover,
            format!("codex-alab autopilot (land: codex-alab: {hover})"),
            "{case}"
        );
        assert!(
            surfaces.line.starts_with(
                "codex-alab: not landed yet: a need named earlier in its reply; 2 unpushed"
            ) && surfaces.line.contains(" \u{b7} landing since ")
                && surfaces.line.ends_with(" \u{b7} 3 nudges"),
            "{case}: {}",
            surfaces.line
        );
    };
    let (mut app, sid, _host) = codex_desk("land");
    codex_hold_surfaces(&mut app, &sid).assert_none("CONTROL: no claim");
    assert_eq!(
        self_meta(&mut app, &held_line(), 4),
        "ERR denied\n",
        "CONTROL: no claim to tell it on"
    );
    let me = format!("codex-alab@{}", crate::self_claim::stand_in::pid());
    assert_eq!(
        self_meta(&mut app, &format!("set supervisor {me}"), 4),
        "OK\n"
    );
    codex_hold_surfaces(&mut app, &sid).assert_none("CONTROL: claimed, nothing told");
    assert!(super::on_land_held(&mut app, 0, Some(held())));
    assert!(
        app.harness_glass[&0].hold().is_some(),
        "CONTROL: on aterm's glass"
    );
    codex_hold_surfaces(&mut app, &sid).assert_none("aterm's own glass under codex-alab's claim");
    assert!(super::on_land_held(&mut app, 0, None));

    assert_eq!(
        self_meta(
            &mut app,
            &format!("set harness_land since={since}000 nudges=0"),
            4
        ),
        "OK\n"
    );
    let begun = codex_hold_surfaces(&mut app, &sid);
    assert_eq!(begun.status, ["-", "0", since.as_str()], "{begun:#?}");
    assert_eq!(
        begun.landing,
        (true, 1),
        "Land's instruction typed: landing"
    );
    assert_eq!(begun.tip, "", "no yield read, no nudge: no tip");
    assert_eq!(begun.overview.0, "", "no yield read: no held line");
    assert_eq!(begun.hover, "codex-alab autopilot (land)", "no yield read");
    assert!(
        begun.line.starts_with("codex-alab: landing since "),
        "{}",
        begun.line
    );
    assert_eq!(
        self_meta(
            &mut app,
            &format!("set harness_land since={since}000 nudges=1"),
            4
        ),
        "OK\n"
    );
    let nudged = codex_hold_surfaces(&mut app, &sid);
    assert_eq!(nudged.status, ["-", "1", since.as_str()]);
    assert_eq!(nudged.tip, "codex-alab: 1 nudge");
    assert!(nudged.line.ends_with(" \u{b7} 1 nudge"), "{}", nudged.line);

    assert_eq!(self_meta(&mut app, &held_line(), 4), "OK\n");
    shown(&codex_hold_surfaces(&mut app, &sid), "a judged yield");

    assert_eq!(
        self_meta(&mut app, "set harness_held same-reply", 4),
        "OK\n"
    );
    let stopped = codex_hold_surfaces(&mut app, &sid);
    let stop = |form| format!("codex-alab {}", stop_words(HeldBy::SameReply, form, false));
    assert_eq!(
        stopped.tip,
        format!(
            "{}{}{tip}",
            stop(HoldForm::Tip),
            crate::native_ui::FACT_SEPARATOR
        ),
        "the stop names its holder once"
    );
    assert_eq!(
        stopped.hover,
        format!(
            "codex-alab autopilot (land: {}; {hover})",
            stop(HoldForm::Hover)
        )
    );
    assert!(
        stopped.line.starts_with(&stop(HoldForm::Line))
            && stopped.line.contains(" \u{b7} not landed yet: ")
            && !stopped.line.contains("codex-alab: "),
        "{}",
        stopped.line
    );
    assert_eq!(
        stopped.overview.0,
        format!("codex-alab: {tip}"),
        "a line of its own"
    );
    assert_eq!(self_meta(&mut app, "unset harness_held", 4), "OK\n");
    shown(&codex_hold_surfaces(&mut app, &sid), "the stop lifted");

    let tell = |app: &mut App, face: TaskFace| {
        let ctx = app.pool.get(0).expect("session 0").ctx.clone();
        ctx.timeline.lock().expect("timeline").queue_self_task(face);
        app.apply_harness_fact(&sid, crate::HarnessFact::SelfTask(face));
        assert_eq!(app.chip_glass(0).task, Some(face), "codex-alab's face");
    };
    for face in [TaskFace::Landed, TaskFace::Theirs] {
        tell(&mut app, face);
        codex_hold_surfaces(&mut app, &sid).assert_none(&format!("{face:?}"));
        tell(&mut app, TaskFace::Open);
        shown(
            &codex_hold_surfaces(&mut app, &sid),
            &format!("open after {face:?}"),
        );
    }
    meta(&mut app, "set autopilot cruise");
    codex_hold_surfaces(&mut app, &sid).assert_none("under Cruise");
    meta(&mut app, "set autopilot land");
    shown(&codex_hold_surfaces(&mut app, &sid), "under Land again");

    assert_eq!(self_meta(&mut app, "unset harness_land", 4), "OK\n");
    codex_hold_surfaces(&mut app, &sid).assert_none("its unset");
    assert_eq!(self_meta(&mut app, &held_line(), 4), "OK\n");
    shown(&codex_hold_surfaces(&mut app, &sid), "CONTROL: told again");
    assert_eq!(self_meta(&mut app, "unset supervisor", 4), "OK\n");
    codex_hold_surfaces(&mut app, &sid).assert_none("the claim's end");

    let (mut app, sid, _host) = desk("land");
    tell_hold(&mut app, &sid, Some(held()));
    let handle = app.store.read().unwrap().by_local(0).cloned().expect("0");
    assert_eq!(
        land_fields(&app),
        ["needs,repo,open", "3", since.as_str()],
        "CONTROL: aterm's claim"
    );
    assert_eq!(
        app.status_session_row(&handle).landing_tip.as_deref(),
        Some(tip.as_str()),
        "CONTROL: aterm's own words, no name"
    );
    assert_eq!(driven(&app).as_deref(), Some(WHY), "CONTROL: aterm's claim");
    let ctx = app.pool.get(0).expect("session 0").ctx.clone();
    assert!(crate::session_timeline::release_supervisor(
        &ctx,
        None,
        crate::metrics::now_us(),
        aterm_slog::Actor::Ctl
    ));
    assert!(app.harness_glass[&0].hold().is_some(), "still on the glass");
    assert_eq!(land_fields(&app), ["-", "-", "-"], "no claim of aterm's");
    let row = app.status_session_row(&handle);
    assert!(!row.landing, "no claim of aterm's");
    assert_eq!(row.landing_tip, None, "no claim of aterm's");
}

/// The words aterm's loop tells beside a hold whose agent listed two items over an untracked
/// file (slice 4b).
fn quoted_said(first: &str) -> aterm_agent::harness::hold_said::HoldSaid {
    aterm_agent::harness::hold_said::HoldSaid {
        open: vec![first.to_string(), "Bump the version.".to_string()],
        more: 0,
        paths: vec!["wip-notes.txt".to_string()],
    }
}

/// A hold on an untracked file and the agent's own list, three nudges since the act.
fn listed_hold() -> LandHeld {
    LandHeld {
        legs: HoldLeg::Repo.bit() | HoldLeg::Open.bit(),
        unpushed: 0,
        untracked: 1,
        ..held()
    }
}

/// THE HOLD QUOTES THE AGENT AND NAMES THE FILES GIT FOUND, in every form (§14.17 item 1, slice
/// 4b): the state line quotes each item, the title, the hover and a tip the first, counting the
/// rest, and a screen reader hears the first as a sentence; the repository leg names the file.
/// Negative control: the hold's words not passed (`None`) fails every quote and the file's name.
#[test]
fn the_hold_quotes_the_agent_and_names_the_files_git_found() {
    use aterm_agent::harness::acts::held_tip;
    let hold = glass(listed_hold(), TODAY);
    let said = quoted_said("The docs PR is still open.");
    let words = |form| hold_words(&hold, None, form, Some(TODAY), Some(&said)).expect("said");
    let file = "1 untracked file: wip-notes.txt";
    let first = "open in its report: \u{201c}The docs PR is still open.\u{201d} (+1 more)";
    assert_eq!(
        words(HoldForm::Line),
        format!(
            "not landed yet: {file}; open in its report: \u{201c}The docs PR is still \
             open.\u{201d}; \u{201c}Bump the version.\u{201d} \u{b7} landing since 14:02 \u{b7} \
             3 nudges"
        )
    );
    assert_eq!(
        words(HoldForm::Title),
        format!("not landed yet: {file}; {first} \u{b7} landing since 14:02, 3 nudges")
    );
    assert_eq!(
        words(HoldForm::Hover),
        format!("not landed yet, {file}; {first}")
    );
    assert_eq!(
        words(HoldForm::Tip),
        held_tip(&format!("{file}; {first}"), 3)
    );
    let spoken = words(HoldForm::Spoken);
    assert!(
        spoken.contains("Open in its report: The docs PR is still open. And 1 more."),
        "{spoken}"
    );
    assert!(
        spoken.contains("1 untracked file: wip-notes.txt."),
        "{spoken}"
    );
    assert!(
        spoken
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, ' ' | '.' | ':' | ',' | '-' | '_')),
        "glyph-free: {spoken}"
    );
}

/// A hold of three changed files and the agent's one item, `item`, as a screen reader hears it.
fn spoken_said(item: &str) -> String {
    let hold = glass(
        LandHeld {
            legs: HoldLeg::Repo.bit() | HoldLeg::Open.bit(),
            unpushed: 0,
            changed: 3,
            untracked: 0,
            ..held()
        },
        TODAY,
    );
    let said = aterm_agent::harness::hold_said::HoldSaid {
        open: vec![item.to_string()],
        more: 0,
        paths: vec![
            "crates/aterm_x/src/hold_said.rs".to_string(),
            "docs/my-notes/".to_string(),
        ],
    };
    hold_words(&hold, None, HoldForm::Spoken, Some(TODAY), Some(&said)).expect("said")
}

/// A SCREEN READER HEARS EACH FILE'S NAME WHOLE: a nested path is said by its file name, its `-`,
/// `_` and `.` kept and its folders left out, then `and more` where git found more than it named.
/// Negative control: the names passed through `speakable` (`cratesaterm xsrcholdsaid.rs`) fail.
#[test]
fn a_screen_reader_hears_each_files_name_whole() {
    let spoken = spoken_said("The docs PR is still open.");
    assert!(
        spoken.contains("3 changed files: hold_said.rs, my-notes and more."),
        "{spoken}"
    );
}

/// A SCREEN READER HEARS THE AGENT'S WORDS APART: a quote's `/`, `\\` and `_` are spaces, so
/// the words they join never run together.
/// Negative control: `/` and `_` dropped (`srcholdsaid.rs`) fail.
#[test]
fn a_screen_reader_hears_the_agents_words_apart() {
    let spoken = spoken_said("Fix src/hold_said.rs in the my_tool crate.");
    assert!(
        spoken.contains("Open in its report: Fix src hold said.rs in the my tool crate. Landing"),
        "{spoken}"
    );
}

/// A QUOTE THAT CLOSES ITSELF IS NOT CLOSED AGAIN in the spoken sentence (`.`, `:`, the closing
/// marks a screen reader is handed), and one that does not, or whose `?` is not handed on, is
/// closed with `.`.
/// Negative control: `.` added after every quote but one ending in `.` (`review:.`) fails.
#[test]
fn a_spoken_quote_is_closed_once() {
    for (item, heard) in [
        (
            "Is the docs PR merged?",
            "Open in its report: Is the docs PR merged. Landing",
        ),
        (
            "The docs PR is still open.",
            "Open in its report: The docs PR is still open. Landing",
        ),
        (
            "Bump the version",
            "Open in its report: Bump the version. Landing",
        ),
        (
            "Waiting on review:",
            "Open in its report: Waiting on review: Landing",
        ),
    ] {
        let spoken = spoken_said(item);
        assert!(spoken.contains(heard), "{item}: {spoken}");
    }
}

/// A LONG ITEM IS CUT IN THE SHORT FORMS AND WHOLE ON THE STATE LINE: the title, the hover and a
/// tip quote at most [`aterm_agent::harness::hold_said::SHORT_CHARS`] of it, cut at a word with
/// `…`; the line, with room, quotes it whole.
/// Negative control: the short forms quoting the item uncut fails the bound.
#[test]
fn a_long_item_is_cut_in_the_short_forms_and_whole_on_the_line() {
    use aterm_agent::harness::hold_said::SHORT_CHARS;
    let long =
        "The follow-up PR for the docs is still open, waiting on a review from the docs team.";
    let hold = glass(listed_hold(), TODAY);
    let said = quoted_said(long);
    let words = |form| hold_words(&hold, None, form, Some(TODAY), Some(&said)).expect("said");
    assert!(words(HoldForm::Line).contains(&format!("\u{201c}{long}\u{201d}")));
    for form in [HoldForm::Title, HoldForm::Hover, HoldForm::Tip] {
        let said = words(form);
        let quote = said
            .split_once('\u{201c}')
            .and_then(|(_, rest)| rest.split_once('\u{201d}'))
            .map(|(quote, _)| quote)
            .expect("a quote");
        assert!(quote.chars().count() <= SHORT_CHARS, "{form:?}: {said}");
        assert!(quote.ends_with('\u{2026}'), "{form:?}: {said}");
        assert!(said.contains("\u{201d} (+1 more)"), "{form:?}: {said}");
    }
}

/// SAID ONCE: A HOLD ONLY A PERSON'S NEED HOLDS WAITS ONLY ON YOU in every form (§14.4), the need
/// itself on the title and the state line, and no form says `not landed yet:` or `needs you`
/// beside it; the hover and a tip, under the need's own line, say only that it waits.
/// Negative control: the need alone worded as any other leg (`not landed yet: needs you: …`)
/// fails every form.
#[test]
fn a_need_alone_waits_only_on_you_in_every_form() {
    let hold = glass(
        LandHeld {
            legs: HoldLeg::Needs.bit(),
            open: [None; 4],
            unpushed: 0,
            ..held()
        },
        TODAY,
    );
    let need = ask();
    let words = |form| hold_words(&hold, Some(&need), form, Some(TODAY), None).expect("said");
    assert_eq!(
        words(HoldForm::Line),
        format!(
            "waits only on you: {} (+1 more) \u{b7} landing since 14:02 \u{b7} 3 nudges",
            need.first
        )
    );
    assert_eq!(
        words(HoldForm::Title),
        "waits only on you: sign 0.48.0, the release credential, on this Mac (+1 more) \u{b7} \
         landing since 14:02, 3 nudges"
    );
    assert_eq!(words(HoldForm::Hover), "waits only on you");
    assert_eq!(words(HoldForm::Tip), "waits only on you \u{b7} 3 nudges");
    assert_eq!(
        words(HoldForm::Spoken),
        "It waits only on you: sign 0.48.0 the release credential on this Mac. And 1 more. \
         Landing since 14:02, 3 nudges."
    );
    for form in [
        HoldForm::Title,
        HoldForm::Line,
        HoldForm::Hover,
        HoldForm::Tip,
        HoldForm::Spoken,
    ] {
        let said = words(form).to_lowercase();
        assert!(
            !said.contains("not landed yet") && !said.contains("needs you"),
            "{form:?}: {said}"
        );
    }
}

/// Tab `sid`'s loop tells the hold's words, then the hold they belong to, as
/// `turn_end_loop::tell_hold` does.
fn tell_quoted(
    app: &mut App,
    sid: &str,
    said: Option<&aterm_agent::harness::hold_said::HoldSaid>,
    held: Option<LandHeld>,
) {
    let _ = crate::harness_host::store_land_said(&app.store, sid, said);
    tell_hold(app, sid, held);
}

/// A REWORDED ITEM REFRESHES EVERY SURFACE THOUGH THE HOLD IS EQUAL (slice 4b): the chip's title
/// and description, the state line, the tab's hover and the worker row's tip quote the new words
/// once the loop tells them, the hold itself told again unchanged.
/// Negative control: the stored hold's equality alone deciding whether the window reads again
/// (main's `set_land_held`) leaves every surface on the old words.
#[test]
fn a_reworded_item_refreshes_every_surface_though_the_hold_is_equal() {
    use crate::autopilot::AutopilotView;
    let (mut app, sid, _host) = desk("land");
    let (old, new) = (
        "The docs PR is still open.",
        "The pull request for the docs is still open.",
    );
    tell_quoted(&mut app, &sid, Some(&quoted_said(old)), Some(listed_hold()));
    let before = stop_surfaces(&mut app, AutopilotView::Land);
    for said in &before[..5] {
        assert!(
            said.contains(old.trim_end_matches('.')),
            "CONTROL: {before:#?}"
        );
    }
    tell_quoted(&mut app, &sid, Some(&quoted_said(new)), Some(listed_hold()));
    let after = stop_surfaces(&mut app, AutopilotView::Land);
    for said in &after[..5] {
        assert!(
            said.contains("pull request for the") && !said.contains(old.trim_end_matches('.')),
            "{after:#?}"
        );
    }
    tell_quoted(&mut app, &sid, None, Some(listed_hold()));
    let plain = stop_surfaces(&mut app, AutopilotView::Land);
    assert!(
        plain.iter().all(|said| !said.contains('\u{201c}')),
        "the words taken down, the reader's left: {plain:#?}"
    );
}

/// THE HOLD'S WORDS ARE SANITIZED AND BOUNDED AT STORE: an item's escapes and bidi controls never
/// reach the glass, and an item or path is at most
/// [`aterm_agent::harness::hold_said::ITEM_BYTES`]; words that leave nothing store none.
/// Negative control: the words stored as told fails the escape.
#[test]
fn the_holds_words_are_sanitized_and_bounded_at_store() {
    use aterm_agent::harness::hold_said::{HoldSaid, ITEM_BYTES};
    let (mut app, sid, _host) = desk("land");
    let told = HoldSaid {
        open: vec![
            "The docs \u{1b}[31mPR\u{1b}[0m is \u{202e}still open.".to_string(),
            "x".repeat(4 * ITEM_BYTES),
        ],
        more: 0,
        paths: vec!["wip\u{7}-notes.txt".to_string()],
    };
    tell_quoted(&mut app, &sid, Some(&told), Some(listed_hold()));
    let stored = app.hold_said.get(&0).cloned().expect("stored");
    for word in stored.open.iter().chain(&stored.paths) {
        assert!(
            !word
                .chars()
                .any(|c| c.is_control() || ('\u{202a}'..='\u{202e}').contains(&c)),
            "{word:?}"
        );
        assert!(word.len() <= ITEM_BYTES, "{} bytes", word.len());
    }
    assert!(stored.open[0].contains("PR"), "{stored:?}");
    let blank = HoldSaid {
        open: vec!["\u{1b}\u{202e}\u{7}".to_string()],
        more: 0,
        paths: Vec::new(),
    };
    tell_quoted(&mut app, &sid, Some(&blank), Some(listed_hold()));
    assert!(!app.hold_said.contains_key(&0), "nothing left, none stored");
}

/// SAID ONCE (§14.4): while a read hold says `landing since 10:30, 2 nudges`, the title's
/// `last:` leaves Land's own acts out (`⤓ landing`, `⤓ nudged`, `⤓ land check`), and the chip's
/// spoken description does not say Land's newest act again; any other act still shows.
/// Negative controls: the trail unfiltered fails the title; the spoken last act kept fails the
/// description.
#[test]
fn the_titles_trail_drops_lands_own_acts_while_a_read_hold_says_them() {
    use crate::autopilot::AutopilotView;
    use aterm_agent::harness::acts::Act;
    let (mut app, sid, _host) = desk("land");
    // The trail keeps the three newest; Land's newest act is the one a screen reader hears.
    for act in [Act::Landing, Act::LandCheck, Act::Approved, Act::Nudged] {
        crate::harness_glass::on_act(&mut app, 0, act);
    }
    let read = |app: &mut App| {
        let [title, spoken, ..] = stop_surfaces(app, AutopilotView::Land);
        (title, spoken)
    };
    let (before, spoken) = read(&mut app);
    for word in ["\u{2913} nudged", "\u{2713} approved"] {
        assert!(before.contains(word), "CONTROL, no hold: {before}");
    }
    assert!(spoken.contains("Last:"), "CONTROL, no hold: {spoken}");
    tell_hold(&mut app, &sid, Some(listed_hold()));
    let (held, spoken) = read(&mut app);
    assert!(held.contains("not landed yet"), "{held}");
    for word in ["\u{2913} landing", "\u{2913} nudged", "\u{2913} land check"] {
        assert!(!held.contains(word), "{word} said by the hold: {held}");
    }
    assert!(held.contains("\u{2713} approved"), "{held}");
    assert!(spoken.contains("Not landed yet."), "{spoken}");
    assert!(!spoken.contains("Last:"), "the hold says it: {spoken}");
}
