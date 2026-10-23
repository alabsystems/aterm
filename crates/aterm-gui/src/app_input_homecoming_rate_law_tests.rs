// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::{App, WindowId};
use aterm_effects::companion_look::TENURE;
use aterm_effects::kitty_pet::PetArrival;
use aterm_effects::kitty_registry::{KittyLook, SUFFICIENT_DIFFERENCE, coat_distance};
use std::time::{Duration, Instant};

/// Feed the pane the zsh integration command-start byte stream (OSC 633;E and 133;C).
fn run_program(app: &mut App, cmdline: &str) {
    let term = app.pool.get(0).expect("session 0").term.clone();
    let mut t = crate::term_lock(&term);
    t.process(format!("{cmdline}\x1b]633;E;{cmdline}\x07\r\n\x1b]133;C\x07").as_bytes());
}

/// Feed the pane the command-finish and prompt-return byte stream.
fn exit_to_prompt(app: &mut App) {
    let term = app.pool.get(0).expect("session 0").term.clone();
    let mut t = crate::term_lock(&term);
    t.process(b"\x1b]133;D;0\x07\x1b]133;A\x07user@host repo % \x1b]133;B\x07");
}

/// Tick the pet brain until its fade envelope is on glass (`alpha > 0`).
fn tick_pet_on_glass(app: &mut App, wid: WindowId, t: Instant) {
    let ws = app.windows.get_mut(&wid).expect("window");
    for step in 0..100u64 {
        let frame = crate::app_render::pet_tick_for_test(
            ws,
            aterm_effects::host::CaptureMode::Present,
            aterm_effects::kitty_pet::PetSense {
                caret_drawn: true,
                now: t + Duration::from_millis(100 * step),
                caret: Some((5, 10)),
                rows: 24,
                cols: 80,
                cell_w: 10,
                cell_h: 20,
                reduced_motion: false,
                output_burst: false,
                pointer: None,
                wrapped: false,
            },
        );
        if frame.alpha > 0 {
            return;
        }
    }
    panic!("fixture: the pet never reached the glass in 10 s of fed frames");
}

/// Feed the prompt's byte stream (OSC 133;A/B).
fn at_prompt(app: &mut App) {
    let term = app.pool.get(0).expect("session 0").term.clone();
    let mut t = crate::term_lock(&term);
    t.process(b"\x1b]133;A\x07user@host repo % \x1b]133;B\x07");
}

/// Emit the resident pet with `present = true` and return its worn pair.
fn perform_on_glass(app: &mut App, wid: WindowId, look: KittyLook, t: Instant) -> (u8, u8) {
    let ws = app.windows.get_mut(&wid).expect("window");
    let capture = if ws.fx.companion_mut().alpha() > 0 {
        aterm_effects::host::CaptureMode::Present
    } else {
        aterm_effects::host::CaptureMode::StaticCapture
    };
    let pet = crate::app_render::pet_owner_tick_for_test(
        ws,
        capture,
        aterm_effects::kitty_pet::PetSense {
            caret_drawn: true,
            now: t,
            caret: Some((5, 10)),
            rows: 24,
            cols: 80,
            cell_w: 10,
            cell_h: 20,
            reduced_motion: false,
            output_burst: false,
            pointer: None,
            wrapped: false,
        },
    );
    assert!(pet.on_glass, "fixture: the performance is a drawn frame");
    let _ = crate::app_render::emit_resident_pet(
        ws,
        &pet,
        look.normalized(),
        crate::app_render::CompanionInk::Single,
        true,
        t,
        aterm_effects::companion::ContrastFallback {
            bg: 0x0010_1010,
            cursor: 0x00FF_FFFF,
            accent: 0x0050_FA7B,
        },
    );
    ws.fx
        .companion_mut()
        .brain()
        .worn_pair()
        .expect("the performance dresses the pet")
}

