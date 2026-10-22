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

/// The screen a render draws now, from the stop it drew from: not the
/// scroll it was asked for, which a landing or a reveal moves.
fn screen_now(
    runtime: &NativeRuntime,
    instance: crate::native_app::AppInstanceId,
    view: crate::native_app::ViewId,
    cx: &ViewCx<'_>,
) -> Option<(Turned, scroll::ScrollPlan)> {
    let compiled = compile_settings_view(runtime, instance, view, cx);
    let state = messages_view_state(runtime, view);
    let plan = state.scroll_plan()?;
    let top = state
        .scroll_record
        .borrow()
        .as_ref()
        .map_or(0, |record| record.top());
    let whole = shown_whole(&compiled, &plan, top);
    let offset = plan.screen(top).offset;
    Some((Turned { top, offset, whole }, plan))
}

/// MODIFIED'S ROW WHOSE FACTS THE STRIP HOLDS IS SHOWN WHOLE, AND PAGE DOWN
/// TURNS PAST IT (review): every Modified row carries its facts on a
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
                state.scroll_page_to(top);
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
                    state.scroll_page_to(top);
                    state.page_reveal = None;
                    if let Some((screen, _)) = screen_now(&runtime, instance, view, &cx) {
                        if screen.top != top {
                            faults.push(format!(
                                "{context}: screen {top} draws from stop {}",
                                screen.top
                            ));
                        }
                        showable.extend(screen.whole);
                    }
                }
                {
                    let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
                        unreachable!();
                    };
                    state.scroll_page_to(0);
                    state.page_reveal = None;
                }
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
/// screen with stops after it, at 60x20, 80x24, 102x29, 150x48 and 167x36
/// (with no settings changed and with many), the card the list's bottom edge
/// cuts is drawn on to the page's bottom — its room and the strip kept for a
/// status bar — with no band of blank over 8pt under it; a screen that ends on
/// a whole card instead ends on that card's boundary, the gap and less than
/// [`scroll::PEEK_MIN`] of the next card under it, and the next screen starts
/// with the next card — the list's first screen only: any later one scrolls
/// on into its first card's padding until the next card peeks in (the peek
/// seat), where it ended on a whole card over up to 26pt of blank with
/// nothing of the next in view (Window at 150x48 under SESSION, Appearance at
/// 167x36 under TEXT & CONTRAST). A list that ends on whole stops
/// ([`scroll::ListFoot::Whole`], Themes' chooser beside the demo) cuts no
/// card and ends on its last whole stop, less than the next stop's height
/// over the page's bottom. Judged from the paint. The hard edge used to cut a
/// card that would end whole PEEK_MIN short of its bottom (Packages at 80x24:
/// a 36pt band) and to land on a caption's top (Software Update at 60x20).
#[test]
fn no_screen_ends_on_blank_under_a_card_it_cuts() {
    let (mut cut_screens, mut whole_foot_screens) = (0usize, 0usize);
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
                (1_193.0, 516.0),
            ] {
                let cx = view_cx_at(width, height);
                let (mut runtime, instance, view) = paging_view(source, route);
                each_screen_of(&mut runtime, instance, view, &cx, |state, compiled, top| {
                    let Some(plan) = state.scroll_plan() else {
                        return;
                    };
                    let Some(list) = painted_list(compiled, &plan, top) else {
                        return;
                    };
                    // A screen that draws on to the list's end has nothing
                    // after it to peek in.
                    if plan
                        .drawn_bands(top)
                        .last()
                        .is_none_or(|band| band.stop + 1 >= plan.len())
                    {
                        return;
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
                    if plan.foot() == scroll::ListFoot::Whole {
                        whole_foot_screens += 1;
                        // The room the next stop would take on this screen.
                        let bands = plan.drawn_bands(top);
                        let (first, next) = (bands[0].stop, bands[bands.len() - 1].stop + 1);
                        let takes = plan.span(first..next + 1) - plan.span(first..next);
                        if cut || band + 0.5 >= takes {
                            faults.push(format!(
                                "{context}: a list that ends on whole stops {} \
                                 ({band}pt over the page's bottom, the next stop {takes}pt)",
                                if cut { "cuts a card" } else { "ends short" }
                            ));
                        }
                    } else if cut {
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
                    } else if top > 0
                        && band > plan.gap() + 0.5
                        && plan
                            .peek_lack(top)
                            .is_some_and(|(lacks, padding)| lacks <= padding + 0.01)
                    {
                        // THE PEEK SEAT: a screen that is not the list's
                        // first scrolls on into its first card's padding
                        // until the next card peeks in — where that padding
                        // holds what the peek lacks: past it the scroll would
                        // cut the first stop's words or controls, and the
                        // screen ends on its card's boundary (Appearance at
                        // 60x20: 1.5pt short of the gap's end, 14.5pt of
                        // peek lacking under a row's 4pt of padding).
                        faults.push(format!(
                            "{context}: ends {band}pt above the page's bottom on a whole card, \
                             nothing of the next in view"
                        ));
                    }
                });
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
    assert!(
        whole_foot_screens > 0,
        "the sweep holds screens of a list that ends on whole stops"
    );
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
                    state.scroll_page_to(top);
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
            inset: Some(12.0),
            bar_room_given: false,
            preview_listed: false,
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
            inset: None,
            bar_room_given: false,
            preview_listed: false,
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

/// THE BAR'S ROOM AND THE CHROME NEVER STARVE THE LIST (#58, [`PageChrome`]):
/// at 220×12 with 2× text (1564×180) Settings ▸ Themes kept 64pt for a status
/// bar that was not up and gave its chooser 68pt, so every 72pt row was cut;
/// the list now takes that room, and every row of the chooser is whole on its
/// screen. A bar that comes then takes its room from the list, which is laid
/// out again under it, and the decision, made with no bar up, stands. The
/// page sheds no more than leaves every stop whole: Top Settings' font rows,
/// Tab Color's wheel and Packages' rows at 220×12 at 2× text, each whole on
/// the screen that lets its card's caption go, take the bar's room and keep
/// the subtitle (counted under their captions, they read as cut and shed it
/// too). Cursor Styles lists its preview as the list's first stop only where
/// its leanest chrome still cuts stops under the pinned preview (80x16 at 2×
/// text: its 92pt style tiles under the pinned preview had an 88pt list);
/// at 220×16 it once pinned the preview and cut every style row but Off by
/// 14pt under its group's caption, and at 220×12, 220×16 and from 220×20 it
/// keeps the preview pinned over whole rows (each whole on the screen that
/// lets its caption go), as at 849×300 and 150×24 at 1×. CONTROLS: every
/// shipped size keeps the bar's room, its chrome and the pinned preview.
#[test]
fn the_room_kept_for_a_status_bar_never_cuts_every_row() {
    at_each_text_scale(&[1.0, 2.0], "the bar's room", |scale| {
        let seated = |route: SettingsRoute, (width, height): (f32, f32), bar: bool| {
            let cx = view_cx_at(width, height);
            let (mut runtime, instance, view) = paging_view("", route);
            let _ = compile_settings_view(&runtime, instance, view, &cx);
            if bar {
                let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
                    unreachable!();
                };
                state.show_transient_status_for_test("Saved");
            }
            let compiled = compile_settings_view(&runtime, instance, view, &cx);
            let state = messages_view_state(&runtime, view);
            let plan = state.scroll_plan().expect("a list");
            (
                state.page_chrome.get(),
                plan.room(),
                plan.cut_stops(),
                compiled,
            )
        };
        let preview = UiKey::new("settings/preview/cursor-styles");
        if scale > 1.5 {
            let short = (1_564.0, 180.0);
            let (chrome, room, cuts, _) = seated(SettingsRoute::Themes, short, false);
            assert!(
                chrome.bar_room_given,
                "Themes gives its chooser the bar's room"
            );
            assert_eq!(cuts, 0, "every row of the chooser is whole in {room}pt");
            let (still, barred, _, compiled) = seated(SettingsRoute::Themes, short, true);
            assert_eq!(
                still, chrome,
                "a bar that comes changes nothing of the chrome"
            );
            assert!(
                compiled
                    .semantics
                    .iter()
                    .any(|node| node.key.as_str() == "settings/status"),
                "the bar is up"
            );
            assert!(barred < room - 0.5, "and takes its room from the list");
            for route in [
                SettingsRoute::Home,
                SettingsRoute::TabColor,
                SettingsRoute::Packages,
                SettingsRoute::CursorStyles,
            ] {
                let (chrome, room, cuts, _) = seated(route, short, false);
                assert_eq!(
                    cuts, 0,
                    "{route:?} at 1564×180: every stop whole in {room}pt"
                );
                assert!(
                    chrome.bar_room_given,
                    "{route:?} at 1564×180 sheds for it: {chrome:?}"
                );
            }
        }
        // Cursor Styles' preview, listed where only listing it leaves every
        // stop whole, else pinned over whole rows.
        let windows: &[((f32, f32), bool)] = if scale > 1.5 {
            &[
                ((584.0, 236.0), true),
                ((1_564.0, 180.0), false),
                ((1_564.0, 240.0), false),
                ((1_564.0, 280.0), false),
                ((1_074.0, 348.0), false),
            ]
        } else {
            &[((849.0, 300.0), false), ((1_074.0, 348.0), false)]
        };
        for &(window, listed) in windows {
            let cx = view_cx_at(window.0, window.1);
            let (runtime, instance, view) = paging_view("", SettingsRoute::CursorStyles);
            let _ = compile_settings_view(&runtime, instance, view, &cx);
            let state = messages_view_state(&runtime, view);
            let plan = state.scroll_plan().expect("a list");
            assert_eq!(
                plan.cut_stops(),
                0,
                "{window:?} at {scale}×: every stop whole in {}pt",
                plan.room()
            );
            assert_eq!(
                plan.stop_of_preview(&preview),
                listed.then_some(0),
                "{window:?} at {scale}×: the preview listed first ({listed}) or pinned"
            );
            assert_eq!(state.page_chrome.get().preview_listed, listed);
        }
        // CONTROLS: the shipped sizes keep the bar's room, the chrome the
        // lead keeps, and the pinned preview.
        for route in [
            SettingsRoute::Themes,
            SettingsRoute::CursorStyles,
            SettingsRoute::Sound,
            SettingsRoute::Home,
        ] {
            for window in [
                (584.0, 348.0),
                (738.0, 418.0),
                (1_074.0, 684.0),
                (1_564.0, 854.0),
            ] {
                let (chrome, ..) = seated(route, window, false);
                assert_eq!(
                    chrome,
                    PageChrome::FULL,
                    "{route:?} {window:?} at {scale}× keeps its chrome and the bar's room"
                );
            }
        }
    });
}

/// THE CHROME IS DECIDED ONCE PER LAYOUT INPUT, NOT EVERY FRAME (#64): a
/// short window sheds its page's chrome by seating the page under each rung
/// of a ladder, and an animated preview re-rendered the page every frame, so
/// every frame ran the ladder again. Frames that move only the clock now
/// take the chrome the last decision chose — the same chrome, every render
/// the tests make deciding again to prove it ([`page`]) — while a change
/// of content (an event), of window or of text size decides again.
#[test]
fn a_short_windows_chrome_is_decided_once_per_layout_input() {
    let remembered = || super::CHROME_REMEMBERED.with(std::cell::Cell::get);
    let short = (1_074.0, 348.0);
    let at = |phase: u64, (width, height): (f32, f32)| ViewCx {
        animation_phase_ms: phase,
        ..view_cx_at(width, height)
    };
    // Top Settings, its cursor preview animated, sheds its subtitle and
    // its insets to 12pt at 1074×348: three rungs a frame.
    let (mut runtime, instance, view) = paging_view("", SettingsRoute::Home);
    let first = compile_settings_view(&runtime, instance, view, &at(0, short));
    let chrome = messages_view_state(&runtime, view).page_chrome.get();
    assert_ne!(chrome, PageChrome::FULL, "the short window sheds chrome");
    let before = remembered();
    for phase in [16, 33, 50, 66] {
        let frame = compile_settings_view(&runtime, instance, view, &at(phase, short));
        assert_eq!(
            messages_view_state(&runtime, view).page_chrome.get(),
            chrome
        );
        assert_eq!(
            list_paint(&frame),
            list_paint(&first),
            "a frame of the clock lays out what the first did"
        );
    }
    assert_eq!(
        remembered() - before,
        4,
        "each frame took the remembered chrome"
    );
    // An event, a window and a text size each decide again.
    runtime
        .dispatch(instance, view, AppEvent::ScrollLines(1))
        .unwrap();
    let before = remembered();
    let _ = compile_settings_view(&runtime, instance, view, &at(83, short));
    assert_eq!(remembered(), before, "an event decides again");
    let _ = compile_settings_view(&runtime, instance, view, &at(100, (1_074.0, 360.0)));
    assert_eq!(remembered(), before, "a window decides again");
    crate::native_appearance::install_preferences(
        crate::native_appearance::AppearancePreferences {
            text_scale: 1.25,
            ..crate::native_appearance::current_preferences()
        },
    );
    let _ = compile_settings_view(&runtime, instance, view, &at(116, (1_074.0, 360.0)));
    crate::native_appearance::install_preferences(
        crate::native_appearance::AppearancePreferences {
            text_scale: 1.0,
            ..crate::native_appearance::current_preferences()
        },
    );
    assert_eq!(remembered(), before, "a text size decides again");
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
/// Up and Down and Home and End take the keyboard through the list, and
/// stay the list's on a slider, however the keyboard got there. A Page Up
/// that took the keyboard to Text & Fonts' Weight slider
/// (the last control of the first screen) made every Page Up after it a step
/// down the font's weight, written to aterm.toml, and End from the slider a
/// list ended on set it to its top. On every route at 80x24, 102x29 and
/// 150x48, with no settings changed and with many, the keyboard on the
/// list's first control, every Page Down to the list's end, End, every Page
/// Up to its top, one more, and Home write nothing; at 102x29, so do Down
/// and Up through every control of the list (a colour wheel takes them only
/// once worked). A slider the person worked or put the keyboard on (Tab, a
/// click) writes nothing on those keys either; its arrows work it.
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
    // click), takes its arrows; the page keys and Home and End stay the
    // list's, so they never write there either.
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
    for key in [
        AppEvent::ScrollPage(-1),
        AppEvent::ScrollPage(1),
        end.clone(),
        home.clone(),
    ] {
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
    for key in [AppEvent::ScrollPage(-1), AppEvent::ScrollPage(1), end, home] {
        let (mut runtime, instance, view) = on_weight(false);
        let outcome = runtime.dispatch(instance, view, key.clone()).unwrap();
        assert!(
            !writes(&outcome),
            "{key:?}: a slider Tab or a click put the keyboard on writes nothing"
        );
    }
    let (mut runtime, instance, view) = on_weight(false);
    let right = runtime
        .dispatch(
            instance,
            view,
            AppEvent::TextInput(TextInputEvent::Right { extend: true }),
        )
        .unwrap();
    assert!(writes(&right), "its arrows work it");
    // Carried there, then Tab away and Shift-Tab back (the host's focus
    // changes): the person put the keyboard on it, and Page Up still turns
    // the list.
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
        !writes(&page_up),
        "a slider Tab put the keyboard back on writes nothing on Page Up"
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
                each_screen_of(&mut runtime, instance, view, &cx, |state, compiled, top| {
                    let Some(plan) = state.scroll_plan() else {
                        return;
                    };
                    let Some(list) = painted_list(compiled, &plan, top) else {
                        return;
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
                });
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
/// it as one stop). Its own note is the line under its name where the name's
/// line cannot hold its tags (Helvetica Neue's measures); where it can
/// (Cantarell's), the row has no such line, and is drawn whole alone.
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
    // The switch's row with its note: the row and the line under it, held
    // together, where it has that line; else the row.
    let row = "packages/switch-row/reroute.announce";
    let noted = "packages/switch-row/reroute.announce/with-note";
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
        let row = if compiled.paint.iter().any(|node| node.key.as_str() == noted) {
            noted
        } else {
            row
        };
        (top, plan.limit(), drawn_whole(switch) && drawn_whole(row))
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

/// A LIST WALKS ITS CONTROLS IN THE ORDER THE PAGE DRAWS THEM: Tab past a
/// screen's edge and the list's keys go by the list's order of controls, so
/// every screen's controls stand in it as the compiled page orders them. A
/// stacked row listed its control ahead of the Reset its label line draws
/// over it, and Tab from Appearance's Faint text opacity slider (80x24, 2x
/// text, the setting changed) went back up to that Reset, and from there
/// down to the slider again: the keyboard never left the row. Every route
/// at 80x24, 102x29 and 150x48, 1x and 2x text, each screen of its list.
#[test]
fn a_list_walks_its_controls_in_the_order_the_page_draws_them() {
    let source = format!("{CHANGED}faint_opacity = 0.4\nbackground_opacity = 0.9\n");
    at_each_text_scale(&[1.0, 2.0], "list order", |scale| {
        let mut faults = Vec::new();
        let mut screens = 0usize;
        for route in SettingsRoute::ALL {
            for (width, height) in PAGING_SIZES {
                let cx = view_cx_at(width, height);
                let (mut runtime, instance, view) = paging_view(&source, route);
                let mut compiled = compile_settings_view(&runtime, instance, view, &cx);
                let Some(plan) = messages_view_state(&runtime, view).scroll_plan() else {
                    continue;
                };
                for _ in 0..plan.limit() + 2 {
                    let plan = messages_view_state(&runtime, view)
                        .scroll_plan()
                        .expect("the list stays listed");
                    let listed: Vec<(usize, &UiKey)> = compiled
                        .focus_order
                        .iter()
                        .filter_map(|key| plan.control_index(key).map(|index| (index, key)))
                        .collect();
                    screens += 1;
                    if let Some(pair) = listed.windows(2).find(|pair| pair[1].0 < pair[0].0) {
                        faults.push(format!(
                            "{route:?} {width}x{height} {scale}x: {} is drawn before {} but listed after it",
                            pair[0].1.as_str(),
                            pair[1].1.as_str()
                        ));
                    }
                    runtime
                        .dispatch(instance, view, AppEvent::ScrollPage(1))
                        .unwrap();
                    compiled = compile_settings_view(&runtime, instance, view, &cx);
                }
            }
        }
        assert!(screens > 0, "lists were walked");
        assert!(
            faults.is_empty(),
            "{} faults:\n{}",
            faults.len(),
            faults.join("\n")
        );
    });
}

/// The short windows the short-window sweep covers: 220x12, 220x13 and
/// 220x16, 102x12, 102x16 and 80x16 (the narrow windows' 180-240pt), 60x20,
/// 80x20 and 80x24.
const SHORT_SIZES: [(f32, f32); 9] = [
    (1_564.0, 180.0),
    (1_564.0, 194.0),
    (1_564.0, 236.0),
    (738.0, 180.0),
    (738.0, 236.0),
    (584.0, 236.0),
    (444.0, 292.0),
    (584.0, 292.0),
    (584.0, 348.0),
];

/// [`shown_whole`], a list that shows all of itself — drawn with no thumb —
/// included: every stop it draws whole there.
fn shown_whole_on(
    compiled: &crate::native_ui::CompiledUi,
    plan: &scroll::ScrollPlan,
    top: usize,
) -> BTreeSet<usize> {
    let screen = plan.screen(top.min(plan.limit()));
    if !screen.whole {
        return shown_whole(compiled, plan, top);
    }
    let Some(list) = compiled.paint.iter().find(|node| {
        node.key.as_str() != "settings/navigation/list"
            && matches!(&node.content, UiContent::Group(spec) if spec.role == SemanticRole::List)
            && (node.rect.height - screen.height).abs() < 0.5
    }) else {
        return BTreeSet::new();
    };
    let list = list.rect;
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
                        && middle >= from - 0.01
                        && middle < to - 0.01
                })
                .collect();
            !nodes.is_empty()
                && nodes.iter().all(|node| {
                    (node.clip.y - node.rect.y).abs() < 0.5
                        && (node.clip.height - node.rect.height).abs() < 0.5
                })
        })
        .map(|band| band.stop)
        .collect()
}

