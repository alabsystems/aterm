// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for rendering full `NEEDS YOU` sets (`needs::attention_outcome`).
use aterm_types::needs::AttentionKind;

use super::*;
use crate::App;

const NEED_A: &str = "grant Screen Recording to com.aterm.aterm in System Settings ▸ Privacy \
                      & Security ▸ Screen Recording, so the windowed check can capture the \
                      menu";
const NEED_B: &str = "the release credential (~/.aterm/release-credentials.toml) to sign 0.48.0";
const NEED_C: &str = "approve the 0.48.0 release notes";
const ENTRY_AB: &str = "claude needs you: grant Screen Recording to com.aterm.aterm in System \
                        Settings ▸ … (+1 more) \u{2014} carrying on";
const ENTRY_B: &str = "claude needs you: the release credential \
                       (~/.aterm/release-credentials.toml) to sign 0.48.0 \u{2014} carrying on";
const NEED_A_CUT: &str = "grant Screen Recording to com.aterm.aterm in System Settings ▸ …";

fn set(needs: &[&str]) -> Vec<String> {
    needs.iter().map(|need| (*need).to_string()).collect()
}

fn desk() -> (App, String, Arc<crate::SessionCtx>) {
    let mut app = App::headless_for_test();
    let ctx = app.pool.get(0).expect("session 0").ctx.clone();
    let sid = ctx.self_id.as_str().to_string();
    {
        let mut tl = ctx.timeline.lock().unwrap();
        tl.note_foreground_group(7);
        tl.set_program(7, Some("claude".into()));
    }
    app.refresh_presence_session(0);
    (app, sid, ctx)
}

fn tell(app: &mut App, sid: &str, needs: &[&str], entry: &str) {
    if store_needs(&app.store, sid, &set(needs), entry) {
        app.apply_harness_fact(sid, crate::HarnessFact::Needs);
    }
}

fn land(app: &mut App, ctx: &crate::SessionCtx, text: Option<&str>) {
    ctx.meta
        .lock()
        .unwrap()
        .set_attention_kind(
            NEEDS_ATTENTION_OWNER,
            text.map(str::to_string),
            text.map(|_| AttentionKind::Ask),
        )
        .expect("a slot");
    app.on_meta_changed(0);
}

fn needs_lines(app: &App) -> Vec<String> {
    let lines = app.status_item_chrome_lines();
    let mut section = false;
    let mut out = Vec::new();
    for line in lines {
        let Some(rest) = line.strip_prefix("statusmenu ") else {
            continue;
        };
        if rest.starts_with("\"Needs you (") {
            section = true;
            continue;
        }
        if !section {
            continue;
        }
        if rest.starts_with("---") {
            break;
        }
        out.push(
            rest.rsplit_once(" at=")
                .map_or_else(|| rest.to_string(), |(row, _)| row.to_string()),
        );
    }
    out
}

fn one_in(line: &str, need: &str) -> bool {
    line.strip_prefix(&format!(
        "{need:?} enabled=yes checked=no tip=\"Claude Code in "
    ))
    .is_some_and(|rest| rest.ends_with(" needs this too\" indent=1"))
}

fn hover(app: &App) -> Vec<String> {
    app.presence_hover(0, None)
        .map(|h| h.lines().map(str::to_string).collect())
        .unwrap_or_default()
}

#[test]
fn the_hover_lists_every_need_whole() {
    let (mut app, sid, ctx) = desk();
    tell(&mut app, &sid, &[NEED_A, NEED_B], ENTRY_AB);
    land(&mut app, &ctx, Some(ENTRY_AB));
    let lines = hover(&app);
    assert_eq!(
        lines.get(..3),
        Some(
            [
                format!("Needs you: {NEED_A}"),
                format!("Needs you: {NEED_B}"),
                "Claude Code carries on meanwhile".to_string(),
            ]
            .as_slice()
        ),
        "{lines:#?}"
    );
    assert!(
        !lines.iter().any(|l| l.starts_with("Needs attention")),
        "{lines:#?}"
    );
    let (mut app, _, ctx) = desk();
    land(&mut app, &ctx, Some(ENTRY_AB));
    let lines = hover(&app);
    assert_eq!(
        lines.get(..2),
        Some(
            [
                format!("Needs you: {NEED_A_CUT} (+1 more)"),
                "Claude Code carries on meanwhile".to_string(),
            ]
            .as_slice()
        ),
        "{lines:#?}"
    );
}

#[test]
fn the_rows_state_line_says_the_first_need_whole() {
    let (mut app, sid, ctx) = desk();
    tell(&mut app, &sid, &[NEED_A, NEED_B], ENTRY_AB);
    land(&mut app, &ctx, Some(ENTRY_AB));
    let line = app
        .harness_state()
        .projection()
        .sessions
        .iter()
        .find(|row| row.local_id == 0)
        .and_then(crate::harness_screen::HarnessSessionRow::state_line);
    assert_eq!(
        line.as_deref().and_then(|l| l.split(" \u{b7} ").next()),
        Some(format!("needs you: {NEED_A} (+1 more); carrying on").as_str()),
        "{line:?}"
    );
}

