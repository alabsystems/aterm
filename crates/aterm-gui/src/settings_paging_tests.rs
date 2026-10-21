// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A Settings list's screens, judged from what each compiled screen paints:
//! Page Up and Page Down turn over exactly the stops a screen showed whole, a
//! hard edge never leaves blank ground under a card it cuts, and a short
//! window sheds the page's chrome before its lead leaves the first screen.

use super::*;

/// A config that changes enough settings for Modified to scroll at every size:
/// rows with their facts on a second line, one-row cards and a key only
/// aterm.toml holds.
const CHANGED: &str = "theme = \"Dracula\"\ncolumns = 120\nlines = 40\n\
    allow_osc52_query = true\nscrollback_lines = 5000\ncursor_blink = false\n\
    cursor_trail_style = \"comet\"\nsearch_history_lines = 2000\n\
    window_padding = 20.0\nfont_px = 15.0\nagents_auto_prime = false\n";

/// The 80x24, 102x29 and 150x48 windows' Settings views.
const PAGING_SIZES: [(f32, f32); 3] = [(584.0, 348.0), (738.0, 418.0), (1_074.0, 684.0)];

/// A Settings view over `source`, with live packages, on `route`.
fn paging_view(
    source: &str,
    route: SettingsRoute,
) -> (
    NativeRuntime,
    crate::native_app::AppInstanceId,
    crate::native_app::ViewId,
) {
    let service = VersionedConfigService::new(source.to_string()).expect("valid fixture");
    let (mut runtime, instance, view) = setup();
    assert!(runtime.replace_settings_packages(live_packages_state(None), 2));
    let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
        panic!("Settings view");
    };
    **state = SettingsViewState::from_snapshot(&service.snapshot()).expect("projects");
    state.navigate(route);
    (runtime, instance, view)
}

/// The scrolling list a compiled page draws for `plan`'s screen at `top`: the
/// node that carries a scroll thumb (the sidebar's aside), drawn as tall as
/// that screen.
fn painted_list(
    compiled: &crate::native_ui::CompiledUi,
    plan: &scroll::ScrollPlan,
    top: usize,
) -> Option<LogicalRect> {
    let height = plan.screen(top.min(plan.limit())).height;
    let lists: Vec<_> = compiled
        .paint
        .iter()
        .filter(|node| {
            let key = node.key.as_str();
            key != "settings/navigation/list"
                && compiled
                    .paint
                    .iter()
                    .any(|thumb| thumb.key.as_str() == format!("{key}/thumb"))
        })
        .map(|node| node.rect)
        .collect();
    lists
        .iter()
        .find(|rect| (rect.height - height).abs() < 0.5)
        .or(lists.first())
        .copied()
}

/// What a node paints that a person reads or presses: words, a control, a
/// picture — not a group, and not the list's thumb.
fn paints_content(node: &crate::native_ui::PaintNode) -> bool {
    !matches!(node.content, UiContent::Group(_)) && !node.key.as_str().ends_with("/thumb")
}

/// The stops the compiled screen whose first stop is `top` SHOWS WHOLE, judged
/// from its paint: a stop is shown whole where everything it paints — its
/// words and controls — is drawn uncut and at full strength, none of it cut
/// by the list's edges, scrolled under a kept caption, or fading in the strip
/// under the room, and where all it draws lies on the list as painted (what
/// an edge cuts away entirely is in no paint node). The plan says only which
/// painted nodes are whose, and where each stop's drawing begins and ends.
fn shown_whole(
    compiled: &crate::native_ui::CompiledUi,
    plan: &scroll::ScrollPlan,
    top: usize,
) -> BTreeSet<usize> {
    let Some(list) = painted_list(compiled, plan, top) else {
        return BTreeSet::new();
    };
    plan.drawn_bands(top)
        .into_iter()
        .filter(|band| {
            let (from, to) = (list.y + band.at, list.y + band.at + band.height);
            let nodes: Vec<_> = compiled
                .paint
                .iter()
                .filter(|node| {
                    let middle = node.rect.y + node.rect.height / 2.0;
                    paints_content(node)
                        && !node.clip.is_empty()
                        && node.clip.x >= list.x - 0.5
                        && node.clip.right() <= list.right() + 0.5
                        && node.clip.y >= list.y - 0.5
                        && node.clip.bottom() <= list.bottom() + 0.5
                        && middle >= from - 0.01
                        && middle < to - 0.01
                })
                .collect();
            band.ink.0 >= band.floor - 0.5
                && band.ink.1 <= list.height + 0.5
                && !nodes.is_empty()
                && nodes.iter().all(|node| {
                    node.fade.is_none()
                        && (node.clip.y - node.rect.y).abs() < 0.5
                        && (node.clip.height - node.rect.height).abs() < 0.5
                })
        })
        .map(|band| band.stop)
        .collect()
}

/// One screen of the turn: where the list is — its first stop, and how far
/// past that stop's seat it is scrolled (a screen Page Up scrolled to end on
/// a stop) — and what it shows whole.
struct Turned {
    top: usize,
    offset: f32,
    whole: BTreeSet<usize>,
}

impl Turned {
    fn place(&self) -> (usize, i64) {
        (self.top, (self.offset * 100.0).round() as i64)
    }
}

fn screen_now(
    runtime: &NativeRuntime,
    instance: crate::native_app::AppInstanceId,
    view: crate::native_app::ViewId,
    cx: &ViewCx<'_>,
) -> Option<(Turned, scroll::ScrollPlan)> {
    let compiled = compile_settings_view(runtime, instance, view, cx);
    let state = messages_view_state(runtime, view);
    let plan = state.scroll_plan()?;
    let top = state.page_scroll.min(plan.limit());
    let whole = shown_whole(&compiled, &plan, top);
    let offset = plan.screen(top).offset;
    Some((Turned { top, offset, whole }, plan))
}

