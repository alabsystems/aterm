// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! EVERY NEED WHOLE (`docs/DESIGN-harness-autopilot-2026-10-15.md` §12.2.1,
//! §12.2.3 item 4, §12.2.6; slice 2d): the window reads the worker's NEEDS
//! YOU set its loop told it (slice 2c's store) wherever that set was told
//! with the needs entry that stands, and says every need whole where there is
//! room — the menu bar's Needs you section, a row one level in for each
//! further need, the tab's hover, a line each — through the one published
//! verdict (`needs::attention_outcome`, `App::need_facts_with`).
//!
//! A real headless App's session 0 stands as a Claude Code; the loop's tell
//! is the live hook (`store_needs`) over the App's store, and its request is
//! the entry written as `meta set attention owner=needs kind=ask` writes it.
//! The menu is read as `chrome` prints it (`App::status_item_chrome_lines`),
//! the hover as the strip is stamped with it (`App::presence_hover`). Each
//! test names its NEGATIVE CONTROL: the mutant it was run against, and what
//! failed.

use aterm_types::needs::AttentionKind;

use super::*;
use crate::App;

/// Need (a) of §12.2.1: 144 characters, longer than the entry's 64 cells.
const NEED_A: &str = "grant Screen Recording to com.aterm.aterm in System Settings ▸ Privacy \
                      & Security ▸ Screen Recording, so the windowed check can capture the \
                      menu";
/// Need (b) of §12.2.1.
const NEED_B: &str = "the release credential (~/.aterm/release-credentials.toml) to sign 0.48.0";
/// A third need, standing in for (b) under the same entry.
const NEED_C: &str = "approve the 0.48.0 release notes";
/// The entry the loop raises for (a) and one more: its writer's form, the
/// first need cut at 64 cells (`escalate::needs_attention_text`).
const ENTRY_AB: &str = "claude needs you: grant Screen Recording to com.aterm.aterm in System \
                        Settings ▸ … (+1 more) \u{2014} carrying on";
/// The entry for (b) alone.
const ENTRY_B: &str = "claude needs you: the release credential \
                       (~/.aterm/release-credentials.toml) to sign 0.48.0 \u{2014} carrying on";
/// (a) as its entry carries it, cut at 64 cells.
const NEED_A_CUT: &str = "grant Screen Recording to com.aterm.aterm in System Settings ▸ …";

fn set(needs: &[&str]) -> Vec<String> {
    needs.iter().map(|need| (*need).to_string()).collect()
}

/// A headless App whose session 0 is a Claude Code: the App, its sid and
/// its context.
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

/// The loop's tell: the live hook over the App's store, then the window
/// woken as the host wakes it where a reading moved (`HarnessFact::Needs`).
fn tell(app: &mut App, sid: &str, needs: &[&str], entry: &str) {
    if store_needs(&app.store, sid, &set(needs), entry) {
        app.apply_harness_fact(sid, crate::HarnessFact::Needs);
    }
}

/// The loop's request landing: the needs owner's entry written (`None`: its
/// `unset`), and its `MetaChanged` wake taken (`on_meta_changed`).
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

/// The Needs you section's rows as `chrome` prints them, each without its
/// path (` at=…`, always last).
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

/// Whether `line` (a `needs_lines` row) is `need`'s row one level in: the
/// need whole, acting, its hover naming whose need it is, ` indent=1`.
fn one_in(line: &str, need: &str) -> bool {
    line.strip_prefix(&format!(
        "{need:?} enabled=yes checked=no tip=\"Claude Code in "
    ))
    .is_some_and(|rest| rest.ends_with(" needs this too\" indent=1"))
}

/// The hover's lines for session 0.
fn hover(app: &App) -> Vec<String> {
    app.presence_hover(0, None)
        .map(|h| h.lines().map(str::to_string).collect())
        .unwrap_or_default()
}

/// THE HOVER LISTS EVERY NEED WHOLE (§12.2.1, §12.2.6): with (a) and (b)
/// told and their entry up, the tab's hover says `Needs you: <need>` for
/// each, whole — (a) at its 144 characters, past the entry's 64 cells — then
/// `Claude Code carries on meanwhile`, and never `Needs attention:`. Where
/// the window holds only the entry's parse (no set told with it), it says
/// the one need it has, cut, with ` (+1 more)`. NEGATIVE
/// CONTROL (run as a mutant for the landing): `harness_need_hover` saying
/// the first need alone (2a's one line) drops (b)'s line.
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
    // An entry no set was told with (a loop before this build's, a
    // successor's carried entry): the parse's one line.
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

/// THE ROW'S STATE LINE SAYS THE FIRST NEED WHOLE (§12.2.1's SESSIONS row):
/// with (a) and (b) told and their entry up, session 0's state line on
/// Settings ▸ Harness leads with `needs you: <(a) whole> (+1 more); carrying
/// on` — the page draws no row for (b), so it counts it. NEGATIVE CONTROL
/// (run as a mutant for the landing): `harness_ask` ignoring the told set
/// leaves (a) at the entry's 64 cells.
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