#[test]
fn a_set_that_moves_under_the_same_entry_moves_what_is_drawn() {
    let (mut app, sid, ctx) = desk();
    tell(&mut app, &sid, &[NEED_A, NEED_B], ENTRY_AB);
    land(&mut app, &ctx, Some(ENTRY_AB));
    let key = ctx
        .timeline
        .lock()
        .unwrap()
        .need()
        .need
        .as_ref()
        .map(|n| n.key);
    let lines = needs_lines(&app);
    assert_eq!(lines.len(), 2, "{lines:#?}");
    assert!(
        lines[0].starts_with("\"\u{25c7} ")
            && lines[0].contains(" \u{2014} grant Screen Recording"),
        "{lines:#?}"
    );
    assert!(!lines[0].contains("more)"), "{lines:#?}");
    assert!(!lines[0].contains("indent="), "{lines:#?}");
    assert!(one_in(&lines[1], NEED_B), "{lines:#?}");
    let status = app.session_status_record(0).expect("status");
    assert!(status.contains("needs=ask needs_n=2"), "{status}");

    tell(&mut app, &sid, &[NEED_A, NEED_C], ENTRY_AB);
    let moved = ctx.timeline.lock().unwrap().need().clone();
    assert_eq!(
        moved.harness_ask().map(|a| a.others.clone()),
        Some(set(&[NEED_C])),
        "the published need moved with the set"
    );
    assert_eq!(moved.need.as_ref().map(|n| n.key), key, "its key did not");
    let lines = needs_lines(&app);
    assert!(
        lines.get(1).is_some_and(|l| one_in(l, NEED_C)),
        "{lines:#?}"
    );
    assert!(!lines.iter().any(|l| l.contains(NEED_B)), "{lines:#?}");
    assert!(
        hover(&app).contains(&format!("Needs you: {NEED_C}")),
        "{:#?}",
        hover(&app)
    );
}

#[test]
fn the_outgoing_set_is_never_drawn_cut() {
    let (mut app, sid, ctx) = desk();
    let first_line = |app: &App| needs_lines(app).first().cloned().unwrap_or_default();
    tell(&mut app, &sid, &[NEED_A, NEED_B], ENTRY_AB);
    land(&mut app, &ctx, Some(ENTRY_AB));
    let whole = needs_lines(&app);
    tell(&mut app, &sid, &[NEED_B], ENTRY_B);
    app.refresh_presence_session(0);
    assert_eq!(
        needs_lines(&app),
        whole,
        "the standing entry keeps its words"
    );
    assert_eq!(
        hover(&app).first(),
        Some(&format!("Needs you: {NEED_A}")),
        "the hover too"
    );
    land(&mut app, &ctx, Some(ENTRY_B));
    let lines = needs_lines(&app);
    assert_eq!(lines.len(), 1, "{lines:#?}");
    assert!(
        first_line(&app).contains(&format!(" \u{2014} {NEED_B} \u{b7} ")),
        "{lines:#?}"
    );
    tell(&mut app, &sid, &[], "");
    app.refresh_presence_session(0);
    assert!(
        first_line(&app).contains(&format!(" \u{2014} {NEED_B} \u{b7} ")),
        "the entry that still stands keeps its words: {:#?}",
        needs_lines(&app)
    );
    land(&mut app, &ctx, None);
    assert_eq!(needs_lines(&app), Vec::<String>::new(), "gone");
}

/// A PRESS ON A FURTHER NEED'S ROW GOES TO ITS TAB (§12.2.6, §12.5.2's
/// press seam): with the needing session's tab behind another, the
/// stand-in for the person presses the row one level in by the path
/// `chrome` prints for it (`person menubar press <at>`); the click it posts
/// brings that session's tab in front, as a click on its own row does.
#[cfg(target_os = "macos")]
#[test]
fn a_press_on_a_further_needs_row_brings_its_tab_in_front() {
    let (mut app, sid, ctx) = desk();
    let wid = crate::WindowId(0);
    tell(&mut app, &sid, &[NEED_A, NEED_B], ENTRY_AB);
    land(&mut app, &ctx, Some(ENTRY_AB));
    let behind = app.next_session_id;
    app.push_stub_tab(wid, crate::stub_session(behind));
    assert_eq!(
        app.focused_session_id(wid),
        Some(behind),
        "PRECONDITION: another tab in front"
    );
    let lines = app.status_item_chrome_lines();
    let at = lines
        .iter()
        .find(|l| l.contains(" indent=1 at="))
        .and_then(|l| l.rsplit_once(" at="))
        .and_then(|(_, at)| crate::status_item::MenuAt::parse(at))
        .unwrap_or_else(|| panic!("a row one level in: {lines:#?}"));
    let mut posted = Vec::new();
    let pressed = app.person_menubar_press(&at, &mut |wake| {
        posted.push(wake);
        true
    });
    assert_eq!(pressed, Ok(format!("pressed {NEED_B:?}")));
    assert_eq!(posted.len(), 1, "one click posted: {posted:?}");
    for wake in posted {
        let crate::Wake::StatusAction { action } = wake else {
            panic!("a status action: {wake:?}");
        };
        app.dispatch_status_action(action);
    }
    assert_eq!(
        app.focused_session_id(wid),
        Some(0),
        "the needing session's tab in front"
    );
}

fn stamped_hover() -> String {
    crate::work_counts::last_tooltips_pushed(crate::WindowId(0))
        .and_then(|tips| tips.first().cloned().flatten())
        .unwrap_or_default()
}

fn live_desk() -> (App, String, Arc<crate::SessionCtx>) {
    live(desk())
}