/// MODIFIED'S ROW WHOSE FACTS THE STRIP HOLDS IS SHOWN WHOLE, AND PAGE DOWN
/// TURNS PAST IT (round-7 review): every Modified row carries its facts on a
/// second line, so at the shipped 102x29 and at 150x48 the room's edge falls
/// between a row's field and its facts, which the strip kept for a status bar
/// painted legibly under it, while the screen did not count the row, and Page
/// Down opened the next screen on that row again. The row is now drawn whole
/// and at full strength — its facts live too — and Page Down starts after it.
#[test]
fn modifieds_row_whose_facts_the_strip_holds_is_shown_and_turned_past() {
    let mut found = 0;
    for (width, height) in [(738.0, 418.0), (1_074.0, 684.0)] {
        let cx = view_cx_at(width, height);
        let (mut runtime, instance, view) = paging_view(CHANGED, SettingsRoute::Modified);
        let limit = screen_now(&runtime, instance, view, &cx)
            .expect("a list")
            .1
            .limit();
        for top in 0..limit {
            {
                let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
                    unreachable!();
                };
                state.page_scroll = top;
                state.page_reveal = None;
            }
            let compiled = compile_settings_view(&runtime, instance, view, &cx);
            let plan = messages_view_state(&runtime, view)
                .scroll_plan()
                .expect("a list");
            let shown = plan.shown(top);
            let Some(list) = painted_list(&compiled, &plan, top) else {
                continue;
            };
            // The last row the screen shows, where its facts end under the room.
            let Some(band) = plan
                .drawn_bands(top)
                .into_iter()
                .find(|band| band.stop + 1 == shown.end && band.ink.1 > plan.room() + 0.5)
            else {
                continue;
            };
            found += 1;
            let context = format!("{width}x{height} screen {top} stop {}", band.stop);
            let (from, to) = (list.y + band.at, list.y + band.ink.1);
            let painted: Vec<_> = compiled
                .paint
                .iter()
                .filter(|node| {
                    paints_content(node)
                        && node.rect.x >= list.x - 0.5
                        && node.rect.y >= from - 0.5
                        && node.rect.bottom() <= to + 0.5
                })
                .collect();
            assert!(
                painted
                    .iter()
                    .any(|node| node.rect.bottom() > list.y + plan.room() + 0.5),
                "{context}: its facts are painted under the room"
            );
            for node in &painted {
                assert!(
                    node.fade.is_none() && (node.clip.height - node.rect.height).abs() < 0.5,
                    "{context}: {} is drawn whole, at full strength",
                    node.key.as_str()
                );
            }
            assert!(
                shown_whole(&compiled, &plan, top).contains(&band.stop),
                "{context}: the paint shows it whole"
            );
            runtime
                .dispatch(instance, view, AppEvent::ScrollPage(1))
                .unwrap();
            let (next, plan) = screen_now(&runtime, instance, view, &cx).expect("a list");
            if next.top < plan.limit() {
                assert_eq!(
                    next.top,
                    band.stop + 1,
                    "{context}: Page Down starts after it"
                );
                assert!(!next.whole.contains(&band.stop), "{context}");
            }
        }
    }
    assert!(found > 0, "Modified holds such a row at these sizes");
}

/// PAGE DOWN AND PAGE UP TURN OVER EXACTLY WHAT A SCREEN SHOWED WHOLE: on
/// every route at 60x20, 80x24, 102x29 and 150x48, with no settings changed
/// and with many, each Page Down from the list's top shows no stop the screen
/// before it showed whole — until the last screen, which is anchored at the
/// list's end and full — and skips none; Page Up, from the end back to the
/// top, likewise. Judged from what each compiled screen paints, not from the
/// plan's own measure. Modified, every row of which carries its facts on a
/// second line, turned a screen whose last row's facts were painted in the
/// strip under the room at full strength over onto that row again. At 60x20
/// the screen before the last drew the list's last row whole, only its
/// card's closing padding past the edge, and did not count it: Page Up from
/// the last screen turned back onto it (Software Update, Appearance, Cursor
/// Styles, Window).
#[test]
fn page_keys_turn_over_exactly_what_a_screen_showed_whole() {
    let mut turned = 0usize;
    let mut faults = Vec::new();
    for source in ["", CHANGED] {
        for route in SettingsRoute::ALL {
            if route == SettingsRoute::Messages {
                continue;
            }
            for (width, height) in std::iter::once((444.0, 292.0)).chain(PAGING_SIZES) {
                let context = format!(
                    "{route:?} {width}x{height}{}",
                    if source.is_empty() { "" } else { " changed" }
                );
                let cx = view_cx_at(width, height);
                let (mut runtime, instance, view) = paging_view(source, route);
                let Some((first, plan)) = screen_now(&runtime, instance, view, &cx) else {
                    continue;
                };
                let limit = plan.limit();
                if limit == 0 {
                    continue;
                }
                // What any screen shows whole: a stop no screen shows whole
                // (taller than the room) can be skipped by none.
                let mut showable = BTreeSet::new();
                for top in 0..=limit {
                    let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
                        unreachable!();
                    };
                    state.page_scroll = top;
                    state.page_reveal = None;
                    if let Some((screen, _)) = screen_now(&runtime, instance, view, &cx) {
                        showable.extend(screen.whole);
                    }
                }
                {
                    let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
                        unreachable!();
                    };
                    state.page_scroll = 0;
                    state.page_reveal = None;
                }
                let _ = compile_settings_view(&runtime, instance, view, &cx);
                for direction in [1, -1] {
                    let mut screens = vec![if direction > 0 { first.top } else { limit }];
                    let start = screen_now(&runtime, instance, view, &cx).expect("a list").0;
                    assert_eq!(
                        start.top,
                        if direction > 0 { 0 } else { limit },
                        "{context}"
                    );
                    let mut seen = start.whole.clone();
                    let mut before = start;
                    loop {
                        runtime
                            .dispatch(instance, view, AppEvent::ScrollPage(direction))
                            .unwrap();
                        let (now, _) = screen_now(&runtime, instance, view, &cx).expect("a list");
                        if now.place() == before.place() {
                            break;
                        }
                        turned += 1;
                        screens.push(now.top);
                        // A turn that lands on an end of the list stops there:
                        // the last screen is anchored at the list's end and
                        // full, the first at its top, so what either holds past
                        // the stops the turn brought the screen before may have
                        // shown already, as a platform list's last page does.
                        let at_end = if direction > 0 {
                            now.top == limit
                        } else {
                            now.place() == (0, 0)
                        };
                        if !at_end {
                            let again: Vec<_> = now.whole.intersection(&before.whole).collect();
                            if !again.is_empty() {
                                faults.push(format!(
                                    "{context}: Page {} from {} to {} shows {again:?} again \
                                 (shown whole {:?} then {:?})",
                                    if direction > 0 { "Down" } else { "Up" },
                                    before.top,
                                    now.top,
                                    before.whole,
                                    now.whole
                                ));
                            }
                        }
                        seen.extend(now.whole.iter().copied());
                        before = now;
                        if at_end || screens.len() > 4 * (limit + 2) {
                            break;
                        }
                    }
                    let skipped: Vec<_> = showable.difference(&seen).collect();
                    if !skipped.is_empty() {
                        faults.push(format!(
                            "{context}: Page {} over {screens:?} skips {skipped:?}",
                            if direction > 0 { "Down" } else { "Up" },
                        ));
                    }
                }
            }
        }
    }
    assert!(
        faults.is_empty(),
        "{} faults:\n{}",
        faults.len(),
        faults.join("\n")
    );
    assert!(turned > 0, "the sweep turns pages");
}

