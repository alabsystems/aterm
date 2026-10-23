// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What the footer's per-frame pass costs on one Claude Code pane ([`super::probe`]'s glass): at rest nothing.

use super::*;

use super::probe::*;

const PLAN: &str = "  \u{23F8} plan mode on (shift+tab to cycle)";

/// What one pass cost the footer, counter by counter.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Cost {
    allocations: usize,
    /// Rebuilds of the pane's text.
    rebuilds: usize,
    /// The pane's rows read for the footer and the lights.
    reads: usize,
    /// The composer's frame searched for again to keep a reading.
    searches: usize,
    /// The rule read off the frame for its coverable cells.
    masks: usize,
    /// Launch cards read for an open model or effort.
    cards: usize,
    /// Footer rules fitted and composed.
    composed: usize,
    /// Chip blocks fitted (`App::claude_lights_block`), as against a
    /// pane's block kept from a frame of the same fit key.
    fits: usize,
    /// Pane rows whose text a rebuild compared, as against rows the
    /// engine's revisions vouch unchanged.
    compared: usize,
    /// Chrome band tones derived.
    tones: usize,
    /// Autopilot chip titles built (`claude_lights::autopilot_title`).
    titles: usize,
}

fn cost(app: &mut App, wid: WindowId) -> Cost {
    let (plan, route) = app.active_visible_frame_layout(wid).expect("a layout");
    let _ = (
        crate::work_counts::take_pane_text_rebuilds(),
        crate::work_counts::take_footer_pane_reads(),
        crate::work_counts::take_footer_frame_searches(),
        crate::work_counts::take_footer_rule_masks(),
        crate::work_counts::take_footer_card_reads(),
        crate::work_counts::take_footer_rules_composed(),
        crate::work_counts::take_footer_block_fits(),
        crate::work_counts::take_footer_rows_compared(),
        crate::work_counts::take_band_derives(),
        crate::work_counts::take_autopilot_titles_built(),
    );
    let (_, allocations) = crate::work_counts::allocations_in(|| pass_on(app, wid, &plan, route));
    Cost {
        allocations,
        rebuilds: crate::work_counts::take_pane_text_rebuilds(),
        reads: crate::work_counts::take_footer_pane_reads(),
        searches: crate::work_counts::take_footer_frame_searches(),
        masks: crate::work_counts::take_footer_rule_masks(),
        cards: crate::work_counts::take_footer_card_reads(),
        composed: crate::work_counts::take_footer_rules_composed(),
        fits: crate::work_counts::take_footer_block_fits(),
        compared: crate::work_counts::take_footer_rows_compared(),
        tones: crate::work_counts::take_band_derives(),
        titles: crate::work_counts::take_autopilot_titles_built(),
    }
}

fn glass_row(app: &App, wid: WindowId, row: usize) -> String {
    let ws = &app.windows[&wid];
    let strip = ws.input_scratch.cells.len() - usize::from(ws.rows);
    read_glass(&ws.input_scratch.cells[strip + row])
}

#[test]
fn selected_subscription_repaints_without_old_account_data_or_extra_rows() {
    let (mut app, wid, session) = probe_glass();
    let rule = AT_REST.bottom();
    let mode = glass_row(&app, wid, rule + 1);
    let selected = |email: &str, plan: &str, weekly: Option<&str>| FooterFacts {
        subscription: Some(aterm_agent::harness::profile::SubscriptionDisplay {
            email: Some(email.into()),
            plan: Some(plan.into()),
            weekly: weekly.map(str::to_owned),
            ..Default::default()
        }),
        ..facts()
    };
    publish(
        &app,
        session,
        selected("first@example.com", "Max 5x", Some("weekly 29% left")),
    );
    assert_eq!(cost(&mut app, wid).composed, 1);
    let first = glass_row(&app, wid, rule);
    assert!(first.contains("first@example.com"), "{first:?}");
    assert!(first.contains("Max 5x"), "{first:?}");
    assert!(first.contains("weekly 29% left"), "{first:?}");
    assert_eq!(glass_row(&app, wid, rule + 1), mode);
    assert_eq!(cost(&mut app, wid), Cost::default());

    publish(&app, session, selected("second@example.com", "Pro", None));
    assert_eq!(cost(&mut app, wid).composed, 1);
    let second = glass_row(&app, wid, rule);
    assert!(second.contains("second@example.com"), "{second:?}");
    assert!(second.contains("Pro"), "{second:?}");
    assert!(second.contains("weekly unavailable"), "{second:?}");
    assert!(
        !second.contains("first@example.com") && !second.contains("29%"),
        "{second:?}"
    );
    assert_eq!(glass_row(&app, wid, rule + 1), mode);
    assert_eq!(cost(&mut app, wid), Cost::default());
}