/// `desk`'s slot past its first sight: a refresh that moves nothing stamps no strip.
fn live(
    (mut app, sid, ctx): (App, String, Arc<crate::SessionCtx>),
) -> (App, String, Arc<crate::SessionCtx>) {
    use crate::session_status::{ActivitySample, Evidence};
    let t0 = std::time::Instant::now();
    let reading = Evidence {
        shell: None,
        completed_block: None,
        lifecycle: None,
        foreground_job: Some(true),
        activity: ActivitySample {
            alt_screen: false,
            content_seq: 1,
            last_output: Some(t0),
        },
    };
    app.session_status.observe(0, &reading, t0);
    app.refresh_presence_session(0);
    assert!(
        app.session_status.status(0).is_some(),
        "PRECONDITION: a shell reading"
    );
    let _ = crate::work_counts::take_tooltip_pushes();
    app.refresh_presence_session(0);
    assert_eq!(
        crate::work_counts::take_tooltip_pushes(),
        0,
        "PRECONDITION: a refresh that moves nothing stamps no strip"
    );
    (app, sid, ctx)
}

#[test]
fn the_stamped_hover_follows_a_set_under_one_entry() {
    let (mut app, sid, ctx) = live_desk();
    tell(&mut app, &sid, &[NEED_A, NEED_B], ENTRY_AB);
    land(&mut app, &ctx, Some(ENTRY_AB));
    assert!(
        stamped_hover().contains(&format!("Needs you: {NEED_B}")),
        "PRECONDITION (b) stamped: {}",
        stamped_hover()
    );
    let _ = crate::work_counts::take_tooltip_pushes();
    tell(&mut app, &sid, &[NEED_A, NEED_C], ENTRY_AB);
    let stamps = crate::work_counts::take_tooltip_pushes();
    let now = stamped_hover();
    assert!(
        now.contains(&format!("Needs you: {NEED_C}")) && !now.contains(NEED_B),
        "stamps={stamps}; the stamped hover: {now}"
    );
    assert_eq!(stamps, 1, "one stamp for the set's move: {now}");
    tell(&mut app, &sid, &[NEED_A, NEED_B, NEED_C], ENTRY_AB);
    let stamps = crate::work_counts::take_tooltip_pushes();
    let now = stamped_hover();
    assert!(
        now.contains(&format!("Needs you: {NEED_B}"))
            && now.contains(&format!("Needs you: {NEED_C}")),
        "the stamped hover: {now}"
    );
    assert_eq!(
        stamps, 1,
        "one stamp where the need's words moved too: {now}"
    );
}

#[test]
fn the_stamped_hover_follows_a_first_tell_beside_a_standing_entry() {
    const SHORT_A: &str = "sign the release";
    const ENTRY_SHORT: &str = "claude needs you: sign the release (+1 more) \u{2014} carrying on";
    let (mut app, sid, ctx) = live_desk();
    land(&mut app, &ctx, Some(ENTRY_SHORT));
    assert!(
        stamped_hover().contains("(+1 more)"),
        "PRECONDITION the parse: {}",
        stamped_hover()
    );
    let _ = crate::work_counts::take_tooltip_pushes();
    tell(&mut app, &sid, &[SHORT_A, NEED_B], ENTRY_SHORT);
    let stamps = crate::work_counts::take_tooltip_pushes();
    let now = stamped_hover();
    assert!(
        now.contains(&format!("Needs you: {NEED_B}")) && !now.contains("(+1 more)"),
        "stamps={stamps}; the stamped hover: {now}"
    );
    assert_eq!(stamps, 1, "one stamp for the first tell: {now}");
}

/// A TAIL THAT ALONE MOVED RESTAMPS THE HOVER: a rewrite from `carrying on` to `waits on you` keeps
/// the need's words, and a write that lands between a refresh's publication and its read of the
/// facts (`drive::refresh_session`: `publish_and_age_need`, then `sense`) has that refresh absorb
/// the new entry under the old verdict, so the write's own wake publishes a need that differs
/// from the slot's by its key alone. That wake stamps the strip before it publishes, with the old
/// tail, so only the refresh's own restamp can carry the new one. CONTROL: once published, a
/// refresh moves nothing.
#[test]
fn the_stamped_hover_follows_a_tail_that_alone_moved() {
    use aterm_messages::presence::drive::Desk;
    const CARRYING: &str = "claude needs you: sign the release \u{2014} carrying on";
    const WAITING: &str = "claude needs you: sign the release \u{2014} waits on you";
    let (mut app, _, ctx) = live_desk();
    land(&mut app, &ctx, Some(CARRYING));
    assert!(
        stamped_hover().contains("carries on meanwhile"),
        "PRECONDITION the first tail: {}",
        stamped_hover()
    );
    ctx.meta
        .lock()
        .unwrap()
        .set_attention_kind(
            NEEDS_ATTENTION_OWNER,
            Some(WAITING.to_string()),
            Some(AttentionKind::Ask),
        )
        .expect("a slot");
    let sensed = Desk::sense(&app, 0).expect("session 0 is pooled");
    Desk::table_mut(&mut app).absorb(0, sensed, std::time::Instant::now());
    let _ = crate::work_counts::take_tooltip_pushes();
    app.on_meta_changed(0);
    let stamps = crate::work_counts::take_tooltip_pushes();
    let now = stamped_hover();
    assert!(
        now.contains("waits on you") && !now.contains("carries on meanwhile"),
        "stamps={stamps}; the stamped hover: {now}"
    );
    let sensed = Desk::sense(&app, 0).expect("session 0 is pooled");
    assert!(
        !Desk::table_mut(&mut app).absorb(0, sensed, std::time::Instant::now()),
        "CONTROL: nothing is left to move"
    );
}

