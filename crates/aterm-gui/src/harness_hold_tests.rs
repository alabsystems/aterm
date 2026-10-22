// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE HOLD IN THE WINDOW (`docs/DESIGN-harness-autopilot-2026-10-15.md`
//! §12.3.1, §12.3.3 item 3, §12.3.6; slice 3c): why Land still holds, as
//! the window says it. The loop's hold (`LandHeld`, told through the host's
//! hook onto the session's timeline, then a `HarnessFact::LandHeld` wake that
//! carries nothing) goes onto the glass with its `since` stamped once
//! ([`HeldGlass`]), and ONE function words it in five forms
//! ([`hold_words`]): the chip's title (W4 wires it), the SESSIONS state
//! line, the tab's hover and the Overview, the menu bar's landing worker's
//! tip, and the accessibility sentence (W4 wires it). It is shown only while
//! the chip wears `⤓ land` ([`App::shown_hold`]); a person's own exchange and
//! a proven self-claim take a standing hold down; `status` prints
//! `land_held= land_nudges= land_since=` after `pause=` only where this
//! host holds the claim under Land.
//!
//! Surfaces are read as their readers read them: the hover as the strip was
//! last stamped with it (`work_counts::last_tooltips_pushed`), the menu as
//! `chrome` prints it (`menu_lines`), the page as its projection says it,
//! `status` by key. Each test names its NEGATIVE CONTROL: the mutant it was
//! run against, and what failed.

use std::sync::Arc;

use aterm_agent::harness::acts::{HoldLeg, LandHeld, OpenWord, TaskFace};

use super::{HeldGlass, HoldForm, hold_words, on_land_held};
use crate::claude_lights::{ChipGlass, Clock, Stamp};
use crate::harness_glass::PauseGlass;
use crate::harness_screen::HarnessSessionRow;
use crate::{App, WindowId};

/// The publish's local day in the line tests (a Friday).
const TODAY: i64 = 20_007;

/// 2026-10-16 14:02:00 UTC, the land act's second in the App tests.
const SINCE_S: i64 = 1_792_159_320;

/// 14:02 on `day`, as the glass keeps a hold's `since`.
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
        // Counted, unread: the entry's parse (`HarnessAsk::of`).
        others: Vec::new(),
        program: aterm_phase::Program::Claude,
    }
}