#[test]
fn unicode_subscription_text_survives_the_native_splice_and_rests_without_allocations() {
    for email in [
        "jose\u{0301}@example.com",
        "x\u{0301}\u{0323}@example.com",
        "使用者-jose\u{0301}-x\u{0301}\u{0323}-\u{1f469}\u{200d}\u{1f4bb}@example.com",
    ] {
        let facts = FooterFacts {
            subscription: Some(aterm_agent::harness::profile::SubscriptionDisplay {
                email: Some(email.into()),
                plan: Some("Pro".into()),
                weekly: Some("weekly 29% left".into()),
                ..Default::default()
            }),
            ..FooterFacts::default()
        };
        let (mut app, wid, _) = glass_with(&AT_REST, facts);
        let reading = |app: &App| {
            let ws = &app.windows[&wid];
            let input = &ws.input_scratch;
            let row = input.cells.len() - usize::from(ws.rows) + AT_REST.bottom();
            let mut text = String::new();
            for (col, cell) in input.cells[row].iter().enumerate() {
                if cell.wide || cell.ch.is_whitespace() {
                    continue;
                }
                if let Some(cluster) = input.cluster_at(row, col) {
                    text.push_str(cluster);
                } else {
                    text.push(cell.ch);
                    if let Some(marks) = input.combining_at(row, col) {
                        text.extend(marks);
                    }
                }
            }
            text
        };
        assert!(
            reading(&app).contains(email),
            "{email:?}: {:?}",
            reading(&app)
        );
        if email.contains('\u{0301}') {
            let input = &app.windows[&wid].input_scratch;
            let mut renderer = aterm_render::Renderer::from_bytes(
                aterm_render::embedded_font(),
                24.0,
                aterm_render::Theme::default(),
            )
            .expect("bundled font");
            renderer.debug_block_on_lazy_fallbacks();
            let marked = renderer.render_input(input);
            let mut bare = input.clone();
            let row = bare.cells.len() - usize::from(app.windows[&wid].rows) + AT_REST.bottom();
            bare.combining[row].clear();
            assert_ne!(
                marked.pixels,
                renderer.render_input(&bare).pixels,
                "{email:?}: the accent paints ink"
            );
            if email.contains('👩') {
                let at = input.cells[row]
                    .iter()
                    .position(|cell| cell.ch == '@')
                    .unwrap();
                let mut without_at = input.clone();
                without_at.cells[row][at].ch = ' ';
                let without = renderer.render_input(&without_at);
                let differences = |a: &aterm_render::Frame, b: &aterm_render::Frame| {
                    a.pixels
                        .iter()
                        .zip(&b.pixels)
                        .enumerate()
                        .filter_map(|(index, (a, b))| (a != b).then_some(index))
                        .collect::<Vec<_>>()
                };
                let actual = differences(&marked, &without);
                let mut isolated = without_at;
                for cell in &mut isolated.cells[row] {
                    cell.ch = ' ';
                    cell.wide = false;
                }
                isolated.clusters[row].clear();
                isolated.combining[row].clear();
                let empty = renderer.render_input(&isolated);
                isolated.cells[row][at] = input.cells[row][at];
                isolated.cells[row][at].script = aterm_core::terminal::Script::Sub { nudge: 0 };
                let reference = renderer.render_input(&isolated);
                let expected = differences(&reference, &empty);
                assert!(!expected.is_empty(), "the isolated @ paints ink");
                assert_eq!(
                    actual, expected,
                    "the emoji must leave every native-position @ pixel visible"
                );
            }
        }
        for frame in 0..3 {
            assert_eq!(
                cost(&mut app, wid),
                Cost::default(),
                "{email:?}, frame {frame}"
            );
            assert!(reading(&app).contains(email), "{email:?}, frame {frame}");
        }
    }
}

#[test]
fn a_frame_at_rest_costs_the_footer_nothing() {
    let (mut app, wid, session) = probe_glass();
    let rule = AT_REST.bottom();
    let at_rest = glass_row(&app, wid, rule);
    assert!(
        at_rest.contains("Opus 5.5 xhigh") && at_rest.contains("main"),
        "the facts are on the rule: {at_rest:?}"
    );
    for i in 0..30 {
        assert_eq!(cost(&mut app, wid), Cost::default(), "frame {i}");
        assert_eq!(
            glass_row(&app, wid, rule),
            at_rest,
            "frame {i}: the same glass"
        );
    }
    let more = FooterFacts {
        usage: usage(49_000_000),
        ..facts()
    };
    let branch = FooterFacts {
        branch: Some("perf/rest".into()),
        ..more.clone()
    };
    let key = pass(&mut app, wid);
    assert_eq!(pass(&mut app, wid), key, "the repaint key stands at rest");
    let mut keys = vec![key];
    for (what, facts) in [("49M", more), ("perf/rest", branch)] {
        publish(&app, session, facts);
        let (plan, _) = app.active_visible_frame_layout(wid).expect("a layout");
        let key = app.claude_footer_fp(wid, &plan);
        assert!(
            !keys.contains(&key),
            "CONTROL {what}: the repaint key moved"
        );
        keys.push(key);
        let moved = cost(&mut app, wid);
        assert!(
            moved.composed == 1 && moved.allocations > 0 && moved.reads == 0,
            "CONTROL {what}: composed anew, the pane not read again ({moved:?})"
        );
        assert!(
            glass_row(&app, wid, rule).contains(what),
            "CONTROL {what}: on the glass: {:?}",
            glass_row(&app, wid, rule)
        );
        assert_eq!(
            cost(&mut app, wid),
            Cost::default(),
            "{what}: at rest again"
        );
        assert_eq!(
            pass(&mut app, wid),
            key,
            "{what}: the key it moved to stands"
        );
    }
    let mut fresh = App::headless_for_test();
    let fresh_wid = WindowId(0);
    let fresh_session = fresh.focused_session_id(fresh_wid).expect("a session");
    assert!(fresh.apply_term_resize(fresh_wid, ROWS, COLS));
    feed(&fresh, fresh_session, &screen(&AT_REST));
    publish(&fresh, fresh_session, facts());
    refill(&mut fresh, fresh_wid);
    let first = cost(&mut fresh, fresh_wid);
    assert!(
        first.rebuilds == 1
            && first.reads == 1
            && first.masks == 1
            && first.fits == 1
            && first.composed == 1,
        "CONTROL: the first frame reads, fits and composes ({first:?})"
    );
}