/// A SET THAT MOVES UNDER THE SAME ENTRY MOVES WHAT IS DRAWN (§12.2.6):
/// `[a, b]` up, then `[a, c]` told under the very same entry text — which
/// no server write announces — moves the published verdict (its key
/// unchanged: one need, one age, one notice), the menu's row one level in
/// from (b) to (c), and the hover; `status needs_n=` counts the set. The
/// session's own row reads (a) clipped with its age, and no ` (+N more)`,
/// since its other need has a row. NEGATIVE CONTROL (run as a mutant for
/// the landing): `harness_ask` ignoring the told set leaves the menu with
/// no row one level in and the session's row `(+1 more)`.
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

/// THE OUTGOING SET IS NEVER DRAWN CUT (§12.2.11, the review of 2c): from
/// the tell of a new set until its request lands, the entry that stands is
/// still the old set's, and every surface keeps saying the old set whole —
/// at a change of the set (`[a, b]` to `[b]`) and at the take-down — then
/// the new one whole the moment its request lands, never the old entry's
/// 64 cells in between. NEGATIVE CONTROL (run as a mutant for the landing):
/// `NeedFacts::told_needs` reading the set told last alone (no outgoing
/// set) draws (a) cut, with ` (+1 more)` and no row for (b), in the gap.
#[test]
fn the_outgoing_set_is_never_drawn_cut() {
    let (mut app, sid, ctx) = desk();
    let first_line = |app: &App| needs_lines(app).first().cloned().unwrap_or_default();
    tell(&mut app, &sid, &[NEED_A, NEED_B], ENTRY_AB);
    land(&mut app, &ctx, Some(ENTRY_AB));
    let whole = needs_lines(&app);
    // The next report names (b) alone: told, its `meta set` not landed. A
    // publication in the gap (any fact, here the session's own refresh).
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
    // The take-down: told, its `unset` not yet landed.
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
/// NEGATIVE CONTROL (run as a mutant for the landing): rows one level in
/// built as information lines answer `not an action`, and nothing moves.
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

/// The hover AS THE STRIP WAS LAST STAMPED with it
/// (`work_counts::last_tooltips_pushed`: the words the native tooltip
/// shows), never as [`App::presence_hover`] composes it on the call.
fn stamped_hover() -> String {
    crate::work_counts::last_tooltips_pushed(crate::WindowId(0))
        .and_then(|tips| tips.first().cloned().flatten())
        .unwrap_or_default()
}

/// The desk with a shell reading folded into session 0's presence slot, as
/// every live session has: past the slot's first sight, a refresh that
/// moves nothing stamps no strip (the bare [`desk`] stamps at every one).
fn live_desk() -> (App, String, Arc<crate::SessionCtx>) {
    use crate::session_status::{ActivitySample, Evidence};
    let (mut app, sid, ctx) = desk();
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

/// THE STAMPED HOVER FOLLOWS A SET THAT MOVES UNDER ONE ENTRY (§12.2.1; the
/// review of 2d): the tab's hover is composed only where a strip is
/// stamped, and a refresh stamps only where the pill's words moved. `[a,
/// b]` to `[a, c]` under the same entry leaves the pill (still (a) and one
/// more), so the Needs fact stamps the strip ONCE itself, and the stamped
/// hover says (c), never (b); a tell that moves the pill too (`[a, b, c]`:
/// two more) is stamped once, by the refresh alone. NEGATIVE CONTROLS (run
/// as mutants for the landing): the Needs arm refreshing alone (2c's)
/// stamps nothing, and the hover keeps (b); the arm stamping whatever the
/// refresh did stamps the pill's move twice.
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
    // A third need: the pill's words move (`2 more needs`), so the
    // refresh stamps, and the Needs arm adds none.
    tell(&mut app, &sid, &[NEED_A, NEED_B, NEED_C], ENTRY_AB);
    let stamps = crate::work_counts::take_tooltip_pushes();
    let now = stamped_hover();
    assert!(
        now.contains(&format!("Needs you: {NEED_B}"))
            && now.contains(&format!("Needs you: {NEED_C}")),
        "the stamped hover: {now}"
    );
    assert_eq!(stamps, 1, "one stamp where the pill moved too: {now}");
}

/// THE STAMPED HOVER FOLLOWS A FIRST TELL BESIDE A STANDING ENTRY (§12.2.1,
/// §12.2.5; the review of 2d): a successor's first tell restates the entry
/// it carried, which no server write announces, and the pill reads the same
/// (the first need and one more); the Needs fact stamps the strip once, and
/// the stamped hover moves from the parse's one line (`(+1 more)`) to a line
/// each. NEGATIVE CONTROL (run as a mutant for the landing): the Needs arm
/// refreshing alone (2c's) stamps nothing, and the hover keeps `(+1 more)`.
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