/// What one walk over a list's screens found: the stops it showed whole,
/// each group caption it drew and whether it drew that whole on some screen,
/// and what the audit flagged on any screen it drew.
#[derive(Default)]
struct Walk {
    whole: BTreeSet<usize>,
    captions: BTreeMap<String, bool>,
    faults: Vec<String>,
}

/// Each group caption the compiled screen whose first stop is `top` draws on
/// its list, by key (a caption on several lines as one), and whether it is
/// drawn whole in the list's room: none of its lines cut by an edge, fading
/// in the strip under the room, or past it.
fn captions_drawn(
    compiled: &crate::native_ui::CompiledUi,
    plan: &scroll::ScrollPlan,
    top: usize,
) -> BTreeMap<String, bool> {
    let mut captions = BTreeMap::new();
    let screen = plan.screen(top.min(plan.limit()));
    let Some(list) = painted_list(compiled, plan, top) else {
        return captions;
    };
    let room = if screen.whole {
        list.height
    } else {
        plan.room()
    };
    for node in &compiled.paint {
        let UiContent::Text(text) = &node.content else {
            continue;
        };
        let middle = node.rect.y + node.rect.height / 2.0;
        if text.role != SemanticRole::Heading
            || text.style != StyleRef::Quiet
            || node.clip.is_empty()
            || node.rect.x < list.x - 0.5
            || node.rect.right() > list.right() + 0.5
            || middle < list.y
            || middle > list.bottom()
        {
            continue;
        }
        let key = node.key.as_str();
        let key = key
            .rsplit_once("/line-")
            .map_or(key, |(caption, _)| caption);
        let whole = node.fade.is_none()
            && (node.clip.y - node.rect.y).abs() < 0.5
            && (node.clip.height - node.rect.height).abs() < 0.5
            && node.rect.y >= list.y - 0.5
            && node.rect.bottom() <= list.y + room + 0.5;
        *captions.entry(key.to_string()).or_insert(true) &= whole;
    }
    captions
}