/// NO SCREEN ENDS ON BLANK GROUND UNDER A CARD IT CUTS: on every route, every
/// screen with stops after it, at 60x20, 80x24, 102x29 and 150x48 (with no
/// settings changed and with many), the card the list's bottom edge cuts is
/// drawn on to the page's bottom — its room and the strip kept for a status
/// bar — with no band of blank over 8pt under it; a screen that ends on a
/// whole card instead ends on that card's boundary, the gap and less than
/// [`scroll::PEEK_MIN`] of the next card under it, and the next screen starts
/// with the next card. Judged from the paint. The hard edge used to cut a
/// card that would end whole PEEK_MIN short of its bottom (Packages at 80x24:
/// a 36pt band) and to land on a caption's top (Software Update at 60x20).
#[test]
fn no_screen_ends_on_blank_under_a_card_it_cuts() {
    let mut cut_screens = 0usize;
    let mut faults = Vec::new();
    for source in ["", CHANGED] {
        for route in SettingsRoute::ALL {
            if route == SettingsRoute::Messages {
                continue;
            }
            for (width, height) in [
                (444.0, 292.0),
                (584.0, 348.0),
                (738.0, 418.0),
                (1_074.0, 684.0),
            ] {
                let cx = view_cx_at(width, height);
                let (mut runtime, instance, view) = paging_view(source, route);
                let _ = compile_settings_view(&runtime, instance, view, &cx);
                let Some(plan) = messages_view_state(&runtime, view).scroll_plan() else {
                    continue;
                };
                for top in 0..=plan.limit() {
                    {
                        let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view)
                        else {
                            unreachable!();
                        };
                        state.page_scroll = top;
                        state.page_reveal = None;
                    }
                    let compiled = compile_settings_view(&runtime, instance, view, &cx);
                    let state = messages_view_state(&runtime, view);
                    let Some(plan) = state.scroll_plan() else {
                        continue;
                    };
                    let Some(list) = painted_list(&compiled, &plan, top) else {
                        continue;
                    };
                    // A screen that draws on to the list's end has nothing
                    // after it to peek in.
                    if plan
                        .drawn_bands(top)
                        .last()
                        .is_none_or(|band| band.stop + 1 >= plan.len())
                    {
                        continue;
                    }
                    let context = format!(
                        "{route:?} {width}x{height}{} screen {top}",
                        if source.is_empty() { "" } else { " changed" }
                    );
                    // The page's bottom: the list's room and the strip under
                    // it, where nothing over the list cuts it shorter (a page
                    // that clips its own list is no band of blank).
                    let cut_by_page = compiled
                        .paint
                        .iter()
                        .find(|node| node.rect == list)
                        .map(|node| node.clip.bottom())
                        .filter(|bottom| *bottom < list.bottom() - 0.5)
                        .unwrap_or(f32::INFINITY);
                    let page_bottom = (list.y + plan.room() + plan.overhang()).min(cut_by_page);
                    let inside: Vec<_> = compiled
                        .paint
                        .iter()
                        .filter(|node| {
                            !node.clip.is_empty()
                                && node.rect != list
                                && !node.key.as_str().ends_with("/thumb")
                                && !node.key.as_str().ends_with("/scroll")
                                && node.clip.x >= list.x - 0.5
                                && node.clip.right() <= list.right() + 0.5
                                && node.clip.y >= list.y - 0.5
                                && node.clip.bottom() <= list.bottom() + 0.5
                        })
                        .collect();
                    let drawn_to = inside
                        .iter()
                        .map(|node| node.clip.bottom())
                        .fold(list.y, f32::max);
                    let cut = inside.iter().any(|node| {
                        node.rect.bottom() > node.clip.bottom() + 0.5
                            && node.clip.bottom() >= drawn_to - 0.5
                    });
                    let band = page_bottom - drawn_to;
                    if cut {
                        cut_screens += 1;
                        if band > 8.0 {
                            faults.push(format!(
                                "{context}: {band}pt of blank under the card it cuts \
                                 (drawn to {drawn_to}, page bottom {page_bottom})"
                            ));
                        }
                    } else if band > plan.gap() + scroll::PEEK_MIN + 0.5 {
                        faults.push(format!(
                            "{context}: ends {band}pt above the page's bottom on a whole card"
                        ));
                    }
                }
            }
        }
    }
    assert!(
        faults.is_empty(),
        "{} faults:\n{}",
        faults.len(),
        faults.join("\n")
    );
    assert!(cut_screens > 0, "the sweep holds screens that end on a cut");
}

