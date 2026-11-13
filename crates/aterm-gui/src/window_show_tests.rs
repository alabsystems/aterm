// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn show(z_order: u32, key: bool) -> WindowShow {
    WindowShow {
        fullscreen: Some(false),
        minimized: Some(false),
        z_order: Some(z_order),
        key: Some(key),
    }
}

/// The MRU is the stack: the most recently focused window is rank 0, and
/// a window closed since it was focused is not ranked at all.
#[test]
fn stacking_ranks_follow_the_focus_mru() {
    let ranks = stacking_ranks(&[1_u64, 2, 3], &[9, 1, 3, 2]);
    assert_eq!(ranks, vec![2, 0, 1]);
}

/// A window that never gained focus sits in front of every window that did
/// (its reveal ordered it front and nothing has reordered since), newest
/// first — the successor that restored while the user was elsewhere.
#[test]
fn never_focused_windows_rank_in_front_newest_first() {
    let ranks = stacking_ranks(&[1_u64, 2, 3, 4], &[2, 1]);
    assert_eq!(ranks, vec![2, 3, 1, 0]);
    assert_eq!(stacking_ranks(&[5_u64, 7], &[]), vec![1, 0]);
}

#[test]
fn capture_maps_every_probe_bit() {
    let probe = ShowProbe {
        fullscreen: true,
        minimized: Some(false),
    };
    assert_eq!(
        captured_show(Some(probe), 2, false, false),
        WindowShow {
            fullscreen: Some(true),
            minimized: Some(false),
            z_order: Some(2),
            key: Some(false),
        }
    );
    let minimized = ShowProbe {
        fullscreen: false,
        minimized: Some(true),
    };
    assert_eq!(
        captured_show(Some(minimized), 0, true, false),
        WindowShow {
            fullscreen: Some(false),
            minimized: Some(true),
            z_order: Some(0),
            key: Some(true),
        }
    );
}

/// A carried full screen this process has not re-entered yet (aterm has
/// not been in front since the update) is still captured AS full screen,
/// so a second update does not forget it.
#[test]
fn a_pending_full_screen_is_captured_as_full_screen() {
    let probe = ShowProbe {
        fullscreen: false,
        minimized: Some(false),
    };
    assert_eq!(
        captured_show(Some(probe), 0, true, true).fullscreen,
        Some(true)
    );
    let minimized = ShowProbe {
        fullscreen: false,
        minimized: Some(true),
    };
    assert_eq!(
        captured_show(Some(minimized), 0, true, true).fullscreen,
        Some(false)
    );
}

/// A manifest that carries no show state plans nothing, on either lane.
#[test]
fn a_manifest_without_show_state_plans_nothing() {
    let windows = [(1_u64, WindowShow::UNKNOWN), (2, WindowShow::UNKNOWN)];
    for lane in [ShowLane::Handoff, ShowLane::Cold] {
        let plan = plan(&windows, lane);
        assert!(plan.is_empty(), "{lane:?}: {plan:?}");
        assert!(plan.stacking_ops().is_empty());
        assert!(plan.fullscreen_ops().is_empty());
    }
}

/// Three windows created in manifest order 1, 2, 3 (so the reveals left 3
/// key and in front), while the carried stack was 2 in front and key, then
/// 1, then 3: the plan raises back to front with the key window last.
#[test]
fn the_stack_is_raised_back_to_front_with_the_key_window_last() {
    let windows = [
        (1_u64, show(1, false)),
        (2, show(0, true)),
        (3, show(2, false)),
    ];
    let plan = plan(&windows, ShowLane::Handoff);
    assert_eq!(plan.key, Some(2));
    assert_eq!(plan.raise, vec![3, 1, 2]);
    assert_eq!(
        plan.stacking_ops(),
        vec![ShowOp::Raise(3), ShowOp::Raise(1), ShowOp::Raise(2)]
    );
    assert_eq!(plan.focus_seed, vec![3, 1, 2]);
    assert!(plan.fullscreen_ops().is_empty());
}

/// The key flag outranks the stack when the two disagree (a window the app
/// keyed without a focus event): the key window is raised last either way.
#[test]
fn the_key_flag_outranks_the_stack() {
    let windows = [(1_u64, show(0, false)), (2, show(1, true))];
    let plan = plan(&windows, ShowLane::Handoff);
    assert_eq!(plan.key, Some(2));
    assert_eq!(plan.raise, vec![1, 2]);
}

/// One window: its reveal already made it key and front, so there is
/// nothing to raise — a single-window update performs no extra AppKit call
/// unless it has to minimize or re-enter full screen, and records no carry.
#[test]
fn a_single_window_raises_nothing() {
    let plan = plan(&[(1_u64, show(0, true))], ShowLane::Handoff);
    assert!(plan.raise.is_empty());
    assert!(plan.stacking_ops().is_empty());
    assert_eq!(plan.key, Some(1));
    assert!(plan.is_empty(), "nothing to do: no carry is recorded");
    let mut fullscreen = show(0, true);
    fullscreen.fullscreen = Some(true);
    let plan = super::plan(&[(1_u64, fullscreen)], ShowLane::Handoff);
    assert!(plan.stacking_ops().is_empty());
    assert_eq!(plan.fullscreen_ops(), vec![ShowOp::EnterFullScreen(1)]);
}