/// A HOLD'S WORDS, FIVE FORMS FROM ONE FUNCTION (§12.3.1): the state line
/// `not landed yet: needs you: <need> (+1 more); 2 unpushed commits; open
/// work in its report: follow-up, not pushed · landing since 14:02 · 3
/// nudges`; the title the same legs, title-safe (the need's own ` · ` and
/// ` — ` read `, `, so a title's cut marks are its own), its clock and
/// nudges after one ` · `; the hover `not landed yet, needs you; …` (no
/// clock, the need by `needs you` alone: its own lines stand above); the
/// tip the hover's legs and the nudges; the spoken form one sentence a leg,
/// letters, digits, spaces and `.:,` alone. Another day names its weekday
/// (`Fri 14:02`, `Friday at 14:02`). Before a yield is read, only the clock
/// part: no hover words (main's `land`), the tip only the nudges. The
/// repository's counts are each said only where not zero. NEGATIVE CONTROLS
/// (as run): the title form without `title_safe` gives the title three
/// ` · ` parts; the spoken form without `speakable` keeps `·` and `—`.
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
    // Nothing in the title's, the hover's or the tip's legs reads as another
    // part of its line (the line keeps the worker's own words whole).
    for form in [HoldForm::Title, HoldForm::Hover, HoldForm::Tip] {
        let said = words(form).expect("said");
        for foreign in [" \u{2014} ", " \u{b7} last: ", "pausing until"] {
            assert!(!said.contains(foreign), "{form:?}: {said}");
        }
    }

    // Another day names its weekday; spoken, spelled out.
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

    // Before a yield is read: only the clock part.
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

    // Each count only where not zero, the need named earlier where the
    // store holds none, every leg in the owner's order.
    let every = glass(
        LandHeld {
            legs: HoldLeg::ALL.iter().fold(0, |bits, leg| bits | leg.bit()),
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
}

/// THE OPEN LEG NAMES WHAT THE NUDGE NAMED, NEVER AS A QUOTE (§12.3.5,
/// the review of 3c): every word Land's strict reading can name open — a
/// spelling it matched (`follow-up`), its own word for a kind of line
/// (`unchecked box`, `%`, `#n`, `a question`) or a family's word for its
/// rows (`a part`, `local`) — reads `open work in its report: <word>` in the
/// title, the line, the hover and the tip, as the nudge's suffix named it,
/// with no quotation marks around it (a description is no word of the
/// report, and a spelling may be either); spoken, `Open work in its report:
/// <word said>.` with something said for every word (`%` `percent`, `#n`
/// `number n`). NEGATIVE CONTROLS (as run): the open leg quoting each word
/// (`“unchecked box” in its report`) fails the first assertion on the first
/// word; `speakable` dropping `%` says `Open work in its report.` for it.
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

/// THE GLASS STAMPS A HOLD'S `since` ONCE (§12.3.3 item 3): a hold told
/// again as it stands is no move (nothing redrawn, the trail's generation
/// kept); a nudge moves it and keeps the clock it was stamped with; a new
/// `since` is stamped again; `None` takes it down. NEGATIVE CONTROL (as
/// run): `on_land_held` without its compare (every tell a move) reports the
/// hold told again as a move.
#[test]
fn the_glass_stamps_a_holds_since_once() {
    let mut app = App::headless_for_test();
    assert!(on_land_held(&mut app, 0, Some(held())));
    let first = app.harness_glass[&0];
    let stamped = first.hold().expect("held").since;
    assert!(stamped.is_some(), "stamped where it arrived");
    assert!(!on_land_held(&mut app, 0, Some(held())), "told again");
    assert_eq!(app.harness_glass[&0].trail_gen(), first.trail_gen());
    // A nudge: moved, the clock kept as it was stamped.
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
    // A new hold's since is read again.
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

/// A SESSIONS row under `view`, its glass `glass`, its need `need`, its hold
/// `hold`, published on [`TODAY`].
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

/// THE STATE LINE SAYS WHY LAND HOLDS (§12.3.1, §12.3.6's
/// `state_line_says_why_land_holds`): under `⤓ land`, after a yield that did
/// not land, `not landed yet: …; … · landing since 14:02 · 3 nudges ·
/// pausing until 18:41 (3 short turns in a row)` — the need said once, in
/// the hold's legs, where the hold names it; before a yield, the need's own
/// head and `landing since 14:02 · 3 nudges`. Under Cruise, at a landing and
/// at a person's own exchange the hold says nothing. NEGATIVE CONTROL (as
/// run): main's Land arm (no line of its own) says only the pause.
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
    // Before a yield: the need heads, the hold's clock follows.
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
    // No hold beside another face.
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
            legs: HoldLeg::ALL.iter().fold(0, |bits, leg| bits | leg.bit()),
            changed: 3,
            untracked: 1,
            stashes: 1,
            unlanded: 4,
            ..held()
        },
        0,
    ));
}

/// A host that supervises nothing, shut down on drop.
struct TestHost(crate::harness_host::HostHandle);

impl Drop for TestHost {
    fn drop(&mut self) {
        self.0.shutdown_and_join();
    }
}

/// An App whose session 0 runs Claude Code under its own word `word`,
/// claimed by this host, in window 0's first tab, a second tab in front,
/// its task open. Its sid, and the host.
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

/// Session `sid`'s loop tells `held` as the host does: stored on the
/// timeline by the live hook (`harness_host::store_land_held`), then the
/// wake that carries nothing, applied where it moved.
fn tell_hold(app: &mut App, sid: &str, held: Option<LandHeld>) {
    if crate::harness_host::store_land_held(&app.store, sid, held) {
        app.apply_harness_fact(sid, crate::HarnessFact::LandHeld);
    }
}

/// The hover's who-drives words for session 0, as composed now.
fn driven(app: &App) -> Option<String> {
    app.presence_hover(0, None).and_then(|hover| {
        hover
            .split(crate::native_ui::FACT_SEPARATOR)
            .flat_map(|part| part.lines())
            .find(|part| part.starts_with("driven by aterm"))
            .map(str::to_string)
    })
}

/// Session 0's tab's hover as the strip was last stamped with it.
fn pushed() -> String {
    let tips = crate::work_counts::last_tooltips_pushed(WindowId(0)).expect("a stamp");
    tips.first().cloned().flatten().unwrap_or_default()
}

/// `key=`'s value on session 0's `status` line.
fn status(app: &App, key: &str) -> String {
    let line = app.session_status_record(0).expect("status");
    let prefix = format!("{key}=");
    line.split_whitespace()
        .find_map(|t| t.strip_prefix(prefix.as_str()))
        .unwrap_or_else(|| panic!("{key} in {line}"))
        .to_string()
}