/// A STATUS BAR NEVER MOVES THE LIST, EVEN UNDER A ROW WHOSE FACTS IT
/// COVERS: the keyboard on a row the screen shows with its facts in the strip
/// kept for a bar, a bar that comes takes that strip — the facts go under
/// it, the row's control stays whole and live where it was — and the list
/// does not scroll to bring the facts back: nothing scrolled it, and the
/// control the keyboard is on is in view. Page Down then opens on that row,
/// whose facts the screen no longer showed.
#[test]
fn a_bar_over_a_rows_facts_never_moves_the_list() {
    let mut found = 0;
    for route in [
        SettingsRoute::Sound,
        SettingsRoute::Security,
        SettingsRoute::Modified,
    ] {
        for (width, height) in [(738.0, 418.0), (1_074.0, 684.0)] {
            let cx = view_cx_at(width, height);
            let (mut runtime, instance, view) = paging_view(CHANGED, route);
            let limit = screen_now(&runtime, instance, view, &cx)
                .expect("a list")
                .1
                .limit();
            for top in 0..limit {
                {
                    let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
                        unreachable!();
                    };
                    state.page_scroll = top;
                    state.page_reveal = None;
                    state.feedback = None;
                    state.common.last_focus = None;
                }
                let _ = compile_settings_view(&runtime, instance, view, &cx);
                let plan = messages_view_state(&runtime, view)
                    .scroll_plan()
                    .expect("a list");
                let shown = plan.shown(top);
                let Some(band) = plan
                    .drawn_bands(top)
                    .into_iter()
                    .find(|band| band.stop + 1 == shown.end && band.ink.1 > plan.room() + 0.5)
                else {
                    continue;
                };
                let Some(key) = plan
                    .controls()
                    .find(|(stop, _)| *stop == band.stop)
                    .map(|(_, key)| key.clone())
                else {
                    continue;
                };
                found += 1;
                let context = format!("{route:?} {width}x{height} screen {top} {key:?}");
                {
                    let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
                        unreachable!();
                    };
                    state.common.last_focus = Some(key.clone());
                    state.common.focus_visible = true;
                }
                let before = compile_settings_view(&runtime, instance, view, &cx);
                let at = before.semantic(&key).map(|node| node.rect);
                assert!(at.is_some(), "{context}: the control is live");
                {
                    let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
                        unreachable!();
                    };
                    state.show_transient_status_for_test("Copied");
                }
                let after = compile_settings_view(&runtime, instance, view, &cx);
                let state = messages_view_state(&runtime, view);
                assert_eq!(state.page_scroll, top, "{context}: the list stays");
                assert_eq!(
                    after.semantic(&key).map(|node| node.rect),
                    at,
                    "{context}: the control stays whole and live where it was"
                );
                let plan = state.scroll_plan().expect("a list");
                assert!(
                    !plan.shown(top).contains(&band.stop),
                    "{context}: its facts under the bar, the row is not shown"
                );
            }
        }
    }
    assert!(found > 0, "the sweep holds such rows");
}

/// SHORT WINDOWS SHED CHROME BEFORE CONTENT, ONE MEASURED STEP AT A TIME
/// ([`PageChrome`]): a page whose list's first screen holds its lead — its
/// first stop and the first row of controls after it — keeps its whole
/// chrome; one whose does not gives its subtitle, then its insets, to the
/// list, the least step that lets the first screen hold it, the heading that
/// names the page staying; and one no step helps keeps its own. Security's
/// press on a Wide 1074x348 window needed 188pt (212pt with a broken anchor)
/// of a list the subtitle and 32pt insets left 158pt: without the subtitle it
/// has 194pt, with the insets at 12pt 234pt. A status bar that comes, taller
/// than the strip kept for it, changes nothing. CONTROLS: Terminal at
/// 1074x348 and Packages at 80x24 (its status card and first switch outgrow
/// any chrome's room) keep their chrome.
#[test]
fn a_short_window_sheds_chrome_only_as_far_as_the_first_screen_needs() {
    use aterm_containment::{FdaScope, ImageAnchor};
    let mut anchored = access_fixture(FdaState::Granted, DrClass::Identity, FdaScope::Unknown);
    anchored.anchor = ImageAnchor::Deleted;
    let granted = access_fixture(FdaState::Granted, DrClass::Identity, FdaScope::Unknown);
    let chrome = |route: SettingsRoute,
                  access: Option<&MacosAccess>,
                  (width, height): (f32, f32),
                  status: Option<&str>| {
        let cx = view_cx_at(width, height);
        let (mut runtime, instance, view) = paging_view("", route);
        {
            let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
                unreachable!();
            };
            if let Some(access) = access {
                assert!(state.replace_macos_access(access.clone()));
            }
        }
        let _ = compile_settings_view(&runtime, instance, view, &cx);
        if let Some(status) = status {
            let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
                unreachable!();
            };
            // The reload warning: a bar of its own over the page's.
            state.config_watch_status = Some(status.to_string());
            state.show_transient_status_for_test("Saved");
        }
        let compiled = compile_settings_view(&runtime, instance, view, &cx);
        let state = messages_view_state(&runtime, view);
        if status.is_some() {
            let width = SettingsWidth::for_viewport(width);
            assert!(
                settings_status_bars_taken(state, width, cx.viewport)
                    > settings_status_bar_cost(state, width, cx.viewport) + 0.5,
                "the bar is taller than the strip kept for it"
            );
        }
        let plan = state.scroll_plan().expect("a list");
        let heading = compiled
            .semantics
            .iter()
            .any(|node| node.key.as_str().starts_with("settings/page-heading/"));
        let subtitle = compiled
            .semantics
            .iter()
            .any(|node| node.key.as_str().starts_with("settings/page-subtitle/"));
        let press = compiled
            .hits
            .iter()
            .find(|hit| hit.key == UiKey::new(MACOS_ACCESS_OPEN_FDA))
            .map(|hit| hit.rect.height);
        (
            state.page_chrome.get(),
            (plan.first_screen_need(), plan.room()),
            (heading, subtitle),
            press,
        )
    };
    let wide_short = (1_074.0, 348.0);
    // The broken anchor's press needs 212pt: the subtitle's line is not enough.
    let (anchor, (need, room), (heading, subtitle), press) =
        chrome(SettingsRoute::Security, Some(&anchored), wide_short, None);
    assert_eq!(
        anchor,
        PageChrome {
            no_subtitle: true,
            inset: Some(12.0)
        }
    );
    assert_eq!((need, room), (212.0, 234.0));
    assert!(heading && !subtitle, "the heading stays, the subtitle goes");
    assert_eq!(press, Some(44.0), "Open Privacy & Security… whole and live");
    // A granted one's needs 188pt: the subtitle's line is.
    let (step, (need, room), _, press) =
        chrome(SettingsRoute::Security, Some(&granted), wide_short, None);
    assert_eq!(
        step,
        PageChrome {
            no_subtitle: true,
            inset: None
        }
    );
    assert_eq!((need, room), (188.0, 194.0));
    assert_eq!(press, Some(44.0));
    // At 80x20 and 60x20 the broken anchor's press needs 229pt of 216pt.
    for window in [(584.0, 292.0), (444.0, 292.0)] {
        let (step, (need, room), _, press) =
            chrome(SettingsRoute::Security, Some(&anchored), window, None);
        assert_eq!(
            (step.inset, need, room),
            (Some(4.0), 229.0, 232.0),
            "{window:?}"
        );
        assert_eq!(press, Some(44.0), "{window:?}");
    }
    // Bars that come, taller than the strip kept for one, move nothing.
    let warning = "aterm.toml changed on disk; reload to see it.";
    assert_eq!(
        chrome(
            SettingsRoute::Security,
            Some(&anchored),
            wide_short,
            Some(warning)
        )
        .0,
        anchor
    );
    // CONTROLS: a page whose first screen holds its lead keeps its chrome,
    // and so does one no chrome lets hold it.
    let (terminal, (need, room), (_, subtitle), _) =
        chrome(SettingsRoute::Terminal, None, wide_short, None);
    assert_eq!(terminal, PageChrome::FULL, "{need} of {room}");
    assert!(subtitle);
    let (packages, (need, room), ..) = chrome(SettingsRoute::Packages, None, (584.0, 348.0), None);
    assert_eq!(packages, PageChrome::FULL, "{need} of {room}");
    assert!(need > room + 32.0, "no chrome holds it: {need} of {room}");
}