#[test]
fn the_title_follows_the_needs_words_and_nothing_else() {
    use crate::claude_lights::glass_bits;
    let (mut app, sid, ctx) = desk();
    let generation = |app: &App| {
        app.harness_glass
            .get(&0)
            .map_or(0, crate::harness_glass::SessionGlass::trail_gen)
    };
    let at_rest = |app: &App| glass_bits(app.chip_glass(0), false);
    let (g0, rest) = (generation(&app), at_rest(&app));
    tell(&mut app, &sid, &[NEED_A, NEED_B], ENTRY_AB);
    land(&mut app, &ctx, Some(ENTRY_AB));
    let g1 = generation(&app);
    assert_ne!(g1, g0, "the need's first tell");
    assert_eq!(at_rest(&app), rest, "at rest the term keys nothing new");
    tell(&mut app, &sid, &[NEED_A, NEED_C], ENTRY_AB);
    assert_eq!(
        ctx.timeline
            .lock()
            .unwrap()
            .need()
            .harness_ask()
            .map(|a| a.others.clone()),
        Some(set(&[NEED_C])),
        "CONTROL: the published verdict moved with the set"
    );
    assert_eq!(generation(&app), g1, "the title's words did not");
    assert!(!app.publish_need(0), "CONTROL: nothing moved");
    assert_eq!(generation(&app), g1);
    tell(&mut app, &sid, &[NEED_B], ENTRY_B);
    land(&mut app, &ctx, Some(ENTRY_B));
    let g2 = generation(&app);
    assert_ne!(g2, g1, "a new first need");
    land(&mut app, &ctx, None);
    assert_ne!(generation(&app), g2, "the take-down");
    assert_eq!(at_rest(&app), rest, "at rest still nothing");
}

#[test]
fn the_chips_description_costs_no_more_for_nine_needs_than_for_two() {
    use crate::autopilot::AutopilotView;
    let (mut app, sid, ctx) = desk();
    let described = |app: &App| {
        let glass = app.chip_glass(0);
        let ((_, description), allocations) = crate::work_counts::allocations_in(|| {
            app.autopilot_spoken(0, AutopilotView::Cruise, &glass)
        });
        (description.expect("a need described"), allocations)
    };
    tell(&mut app, &sid, &[NEED_A, NEED_B], ENTRY_AB);
    land(&mut app, &ctx, Some(ENTRY_AB));
    let (two, two_cost) = described(&app);
    assert!(
        two.contains("And 1 more."),
        "CONTROL: two needs read: {two}"
    );
    let nine: Vec<String> = std::iter::once(NEED_A.to_string())
        .chain((1..9).map(|n| format!("{NEED_C}, part {n}")))
        .collect();
    let nine: Vec<&str> = nine.iter().map(String::as_str).collect();
    let entry = ENTRY_AB.replace("(+1 more)", "(+8 more)");
    tell(&mut app, &sid, &nine, &entry);
    land(&mut app, &ctx, Some(&entry));
    assert_eq!(
        ctx.timeline
            .lock()
            .unwrap()
            .need()
            .harness_ask()
            .map(|a| a.others.len()),
        Some(8),
        "CONTROL: the eight further needs are told whole"
    );
    let (nine, nine_cost) = described(&app);
    assert!(
        nine.contains("And 8 more."),
        "CONTROL: nine needs read: {nine}"
    );
    assert_eq!(
        nine_cost, two_cost,
        "the description's cost: {two:?} / {nine:?}"
    );
}

/// The `owner=needs` entry codex-alab sets for [`CODEX_NEEDS`], as it sets it.
const CODEX_ENTRY: &str = "Codex needs you: the release credential \
                           (~/.aterm/release-credentials.toml) to sign 0.48.0 (+2 more) \u{2014} \
                           carrying on";
const NEED_D: &str = "rotate the deploy key";
const CODEX_NEEDS: [&str; 3] = [NEED_B, NEED_C, NEED_D];

/// Session 0 as a codex-alab tab: codex in front, read with no state of its own.
fn codex_desk() -> (App, String, Arc<crate::SessionCtx>) {
    let mut app = App::headless_for_test();
    let ctx = app.pool.get(0).expect("session 0").ctx.clone();
    {
        let mut tl = ctx.timeline.lock().unwrap();
        tl.note_foreground_group(7);
        tl.set_program(7, Some("codex".into()));
    }
    app.refresh_presence_session(0);
    (app, ctx.self_id.as_str().to_string(), ctx)
}

/// `meta <rest>` on session 0 through the real dispatch, from connection 4 of the process
/// [`crate::self_claim::stand_in`] proves, the window woken as its wakes say.
fn self_meta(app: &mut App, rest: &str) -> String {
    let session = app.pool.get(0).expect("session 0");
    let (term, ctx) = (session.term.clone(), session.ctx.clone());
    let (reply, wakes) = crate::control::meta_wakes_proving_for_test(
        &app.store,
        &term,
        0,
        &ctx,
        rest,
        Some(4),
        &|_| Some(crate::self_claim::stand_in::running()),
    );
    for wake in wakes {
        match wake {
            crate::Wake::MetaChanged { session } => app.on_meta_changed(session),
            crate::Wake::HarnessFact { sid, fact } => app.apply_harness_fact(&sid, fact),
            other => panic!("a wake the meta lane sends: {other:?}"),
        }
    }
    reply
}

/// `meta set harness_needs` for `needs` beside `entry`, each percent-encoded.
fn needs_line(entry: &str, needs: &[&str]) -> String {
    let pct = aterm_control::wire::pct_encode;
    let needs: Vec<String> = needs
        .iter()
        .map(|need| format!("need={}", pct(need)))
        .collect();
    format!("set harness_needs entry={} {}", pct(entry), needs.join(" "))
}