/// Walk the list from where it stands, each `event` drawn as the window draws
/// between presses, until one moves it no further: every stop each screen
/// shows whole, judged from its paint, and each screen's audit.
fn walk_screens(
    runtime: &mut NativeRuntime,
    instance: crate::native_app::AppInstanceId,
    view: crate::native_app::ViewId,
    cx: &ViewCx<'_>,
    event: &AppEvent,
) -> Walk {
    let mut walk = Walk::default();
    let mut before = None;
    for _ in 0..400 {
        let compiled = compile_settings_view(runtime, instance, view, cx);
        let state = messages_view_state(runtime, view);
        let Some(plan) = state.scroll_plan() else {
            break;
        };
        let top = state.page_scroll.min(plan.limit());
        let place = (top, (plan.screen(top).offset * 100.0).round() as i64);
        if before == Some(place) {
            break;
        }
        before = Some(place);
        walk.whole.extend(shown_whole_on(&compiled, &plan, top));
        for (caption, whole) in captions_drawn(&compiled, &plan, top) {
            *walk.captions.entry(caption).or_insert(false) |= whole;
        }
        walk.faults.extend(
            compiled
                .fit_failures()
                .into_iter()
                .chain(compiled.sliced_marks())
                .map(|fault| format!("screen {top}: {fault}")),
        );
        runtime.dispatch(instance, view, event.clone()).unwrap();
    }
    walk
}