#[test]
fn a_hovered_chip_costs_its_title_alone() {
    use crate::claude_lights::Chip;
    let (mut app, wid, session) = probe_glass();
    let _host = hosted(&mut app);
    for _ in 0..2 {
        let _ = pass(&mut app, wid);
    }
    let rule = AT_REST.bottom();
    assert!(
        glass_row(&app, wid, rule).contains('\u{221e}'),
        "CONTROL: Autopilot's chip on the rule: {:?}",
        glass_row(&app, wid, rule)
    );
    assert_eq!(
        cost(&mut app, wid),
        Cost::default(),
        "CONTROL: chips at rest"
    );
    app.hover_claude_chip(wid, Some((session, Chip::Autopilot)));
    let first = cost(&mut app, wid);
    assert!(
        first.allocations > 0,
        "CONTROL: the hover's first frame lays its title ({first:?})"
    );
    for _ in 0..2 {
        let _ = pass(&mut app, wid);
    }
    let hovered = glass_row(&app, wid, rule);
    let (whole, _) = crate::claude_lights::autopilot_title(
        crate::autopilot::AutopilotView::Cruise,
        &app.chip_glass(session),
        "",
        None,
        crate::claude_lights::TitleWords::default(),
    );
    assert!(
        !hovered.contains(&whole)
            && crate::claude_lights::lean_titles(&whole)
                .iter()
                .filter(|(head, _)| !head.is_empty())
                .any(|(head, tail)| hovered.contains(&format!("{head}{tail} "))),
        "CONTROL: a leaner form is the one laid: {hovered:?}"
    );
    for i in 0..12 {
        assert_eq!(cost(&mut app, wid), Cost::default(), "hovered, frame {i}");
    }
    app.hover_claude_chip(wid, None);
    for _ in 0..2 {
        let _ = pass(&mut app, wid);
    }
    assert_eq!(
        cost(&mut app, wid),
        Cost::default(),
        "CONTROL: the pointer gone"
    );
}

#[test]
fn a_frame_at_rest_with_fact_targets_costs_nothing() {
    use crate::claude_lights::Fact;
    let (mut app, wid, session) = probe_glass();
    let targets = |app: &App| app.windows[&wid].claude_lights.fact_hits.clone();
    let at_rest = targets(&app);
    let kinds: Vec<Fact> = at_rest.iter().map(|h| h.fact).collect();
    assert_eq!(
        kinds,
        [
            Fact::Model,
            Fact::Effort,
            Fact::Path,
            Fact::Branch,
            Fact::Usage
        ],
        "CONTROL: every fact is a target"
    );
    for i in 0..30 {
        assert_eq!(cost(&mut app, wid), Cost::default(), "frame {i}");
        assert_eq!(targets(&app), at_rest, "frame {i}: the same targets");
    }
    let key = pass(&mut app, wid);
    app.set_fact_mask(session, FactMask::default().with(FactKind::Branch, true));
    assert_ne!(pass_key(&app, wid), key, "CONTROL: the repaint key moved");
    let hidden = cost(&mut app, wid);
    assert_eq!(hidden.composed, 1, "CONTROL: composed once ({hidden:?})");
    let rule = AT_REST.bottom();
    assert!(
        glass_row(&app, wid, rule).contains("\u{2387} main off"),
        "CONTROL: the stub on the glass: {:?}",
        glass_row(&app, wid, rule)
    );
    let stub = targets(&app);
    assert!(
        stub.iter()
            .any(|h| h.fact == Fact::Branch && h.restores.has(FactKind::Branch)),
        "CONTROL: the stub is the target: {stub:?}"
    );
    for i in 0..12 {
        assert_eq!(cost(&mut app, wid), Cost::default(), "hidden, frame {i}");
    }
}

fn context_screen(left: u8) -> Vec<String> {
    let mut rows = screen(&AT_REST);
    let top = rows
        .iter()
        .position(|r| r.starts_with('\u{2500}'))
        .expect("the composer's top rule");
    let text = format!("{left}% until auto-compact");
    rows[top - 1] = format!("{}{text}", " ".repeat(usize::from(COLS) - 2 - text.len()));
    rows
}

#[test]
fn a_context_reading_at_rest_costs_nothing() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let session = app.focused_session_id(wid).expect("the front session");
    assert!(app.apply_term_resize(wid, ROWS, COLS));
    feed(&app, session, &context_screen(12));
    publish(&app, session, facts());
    {
        let entry = app.pool.get(session).expect("session");
        let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        assert!(timeline.set_program(4242, Some("claude".into())));
    }
    for _ in 0..3 {
        refill(&mut app, wid);
        let _ = pass(&mut app, wid);
    }
    let rule = AT_REST.bottom();
    let at_rest = glass_row(&app, wid, rule);
    assert!(
        at_rest.contains("\u{25D0} ctx 12% left"),
        "the reading is on the rule: {at_rest:?}"
    );
    for i in 0..30 {
        assert_eq!(cost(&mut app, wid), Cost::default(), "frame {i}");
        assert_eq!(glass_row(&app, wid, rule), at_rest, "frame {i}");
    }
    feed(&app, session, &context_screen(11));
    refill(&mut app, wid);
    let moved = cost(&mut app, wid);
    assert!(
        moved.allocations > 0,
        "CONTROL: the new reading's words allocate ({moved:?})"
    );
    refill(&mut app, wid);
    let _ = pass(&mut app, wid);
    let now = glass_row(&app, wid, rule);
    assert!(
        now.contains("\u{25D0} ctx 11% left"),
        "CONTROL: the new reading is on the rule: {now:?}"
    );
    for i in 0..12 {
        assert_eq!(cost(&mut app, wid), Cost::default(), "moved, frame {i}");
    }
}