/// A row one level in that says `need`, whoever's tab it names.
fn one_in_any(line: &str, need: &str) -> bool {
    line.strip_prefix(&format!("{need:?} enabled=yes checked=no tip=\""))
        .is_some_and(|rest| rest.ends_with(" needs this too\" indent=1"))
}

fn status_field(app: &App, key: &str) -> Option<String> {
    let line = app.session_status_record(0).expect("status");
    let prefix = format!("{key}=");
    line.split_whitespace()
        .find_map(|w| w.strip_prefix(prefix.as_str()))
        .map(str::to_owned)
}

/// A CODEX-ALAB'S OWN SET SHOWS EVERY NEED WHERE ATERM'S OWN LOOP'S DOES, WHILE ITS CLAIM STANDS
/// (§14.3.4): three needs told on its proven claim (`meta set harness_needs`) beside the entry it
/// set read whole on the menu bar, its first on the session's own row and the other two one level
/// in, a hover line each, `status needs=ask needs_n=3 needs_told=3`, and the set told before its
/// entry shows once the entry stands. NEGATIVE CONTROLS: the entry alone (no claim to tell it on,
/// and the claim with nothing told) shows the first need and a count, as before, with no row one
/// level in and `needs_told=-`; a set aterm's own loop left for the same entry is never shown
/// under the claim, and once the claim ends the window reads aterm's own loop's set again.
#[test]
fn a_codex_alabs_own_set_shows_every_need_while_its_claim_stands() {
    let (mut app, sid, ctx) = codex_desk();
    let first_and_count = |app: &App, case: &str| {
        let lines = needs_lines(app);
        assert_eq!(lines.len(), 1, "{case}: {lines:#?}");
        assert!(
            lines[0].contains(" (+2 more)"),
            "{case}: the entry's count: {lines:#?}"
        );
        let hover = hover(app);
        assert!(
            hover
                .first()
                .is_some_and(|l| l.starts_with("Needs you: ") && l.ends_with(" (+2 more)")),
            "{case}: {hover:#?}"
        );
        assert!(
            !hover.contains(&format!("Needs you: {NEED_C}")),
            "{case}: {hover:#?}"
        );
        assert_eq!(
            status_field(app, "needs_told").as_deref(),
            Some("-"),
            "{case}"
        );
    };
    let every_need = |app: &App, needs: [&str; 3], case: &str| {
        let lines = needs_lines(app);
        assert_eq!(lines.len(), 3, "{case}: {lines:#?}");
        assert!(
            lines[0].contains(&format!(" \u{2014} {}", needs[0])) && !lines[0].contains("more)"),
            "{case}: {lines:#?}"
        );
        assert!(
            one_in_any(&lines[1], needs[1]) && one_in_any(&lines[2], needs[2]),
            "{case}: {lines:#?}"
        );
        let hover = hover(app);
        assert_eq!(
            hover.get(..3),
            Some(needs.map(|need| format!("Needs you: {need}")).as_slice()),
            "{case}: {hover:#?}"
        );
        assert_eq!(
            (
                status_field(app, "needs").as_deref(),
                status_field(app, "needs_n").as_deref(),
                status_field(app, "needs_told").as_deref()
            ),
            (Some("ask"), Some("3"), Some("3")),
            "{case}"
        );
    };
    let entry_line = format!("set attention owner=needs kind=ask {CODEX_ENTRY}");
    assert_eq!(
        self_meta(&mut app, &needs_line(CODEX_ENTRY, &CODEX_NEEDS)),
        "ERR denied\n",
        "CONTROL: no claim to tell it on"
    );
    assert_eq!(self_meta(&mut app, &entry_line), "OK\n");
    first_and_count(&app, "CONTROL: no claim");
    let me = format!("codex-alab@{}", crate::self_claim::stand_in::pid());
    assert_eq!(self_meta(&mut app, &format!("set supervisor {me}")), "OK\n");
    first_and_count(&app, "CONTROL: claimed, nothing told");

    assert_eq!(
        self_meta(&mut app, &needs_line(CODEX_ENTRY, &CODEX_NEEDS)),
        "OK\n"
    );
    every_need(&app, CODEX_NEEDS, "told on the claim");
    tell(&mut app, &sid, &[NEED_D, NEED_C, NEED_B], CODEX_ENTRY);
    every_need(
        &app,
        CODEX_NEEDS,
        "aterm's own loop's set for the same entry is never read under the claim",
    );

    assert_eq!(self_meta(&mut app, "unset harness_needs"), "OK\n");
    assert_eq!(self_meta(&mut app, "unset attention owner=needs"), "OK\n");
    assert_eq!(needs_lines(&app), Vec::<String>::new(), "taken down");
    assert_eq!(
        self_meta(&mut app, &needs_line(CODEX_ENTRY, &CODEX_NEEDS)),
        "OK\n",
        "told before its entry stands"
    );
    assert_eq!(needs_lines(&app), Vec::<String>::new(), "no entry yet");
    assert_eq!(self_meta(&mut app, &entry_line), "OK\n");
    every_need(&app, CODEX_NEEDS, "the set told before its entry");

    assert!(crate::session_timeline::release_supervisor(
        &ctx,
        Some(4),
        crate::metrics::now_us(),
        aterm_slog::Actor::Aterm
    ));
    app.on_meta_changed(0);
    every_need(
        &app,
        [NEED_D, NEED_C, NEED_B],
        "CONTROL: the claim gone, aterm's own loop's set is read again",
    );
}