/// EVERY STOP OF A SHORT WINDOW'S LIST IS WHOLE ON SOME SCREEN, BY PAGE DOWN
/// AND BY THE WHEEL, AND NO SCREEN SLICES A WORD OR A PICTOGRAM AT AN EDGE
/// (#96-#99, and the sweep that followed them): every route, at 220x12,
/// 220x13, 220x16, 102x12, 102x16, 80x16, 60x20, 80x20 and 80x24, 1× and 2×
/// text, both themes, from the list's top a stop at a time (the wheel) and a
/// screen at a time (Page Down), each screen judged from its paint
/// ([`shown_whole`]): every stop that draws anything is drawn whole on some
/// screen of each walk, so is every group caption it draws (in the room,
/// not fading in the strip under it), and no screen overflows, clips by
/// accident, paints a mark over another, or cuts a line of words or a
/// pictogram through at either edge of a list
/// ([`crate::native_ui::CompiledUi::sliced_marks`]). With the strip kept
/// for a status bar, Sound's 72pt list at 220x12 let TERMINAL BELL go on
/// the screen of its bell row and drew it only in the strip on the screen
/// before (Window & Tabs' PRESENCE, TAB STATUS and TABS & PANES, Terminal's
/// SYSTEM, Security's NOTIFICATIONS, Harness's and Ai's first cards
/// likewise at 1×; Appearance's EFFECTS, Cursor & Motion's CURSOR and
/// Software Update's automatic card at 2×).
/// At 102x12 at 2× text a group's 50pt caption over a 64pt control in a
/// 100pt list left 21 of Cursor Styles' 37 stops (19 of Appearance's 48, 18
/// of Window's 54) cut on every screen; Software Update's 218pt workbench in
/// a 72pt list (220x12, 1×), About's short hero, Tab Color's 120pt wheel,
/// the theme card's Follow system row and Modified's empty-state line under
/// its pinned summary (102x12, 2×) were on no screen whole; Top Settings
/// sliced "COLOR PREVIEW" at the window's bottom and the sidebar a row's
/// icon under its search field (220x12, 2×); Packages' service line ran
/// past its column (60x20, 2×).
#[test]
fn every_stop_of_a_short_window_is_whole_by_page_and_by_wheel() {
    at_each_text_scale(&[1.0, 2.0], "short windows", |scale| {
        let report = std::sync::Mutex::new(Vec::new());
        let faults = std::sync::atomic::AtomicUsize::new(0);
        // Each short window of each page in each theme, spread over a few
        // threads at the rung's text size.
        let windows = ["", "theme = \"GitHub Light\"\n"]
            .into_iter()
            .flat_map(|source| {
                SettingsRoute::ALL
                    .into_iter()
                    .filter(|route| *route != SettingsRoute::Messages)
                    .flat_map(move |route| {
                        SHORT_SIZES.map(|(width, height)| (source, route, width, height))
                    })
            })
            .collect::<Vec<_>>();
        on_workers(
            &windows,
            "short windows",
            |&(source, route, width, height)| {
                let context = format!(
                    "{route:?} {width}x{height} {scale}x{}",
                    if source.is_empty() { "" } else { " light" }
                );
                let cx = view_cx_at(width, height);
                // Tab Control with tabs open, as the narrow sweep's fleet
                // has them: its rows are listed a line at a time.
                let (mut runtime, instance, view) = if route == SettingsRoute::Tabs {
                    let (mut runtime, instance, view) =
                        super::narrow::every_page_runtime_over(source);
                    runtime
                        .dispatch(
                            instance,
                            view,
                            AppEvent::Action(ActionInvocation {
                                id: route_action(route),
                                value: None,
                            }),
                        )
                        .unwrap();
                    (runtime, instance, view)
                } else {
                    paging_view(source, route)
                };
                let _ = compile_settings_view(&runtime, instance, view, &cx);
                let Some(plan) = messages_view_state(&runtime, view).scroll_plan() else {
                    return;
                };
                let n = plan.len();
                let name = |stop: usize| {
                    plan.identities(stop)
                        .next()
                        .map_or_else(|| format!("#{stop}"), |key| key.as_str().to_string())
                };
                // From the list's top, as a person scrolls down it: from
                // where the last screen put the list (a landing on the
                // page's current item), as every event takes it.
                let to_top = |runtime: &mut NativeRuntime| {
                    let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
                        unreachable!();
                    };
                    state.scroll_page_to(0);
                    state.page_reveal = None;
                };
                to_top(&mut runtime);
                let wheel =
                    walk_screens(&mut runtime, instance, view, &cx, &AppEvent::ScrollLines(1));
                to_top(&mut runtime);
                let page =
                    walk_screens(&mut runtime, instance, view, &cx, &AppEvent::ScrollPage(1));
                let never = |walk: &Walk| {
                    (0..n)
                        .filter(|stop| plan.paints(*stop) && !walk.whole.contains(stop))
                        .map(name)
                        .collect::<Vec<_>>()
                };
                let (by_wheel, by_page) = (never(&wheel), never(&page));
                // A group's caption a walk draws, on no screen whole.
                let unread = |walk: &Walk| {
                    walk.captions
                        .iter()
                        .filter(|(_, whole)| !**whole)
                        .map(|(caption, _)| caption.clone())
                        .collect::<Vec<_>>()
                };
                let mut captions = unread(&wheel);
                captions.extend(unread(&page));
                captions.sort();
                captions.dedup();
                let mut audit = wheel.faults;
                audit.extend(page.faults);
                audit.sort();
                audit.dedup();
                if !by_wheel.is_empty()
                    || !by_page.is_empty()
                    || !captions.is_empty()
                    || !audit.is_empty()
                {
                    faults.fetch_add(
                        by_wheel.len() + by_page.len() + captions.len() + audit.len(),
                        std::sync::atomic::Ordering::Relaxed,
                    );
                    report
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push(format!(
                            "{context}: {n} stops; never whole by wheel {}/{n} {by_wheel:?}; \
                             by Page Down {}/{n} {by_page:?}; captions never whole {captions:?}; \
                             audit {} {:#?}",
                            by_wheel.len(),
                            by_page.len(),
                            audit.len(),
                            audit.iter().take(6).collect::<Vec<_>>()
                        ));
                }
            },
        );
        let faults = faults.into_inner();
        let report = report
            .into_inner()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(
            faults == 0,
            "{faults} faults at {scale}x:\n{}",
            report.join("\n")
        );
    });
}