#[test]
fn a_hover_change_composes_once_then_rests() {
    use crate::claude_lights::Fact;
    let (mut app, wid, session) = probe_glass();
    assert_eq!(cost(&mut app, wid), Cost::default(), "CONTROL: at rest");
    app.hover_claude_fact(wid, Some((session, Fact::Branch)));
    let first = cost(&mut app, wid);
    assert_eq!(first.composed, 1, "the hover composes once ({first:?})");
    assert_eq!(first.reads, 0, "and reads no pane ({first:?})");
    for i in 0..12 {
        assert_eq!(cost(&mut app, wid), Cost::default(), "hovered, frame {i}");
    }
    app.hover_claude_fact(wid, None);
    let left = cost(&mut app, wid);
    assert_eq!(
        left.composed, 1,
        "the pointer leaving composes once more ({left:?})"
    );
    for i in 0..12 {
        assert_eq!(cost(&mut app, wid), Cost::default(), "left, frame {i}");
    }
    let _host = hosted(&mut app);
    for _ in 0..2 {
        let _ = pass(&mut app, wid);
    }
    assert_eq!(
        cost(&mut app, wid),
        Cost::default(),
        "CONTROL: chips at rest"
    );
    app.hover_claude_fact(wid, Some((session, Fact::Branch)));
    let titled = cost(&mut app, wid);
    assert!(
        titled.composed == 1 && titled.fits == 1,
        "the title laid once ({titled:?})"
    );
    let rule = AT_REST.bottom();
    assert!(
        glass_row(&app, wid, rule).contains("\u{2387} main \u{b7} click: hide"),
        "the fact's title: {:?}",
        glass_row(&app, wid, rule)
    );
    for i in 0..12 {
        assert_eq!(cost(&mut app, wid), Cost::default(), "titled, frame {i}");
    }
}

fn pass_key(app: &App, wid: WindowId) -> u64 {
    let (plan, _) = app.active_visible_frame_layout(wid).expect("a layout");
    app.claude_footer_fp(wid, &plan)
}

fn set_word(app: &mut App, session: u64, word: &str) {
    app.pool
        .get(session)
        .expect("session")
        .ctx
        .meta
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .set("autopilot", Some(word.into()));
    app.on_meta_changed(session);
}

fn need(app: &mut App, session: u64, i: usize) {
    let entry =
        format!("claude needs you: approve the release notes of 0.4{i}.0 \u{2014} carrying on");
    app.pool
        .get(session)
        .expect("session")
        .ctx
        .meta
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .set_attention_kind(
            crate::harness_host::NEEDS_ATTENTION_OWNER,
            Some(entry),
            Some(aterm_types::needs::AttentionKind::Ask),
        )
        .expect("a slot");
    app.on_meta_changed(session);
}

fn hold(app: &mut App, session: u64, i: usize) {
    use aterm_agent::harness::acts::HoldLeg;
    let held = aterm_agent::harness::acts::LandHeld {
        since_ms: Some(1_792_159_320_000),
        nudges: u32::try_from(i + 1).expect("small"),
        read: true,
        legs: HoldLeg::Repo.bit(),
        unpushed: 2,
        ..aterm_agent::harness::acts::LandHeld::default()
    };
    let sid = app
        .pool
        .get(session)
        .expect("session")
        .ctx
        .self_id
        .as_str()
        .to_string();
    assert!(crate::harness_host::store_land_held(
        &app.store,
        &sid,
        Some(held)
    ));
    app.apply_harness_fact(&sid, crate::HarnessFact::LandHeld);
}

fn read_only_while_read(word: &str, what: &str, change: fn(&mut App, u64, usize)) {
    use crate::claude_lights::Chip;
    let asked = || crate::claude_lights::TITLE_READERS_ASKED.with(|a| a.replace(0));
    let (mut app, wid, session) = probe_glass();
    let _host = hosted(&mut app);
    set_word(&mut app, session, word);
    for _ in 0..3 {
        let _ = pass(&mut app, wid);
    }
    let rest = pass(&mut app, wid);
    assert_eq!(cost(&mut app, wid), Cost::default(), "CONTROL: at rest");
    let generation = |app: &App| {
        app.harness_glass
            .get(&session)
            .map_or(0, crate::harness_glass::SessionGlass::trail_gen)
    };
    let _ = asked();
    for i in 0..3 {
        let before = generation(&app);
        change(&mut app, session, i);
        assert_ne!(
            generation(&app),
            before,
            "CONTROL: {what} {i} moves the trail's generation"
        );
        assert_eq!(asked(), 0, "{what} {i}, nobody reading: no frame asked");
        let moved = cost(&mut app, wid);
        assert_eq!(
            Cost { fits: 0, ..moved },
            Cost::default(),
            "{what} {i}, nobody reading: no title, nothing composed ({moved:?})"
        );
        assert_eq!(pass(&mut app, wid), rest, "{what} {i}: the key at rest");
        assert_eq!(
            cost(&mut app, wid),
            Cost::default(),
            "{what} {i}: then nothing"
        );
    }
    app.hover_claude_chip(wid, Some((session, Chip::Autopilot)));
    for _ in 0..2 {
        let _ = pass(&mut app, wid);
    }
    assert_eq!(
        cost(&mut app, wid),
        Cost::default(),
        "CONTROL: hovered, nothing moved"
    );
    for i in 3..6 {
        change(&mut app, session, i);
        assert_eq!(asked(), 1, "{what} {i}, read: its window asked for a frame");
        let first = cost(&mut app, wid);
        assert_eq!(
            (first.titles, first.fits),
            (1, 1),
            "{what} {i}, read: the title built again, once ({first:?})"
        );
        assert_eq!(cost(&mut app, wid), Cost::default(), "{what} {i}: once");
    }
}