/// Strangers commit their hellos at the performance, while a return inside `HOMECOMING` is `Quiet`.
#[test]
fn a_return_to_a_recent_program_is_quiet() {
    let t0 = Instant::now();
    let mut app = App::headless_for_test();
    app.recompute_sparkle();
    let wid = WindowId(0);
    let session = app.front_terminal(wid).expect("front terminal").session;
    let (vim, claude) = (KittyLook::for_app("vim"), KittyLook::for_app("claude"));
    let launch = (0..)
        .map(KittyLook::for_launch)
        .find(|l| {
            coat_distance(l.coat, vim.coat) >= SUFFICIENT_DIFFERENCE
                && coat_distance(l.coat, claude.coat) >= SUFFICIENT_DIFFERENCE
        })
        .expect("some launch coat sits far from both program coats");
    app.launch_kitty = launch;
    assert!(
        coat_distance(vim.coat, claude.coat) >= SUFFICIENT_DIFFERENCE,
        "fixture: the owner's own pair is the 99.2% visible case"
    );

    at_prompt(&mut app);
    assert_eq!(app.companion_verdict(wid, session, t0), launch);
    assert_eq!(
        perform_on_glass(&mut app, wid, launch, t0),
        (launch.coat, launch.iris),
        "the base cat is home from the first present"
    );
    assert_eq!(
        app.windows[&wid].fx.look().tenure().arrival(),
        PetArrival::Quiet,
        "…and home is quiet"
    );
    tick_pet_on_glass(&mut app, wid, t0);

    run_program(&mut app, "vim");
    let t1 = t0 + Duration::from_secs(1);
    assert_eq!(
        app.companion_verdict(wid, session, t1),
        launch,
        "no tenure yet"
    );
    let t2 = t1 + TENURE;
    assert_eq!(app.companion_verdict(wid, session, t2), vim, "landing 1");
    assert_eq!(
        app.windows[&wid].fx.look().tenure().arrival(),
        PetArrival::Ceremony
    );
    perform_on_glass(&mut app, wid, vim, t2);
    assert_eq!(
        app.windows[&wid].fx.look().tenure().arrival(),
        PetArrival::Quiet,
        "hello 1 was PERFORMED and committed: the authorisation is consumed"
    );

    exit_to_prompt(&mut app);
    run_program(&mut app, "claude --resume");
    let t3 = t2 + Duration::from_secs(30);
    assert_eq!(
        app.companion_verdict(wid, session, t3),
        vim,
        "claude candidate armed"
    );
    let t4 = t3 + TENURE;
    assert_eq!(app.companion_verdict(wid, session, t4), claude, "landing 2");
    assert_eq!(
        app.windows[&wid].fx.look().tenure().arrival(),
        PetArrival::Ceremony
    );
    perform_on_glass(&mut app, wid, claude, t4);
    assert_eq!(
        app.windows[&wid].fx.look().tenure().arrival(),
        PetArrival::Quiet
    );

    exit_to_prompt(&mut app);
    run_program(&mut app, "vim");
    let t5 = t4 + Duration::from_secs(30);
    assert_eq!(
        app.companion_verdict(wid, session, t5),
        claude,
        "vim candidate armed"
    );
    let t6 = t5 + TENURE;
    assert_eq!(app.companion_verdict(wid, session, t6), vim, "landing 3");
    assert_eq!(
        app.windows[&wid].fx.look().tenure().arrival(),
        PetArrival::Quiet,
        "a return to a recent program is a HOMECOMING, not a ceremony"
    );
    {
        let ws = &app.windows[&wid];
        assert_eq!(
            aterm_effects::companion::pet_arrival_for_sync(
                ws.fx.look().rung(),
                ws.fx.look().tenure().arrival(),
                ws.fx.companion().brain().worn_pair(),
                vim.coat,
            ),
            PetArrival::Quiet,
            "…and the sync-site mapping carries the quiet home"
        );
    }
    perform_on_glass(&mut app, wid, vim, t6);
    assert_eq!(
        app.windows[&wid].fx.look().tenure().arrival(),
        PetArrival::Quiet
    );
}