/// A PAGE TURN TAKES THE KEYBOARD ONTO ITS SCREEN (#84): on every route but
/// Messages and Themes (which move the keyboard through their own rows), at
/// the short windows and 22x24, 80x24, 102x29 and 150x48, 1× and 2× text,
/// with no setting changed and with several (rows that carry a Reset), the
/// keyboard on the first screen's first control, each Page Down to the
/// list's end and each Page Up back leaves the keyboard on a control of a
/// stop the new screen shows as a landing on it reveals it (its controls
/// and words whole, its padding cut or not) — where the turn moved it, the
/// screen's first such control, by Page Up as by Page Down (Page Up took it
/// to the screen's last, so the next Down arrow scrolled the list back to
/// the screen the turn had left): at 80x24 at 2× text
/// Page Down to Top Settings' Sparkle words passed over Profanity's switch,
/// whose stop the screen's edge cut only in its padding, to Feline's — or,
/// where none of those holds a
/// control, on a control the screen draws live, cut by its edge, the list
/// where the turn put it — or, on a screen of words that draws no control
/// at all, where it was. From Appearance's Minimum contrast slider at 80x24
/// at 2× text the first Page Down showed the slider's note and the next
/// row's slider 4pt cut, and left the keyboard on the slider off screen.
/// At 22x24 at 2× text, Page Up onto a screen that draws a stacked row's
/// name line and Reset over its edge held the keyboard on the row's last
/// control, its switch under the edge, which the screen did not draw
/// (Modified's Font size, Lines and Trail rows; Window & Tabs' Lines).
/// CONTROL: some turn holds the keyboard on a cut control.
#[test]
fn a_page_turn_takes_the_keyboard_onto_its_screen() {
    at_each_text_scale(&[1.0, 2.0], "page focus", |scale| {
        let faults = std::sync::Mutex::new(Vec::new());
        let (turns, held) = (
            std::sync::atomic::AtomicUsize::new(0),
            std::sync::atomic::AtomicUsize::new(0),
        );
        // Each window of each page, as published and changed, spread over a
        // few threads at the rung's text size.
        let windows = SettingsRoute::ALL
            .into_iter()
            .filter(|route| !matches!(route, SettingsRoute::Messages | SettingsRoute::Themes))
            .flat_map(|route| [(route, ""), (route, CHANGED)])
            .flat_map(|(route, source)| {
                SHORT_SIZES
                    .into_iter()
                    .chain(PAGING_SIZES)
                    .chain([(178.0, 348.0)])
                    .map(move |(width, height)| (route, source, width, height))
            })
            .collect::<Vec<_>>();
        on_workers(&windows, "page focus", |&(route, source, width, height)| {
            let context = format!(
                "{route:?} {width}x{height} {scale}x{}",
                if source.is_empty() { "" } else { " changed" }
            );
            let cx = view_cx_at(width, height);
            let (mut runtime, instance, view) = paging_view(source, route);
            let _ = compile_settings_view(&runtime, instance, view, &cx);
            let Some(plan) = messages_view_state(&runtime, view).scroll_plan() else {
                return;
            };
            if plan.limit() == 0 {
                return;
            }
            let Some(first) = plan
                .controls()
                .find(|(stop, _)| plan.shown(0).contains(stop))
                .map(|(_, key)| key.clone())
            else {
                return;
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
                let mut before = None;
                let mut moved_from = messages_view_state(&runtime, view)
                    .common
                    .last_focus
                    .clone();
                for _ in 0..4 * (plan.limit() + 2) {
                    runtime
                        .dispatch(instance, view, AppEvent::ScrollPage(direction))
                        .unwrap();
                    let compiled = compile_settings_view(&runtime, instance, view, &cx);
                    let state = messages_view_state(&runtime, view);
                    let plan = state.scroll_plan().expect("a list");
                    let top = state.page_scroll.min(plan.limit());
                    let place = (top, (plan.screen(top).offset * 100.0).round() as i64);
                    if before == Some(place) {
                        break;
                    }
                    before = Some(place);
                    turns.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let shown = plan.shown(top);
                    let focus = state.common.last_focus.clone();
                    let stop = focus.as_ref().and_then(|key| plan.stop_of(key));
                    let whole_control = plan
                        .controls()
                        .any(|(stop, _)| plan.shows_landing(stop, top));
                    let live_control = plan
                        .controls()
                        .any(|(stop, _)| plan.draws_controls_live(stop, top));
                    let live = focus
                        .as_ref()
                        .is_some_and(|key| compiled.semantic(key).is_some());
                    // Where the keyboard moved, onto the screen's first
                    // control shown whole with its words, by either key,
                    // a stop its edge cuts only in its padding among
                    // them.
                    let landing = plan
                        .controls()
                        .find(|(stop, _)| plan.shows_landing(*stop, top))
                        .map(|(_, key)| key.clone());
                    if focus != moved_from
                        && stop.is_some_and(|stop| plan.shows_landing(stop, top))
                        && focus != landing
                    {
                        faults
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .push(format!(
                                "{context}: Page {} to {top} put the keyboard on {:?}, not the \
                                 screen's first control shown whole, {:?}",
                                if direction > 0 { "Down" } else { "Up" },
                                focus.as_ref().map(UiKey::as_str),
                                landing.as_ref().map(UiKey::as_str)
                            ));
                    }
                    moved_from.clone_from(&focus);
                    match stop {
                        Some(stop) if plan.shows_landing(stop, top) => {}
                        Some(stop)
                            if !whole_control && plan.draws_controls_live(stop, top) && live =>
                        {
                            held.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        }
                        _ if !whole_control && !live_control => {}
                        _ => faults
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .push(format!(
                                "{context}: Page {} to {top} shows {shown:?}; the keyboard on \
                                 {:?} (stop {stop:?}, live {live})",
                                if direction > 0 { "Down" } else { "Up" },
                                focus.map(|key| key.as_str().to_string())
                            )),
                    }
                }
            }
        });
        let (turns, held) = (turns.into_inner(), held.into_inner());
        let faults = faults
            .into_inner()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(
            turns > 0 && held > 0,
            "{scale}x: {turns} turns, {held} held"
        );
        assert!(
            faults.is_empty(),
            "{} faults:\n{}",
            faults.len(),
            faults.join("\n")
        );
    });
}