/// Where the compiled page draws its list's controls and words: each node's
/// key, rect and clip, the list's thumb aside.
fn list_paint(compiled: &crate::native_ui::CompiledUi) -> Vec<(String, [i64; 8])> {
    let round = |value: f32| (value * 100.0).round() as i64;
    compiled
        .paint
        .iter()
        .filter(|node| {
            node.key.as_str().starts_with("settings/")
                && !node.key.as_str().starts_with("settings/nav")
                && !node.key.as_str().ends_with("/thumb")
        })
        .map(|node| {
            (
                node.key.as_str().to_string(),
                [
                    round(node.rect.x),
                    round(node.rect.y),
                    round(node.rect.width),
                    round(node.rect.height),
                    round(node.clip.x),
                    round(node.clip.y),
                    round(node.clip.width),
                    round(node.clip.height),
                ],
            )
        })
        .collect()
}

/// A PAGE TURN WITH THE KEYBOARD IN THE LIST LANDS WHERE THE TURN SAYS:
/// Page Down and Page Up take the keyboard to the first (last) control the
/// new screen shows whole, and that control is on the screen, so the render
/// after the turn draws the turn's screen — no reveal moves it. On every
/// route at 80x24, 102x29 and 150x48, with no settings changed and with many,
/// each turn's render draws what the same place draws with no control to
/// reveal. The last screen shows whole the stop over its seat, its top edge
/// cutting only that stop's padding: the keyboard Page Down took to that
/// stop's control was revealed by scrolling up to the stop's own seat, whose
/// screen ends on a peek with the list's last stop cut (Window and
/// Harness at 150x48: the turn showed stops 13..18 and 18..23, the render
/// 13..17 and 18..22). Themes' keyboard moves through its own grid of cards,
/// which follows the card the keyboard is on, and Messages measures its own
/// list.
#[test]
fn a_keyboard_page_turn_lands_where_the_turn_says() {
    let mut turned = 0usize;
    let mut faults = Vec::new();
    for source in ["", CHANGED] {
        for route in SettingsRoute::ALL {
            if matches!(route, SettingsRoute::Messages | SettingsRoute::Themes) {
                continue;
            }
            for (width, height) in PAGING_SIZES {
                let context = format!(
                    "{route:?} {width}x{height}{}",
                    if source.is_empty() { "" } else { " changed" }
                );
                let cx = view_cx_at(width, height);
                let (mut runtime, instance, view) = paging_view(source, route);
                let _ = compile_settings_view(&runtime, instance, view, &cx);
                let Some(plan) = messages_view_state(&runtime, view).scroll_plan() else {
                    continue;
                };
                if plan.limit() == 0 {
                    continue;
                }
                // The keyboard on the list's first control, as a person who
                // tabbed into the page has it.
                let Some(first) = plan
                    .controls()
                    .find(|(stop, _)| plan.shown(0).contains(stop))
                    .map(|(_, key)| key.clone())
                else {
                    continue;
                };
                {
                    let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
                        unreachable!();
                    };
                    state.common.last_focus = Some(first);
                    state.common.focus_visible = true;
                }
                let _ = compile_settings_view(&runtime, instance, view, &cx);
                for direction in [1, -1] {
                    let side = if direction > 0 { "Down" } else { "Up" };
                    let mut before = None;
                    for _ in 0..4 * (plan.limit() + 2) {
                        runtime
                            .dispatch(instance, view, AppEvent::ScrollPage(direction))
                            .unwrap();
                        let focus = messages_view_state(&runtime, view)
                            .common
                            .last_focus
                            .clone();
                        let after_turn =
                            list_paint(&compile_settings_view(&runtime, instance, view, &cx));
                        let drew = messages_view_state(&runtime, view)
                            .scroll_shown()
                            .map(|(_, shown)| shown);
                        // The same place, with no control to reveal.
                        let (settled, meant) = {
                            let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view)
                            else {
                                unreachable!();
                            };
                            let reveal = state.page_reveal.take();
                            let settled =
                                list_paint(&compile_settings_view(&runtime, instance, view, &cx));
                            let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view)
                            else {
                                unreachable!();
                            };
                            state.page_reveal = reveal;
                            (settled, state.scroll_shown().map(|(_, shown)| shown))
                        };
                        let state = messages_view_state(&runtime, view);
                        let place = state.page_scroll;
                        if after_turn != settled {
                            faults.push(format!(
                                "{context}: Page {side} to {place}, the keyboard on {focus:?}: \
                                 its render shows stops {drew:?}, the turn's screen {meant:?}"
                            ));
                        }
                        let plan = state.scroll_plan().expect("a list");
                        let top = place.min(plan.limit());
                        let now = (top, (plan.screen(top).offset * 100.0).round() as i64);
                        if before == Some(now) {
                            break;
                        }
                        turned += 1;
                        before = Some(now);
                    }
                }
            }
        }
    }
    assert!(
        faults.is_empty(),
        "{} faults:\n{}",
        faults.len(),
        faults.join("\n")
    );
    assert!(turned > 0, "the sweep turns pages");
}