#[test]
fn a_need_that_moves_while_nobody_reads_costs_no_frame() {
    read_only_while_read("cruise", "a need", need);
}

#[test]
fn a_hold_that_moves_while_nobody_reads_costs_no_frame() {
    read_only_while_read("land", "a hold", hold);
}

fn rebuilt(change: Change, passes: usize) -> Vec<Cost> {
    const WARM: usize = 48;
    let (mut app, wid, session) = probe_glass();
    for i in 0..WARM {
        change(&app, session, i);
        refill(&mut app, wid);
        let _ = cost(&mut app, wid);
    }
    (WARM..WARM + passes)
        .map(|i| {
            change(&app, session, i);
            refill(&mut app, wid);
            cost(&mut app, wid)
        })
        .collect()
}

#[test]
fn unchanged_rows_cost_no_read() {
    let kept = Cost {
        rebuilds: 1,
        compared: 1,
        ..Cost::default()
    };
    let changes: [(&str, Change, usize); 3] = [
        ("streamed row", streamed, 0),
        ("spinner tick", spinner, 0),
        ("keystroke", keystroke, 1),
    ];
    for (what, change, searches) in changes {
        for (i, cost) in rebuilt(change, 12).into_iter().enumerate() {
            assert_eq!(cost, Cost { searches, ..kept }, "{what} {i}");
        }
    }
}

#[test]
fn each_reading_follows_its_rows() {
    let (mut app, wid, session) = probe_glass();
    let facts_on = |app: &App| -> Vec<usize> {
        (0..usize::from(ROWS))
            .filter(|&r| glass_row(app, wid, r).contains("Opus 5.5 xhigh"))
            .collect()
    };
    assert_eq!(facts_on(&app), [AT_REST.bottom()], "CONTROL: on the rule");
    let up = Live {
        under: &[BYPASS, ""],
        ..AT_REST
    };
    feed(&app, session, &screen(&up));
    refill(&mut app, wid);
    let moved = cost(&mut app, wid);
    assert_eq!(moved.reads, 1, "the rule's rows read anew: {moved:?}");
    assert_eq!(
        facts_on(&app),
        [up.bottom()],
        "the facts on the new rule alone"
    );
    assert_ne!(up.bottom(), AT_REST.bottom());
    let mut boxed = screen(&AT_REST);
    let from = AT_REST.bottom() - 6;
    boxed[from] = "\u{256D}".to_owned() + &"\u{2500}".repeat(60) + "\u{256E}";
    boxed[from + 1] = "\u{2502} Do you want to make this edit to footer.rs?".to_owned();
    boxed[from + 2] = "\u{2502} \u{276F} 1. Yes".to_owned();
    boxed[from + 3] = "\u{2502}   2. No, and tell Claude what to do differently".to_owned();
    boxed[from + 4] = "\u{2570}".to_owned() + &"\u{2500}".repeat(60) + "\u{256F}";
    for row in &mut boxed[from + 5..] {
        row.clear();
    }
    for (what, rows, on) in [
        ("a box", boxed, &[][..]),
        (
            "the composer back",
            screen(&AT_REST),
            &[AT_REST.bottom()][..],
        ),
    ] {
        feed(&app, session, &rows);
        refill(&mut app, wid);
        let read = cost(&mut app, wid);
        assert_eq!(read.reads, 1, "{what}: read anew: {read:?}");
        assert_eq!(facts_on(&app), on, "{what}");
    }
    let (mut app, wid, session) = probe_glass();
    let rule = AT_REST.bottom();
    assert!(
        !glass_row(&app, wid, rule).contains("plan"),
        "CONTROL: bypass draws no light"
    );
    for (mode, drawn) in [(PLAN, true), (BYPASS, false)] {
        feed(
            &app,
            session,
            &screen(&Live {
                under: &[mode],
                ..AT_REST
            }),
        );
        refill(&mut app, wid);
        let read = cost(&mut app, wid);
        assert_eq!(read.reads, 1, "{mode}: the pill read anew: {read:?}");
        assert_eq!(
            glass_row(&app, wid, rule).contains("\u{23F8} plan"),
            drawn,
            "{mode}: {:?}",
            glass_row(&app, wid, rule)
        );
    }
    let banner = aterm_phase::anchor_text("launch.banner");
    let card = |model: &str| -> Vec<String> {
        let mut rows = screen(&AT_REST);
        rows[1] =
            format!(" \u{2590}\u{259B}\u{2588}\u{2588}\u{2588}\u{259B}\u{2588}   {banner}2.1.283");
        rows[2] = format!(
            "\u{259D}\u{259C}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2580}  {model} \u{b7} Claude Max"
        );
        rows[3] = " \u{259D}\u{259D}   \u{259D}\u{259D}   ~/aterm".to_owned();
        for row in &mut rows[4..AT_REST.bottom() - 4] {
            row.clear();
        }
        rows
    };
    let open = FooterFacts {
        model: None,
        effort: None,
        model_open: true,
        ..facts()
    };
    let (mut app, wid, session) = glass_with(&AT_REST, open);
    assert!(
        !glass_row(&app, wid, rule).contains("Opus"),
        "CONTROL: no card, no model: {:?}",
        glass_row(&app, wid, rule)
    );
    drawer(&app, session, 4242, "claude", "2.1.283");
    for model in ["Opus 5.5", "Sonnet 4.5"] {
        feed(&app, session, &card(model));
        refill(&mut app, wid);
        let read = cost(&mut app, wid);
        assert_eq!(read.cards, 1, "{model}: the card read anew: {read:?}");
        assert!(
            glass_row(&app, wid, rule).contains(model),
            "{model}: {:?}",
            glass_row(&app, wid, rule)
        );
        assert_eq!(
            cost(&mut app, wid),
            Cost::default(),
            "{model}: the card's reading kept, at rest"
        );
    }
}