/// A STOP TALLER THAN ANY ROOM THE WINDOW GIVES IS LISTED IN PIECES THE ROOM
/// HOLDS (#97): what a short window's every screen cut is whole on some
/// screen Page Down shows — Software Update's automatic-update switch, in
/// the 218pt workbench beside the status card (the pair stacks where it is
/// taller than the 72pt and 92pt lists of 220x12 and 220x13 at 1× text);
/// About's byline, beside the wordmark in a 126pt short hero (the full
/// hero is listed a row at a time where that row is taller than the 100pt
/// list of 102x12 at 2×); Tab Color's wheel (its floor no taller than the
/// list); the theme card's Follow system switch (its 112pt row cut into
/// pieces); and Modified's empty-state line (the summary pinned over it
/// listed first where pinning left the list 32pt); and the list's last
/// stop, measured as the last screen draws it — with no strip under it —
/// Manual's row for a key only aterm.toml holds (74pt in the 68pt list of
/// 220x12 at 2× text, its top cut on the only screen that drew it in the
/// room). Fails without each.
#[test]
fn a_stop_taller_than_any_room_is_whole_on_some_screen() {
    let cases: [(f32, SettingsRoute, f32, f32, &str); 7] = [
        (
            1.0,
            SettingsRoute::SoftwareUpdate,
            1_564.0,
            180.0,
            "settings/control/update.auto_apply",
        ),
        (
            1.0,
            SettingsRoute::SoftwareUpdate,
            1_564.0,
            194.0,
            "settings/control/update.auto_apply",
        ),
        (2.0, SettingsRoute::About, 738.0, 180.0, "about/byline"),
        (
            2.0,
            SettingsRoute::TabColor,
            738.0,
            180.0,
            "settings/color/active_tab_color/wheel",
        ),
        (
            2.0,
            SettingsRoute::Appearance,
            738.0,
            180.0,
            "settings/theme-mode",
        ),
        (2.0, SettingsRoute::Modified, 738.0, 180.0, "settings/empty"),
        (
            2.0,
            SettingsRoute::Manual,
            1_564.0,
            180.0,
            "settings/modified/manual/label/agents-auto-prime",
        ),
    ];
    at_each_text_scale(&[1.0, 2.0], "tall stops", |scale| {
        for (at, route, width, height, key) in cases {
            if at != scale {
                continue;
            }
            let context = format!("{route:?} {width}x{height} {scale}x: {key}");
            let cx = view_cx_at(width, height);
            // Manual lists a key only aterm.toml holds where the file has one.
            let source = if route == SettingsRoute::Manual {
                CHANGED
            } else {
                ""
            };
            let (mut runtime, instance, view) = paging_view(source, route);
            let mut whole = false;
            let mut before = None;
            for _ in 0..200 {
                let compiled = compile_settings_view(&runtime, instance, view, &cx);
                whole |= compiled.paint.iter().any(|node| {
                    node.key.as_str() == key
                        && node.fade.is_none()
                        && (node.clip.y - node.rect.y).abs() < 0.5
                        && (node.clip.height - node.rect.height).abs() < 0.5
                });
                let state = messages_view_state(&runtime, view);
                let place = state.scroll_plan().map(|plan| {
                    let top = state.page_scroll.min(plan.limit());
                    (top, (plan.screen(top).offset * 100.0).round() as i64)
                });
                if whole || place == before {
                    break;
                }
                before = place;
                runtime
                    .dispatch(instance, view, AppEvent::ScrollPage(1))
                    .unwrap();
            }
            assert!(whole, "{context} is on no screen whole");
        }
    });
}

/// A CONTROL IS NEVER LISTED WITHOUT THE WORDS THAT NAME IT where the two
/// fit the list's room: on Appearance at 80x16 and 102x16 with 2× text,
/// every screen the wheel shows that draws a switch or a slider whole draws
/// its name too. A row cut a part at a time to fit under its card's caption
/// left Minimum contrast's slider, and every row's control of the page, on
/// a screen with no name over it; the name and the control now share a
/// stop, the screen that starts with it letting the caption go. And at
/// 102x12 the list (100pt) is shorter than any stacked row (a 40pt name
/// over a 64pt control): there a row goes on one line where its name keeps
/// its measure beside the control, on every page — every switch and slider
/// of Appearance, Text & Fonts, Sound, Window & Tabs, Security, Harness and
/// Ai was on a screen without its name.
#[test]
fn a_control_is_listed_with_the_words_that_name_it() {
    let cases = [
        ((584.0, 236.0), SettingsRoute::Appearance),
        ((738.0, 236.0), SettingsRoute::Appearance),
        ((738.0, 180.0), SettingsRoute::Appearance),
        ((738.0, 180.0), SettingsRoute::TextFonts),
        ((738.0, 180.0), SettingsRoute::Sound),
        ((738.0, 180.0), SettingsRoute::WindowTabs),
        ((738.0, 180.0), SettingsRoute::Security),
        ((738.0, 180.0), SettingsRoute::Harness),
        ((738.0, 180.0), SettingsRoute::Ai),
    ];
    at_each_text_scale(&[2.0], "named controls", |_| {
        let mut faults = Vec::new();
        let mut seen = 0usize;
        for ((width, height), route) in cases {
            let cx = view_cx_at(width, height);
            let (mut runtime, instance, view) = paging_view("", route);
            let mut before = None;
            for _ in 0..200 {
                let compiled = compile_settings_view(&runtime, instance, view, &cx);
                for node in &compiled.paint {
                    let Some(key) = node.key.as_str().strip_prefix("settings/control/") else {
                        continue;
                    };
                    let whole = matches!(node.content, UiContent::Switch(_) | UiContent::Slider(_))
                        && node.fade.is_none()
                        && (node.clip.y - node.rect.y).abs() < 0.5
                        && (node.clip.height - node.rect.height).abs() < 0.5;
                    if !whole {
                        continue;
                    }
                    seen += 1;
                    let label = format!("settings/label/{key}");
                    if !compiled
                        .paint
                        .iter()
                        .any(|named| named.key.as_str() == label && named.clip.height > 1.0)
                    {
                        faults.push(format!(
                            "{route:?} {width}x{height}: {key} drawn with no name"
                        ));
                    }
                }
                let state = messages_view_state(&runtime, view);
                let place = state
                    .scroll_plan()
                    .map(|plan| plan.limit().min(state.page_scroll));
                if place == before {
                    break;
                }
                before = place;
                runtime
                    .dispatch(instance, view, AppEvent::ScrollLines(1))
                    .unwrap();
            }
        }
        faults.sort();
        faults.dedup();
        assert!(seen > 0, "controls are drawn");
        assert!(faults.is_empty(), "{faults:#?}");
    });
}