/// A STATUS BAR THAT COMES NEVER CHANGES THE PAGE'S CHROME: a short window
/// sheds a page's chrome measured as the page seats with no bar up, so the
/// heading, the subtitle and the insets stay where they were when a bar
/// comes. A bar taller than the strip the page keeps for it (the reload
/// warning, 89pt against 44; the Reset All question, draft recovery where
/// its actions go under its words) takes room from the
/// list and the pinned preview, which re-seated what the list held, and the
/// page shed or took back its chrome under it: 122 of 2,288 pages flipped
/// under the reload warning (Home at 1074x348 took its subtitle and insets
/// back; Software Update at 584x396 shed them), none under a plain status.
#[test]
fn a_status_bar_never_changes_the_page_chrome() {
    let mut shed = 0usize;
    let mut flips = Vec::new();
    for route in SettingsRoute::ALL {
        for (width, height) in [
            (444.0, 412.0),
            (584.0, 412.0),
            (1_074.0, 300.0),
            (1_074.0, 348.0),
            (1_074.0, 412.0),
        ] {
            let cx = view_cx_at(width, height);
            for bar in [
                "reload warning",
                "status",
                "Reset All question",
                "draft recovery",
            ] {
                // Modified lists the draft's own row: its content changed.
                if bar == "draft recovery" && route == SettingsRoute::Modified {
                    continue;
                }
                let (mut runtime, instance, view) = paging_view("", route);
                let _ = compile_settings_view(&runtime, instance, view, &cx);
                let calm = messages_view_state(&runtime, view).page_chrome.get();
                {
                    let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
                        unreachable!();
                    };
                    match bar {
                        "reload warning" => {
                            state.config_watch_status =
                                Some("aterm.toml changed on disk; reload to see it.".to_string());
                        }
                        "status" => state.show_transient_status_for_test("Saved"),
                        "Reset All question" => {
                            state.reset_all_confirmation = true;
                            state.feedback = Some(reset_all_scope(state));
                        }
                        _ => {
                            // A draft of a row no page here draws: the bar
                            // recovers it from any page.
                            state.field_inputs.insert(
                                prefs::EDIT_SYMBOL_FONT.to_string(),
                                crate::native_text_input::TextInputState::new(
                                    "A retained unsaved symbol-font draft".to_string(),
                                ),
                            );
                        }
                    }
                }
                let _ = compile_settings_view(&runtime, instance, view, &cx);
                let barred = messages_view_state(&runtime, view).page_chrome.get();
                if bar == "status" && calm != PageChrome::FULL {
                    shed += 1;
                }
                if calm != barred {
                    flips.push(format!(
                        "{route:?} {width}x{height} under the {bar}: {calm:?} -> {barred:?}"
                    ));
                }
            }
        }
    }
    assert!(
        flips.is_empty(),
        "{} flips:\n{}",
        flips.len(),
        flips.join("\n")
    );
    assert!(shed > 0, "the sweep reaches pages that shed their chrome");
}