#[test]
fn a_refused_card_costs_a_settled_frame_nothing() {
    let rule = AT_REST.bottom();
    let banner = aterm_phase::anchor_text("launch.banner");
    let card = || -> Vec<String> {
        let mut rows = screen(&AT_REST);
        rows[1] =
            format!(" \u{2590}\u{259B}\u{2588}\u{2588}\u{2588}\u{259B}\u{2588}   {banner}2.1.283");
        rows[2] = "\u{259D}\u{259C}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2580}  Opus 5.5 \u{b7} Claude Max".to_owned();
        rows[3] = " \u{259D}\u{259D}   \u{259D}\u{259D}   ~/aterm".to_owned();
        for row in &mut rows[4..AT_REST.bottom() - 4] {
            row.clear();
        }
        rows
    };
    let open = FooterFacts {
        model: None,
        effort: None,
        model_open: true,
        ..facts()
    };
    for (case, own) in [
        ("no drawer", None),
        ("before its cut", Some(false)),
        ("its own", Some(true)),
    ] {
        let (mut app, wid, session) = glass_with(&AT_REST, open.clone());
        if own == Some(true) {
            drawer(&app, session, 4242, "claude", "2.1.283");
        }
        feed(&app, session, &card());
        if own == Some(false) {
            drawer(&app, session, 4242, "claude", "2.1.283");
        }
        refill(&mut app, wid);
        let _ = cost(&mut app, wid);
        for i in 0..3 {
            let _ = crate::work_counts::take_footer_timeline_locks();
            let settled = cost(&mut app, wid);
            let locks = crate::work_counts::take_footer_timeline_locks();
            assert_eq!(
                (settled.allocations, settled.cards, locks),
                (0, 0, 1),
                "{case}, pass {i}: no copy, no reading, one lock: {settled:?}"
            );
        }
        assert_eq!(
            glass_row(&app, wid, rule).contains("Opus 5.5"),
            own == Some(true),
            "{case}: {:?}",
            glass_row(&app, wid, rule)
        );
    }
}

#[test]
fn a_row_compares_as_its_chars() {
    let cells = |text: &str| -> Vec<RenderCell> {
        text.chars()
            .map(|ch| RenderCell {
                ch,
                ..RenderCell::default()
            })
            .collect()
    };
    let texts = [
        "",
        "a",
        "ab",
        "abc",
        "ab ",
        "\u{276F} draft",
        "\u{276F} drafu",
        "\u{2500}\u{2500}",
        "\u{2500}\u{2501}",
        "\u{4E2D} ",
        "\u{4E2D}",
        "\u{1F600}x",
        "\u{1F601}x",
        "a\u{e9}",
        "a\u{e8}",
    ];
    for text in texts {
        for row in texts {
            let row_cells = cells(row);
            assert_eq!(
                same_text(text, Some(&row_cells)),
                text == row,
                "{text:?} as {row:?}"
            );
        }
        assert_eq!(same_text(text, None), text.is_empty(), "{text:?}, no row");
    }
}

fn vendor_rows(app: &App, wid: WindowId) -> Vec<String> {
    let ws = &app.windows[&wid];
    let strip = ws.input_scratch.cells.len() - usize::from(ws.rows);
    ws.input_scratch.cells[strip..]
        .iter()
        .map(|row| row.iter().map(|c| c.ch).collect())
        .collect()
}

fn read_rows(app: &App, wid: WindowId) -> Vec<String> {
    let texts = app.windows[&wid].claude_footer_texts.borrow();
    let cached = texts.panes.iter().flatten().next().expect("a pane read");
    cached.rows.to_vec()
}

/// A 1×1 image, as a placement on a cell carries it.
pub(super) fn image() -> aterm_core::grid::extra::ImageRef {
    use aterm_core::grid::extra::{ImageData, ImageFormat, ImageRef, ImageScaling};
    ImageRef {
        image: std::sync::Arc::new(ImageData {
            bytes: vec![1, 2, 3, 4],
            format: ImageFormat::RawRgba8 {
                width: 1,
                height: 1,
            },
            cols: 1,
            rows: 1,
            z_index: 0,
            band_lift_px: 0,
            cell_offset_px: (0, 0),
            scaling: ImageScaling::Fit,
            source_rect: None,
        }),
        cell_row: 0,
        cell_col: 0,
        kitty: None,
    }
}