/// Minimized windows are minimized FIRST, never raised, never keyed; the
/// key moves to the frontmost window still on screen.
#[test]
fn minimized_windows_are_minimized_first_and_never_keyed() {
    let mut minimized_key = show(0, true);
    minimized_key.minimized = Some(true);
    let windows = [
        (1_u64, minimized_key),
        (2, show(1, false)),
        (3, show(2, false)),
    ];
    let plan = plan(&windows, ShowLane::Handoff);
    assert_eq!(plan.key, Some(2));
    assert_eq!(
        plan.stacking_ops(),
        vec![ShowOp::Minimize(1), ShowOp::Raise(3), ShowOp::Raise(2)]
    );
    assert_eq!(plan.focus_seed, vec![1, 3, 2]);
}

/// The update lane reproduces the outgoing process exactly, even with
/// every window minimized; a cold launch the user started shows the
/// frontmost one instead.
#[test]
fn a_cold_launch_never_restores_every_window_minimized() {
    let mut back = show(1, false);
    back.minimized = Some(true);
    let mut front = show(0, true);
    front.minimized = Some(true);
    let windows = [(1_u64, back), (2, front)];
    let handoff = plan(&windows, ShowLane::Handoff);
    assert_eq!(handoff.key, None);
    assert_eq!(
        handoff.stacking_ops(),
        vec![ShowOp::Minimize(1), ShowOp::Minimize(2)]
    );
    let cold = plan(&windows, ShowLane::Cold);
    assert_eq!(cold.key, Some(2));
    assert_eq!(
        cold.stacking_ops(),
        vec![ShowOp::Minimize(1), ShowOp::Raise(2)]
    );
}

/// Full screen is re-entered back to front with the key window LAST, so
/// the user ends in the Space they were in; a minimized window is never
/// full-screened (AppKit cannot hold both).
#[test]
fn full_screen_is_re_entered_with_the_key_window_last() {
    let mut key = show(0, true);
    key.fullscreen = Some(true);
    let mut other = show(1, false);
    other.fullscreen = Some(true);
    let mut both = show(2, false);
    both.fullscreen = Some(true);
    both.minimized = Some(true);
    let windows = [(1_u64, key), (2, other), (3, both)];
    let plan = plan(&windows, ShowLane::Handoff);
    assert_eq!(
        plan.fullscreen_ops(),
        vec![ShowOp::EnterFullScreen(2), ShowOp::EnterFullScreen(1)]
    );
    assert_eq!(plan.minimize, vec![3]);
}

/// A hand-edited or partial manifest is data: windows without a rank go
/// behind the ranked ones, ties keep manifest order, and a second `key`
/// loses to the first.
#[test]
fn gaps_ties_and_duplicate_keys_are_tolerated() {
    let unranked = WindowShow {
        z_order: None,
        ..show(0, true)
    };
    let windows = [
        (1_u64, unranked),
        (2, show(7, true)),
        (3, show(7, false)),
        (4, show(3, false)),
    ];
    let plan = plan(&windows, ShowLane::Handoff);
    assert_eq!(plan.key, Some(1));
    assert_eq!(plan.raise, vec![3, 2, 4, 1]);
}

/// Three logical windows: 0 is the bootstrap, 1 and 2 are the ones the rebuild created (2 last, so it is the front the rebuild left).
pub(crate) fn three_windows() -> (crate::App, [WindowId; 3]) {
    let mut app = crate::App::headless_for_test();
    let first = WindowId(0);
    let second = app.insert_logical_window(crate::stub_session(app.next_session_id), 24, 80);
    let third = app.insert_logical_window(crate::stub_session(app.next_session_id), 24, 80);
    (app, [first, second, third])
}

fn carry(app: &mut crate::App, lane: ShowLane, windows: &[(WindowId, WindowShow)]) {
    let bootstrap = windows.first().map(|(window, _)| *window);
    app.carried_window_show = Some(CarriedShow::new(lane, plan(windows, lane), bootstrap));
}