/// THE LIST'S KEYS NEVER WORK A CONTROL THEY CARRIED THE KEYBOARD TO: Page
/// Up and Down and Home and End take the keyboard through the list, and a
/// slider they put it on takes those keys for itself only once the person
/// works it. A Page Up that took the keyboard to Text & Fonts' Weight slider
/// (the last control of the first screen) made every Page Up after it a step
/// down the font's weight, written to aterm.toml, and End from the slider a
/// list ended on set it to its top. On every route at 80x24, 102x29 and
/// 150x48, with no settings changed and with many, the keyboard on the
/// list's first control, every Page Down to the list's end, End, every Page
/// Up to its top, one more, and Home write nothing; at 102x29, so do Down
/// and Up through every control of the list (a colour wheel takes them only
/// once worked). A slider the person works takes them again.
#[test]
fn the_lists_keys_never_change_a_control_they_carried_the_keyboard_to() {
    let end = AppEvent::TextInput(TextInputEvent::End { extend: false });
    let home = AppEvent::TextInput(TextInputEvent::Home { extend: false });
    let writes = |outcome: &crate::native_app::DispatchOutcome| {
        outcome
            .effects
            .iter()
            .any(|effect| matches!(effect, AppEffect::ConfigPatch { .. }))
    };
    let mut faults = Vec::new();
    let mut on_a_slider = 0usize;
    for source in ["", CHANGED] {
        for route in SettingsRoute::ALL {
            if matches!(route, SettingsRoute::Messages | SettingsRoute::Themes) {
                continue;
            }
            for (width, height) in PAGING_SIZES {
                let context = format!(
                    "{route:?} {width}x{height}{}",
                    if source.is_empty() { "" } else { " changed" }
                );
                let cx = view_cx_at(width, height);
                let (mut runtime, instance, view) = paging_view(source, route);
                let _ = compile_settings_view(&runtime, instance, view, &cx);
                let Some(plan) = messages_view_state(&runtime, view).scroll_plan() else {
                    continue;
                };
                let Some((_, first)) = plan.controls().next() else {
                    continue;
                };
                {
                    let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
                        unreachable!();
                    };
                    state.common.last_focus = Some(first.clone());
                    state.common.focus_visible = true;
                }
                let _ = compile_settings_view(&runtime, instance, view, &cx);
                let turns = plan.limit() + 2;
                let mut keys = Vec::new();
                keys.extend(std::iter::repeat_n(AppEvent::ScrollPage(1), turns));
                keys.push(end.clone());
                keys.extend(std::iter::repeat_n(AppEvent::ScrollPage(-1), turns + 1));
                keys.push(home.clone());
                if (width, height) == PAGING_SIZES[1] {
                    let steps = plan.controls().count() + 1;
                    keys.extend(std::iter::repeat_n(AppEvent::StepFocus(1), steps));
                    keys.extend(std::iter::repeat_n(AppEvent::StepFocus(-1), steps));
                }
                for key in keys {
                    let outcome = runtime.dispatch(instance, view, key.clone()).unwrap();
                    let _ = compile_settings_view(&runtime, instance, view, &cx);
                    let state = messages_view_state(&runtime, view);
                    if state.focused_slider() || state.focused_color_wheel().is_some() {
                        on_a_slider += 1;
                    }
                    if writes(&outcome) {
                        faults.push(format!(
                            "{context}: {key:?} on {:?} wrote a setting",
                            state.common.last_focus
                        ));
                    }
                }
            }
        }
    }
    assert!(
        faults.is_empty(),
        "{} faults:\n{}",
        faults.len(),
        faults.join("\n")
    );
    assert!(
        on_a_slider > 0,
        "the list's keys carry the keyboard to sliders"
    );

    // A slider the person works, or put the keyboard on themselves (Tab, a
    // click), takes the page keys and Home and End.
    let cx = view_cx_at(1_074.0, 684.0);
    let weight = UiKey::new(format!("settings/control/{}", prefs::EDIT_FONT_WEIGHT));
    let on_weight = |carried: bool| {
        let (mut runtime, instance, view) = paging_view("", SettingsRoute::TextFonts);
        let _ = compile_settings_view(&runtime, instance, view, &cx);
        {
            let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
                unreachable!();
            };
            state.common.last_focus = Some(weight.clone());
            state.list_carried = carried.then(|| weight.clone());
        }
        let _ = compile_settings_view(&runtime, instance, view, &cx);
        (runtime, instance, view)
    };
    let (mut runtime, instance, view) = on_weight(true);
    for key in [AppEvent::ScrollPage(-1), AppEvent::ScrollPage(1), end, home] {
        let outcome = runtime.dispatch(instance, view, key.clone()).unwrap();
        assert!(
            !writes(&outcome),
            "{key:?}: the list's keys carried it there"
        );
        let _ = compile_settings_view(&runtime, instance, view, &cx);
    }
    let (mut runtime, instance, view) = on_weight(true);
    let right = runtime
        .dispatch(
            instance,
            view,
            AppEvent::TextInput(TextInputEvent::Right { extend: false }),
        )
        .unwrap();
    assert!(writes(&right), "an arrow works the slider");
    assert!(
        !messages_view_state(&runtime, view).list_keys_carried(),
        "a worked slider takes its own keys"
    );
    let (mut runtime, instance, view) = on_weight(false);
    let page_up = runtime
        .dispatch(instance, view, AppEvent::ScrollPage(-1))
        .unwrap();
    assert!(
        writes(&page_up),
        "a slider Tab or a click put the keyboard on takes Page Up"
    );
    assert_eq!(
        messages_view_state(&runtime, view).common.last_focus,
        Some(weight.clone())
    );
    // Carried there, then Tab away and Shift-Tab back (the host's focus
    // changes): the person put the keyboard on it, and it takes Page Up.
    let (mut runtime, instance, view) = on_weight(true);
    let size = UiKey::new(format!("settings/control/{}", prefs::EDIT_FONT_PX));
    for focus in [size, weight.clone()] {
        runtime
            .dispatch(instance, view, AppEvent::FocusChanged(Some(focus)))
            .unwrap();
        let _ = compile_settings_view(&runtime, instance, view, &cx);
    }
    let page_up = runtime
        .dispatch(instance, view, AppEvent::ScrollPage(-1))
        .unwrap();
    assert!(
        writes(&page_up),
        "a slider Tab put the keyboard back on takes Page Up"
    );
    assert_eq!(
        messages_view_state(&runtime, view).common.last_focus,
        Some(weight)
    );
}

/// A NOTE A RUN OF ROWS SHARES HEADS NO CONTROL, ON ANY PAGE: on every route
/// at 80x24, 102x29 and 150x48, with no settings changed and with many, every
/// screen's stop that paints a run's shared note (`settings/shared-note/…`)
/// holds no control. A page that lays its own cards out (Cursor Styles'
/// Adjust card, Harness's coding agents) joined the note to the switch after
/// it — Bloom, Autopilot — which a screen whose top edge cut the note drew
/// whole uncounted, and Page Up showed again; a list of settings lists the
/// note as the stop after its run's last row.
#[test]
fn a_shared_note_heads_no_control_on_any_page() {
    let mut notes = 0usize;
    let mut faults = Vec::new();
    for source in ["", CHANGED] {
        for route in SettingsRoute::ALL {
            if matches!(route, SettingsRoute::Messages | SettingsRoute::Themes) {
                continue;
            }
            for (width, height) in PAGING_SIZES {
                let cx = view_cx_at(width, height);
                let (mut runtime, instance, view) = paging_view(source, route);
                let _ = compile_settings_view(&runtime, instance, view, &cx);
                let Some(plan) = messages_view_state(&runtime, view).scroll_plan() else {
                    continue;
                };
                for top in 0..=plan.limit() {
                    {
                        let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view)
                        else {
                            unreachable!();
                        };
                        state.page_scroll = top;
                        state.page_reveal = None;
                    }
                    let compiled = compile_settings_view(&runtime, instance, view, &cx);
                    let Some(plan) = messages_view_state(&runtime, view).scroll_plan() else {
                        continue;
                    };
                    let Some(list) = painted_list(&compiled, &plan, top) else {
                        continue;
                    };
                    for band in plan.drawn_bands(top) {
                        let (from, to) = (list.y + band.at, list.y + band.at + band.height);
                        let note = compiled.paint.iter().find(|node| {
                            let middle = node.rect.y + node.rect.height / 2.0;
                            node.key.as_str().starts_with("settings/shared-note/")
                                && middle >= from
                                && middle < to
                        });
                        let Some(note) = note else {
                            continue;
                        };
                        notes += 1;
                        if plan.controls().any(|(stop, _)| stop == band.stop) {
                            faults.push(format!(
                                "{route:?} {width}x{height}{} screen {top}: stop {} holds {} \
                                 and a control",
                                if source.is_empty() { "" } else { " changed" },
                                band.stop,
                                note.key.as_str()
                            ));
                        }
                    }
                }
            }
        }
    }
    faults.dedup();
    assert!(
        faults.is_empty(),
        "{} faults:\n{}",
        faults.len(),
        faults.join("\n")
    );
    assert!(notes > 0, "the sweep paints shared notes");
}