/// An authorised ceremony between dark-collapse neighbour coats is demoted to `Quiet` at the sync site without stamping the floor.
#[test]
fn a_near_identical_program_switch_never_ceremonies() {
    let t0 = Instant::now();
    let mut app = App::headless_for_test();
    app.recompute_sparkle();
    let wid = WindowId(0);
    let session = app.front_terminal(wid).expect("front terminal").session;
    let (vim, git) = (KittyLook::for_app("vim"), KittyLook::for_app("git"));
    assert!(
        coat_distance(vim.coat, git.coat) < SUFFICIENT_DIFFERENCE,
        "fixture: vim/git coats are dark-collapse neighbours ({})",
        coat_distance(vim.coat, git.coat)
    );
    assert_ne!(
        (vim.coat, vim.iris),
        (git.coat, git.iris),
        "fixture: still a real pair change — only the ANNOUNCEMENT is noise"
    );

    at_prompt(&mut app);
    run_program(&mut app, "vim");
    app.companion_verdict(wid, session, t0);
    let t1 = t0 + TENURE;
    assert_eq!(app.companion_verdict(wid, session, t1), vim);
    assert_eq!(
        app.windows[&wid].fx.look().tenure().arrival(),
        PetArrival::Ceremony
    );
    assert_eq!(
        perform_on_glass(&mut app, wid, vim, t1),
        (vim.coat, vim.iris),
        "birth apply: the pet wears vim at once"
    );
    assert_eq!(
        app.windows[&wid].fx.look().tenure().arrival(),
        PetArrival::Ceremony,
        "a silent apply performs no ceremony: nothing was committed"
    );
    tick_pet_on_glass(&mut app, wid, t1);

    exit_to_prompt(&mut app);
    run_program(&mut app, "git log");
    let t2 = t1 + Duration::from_secs(30);
    app.companion_verdict(wid, session, t2);
    let t3 = t2 + TENURE;
    assert_eq!(app.companion_verdict(wid, session, t3), git, "git is worn");
    assert_eq!(
        app.windows[&wid].fx.look().tenure().arrival(),
        PetArrival::Ceremony,
        "the gate authorises: git is a stranger and the floor is clear"
    );
    {
        let ws = &app.windows[&wid];
        assert_eq!(
            aterm_effects::companion::pet_arrival_for_sync(
                ws.fx.look().rung(),
                ws.fx.look().tenure().arrival(),
                ws.fx.companion().brain().worn_pair(),
                git.coat,
            ),
            PetArrival::Quiet,
            "…and the sync site DEMOTES: too similar to announce"
        );
    }
    perform_on_glass(&mut app, wid, git, t3);
    assert_eq!(
        app.windows[&wid].fx.look().tenure().arrival(),
        PetArrival::Ceremony,
        "a demoted hello commits nothing: the authorisation stands"
    );
    assert_eq!(
        app.windows[&wid].fx.companion().brain().worn_pair(),
        Some((vim.coat, vim.iris)),
        "the quiet re-dress is parked; the costume law is untouched"
    );

    let cargo = KittyLook::for_app("cargo");
    assert!(
        coat_distance(vim.coat, cargo.coat) >= SUFFICIENT_DIFFERENCE
            && coat_distance(git.coat, cargo.coat) >= SUFFICIENT_DIFFERENCE,
        "fixture: cargo is visibly different from both"
    );
    exit_to_prompt(&mut app);
    run_program(&mut app, "cargo build");
    let t4 = t3 + Duration::from_secs(1);
    app.companion_verdict(wid, session, t4);
    let t5 = t4 + TENURE;
    assert_eq!(app.companion_verdict(wid, session, t5), cargo);
    assert_eq!(
        app.windows[&wid].fx.look().tenure().arrival(),
        PetArrival::Ceremony,
        "demotion kept the floor unstamped: the next real stranger is not spaced"
    );
}