/// Stage 1 on the update lane: nothing moves while a carried window is
/// still waiting for its first present, then the stack goes on back to
/// front with the key window last, and the MRU and the front become that
/// stack — the rebuild had left window 2 in front, the carried key was 1.
#[test]
fn the_stack_waits_for_the_reveal_then_seeds_the_mru_and_the_front() {
    let (mut app, [w0, w1, w2]) = three_windows();
    assert_eq!(
        app.frontmost_window,
        Some(w2),
        "PRECONDITION: the rebuild's front"
    );
    carry(
        &mut app,
        ShowLane::Handoff,
        &[
            (w0, show(1, false)),
            (w1, show(0, true)),
            (w2, show(2, false)),
        ],
    );
    app.windows.get_mut(&w2).expect("w2").pending_reveal = Some(std::time::Instant::now());
    assert!(app.settle_carried_window_show().is_empty());
    assert!(!app.carried_window_show.as_ref().expect("carried").stacked);
    assert_eq!(
        app.frontmost_window,
        Some(w2),
        "nothing moved before the reveal"
    );

    app.windows.get_mut(&w2).expect("w2").pending_reveal = None;
    assert_eq!(
        app.settle_carried_window_show(),
        vec![ShowOp::Raise(w2), ShowOp::Raise(w0), ShowOp::Raise(w1)]
    );
    assert_eq!(app.focus_order, vec![w2, w0, w1], "the MRU is the stack");
    assert_eq!(
        app.frontmost_window,
        Some(w1),
        "the carried key window is in front"
    );
    let carried = app.carried_window_show.as_ref().expect("waits for Commit");
    assert!(carried.stacked && !carried.committed);
    assert!(
        app.settle_carried_window_show().is_empty(),
        "the stack goes on ONCE"
    );
}

/// Window 2 was carried minimized, but every carried window is on glass
/// from its reveal until the stack, and the reveal left window 2 key.
#[test]
fn keys_typed_into_a_window_carried_minimized_keep_it_out_of_the_dock() {
    let windows = |[w0, w1, w2]: [WindowId; 3]| {
        let mut minimized = show(1, false);
        minimized.minimized = Some(true);
        [(w0, show(2, false)), (w1, show(0, true)), (w2, minimized)]
    };
    for typed in [false, true] {
        let (mut app, ids) = three_windows();
        carry(&mut app, ShowLane::Handoff, &windows(ids));
        app.frontmost_window = Some(ids[2]);
        app.incoming_handoff_pending = true;
        if typed {
            let winit_id = aterm_window::window::WindowId::from(2u64);
            app.winit_to_window.insert(winit_id, ids[2]);
            app.queue_pre_commit_input(
                winit_id,
                aterm_window::event::WindowEvent::Ime(aterm_window::event::Ime::Commit(
                    "git pu".into(),
                )),
            );
        }
        let stack = app.settle_carried_window_show_at_proof();
        if typed {
            assert!(
                !stack.contains(&ShowOp::Minimize(ids[2])),
                "the window typed into is not sent to the Dock: {stack:?}"
            );
            assert_eq!(
                stack.last(),
                Some(&ShowOp::Raise(ids[2])),
                "it is raised last, so it keeps the keyboard: {stack:?}"
            );
            assert_eq!(
                app.focus_order.last(),
                Some(&ids[2]),
                "the focus seed puts it most recent"
            );
        } else {
            assert_eq!(
                stack.first(),
                Some(&ShowOp::Minimize(ids[2])),
                "nothing typed: it goes back to the Dock: {stack:?}"
            );
            assert_eq!(stack.last(), Some(&ShowOp::Raise(ids[1])), "{stack:?}");
        }
        let key = if typed { ids[2] } else { ids[1] };
        let typed_into = app.pre_commit_typed_windows();
        let commit = app.commit_carried_window_show(&typed_into);
        assert_eq!(
            commit,
            vec![ShowOp::Raise(key)],
            "typed={typed}: Commit keys it again"
        );
        assert_eq!(
            app.frontmost_window,
            Some(key),
            "typed={typed}: the keyboard is where the person was typing"
        );
    }
}

/// A carried window the user closed before its stage runs is dropped from
/// the plan — never raised, never keyed — and the rest still goes on.
#[test]
fn a_closed_window_is_dropped_from_the_carry() {
    let (mut app, [w0, w1, w2]) = three_windows();
    carry(
        &mut app,
        ShowLane::Cold,
        &[
            (w0, show(1, false)),
            (w1, show(0, true)),
            (w2, show(2, false)),
        ],
    );
    let _ = app.close_window_logical(w1);
    assert_eq!(
        app.settle_carried_window_show(),
        vec![ShowOp::Raise(w2), ShowOp::Raise(w0)]
    );
    assert!(!app.focus_order.contains(&w1));
}

/// A headless instance records no carry, and a headless capture records no
/// show state, so its layout wire omits `[windows.show]`.
#[test]
fn a_headless_app_carries_and_captures_nothing() {
    let (mut app, [w0, w1, _]) = three_windows();
    app.record_carried_window_show(&[(w0, show(0, true)), (w1, show(1, false))], Some(w0));
    assert!(app.carried_window_show.is_none());
    let manifest = app.capture_restore_manifest();
    assert!(manifest.windows.iter().all(|w| w.show.is_unknown()));
    let wire = manifest.to_toml().expect("serialize");
    assert!(!wire.contains("[windows.show]"), "wire: {wire}");
}