/// A COPY WITHOUT ITS PACKAGE TOOL, AS THE LAB'S HEADLESS COPY IS: Packages
/// at 80x24 draws the reroute switch, its own note and all, whole over the
/// consent note ("Saved for a copy of aterm that has its package tool.") the
/// hard edge cuts away. That screen counts the switch, and Page Down turns to
/// the note — never to the switch again (it did while the note was joined to
/// it as one stop).
#[test]
fn packages_without_its_tool_turns_past_the_reroute_switch_it_drew_whole() {
    let mut service = crate::packages_screen::PackagesService::new();
    assert!(service.note_tool(false));
    let sequence = service.begin(None).expect("a status read starts");
    assert!(service.finish(
        sequence,
        crate::packages_screen::PackagesWorkerCompletion::refresh(
            crate::packages_screen::PackagesStatusReport::from_parts(
                false,
                atpkg::manager_enabled(),
                "fp".to_string(),
                None,
                &[],
            ),
        ),
    ));
    let cx = view_cx_at(584.0, 348.0);
    let (mut runtime, instance, view) = paging_view("", SettingsRoute::Packages);
    assert!(runtime.replace_settings_packages(service.state(false, true, false), 3));
    let switch = "settings/control/reroute.announce";
    let note = "packages/detail/reroute.announce";
    let whole = |runtime: &NativeRuntime| {
        let compiled = compile_settings_view(runtime, instance, view, &cx);
        let state = messages_view_state(runtime, view);
        let plan = state.scroll_plan().expect("a list");
        let top = state.page_scroll.min(plan.limit());
        let list = painted_list(&compiled, &plan, top).expect("a painted list");
        let drawn_whole = |key: &str| {
            compiled.paint.iter().any(|node| {
                node.key.as_str() == key
                    && node.fade.is_none()
                    && (node.clip.y - node.rect.y).abs() < 0.5
                    && (node.clip.height - node.rect.height).abs() < 0.5
                    && node.rect.y >= list.y - 0.5
                    && node.rect.bottom() <= list.bottom() + 0.5
            })
        };
        (top, plan.limit(), drawn_whole(switch) && drawn_whole(note))
    };
    let mut before = whole(&runtime);
    let mut drew_it = false;
    for _ in 0..12 {
        runtime
            .dispatch(instance, view, AppEvent::ScrollPage(1))
            .unwrap();
        let now = whole(&runtime);
        if now.0 == before.0 {
            break;
        }
        drew_it |= before.2;
        assert!(
            !(before.2 && now.2 && now.0 < now.1),
            "Page Down from {} to {} draws the reroute switch and its note whole again",
            before.0,
            now.0
        );
        before = now;
    }
    assert!(
        drew_it,
        "a screen draws the reroute switch and its note whole"
    );
}

/// OPENING A MESSAGE NEVER CHANGES THE PAGE'S CHROME: Messages measures its
/// first screen with every entry closed. Measured with the open entry, a
/// usage-limit notice too tall for the room under the pinned header (its
/// header then scrolls away with the list) left the first screen holding no
/// entry, and the page shed its subtitle as the entry opened and took it
/// back as it closed — the heading and the whole list jumped under the
/// pointer that pressed the title (864x462).
#[test]
fn opening_a_message_never_changes_the_page_chrome() {
    let mut log = sample_messages();
    let mut notice = log.entries[0].clone();
    notice.id = 41;
    notice.tag = "harness".to_string();
    notice.severity = "warn";
    notice.title = "Claude usage limit reached in tab 1".to_string();
    notice.detail = [
        "Run aterm harness accounts",
        "Claude Code hit this account's usage limit, and no other listed account can take over.",
        "aterm keeps this tab on its own account, so no other account replaces its login.",
        "aterm continues the tab by itself after the reset on Oct 23 at 9am.",
    ]
    .map(str::to_string)
    .to_vec();
    notice.sentences = 4;
    notice.state = "live";
    notice.retired_unix_ms = None;
    log.entries.insert(0, notice);
    log.tags.push(("harness".to_string(), 1));
    let mut faults = Vec::new();
    for (width, height) in [
        (864.0, 462.0),
        (1_074.0, 348.0),
        (1_074.0, 420.0),
        (738.0, 418.0),
    ] {
        let (mut runtime, instance, view) = setup_with_messages();
        assert!(runtime.replace_settings_messages(log.clone(), 8));
        let cx = view_cx_at(width, height);
        let _ = compile_settings_view(&runtime, instance, view, &cx);
        let closed = messages_view_state(&runtime, view).page_chrome.get();
        select_message(&mut runtime, instance, view, 41);
        let _ = compile_settings_view(&runtime, instance, view, &cx);
        let open = messages_view_state(&runtime, view).page_chrome.get();
        if open != closed {
            faults.push(format!("{width}x{height}: {closed:?} -> {open:?}"));
        }
    }
    assert!(faults.is_empty(), "{faults:#?}");
}