/// A capture syncs the look with `present = false`, preserving the hello debt for the next drawn present.
#[test]
fn a_capture_never_spends_a_hello() {
    let t0 = Instant::now();
    let mut app = App::headless_for_test();
    app.config.cursor_trail = Some(true);
    app.recompute_sparkle();
    let wid = WindowId(0);
    let session = app.front_terminal(wid).expect("front terminal").session;
    let launch = KittyLook::for_launch(App::TEST_LAUNCH_SEED);
    let claude = KittyLook::for_app("claude");
    assert!(
        coat_distance(launch.coat, claude.coat) >= SUFFICIENT_DIFFERENCE,
        "fixture: the hop is visible — no demotion in this trace"
    );

    at_prompt(&mut app);
    app.capture_words(wid, t0);
    assert_eq!(
        app.windows[&wid].fx.companion().brain().worn_pair(),
        Some((launch.coat, launch.iris)),
        "fixture: the capture splice dresses the pet through the shared emitter"
    );
    tick_pet_on_glass(&mut app, wid, t0);

    run_program(&mut app, "claude --resume");
    let t1 = t0 + Duration::from_secs(1);
    app.companion_verdict(wid, session, t1);
    let t2 = t1 + TENURE;
    assert_eq!(app.companion_verdict(wid, session, t2), claude);
    assert_eq!(
        app.windows[&wid].fx.look().tenure().arrival(),
        PetArrival::Ceremony
    );

    app.capture_words(wid, t2);
    assert_eq!(
        app.windows[&wid].fx.look().tenure().arrival(),
        PetArrival::Ceremony,
        "a capture never spends a hello: the authorisation survives it"
    );

    perform_on_glass(&mut app, wid, claude, t2 + Duration::from_millis(16));
    assert_eq!(
        app.windows[&wid].fx.look().tenure().arrival(),
        PetArrival::Quiet,
        "the present performs and commits the hello the capture left intact"
    );
}

/// A background window resolves the verdict without reaching the sync site, spending the hello only when focus returns.
#[test]
fn a_background_window_never_spends_a_hello() {
    let t0 = Instant::now();
    let mut app = App::headless_for_test();
    app.config.cursor_trail = Some(true);
    app.recompute_sparkle();
    let wid = WindowId(0);
    let session = app.front_terminal(wid).expect("front terminal").session;
    let launch = KittyLook::for_launch(App::TEST_LAUNCH_SEED);
    let claude = KittyLook::for_app("claude");

    at_prompt(&mut app);
    app.capture_words(wid, t0);
    assert_eq!(
        app.windows[&wid].fx.companion().brain().worn_pair(),
        Some((launch.coat, launch.iris)),
        "fixture: the pet is dressed while the window is focused"
    );
    tick_pet_on_glass(&mut app, wid, t0);

    app.windows.get_mut(&wid).expect("window").focused = false;
    run_program(&mut app, "claude --resume");
    let t1 = t0 + Duration::from_secs(1);
    app.companion_verdict(wid, session, t1);
    let t2 = t1 + TENURE;
    assert_eq!(
        app.companion_verdict(wid, session, t2),
        claude,
        "the verdict resolves for a background window"
    );
    assert_eq!(
        app.windows[&wid].fx.look().tenure().arrival(),
        PetArrival::Ceremony
    );

    app.capture_words(wid, t2);
    assert_eq!(
        app.windows[&wid].fx.companion().brain().worn_pair(),
        Some((launch.coat, launch.iris)),
        "not on glass: the pet was never even re-dressed"
    );
    assert_eq!(
        app.windows[&wid].fx.look().tenure().arrival(),
        PetArrival::Ceremony,
        "a hello nobody saw was not spent"
    );

    app.windows.get_mut(&wid).expect("window").focused = true;
    tick_pet_on_glass(&mut app, wid, t2 + Duration::from_secs(1));
    assert!(
        app.windows[&wid].fx.companion().brain().is_active(),
        "fixture: focus returned and the pet is drawable again"
    );
    perform_on_glass(&mut app, wid, claude, t2 + Duration::from_secs(1));
    assert_eq!(
        app.windows[&wid].fx.look().tenure().arrival(),
        PetArrival::Quiet,
        "the hello is collected when you look at it"
    );
}