/// The three `status` fields of Land's hold.
fn land_fields(app: &App) -> [String; 3] {
    ["land_held", "land_nudges", "land_since"].map(|key| status(app, key))
}

/// The wire's `meta` on session 0, its wake applied where it changed.
fn meta(app: &mut App, rest: &str) {
    let session = app.pool.get(0).expect("session 0");
    let (term, ctx) = (session.term.clone(), session.ctx.clone());
    let (reply, changed) = crate::control::cmd_meta(&term, &app.store, 0, &ctx, rest);
    assert!(reply.starts_with("OK"), "{reply}");
    if changed {
        app.on_meta_changed(0);
    }
}

/// The hover's words for [`held`].
const WHY: &str = "driven by aterm (land: not landed yet, needs you; 2 unpushed commits; open \
                   work in its report: follow-up, not pushed)";

/// WHO DRIVES SAYS WHY A LANDING HOLDS (§12.3.1, §12.3.6): under `⤓ land`
/// the tab's hover says `driven by aterm (land)` until a yield is read, then
/// `driven by aterm (land: not landed yet, needs you; 2 unpushed commits;
/// open work in its report: follow-up, not pushed)` — no clock (the hover is read
/// long after its push) — and `(land)` again where the loop takes the hold
/// down. NEGATIVE CONTROL (as run): `driven_words` ignoring the hold
/// (main's `land`) fails the second assertion.
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

/// THE HOVER FOLLOWS THE HOLD AND THE FACE (§12.3.6's
/// `the_hover_follows_the_hold_and_the_face`, 3c's half): the hold's wake
/// alone — no face moved, no footer posted — stamps the strip again, so the
/// pushed hover says why; a nudge that moves no hover words stamps nothing
/// new; the landing stamps `(land, landed)` and the hold is not said beside
/// it. A headless `chrome` prints the hover as pushed beside the one
/// composed at the read (`pushed_tooltips=`), which a lab reads. NEGATIVE
/// CONTROLS (as run): `show_land_held` dirtying the menu alone (no
/// `restamp_for_hover`) leaves the pushed hover at `driven by aterm
/// (land)`; `refresh_window_tabs` not keeping what it pushed leaves
/// `chrome`'s `pushed_tooltips=[]`.
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

/// A NUDGE THE HOVER DOES NOT SAY STILL REACHES THE MENU AND THE OVERVIEW
/// (§12.3.1, the review of 3c): the hover's words carry no nudges, so a hold
/// that moves only by a nudge stamps no strip; the menu bar's landing tip
/// and the Overview's held line say the nudges, and both are composed where
/// the glance is rebuilt, so the move owes a rebuild all the same, and the
/// rebuilt words count the new nudge. NEGATIVE CONTROL (as run):
/// `show_land_held` without its `mark_status_dirty` (where the hover's words
/// stand) owes no rebuild, and the tip keeps `3 nudges` until something
/// unrelated dirties the glance.
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

/// A HOLD IS SHOWN ONLY WHILE THE WORD IS LAND (§12.3.10, 3b's contract):
/// the window owns the word, so a switch to Cruise hides a standing hold at
/// once — the hover says `(cruise)`, `status` prints the three `-`, the
/// worker row is no landing — though the loop tells `None` only at its next
/// read; switched back, the hold the glass kept shows again. NEGATIVE
/// CONTROL (as run): `shown_hold` without its face gate keeps the hold under
/// Cruise.
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

/// The hold on session 0's timeline, where `status` reads it.
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

/// A PERSON'S OWN EXCHANGE CLEARS THE HOLD (§12.3.3 item 3, §12.3.5): at
/// `task=theirs` the hold goes from the glass and the timeline alike,
/// whichever of the two facts the loop's point tells first. NEGATIVE
/// CONTROL (as run): the Task arm without its clear at Theirs keeps the hold
/// on the glass and the timeline.
#[test]
fn a_persons_own_exchange_clears_the_hold() {
    let (mut app, sid, _host) = desk("land");
    tell_hold(&mut app, &sid, Some(held()));
    assert_eq!(on_timeline(&app), Some(held()));
    app.apply_harness_fact(&sid, crate::HarnessFact::Task(Some(TaskFace::Theirs)));
    assert_eq!(app.harness_glass[&0].hold(), None, "off the glass");
    assert_eq!(on_timeline(&app), None, "off the timeline");
}

/// Session 0's claim handed from this host to a proven self-claim.
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