/// A CODEX-ALAB'S SET CUT AT 32 COUNTS EVERY NEED ITS ENTRY COUNTS, WITHIN THE MENU'S CAPS
/// (§14.3.4, §12.2.12): 41 needs stand, so codex-alab's entry counts `(+40 more)` and it tells the
/// first 32 (`HARNESS_NEEDS_MAX`). The menu bar draws the first on the session's row and only
/// `NEED_MORE_ROWS` (2) further rows one level in, the last counting the 38 past them; the hover
/// gives each of the 32 told a `Needs you:` line, the last counting the 9 nobody told; `status
/// needs_n=41`, `needs_told=32`. NEGATIVE CONTROL: the needs told are each read whole where a
/// surface lists them.
#[test]
fn a_codex_alabs_set_cut_at_32_counts_every_need_its_entry_counts() {
    // `control_session::HARNESS_NEEDS_MAX`, the most one line tells.
    const TOLD: usize = 32;
    let (mut app, _, _) = codex_desk();
    let needs: Vec<String> = (1..=41).map(|i| format!("need number {i}")).collect();
    let entry = "Codex needs you: need number 1 (+40 more) \u{2014} carrying on";
    let me = format!("codex-alab@{}", crate::self_claim::stand_in::pid());
    assert_eq!(self_meta(&mut app, &format!("set supervisor {me}")), "OK\n");
    assert_eq!(
        self_meta(
            &mut app,
            &format!("set attention owner=needs kind=ask {entry}")
        ),
        "OK\n"
    );
    let told: Vec<&str> = needs[..TOLD].iter().map(String::as_str).collect();
    assert_eq!(self_meta(&mut app, &needs_line(entry, &told)), "OK\n");

    let lines = needs_lines(&app);
    assert_eq!(
        lines.len(),
        3,
        "the session's row and two one level in: {lines:#?}"
    );
    assert!(
        lines[0].contains(" \u{2014} need number 1 ") && !lines[0].contains("more)"),
        "{lines:#?}"
    );
    assert!(
        one_in_any(&lines[1], "need number 2") && one_in_any(&lines[2], "need number 3 (+38 more)"),
        "{lines:#?}"
    );
    let hover = hover(&app);
    assert_eq!(
        hover.get(30..32),
        Some(
            [
                "Needs you: need number 31".to_string(),
                "Needs you: need number 32 (+9 more)".to_string()
            ]
            .as_slice()
        ),
        "{hover:#?}"
    );
    assert_eq!(
        (
            status_field(&app, "needs_n").as_deref(),
            status_field(&app, "needs_told").as_deref()
        ),
        (Some("41"), Some("32"))
    );
}

/// A CODEX-ALAB'S SET THAT MOVES UNDER ONE ENTRY RESTAMPS THE HOVER ONCE (§14.3.4, §12.2.12's
/// stamp): its `meta set harness_needs` wakes the window with the needs fact aterm's own loop's
/// tell sends, so `[b, c, d]` to `[b, a, d]` beside the same entry, which leaves the need's words
/// as they were, still moves the stamped hover, in one stamp. NEGATIVE CONTROL: the same set told
/// again stamps nothing.
#[test]
fn the_stamped_hover_follows_a_codex_alabs_set_under_one_entry() {
    let (mut app, _, _) = live(codex_desk());
    let me = format!("codex-alab@{}", crate::self_claim::stand_in::pid());
    assert_eq!(self_meta(&mut app, &format!("set supervisor {me}")), "OK\n");
    assert_eq!(
        self_meta(
            &mut app,
            &format!("set attention owner=needs kind=ask {CODEX_ENTRY}")
        ),
        "OK\n"
    );
    assert_eq!(
        self_meta(&mut app, &needs_line(CODEX_ENTRY, &CODEX_NEEDS)),
        "OK\n"
    );
    assert!(
        stamped_hover().contains(&format!("Needs you: {NEED_C}")),
        "PRECONDITION (c) stamped: {}",
        stamped_hover()
    );
    let _ = crate::work_counts::take_tooltip_pushes();
    assert_eq!(
        self_meta(
            &mut app,
            &needs_line(CODEX_ENTRY, &[NEED_B, NEED_A, NEED_D])
        ),
        "OK\n"
    );
    let stamps = crate::work_counts::take_tooltip_pushes();
    let now = stamped_hover();
    assert!(
        now.contains(&format!("Needs you: {NEED_A}")) && !now.contains(NEED_C),
        "stamps={stamps}; the stamped hover: {now}"
    );
    assert_eq!(stamps, 1, "one stamp for the set's move: {now}");
    assert_eq!(
        self_meta(
            &mut app,
            &needs_line(CODEX_ENTRY, &[NEED_B, NEED_A, NEED_D])
        ),
        "OK\n"
    );
    assert_eq!(
        crate::work_counts::take_tooltip_pushes(),
        0,
        "CONTROL: the standing set told again"
    );
}

/// The go-ahead's line (DESIGN-harness-autopilot §15.6): it parks the agent's own reversible work
/// on a person's go-ahead.
const GO_AHEAD_LINE: &str = "NEEDS YOU: say \"rerun the checks\" so I can push my branch to main.";
const SIGN_IN: &str = "sign agy and Muse in on this Mac (browser OAuth)";
const HOSTING: &str = "decide on public hosting of the fleet document";

/// A tab whose agent the window's own hosted loop carries under Cruise: the loop's claim on it,
/// the `owner=needs` entry it sets on the session's meta, and the `turn` that types its answer,
/// which the agent takes (its screen goes busy).
struct Carried {
    term: Arc<Mutex<aterm_core::terminal::Terminal>>,
    ctx: Arc<crate::SessionCtx>,
    /// The holder the loop's `meta set supervisor` claimed the tab for.
    supervisor: Option<String>,
}