#[test]
fn the_kept_reads_and_fits_follow_their_inputs() {
    let (mut app, wid, session) = probe_glass();
    let rows = usize::from(ROWS);
    let rule = AT_REST.bottom();
    let strip = |app: &App| {
        let ws = &app.windows[&wid];
        ws.input_scratch.cells.len() - usize::from(ws.rows)
    };
    let mut typed = 0;
    let frame = |app: &mut App, what: &str, change: &dyn Fn(&App), then: &dyn Fn(&mut App)| {
        change(app);
        refill(app, wid);
        then(app);
        let vendor = vendor_rows(app, wid);
        let cost = cost(app, wid);
        assert_eq!(
            read_rows(app, wid),
            vendor,
            "{what}: the vendor's rows read"
        );
        cost
    };
    let key = |i: usize| move |app: &App| keystroke(app, session, i);
    let none = |_: &mut App| {};
    for _ in 0..8 {
        typed += 1;
        let _ = frame(&mut app, "warm", &key(typed), &none);
    }
    typed += 1;
    let one = frame(&mut app, "a keystroke", &key(typed), &none);
    assert_eq!(
        (one.compared, one.masks, one.fits),
        (1, 0, 0),
        "a keystroke compares the caret row alone ({one:?})"
    );
    typed += 1;
    let every = crate::claude_footer::read_every_row_in(|| {
        frame(&mut app, "every row", &key(typed), &none)
    });
    assert_eq!(
        (every.compared, every.masks),
        (rows, 1),
        "CONTROL: every row compared, the mask read ({every:?})"
    );
    typed += 1;
    let anew = crate::claude_lights::fit_anew_in(|| frame(&mut app, "anew", &key(typed), &none));
    assert_eq!(anew.fits, 1, "CONTROL: fitted anew ({anew:?})");
    let repainted = frame(
        &mut app,
        "the rule repainted",
        &|app: &App| {
            bytes(
                app,
                session,
                format!(
                    "\x1b7\x1b[{};1H\x1b[38;2;90;90;90m{}\x1b[0m\x1b8",
                    rule + 1,
                    "\u{2500}".repeat(usize::from(COLS))
                )
                .as_bytes(),
            );
        },
        &none,
    );
    assert_eq!(
        (repainted.compared, repainted.masks, repainted.reads),
        (1, 1, 0),
        "the rule's row compared and its mask read, its text the same ({repainted:?})"
    );
    let scrolled = frame(
        &mut app,
        "a scroll",
        &|app: &App| {
            bytes(
                app,
                session,
                format!("\x1b7\x1b[{rows};1H\n\x1b8").as_bytes(),
            )
        },
        &none,
    );
    assert_eq!(scrolled.compared, rows, "every row compared ({scrolled:?})");
    assert!(
        glass_row(&app, wid, rule - 1).contains("Opus 5.5 xhigh")
            && !glass_row(&app, wid, rule).contains("Opus"),
        "the facts on the rule where it stands: {:?}",
        glass_row(&app, wid, rule - 1)
    );
    let _ = frame(
        &mut app,
        "drawn again",
        &|app: &App| feed(app, session, &screen(&AT_REST)),
        &none,
    );
    typed += 1;
    let _ = frame(&mut app, "settled", &key(typed), &none);
    let rule_cells = |app: &App| app.windows[&wid].input_scratch.cells[strip(app) + rule].clone();
    refill(&mut app, wid);
    let vendor = rule_cells(&app);
    let _ = cost(&mut app, wid);
    let written = rule_cells(&app);
    let col = (0..written.len())
        .find(|&c| written[c] != vendor[c])
        .expect("a cell the footer writes");
    let placed = move |app: &mut App| {
        let frame_row = strip(app) + rule;
        let ws = app.windows.get_mut(&wid).expect("window");
        ws.input_scratch.images[frame_row].push((col, image()));
    };
    typed += 1;
    let on = frame(&mut app, "an image", &key(typed), &placed);
    assert_eq!(on.masks, 1, "an image on the rule: the mask read ({on:?})");
    assert_eq!(
        rule_cells(&app)[col],
        vendor[col],
        "the image's cell the vendor's"
    );
    typed += 1;
    let after = frame(&mut app, "the image gone", &key(typed), &none);
    assert_eq!(
        after.masks, 1,
        "read beside an image: read again ({after:?})"
    );
    assert_eq!(
        rule_cells(&app)[col],
        written[col],
        "the cell written again"
    );
    typed += 1;
    let kept = frame(&mut app, "kept", &key(typed), &none);
    assert_eq!((kept.compared, kept.masks), (1, 0), "kept again ({kept:?})");
    let unvouched = move |app: &mut App| {
        app.windows
            .get_mut(&wid)
            .expect("window")
            .input_scratch
            .row_rev_lane = 0;
    };
    typed += 1;
    let no_lane = frame(&mut app, "no lane", &key(typed), &unvouched);
    assert_eq!(
        (no_lane.compared, no_lane.masks),
        (rows, 1),
        "no lane: every row compared ({no_lane:?})"
    );
    typed += 1;
    let after = frame(&mut app, "after no lane", &key(typed), &none);
    assert_eq!(
        (after.compared, after.masks),
        (rows, 1),
        "the last stamps vouch nothing ({after:?})"
    );
    typed += 1;
    let back = frame(&mut app, "vouched again", &key(typed), &none);
    assert_eq!(
        (back.compared, back.masks),
        (1, 0),
        "one row again ({back:?})"
    );
    let ghost_row = 2;
    let ghost = move |app: &mut App| {
        let frame_row = strip(app) + ghost_row;
        let s = &mut app.windows.get_mut(&wid).expect("window").input_scratch;
        let (cols, blank) = (s.cols, s.implicit_blank);
        assert!(
            s.paint_prediction_ghost(frame_row, 0, 'Z', cols, blank),
            "CONTROL: the ghost painted"
        );
        s.snapshot_seq = s.snapshot_seq.wrapping_add(1);
    };
    typed += 1;
    let host = frame(&mut app, "a host write", &key(typed), &ghost);
    assert_eq!(
        host.compared, rows,
        "a host write: every row compared ({host:?})"
    );
    assert!(
        read_rows(&app, wid)[ghost_row].starts_with('Z'),
        "CONTROL: the ghost on the rows read"
    );
    typed += 1;
    let after = frame(&mut app, "after the host write", &key(typed), &none);
    assert_eq!(
        after.compared, rows,
        "the host write's stamps vouch nothing ({after:?})"
    );
    assert!(
        !read_rows(&app, wid)[ghost_row].starts_with('Z'),
        "the engine's row again"
    );
    typed += 1;
    let back = frame(&mut app, "vouched past the host write", &key(typed), &none);
    assert_eq!(back.compared, 1, "one row again ({back:?})");
    let unstamped_row = 6;
    let unstamp = move |app: &mut App| {
        let frame_row = strip(app) + unstamped_row;
        let s = &mut app.windows.get_mut(&wid).expect("window").input_scratch;
        s.row_rev[frame_row] = 0;
    };
    typed += 1;
    let first = frame(&mut app, "a row unstamped", &key(typed), &unstamp);
    assert_eq!(
        first.compared, 2,
        "the caret's row and the unstamped one ({first:?})"
    );
    let rewrite = |app: &App| {
        bytes(
            app,
            session,
            format!(
                "\x1b7\x1b[{};1Hrewritten under no stamp\x1b[K\x1b8",
                unstamped_row + 1
            )
            .as_bytes(),
        );
    };
    let rewritten = frame(&mut app, "the unstamped row rewritten", &rewrite, &unstamp);
    assert_eq!(
        rewritten.compared, 1,
        "the unstamped row compared ({rewritten:?})"
    );
    assert_eq!(
        read_rows(&app, wid)[unstamped_row],
        "rewritten under no stamp",
        "CONTROL: the row rewritten"
    );
    let alternate: Vec<String> = (0..rows).map(|i| format!("alternate row {i}")).collect();
    let _ = frame(
        &mut app,
        "the alternate screen",
        &|app: &App| {
            bytes(app, session, b"\x1b[?1049h");
            feed(app, session, &alternate);
        },
        &none,
    );
    for _ in 0..3 {
        typed += 1;
        let _ = frame(
            &mut app,
            "typed on the alternate screen",
            &key(typed),
            &none,
        );
    }
    let _ = frame(
        &mut app,
        "the primary screen again",
        &|app: &App| bytes(app, session, b"\x1b[?1049l"),
        &none,
    );
    for _ in 0..3 {
        typed += 1;
        let _ = frame(&mut app, "typed on the primary again", &key(typed), &none);
    }
    assert!(
        glass_row(&app, wid, rule).contains("Opus 5.5 xhigh"),
        "the facts on the rule again: {:?}",
        glass_row(&app, wid, rule)
    );
    typed += 1;
    let settled = frame(&mut app, "settled on the primary", &key(typed), &none);
    assert_eq!(settled.compared, 1, "one row again ({settled:?})");
}