/// MESSAGES' LIST HOLDS AN ENTRY WHOLE IN A SHORT WINDOW (the short-window
/// sweep's Messages): at the short windows and 80x24, 1× and 2× text, every
/// screen of the Messages page the wheel or Page Down shows audits clean,
/// and every entry the list holds is whole on some screen of each walk.
/// At 220x12 at 1× its filters and count line left the list 12pt under
/// the page's full insets, its 30pt rows clipped on every screen (overflow
/// 6, clipped 11), because the page took its full chrome wherever its
/// first screen listed an entry, whole or not. Where the room under the
/// pinned header is shorter than an entry, the header now scrolls away
/// with the list, as a compact page's does, and the first screen, holding
/// no entry then, sheds the page's insets where that brings one under the
/// header (220x12 at 1×).
#[test]
fn messages_list_holds_an_entry_whole_in_a_short_window() {
    at_each_text_scale(&[1.0, 2.0], "short messages", |scale| {
        let mut faults = Vec::new();
        if scale < 1.5 {
            // The first screen of 220x12 holds the newest entry whole.
            let cx = view_cx_at(1_564.0, 180.0);
            let (runtime, instance, view) = setup_with_messages();
            let compiled = compile_settings_view(&runtime, instance, view, &cx);
            let first_whole = compiled.paint.iter().any(|node| {
                node.key
                    .as_str()
                    .strip_prefix("settings/messages/row/")
                    .is_some_and(|rest| !rest.contains('/'))
                    && node.clip == node.rect
            });
            if !first_whole {
                faults.push("220x12 1x: the first screen holds no entry whole".to_string());
            }
        }
        for (width, height) in SHORT_SIZES {
            let context = format!("{width}x{height} {scale}x");
            let cx = view_cx_at(width, height);
            for event in [AppEvent::ScrollLines(1), AppEvent::ScrollPage(1)] {
                let (mut runtime, instance, view) = setup_with_messages();
                let mut whole = BTreeSet::new();
                let mut listed = BTreeSet::new();
                let mut before = None;
                for _ in 0..200 {
                    let compiled = compile_settings_view(&runtime, instance, view, &cx);
                    for fault in compiled.fit_failures() {
                        faults.push(format!("{context} {event:?}: {fault}"));
                    }
                    for node in &compiled.paint {
                        let Some(row) = node
                            .key
                            .as_str()
                            .strip_prefix("settings/messages/row/")
                            .filter(|rest| !rest.contains('/'))
                        else {
                            continue;
                        };
                        listed.insert(row.to_string());
                        if (node.clip.y - node.rect.y).abs() < 0.5
                            && (node.clip.height - node.rect.height).abs() < 0.5
                        {
                            whole.insert(row.to_string());
                        }
                    }
                    let state = messages_view_state(&runtime, view);
                    let place = (state.page_scroll, state.messages_drawn.get());
                    if before == Some(place) {
                        break;
                    }
                    before = Some(place);
                    runtime.dispatch(instance, view, event.clone()).unwrap();
                }
                let never: Vec<_> = listed.difference(&whole).collect();
                if listed.is_empty() || !never.is_empty() {
                    faults.push(format!(
                        "{context} {event:?}: {} entries drawn, never whole: {never:?}",
                        listed.len()
                    ));
                }
            }
        }
        faults.sort();
        faults.dedup();
        assert!(
            faults.is_empty(),
            "{} faults:\n{}",
            faults.len(),
            faults.join("\n")
        );
    });
}

/// MANUAL'S EXPLANATION IS SET ON LINES OF ITS OWN HEIGHT: where a compact
/// window shows no subtitle, the aterm.toml card opens with what Manual is,
/// wrapped on lines as tall as the card's lines — not as tall as the whole
/// path under it, which it took for its line height (f222a198c4b): once the
/// path wrapped (a folder outside the home folder, at 2× text in a 102x12
/// window) each line of the explanation was 144pt, taller than the 100pt
/// list, and on no screen whole.
#[test]
fn manuals_explanation_is_set_on_lines_of_its_own_height() {
    at_each_text_scale(&[1.0, 2.0], "manual explanation", |scale| {
        let line = 22.0_f32.max(18.0 * scale);
        for (width, height) in [(738.0, 180.0), (444.0, 292.0), (584.0, 348.0)] {
            let cx = view_cx_at(width, height);
            let (mut runtime, instance, view) = paging_view("", SettingsRoute::Manual);
            {
                let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
                    unreachable!();
                };
                state.arm_config_file(
                    Some(std::path::PathBuf::from(
                        "/Volumes/Shared Drive/Projects/someone/dotfiles/config/aterm/aterm.toml",
                    )),
                    Some(std::path::Path::new("/Users//someone")),
                    ConfigFileArm::inert(),
                );
            }
            let compiled = compile_settings_view(&runtime, instance, view, &cx);
            let path_lines = compiled
                .paint
                .iter()
                .filter(|node| {
                    node.key
                        .as_str()
                        .starts_with("settings/manual/path/visual/")
                })
                .count();
            let lines: Vec<_> = compiled
                .paint
                .iter()
                .filter(|node| {
                    node.key.as_str().starts_with("settings/manual/explanation")
                        && matches!(node.content, UiContent::Text(_))
                })
                .map(|node| (node.key.as_str().to_string(), node.rect.height))
                .collect();
            assert!(
                lines.iter().all(|(_, tall)| *tall <= line + 0.5),
                "{width}x{height} {scale}x: the path on {path_lines} lines, the explanation \
                 {lines:?} (lines {line}pt)"
            );
        }
    });
}