impl aterm_agent::supervise::run::Ctl for Carried {
    fn call(&mut self, args: &[&str]) -> Result<aterm_agent::supervise::run::CtlReply, String> {
        let words: Vec<&str> = args
            .iter()
            .copied()
            .filter(|a| !a.starts_with('@'))
            .collect();
        let reply = |stdout: &str| aterm_agent::supervise::run::CtlReply {
            code: 0,
            stdout: stdout.to_string(),
            stderr: String::new(),
        };
        let seq = || self.term.lock().unwrap().content_seq();
        Ok(match words.as_slice() {
            ["meta"] => reply(&format!(
                "OK title=- user_title=- description=- icon=- role=- attention=- cwd=- \
                 state=alive attention_owner=- attention_owners=0 supervisor={} \
                 autopilot=cruise agent_cwd=-\n",
                self.supervisor.as_deref().unwrap_or("-")
            )),
            ["meta", "set", "supervisor", holder, ..] => {
                self.supervisor = Some((*holder).to_string());
                reply("OK\n")
            }
            ["meta", "set", "attention", owner, kind, text]
                if *owner == format!("owner={NEEDS_ATTENTION_OWNER}") =>
            {
                assert_eq!(*kind, "kind=ask");
                self.ctx
                    .meta
                    .lock()
                    .unwrap()
                    .set_attention_kind(
                        NEEDS_ATTENTION_OWNER,
                        Some((*text).to_string()),
                        Some(AttentionKind::Ask),
                    )
                    .expect("a slot");
                reply("OK\n")
            }
            ["meta", "unset", "attention", owner]
                if *owner == format!("owner={NEEDS_ATTENTION_OWNER}") =>
            {
                self.ctx
                    .meta
                    .lock()
                    .unwrap()
                    .set_attention_kind(NEEDS_ATTENTION_OWNER, None, None)
                    .expect("a slot");
                reply("OK\n")
            }
            ["text", "--json", rest @ ..] => {
                let args = crate::control::text_args(&rest.join(" "))?;
                reply(&crate::control::cmd_text_json_looked(
                    &self.term, &self.ctx, args,
                ))
            }
            ["status"] => reply(&format!(
                "OK schema=1 sid=- phase=running hold=0 fabric=connected program=claude agent=- \
                 seq={} hand=-\n",
                seq()
            )),
            ["await", "seq", ..] => {
                std::thread::sleep(Duration::from_millis(5));
                aterm_agent::supervise::run::CtlReply {
                    code: 124,
                    stdout: "OK timeout\n".to_string(),
                    stderr: String::new(),
                }
            }
            ["turn", ..] => {
                let cols = usize::from(self.term.lock().unwrap().cols());
                self.term.lock().unwrap().process(&working_screen(cols));
                reply(&format!("OK turn submitted=1 pressed=1 seq={}\n", seq()))
            }
            _ => reply("OK\n"),
        })
    }
}

/// The agent at work on the answer it took.
fn working_screen(cols: usize) -> Vec<u8> {
    let rule = "─".repeat(cols);
    let rows = [
        "⏺ Rerunning the checks on my branch.".to_string(),
        String::new(),
        "✻ Synthesizing… (18s · esc to interrupt)".to_string(),
        String::new(),
        rule.clone(),
        "❯ ".to_string(),
        rule,
        "  esc to interrupt".to_string(),
    ];
    format!("\x1b[2J\x1b[H{}\x1b[6;3H", rows.join("\r\n")).into_bytes()
}

/// What the window showed of a tab its hosted loop carries ([`carried_over`]).
struct Seen {
    /// The loop decided the owner's standing go-ahead, typed or waiting out its own back-off.
    acted: bool,
    journal: Vec<String>,
    /// The menu bar's `Needs you` rows, the tab's hover, its `status` and the whole menu.
    rows: Vec<String>,
    hover: Vec<String>,
    status: String,
    menu: Vec<String>,
}