#[test]
fn the_stamp_lane_vouches_for_rows_exactly_where_the_renderer_does() {
    let (mut app, wid, _) = probe_glass();
    refill(&mut app, wid);
    let stamped = app.windows[&wid].input_scratch.clone();
    let renderer = |prev: &RenderInput, now: &RenderInput| {
        aterm_render::row_revisions_comparable(prev, now)
            && [prev, now].iter().all(|s| s.cells.len() == s.rows)
    };
    let lane = |prev: &RenderInput, now: &RenderInput| {
        let lane = StampLane::of(prev);
        lane.is_some() && lane == StampLane::of(now)
    };
    assert!(
        renderer(&stamped, &stamped) && lane(&stamped, &stamped),
        "CONTROL: a stamped frame is comparable to itself"
    );
    type Move = (&'static str, fn(&mut RenderInput));
    let moves: [Move; 13] = [
        ("another lane", |s| {
            s.row_rev_lane = s.row_rev_lane.wrapping_add(1)
        }),
        ("no lane", |s| s.row_rev_lane = 0),
        ("a revision short", |s| {
            s.row_rev.pop();
        }),
        ("a row of cells short", |s| {
            s.cells.pop();
        }),
        ("a host write", |s| {
            s.snapshot_seq = s.snapshot_seq.wrapping_add(1);
        }),
        ("a strip row more", |s| s.row_shift += 1),
        ("the alternate screen", |s| s.engine_alt = !s.engine_alt),
        ("rows in visual order", |s| {
            s.engine_row_order = aterm_core::render::RowOrder::BidiVisual;
        }),
        ("a scrolled view", |s| s.display_offset = 1),
        ("a scroll", |s| s.base_y += 1),
        ("a renumbering", |s| s.absolute_row_revision += 1),
        ("a row more", |s| {
            s.rows += 1;
            s.row_rev.push(1);
            s.cells.push(Vec::new());
        }),
        ("a column more", |s| s.cols += 1),
    ];
    for (what, moved) in moves {
        let mut now = stamped.clone();
        moved(&mut now);
        for (side, prev, next) in [("after", &stamped, &now), ("before", &now, &stamped)] {
            assert!(
                !renderer(prev, next),
                "CONTROL: {what}, {side}: the renderer vouches for no row"
            );
            assert!(!lane(prev, next), "{what}, {side}: nor the lane");
        }
        assert_eq!(
            lane(&now, &now),
            renderer(&now, &now),
            "{what}, on both sides"
        );
    }
}