/// A PROVEN SELF-CLAIM CLEARS A STANDING HOLD (§12.3.3 item 3, Autopilot Land
/// §10.4): beside a proven self-claim the hosted loop's hold is not the
/// session's. A hold standing as the proof arrives goes at the hosted
/// loop's next hold fact, which shows none, at its next task face, which
/// moves nothing beside the proof, and at the self-claim's own first act.
/// NEGATIVE CONTROLS (as run): a LandHeld arm that only skips beside the
/// proof (as the Task arm skips the face) leaves the old legs on the glass
/// and the timeline; the Task arm without its clear beside the proof does
/// too; the SelfTask arm without its clear does too.
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

    // The hosted loop's next face, beside the proof: skipped, the hold gone.
    let (mut app, sid, _host) = desk("land");
    tell_hold(&mut app, &sid, Some(held()));
    prove_self_claim(&app);
    app.apply_harness_fact(&sid, crate::HarnessFact::Task(Some(TaskFace::Open)));
    assert_eq!(app.harness_glass[&0].hold(), None, "the hosted loop's face");
    assert_eq!(on_timeline(&app), None);

    let (mut app, sid, _host) = desk("land");
    tell_hold(&mut app, &sid, Some(held()));
    prove_self_claim(&app);
    // The act's face is queued where its row commits; the wake is a bell.
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

/// `status` PRINTS WHY LAND HOLDS, BY KEY, ONLY WHERE CLAIMED HERE (§12.3.1,
/// §12.6): `land_held=- land_nudges=- land_since=-` with no hold; before a
/// yield is read `land_held=-` beside the nudges and the act's second; after
/// one `land_held=needs,repo,open land_nudges=3 land_since=<s>`; all three
/// `-` once this host's claim is gone, under another process's claim, and
/// at the landing. `pause=` reads by key as before. NEGATIVE CONTROL (as
/// run): `land_fields` over the timeline's hold whatever the claim and the
/// word prints `land_held=needs,repo,open` with the claim released.
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

/// A WORKER READS `landing` ONLY AFTER ITS INSTRUCTION (§12.4.3 item 1,
/// §12.4.6; its tip §12.3.1): a worker whose session is landing reads `●
/// bench — landing 4m` (the task's age, as `working`'s), its tip the hold's
/// words with no clock; one not landing reads `working 4m`; its manager
/// counts `1 landing, 1 working`. The App fills the row from the hold it
/// shows: under Land with no hold (its first working turn) no landing, with
/// one a landing (and under Cruise none: `a_hold_is_shown_only_while_the_
/// word_is_land`). NEGATIVE CONTROLS (as run): `WorkerRow::word` without its
/// landing arm prints `● bench — working 4m` and `2 working`; a row's
/// `landing` made from the view and the open face alone is a landing
/// before the instruction.
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

    // The App fills the row from the hold it shows, no clock in the tip.
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

/// THE PAGE SAYS IT AS THE HOLD MOVES (§12.3.1): Settings ▸ Harness open,
/// the hold's wake alone republishes it, and its SESSIONS row's state line
/// says why Land holds. NEGATIVE CONTROL (as run): `show_land_held` without
/// `harness_screen_moved` leaves the line without the hold.
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

/// A GONE SESSION'S HOLD GOES WITH ITS GLASS (§12.3.3 item 3): the hold is
/// on the session's glass, and a retired session's glass goes. NEGATIVE
/// CONTROL (as run): the retire without `forget_harness_glass` keeps it.
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

/// A LAND HOLD DOES NOT GROW THE WAKE (§12.3.3 item 3): its fact carries
/// nothing, so `HarnessFact` stays main's 24 bytes and `Wake` does not grow
/// beyond the recorded main size. Unrelated field removals may shrink it.
/// NEGATIVE CONTROL (as run): a fact carrying the hold inline
/// (`LandHeld(Option<LandHeld>)`) grows `HarnessFact` to 40 bytes.
#[test]
fn a_land_hold_does_not_grow_the_wake() {
    /// `size_of::<HarnessFact>()` on main at 7607bb9d70.
    const MAIN_FACT_BYTES: usize = 24;
    /// `size_of::<Wake>()` on main at 7607bb9d70.
    const MAIN_WAKE_BYTES: usize = 4144;
    fn copy<T: Copy>() {}
    copy::<crate::HarnessFact>();
    assert_eq!(std::mem::size_of::<crate::HarnessFact>(), MAIN_FACT_BYTES);
    assert!(std::mem::size_of::<crate::Wake>() <= MAIN_WAKE_BYTES);
}