/// The window's hosted loop carrying session 0 of a fresh window under Cruise and `cfg` over a
/// turn that ended on `said`: what the window shows once the loop decided the point (a point the
/// loop attaches to waits out the go-ahead's back-off, in real time), with the facts and the entry
/// it told applied, before the loop ends and takes its entry down.
fn carried_over(said: &str, cfg: SupervisorConfig, tag: &str) -> Seen {
    let mut app = App::headless_for_test();
    let app = &mut app;
    use std::sync::mpsc;
    let entry = app.pool.get(0).unwrap();
    let (term, ctx) = (entry.term.clone(), entry.ctx.clone());
    let sid = ctx.self_id.as_str().to_string();
    {
        let mut tl = ctx.timeline.lock().unwrap();
        tl.note_foreground_group(7);
        tl.set_program(7, Some("claude".into()));
    }
    ctx.meta
        .lock()
        .unwrap()
        .set("autopilot", Some("cruise".into()));
    let cols = usize::from(term.lock().unwrap().cols());
    term.lock()
        .unwrap()
        .process(&super::landed_restart_tests::ended_screen(said, cols));
    let dir = std::env::temp_dir().join(format!(
        "aterm-go-ahead-bar-{tag}-{}-{}",
        std::process::id(),
        aterm_agent::supervise::journal::unix_ms()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let ledger = dir.join(format!("{sid}.jsonl"));
    let journal = aterm_agent::supervise::approvals::journal_beside(&ledger);
    let (lane_term, lane_ctx) = (term, ctx);
    let body: Arc<BodyFn> = Arc::new(move |job: &WorkerJob| {
        let mut lane = Carried {
            term: Arc::clone(&lane_term),
            ctx: Arc::clone(&lane_ctx),
            supervisor: None,
        };
        let opts = SuperviseOpts {
            ledger: Some(ledger.clone()),
            journal: Some(aterm_agent::supervise::approvals::journal_beside(&ledger)),
            ..job.opts.clone()
        };
        let mut session =
            aterm_agent::supervise::run::Session::new(&mut lane, Some(format!("@{}", job.sid)));
        session.set_supervisor_name(Some(holder()));
        session.set_handover(Arc::clone(&job.handover));
        let mut out: Vec<u8> = Vec::new();
        match session.run_hosted(&opts, Arc::clone(&job.stop), &mut out) {
            Ok(()) => BodyEnd::Stopped,
            Err(e) => BodyEnd::Failed(e),
        }
    });
    let world = Arc::new(World::default());
    world.set(&[(sid.as_str(), Program::Claude)]);
    world.word(&sid, Some(Autopilot::Cruise));
    let (tx, facts) = mpsc::channel();
    let tx = Mutex::new(tx);
    let mut h = hooks(&world, body);
    let mine = sid.clone();
    h.fact = Arc::new(move |sid, fact| {
        if sid == mine {
            let _ = tx.lock().unwrap().send(fact);
        }
    });
    h.needs = store_hooks(&app.store).needs;
    app.refresh_presence_session(0);
    let host = HostHandle::start(cfg, false, false, h);
    let mut lines = Vec::new();
    let acted = aterm_spec::wait::within(
        aterm_spec::wait::MUST_HAPPEN,
        Duration::from_millis(5),
        || {
            lines = aterm_agent::supervise::journal::read_journal(&journal)
                .map(|(records, _)| records.into_iter().map(|r| r.line).collect::<Vec<_>>())
                .unwrap_or_default();
            lines
                .iter()
                .find(|l| {
                    l.starts_with("CONTINUED ")
                        || l.starts_with("ESCALATED ")
                        || (l.starts_with("WAITING ") && l.ends_with(" back-off"))
                })
                .map(|l| l.contains(" rule=go-ahead@v1 ") || l.starts_with("WAITING "))
        },
    );
    for fact in facts.try_iter() {
        app.apply_harness_fact(&sid, fact);
    }
    app.on_meta_changed(0);
    app.refresh_presence_session(0);
    let seen = Seen {
        acted: acted.unwrap_or_else(|| panic!("not within 60 s: the loop's act: {lines:#?}")),
        rows: needs_lines(app),
        hover: hover(app),
        status: app.session_status_record(0).expect("status"),
        menu: app.status_item_chrome_lines(),
        journal: lines,
    };
    host.shutdown_and_join();
    let _ = std::fs::remove_dir_all(&dir);
    seen
}

/// A GO-AHEAD THE HARNESS ANSWERS NEVER REACHES THE MENU BAR (DESIGN-harness-autopilot §15.6): the
/// window's own hosted loop carries a Claude Code tab under Cruise whose report parks on the
/// go-ahead beside a sign-in and a hosting decision. It decides the owner's standing go-ahead
/// (and waits out its back-off, as at any point it attaches to), and the menu bar's rows, the
/// tab's hover and `status` name the person's two needs and never the go-ahead's line. With the go-ahead's line alone nothing is on the menu bar. NEGATIVE CONTROL:
/// with answers off the go-ahead is not typed and the menu bar names all three.
#[test]
fn a_go_ahead_the_harness_answers_never_reaches_the_menu_bar() {
    let beside = format!(
        "Committed the parser fix on my branch.\n{GO_AHEAD_LINE}\nNEEDS YOU: {SIGN_IN}.\nNEEDS \
         YOU: {HOSTING}."
    );
    let seen = carried_over(&beside, on(), "beside");
    let journal = &seen.journal;
    assert!(seen.acted, "the go-ahead: {journal:#?}");
    for (what, text) in [
        ("rows", seen.rows.join("\n")),
        ("hover", seen.hover.join("\n")),
    ] {
        assert!(
            text.contains(SIGN_IN) && text.contains(HOSTING),
            "{what}: {text}\n{journal:#?}"
        );
        assert!(
            !text.contains("rerun the checks"),
            "{what}: {text}\n{journal:#?}"
        );
    }
    assert!(
        seen.status.contains(" needs=ask needs_n=2"),
        "{}\n{journal:#?}",
        seen.status
    );

    let alone = format!("Committed the parser fix on my branch.\n{GO_AHEAD_LINE}");
    let seen = carried_over(&alone, on(), "alone");
    let journal = &seen.journal;
    assert!(seen.acted, "the go-ahead: {journal:#?}");
    assert!(
        !seen.menu.iter().any(|l| l.contains("\"Needs you (")),
        "{:#?}\n{journal:#?}",
        seen.menu
    );
    assert!(
        seen.status.contains(" needs=- needs_n=-"),
        "{}\n{journal:#?}",
        seen.status
    );

    let mut answers_off = on();
    answers_off.set("answer_questions", "false").unwrap();
    let seen = carried_over(&beside, answers_off, "answers-off");
    let journal = &seen.journal;
    assert!(!seen.acted, "no go-ahead with answers off: {journal:#?}");
    let rows = seen.rows.join("\n");
    assert!(
        rows.contains("rerun the checks"),
        "NEGATIVE CONTROL: {rows}\n{journal:#?}"
    );
    assert!(
        seen.status.contains(" needs=ask needs_n=3"),
        "{}\n{journal:#?}",
        seen.status
    );
}