/// END REACHES THE LIST'S END BEFORE ANY RENDER HAS MEASURED IT: a
/// navigation, an aterm.toml change and a focus move each take away the
/// bound the last render measured, and an End that comes before the next
/// render — a headless instance driven over ctl, a real window's key before
/// its frame — lands that render on the list's last screen, as an End after
/// a render does. The reducer guessed the bound meanwhile (the page's field
/// count, or none for a page of cards), lower than the real one wherever a
/// list holds stops that are not fields: the first End stopped at Background
/// material on Appearance, moved nothing on Top Settings, and stopped short
/// on Text & Fonts, Harness, Packages and Software Update.
#[test]
fn end_reaches_the_lists_end_before_a_render_has_measured_it() {
    let mut ended = 0usize;
    let mut faults = Vec::new();
    for route in SettingsRoute::ALL {
        if route == SettingsRoute::Messages {
            continue;
        }
        for (width, height) in [(584.0, 348.0), (738.0, 418.0)] {
            let cx = view_cx_at(width, height);
            for trigger in ["navigate", "aterm.toml", "focus"] {
                let from = if route == SettingsRoute::About {
                    SettingsRoute::Terminal
                } else {
                    SettingsRoute::About
                };
                let (mut runtime, instance, view) =
                    paging_view(CHANGED, if trigger == "navigate" { from } else { route });
                let _ = compile_settings_view(&runtime, instance, view, &cx);
                match trigger {
                    "navigate" => {
                        let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view)
                        else {
                            unreachable!();
                        };
                        state.navigate(route);
                    }
                    "aterm.toml" => {
                        let mut service = VersionedConfigService::new(CHANGED.to_string())
                            .expect("valid fixture");
                        let snapshot = service
                            .replace_external(
                                CHANGED.replace("window_padding = 20.0", "window_padding = 22.0"),
                            )
                            .expect("valid edit");
                        runtime
                            .dispatch(instance, view, AppEvent::ConfigChanged(snapshot))
                            .unwrap();
                    }
                    _ => {
                        runtime
                            .dispatch(instance, view, AppEvent::FocusChanged(None))
                            .unwrap();
                    }
                }
                // End, with no render since the trigger.
                runtime
                    .dispatch(
                        instance,
                        view,
                        AppEvent::TextInput(TextInputEvent::End { extend: false }),
                    )
                    .unwrap();
                let _ = compile_settings_view(&runtime, instance, view, &cx);
                let state = messages_view_state(&runtime, view);
                let Some(plan) = state.scroll_plan() else {
                    continue;
                };
                if plan.limit() == 0 {
                    continue;
                }
                ended += 1;
                let top = state.page_scroll.min(plan.limit());
                if top != plan.limit() {
                    faults.push(format!(
                        "{route:?} {width}x{height} after {trigger}: End drew screen {top}, \
                         the list's last is {}",
                        plan.limit()
                    ));
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
    assert!(ended > 0, "the sweep ends scrolling lists");
}

/// NO PAGE DOWN ONLY MOVES THE LIST: on every route at 80x24, 102x29 and
/// 150x48 — Cursor Styles with Beam saved too — each Page Down from the
/// list's top to its end, the turn onto the last screen included, shows a
/// stop the screen before did not show whole, or draws the list's end the
/// screen before did not. The bound stood a stop past the screen that drew
/// the last stop whole, its note in the strip under the room, and the last
/// turn moved the Adjust card 4pt and drew nothing new (Cursor Styles at
/// 150x48 with Beam saved).
#[test]
fn no_page_down_only_moves_the_list() {
    let mut turned = 0usize;
    let mut faults = Vec::new();
    for source in ["", "cursor_trail_style = \"beam\"\n", CHANGED] {
        for route in SettingsRoute::ALL {
            if route == SettingsRoute::Messages {
                continue;
            }
            for (width, height) in PAGING_SIZES {
                let cx = view_cx_at(width, height);
                let (mut runtime, instance, view) = paging_view(source, route);
                let Some((mut before, plan)) = screen_now(&runtime, instance, view, &cx) else {
                    continue;
                };
                if plan.limit() == 0 {
                    continue;
                }
                // Where the list's end is drawn: its last stop's band's
                // bottom on the painted list, or nowhere.
                let end_drawn = |runtime: &NativeRuntime| {
                    let compiled = compile_settings_view(runtime, instance, view, &cx);
                    let state = messages_view_state(runtime, view);
                    let plan = state.scroll_plan()?;
                    let top = state.page_scroll.min(plan.limit());
                    let list = painted_list(&compiled, &plan, top)?;
                    let band = plan.drawn_bands(top).pop()?;
                    (band.stop + 1 == plan.len() && band.at + band.height <= list.height + 0.5)
                        .then_some(())
                };
                let mut end_before = end_drawn(&runtime);
                for _ in 0..4 * (plan.limit() + 2) {
                    runtime
                        .dispatch(instance, view, AppEvent::ScrollPage(1))
                        .unwrap();
                    let (now, _) = screen_now(&runtime, instance, view, &cx).expect("a list");
                    if now.place() == before.place() {
                        break;
                    }
                    turned += 1;
                    let end_now = end_drawn(&runtime);
                    let new = now.whole.difference(&before.whole).count() > 0;
                    if !new && !(end_now.is_some() && end_before.is_none()) {
                        faults.push(format!(
                            "{route:?} {width}x{height}{}: Page Down from {} to {} shows {:?} \
                             after {:?}, nothing new",
                            if source.is_empty() { "" } else { " (changed)" },
                            before.top,
                            now.top,
                            now.whole,
                            before.whole
                        ));
                    }
                    before = now;
                    end_before = end_now;
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

/// A NOTE IS NEVER LEFT ALONE AT THE TOP BY THE WHEEL, AND NO SCREEN DRAWS A
/// CAPTION TWICE: on Settings ▸ Harness (supervision off, as the lab's), AI
/// CLIs and Tab Control at 80x24, 102x29 and 150x48, a wheel turned a notch
/// at a time from the list's top never seats a screen at a note on the rows
/// over it that its row shows with it — the wheel at Harness 102x29 left the
/// Gemini row's shared note alone under SUPERVISION · CONTINUED, its rows
/// scrolled away — and no screen paints two captions that read alike: the
/// pages cut a tall group into continued cards, each with a heading of its
/// own, under the one the list keeps (two SUPERVISION · CONTINUED headings on
/// one screen).
#[test]
fn the_wheel_leaves_no_note_alone_and_no_screen_draws_a_caption_twice() {
    let mut notches = 0usize;
    let mut passed_over = 0usize;
    let mut faults = Vec::new();
    for source in ["[harness]\nenabled = false\n", ""] {
        for route in [
            SettingsRoute::Harness,
            SettingsRoute::Ai,
            SettingsRoute::Tabs,
        ] {
            for (width, height) in PAGING_SIZES {
                let cx = view_cx_at(width, height);
                let (mut runtime, instance, view) = paging_view(source, route);
                let _ = compile_settings_view(&runtime, instance, view, &cx);
                let Some(plan) = messages_view_state(&runtime, view).scroll_plan() else {
                    continue;
                };
                passed_over += (0..plan.len())
                    .filter(|stop| plan.passes_over(*stop))
                    .count();
                for _ in 0..4 * (plan.len() + 2) {
                    let compiled = compile_settings_view(&runtime, instance, view, &cx);
                    let state = messages_view_state(&runtime, view);
                    let plan = state.scroll_plan().expect("a list");
                    let top = state.page_scroll.min(plan.limit());
                    let context = format!("{route:?} {width}x{height} screen {top}");
                    if plan.passes_over(top) {
                        faults.push(format!("{context}: seated at a note its row shows"));
                    }
                    let mut captions: Vec<String> = compiled
                        .paint
                        .iter()
                        .filter(|node| {
                            let key = node.key.as_str();
                            (key.ends_with("/heading") || key.contains("/heading/"))
                                && !node.clip.is_empty()
                        })
                        .filter_map(|node| match &node.content {
                            UiContent::Text(spec) => Some(spec.text.clone()),
                            _ => None,
                        })
                        .collect();
                    captions.sort();
                    if captions.windows(2).any(|pair| pair[0] == pair[1]) {
                        faults.push(format!("{context}: captions drawn twice: {captions:?}"));
                    }
                    if top >= plan.limit() {
                        break;
                    }
                    notches += 1;
                    runtime
                        .dispatch(instance, view, AppEvent::ScrollLines(1))
                        .unwrap();
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
    assert!(notches > 0, "the wheel turns");
    assert!(
        passed_over > 0,
        "some page holds a note the wheel passes over"
    );
}
